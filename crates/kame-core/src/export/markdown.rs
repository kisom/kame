//! Markdown export.
//!
//! `to_markdown` renders a [`Document`] as CommonMark-flavoured Markdown so a
//! draft can leave kame and live anywhere: Pandoc, a static site, a code
//! review. The renderer is deliberately lossless in one direction only —
//! Markdown keeps the text, the block structure, bold/italic runs and footnotes.
//! Marginalia have no Markdown form and are the cost of leaving kame's own format;
//! the HTML export keeps them. Notes are the author's own and never publish.

use crate::model::{
    is_script_url, list_positions, AnnotationKind, Block, BlockId, BlockKind, Document, Metadata,
    SpanStyle,
};
use crate::references::{self, Reference, References, Run, RunStyle};
use std::collections::HashMap;

/// Render `doc` as Markdown.
///
/// Layout rules:
/// - headings become `#` runs (level clamped to 1–6),
/// - ordered list items are numbered per consecutive run, unordered ones use `- `,
/// - prose is escaped: backslash, asterisk, underscore, backtick, square and angle
///   brackets in the text are backslash-escaped so they cannot open emphasis,
///   code, a link or raw HTML, and a paragraph whose first glyph would start block
///   structure (`#`, `>`, `-`, `+`, `=`, `~`, or `1.`/`1)`) has that glyph
///   escaped too, so it stays a paragraph,
/// - blocks are separated by a blank line and the document ends in a newline,
/// - a line break inside a block becomes a hard break,
/// - inline spans become `*italic*` / `**bold**`, nesting innermost-first,
/// - footnotes get `[^n]` inline at the end of their anchored range and a
///   `[^n]: body` line after the body, numbered in document order,
/// - marginalia do not publish, for want of a Markdown form,
/// - notes do not publish,
/// - citations are kept as the text they are, and [`to_markdown_with`] lists the sources cited.
///
/// The output depends only on the document contents: footnote numbering follows
/// block order rather than the order annotations happen to be stored in, so
/// exporting the same document twice (or after a serde round trip) is stable.
pub fn to_markdown(doc: &Document) -> String {
    markdown_with(doc, &References::new())
}

/// [`to_markdown`], ending in a `## References` section for the sources `doc` cites, if any.
/// Marginals aren't exported, so one that cites a source doesn't list it.
fn markdown_with(doc: &Document, references: &References) -> String {
    let footnotes = numbered_footnotes(doc);
    let mut by_block: HashMap<BlockId, Vec<(usize, usize)>> = HashMap::new();
    for (number, annotation) in &footnotes {
        let end = annotation.anchor.byte_range().map(|(_, e)| e).unwrap_or(0);
        by_block
            .entry(annotation.anchor.block_id())
            .or_default()
            .push((end, *number));
    }

    let mut parts: Vec<String> = Vec::new();
    let positions = list_positions(&doc.blocks, true);
    // The width of each open list's marker, for indenting what is nested inside it.
    let mut marker_widths: Vec<usize> = Vec::new();
    for (index, block) in doc.blocks.iter().enumerate() {
        // An image is a block with no text: emit it as a Markdown image before
        // the empty-text skip drops it.
        if let BlockKind::Image { path, link, .. } = &block.kind {
            let image = format!("![]({})", link_destination(path));
            parts.push(match link.as_deref().filter(|url| !is_script_url(url)) {
                Some(url) => format!("[{image}]({})", link_destination(url)),
                None => image,
            });
            continue;
        }
        if block.kind == BlockKind::Rule {
            parts.push("---".to_string());
            continue;
        }
        if let (BlockKind::Code { language }, false) = (&block.kind, block.text.is_empty()) {
            parts.push(code_fence(language.as_deref(), &block.text));
            continue;
        }
        // An empty block carries no text and no apparatus: exporting it would
        // only add stray blank lines. Checked before the prefix match so that a
        // just-emptied list item cannot consume an ordered number (or reset the
        // run) for a line that is never exported.
        if block.text.is_empty() {
            continue;
        }
        let mut continuation = None;
        let prefix = match block.kind {
            BlockKind::Heading { level, .. } => {
                format!("{} ", "#".repeat(level.clamp(1, 6) as usize))
            }
            BlockKind::ListItem { .. } => {
                let position = positions[index].expect("a list item has a position");
                let marker = position
                    .number
                    .map_or("- ".to_string(), |number| format!("{number}. "));
                // A nested item has to start past its parents' markers, whose width
                // depends on what they are.
                let level = usize::from(position.depth);
                marker_widths.truncate(level);
                let indent = " ".repeat(marker_widths.iter().sum());
                marker_widths.push(marker.len());
                continuation = (level > 0).then(|| " ".repeat(marker_widths.iter().sum()));
                format!("{indent}{marker}")
            }
            BlockKind::Paragraph => String::new(),
            BlockKind::BlockQuote | BlockKind::Prose => "> ".to_string(),
            BlockKind::Image { .. } | BlockKind::Rule | BlockKind::Code { .. } => {
                unreachable!("these are emitted before the prefix match, or empty")
            }
        };
        let markers = by_block.get(&block.id).map(Vec::as_slice).unwrap_or(&[]);
        let mut body = line_breaks(&render_block_text(block, markers), &block.kind);
        // A paragraph's own text must not turn into block structure. Headings
        // and list items already carry a marker, so they are left alone.
        if block.kind == BlockKind::Paragraph {
            escape_block_start(&mut body);
        }
        // Every line of a quoted passage keeps the `>` marker, so a pasted
        // multi-line quote still exports as one block quote.
        if matches!(block.kind, BlockKind::BlockQuote | BlockKind::Prose) {
            body = body.replace('\n', "\n> ");
        }
        if let Some(continuation) = continuation.take() {
            body = body.replace('\n', &format!("\n{continuation}"));
        }
        parts.push(format!("{prefix}{body}"));
    }

    let mut out = String::new();
    if !parts.is_empty() {
        out.push_str(&parts.join("\n\n"));
        out.push('\n');
    }
    if !footnotes.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        let lines: Vec<String> = footnotes
            .iter()
            .map(|(number, annotation)| {
                let mut body = String::new();
                // One line: a footnote definition ends at a blank line, and what
                // followed would parse as blocks of its own.
                push_escaped(&mut body, &annotation.body.replace(['\r', '\n'], " "));
                format!("[^{number}]: {body}")
            })
            .collect();
        out.push_str(&lines.join("\n"));
        out.push('\n');
    }
    let cited = cited_in_output(doc, &footnotes, references);
    if !cited.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        let entries: Vec<String> = cited
            .iter()
            .map(|(key, reference)| {
                let mut entry = String::new();
                push_escaped(&mut entry, &format!("[{key}]"));
                entry.push(' ');
                entry.push_str(&reference_markdown(reference));
                entry
            })
            .collect();
        out.push_str(&format!("## References\n\n{}\n", entries.join("\n\n")));
    }
    out
}

/// `doc` with the footnotes that aren't written taken out: those whose block is gone or empty.
fn as_written(doc: &Document, footnotes: &[(usize, &crate::model::Annotation)]) -> Document {
    let mut written = doc.clone();
    written.annotations.retain(|annotation| {
        annotation.kind != AnnotationKind::Footnote
            || footnotes.iter().any(|(_, kept)| kept.id == annotation.id)
    });
    written
}

/// The sources cited in what is written: footnotes that aren't, because their block is gone or
/// empty, don't count, or a source would be listed with no citation in the output.
fn cited_in_output<'a>(
    doc: &Document,
    footnotes: &[(usize, &crate::model::Annotation)],
    references: &'a References,
) -> Vec<(&'a str, &'a Reference)> {
    if references.is_empty() {
        return Vec::new();
    }
    references::cited(&as_written(doc, footnotes), references, false)
}

/// Keys cited in what [`to_markdown`] writes that have no reference and look like a suggested
/// key, as [`references::unresolved`] finds them. A key cited only in a footnote the file leaves
/// out, or in a marginal, isn't named.
pub fn markdown_unresolved(doc: &Document, references: &References) -> Vec<String> {
    let footnotes = numbered_footnotes(doc);
    references::unresolved(&as_written(doc, &footnotes), references, false)
}

