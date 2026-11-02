//! Markdown import.
//!
//! `from_markdown` turns Markdown written anywhere into a [`Document`]. The
//! document model is narrower than Markdown, so anything without an equivalent
//! is simplified rather than dropped, keeping every word, and counted in an
//! [`ImportReport`] so the author can see what changed.

use crate::model::{Anchor, Annotation, AnnotationKind, Block, BlockId, BlockKind, Document};
use crate::model::{Span, SpanStyle};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

/// What an import had to simplify, counted so the author can be told.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub front_matter: bool,
    pub code_blocks: usize,
    pub inline_code: usize,
    pub tables: usize,
    pub raw_html: usize,
    pub rules: usize,
    pub nested_list_items: usize,
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
        count(
            self.code_blocks,
            "code block as plain text",
            "code blocks as plain text",
        );
        count(
            self.inline_code,
            "inline code span as plain text",
            "inline code spans as plain text",
        );
        count(self.tables, "table as plain text", "tables as plain text");
        count(
            self.raw_html,
            "raw HTML fragment as plain text",
            "raw HTML fragments as plain text",
        );
        count(
            self.nested_list_items,
            "nested list item flattened",
            "nested list items flattened",
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
            self.rules,
            "horizontal rule dropped",
            "horizontal rules dropped",
        );
        if self.front_matter {
            parts.push("front matter left out".to_string());
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// An imported document and what the import simplified to produce it.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    pub document: Document,
    pub report: ImportReport,
}

/// Parse `source` into a new document. Relative image paths resolve against
/// `base_dir`. `fallback_title` names the document when the source's front
/// matter does not.
pub fn from_markdown(source: &str, base_dir: &Path, fallback_title: &str) -> Imported {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let front_matter = split_front_matter(source);
    let body = front_matter.as_ref().map_or(source, |found| found.body);
    let title = front_matter.as_ref().and_then(|found| found.title.clone());

    let mut builder = Builder::new(base_dir);
    builder.report.front_matter = front_matter.is_some();
    // Smart punctuation stays off: it would rewrite the author's characters,
    // and a straight quote kame exported would not read back as itself.
    let options = Options::ENABLE_FOOTNOTES | Options::ENABLE_TABLES;
    for event in Parser::new_ext(body, options) {
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
            blocks.insert(0, new_block(BlockKind::Heading { level: 1 }, title.clone()));
        }
    }

    let mut document = Document::new(title.as_deref().unwrap_or(fallback_title));
    // A new document's lone empty block stands in when nothing was imported, so
    // the caret always has somewhere to go.
    if !blocks.is_empty() {
        document.blocks = blocks;
    }
    document.annotations = annotations;
    Imported { document, report }
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
struct FrontMatter<'a> {
    title: Option<String>,
    body: &'a str,
}

fn split_front_matter(source: &str) -> Option<FrontMatter<'_>> {
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
    let mut title = None;
    for (index, line) in lines.enumerate() {
        consumed += line.len();
        let trimmed = line.trim_end();
        if index == 0 && trimmed.is_empty() {
            return None;
        }
        if trimmed == "---" || trimmed == "..." {
            return Some(FrontMatter {
                title,
                body: &source[consumed..],
            });
        }
        if let Some((key, value)) = metadata_pair(trimmed) {
            if key.eq_ignore_ascii_case("title") {
                title = title.or_else(|| non_empty(unquote(value)));
            }
        }
    }
    None
}

