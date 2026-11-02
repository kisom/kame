//! Markdown export.
//!
//! `to_markdown` renders a [`Document`] as CommonMark-flavoured Markdown so a
//! draft can leave kame and live anywhere: Pandoc, a static site, a code
//! review. The renderer is deliberately lossless in one direction only —
//! Markdown keeps the text, the block structure, bold/italic runs and
//! footnotes, and drops the apparatus that has no Markdown equivalent
//! (marginalia and notes are v1-omitted).

use crate::model::{AnnotationKind, Block, BlockId, BlockKind, Document, SpanStyle};
use std::collections::HashMap;

/// Render `doc` as Markdown.
///
/// Layout rules:
/// - headings become `#` runs (level clamped to 1–6),
/// - ordered list items are numbered per consecutive run, unordered ones use `- `,
/// - prose is escaped: backslash, asterisk, underscore, backtick and square
///   brackets in the text are backslash-escaped so they cannot open emphasis,
///   code or a link, and a paragraph whose first glyph would start block
///   structure (`#`, `>`, `-`, `+`, `=`, `~`, or `1.`/`1)`) has that glyph
///   escaped too, so it stays a paragraph,
/// - blocks are separated by a blank line and the document ends in a newline,
/// - inline spans become `*italic*` / `**bold**`, nesting innermost-first,
/// - footnotes get `[^n]` inline at the end of their anchored range and a
///   `[^n]: body` line after the body, numbered in document order,
/// - marginalia and notes are omitted.
///
/// The output depends only on the document contents: footnote numbering follows
/// block order rather than the order annotations happen to be stored in, so
/// exporting the same document twice (or after a serde round trip) is stable.
pub fn to_markdown(doc: &Document) -> String {
    let footnotes = numbered_footnotes(doc);
    let mut by_block: HashMap<BlockId, Vec<(usize, usize)>> = HashMap::new();
    for (number, annotation) in &footnotes {
        by_block
            .entry(annotation.anchor.block_id)
            .or_default()
            .push((annotation.anchor.end, *number));
    }

    let mut parts: Vec<String> = Vec::new();
    let mut ordered_index = 0usize;
    for block in &doc.blocks {
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
        };
        let markers = by_block.get(&block.id).map(Vec::as_slice).unwrap_or(&[]);
        let mut body = render_block_text(block, markers);
        // A paragraph's own text must not turn into block structure. Headings
        // and list items already carry a marker, so they are left alone.
        if block.kind == BlockKind::Paragraph {
            escape_block_start(&mut body);
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
                push_escaped(&mut body, &annotation.body);
                format!("[^{number}]: {body}")
            })
            .collect();
        out.push_str(&lines.join("\n"));
        out.push('\n');
    }
    out
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
            let index = *block_order.get(&annotation.anchor.block_id)?;
            if doc.blocks[index].text.is_empty() {
                return None;
            }
            Some((
                index,
                annotation.anchor.start,
                annotation.anchor.end,
                annotation,
            ))
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
        let wanted: Vec<SpanStyle> = style_order()
            .into_iter()
            .filter(|style| {
                spans
                    .iter()
                    .any(|(s, e, covered)| covered == style && *s <= start && *e >= end)
            })
            .collect();

        // Close from the inside out, but only what stopped covering the text.
        // A style that still covers it but sits under one that just ended stays
        // open and its run extends, rather than being reopened with a stack of
        // adjacent asterisks that no parser can unpick.
        while let Some(innermost) = open.last().cloned() {
            if wanted.contains(&innermost) {
                break;
            }
            out.push_str(delimiter(&innermost));
            open.pop();
        }
        // A marker at a span edge belongs after the closing and before the
        // opening delimiters, matching where the superscript renders.
        for (_, number) in footnotes.iter().filter(|(offset, _)| *offset == start) {
            out.push_str(&format!("[^{number}]"));
        }
        for style in &wanted {
            if !open.contains(style) {
                out.push_str(delimiter(style));
                open.push(style.clone());
            }
        }
        push_escaped(&mut out, &text[start..end]);
    }
    while let Some(style) = open.pop() {
        out.push_str(delimiter(&style));
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
const ESCAPED_IN_TEXT: [char; 6] = ['\\', '*', '_', '`', '[', ']'];

/// Appends `text` with the characters in [`ESCAPED_IN_TEXT`] backslash-escaped.
fn push_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        if ESCAPED_IN_TEXT.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
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
/// a segment carrying both exports as `**a *b***`.
fn style_order() -> [SpanStyle; 2] {
    [SpanStyle::Bold, SpanStyle::Italic]
}

fn delimiter(style: &SpanStyle) -> &'static str {
    match style {
        SpanStyle::Bold => "**",
        SpanStyle::Italic => "*",
    }
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
            anchor: Anchor {
                block_id,
                start,
                end,
            },
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
    fn footnotes_export_pandoc_style() {
        let body = paragraph("Body text");
        let id = body.id;
        let doc = document(vec![body], vec![footnote(id, 0, 9, "The note body.")]);
        let md = to_markdown(&doc);
        assert_eq!(md, "Body text[^1]\n\n[^1]: The note body.\n");
    }

    #[test]
    fn marginalia_and_notes_omitted() {
        let body = paragraph("Body text");
        let id = body.id;
        let with = document(
            vec![body.clone()],
            vec![
                annotation(AnnotationKind::Marginal, id, 0, 4, "margin"),
                annotation(AnnotationKind::Note, id, 5, 9, "note"),
            ],
        );
        let without = document(vec![body], Vec::new());
        assert_eq!(to_markdown(&with), to_markdown(&without));
        assert_eq!(to_markdown(&with), "Body text\n");
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
}