/// One entry's text: titles in italics, a web or mail address as an autolink, and any other
/// address as text. A line break becomes a space, to stay one paragraph.
fn reference_markdown(reference: &Reference) -> String {
    let mut out = String::new();
    for Run { text, style } in references::entry(reference) {
        let text = text.replace(['\r', '\n'], " ");
        match style {
            RunStyle::Plain => push_escaped(&mut out, &text),
            RunStyle::Italic => {
                // Emphasis can't open before a space or close after one, so the spaces at its
                // edges go outside.
                let inner = text.trim();
                if inner.is_empty() {
                    out.push_str(&text);
                    continue;
                }
                let start = text.len() - text.trim_start().len();
                out.push_str(&text[..start]);
                out.push('*');
                push_escaped(&mut out, inner);
                out.push('*');
                out.push_str(&text[start + inner.len()..]);
            }
            RunStyle::Url if references::linkable_url(&text) => out.push_str(&format!("<{text}>")),
            RunStyle::Url => push_escaped(&mut out, &text),
        }
    }
    out
}

/// Render `doc` for publishing: [`to_markdown`] under a `Key: value` header, the
/// form Pelican reads, leaving out any field that is empty.
///
/// A first heading repeating the title is dropped, since a blog renders the title
/// itself, unless a footnote is anchored in it. The date is the published date,
/// or the created date for a document not yet published. A source cited by the document that
/// `references` holds is listed after the footnotes.
pub fn to_markdown_with(doc: &Document, metadata: &Metadata, references: &References) -> String {
    let date = if metadata.published.is_empty() {
        &metadata.created
    } else {
        &metadata.published
    };
    let tags = metadata.tags.join(", ");
    let header: String = [
        ("Title", metadata.title.as_str()),
        ("Date", date.as_str()),
        ("Author", metadata.author.as_str()),
        ("Tags", tags.as_str()),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(key, value)| format!("{key}: {}\n", header_value(value)))
    .collect();

    let body = markdown_with(
        &super::without_repeated_title(doc, &metadata.title),
        references,
    );

    match (header.is_empty(), body.is_empty()) {
        (true, _) => body,
        (false, true) => header,
        (false, false) => format!("{header}\n{body}"),
    }
}

/// A header value on one line: any line break or control character would let the
/// value start a header field of its own, or end the header early.
fn header_value(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Footnotes in deterministic document order, numbered from 1.
///
/// Only footnotes whose anchor block is still present — and non-empty, so that
/// the block is actually exported — are kept: an orphaned annotation has
/// nowhere to put its inline marker. Order is block order, then anchor
/// position, then annotation id, so the numbering never depends on how the
/// annotations happen to be ordered in storage.
fn numbered_footnotes(doc: &Document) -> Vec<(usize, &crate::model::Annotation)> {
    let block_order: HashMap<BlockId, usize> = doc
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect();

    let mut footnotes: Vec<(usize, usize, usize, &crate::model::Annotation)> = doc
        .annotations
        .iter()
        .filter(|annotation| annotation.kind == AnnotationKind::Footnote)
        .filter_map(|annotation| {
            let index = *block_order.get(&annotation.anchor.block_id())?;
            if doc.blocks[index].text.is_empty() {
                return None;
            }
            let (start, end) = annotation.anchor.byte_range().unwrap_or((0, 0));
            Some((index, start, end, annotation))
        })
        .collect();
    footnotes.sort_by_key(|(index, start, end, annotation)| (*index, *start, *end, annotation.id));

    footnotes
        .into_iter()
        .enumerate()
        .map(|(offset, (_, _, _, annotation))| (offset + 1, annotation))
        .collect()
}

/// Render one block's text with span markers and footnote markers applied.
///
/// `footnotes` holds `(byte offset, number)` pairs; the marker is inserted just
/// after the anchored range.
///
/// The text is cut at every span edge (and every footnote offset) and the
/// active style set is taken per segment, the way `layout::text::styled_spans`
/// styles runs. Delimiters are closed innermost-first at each boundary and
/// reopened only for styles that keep covering the following text, so the
/// markers stay properly nested even when spans overlap — emitting each span's
/// pair independently would cross them (`*abcde**fghij*klmno**`) and CommonMark
/// would print the asterisks literally.
fn render_block_text(block: &Block, footnotes: &[(usize, usize)]) -> String {
    let text = block.text.as_str();

    // Span edges snapped to char boundaries, then pulled in past whitespace: a
    // delimiter hugging a space (`**Hello **world`) is read as literal
    // asterisks, so emphasis has to start and end on a glyph.
    let spans: Vec<(usize, usize, SpanStyle)> = block
        .spans
        .iter()
        // A script link publishes as its text, as does a link to a kame document, which
        // can't lead anywhere outside kame. A link to a heading stays in this document.
        .filter(|span| !matches!(&span.style, SpanStyle::Link { url } if is_script_url(url)))
        .filter(|span| {
            !matches!(&span.style, SpanStyle::DocumentLink { target } if !target.starts_with('#'))
        })
        .filter_map(|span| {
            let start = snap_up(text, span.start);
            let end = snap_down(text, span.end);
            let (start, end) = trim_whitespace_edges(text, start, end);
            (start < end).then(|| (start, end, span.style.clone()))
        })
        .collect();

    // Footnote markers snapped the same way, in deterministic order.
    let mut footnotes: Vec<(usize, usize)> = footnotes
        .iter()
        .map(|(offset, number)| (snap_down(text, *offset), *number))
        .collect();
    footnotes.sort_unstable();

    let mut bounds = Vec::with_capacity(spans.len() * 2 + footnotes.len() + 2);
    bounds.push(0);
    bounds.push(text.len());
    for (start, end, _) in &spans {
        bounds.push(*start);
        bounds.push(*end);
    }
    for (offset, _) in &footnotes {
        bounds.push(*offset);
    }
    bounds.sort_unstable();
    bounds.dedup();

    let mut out = String::with_capacity(text.len() + bounds.len() * 2 + footnotes.len() * 4);
    // Delimiters currently open, outermost first: always a prefix of `wanted`
    // for the segment being emitted, which is what keeps the output nested.
    let mut open: Vec<SpanStyle> = Vec::new();
    // What closes the code span that is open, which depends on its text.
    let mut code_close = String::new();
    for window in bounds.windows(2) {
        let (start, end) = (window[0], window[1]);
        if start >= end {
            continue;
        }
        // A link wraps everything inside it, so it goes first (outermost);
        // emphasis nests under it in `style_order()`.
        let mut wanted: Vec<SpanStyle> = Vec::new();
        if let Some((_, _, style)) = spans.iter().find(|(s, e, style)| {
            matches!(
                style,
                SpanStyle::Link { .. } | SpanStyle::DocumentLink { .. }
            ) && *s <= start
                && *e >= end
        }) {
            wanted.push(style.clone());
        }
        wanted.extend(style_order().into_iter().filter(|style| {
            spans
                .iter()
                .any(|(s, e, covered)| covered == style && *s <= start && *e >= end)
        }));

        // Close from the inside out, but only what stopped covering the text.
        // A style that still covers it but sits under one that just ended stays
        // open and its run extends, rather than being reopened with a stack of
        // adjacent asterisks that no parser can unpick.
        // A footnote marker inside code would be read as code, so code closes for it.
        let marker_here = footnotes.iter().any(|(offset, _)| *offset == start);
        while let Some(innermost) = open.last().cloned() {
            let is_code = innermost == SpanStyle::Code;
            if wanted.contains(&innermost) && !(is_code && marker_here) {
                break;
            }
            if is_code {
                out.push_str(&code_close);
            } else {
                out.push_str(&close_delimiter(&innermost));
            }
            open.pop();
        }
        // A marker at a span edge belongs after the closing and before the
        // opening delimiters, matching where the superscript renders.
        for (_, number) in footnotes.iter().filter(|(offset, _)| *offset == start) {
            out.push_str(&format!("[^{number}]"));
        }
        for style in &wanted {
            if open.contains(style) {
                continue;
            }
            if *style == SpanStyle::Code {
                let whole = spans
                    .iter()
                    .find(|(s, e, covered)| *covered == *style && *s <= start && *e >= end)
                    .map_or(&text[start..end], |(s, e, _)| &text[*s..*e]);
                let (opening, closing) = code_delimiters(whole);
                out.push_str(&opening);
                code_close = closing;
            } else {
                out.push_str(&open_delimiter(style));
            }
            open.push(style.clone());
        }
        // What is inside a code span is not Markdown, so it isn't escaped either.
        if open.contains(&SpanStyle::Code) {
            out.push_str(&text[start..end]);
        } else {
            push_escaped(&mut out, &text[start..end]);
        }
    }
    while let Some(style) = open.pop() {
        if style == SpanStyle::Code {
            out.push_str(&code_close);
        } else {
            out.push_str(&close_delimiter(&style));
        }
    }
    // A marker past the last glyph has no segment of its own to sit in.
    for (_, number) in footnotes.iter().filter(|(offset, _)| *offset == text.len()) {
        out.push_str(&format!("[^{number}]"));
    }
    out
}

/// Markdown punctuation that would start emphasis, code or a link if it were
/// emitted verbatim. Escaped per character, so cutting the text at a span edge
/// cannot leave half an escape sequence in one segment.
const ESCAPED_IN_TEXT: [char; 8] = ['\\', '*', '_', '`', '[', ']', '<', '>'];

/// Appends `text` with the characters in [`ESCAPED_IN_TEXT`] backslash-escaped.
fn push_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        if ESCAPED_IN_TEXT.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
}

