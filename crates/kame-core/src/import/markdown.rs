//! Markdown import.
//!
//! `from_markdown` turns Markdown written anywhere into a [`Document`]. The
//! document model is narrower than Markdown, so anything without an equivalent
//! is simplified rather than dropped, keeping every word, and counted in an
//! [`ImportReport`] so the author can see what changed.

use crate::model::{
    is_script_url, link_style, Anchor, Annotation, AnnotationKind, Block, BlockId, BlockKind,
    Document, Metadata, MAX_LIST_DEPTH,
};
use crate::model::{Span, SpanStyle};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

/// What an import had to simplify, counted so the author can be told.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub front_matter: bool,
    pub dropped_front_matter: usize,
    pub script_links: usize,
    pub tables: usize,
    pub raw_html: usize,
    pub image_alt_text: usize,
    pub unanchored_footnotes: usize,
}

impl ImportReport {
    /// One line naming what changed, or `None` when the source mapped cleanly.
    pub fn summary(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        let mut count = |n: usize, one: &str, many: &str| {
            if n > 0 {
                parts.push(format!("{n} {}", if n == 1 { one } else { many }));
            }
        };
        count(self.tables, "table as plain text", "tables as plain text");
        count(
            self.raw_html,
            "raw HTML fragment as plain text",
            "raw HTML fragments as plain text",
        );
        count(
            self.image_alt_text,
            "image description dropped",
            "image descriptions dropped",
        );
        count(
            self.unanchored_footnotes,
            "uncited footnote kept as prose",
            "uncited footnotes kept as prose",
        );
        count(
            self.script_links,
            "script link removed",
            "script links removed",
        );
        count(
            self.dropped_front_matter,
            "front matter field left out",
            "front matter fields left out",
        );
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// An imported document and what the import simplified to produce it.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    pub document: Document,
    pub metadata: Metadata,
    pub report: ImportReport,
}

/// Parse `source` into a new document. Relative image paths resolve against
/// `base_dir`. `fallback_title` names the document when the source's front
/// matter does not.
pub fn from_markdown(source: &str, base_dir: &Path, fallback_title: &str) -> Imported {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let front_matter = split_front_matter(source);
    let body = front_matter.as_ref().map_or(source, |found| found.body);
    let metadata = front_matter
        .as_ref()
        .map_or_else(Metadata::default, |found| found.metadata.clone());
    let mut metadata = metadata;
    metadata.title = typographic(&metadata.title);
    let title = (!metadata.title.is_empty()).then(|| metadata.title.clone());

    let mut builder = Builder::new(base_dir);
    builder.report.front_matter = front_matter.is_some();
    builder.report.dropped_front_matter = front_matter.as_ref().map_or(0, |found| found.dropped);
    for event in Parser::new_ext(body, parser_options()) {
        builder.event(event);
    }
    let (mut blocks, annotations, report) = builder.finish();

    // A title lives in the prose too: the document title never reaches an
    // export, and front matter is the only place these sources name themselves.
    if let Some(title) = &title {
        let repeated = blocks.first().is_some_and(|first| {
            matches!(first.kind, BlockKind::Heading { .. }) && first.text == *title
        });
        if !repeated {
            blocks.insert(
                0,
                new_block(
                    BlockKind::Heading {
                        level: 1,
                        centered: false,
                    },
                    title.clone(),
                ),
            );
        }
    }

    let mut document = Document::new(title.as_deref().unwrap_or(fallback_title));
    // A new document's lone empty block stands in when nothing was imported, so
    // the caret always has somewhere to go.
    if !blocks.is_empty() {
        document.blocks = blocks;
    }
    document.annotations = annotations;
    Imported {
        document,
        metadata,
        report,
    }
}

/// How a source is parsed. Plain punctuation becomes typographic, as a page should
/// have it: `---` an em dash, straight quotes curly. Code and link targets are left
/// as written.
fn parser_options() -> Options {
    Options::ENABLE_FOOTNOTES | Options::ENABLE_TABLES | Options::ENABLE_SMART_PUNCTUATION
}

/// `text` with the same punctuation as the body, for what is read outside it. It
/// is parsed after a placeholder word, so it cannot be read as block structure.
fn typographic(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let source = format!("x {text}");
    let parsed: String = Parser::new_ext(&source, Options::ENABLE_SMART_PUNCTUATION)
        .filter_map(|event| match event {
            Event::Text(part) | Event::Code(part) => Some(part.to_string()),
            _ => None,
        })
        .collect();
    match parsed.strip_prefix("x ") {
        Some(words) if !words.is_empty() => words.to_string(),
        _ => text.to_string(),
    }
}

fn new_block(kind: BlockKind, text: String) -> Block {
    Block {
        id: Uuid::new_v4(),
        kind,
        text,
        spans: Vec::new(),
    }
}

/// Front matter found at the top of a source.
pub struct FrontMatter<'a> {
    pub metadata: Metadata,
    /// Fields with no place in [`Metadata`].
    pub dropped: usize,
    /// The template a page asks for, which only a site generator reads.
    pub template: Option<String>,
    pub body: &'a str,
}
impl FrontMatter<'_> {
    /// Files one `key: value` field.
    fn record(&mut self, key: &str, value: &str) {
        let value = unquote(value);
        if key.eq_ignore_ascii_case("template") {
            self.template = Some(value.to_string());
            return;
        }
        let slot = match key.to_ascii_lowercase().as_str() {
            "title" => &mut self.metadata.title,
            "date" => &mut self.metadata.published,
            "author" | "authors" => &mut self.metadata.author,
            "publish" => {
                self.metadata.publish = value.eq_ignore_ascii_case("true");
                return;
            }
            "tags" | "keywords" => {
                self.metadata.tags = value
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split(',')
                    .map(|tag| unquote(tag.trim()).to_string())
                    .filter(|tag| !tag.is_empty())
                    .collect();
                return;
            }
            _ => {
                self.dropped += 1;
                return;
            }
        };
        if slot.is_empty() {
            *slot = value.to_string();
        }
    }
}

pub fn split_front_matter(source: &str) -> Option<FrontMatter<'_>> {
    yaml_front_matter(source).or_else(|| pelican_front_matter(source))
}

/// `---` fenced YAML. A blank line straight after the opening `---` means a
/// horizontal rule rather than metadata, the rule CommonMark extensions share.
fn yaml_front_matter(source: &str) -> Option<FrontMatter<'_>> {
    let mut lines = source.split_inclusive('\n');
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut consumed = source.split_inclusive('\n').next()?.len();
    let mut found = FrontMatter {
        metadata: Metadata::default(),
        dropped: 0,
        template: None,
        body: "",
    };
    for (index, line) in lines.enumerate() {
        consumed += line.len();
        let trimmed = line.trim_end();
        if index == 0 && trimmed.is_empty() {
            return None;
        }
        if trimmed == "---" || trimmed == "..." {
            found.body = &source[consumed..];
            return Some(found);
        }
        if let Some((key, value)) = metadata_pair(trimmed) {
            found.record(key, value);
        }
    }
    None
}

