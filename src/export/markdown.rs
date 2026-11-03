//! Markdown export.
//!
//! `to_markdown` renders a [`Document`] as CommonMark-flavoured Markdown so a
//! draft can leave kame and live anywhere: Pandoc, a static site, a code
//! review. The renderer is deliberately lossless in one direction only —
//! Markdown keeps the text, the block structure, bold/italic runs and footnotes.
//! Marginalia have no Markdown form and are the cost of leaving kame's own format;
//! the HTML export keeps them. Notes are the author's own and never publish.

use crate::model::{
    is_script_url, AnnotationKind, Block, BlockId, BlockKind, Document, Metadata, SpanStyle,
};
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
/// - notes do not publish.
///
/// The output depends only on the document contents: footnote numbering follows
/// block order rather than the order annotations happen to be stored in, so
/// exporting the same document twice (or after a serde round trip) is stable.
pub fn to_markdown(doc: &Document) -> String {
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
    let mut ordered_index = 0usize;
    for block in &doc.blocks {
        // An image is a block with no text: emit it as a Markdown image before
        // the empty-text skip drops it.
        if let BlockKind::Image { path, link } = &block.kind {
            ordered_index = 0;
            let image = format!("![]({})", link_destination(path));
            parts.push(match link.as_deref().filter(|url| !is_script_url(url)) {
                Some(url) => format!("[{image}]({})", link_destination(url)),
                None => image,
            });
            continue;
        }
        if block.kind == BlockKind::Rule {
            ordered_index = 0;
            parts.push("---".to_string());
            continue;
        }
        if let (BlockKind::Code { language }, false) = (&block.kind, block.text.is_empty()) {
            ordered_index = 0;
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
        let prefix = match block.kind {
            BlockKind::Heading { level } => {
                ordered_index = 0;
                format!("{} ", "#".repeat(level.clamp(1, 6) as usize))
            }
            BlockKind::ListItem { ordered: false } => {
                ordered_index = 0;
                "- ".to_string()
            }
            BlockKind::ListItem { ordered: true } => {
                ordered_index += 1;
                format!("{ordered_index}. ")
            }
            BlockKind::Paragraph => {
                ordered_index = 0;
                String::new()
            }
            BlockKind::BlockQuote | BlockKind::Prose => {
                ordered_index = 0;
                "> ".to_string()
            }
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
    out
}

/// Render `doc` for publishing: [`to_markdown`] under a `Key: value` header, the
/// form Pelican reads, leaving out any field that is empty.
///
/// A first heading repeating the title is dropped, since a blog renders the title
/// itself, unless a footnote is anchored in it. The date is the published date,
/// or the created date for a document not yet published.
pub fn to_markdown_with(doc: &Document, metadata: &Metadata) -> String {
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

    let repeats_title = doc.blocks.first().is_some_and(|first| {
        matches!(first.kind, BlockKind::Heading { .. })
            && first.text == metadata.title
            && !doc.annotations.iter().any(|annotation| {
                annotation.kind == AnnotationKind::Footnote
                    && annotation.anchor.block_id() == first.id
            })
    });
    let body = if repeats_title {
        let mut rest = doc.clone();
        rest.blocks.remove(0);
        to_markdown(&rest)
    } else {
        to_markdown(doc)
    };

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
        // can't lead anywhere outside kame.
        .filter(|span| !matches!(&span.style, SpanStyle::Link { url } if is_script_url(url)))
        .filter(|span| !matches!(span.style, SpanStyle::DocumentLink { .. }))
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
    for window in bounds.windows(2) {
        let (start, end) = (window[0], window[1]);
        if start >= end {
            continue;
        }
        // A link wraps everything inside it, so it goes first (outermost);
        // emphasis nests under it in `style_order()`.
        let mut wanted: Vec<SpanStyle> = Vec::new();
        if let Some((_, _, style)) = spans.iter().find(|(s, e, style)| {
            matches!(style, SpanStyle::Link { .. }) && *s <= start && *e >= end
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
        while let Some(innermost) = open.last().cloned() {
            if wanted.contains(&innermost) {
                break;
            }
            out.push_str(&close_delimiter(&innermost));
            open.pop();
        }
        // A marker at a span edge belongs after the closing and before the
        // opening delimiters, matching where the superscript renders.
        for (_, number) in footnotes.iter().filter(|(offset, _)| *offset == start) {
            out.push_str(&format!("[^{number}]"));
        }
        for style in &wanted {
            if !open.contains(style) {
                out.push_str(&open_delimiter(style));
                open.push(style.clone());
            }
        }
        push_escaped(&mut out, &text[start..end]);
    }
    while let Some(style) = open.pop() {
        out.push_str(&close_delimiter(&style));
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
fn style_order() -> [SpanStyle; 2] {
    [SpanStyle::Bold, SpanStyle::Italic]
}

/// The marker that opens `style`'s run.
fn open_delimiter(style: &SpanStyle) -> String {
    match style {
        SpanStyle::Bold => "**".to_string(),
        SpanStyle::Italic => "*".to_string(),
        SpanStyle::Link { .. } => "[".to_string(),
        SpanStyle::DocumentLink { .. } => unreachable!("document links are dropped"),
    }
}

/// The marker that closes `style`'s run. A link is asymmetric: `[...](url)`.
fn close_delimiter(style: &SpanStyle) -> String {
    match style {
        SpanStyle::Bold => "**".to_string(),
        SpanStyle::Italic => "*".to_string(),
        SpanStyle::Link { url } => format!("]({})", link_destination(url)),
        SpanStyle::DocumentLink { .. } => unreachable!("document links are dropped"),
    }
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
        let bullet = block(BlockKind::ListItem { ordered: false }, "Bullet");
        let ordered = block(BlockKind::ListItem { ordered: true }, "First");
        let note_block = paragraph("Anchored text");
        let note_id = note_block.id;
        document(
            vec![
                block(BlockKind::Heading { level: 2 }, "Section"),
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
                block(BlockKind::Heading { level: 1 }, "Title"),
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
                block(BlockKind::ListItem { ordered: false }, "One"),
                block(BlockKind::ListItem { ordered: false }, "Two"),
                block(BlockKind::ListItem { ordered: true }, "First"),
                block(BlockKind::ListItem { ordered: true }, "Second"),
                block(BlockKind::ListItem { ordered: false }, "Back"),
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
            vec![block(BlockKind::Heading { level: 0 }, "Low")],
            Vec::new(),
        );
        assert_eq!(to_markdown(&doc), "# Low\n");
        let doc = document(
            vec![block(BlockKind::Heading { level: 9 }, "High")],
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
                block(BlockKind::ListItem { ordered: true }, "First"),
                block(BlockKind::ListItem { ordered: true }, ""),
                block(BlockKind::ListItem { ordered: true }, "Second"),
            ],
            Vec::new(),
        );
        assert_eq!(to_markdown(&doc), "1. First\n\n2. Second\n");

        // An emptied paragraph is likewise not allowed to reset the counter.
        let doc = document(
            vec![
                block(BlockKind::ListItem { ordered: true }, "First"),
                paragraph(""),
                block(BlockKind::ListItem { ordered: true }, "Second"),
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
        doc.blocks[0].kind = BlockKind::Heading { level: 2 };
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
        }
    }

    #[test]
    fn publishing_writes_front_matter_in_a_fixed_order() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("Body.")];
        assert_eq!(
            to_markdown_with(&doc, &metadata()),
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
        assert!(to_markdown_with(&doc, &meta).contains("Date: 2019-01-01\n"));
    }

    #[test]
    fn empty_fields_are_left_out_and_no_metadata_means_no_front_matter() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("Body.")];
        let title_only = crate::model::Metadata {
            title: "T".to_string(),
            ..Default::default()
        };
        assert_eq!(to_markdown_with(&doc, &title_only), "Title: T\n\nBody.\n");
        assert_eq!(
            to_markdown_with(&doc, &crate::model::Metadata::default()),
            "Body.\n"
        );
    }

    /// A blog renders the title itself, so it would otherwise appear twice.
    #[test]
    fn a_first_heading_repeating_the_title_is_not_published_twice() {
        let mut doc = Document::new("t");
        doc.blocks = vec![
            block(BlockKind::Heading { level: 1 }, "On Burnout"),
            paragraph("Body."),
        ];
        let out = to_markdown_with(&doc, &metadata());
        assert!(!out.contains("# On Burnout"), "{out}");
        assert!(out.contains("Title: On Burnout"), "{out}");
    }

    #[test]
    fn a_different_first_heading_stays() {
        let mut doc = Document::new("t");
        doc.blocks = vec![
            block(BlockKind::Heading { level: 1 }, "Introduction"),
            paragraph("Body."),
        ];
        assert!(to_markdown_with(&doc, &metadata()).contains("# Introduction"));
    }

    #[test]
    fn a_title_heading_with_a_footnote_keeps_the_footnote() {
        let mut doc = Document::new("t");
        let heading = block(BlockKind::Heading { level: 1 }, "On Burnout");
        doc.annotations = vec![annotation(
            AnnotationKind::Footnote,
            heading.id,
            2,
            2,
            "A note.",
        )];
        doc.blocks = vec![heading, paragraph("Body.")];
        assert!(to_markdown_with(&doc, &metadata()).contains("[^1]: A note."));
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
            let out = to_markdown_with(&doc, &meta);
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
        let out = to_markdown_with(&doc, &meta);
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
            },
            "",
        )];
        assert_eq!(to_markdown(&doc), "![](my%20pic.png)\n");
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
        let heading = block(BlockKind::Heading { level: 1 }, "On Burnout");
        doc.annotations = vec![annotation(
            AnnotationKind::Note,
            heading.id,
            0,
            2,
            "private",
        )];
        doc.blocks = vec![heading, paragraph("Body.")];
        assert!(!to_markdown_with(&doc, &metadata()).contains("# On Burnout"));
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
        let heading = block(BlockKind::Heading { level: 1 }, "On Burnout");
        doc.annotations = vec![annotation(
            AnnotationKind::Marginal,
            heading.id,
            0,
            0,
            "side",
        )];
        doc.blocks = vec![heading, paragraph("Body.")];
        assert!(!to_markdown_with(&doc, &metadata()).contains("# On Burnout"));
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
}