/// A code block as a fence, its text exact. The fence is longer than any run of
/// backticks in the text, which would otherwise close it.
fn code_fence(language: Option<&str>, text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let language = language.unwrap_or("");
    let text = text.trim_end_matches('\n');
    format!("{fence}{language}\n{text}\n{fence}")
}

/// Writes a block's line breaks as Markdown hard breaks, since a bare newline
/// renders as a space. Each line after a break could open block structure, so it
/// is escaped the way a paragraph's first line is. A heading is one line by
/// definition, so its breaks become spaces.
fn line_breaks(rendered: &str, kind: &BlockKind) -> String {
    let rendered = rendered.trim_end_matches('\n');
    if matches!(kind, BlockKind::Heading { .. }) {
        return rendered.replace('\n', " ");
    }
    let mut lines = rendered.split('\n');
    let mut out = lines.next().unwrap_or("").to_string();
    for line in lines {
        let mut line = line.to_string();
        escape_block_start(&mut line);
        out.push_str("\\\n");
        out.push_str(&line);
    }
    out
}

/// Escapes the first glyph of a rendered paragraph when CommonMark would read
/// it as block structure: `#` (heading), `>` (quote), `-`, `+`, `=`, `~` (list
/// or setext underline), and a digit run followed by `.`/`)` (ordered list).
///
/// The escape goes on the punctuation, not on a leading digit: `\1.` is not a
/// valid CommonMark escape and would print the backslash, while `1\.` is.
fn escape_block_start(rendered: &mut String) {
    let Some(first) = rendered.chars().next() else {
        return;
    };
    match first {
        '#' | '>' | '-' | '+' | '=' | '~' => rendered.insert(0, '\\'),
        '0'..='9' => {
            let digits_end = rendered
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rendered.len());
            let mut rest = rendered[digits_end..].chars();
            let marker = rest.next();
            if matches!(marker, Some('.') | Some(')'))
                && matches!(rest.next(), None | Some(' ') | Some('\t'))
            {
                rendered.insert(digits_end, '\\');
            }
        }
        _ => {}
    }
}

/// Opening order for the two emphasis styles: bold outside, italic inside, so
/// a segment carrying both exports as `**a *b***`. A link is not listed here:
/// it is pushed first and therefore sits outermost, wrapping any emphasis.
fn style_order() -> [SpanStyle; 3] {
    [SpanStyle::Bold, SpanStyle::Italic, SpanStyle::Code]
}

/// The backticks around a code span of `text`, more than any run inside it. Text that
/// starts or ends in a backtick needs a space between it and them, which is not part of
/// the code.
fn code_delimiters(text: &str) -> (String, String) {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat(longest + 1);
    if text.starts_with('`') || text.ends_with('`') {
        (format!("{ticks} "), format!(" {ticks}"))
    } else {
        (ticks.clone(), ticks)
    }
}

/// The marker that opens `style`'s run.
fn open_delimiter(style: &SpanStyle) -> String {
    match style {
        SpanStyle::Bold => "**".to_string(),
        SpanStyle::Italic => "*".to_string(),
        SpanStyle::Link { .. } | SpanStyle::DocumentLink { .. } => "[".to_string(),
        SpanStyle::Code => unreachable!("a code span's delimiter depends on its text"),
    }
}

/// The marker that closes `style`'s run. A link is asymmetric: `[...](url)`.
fn close_delimiter(style: &SpanStyle) -> String {
    match style {
        SpanStyle::Bold => "**".to_string(),
        SpanStyle::Italic => "*".to_string(),
        SpanStyle::Link { url } => format!("]({})", link_destination(url)),
        SpanStyle::DocumentLink { target } => format!("](<{}>)", heading_destination(target)),
        SpanStyle::Code => unreachable!("a code span's delimiter depends on its text"),
    }
}