/// Pelican's `Key: value` header: the opening block of lines, with indented
/// continuations. Prose can open the same way ("Note: this…"), so every line
/// must fit and the block must carry a title or date to count as metadata.
fn pelican_front_matter(source: &str) -> Option<FrontMatter<'_>> {
    let mut consumed = 0;
    let mut identified = false;
    let mut any = false;
    let mut found = FrontMatter {
        metadata: Metadata::default(),
        dropped: 0,
        template: None,
        body: "",
    };
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if any && trimmed.starts_with([' ', '\t']) {
            consumed += line.len();
            continue;
        }
        let (key, value) = metadata_pair(trimmed)?;
        identified |= key.eq_ignore_ascii_case("title") || key.eq_ignore_ascii_case("date");
        found.record(key, value);
        any = true;
        consumed += line.len();
    }
    if !identified {
        return None;
    }
    found.body = &source[consumed..];
    Some(found)
}

/// Splits a `key: value` line. The key opens with a letter and holds only word
/// characters, spaces and hyphens; the colon is followed by a space or nothing.
fn metadata_pair(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    let opens_with_letter = key.chars().next()?.is_ascii_alphabetic();
    let key_chars = key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' '));
    let separated = value.is_empty() || value.starts_with([' ', '\t']);
    (opens_with_letter && key_chars && separated).then(|| (key, value.trim()))
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Folds parser events into blocks. Text collects until a block boundary, which
/// also closes any style still open and carries it into the next block.
struct Builder<'a> {
    base_dir: &'a Path,
    blocks: Vec<Block>,
    text: String,
    spans: Vec<Span>,
    open_styles: Vec<(SpanStyle, usize)>,
    heading: Option<u8>,
    quote_depth: usize,
    lists: Vec<bool>,
    image_depth: usize,
    /// In a code or HTML block, where leading whitespace is part of the text.
    preformatted: bool,
    /// In a code block: its language, if it names one.
    code: Option<Option<String>>,
    footnote: Option<(String, String)>,
    footnotes: HashMap<String, String>,
    footnote_order: Vec<String>,
    pending_markers: Vec<(usize, String)>,
    markers: Vec<(BlockId, usize, String)>,
    report: ImportReport,
}

