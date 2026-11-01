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
/// - paragraphs are emitted as-is,
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
        // An empty block carries no text and no apparatus: exporting it would
        // only add stray blank lines.
        if block.text.is_empty() {
            continue;
        }
        let markers = by_block.get(&block.id).map(Vec::as_slice).unwrap_or(&[]);
        parts.push(format!("{prefix}{}", render_block_text(block, markers)));
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
            .map(|(number, annotation)| format!("[^{number}]: {}", annotation.body))
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
fn render_block_text(block: &Block, footnotes: &[(usize, usize)]) -> String {
    let text = block.text.as_str();
    // (offset, ordering class, tiebreak, marker)
    let mut markers: Vec<(usize, u8, usize, String)> = Vec::new();

    for span in &block.spans {
        let start = snap_up(text, span.start);
        let end = snap_down(text, span.end);
        if start >= end {
            continue;
        }
        let delimiter = match span.style {
            SpanStyle::Bold => "**",
            SpanStyle::Italic => "*",
        };
        // Open outermost first (largest extent) and close innermost first
        // (latest start), so nested spans produce valid `**a *b***` runs.
        markers.push((start, 2, usize::MAX - end, delimiter.to_string()));
        markers.push((end, 0, usize::MAX - start, delimiter.to_string()));
    }

    for (offset, number) in footnotes {
        let offset = snap_down(text, *offset);
        markers.push((offset, 1, *number, format!("[^{number}]")));
    }

    markers.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));

    let mut out = String::with_capacity(text.len() + markers.len() * 2);
    let mut cursor = 0;
    for (offset, _, _, marker) in markers {
        if offset > cursor {
            out.push_str(&text[cursor..offset]);
            cursor = offset;
        }
        out.push_str(&marker);
    }
    out.push_str(&text[cursor..]);
    out
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
}