/// A link to a heading as what goes between a Markdown destination's angle brackets, which keep
/// the spaces a heading has. A renderer decodes character references and backslash escapes here,
/// a line break can't be in one at all, and importing it decodes percent signs.
fn heading_destination(target: &str) -> String {
    let mut out = String::with_capacity(target.len());
    for c in target.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '%' => out.push_str("%25"),
            '\\' | '<' | '>' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => {
                let mut bytes = [0; 4];
                for byte in c.encode_utf8(&mut bytes).bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// A URL as a Markdown link destination, meaning the same URL once read back. A
/// renderer decodes character references here, so `&` is escaped, or `&#58;` could
/// decode into a scheme after [`is_script_url`] had passed it. A parenthesis or
/// space would otherwise end the link early, and a leading `<` would make the rest
/// an angle-bracket destination, hiding a scheme from that check.
fn link_destination(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\\' | '(' | ')' | '<' | '>' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_whitespace() || c.is_control() => {
                let mut bytes = [0; 4];
                for byte in c.encode_utf8(&mut bytes).bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Pull a snapped `(start, end)` range in past leading and trailing whitespace,
/// so emphasis sits against glyphs; returns an empty range if nothing is left.
fn trim_whitespace_edges(text: &str, mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end {
        match text[start..end].chars().next() {
            Some(c) if c.is_whitespace() => start += c.len_utf8(),
            _ => break,
        }
    }
    while end > start {
        match text[start..end].chars().next_back() {
            Some(c) if c.is_whitespace() => end -= c.len_utf8(),
            _ => break,
        }
    }
    (start, end)
}

/// Round `offset` up to the next UTF-8 char boundary, clamped to `text.len()`.
fn snap_up(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while offset < text.len() && !text.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

/// Round `offset` down to the previous UTF-8 char boundary, clamped to
/// `text.len()`.
fn snap_down(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Anchor, Annotation, AnnotationKind, Block, BlockId, BlockKind, Span, SpanStyle,
    };
    use uuid::Uuid;

    fn block(kind: BlockKind, text: &str) -> Block {
        Block {
            id: Uuid::new_v4(),
            kind,
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    fn paragraph(text: &str) -> Block {
        block(BlockKind::Paragraph, text)
    }

    fn annotation(
        kind: AnnotationKind,
        block_id: BlockId,
        start: usize,
        end: usize,
        body: &str,
    ) -> Annotation {
        Annotation {
            id: Uuid::new_v4(),
            kind,
            anchor: Anchor::range(block_id, start, end),
            body: body.to_string(),
        }
    }

    fn footnote(block_id: BlockId, start: usize, end: usize, body: &str) -> Annotation {
        annotation(AnnotationKind::Footnote, block_id, start, end, body)
    }

    fn document(blocks: Vec<Block>, annotations: Vec<Annotation>) -> Document {
        Document {
            id: Uuid::new_v4(),
            title: "Doc".to_string(),
            blocks,
            annotations,
        }
    }

    fn rich_document() -> Document {
        let mut styled = paragraph("Hello world");
        styled.spans = vec![
            Span {
                start: 0,
                end: 5,
                style: SpanStyle::Bold,
            },
            Span {
                start: 6,
                end: 11,
                style: SpanStyle::Italic,
            },
        ];
        let bullet = block(
            BlockKind::ListItem {
                ordered: false,
                depth: 0,
            },
            "Bullet",
        );
        let ordered = block(
            BlockKind::ListItem {
                ordered: true,
                depth: 0,
            },
            "First",
        );
        let note_block = paragraph("Anchored text");
        let note_id = note_block.id;
        document(
            vec![
                block(
                    BlockKind::Heading {
                        level: 2,
                        centered: false,
                    },
                    "Section",
                ),
                styled,
                bullet,
                ordered,
                note_block,
            ],
            vec![
                footnote(note_id, 0, 8, "Footnote body."),
                annotation(AnnotationKind::Marginal, note_id, 0, 8, "margin"),
            ],
        )
    }

    #[test]
    fn headings_and_paragraphs_export() {
        let doc_with_heading_and_para = document(
            vec![
                block(
                    BlockKind::Heading {
                        level: 1,
                        centered: false,
                    },
                    "Title",
                ),
                paragraph("Body text"),
            ],
            Vec::new(),
        );
        let md = to_markdown(&doc_with_heading_and_para);
        assert_eq!(md, "# Title\n\nBody text\n");
    }

    #[test]
    fn block_quotes_export_with_gt_markers() {
        let doc = document(
            vec![
                paragraph("Before."),
                block(BlockKind::BlockQuote, "Quoted words"),
                paragraph("After."),
            ],
            Vec::new(),
        );
        let md = to_markdown(&doc);
        assert_eq!(md, "Before.\n\n> Quoted words\n\nAfter.\n");
    }

    #[test]
    fn images_export_as_markdown_image_syntax() {
        let doc = document(
            vec![
                paragraph("Before."),
                block(
                    BlockKind::Image {
                        path: "/img/a.png".into(),
                        link: None,
                        align: Default::default(),
                    },
                    "",
                ),
                paragraph("After."),
            ],
            Vec::new(),
        );
        let md = to_markdown(&doc);
        assert_eq!(md, "Before.\n\n![](/img/a.png)\n\nAfter.\n");
    }

    #[test]
    fn links_export_as_markdown_links() {
        let mut p = paragraph("click here");
        p.spans = vec![Span {
            start: 0,
            end: 10,
            style: SpanStyle::Link {
                url: "http://x.com".into(),
            },
        }];
        let md = to_markdown(&document(vec![p], Vec::new()));
        assert_eq!(md, "[click here](http://x.com)\n");
    }

    #[test]
    fn footnotes_export_pandoc_style() {
        let body = paragraph("Body text");
        let id = body.id;
        let doc = document(vec![body], vec![footnote(id, 0, 9, "The note body.")]);
        let md = to_markdown(&doc);
        assert_eq!(md, "Body text[^1]\n\n[^1]: The note body.\n");
    }

    #[test]
    fn export_is_deterministic() {
        let d = rich_document();
        assert_eq!(to_markdown(&d), to_markdown(&d));
    }

    #[test]
    fn export_stable_across_round_trip() {
        let d = rich_document();
        let json = serde_json::to_string(&d).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(to_markdown(&d), to_markdown(&back));
    }

    #[test]
    fn empty_document_exports_without_panic() {
        assert_eq!(to_markdown(&Document::new("")), "");
    }

    #[test]
    fn lists_export_with_bullets_and_numbers() {
        let doc = document(
            vec![
                block(
                    BlockKind::ListItem {
                        ordered: false,
                        depth: 0,
                    },
                    "One",
                ),
                block(
                    BlockKind::ListItem {
                        ordered: false,
                        depth: 0,
                    },
                    "Two",
                ),
                block(
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                    "First",
                ),
                block(
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                    "Second",
                ),
                block(
                    BlockKind::ListItem {
                        ordered: false,
                        depth: 0,
                    },
                    "Back",
                ),
            ],
            Vec::new(),
        );
        assert_eq!(
            to_markdown(&doc),
            "- One\n\n- Two\n\n1. First\n\n2. Second\n\n- Back\n"
        );
    }

    #[test]
    fn inline_spans_export_markdown_markers() {
        let mut p = paragraph("Hello world");
        p.spans = vec![
            Span {
                start: 0,
                end: 5,
                style: SpanStyle::Bold,
            },
            Span {
                start: 6,
                end: 11,
                style: SpanStyle::Italic,
            },
        ];
        assert_eq!(
            to_markdown(&document(vec![p], Vec::new())),
            "**Hello** *world*\n"
        );
    }

    #[test]
    fn nested_spans_close_inside_out() {
        let mut p = paragraph("Hello world");
        p.spans = vec![
            Span {
                start: 0,
                end: 11,
                style: SpanStyle::Bold,
            },
            Span {
                start: 6,
                end: 11,
                style: SpanStyle::Italic,
            },
        ];
        assert_eq!(
            to_markdown(&document(vec![p], Vec::new())),
            "**Hello *world***\n"
        );
    }

    #[test]
    fn footnote_numbers_follow_document_order_not_storage_order() {
        let alpha = paragraph("Alpha");
        let beta = paragraph("Beta");
        let (alpha_id, beta_id) = (alpha.id, beta.id);
        let doc = document(
            vec![alpha, beta],
            vec![
                footnote(beta_id, 0, 5, "second"),
                footnote(alpha_id, 0, 5, "first"),
            ],
        );
        assert_eq!(
            to_markdown(&doc),
            "Alpha[^1]\n\nBeta[^2]\n\n[^1]: first\n[^2]: second\n"
        );
    }

    #[test]
    fn orphan_footnote_is_skipped() {
        let body = paragraph("Body");
        let doc = document(vec![body], vec![footnote(Uuid::new_v4(), 0, 4, "nowhere")]);
        assert_eq!(to_markdown(&doc), "Body\n");
    }

    #[test]
    fn footnote_in_empty_block_is_not_exported() {
        let empty = paragraph("");
        let empty_id = empty.id;
        let doc = document(
            vec![empty, paragraph("Body")],
            vec![footnote(empty_id, 0, 0, "nowhere")],
        );
        assert_eq!(to_markdown(&doc), "Body\n");
    }

    #[test]
    fn multiline_text_and_out_of_range_anchors_do_not_panic() {
        let mut p = paragraph("Ünïcode");
        p.spans = vec![
            Span {
                start: 4,
                end: 5, // 4 sits inside "ï": snapped to 5, so the span is dropped
                style: SpanStyle::Bold,
            },
            Span {
                start: 0,
                end: 999, // clamped to the end of the text
                style: SpanStyle::Italic,
            },
        ];
        let id = p.id;
        // Anchor end 4 sits inside "ï" and is snapped down to 3.
        let doc = document(vec![p], vec![footnote(id, 0, 4, "snapped")]);
        assert_eq!(to_markdown(&doc), "*Ün[^1]ïcode*\n\n[^1]: snapped\n");
    }

    #[test]
    fn heading_levels_are_clamped_to_six() {
        let doc = document(
            vec![block(
                BlockKind::Heading {
                    level: 0,
                    centered: false,
                },
                "Low",
            )],
            Vec::new(),
        );
        assert_eq!(to_markdown(&doc), "# Low\n");
        let doc = document(
            vec![block(
                BlockKind::Heading {
                    level: 9,
                    centered: false,
                },
                "High",
            )],
            Vec::new(),
        );
        assert_eq!(to_markdown(&doc), "###### High\n");
    }

    #[test]
    fn span_edges_are_trimmed_past_whitespace() {
        // Selection styling often leaves a space inside the span; a delimiter
        // hugging that space (`**Hello **world`) is read as literal asterisks.
        let mut trailing = paragraph("Hello world");
        trailing.spans = vec![Span {
            start: 0,
            end: 6, // covers "Hello "
            style: SpanStyle::Bold,
        }];
        let mut leading = paragraph("Hello world");
        leading.spans = vec![Span {
            start: 5,
            end: 11, // covers " world"
            style: SpanStyle::Bold,
        }];
        let mut only_space = paragraph("Hello world");
        only_space.spans = vec![Span {
            start: 5,
            end: 6, // covers " " only: nothing to emphasise
            style: SpanStyle::Bold,
        }];
        let doc = document(vec![trailing, leading, only_space], Vec::new());
        assert_eq!(
            to_markdown(&doc),
            "**Hello** world\n\nHello **world**\n\nHello world\n"
        );
    }

    #[test]
    fn empty_ordered_item_does_not_skip_a_number() {
        // Enter at the end of a list item leaves an empty item behind; it must
        // not consume a number (nor reset the run) that no line shows.
        let doc = document(
            vec![
                block(
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                    "First",
                ),
                block(
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                    "",
                ),
                block(
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                    "Second",
                ),
            ],
            Vec::new(),
        );
        assert_eq!(to_markdown(&doc), "1. First\n\n2. Second\n");

        // An emptied paragraph is likewise not allowed to reset the counter.
        let doc = document(
            vec![
                block(
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                    "First",
                ),
                paragraph(""),
                block(
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                    "Second",
                ),
            ],
            Vec::new(),
        );
        assert_eq!(to_markdown(&doc), "1. First\n\n2. Second\n");
    }

    #[test]
    fn overlapping_spans_emit_nested_delimiters() {
        // Italic 0..10 and bold 5..15 cross; emitting each span's delimiters
        // independently would produce `*abcde**fghij*klmno**`, which CommonMark
        // reads as literal asterisks.
        let mut p = paragraph("abcdefghijklmnop");
        p.spans = vec![
            Span {
                start: 0,
                end: 10,
                style: SpanStyle::Italic,
            },
            Span {
                start: 5,
                end: 15,
                style: SpanStyle::Bold,
            },
        ];
        assert_eq!(
            to_markdown(&document(vec![p], Vec::new())),
            "*abcde**fghijklmno***p\n"
        );
    }

    /// The Markdown for one paragraph, with nothing else in the document.
    fn exported(text: &str) -> String {
        to_markdown(&document(vec![paragraph(text)], Vec::new()))
    }

    #[test]
    fn prose_that_looks_like_block_structure_stays_a_paragraph() {
        // Each of these would silently change the exported structure if the
        // paragraph's own text were copied through unescaped.
        for (text, expected) in [
            ("# not a heading", "\\# not a heading\n"),
            ("> not a quote", "\\> not a quote\n"),
            ("- not a list", "\\- not a list\n"),
            ("+ not a list", "\\+ not a list\n"),
            ("---", "\\---\n"),
            ("= setext underline", "\\= setext underline\n"),
            ("~ setext underline", "\\~ setext underline\n"),
            ("1. not an ordered item", "1\\. not an ordered item\n"),
            ("2) not an ordered item", "2\\) not an ordered item\n"),
        ] {
            assert_eq!(
                exported(text),
                expected,
                "prose {text:?} must not become block structure"
            );
        }
    }

    #[test]
    fn inline_punctuation_is_escaped_inside_prose() {
        assert_eq!(
            exported("a * b _ c ` d [e] \\ f"),
            "a \\* b \\_ c \\` d \\[e\\] \\\\ f\n"
        );
    }

    #[test]
    fn escaping_holds_across_a_span_boundary() {
        // Asterisks on both sides of a bold run: escaping is per segment, so
        // neither segment can be left with half an escape sequence.
        let mut p = paragraph("*bold*");
        p.spans = vec![Span {
            start: 1,
            end: 5,
            style: SpanStyle::Bold,
        }];
        assert_eq!(
            to_markdown(&document(vec![p], Vec::new())),
            "\\***bold**\\*\n"
        );
    }

    #[test]
    fn footnote_bodies_are_escaped_too() {
        let body = paragraph("Body");
        let id = body.id;
        let doc = document(vec![body], vec![footnote(id, 0, 4, "a *b* [c]")]);
        assert_eq!(to_markdown(&doc), "Body[^1]\n\n[^1]: a \\*b\\* \\[c\\]\n");
    }

    #[test]
    fn a_line_break_exports_as_a_hard_break() {
        let mut doc = Document::new("t");
        doc.blocks[0].text = "first\nsecond".to_string();
        assert_eq!(to_markdown(&doc), "first\\\nsecond\n");
    }

    #[test]
    fn a_line_after_a_break_cannot_become_block_structure() {
        let mut doc = Document::new("t");
        doc.blocks[0].text = "first\n# second".to_string();
        assert_eq!(to_markdown(&doc), "first\\\n\\# second\n");
    }

    #[test]
    fn a_line_break_in_a_heading_is_a_space() {
        let mut doc = Document::new("t");
        doc.blocks[0].kind = BlockKind::Heading {
            level: 2,
            centered: false,
        };
        doc.blocks[0].text = "first\nsecond".to_string();
        assert_eq!(to_markdown(&doc), "## first second\n");
    }

    #[test]
    fn a_trailing_line_break_leaves_no_stray_backslash() {
        let mut doc = Document::new("t");
        doc.blocks[0].text = "first\n".to_string();
        assert_eq!(to_markdown(&doc), "first\n");
    }

    fn metadata() -> crate::model::Metadata {
        crate::model::Metadata {
            title: "On Burnout".to_string(),
            author: "K. Isom".to_string(),
            created: "2019-01-01".to_string(),
            published: "2019-01-02".to_string(),
            tags: vec!["life".to_string(), "work".to_string()],
            ..crate::model::Metadata::default()
        }
    }

    #[test]
    fn publishing_writes_front_matter_in_a_fixed_order() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("Body.")];
        assert_eq!(
            to_markdown_with(&doc, &metadata(), &References::new()),
            "Title: On Burnout\nDate: 2019-01-02\nAuthor: K. Isom\nTags: life, work\n\nBody.\n"
        );
    }

    #[test]
    fn an_unpublished_document_is_dated_by_its_creation() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("Body.")];
        let meta = crate::model::Metadata {
            published: String::new(),
            ..metadata()
        };
        assert!(to_markdown_with(&doc, &meta, &References::new()).contains("Date: 2019-01-01\n"));
    }

    #[test]
    fn empty_fields_are_left_out_and_no_metadata_means_no_front_matter() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("Body.")];
        let title_only = crate::model::Metadata {
            title: "T".to_string(),
            ..Default::default()
        };
        assert_eq!(
            to_markdown_with(&doc, &title_only, &References::new()),
            "Title: T\n\nBody.\n"
        );
        assert_eq!(
            to_markdown_with(&doc, &crate::model::Metadata::default(), &References::new()),
            "Body.\n"
        );
    }

    /// A blog renders the title itself, so it would otherwise appear twice.
    #[test]
    fn a_first_heading_repeating_the_title_is_not_published_twice() {
        let mut doc = Document::new("t");
        doc.blocks = vec![
            block(
                BlockKind::Heading {
                    level: 1,
                    centered: false,
                },
                "On Burnout",
            ),
            paragraph("Body."),
        ];
        let out = to_markdown_with(&doc, &metadata(), &References::new());
        assert!(!out.contains("# On Burnout"), "{out}");
        assert!(out.contains("Title: On Burnout"), "{out}");
    }

    #[test]
    fn a_different_first_heading_stays() {
        let mut doc = Document::new("t");
        doc.blocks = vec![
            block(
                BlockKind::Heading {
                    level: 1,
                    centered: false,
                },
                "Introduction",
            ),
            paragraph("Body."),
        ];
        assert!(to_markdown_with(&doc, &metadata(), &References::new()).contains("# Introduction"));
    }

    #[test]
    fn a_title_heading_with_a_footnote_keeps_the_footnote() {
        let mut doc = Document::new("t");
        let heading = block(
            BlockKind::Heading {
                level: 1,
                centered: false,
            },
            "On Burnout",
        );
        doc.annotations = vec![annotation(
            AnnotationKind::Footnote,
            heading.id,
            2,
            2,
            "A note.",
        )];
        doc.blocks = vec![heading, paragraph("Body.")];
        assert!(to_markdown_with(&doc, &metadata(), &References::new()).contains("[^1]: A note."));
    }

    #[test]
    fn no_line_break_of_any_kind_can_start_a_header_line() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("Body.")];
        for separator in [
            "\n", "\r", "\r\n", "\u{85}", "\u{2028}", "\u{2029}", "\u{b}", "\u{c}",
        ] {
            let meta = crate::model::Metadata {
                title: format!("T{separator}Author: forged"),
                ..Default::default()
            };
            let out = to_markdown_with(&doc, &meta, &References::new());
            let header = out.split("\n\n").next().unwrap();
            assert_eq!(
                header.lines().count(),
                1,
                "{separator:?} started a line: {out:?}"
            );
            assert!(
                !header.chars().any(|c| matches!(
                    c,
                    '\r' | '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{b}' | '\u{c}'
                )),
                "{separator:?} survived: {out:?}"
            );
        }
    }

    #[test]
    fn a_tag_cannot_start_a_header_line() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("Body.")];
        let meta = crate::model::Metadata {
            tags: vec!["a\nTitle: forged".to_string()],
            ..Default::default()
        };
        let out = to_markdown_with(&doc, &meta, &References::new());
        assert_eq!(
            out.split("\n\n").next().unwrap().lines().count(),
            1,
            "{out:?}"
        );
    }

    fn linked(url: &str) -> Block {
        let mut linked = block(BlockKind::Paragraph, "the demo");
        linked.spans = vec![crate::model::Span {
            start: 0,
            end: 8,
            style: crate::model::SpanStyle::Link {
                url: url.to_string(),
            },
        }];
        linked
    }

    #[test]
    fn a_script_link_exports_as_its_text() {
        let mut doc = Document::new("t");
        doc.blocks = vec![linked("javascript:alert(1)")];
        assert_eq!(to_markdown(&doc), "the demo\n");
    }

    #[test]
    fn a_pdf_link_still_exports() {
        let mut doc = Document::new("t");
        doc.blocks = vec![linked("paper.pdf")];
        assert_eq!(to_markdown(&doc), "[the demo](paper.pdf)\n");
    }

    /// A renderer decodes character references in a destination, so `&#58;` would
    /// turn this back into a working script link after the check had passed.
    #[test]
    fn a_character_reference_in_a_link_cannot_decode_into_a_scheme() {
        let mut doc = Document::new("t");
        doc.blocks = vec![linked("javascript&#58;alert(1)")];
        let out = to_markdown(&doc);
        assert!(!out.contains("&#58;") || out.contains("&amp;#58;"), "{out}");
    }

    #[test]
    fn a_parenthesis_or_space_cannot_end_a_link_early() {
        let mut doc = Document::new("t");
        doc.blocks = vec![linked("a) <script>x</script> (b")];
        let out = to_markdown(&doc);
        assert_eq!(
            out,
            "[the demo](a\\)%20\\<script\\>x\\</script\\>%20\\(b)\n"
        );
    }

    #[test]
    fn an_image_path_with_a_space_stays_one_destination() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(
            BlockKind::Image {
                path: "my pic.png".to_string(),
                link: None,
                align: Default::default(),
            },
            "",
        )];
        assert_eq!(to_markdown(&doc), "![](my%20pic.png)\n");
    }

    #[test]
    fn markdown_has_no_form_for_a_centered_heading() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(
            BlockKind::Heading {
                level: 2,
                centered: true,
            },
            "Words",
        )];
        assert_eq!(to_markdown(&doc), "## Words\n");
    }

    #[test]
    fn markdown_has_no_form_for_an_images_alignment() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(
            BlockKind::Image {
                path: "a.png".to_string(),
                link: None,
                align: crate::model::ImageAlign::Center,
            },
            "",
        )];
        assert_eq!(to_markdown(&doc), "![](a.png)\n");
    }

    #[test]
    fn a_note_leaves_no_trace_in_markdown() {
        let mut doc = Document::new("t");
        let first = paragraph("First.");
        doc.annotations = vec![annotation(
            AnnotationKind::Note,
            first.id,
            0,
            5,
            "SECRET BODY",
        )];
        doc.blocks = vec![first];
        assert_eq!(to_markdown(&doc), "First.\n");
    }

    /// Only what publishes can pin a title heading in place.
    #[test]
    fn a_note_on_the_title_heading_does_not_print_the_title_twice() {
        let mut doc = Document::new("t");
        let heading = block(
            BlockKind::Heading {
                level: 1,
                centered: false,
            },
            "On Burnout",
        );
        doc.annotations = vec![annotation(
            AnnotationKind::Note,
            heading.id,
            0,
            2,
            "private",
        )];
        doc.blocks = vec![heading, paragraph("Body.")];
        assert!(!to_markdown_with(&doc, &metadata(), &References::new()).contains("# On Burnout"));
    }

    #[test]
    fn a_marginal_does_not_publish_in_markdown() {
        let mut doc = Document::new("t");
        let first = paragraph("First.");
        doc.annotations = vec![
            annotation(AnnotationKind::Marginal, first.id, 0, 0, "SIDE"),
            annotation(AnnotationKind::Marginal, Uuid::new_v4(), 0, 0, "ORPHAN"),
        ];
        doc.blocks = vec![first];
        assert_eq!(to_markdown(&doc), "First.\n");
    }

    #[test]
    fn a_marginal_on_the_title_heading_does_not_print_the_title_twice() {
        let mut doc = Document::new("t");
        let heading = block(
            BlockKind::Heading {
                level: 1,
                centered: false,
            },
            "On Burnout",
        );
        doc.annotations = vec![annotation(
            AnnotationKind::Marginal,
            heading.id,
            0,
            0,
            "side",
        )];
        doc.blocks = vec![heading, paragraph("Body.")];
        assert!(!to_markdown_with(&doc, &metadata(), &References::new()).contains("# On Burnout"));
    }

    #[test]
    fn angle_brackets_in_prose_cannot_become_html() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("Run <script>alert(1)</script> & a > b.")];
        assert_eq!(
            to_markdown(&doc),
            "Run \\<script\\>alert(1)\\</script\\> & a \\> b.\n"
        );
    }

    #[test]
    fn a_leading_angle_bracket_is_escaped_once() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("> not a quote")];
        assert_eq!(to_markdown(&doc), "\\> not a quote\n");
    }

    #[test]
    fn a_footnote_body_cannot_become_html_or_new_blocks() {
        let mut doc = Document::new("t");
        let first = paragraph("Body");
        doc.annotations = vec![annotation(
            AnnotationKind::Footnote,
            first.id,
            0,
            4,
            "<b>x</b>\n\n# not a heading",
        )];
        doc.blocks = vec![first];
        assert_eq!(
            to_markdown(&doc),
            "Body[^1]\n\n[^1]: \\<b\\>x\\</b\\>  # not a heading\n"
        );
    }

    #[test]
    fn an_image_with_a_script_link_exports_without_it() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(
            BlockKind::Image {
                path: "a.png".to_string(),
                link: Some("javascript:alert(1)".to_string()),
                align: Default::default(),
            },
            "",
        )];
        assert_eq!(to_markdown(&doc), "![](a.png)\n");
    }

    #[test]
    fn a_rule_exports_as_three_dashes() {
        let mut doc = Document::new("t");
        doc.blocks = vec![
            paragraph("One."),
            block(BlockKind::Rule, ""),
            paragraph("Two."),
        ];
        assert_eq!(to_markdown(&doc), "One.\n\n---\n\nTwo.\n");
    }

    #[test]
    fn code_exports_as_a_fence_with_its_text_exact() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(
            BlockKind::Code {
                language: Some("rust".to_string()),
            },
            "fn main() {\n    let a = *b_c;\n\n}",
        )];
        assert_eq!(
            to_markdown(&doc),
            "```rust\nfn main() {\n    let a = *b_c;\n\n}\n```\n"
        );
    }

    #[test]
    fn a_fence_inside_code_gets_a_longer_fence_around_it() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(BlockKind::Code { language: None }, "```\nx\n```")];
        assert_eq!(to_markdown(&doc), "````\n```\nx\n```\n````\n");
    }

    #[test]
    fn prose_exports_as_a_quote_with_hard_breaks() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(BlockKind::Prose, "Roses are red,\nviolets are blue.")];
        assert_eq!(
            to_markdown(&doc),
            "> Roses are red,\\\n> violets are blue.\n"
        );
    }

    #[test]
    fn emphasis_in_prose_exports_inside_the_quote() {
        let mut doc = Document::new("t");
        let mut prose = block(BlockKind::Prose, "one\ntwo");
        prose.spans = vec![Span {
            start: 0,
            end: 3,
            style: SpanStyle::Italic,
        }];
        doc.blocks = vec![prose];
        assert_eq!(to_markdown(&doc), "> *one*\\\n> two\n");
    }

    #[test]
    fn a_document_link_publishes_as_its_text() {
        let mut doc = Document::new("t");
        let mut linked = paragraph("See the intro now.");
        linked.spans = vec![Span {
            start: 4,
            end: 13,
            style: SpanStyle::DocumentLink {
                target: "notes/a.kame#Intro".to_string(),
            },
        }];
        doc.blocks = vec![linked];
        assert_eq!(to_markdown(&doc), "See the intro now.\n");
    }

    /// A paragraph with a link to `target` on the words `words`.
    fn with_heading_link(before: &str, words: &str, target: &str) -> Document {
        let mut doc = Document::new("t");
        let mut linked = paragraph(&format!("{before}{words}"));
        linked.spans = vec![Span {
            start: before.len(),
            end: before.len() + words.len(),
            style: SpanStyle::DocumentLink {
                target: target.to_string(),
            },
        }];
        doc.blocks = vec![linked];
        doc
    }

    #[test]
    fn a_link_to_a_heading_exports_as_a_link() {
        assert_eq!(
            to_markdown(&with_heading_link("See ", "the intro", "#The Intro")),
            "See [the intro](<#The Intro>)\n"
        );
        assert_eq!(
            to_markdown(&with_heading_link("", "x", "#Plain")),
            "[x](<#Plain>)\n"
        );
    }

    #[test]
    fn a_link_to_a_heading_keeps_its_emphasis_inside() {
        let mut doc = with_heading_link("", "the intro", "#The Intro");
        doc.blocks[0].spans.push(Span {
            start: 4,
            end: 9,
            style: SpanStyle::Bold,
        });
        assert_eq!(to_markdown(&doc), "[the **intro**](<#The Intro>)\n");
    }

    #[test]
    fn a_link_to_a_heading_reads_back_as_itself() {
        for target in [
            "#The Intro",
            "#Plain",
            "#Q&A: <now> \\ (then)",
            "#été à Paris",
            "#a&amp;b",
            "#50% off",
            "#a%20b",
            "#a\nb",
        ] {
            let doc = with_heading_link("See ", "this", target);
            let exported = to_markdown(&doc);
            let back =
                crate::import::markdown::from_markdown(&exported, std::path::Path::new(""), "t")
                    .document;
            assert_eq!(
                back.blocks[0].spans[0].style,
                SpanStyle::DocumentLink {
                    target: target.to_string()
                },
                "{exported}"
            );
            assert_eq!(back.blocks[0].text, "See this");
            assert_eq!(to_markdown(&back), exported);
        }
    }

    #[test]
    fn a_link_to_a_heading_in_another_document_still_publishes_as_its_text() {
        assert_eq!(
            to_markdown(&with_heading_link("See ", "this", "other.kame#The Intro")),
            "See this\n"
        );
    }

    #[test]
    fn an_image_linked_to_a_heading_reads_back_as_itself() {
        let source = "[![](a.png)](<#The Intro>)\n";
        let doc =
            crate::import::markdown::from_markdown(source, std::path::Path::new("/"), "t").document;
        let exported = to_markdown(&doc);
        assert_eq!(exported, "[![](/a.png)](#The%20Intro)\n");
        let back =
            crate::import::markdown::from_markdown(&exported, std::path::Path::new("/"), "t")
                .document;
        assert_eq!(back.blocks[0].kind, doc.blocks[0].kind);
    }

    fn with_code(text: &str, ranges: &[(usize, usize)]) -> Document {
        let mut doc = Document::new("t");
        let mut block = paragraph(text);
        block.spans = ranges
            .iter()
            .map(|(start, end)| Span {
                start: *start,
                end: *end,
                style: SpanStyle::Code,
            })
            .collect();
        doc.blocks = vec![block];
        doc
    }

    #[test]
    fn inline_code_exports_in_backticks_without_escaping() {
        assert_eq!(
            to_markdown(&with_code("run a_b*c now", &[(4, 9)])),
            "run `a_b*c` now\n"
        );
    }

    #[test]
    fn a_backtick_in_inline_code_gets_a_longer_delimiter() {
        assert_eq!(to_markdown(&with_code("a`b", &[(0, 3)])), "``a`b``\n");
    }

    #[test]
    fn inline_code_starting_or_ending_in_a_backtick_is_padded() {
        assert_eq!(to_markdown(&with_code("`a", &[(0, 2)])), "`` `a ``\n");
        assert_eq!(to_markdown(&with_code("a`", &[(0, 2)])), "`` a` ``\n");
    }

    #[test]
    fn inline_code_nests_inside_bold_and_links() {
        let mut doc = with_code("use it now", &[(4, 6)]);
        doc.blocks[0].spans.push(Span {
            start: 0,
            end: 10,
            style: SpanStyle::Bold,
        });
        assert_eq!(to_markdown(&doc), "**use `it` now**\n");
        let mut doc = with_code("use it now", &[(4, 6)]);
        doc.blocks[0].spans.push(Span {
            start: 4,
            end: 6,
            style: SpanStyle::Link {
                url: "https://a.b".to_string(),
            },
        });
        assert_eq!(to_markdown(&doc), "use [`it`](https://a.b) now\n");
    }

    fn list(items: &[(bool, u8, &str)]) -> Document {
        let mut doc = Document::new("t");
        doc.blocks = items
            .iter()
            .map(|(ordered, depth, text)| {
                block(
                    BlockKind::ListItem {
                        ordered: *ordered,
                        depth: *depth,
                    },
                    text,
                )
            })
            .collect();
        doc
    }

    #[test]
    fn a_nested_bullet_is_indented_past_its_parents_marker() {
        assert_eq!(
            to_markdown(&list(&[
                (false, 0, "a"),
                (false, 1, "b"),
                (false, 2, "c"),
                (false, 0, "d")
            ])),
            "- a\n\n  - b\n\n    - c\n\n- d\n"
        );
    }

    #[test]
    fn a_list_nested_in_a_numbered_one_is_indented_past_the_number() {
        assert_eq!(
            to_markdown(&list(&[
                (true, 0, "a"),
                (false, 1, "b"),
                (true, 0, "c"),
                (true, 1, "d")
            ])),
            "1. a\n\n   - b\n\n2. c\n\n   1. d\n"
        );
    }

    #[test]
    fn a_line_break_in_a_nested_item_stays_in_the_item() {
        assert_eq!(
            to_markdown(&list(&[(false, 0, "a"), (false, 1, "b\nc")])),
            "- a\n\n  - b\\\n    c\n"
        );
    }

    #[test]
    fn a_first_heading_repeating_the_title_can_be_left_out() {
        let mut doc = Document::new("t");
        doc.blocks = vec![
            block(
                BlockKind::Heading {
                    level: 1,
                    centered: false,
                },
                "The Title",
            ),
            paragraph("Words."),
        ];
        let without = crate::export::without_repeated_title(&doc, "The Title");
        assert_eq!(without.blocks.len(), 1);
        let different = crate::export::without_repeated_title(&doc, "Another");
        assert_eq!(different.blocks.len(), 2);
    }

    #[test]
    fn a_title_differing_only_by_case_or_space_still_counts_as_repeated() {
        let mut doc = Document::new("t");
        doc.blocks = vec![
            block(
                BlockKind::Heading {
                    level: 1,
                    centered: false,
                },
                " On The Commanding Heights ",
            ),
            paragraph("Words."),
        ];
        let without = crate::export::without_repeated_title(&doc, "On the Commanding Heights");
        assert_eq!(without.blocks.len(), 1);
        let different = crate::export::without_repeated_title(&doc, "On the Heights");
        assert_eq!(different.blocks.len(), 2);
    }

    #[test]
    fn a_repeated_title_stays_if_a_footnote_is_anchored_in_it() {
        let mut doc = Document::new("t");
        let heading = block(
            BlockKind::Heading {
                level: 1,
                centered: false,
            },
            "The Title",
        );
        let id = heading.id;
        doc.blocks = vec![heading];
        doc.annotations.push(Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Footnote,
            anchor: Anchor::Range {
                block_id: id,
                start: 0,
                end: 3,
            },
            body: "A note.".to_string(),
        });
        assert_eq!(
            crate::export::without_repeated_title(&doc, "The Title")
                .blocks
                .len(),
            1
        );
    }

    #[test]
    fn markdown_keeps_a_highlighted_passage_as_plain_text() {
        let mut doc = Document::new("t");
        let para = paragraph("See this bit now.");
        let id = para.id;
        doc.blocks = vec![para];
        doc.annotations.push(Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Note,
            anchor: Anchor::Range {
                block_id: id,
                start: 4,
                end: 12,
            },
            body: "SECRET NOTE".to_string(),
        });
        assert_eq!(to_markdown(&doc), "See this bit now.\n");
    }

    fn article() -> Reference {
        Reference {
            kind: crate::references::Kind::Article,
            authors: vec!["D. E. Knuth".to_string(), "M. F. Plass".to_string()],
            title: "Breaking paragraphs into lines".to_string(),
            published_in: "Softw.: Pract. Exper.".to_string(),
            volume: "11".to_string(),
            number: "11".to_string(),
            pages: "1119\u{2013}1184".to_string(),
            month: "Nov.".to_string(),
            year: "1981".to_string(),
            ..Reference::default()
        }
    }

    fn knuth() -> References {
        let mut references = References::new();
        references.insert("knuth1981".to_string(), article());
        references
    }

    fn with_reference(reference: Reference) -> References {
        let mut references = References::new();
        references.insert("src".to_string(), reference);
        references
    }

    fn md_of(text: &str, references: &References) -> String {
        to_markdown_with(
            &document(vec![paragraph(text)], Vec::new()),
            &Metadata::default(),
            references,
        )
    }

    #[test]
    fn a_document_citing_nothing_exports_as_it_always_did() {
        let mut doc = rich_document();
        doc.blocks
            .push(paragraph("A stray [knuth1981a] and [sic]."));
        let expected = to_markdown(&doc);
        assert_eq!(
            to_markdown_with(&doc, &Metadata::default(), &knuth()),
            expected
        );
        assert!(!expected.contains("## References"));
    }

    #[test]
    fn markdown_keeps_a_citation_as_text_and_lists_it_after_the_footnotes() {
        let block = paragraph("Broken [knuth1981].");
        let id = block.id;
        let doc = document(vec![block], vec![footnote(id, 19, 19, "A footnote.")]);
        let md = to_markdown_with(&doc, &Metadata::default(), &knuth());
        assert!(md.contains("Broken \\[knuth1981\\]."), "{md}");
        assert!(
            md.contains(
                "\n## References\n\n\\[knuth1981\\] D. E. Knuth and M. F. Plass, \
                 \u{201c}Breaking paragraphs into lines,\u{201d} *Softw.: Pract. Exper.*, \
                 vol. 11, no. 11, pp. 1119\u{2013}1184, Nov. 1981.\n"
            ),
            "{md}"
        );
        assert!(md.find("[^1]: A footnote.").unwrap() < md.find("## References").unwrap());
        assert!(md.ends_with("Nov. 1981.\n"), "{md}");
    }

    #[test]
    fn each_entry_is_a_paragraph_in_key_order() {
        let mut references = knuth();
        references.insert("Alpha".to_string(), article());
        let md = md_of("[knuth1981] [Alpha]", &references);
        let (_, section) = md.split_once("## References\n\n").expect("a section");
        let entries: Vec<&str> = section.trim_end().split("\n\n").collect();
        assert_eq!(entries.len(), 2, "{md}");
        assert!(entries[0].starts_with("\\[Alpha\\] "), "{md}");
        assert!(entries[1].starts_with("\\[knuth1981\\] "), "{md}");
        assert_eq!(md, md_of("[knuth1981] [Alpha]", &references));
    }

    #[test]
    fn a_key_cited_only_in_a_footnote_is_listed() {
        let block = paragraph("Text.");
        let id = block.id;
        let doc = document(vec![block], vec![footnote(id, 5, 5, "See [knuth1981].")]);
        let md = to_markdown_with(&doc, &Metadata::default(), &knuth());
        assert!(md.contains("[^1]: See \\[knuth1981\\]."), "{md}");
        assert!(
            md.contains("## References\n\n\\[knuth1981\\] D. E. Knuth"),
            "{md}"
        );
    }

    #[test]
    fn a_key_cited_only_in_a_marginal_is_not_listed() {
        let block = paragraph("Text.");
        let id = block.id;
        let doc = document(
            vec![block],
            vec![annotation(
                AnnotationKind::Marginal,
                id,
                0,
                5,
                "Per [knuth1981].",
            )],
        );
        let md = to_markdown_with(&doc, &Metadata::default(), &knuth());
        assert!(!md.contains("References"), "{md}");
    }

    #[test]
    fn a_citation_in_a_note_in_code_or_in_a_links_words_is_not_listed() {
        let code = block(BlockKind::Code { language: None }, "see [knuth1981]");
        let mut inline = paragraph("see [knuth1981] here");
        inline.spans = vec![Span {
            start: 4,
            end: 15,
            style: SpanStyle::Code,
        }];
        let mut linked = paragraph("see [knuth1981] here");
        linked.spans = vec![Span {
            start: 0,
            end: 15,
            style: SpanStyle::Link {
                url: "https://example.com/".to_string(),
            },
        }];
        let noted = paragraph("Plain.");
        let id = noted.id;
        let doc = document(
            vec![code, inline, linked, noted],
            vec![annotation(
                AnnotationKind::Note,
                id,
                0,
                5,
                "Check [knuth1981].",
            )],
        );
        let md = to_markdown_with(&doc, &Metadata::default(), &knuth());
        assert!(!md.contains("References"), "{md}");
    }

    #[test]
    fn an_unknown_key_stays_as_typed_with_no_section() {
        let md = md_of("Lost [smith2020].", &knuth());
        assert_eq!(md, "Lost \\[smith2020\\].\n");
    }

    #[test]
    fn the_section_follows_a_repeated_title_being_dropped() {
        let mut doc = Document::new("t");
        doc.blocks = vec![
            block(
                BlockKind::Heading {
                    level: 1,
                    centered: false,
                },
                "[knuth1981]",
            ),
            paragraph("Body."),
        ];
        let metadata = Metadata {
            title: "[knuth1981]".to_string(),
            ..Metadata::default()
        };
        let md = to_markdown_with(&doc, &metadata, &knuth());
        assert!(
            !md.contains("## References"),
            "the dropped heading cites nothing: {md}"
        );
    }

    #[test]
    fn a_script_url_is_escaped_text_and_a_web_one_is_an_autolink() {
        let web = |url: &str| Reference {
            kind: crate::references::Kind::Web,
            title: "Page".to_string(),
            url: url.to_string(),
            ..Reference::default()
        };
        let md = md_of("See [src].", &with_reference(web("javascript:alert(1)")));
        assert!(md.contains("javascript:alert(1)"), "{md}");
        assert!(!md.contains("<javascript"), "{md}");

        let md = md_of("See [src].", &with_reference(web("https://a.b/c_d")));
        assert!(md.contains("<https://a.b/c_d>"), "{md}");
    }

    #[test]
    fn reference_text_is_escaped() {
        let hostile = Reference {
            kind: crate::references::Kind::Book,
            title: "<script>alert(1)</script>".to_string(),
            ..Reference::default()
        };
        let md = md_of("See [src].", &with_reference(hostile));
        assert!(md.contains("*\\<script\\>alert(1)\\</script\\>.*"), "{md}");
    }

    #[test]
    fn spaces_at_the_edges_of_an_italic_title_stay_outside_its_emphasis() {
        let book = Reference {
            kind: crate::references::Kind::Book,
            title: " Algorithms ".to_string(),
            edition: "2nd".to_string(),
            ..Reference::default()
        };
        let md = md_of("See [src].", &with_reference(book));
        assert!(md.contains("\\[src\\]  *Algorithms* , 2nd ed."), "{md}");
    }

    #[test]
    fn a_freeform_reference_stays_one_paragraph_and_escapes_markup() {
        let freeform = Reference {
            kind: crate::references::Kind::Freeform,
            text: "Smith *et al.*\n\nsecond line".to_string(),
            ..Reference::default()
        };
        let md = md_of("See [src].", &with_reference(freeform));
        assert!(
            md.ends_with("## References\n\n\\[src\\] Smith \\*et al.\\*  second line\n"),
            "{md}"
        );
    }

    #[test]
    fn only_a_web_or_mail_address_in_a_reference_is_an_autolink() {
        let web = |url: &str| Reference {
            kind: crate::references::Kind::Web,
            title: "Page".to_string(),
            url: url.to_string(),
            ..Reference::default()
        };
        for url in [
            "script",
            "/about",
            "www.example.com/x",
            "javascript:alert(1)",
        ] {
            let md = md_of("See [src].", &with_reference(web(url)));
            assert!(!md.contains(&format!("<{url}>")), "{url}: {md}");
            assert!(md.ends_with(&format!("Available: {url}\n")), "{url}: {md}");
        }
        for url in ["HTTPS://a.b", "mailto:a@b.c"] {
            let md = md_of("See [src].", &with_reference(web(url)));
            assert!(md.contains(&format!("<{url}>")), "{md}");
        }
    }

    #[test]
    fn a_citation_inside_a_bare_url_is_not_listed() {
        let md = md_of("see https://a.b/c[knuth1981] now", &knuth());
        assert!(!md.contains("References"), "{md}");
    }

    #[test]
    fn a_footnote_that_is_not_exported_lists_no_source() {
        let orphan = footnote(Uuid::new_v4(), 0, 0, "See [knuth1981].");
        let doc = document(vec![paragraph("Text.")], vec![orphan]);
        let md = to_markdown_with(&doc, &Metadata::default(), &knuth());
        assert!(!md.contains("References") && !md.contains("[^1]"), "{md}");

        let empty = block(BlockKind::Paragraph, "");
        let id = empty.id;
        let doc = document(
            vec![paragraph("Text."), empty],
            vec![footnote(id, 0, 0, "See [knuth1981].")],
        );
        let md = to_markdown_with(&doc, &Metadata::default(), &knuth());
        assert!(!md.contains("References") && !md.contains("[^1]"), "{md}");
    }
}