impl<'a> Builder<'a> {
    fn new(base_dir: &'a Path) -> Builder<'a> {
        Builder {
            base_dir,
            blocks: Vec::new(),
            text: String::new(),
            spans: Vec::new(),
            open_styles: Vec::new(),
            heading: None,
            quote_depth: 0,
            lists: Vec::new(),
            image_depth: 0,
            preformatted: false,
            code: None,
            footnote: None,
            footnotes: HashMap::new(),
            footnote_order: Vec::new(),
            pending_markers: Vec::new(),
            markers: Vec::new(),
            report: ImportReport::default(),
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.image_depth > 0 {
                    self.report.image_alt_text += 1;
                } else {
                    self.push_text(&text);
                }
            }
            Event::Code(text) => {
                let start = self.text.len();
                self.push_text(&text);
                // A footnote's body is plain text, so its code has nowhere to go.
                if self.footnote.is_none() {
                    self.push_span(start, self.text.len(), SpanStyle::Code);
                }
            }
            Event::Html(text) => self.push_text(&text),
            Event::InlineHtml(text) => {
                self.report.raw_html += 1;
                self.push_text(&text);
            }
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.push_text("\n"),
            Event::Rule => {
                self.flush();
                if self.footnote.is_none() {
                    self.blocks.push(new_block(BlockKind::Rule, String::new()));
                }
            }
            // A body cannot hold a marker of its own: annotations are flat.
            Event::FootnoteReference(label) if self.footnote.is_none() => {
                self.pending_markers
                    .push((self.text.len(), label.to_string()));
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph | Tag::Table(_) | Tag::CodeBlock(_) | Tag::HtmlBlock => {
                self.flush();
                match &tag {
                    Tag::Table(_) => self.report.tables += 1,
                    Tag::CodeBlock(kind) => {
                        let language = match kind {
                            CodeBlockKind::Fenced(info) => {
                                info.split_whitespace().next().map(str::to_string)
                            }
                            CodeBlockKind::Indented => None,
                        };
                        self.code = Some(language);
                    }
                    Tag::HtmlBlock => self.report.raw_html += 1,
                    _ => {}
                }
                self.preformatted = matches!(tag, Tag::CodeBlock(_) | Tag::HtmlBlock);
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.heading = Some(heading_level(level));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote_depth += 1;
            }
            Tag::List(first_number) => {
                self.flush();
                self.lists.push(first_number.is_some());
            }
            Tag::Item => self.flush(),
            Tag::TableCell => {
                if !self.text.is_empty() {
                    self.push_text(" | ");
                }
            }
            Tag::Emphasis => self.open_style(SpanStyle::Italic),
            Tag::Strong => self.open_style(SpanStyle::Bold),
            Tag::Link { dest_url, .. } => {
                if is_script_url(&dest_url) {
                    self.report.script_links += 1;
                }
                // A link to a heading is one to a heading here, as typing it makes.
                let url = dest_url.to_string();
                self.open_style(if url.starts_with('#') {
                    link_style(percent_decoded(&url))
                } else {
                    SpanStyle::Link { url }
                });
            }
            Tag::Image { dest_url, .. } => {
                // An image block cannot sit inside a footnote body, so there
                // its description stays as the text.
                if self.footnote.is_none() {
                    self.flush();
                    let path = self.resolve_image(&dest_url);
                    // The innermost link around it, unless that runs script. A document link
                    // here can only be to a heading, as that's all an import makes.
                    let link = self
                        .open_styles
                        .iter()
                        .rev()
                        .find_map(|(style, _)| match style {
                            SpanStyle::Link { url } if !is_script_url(url) => Some(url.clone()),
                            SpanStyle::DocumentLink { target } => Some(target.clone()),
                            _ => None,
                        });
                    self.blocks.push(new_block(
                        BlockKind::Image {
                            path,
                            link,
                            align: Default::default(),
                        },
                        String::new(),
                    ));
                    self.image_depth += 1;
                }
            }
            Tag::FootnoteDefinition(label) => {
                self.flush();
                self.footnote = Some((label.to_string(), String::new()));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::CodeBlock | TagEnd::HtmlBlock => {
                self.flush();
                self.preformatted = false;
                self.code = None;
            }
            TagEnd::Paragraph | TagEnd::TableHead | TagEnd::TableRow | TagEnd::Item => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote_depth -= 1;
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Link => self.close_style(),
            TagEnd::Image => {
                self.image_depth = self.image_depth.saturating_sub(1);
            }
            TagEnd::FootnoteDefinition => {
                if let Some((label, body)) = self.footnote.take() {
                    self.footnote_order.push(label.clone());
                    self.footnotes.insert(label, body.trim().to_string());
                }
            }
            _ => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        match &mut self.footnote {
            Some((_, body)) => body.push_str(text),
            // Prose resuming after an image would otherwise open on the space
            // that followed it.
            None if self.text.is_empty() && !self.preformatted => {
                self.text.push_str(text.trim_start())
            }
            None => self.text.push_str(text),
        }
    }

    fn open_style(&mut self, style: SpanStyle) {
        self.open_styles.push((style, self.text.len()));
    }

    fn close_style(&mut self) {
        if let Some((style, start)) = self.open_styles.pop() {
            self.push_span(start, self.text.len(), style);
        }
    }

    fn push_span(&mut self, start: usize, end: usize, style: SpanStyle) {
        // A script link keeps its words and loses its target.
        if matches!(&style, SpanStyle::Link { url } if is_script_url(url)) {
            return;
        }
        if start < end {
            self.spans.push(Span { start, end, style });
        }
    }

    /// Ends the block being collected. A style still open carries on at the
    /// start of the next block, which is how text around an image stays styled.
    fn flush(&mut self) {
        if let Some((_, body)) = &mut self.footnote {
            if !body.is_empty() && !body.ends_with(' ') {
                body.push(' ');
            }
            return;
        }
        let end = self.text.trim_end().len();
        let open: Vec<(SpanStyle, usize)> = std::mem::take(&mut self.open_styles);
        for (style, start) in &open {
            self.push_span(*start, end, style.clone());
        }
        self.open_styles = open.into_iter().map(|(style, _)| (style, 0)).collect();

        let mut text = std::mem::take(&mut self.text);
        let mut spans = std::mem::take(&mut self.spans);
        text.truncate(end);
        // A marker with no text here waits for the next block that has some:
        // an export cannot place a footnote on an empty block.
        if text.is_empty() {
            return;
        }
        let markers = std::mem::take(&mut self.pending_markers);
        for span in &mut spans {
            span.end = span.end.min(end);
        }
        // Inner styles close first in the source; reading order is what a
        // re-import or an export should see.
        spans.retain(|span| span.start < span.end);
        spans.sort_by_key(|span| (span.start, span.end));
        let mut block = new_block(self.current_kind(), text);
        block.spans = spans;
        for (offset, label) in markers {
            self.markers.push((block.id, offset.min(end), label));
        }
        self.blocks.push(block);
    }

    fn current_kind(&self) -> BlockKind {
        if let Some(language) = &self.code {
            BlockKind::Code {
                language: language.clone(),
            }
        } else if let Some(level) = self.heading {
            BlockKind::Heading {
                level,
                centered: false,
            }
        } else if let Some(ordered) = self.lists.last() {
            BlockKind::ListItem {
                ordered: *ordered,
                depth: (self.lists.len() - 1).min(usize::from(MAX_LIST_DEPTH)) as u8,
            }
        } else if self.quote_depth > 0 {
            BlockKind::BlockQuote
        } else {
            BlockKind::Paragraph
        }
    }

    fn resolve_image(&self, url: &str) -> String {
        let path = Path::new(url);
        if url.contains("://") || path.is_absolute() {
            url.to_string()
        } else {
            self.base_dir.join(path).display().to_string()
        }
    }

    fn finish(mut self) -> (Vec<Block>, Vec<Annotation>, ImportReport) {
        self.flush();
        // Markers still waiting go at the end of the last text there is.
        let last_text = self
            .blocks
            .iter()
            .rev()
            .find(|block| !block.text.is_empty())
            .map(|block| (block.id, block.text.len()));
        if let Some((block_id, end)) = last_text {
            for (_, label) in std::mem::take(&mut self.pending_markers) {
                self.markers.push((block_id, end, label));
            }
        }

        let mut annotations = Vec::new();
        let mut cited = HashSet::new();
        for (block_id, offset, label) in &self.markers {
            if let Some(body) = self.footnotes.get(label) {
                cited.insert(label.as_str());
                annotations.push(Annotation {
                    id: Uuid::new_v4(),
                    kind: AnnotationKind::Footnote,
                    anchor: Anchor::range(*block_id, *offset, *offset),
                    body: body.clone(),
                });
            }
        }

        // A body nothing anchors would otherwise vanish, so it stays as prose,
        // in source order: a map's order would make imports differ run to run.
        let leftover: Vec<String> = self
            .footnote_order
            .iter()
            .filter(|label| !cited.contains(label.as_str()))
            .filter_map(|label| self.footnotes.get(label).cloned())
            .collect();
        self.report.unanchored_footnotes = leftover.len();
        for body in leftover {
            self.blocks.push(new_block(BlockKind::Paragraph, body));
        }
        (self.blocks, annotations, self.report)
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Every block's kind and text, the shape most tests assert on.
#[cfg(test)]
fn outline(document: &Document) -> Vec<(crate::model::BlockKind, String)> {
    document
        .blocks
        .iter()
        .map(|block: &Block| (block.kind.clone(), block.text.clone()))
        .collect()
}

/// `text` with each `%XX` decoded, as a browser reads a link to a heading. Only ASCII is looked at,
/// so a character in `text` is never cut. A `%` that isn't followed by two hex digits, and bytes
/// that don't decode to whole characters, stay as they were written.
fn percent_decoded(text: &str) -> String {
    let encoded_at = |at: usize| {
        text.as_bytes()[at] == b'%'
            && text
                .get(at + 1..at + 3)
                .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        if !encoded_at(at) {
            // Up to the next `%`, which isn't this one if this is one.
            let from = at + usize::from(text.as_bytes()[at] == b'%');
            let next = text[from..]
                .find('%')
                .map_or(text.len(), |found| from + found);
            out.push_str(&text[at..next]);
            at = next;
            continue;
        }
        // A run of them, since a character can take several.
        let start = at;
        let mut bytes = Vec::new();
        while at < text.len() && encoded_at(at) {
            bytes.push(u8::from_str_radix(&text[at + 1..at + 3], 16).expect("hex digits"));
            at += 3;
        }
        let mut done = 0;
        for chunk in bytes.utf8_chunks() {
            out.push_str(chunk.valid());
            done += chunk.valid().len();
            let invalid = chunk.invalid().len();
            out.push_str(&text[start + done * 3..start + (done + invalid) * 3]);
            done += invalid;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::to_markdown;
    use crate::model::{Anchor, AnnotationKind, BlockKind, Span, SpanStyle};
    use std::path::PathBuf;

    fn import(source: &str) -> Imported {
        from_markdown(source, Path::new(""), "Fallback")
    }

    fn paragraph(text: &str) -> (BlockKind, String) {
        (BlockKind::Paragraph, text.to_string())
    }

    #[test]
    fn block_structure_maps_onto_block_kinds() {
        let doc =
            import("# One\n\nBody text.\n\n## Two\n\n> Quoted.\n\n- a\n- b\n\n1. c\n").document;
        assert_eq!(
            outline(&doc),
            vec![
                (
                    BlockKind::Heading {
                        level: 1,
                        centered: false
                    },
                    "One".to_string()
                ),
                paragraph("Body text."),
                (
                    BlockKind::Heading {
                        level: 2,
                        centered: false
                    },
                    "Two".to_string()
                ),
                (BlockKind::BlockQuote, "Quoted.".to_string()),
                (
                    BlockKind::ListItem {
                        ordered: false,
                        depth: 0
                    },
                    "a".to_string()
                ),
                (
                    BlockKind::ListItem {
                        ordered: false,
                        depth: 0
                    },
                    "b".to_string()
                ),
                (
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0
                    },
                    "c".to_string()
                ),
            ]
        );
    }

    #[test]
    fn hard_wrapped_lines_join_into_one_paragraph() {
        let doc = import("Lately, I've been\ngoing through\na period.\n").document;
        assert_eq!(
            outline(&doc),
            vec![paragraph("Lately, I’ve been going through a period.")]
        );
    }

    #[test]
    fn a_hard_line_break_survives_in_the_text() {
        let doc = import("Roses are red,  \nviolets are blue.\n").document;
        assert_eq!(
            outline(&doc),
            vec![paragraph("Roses are red,\nviolets are blue.")]
        );
    }

    #[test]
    fn setext_headings_are_headings() {
        let doc = import("Title\n=====\n\nSection\n-------\n").document;
        assert_eq!(
            outline(&doc),
            vec![
                (
                    BlockKind::Heading {
                        level: 1,
                        centered: false
                    },
                    "Title".to_string()
                ),
                (
                    BlockKind::Heading {
                        level: 2,
                        centered: false
                    },
                    "Section".to_string()
                ),
            ]
        );
    }

    #[test]
    fn inline_styles_become_spans_at_byte_offsets() {
        // The multibyte lead-in proves the offsets are bytes, not chars.
        let doc = import("Café *soft* and **hard**.\n").document;
        let block = &doc.blocks[0];
        assert_eq!(block.text, "Café soft and hard.");
        let at = |word: &str| block.text.find(word).unwrap();
        assert_eq!(
            block.spans,
            vec![
                Span {
                    start: at("soft"),
                    end: at("soft") + 4,
                    style: SpanStyle::Italic,
                },
                Span {
                    start: at("hard"),
                    end: at("hard") + 4,
                    style: SpanStyle::Bold,
                },
            ]
        );
    }

    #[test]
    fn links_keep_their_target() {
        let doc = import("See [the docs](https://example.com/x) now.\n").document;
        let block = &doc.blocks[0];
        assert_eq!(block.text, "See the docs now.");
        assert_eq!(
            block.spans,
            vec![Span {
                start: 4,
                end: 12,
                style: SpanStyle::Link {
                    url: "https://example.com/x".to_string(),
                },
            }]
        );
    }

    #[test]
    fn a_link_to_a_heading_is_a_document_link() {
        let doc = import("See [the intro](<#The Intro>) and [here](#Plain).\n").document;
        let block = &doc.blocks[0];
        assert_eq!(block.text, "See the intro and here.");
        let styles: Vec<&SpanStyle> = block.spans.iter().map(|span| &span.style).collect();
        assert_eq!(
            styles,
            [
                &SpanStyle::DocumentLink {
                    target: "#The Intro".to_string()
                },
                &SpanStyle::DocumentLink {
                    target: "#Plain".to_string()
                },
            ]
        );
        // Nothing about another page's heading, or a bare `#`, changes.
        let doc = import("[a](https://e.com/#x) [b](#)\n").document;
        assert!(doc.blocks[0]
            .spans
            .iter()
            .all(|span| matches!(span.style, SpanStyle::Link { .. })));
    }

    #[test]
    fn a_link_to_a_document_by_a_url_stays_a_link() {
        let doc = import("[x](a.kame) and [y](notes/b.kame#Part)\n").document;
        let styles: Vec<&SpanStyle> = doc.blocks[0].spans.iter().map(|span| &span.style).collect();
        assert_eq!(
            styles,
            [
                &SpanStyle::Link {
                    url: "a.kame".to_string()
                },
                &SpanStyle::Link {
                    url: "notes/b.kame#Part".to_string()
                },
            ]
        );
    }

    #[test]
    fn a_link_to_a_heading_covers_its_words_and_no_others() {
        let doc = import("See [the été](<#The Intro>) and [here](#Plain) now.\n").document;
        let block = &doc.blocks[0];
        assert_eq!(block.text, "See the été and here now.");
        let words: Vec<&str> = block
            .spans
            .iter()
            .map(|span| &block.text[span.start..span.end])
            .collect();
        assert_eq!(words, ["the été", "here"]);
    }

    #[test]
    fn a_percent_encoded_link_to_a_heading_names_the_heading() {
        for (source, target) in [
            ("[y](#The%20Intro)", "#The Intro"),
            ("[y](#%C3%A9t%C3%A9%20%C3%A0)", "#été à"),
            ("[y](#été%20à)", "#été à"),
            // What isn't an encoding, or isn't whole characters, stays as it was written.
            ("[y](#50%)", "#50%"),
            ("[y](#50%2)", "#50%2"),
            ("[y](#50%zz)", "#50%zz"),
            ("[y](#%C3)", "#%C3"),
            ("[y](#%C3%20x)", "#%C3 x"),
            ("[y](#a%FFb%20c)", "#a%FFb c"),
        ] {
            let doc = import(source).document;
            assert_eq!(
                doc.blocks[0].spans[0].style,
                SpanStyle::DocumentLink {
                    target: target.to_string()
                },
                "{source}"
            );
        }
    }

    #[test]
    fn only_a_link_to_a_heading_is_percent_decoded() {
        let doc = import("[y](a%20b.md) [z](https://e.com/#a%20b)").document;
        let urls: Vec<&SpanStyle> = doc.blocks[0].spans.iter().map(|span| &span.style).collect();
        assert_eq!(
            urls,
            [
                &SpanStyle::Link {
                    url: "a%20b.md".to_string()
                },
                &SpanStyle::Link {
                    url: "https://e.com/#a%20b".to_string()
                },
            ]
        );
    }

    #[test]
    fn an_image_linked_to_a_heading_keeps_its_link() {
        let doc = import("[![](a.png)](<#The Intro>)\n").document;
        match &doc.blocks[0].kind {
            BlockKind::Image { link, .. } => {
                assert_eq!(link.as_deref(), Some("#The Intro"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reference_links_resolve() {
        let doc = import("See [the docs][d].\n\n[d]: https://example.com/d\n").document;
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(
            doc.blocks[0].spans[0].style,
            SpanStyle::Link {
                url: "https://example.com/d".to_string(),
            }
        );
    }

    #[test]
    fn escapes_become_literal_text() {
        let doc = import("A \\*literal\\* star.\n").document;
        assert_eq!(doc.blocks[0].text, "A *literal* star.");
        assert!(doc.blocks[0].spans.is_empty());
    }

    #[test]
    fn a_relative_image_resolves_against_the_source_directory() {
        let doc = from_markdown("![](pics/a.png)\n", Path::new("/notes"), "t").document;
        assert_eq!(
            doc.blocks[0].kind,
            BlockKind::Image {
                path: PathBuf::from("/notes/pics/a.png").display().to_string(),
                link: None,
                align: Default::default(),
            }
        );
        assert_eq!(doc.blocks[0].text, "");
    }

    #[test]
    fn absolute_and_remote_images_are_left_alone() {
        for path in ["/images/a.png", "https://example.com/a.png"] {
            let source = format!("![]({path})\n");
            let doc = from_markdown(&source, Path::new("/notes"), "t").document;
            assert_eq!(
                doc.blocks[0].kind,
                BlockKind::Image {
                    path: path.to_string(),
                    link: None,
                    align: Default::default(),
                }
            );
        }
    }

    #[test]
    fn an_image_inside_a_paragraph_splits_it_without_losing_styles() {
        let imported = import("Before **bold ![alt](a.png) still** after.\n");
        let doc = imported.document;
        assert_eq!(
            outline(&doc),
            vec![
                paragraph("Before bold"),
                (
                    BlockKind::Image {
                        path: "a.png".to_string(),
                        link: None,
                        align: Default::default(),
                    },
                    String::new(),
                ),
                paragraph("still after."),
            ]
        );
        assert_eq!(doc.blocks[0].spans[0].style, SpanStyle::Bold);
        assert_eq!(doc.blocks[2].spans[0].style, SpanStyle::Bold);
        assert_eq!(doc.blocks[2].spans[0].start, 0);
        assert_eq!(imported.report.image_alt_text, 1);
    }

    #[test]
    fn footnotes_become_annotations_anchored_at_their_marker() {
        let doc = import("A claim[^one] stands.\n\n[^one]: The source, *quoted*.\n").document;
        assert_eq!(outline(&doc), vec![paragraph("A claim stands.")]);
        assert_eq!(doc.annotations.len(), 1);
        let note = &doc.annotations[0];
        assert_eq!(note.kind, AnnotationKind::Footnote);
        assert_eq!(note.body, "The source, quoted.");
        assert_eq!(note.anchor, Anchor::range(doc.blocks[0].id, 7, 7));
    }

    #[test]
    fn a_footnote_reused_twice_gets_an_annotation_at_each_marker() {
        let doc = import("One[^n] and two[^n].\n\n[^n]: Shared.\n").document;
        let anchors: Vec<_> = doc
            .annotations
            .iter()
            .map(|a| a.anchor.byte_range())
            .collect();
        assert_eq!(anchors, vec![Some((3, 3)), Some((11, 11))]);
    }

    #[test]
    fn a_footnote_marker_without_a_definition_keeps_its_text() {
        let imported = import("A dangling[^gone] marker.\n");
        assert_eq!(
            imported.document.blocks[0].text,
            "A dangling[^gone] marker."
        );
        assert!(imported.document.annotations.is_empty());
    }

    #[test]
    fn a_table_keeps_each_row_as_plain_text_and_is_reported() {
        let imported = import("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(
            outline(&imported.document),
            vec![paragraph("a | b"), paragraph("1 | 2")]
        );
        assert_eq!(imported.report.tables, 1);
    }

    #[test]
    fn raw_html_keeps_its_text_and_is_reported() {
        let imported = import("<div>hello</div>\n\nA <b>bold</b> word.\n");
        assert_eq!(
            outline(&imported.document),
            vec![
                paragraph("<div>hello</div>"),
                paragraph("A <b>bold</b> word.")
            ]
        );
        assert_eq!(imported.report.raw_html, 3);
    }

    #[test]
    fn pelican_metadata_names_the_document_instead_of_reaching_the_prose() {
        let imported = import(
            "Title: On Burnout\nDate: 2019-01-02\nTags: life, work\nSummary: A long\n    summary.\n\nBody.\n",
        );
        assert!(imported.report.front_matter);
        assert_eq!(imported.document.title, "On Burnout");
        assert_eq!(
            outline(&imported.document),
            vec![
                (
                    BlockKind::Heading {
                        level: 1,
                        centered: false
                    },
                    "On Burnout".to_string()
                ),
                paragraph("Body."),
            ]
        );
    }

    #[test]
    fn yaml_front_matter_names_the_document_instead_of_reaching_the_prose() {
        let imported = import("---\ntitle: \"On Burnout\"\ndraft: true\n---\n\nBody.\n");
        assert!(imported.report.front_matter);
        assert_eq!(imported.document.title, "On Burnout");
        assert_eq!(
            outline(&imported.document),
            vec![
                (
                    BlockKind::Heading {
                        level: 1,
                        centered: false
                    },
                    "On Burnout".to_string()
                ),
                paragraph("Body."),
            ]
        );
    }

    #[test]
    fn a_title_matching_the_first_heading_is_not_repeated() {
        let doc = import("Title: Same\n\n# Same\n\nBody.\n").document;
        assert_eq!(
            outline(&doc),
            vec![
                (
                    BlockKind::Heading {
                        level: 1,
                        centered: false
                    },
                    "Same".to_string()
                ),
                paragraph("Body."),
            ]
        );
    }

    #[test]
    fn prose_that_resembles_metadata_stays_prose() {
        for source in [
            "Note: this is just a sentence.\n\nBody.\n",
            "Title: looks like metadata\nbut this line is not.\n\nBody.\n",
            "---\n\nA rule, then prose.\n\n---\n",
        ] {
            let imported = import(source);
            assert!(!imported.report.front_matter, "misread {source:?}");
            assert_eq!(imported.document.title, "Fallback");
        }
        let doc = import("Note: this is just a sentence.\n\nBody.\n").document;
        assert_eq!(doc.blocks[0].text, "Note: this is just a sentence.");
    }

    #[test]
    fn an_empty_source_still_has_a_block_for_the_caret() {
        for source in ["", "\n\n", "Title: Only\n"] {
            let doc = import(source).document;
            assert!(!doc.blocks.is_empty(), "no block for {source:?}");
        }
    }

    #[test]
    fn a_byte_order_mark_is_not_text() {
        let doc = import("\u{feff}Hello.\n").document;
        assert_eq!(doc.blocks[0].text, "Hello.");
    }

    #[test]
    fn an_imported_footnote_reaches_the_html_export() {
        let doc = import("A claim[^one] stands.\n\n[^one]: The source.\n").document;
        let html = crate::export::to_html(&doc);
        assert!(html.contains("The source."), "{html}");
    }

    #[test]
    fn the_report_reads_as_english() {
        assert_eq!(ImportReport::default().summary(), None);
        let one = ImportReport {
            tables: 1,
            raw_html: 1,
            ..ImportReport::default()
        };
        assert_eq!(
            one.summary().unwrap(),
            "1 table as plain text, 1 raw HTML fragment as plain text"
        );
        let two = ImportReport {
            tables: 2,
            dropped_front_matter: 1,
            ..ImportReport::default()
        };
        assert_eq!(
            two.summary().unwrap(),
            "2 tables as plain text, 1 front matter field left out"
        );
    }

    #[test]
    fn a_marker_in_a_textless_paragraph_attaches_to_the_next_text() {
        let doc = import("![](a.png)[^a]\n\nNext words.\n\n[^a]: Body.\n").document;
        assert_eq!(doc.annotations.len(), 1);
        assert_eq!(doc.annotations[0].body, "Body.");
        assert_eq!(
            doc.annotations[0].anchor,
            Anchor::range(doc.blocks[1].id, 0, 0)
        );
    }

    #[test]
    fn a_trailing_textless_marker_attaches_to_the_end_of_the_last_text() {
        let doc = import("Some words.\n\n![](a.png)[^a]\n\n[^a]: Body.\n").document;
        assert_eq!(doc.annotations.len(), 1);
        assert_eq!(
            doc.annotations[0].anchor,
            Anchor::range(doc.blocks[0].id, 11, 11)
        );
    }

    #[test]
    fn a_footnote_with_no_text_to_anchor_to_keeps_its_words() {
        let imported = import("[^a]\n\n[^a]: Body.\n");
        assert!(imported.document.annotations.is_empty());
        assert_eq!(outline(&imported.document), vec![paragraph("Body.")]);
        assert_eq!(imported.report.unanchored_footnotes, 1);
    }

    #[test]
    fn an_unreferenced_footnote_keeps_its_words() {
        let imported = import("Prose.\n\n[^spare]: Never cited.\n");
        assert!(imported.document.annotations.is_empty());
        assert_eq!(
            outline(&imported.document),
            vec![paragraph("Prose."), paragraph("Never cited.")]
        );
        assert_eq!(imported.report.unanchored_footnotes, 1);
    }

    /// Line breaks inside a block survive the trip out and back.
    #[test]
    fn line_breaks_survive_export_and_import() {
        for text in [
            "Roses are red,\nviolets are blue.",
            "one\n\nthree",
            "plain\n# not a heading\n- not a list\n1. not a number",
        ] {
            let mut doc = Document::new("t");
            doc.blocks[0].text = text.to_string();
            let exported = to_markdown(&doc);
            let again = import(&exported).document;
            assert_eq!(again.blocks.len(), 1, "{exported:?} split up");
            assert_eq!(again.blocks[0].text, text, "from {exported:?}");
            assert_eq!(to_markdown(&again), exported);
        }
    }

    #[test]
    fn line_breaks_in_a_quote_and_a_list_item_stay_in_the_block() {
        for kind in [
            BlockKind::BlockQuote,
            BlockKind::ListItem {
                ordered: false,
                depth: 0,
            },
        ] {
            let mut doc = Document::new("t");
            doc.blocks[0].kind = kind.clone();
            doc.blocks[0].text = "first\nsecond".to_string();
            let again = import(&to_markdown(&doc)).document;
            assert_eq!(outline(&again), vec![(kind, "first\nsecond".to_string())]);
        }
    }

    /// The contract with the exporter: what kame writes, kame reads back to the
    /// same Markdown, byte for byte.
    #[test]
    fn exporting_an_import_of_an_export_is_byte_identical() {
        let first = import(
            "# Heading *with style*\n\nA paragraph with **bold**, *italic*, a \
             [link](https://example.com) and a note[^a].\n\n> A quoted line.\n\n- one\n- two\n\n\
             1. first\n2. second\n\nStars \\* and \\_unders\\_ and \\[brackets\\] in café text.\n\n\
             ![](pics/a.png)\n\n[^a]: Its body.\n",
        )
        .document;
        let exported = to_markdown(&first);
        let again = to_markdown(&import(&exported).document);
        assert_eq!(exported, again);
        assert!(exported.contains("[^1]: Its body."));
    }

    fn collect_markdown(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read corpus directory") {
            let path = entry.expect("read corpus entry").path();
            if path.is_dir() {
                collect_markdown(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                found.push(path);
            }
        }
    }

    /// Every word the parser sees in `source`, images' descriptions aside.
    fn parsed_words(source: &str) -> HashMap<String, usize> {
        let source = source.strip_prefix('\u{feff}').unwrap_or(source);
        let body = split_front_matter(source).map_or(source, |found| found.body);
        let mut words = HashMap::new();
        let mut in_image = 0;
        for event in Parser::new_ext(body, parser_options()) {
            let text = match event {
                Event::Start(Tag::Image { .. }) => {
                    in_image += 1;
                    continue;
                }
                Event::End(TagEnd::Image) => {
                    in_image -= 1;
                    continue;
                }
                Event::Text(text)
                | Event::Code(text)
                | Event::Html(text)
                | Event::InlineHtml(text)
                    if in_image == 0 =>
                {
                    text
                }
                _ => continue,
            };
            for word in text.split_whitespace() {
                *words.entry(word.to_string()).or_insert(0) += 1;
            }
        }
        words
    }

    /// Imports every `.md` under `KAME_MD_CORPUS`, checking what must hold for
    /// any real file. Run with
    /// `KAME_MD_CORPUS=~/writing cargo test corpus -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs KAME_MD_CORPUS pointing at a directory of Markdown"]
    fn corpus_imports_without_losing_words() {
        let root = PathBuf::from(std::env::var("KAME_MD_CORPUS").expect("KAME_MD_CORPUS is set"));
        let mut files = Vec::new();
        collect_markdown(&root, &mut files);
        files.sort();
        assert!(!files.is_empty(), "no Markdown under {}", root.display());

        let (mut unstable, mut with_newlines, mut lost_files) = (Vec::new(), 0, Vec::new());
        for file in &files {
            let Ok(source) = std::fs::read_to_string(file) else {
                println!("skipped (not UTF-8): {}", file.display());
                continue;
            };
            let document = from_markdown(&source, Path::new(""), "t").document;
            assert!(!document.blocks.is_empty());

            let mut have: HashMap<String, usize> = HashMap::new();
            let mut count = |text: &str| {
                for word in text.split_whitespace() {
                    *have.entry(word.to_string()).or_insert(0) += 1;
                }
            };
            for block in &document.blocks {
                count(&block.text);
                for span in &block.spans {
                    assert!(
                        span.start < span.end
                            && block.text.is_char_boundary(span.start)
                            && block.text.is_char_boundary(span.end),
                        "bad span in {}",
                        file.display()
                    );
                }
            }
            for annotation in &document.annotations {
                count(&annotation.body);
                let (start, end) = annotation.anchor.byte_range().expect("range anchor");
                let block = document
                    .blocks
                    .iter()
                    .find(|block| block.id == annotation.anchor.block_id())
                    .expect("anchor block exists");
                assert!(start <= end && block.text.is_char_boundary(end));
            }

            // Words split at a run boundary can differ in shape, so compare the
            // letters of what is missing rather than each token.
            let lost: Vec<String> = parsed_words(&source)
                .into_iter()
                .filter(|(word, n)| have.get(word).copied().unwrap_or(0) < *n)
                .map(|(word, _)| word)
                .filter(|word| !have.keys().any(|kept| kept.contains(word.as_str())))
                .collect();
            if !lost.is_empty() {
                lost_files.push((file.clone(), lost));
            }

            if document
                .blocks
                .iter()
                .any(|block| block.text.contains('\n'))
            {
                with_newlines += 1;
            } else {
                let exported = to_markdown(&document);
                if to_markdown(&from_markdown(&exported, Path::new(""), "t").document) != exported {
                    unstable.push(file.clone());
                }
            }
        }
        println!(
            "{} files; {with_newlines} with multi-line blocks (stability not checked); \
             {} unstable; {} lost words",
            files.len(),
            unstable.len(),
            lost_files.len()
        );
        for file in &unstable {
            println!("unstable: {}", file.display());
        }
        for (file, words) in &lost_files {
            println!(
                "lost in {}: {:?}",
                file.display(),
                &words[..words.len().min(8)]
            );
        }
        assert!(lost_files.is_empty(), "words were lost");
    }

    #[test]
    fn pelican_front_matter_becomes_metadata() {
        let imported = import(
            "Title: On Burnout\nDate: 2019-01-02 21:01\nAuthor: K. Isom\nTags: life, work\n\nBody.\n",
        );
        let meta = imported.metadata;
        assert_eq!(meta.title, "On Burnout");
        assert_eq!(meta.published, "2019-01-02 21:01");
        assert_eq!(meta.author, "K. Isom");
        assert_eq!(meta.tags, vec!["life", "work"]);
        assert_eq!(imported.report.dropped_front_matter, 0);
    }

    #[test]
    fn yaml_front_matter_becomes_metadata() {
        let imported = import(
            "---\ntitle: \"On Burnout\"\nauthors: K. Isom\ndate: 2019-01-02\ntags: [life, work]\n---\n\nBody.\n",
        );
        let meta = imported.metadata;
        assert_eq!(meta.title, "On Burnout");
        assert_eq!(meta.author, "K. Isom");
        assert_eq!(meta.published, "2019-01-02");
        assert_eq!(meta.tags, vec!["life", "work"]);
    }

    #[test]
    fn front_matter_with_no_equivalent_is_counted() {
        let imported = import("Title: T\nSummary: A long\n    summary.\nSlug: t\n\nBody.\n");
        assert_eq!(imported.report.dropped_front_matter, 2);
        assert_eq!(
            imported.report.summary().unwrap(),
            "2 front matter fields left out"
        );
    }

    #[test]
    fn empty_tags_are_no_tags() {
        let imported = import("Title: T\nTags:\n\nBody.\n");
        assert!(imported.metadata.tags.is_empty());
    }

    #[test]
    fn a_source_without_front_matter_has_no_metadata() {
        let imported = import("Body.\n");
        assert_eq!(imported.metadata, crate::model::Metadata::default());
        assert_eq!(imported.report.summary(), None);
    }

    #[test]
    fn a_script_link_imports_as_its_text() {
        let imported = import("See [the demo](javascript:alert(1)) now.\n");
        let block = &imported.document.blocks[0];
        assert_eq!(block.text, "See the demo now.");
        assert!(block.spans.is_empty(), "{:?}", block.spans);
        assert_eq!(imported.report.script_links, 1);
        assert_eq!(imported.report.summary().unwrap(), "1 script link removed");
    }

    #[test]
    fn a_pdf_link_survives_import() {
        let doc = import("See [paper](paper.pdf) and [local](file:///x/p.pdf).\n").document;
        assert_eq!(doc.blocks[0].spans.len(), 2);
    }

    #[test]
    fn a_style_around_a_script_link_survives() {
        let doc = import("**[bold](javascript:x)**\n").document;
        assert_eq!(doc.blocks[0].spans.len(), 1);
        assert_eq!(doc.blocks[0].spans[0].style, SpanStyle::Bold);
    }

    /// What the exporter writes for a destination reads back as the same URL.
    #[test]
    fn link_destinations_survive_export_and_import() {
        for url in [
            "https://example.com/?a=1&b=2",
            "https://en.wikipedia.org/wiki/Foo_(bar)",
            "javascript&#58;alert(1)",
            "back\\slash",
            "paper.pdf",
        ] {
            let mut doc = Document::new("t");
            doc.blocks[0].text = "words".to_string();
            doc.blocks[0].spans = vec![Span {
                start: 0,
                end: 5,
                style: SpanStyle::Link {
                    url: url.to_string(),
                },
            }];
            let exported = to_markdown(&doc);
            let again = import(&exported).document;
            assert_eq!(
                again.blocks[0].spans, doc.blocks[0].spans,
                "from {exported:?}"
            );
        }
    }

    /// Whatever a stored URL looks like, a Markdown renderer reading the export must
    /// not find a script link in it that the export's own check did not see.
    #[test]
    fn no_exported_destination_reads_back_as_a_script_link() {
        for url in [
            "<javascript:alert(1)>",
            "javascript&#58;alert(1)",
            "javascript&colon;alert(1)",
            "&#106;avascript:alert(1)",
            "java\\script:alert(1)",
            "\\javascript:alert(1)",
            "<javascript:alert(1)",
            "x) [y](javascript:alert(1)",
            "x\" onclick=\"alert(1)",
        ] {
            let mut doc = Document::new("t");
            doc.blocks[0].text = "words".to_string();
            doc.blocks[0].spans = vec![Span {
                start: 0,
                end: 5,
                style: SpanStyle::Link {
                    url: url.to_string(),
                },
            }];
            let exported = to_markdown(&doc);
            for event in Parser::new_ext(&exported, Options::empty()) {
                if let Event::Start(Tag::Link { dest_url, .. }) = event {
                    assert!(
                        !crate::model::is_script_url(&dest_url),
                        "{url:?} exported as {exported:?} reads back as {dest_url:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn plain_punctuation_becomes_typographic() {
        let doc = import("I've said \"it\" -- twice --- and 'more'...\n").document;
        assert_eq!(doc.blocks[0].text, "I’ve said “it” – twice — and ‘more’…");
    }

    #[test]
    fn code_keeps_its_plain_punctuation() {
        let doc = import("Run `a --- 'b'`.\n\n```\nx = \"y\" -- z\n```\n").document;
        assert_eq!(doc.blocks[0].text, "Run a --- 'b'.");
        assert_eq!(doc.blocks[1].text, "x = \"y\" -- z");
    }

    #[test]
    fn a_link_target_keeps_its_plain_punctuation() {
        let doc = import("[a](https://x.example/it's--here)\n").document;
        assert_eq!(
            doc.blocks[0].spans[0].style,
            SpanStyle::Link {
                url: "https://x.example/it's--here".to_string(),
            }
        );
    }

    /// A title is read from front matter, not the body, but appears beside it.
    #[test]
    fn a_front_matter_title_gets_the_same_punctuation() {
        let imported = import("Title: Can't Stop -- \"Won't\"\n\nBody.\n");
        assert_eq!(imported.metadata.title, "Can’t Stop – “Won’t”");
        assert_eq!(imported.document.blocks[0].text, "Can’t Stop – “Won’t”");
    }

    #[test]
    fn a_title_that_looks_like_markdown_keeps_its_words() {
        let imported = import("Title: 1. # First - 2019\n\nBody.\n");
        assert_eq!(imported.metadata.title, "1. # First - 2019");
    }

    #[test]
    fn a_linked_image_keeps_its_link() {
        let doc = import("[![](small.jpg)](large.jpg)\n").document;
        assert_eq!(
            outline(&doc),
            vec![(
                BlockKind::Image {
                    path: "small.jpg".to_string(),
                    link: Some("large.jpg".to_string()),
                    align: Default::default(),
                },
                String::new(),
            )]
        );
    }

    #[test]
    fn an_image_with_a_script_link_keeps_only_the_image() {
        let doc = import("[![](a.png)](javascript:alert(1))\n").document;
        assert_eq!(
            doc.blocks[0].kind,
            BlockKind::Image {
                path: "a.png".to_string(),
                link: None,
                align: Default::default(),
            }
        );
    }

    #[test]
    fn text_after_an_image_keeps_the_link_around_both() {
        let doc = import("[![](a.png) words](b.html)\n").document;
        assert_eq!(
            doc.blocks[0].kind,
            BlockKind::Image {
                path: "a.png".to_string(),
                link: Some("b.html".to_string()),
                align: Default::default(),
            }
        );
        assert_eq!(doc.blocks[1].text, "words");
        assert_eq!(
            doc.blocks[1].spans[0].style,
            SpanStyle::Link {
                url: "b.html".to_string(),
            }
        );
    }

    #[test]
    fn a_linked_image_survives_export_and_import() {
        let source = "[![](small.jpg)](large.jpg)\n";
        let exported = to_markdown(&import(source).document);
        assert_eq!(exported, source);
    }

    /// Files from before the field read as unlinked.
    #[test]
    fn an_older_image_block_has_no_link() {
        let kind: BlockKind = serde_json::from_str(r#"{"Image":{"path":"a.png"}}"#).unwrap();
        assert_eq!(
            kind,
            BlockKind::Image {
                path: "a.png".to_string(),
                link: None,
                align: Default::default(),
            }
        );
    }

    #[test]
    fn a_rule_imports_as_a_rule() {
        let imported = import("One.\n\n***\n\nTwo.\n");
        assert_eq!(imported.document.blocks[1].kind, BlockKind::Rule);
    }

    #[test]
    fn fenced_code_imports_as_code_with_its_language() {
        let imported = import("```rust\nfn a() {\n    b();\n\n}\n```\n");
        assert_eq!(
            outline(&imported.document),
            vec![(
                BlockKind::Code {
                    language: Some("rust".to_string())
                },
                "fn a() {\n    b();\n\n}".to_string()
            )]
        );
    }

    #[test]
    fn indented_code_imports_as_code() {
        let doc = import("Text.\n\n    let a = 1;\n").document;
        assert_eq!(doc.blocks[1].kind, BlockKind::Code { language: None });
        assert_eq!(doc.blocks[1].text, "let a = 1;");
    }

    #[test]
    fn code_survives_export_and_import_exactly() {
        let source = "```rust\nfn a() {\n    let b = \"c -- d\";\n\n}\n```\n";
        assert_eq!(to_markdown(&import(source).document), source);
    }

    #[test]
    fn inline_code_imports_as_a_code_span() {
        let imported = import("Run `make` first.\n");
        let block = &imported.document.blocks[0];
        assert_eq!(block.text, "Run make first.");
        assert_eq!(
            block.spans,
            vec![Span {
                start: 4,
                end: 8,
                style: SpanStyle::Code
            }]
        );
        assert_eq!(imported.report.summary(), None);
    }

    #[test]
    fn inline_code_survives_export_and_import_exactly() {
        let source = "Run `a_b*c` and ``d`e`` and `` `f ``, **then `g`**.\n";
        assert_eq!(to_markdown(&import(source).document), source);
    }

    fn levels(source: &str) -> Vec<(u8, bool, String)> {
        import(source)
            .document
            .blocks
            .iter()
            .filter_map(|block| match block.kind {
                BlockKind::ListItem { ordered, depth } => {
                    Some((depth, ordered, block.text.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn nested_lists_import_at_their_depth() {
        assert_eq!(
            levels("- outer\n  - inner\n    1. deep\n- next\n"),
            vec![
                (0, false, "outer".to_string()),
                (1, false, "inner".to_string()),
                (2, true, "deep".to_string()),
                (0, false, "next".to_string()),
            ]
        );
        assert_eq!(import("- a\n  - b\n").report.summary(), None);
    }

    #[test]
    fn nested_lists_survive_export_and_import() {
        let source = "- a\n\n  - b\n\n    1. c\n\n    2. d\n\n  - e\n\n- f\n";
        assert_eq!(to_markdown(&import(source).document), source);
    }

    #[test]
    fn lists_nested_past_the_limit_stay_at_it() {
        let source =
            "- a\n  - b\n    - c\n      - d\n        - e\n          - f\n            - g\n";
        let deepest = levels(source).into_iter().map(|(depth, _, _)| depth).max();
        assert_eq!(deepest, Some(crate::model::MAX_LIST_DEPTH));
    }

    #[test]
    fn front_matter_is_available_to_other_tools() {
        let found =
            split_front_matter("---\ntitle: About\ntemplate: wide\n---\n\nHello\n").unwrap();
        assert_eq!(found.metadata.title, "About");
        assert_eq!(found.template.as_deref(), Some("wide"));
        assert_eq!(found.body.trim(), "Hello");
    }

    #[test]
    fn a_template_field_is_not_reported_as_dropped() {
        let found =
            split_front_matter("---\ntitle: About\ntemplate: wide\n---\n\nHello\n").unwrap();
        assert_eq!(found.dropped, 0);
    }
}