/// Pelican's `Key: value` header: the opening block of lines, with indented
/// continuations. Prose can open the same way ("Note: this…"), so every line
/// must fit and the block must carry a title or date to count as metadata.
fn pelican_front_matter(source: &str) -> Option<FrontMatter<'_>> {
    let mut consumed = 0;
    let mut title = None;
    let mut identified = false;
    let mut any = false;
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
        if key.eq_ignore_ascii_case("title") {
            title = title.or_else(|| non_empty(value));
            identified = true;
        } else if key.eq_ignore_ascii_case("date") {
            identified = true;
        }
        any = true;
        consumed += line.len();
    }
    identified.then(|| FrontMatter {
        title,
        body: &source[consumed..],
    })
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

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
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
                self.report.inline_code += 1;
                self.push_text(&text);
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
                self.report.rules += 1;
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
                match tag {
                    Tag::Table(_) => self.report.tables += 1,
                    Tag::CodeBlock(_) => self.report.code_blocks += 1,
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
            Tag::Item => {
                self.flush();
                if self.lists.len() > 1 {
                    self.report.nested_list_items += 1;
                }
            }
            Tag::TableCell => {
                if !self.text.is_empty() {
                    self.push_text(" | ");
                }
            }
            Tag::Emphasis => self.open_style(SpanStyle::Italic),
            Tag::Strong => self.open_style(SpanStyle::Bold),
            Tag::Link { dest_url, .. } => self.open_style(SpanStyle::Link {
                url: dest_url.to_string(),
            }),
            Tag::Image { dest_url, .. } => {
                // An image block cannot sit inside a footnote body, so there
                // its description stays as the text.
                if self.footnote.is_none() {
                    self.flush();
                    let path = self.resolve_image(&dest_url);
                    self.blocks
                        .push(new_block(BlockKind::Image { path }, String::new()));
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
        if let Some(level) = self.heading {
            BlockKind::Heading { level }
        } else if let Some(ordered) = self.lists.last() {
            BlockKind::ListItem { ordered: *ordered }
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
                (BlockKind::Heading { level: 1 }, "One".to_string()),
                paragraph("Body text."),
                (BlockKind::Heading { level: 2 }, "Two".to_string()),
                (BlockKind::BlockQuote, "Quoted.".to_string()),
                (BlockKind::ListItem { ordered: false }, "a".to_string()),
                (BlockKind::ListItem { ordered: false }, "b".to_string()),
                (BlockKind::ListItem { ordered: true }, "c".to_string()),
            ]
        );
    }

    #[test]
    fn hard_wrapped_lines_join_into_one_paragraph() {
        let doc = import("Lately, I've been\ngoing through\na period.\n").document;
        assert_eq!(
            outline(&doc),
            vec![paragraph("Lately, I've been going through a period.")]
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
                (BlockKind::Heading { level: 1 }, "Title".to_string()),
                (BlockKind::Heading { level: 2 }, "Section".to_string()),
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
    fn code_keeps_its_words_as_plain_text_and_is_reported() {
        let imported = import("Run `make` first.\n\n```c\nint a;\nint b;\n```\n");
        assert_eq!(
            outline(&imported.document),
            vec![paragraph("Run make first."), paragraph("int a;\nint b;")]
        );
        assert_eq!(imported.report.inline_code, 1);
        assert_eq!(imported.report.code_blocks, 1);
    }

    #[test]
    fn code_keeps_its_indentation() {
        let imported = import("```\n    indented\n  less\n```\n");
        assert_eq!(
            outline(&imported.document),
            vec![paragraph("    indented\n  less")]
        );
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
    fn a_horizontal_rule_is_dropped_and_reported() {
        let imported = import("One.\n\n***\n\nTwo.\n");
        assert_eq!(
            outline(&imported.document),
            vec![paragraph("One."), paragraph("Two.")]
        );
        assert_eq!(imported.report.rules, 1);
    }

    #[test]
    fn nested_lists_flatten_and_are_reported() {
        let imported = import("- outer\n  - inner\n- next\n");
        let bullet = BlockKind::ListItem { ordered: false };
        assert_eq!(
            outline(&imported.document),
            vec![
                (bullet.clone(), "outer".to_string()),
                (bullet.clone(), "inner".to_string()),
                (bullet, "next".to_string()),
            ]
        );
        assert_eq!(imported.report.nested_list_items, 1);
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
                (BlockKind::Heading { level: 1 }, "On Burnout".to_string()),
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
                (BlockKind::Heading { level: 1 }, "On Burnout".to_string()),
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
                (BlockKind::Heading { level: 1 }, "Same".to_string()),
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
            code_blocks: 1,
            tables: 1,
            rules: 1,
            ..ImportReport::default()
        };
        assert_eq!(
            one.summary().unwrap(),
            "1 code block as plain text, 1 table as plain text, 1 horizontal rule dropped"
        );
        let two = ImportReport {
            code_blocks: 2,
            front_matter: true,
            ..ImportReport::default()
        };
        assert_eq!(
            two.summary().unwrap(),
            "2 code blocks as plain text, front matter left out"
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
            BlockKind::ListItem { ordered: false },
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
        for event in Parser::new_ext(body, Options::ENABLE_FOOTNOTES | Options::ENABLE_TABLES) {
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
}
