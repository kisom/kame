//! HTML export.
//!
//! `to_html` renders a [`Document`] as one self-contained HTML file: the page
//! keeps everything the editor knows about the draft. Blocks become headings,
//! paragraphs and lists; spans become `<em>`/`<strong>`; marginalia become
//! floating `<aside>`s beside the passage they annotate, notes become a
//! `<mark>` over the highlighted range with an expandable `<details>` beneath
//! it, and footnotes become superscript links into a numbered footnote list at
//! the foot of the document. The embedded stylesheet carries the house
//! typography — Crimson Pro for headings, EB Garamond for body text, justified
//! prose, old-style figures.
//!
//! Like the Markdown exporter, output depends only on the document contents:
//! all apparatus is numbered in reading order (block order, then anchor
//! position, then annotation id), never in storage order, so exporting the same
//! document twice — or after a serde round trip — produces identical bytes.

use crate::model::{AnnotationKind, Block, BlockId, BlockKind, Document, SpanStyle};
use std::collections::HashMap;

/// The embedded stylesheet.
///
/// The fonts are named, not embedded: Crimson Pro and EB Garamond are the fonts
/// the editor ships (`fonts/`), so an exported file picks them up wherever they
/// are installed and falls back to a serif otherwise.
const STYLE: &str = r#"
:root {
  color-scheme: light;
  --paper: #fdfcf9;
  --ink: #1c1a17;
  --muted: #6f675c;
  --rule: #d9d2c5;
  --highlight: #f4e6a8;
}
html {
  font-size: 100%;
}
body {
  margin: 0;
  background: var(--paper);
  color: var(--ink);
  font-family: "EB Garamond", "Iowan Old Style", Georgia, serif;
  font-size: 1.2rem;
  line-height: 1.62;
  font-variant-numeric: oldstyle-nums;
  font-feature-settings: "liga" 1, "kern" 1, "onum" 1;
  text-align: justify;
  hyphens: auto;
  -webkit-hyphens: auto;
}
main.document {
  box-sizing: border-box;
  max-width: 36rem;
  margin: 0 auto;
  padding: 5rem 1.5rem 6rem;
}
h1, h2, h3, h4, h5, h6 {
  font-family: "Crimson Pro", "EB Garamond", Georgia, serif;
  font-weight: 600;
  line-height: 1.15;
  text-align: left;
  hyphens: none;
  margin: 2.2em 0 0.6em;
}
h1 { font-size: 2.6rem; letter-spacing: -0.01em; }
h2 { font-size: 1.9rem; }
h3 { font-size: 1.45rem; }
h4 { font-size: 1.2rem; }
h5, h6 { font-size: 1.05rem; }
p {
  margin: 0 0 1.1em;
}
strong {
  font-weight: 600;
}
em {
  font-style: italic;
}
ul, ol {
  margin: 0 0 1.1em;
  padding-left: 1.4em;
}
li {
  margin: 0.2em 0;
}
mark {
  background: var(--highlight);
  color: inherit;
  padding: 0 0.1em;
  border-radius: 0.15em;
}
aside.marginalia {
  float: right;
  clear: right;
  width: 13rem;
  margin: 0.2rem -16rem 1rem 2rem;
  font-size: 0.82rem;
  line-height: 1.45;
  font-style: italic;
  color: var(--muted);
  text-align: left;
  hyphens: none;
}
aside.marginalia p {
  margin: 0 0 0.6em;
}
details.note {
  margin: 0 0 1.2em;
  font-size: 0.92rem;
  line-height: 1.5;
  color: var(--muted);
  text-align: left;
}
details.note summary {
  cursor: pointer;
  font-variant-caps: small-caps;
  letter-spacing: 0.04em;
}
details.note p {
  margin: 0.4em 0 0;
}
sup.footnote-ref {
  font-size: 0.7em;
  line-height: 0;
  vertical-align: super;
}
sup.footnote-ref a {
  color: var(--muted);
  text-decoration: none;
  padding: 0 0.15em;
}
ol.footnotes {
  margin: 3rem 0 0;
  padding: 1.2rem 0 0 1.4rem;
  border-top: 1px solid var(--rule);
  font-size: 0.88rem;
  line-height: 1.45;
  text-align: left;
  color: var(--muted);
}
ol.footnotes li {
  margin: 0.5em 0;
}
a.footnote-backref {
  color: var(--muted);
  text-decoration: none;
  margin-left: 0.3em;
}
@media (max-width: 72rem) {
  main.document { max-width: 38rem; }
  aside.marginalia {
    float: none;
    width: auto;
    margin: 0.6rem 0 1.2rem;
    padding-left: 0.9rem;
    border-left: 2px solid var(--rule);
  }
}
@media print {
  body { background: #fff; font-size: 11pt; }
  main.document { padding: 0; }
  aside.marginalia {
    float: right;
    width: 11rem;
    margin: 0 0 1rem 1.5rem;
    padding-left: 0;
    border-left: 0;
  }
}
"#;

/// A note in inline position: the numbered highlight over `start..end` and the
/// body that `render_apparatus` expands beneath the block.
#[derive(Clone)]
struct NoteMark {
    number: usize,
    start: usize,
    end: usize,
    body: String,
}

/// The apparatus anchored to one block, already in reading order.
#[derive(Default, Clone)]
struct BlockApparatus {
    /// Marginal bodies, in reading order.
    marginalia: Vec<String>,
    /// Numbered notes (inline highlight plus expandable body).
    notes: Vec<NoteMark>,
    /// `(anchor end offset, footnote number)` pairs, in reading order.
    footnotes: Vec<(usize, usize)>,
}

/// Render `doc` as a self-contained HTML document.
///
/// Layout rules:
/// - headings become `<h1>`–`<h6>` (level clamped to 1–6), paragraphs `<p>`,
///   and consecutive list items of the same kind share one `<ul>`/`<ol>`,
/// - inline spans become `<em>`/`<strong>`, nesting innermost-first,
/// - marginalia become `<aside class="marginalia">` immediately after the block
///   they annotate (inside the `<li>` for list items), in reading order,
/// - notes wrap their anchored range in `<mark id="noteN">` and add a
///   `<details class="note">` beneath the block,
/// - footnotes leave a `<sup class="footnote-ref"><a href="#fnN">N</a></sup>`
///   marker at the end of their anchored range and collect in one
///   `<ol class="footnotes">` at the foot, numbered in reading order,
/// - empty blocks are skipped, and annotations whose anchor block is missing or
///   empty are dropped,
/// - text is escaped (`&`, `<`, `>`); the output carries no document or
///   annotation ids, so it is stable across runs and serde round trips.
pub fn to_html(doc: &Document) -> String {
    let (apparatus, footnotes) = collect_apparatus(doc);

    let mut body = String::new();
    let mut open_list: Option<bool> = None;
    for (index, block) in doc.blocks.iter().enumerate() {
        // An empty block has no glyphs to carry a marker or to hang a note on.
        // Checked before the list bookkeeping, so an emptied item neither
        // splits a list nor consumes an ordered number.
        if block.text.is_empty() {
            continue;
        }
        let apparatus = &apparatus[index];
        let inner = render_inline(block, &apparatus.notes, &apparatus.footnotes);
        let after = render_apparatus(apparatus);
        match block.kind {
            BlockKind::Heading { level } => {
                close_list(&mut body, &mut open_list);
                let level = level.clamp(1, 6);
                body.push_str(&format!("<h{level}>{inner}</h{level}>\n"));
                body.push_str(&after);
            }
            BlockKind::Paragraph => {
                close_list(&mut body, &mut open_list);
                body.push_str(&format!("<p>{inner}</p>\n"));
                body.push_str(&after);
            }
            BlockKind::ListItem { ordered } => {
                if open_list != Some(ordered) {
                    close_list(&mut body, &mut open_list);
                    body.push_str(if ordered { "<ol>\n" } else { "<ul>\n" });
                    open_list = Some(ordered);
                }
                body.push_str(&format!("<li>{inner}{after}</li>\n"));
            }
        }
    }
    close_list(&mut body, &mut open_list);

    if !footnotes.is_empty() {
        body.push_str("<ol class=\"footnotes\">\n");
        for (number, annotation_body) in &footnotes {
            body.push_str(&format!(
                "<li id=\"fn{number}\">{} <a class=\"footnote-backref\" href=\"#fnref{number}\">&#8617;</a></li>\n",
                escape(annotation_body)
            ));
        }
        body.push_str("</ol>\n");
    }

    let title = escape(&doc.title);
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n\
         <main class=\"document\">\n{body}</main>\n</body>\n</html>\n"
    )
}

/// Close a list still open from the previous block, if any.
fn close_list(out: &mut String, open_list: &mut Option<bool>) {
    match open_list.take() {
        Some(true) => out.push_str("</ol>\n"),
        Some(false) => out.push_str("</ul>\n"),
        None => {}
    }
}

/// Group every live annotation by block, in reading order, and number the
/// footnotes and notes.
///
/// Reading order is block order, then anchor start, then anchor end, then
/// annotation id — storage order never shows through, so numbering is stable.
/// Annotations whose anchor block is gone or empty are dropped: there is no
/// element to put a marker on. Notes whose range covers no glyphs (only
/// whitespace, or an empty/interior offset) are dropped for the same reason.
///
/// Returns the per-block apparatus and the footnote bodies in number order.
fn collect_apparatus(doc: &Document) -> (Vec<BlockApparatus>, Vec<(usize, String)>) {
    let block_order: HashMap<BlockId, usize> = doc
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect();

    let mut ordered: Vec<(usize, usize, usize, &crate::model::Annotation)> = doc
        .annotations
        .iter()
        .filter(|annotation| {
            matches!(
                annotation.kind,
                AnnotationKind::Marginal | AnnotationKind::Note | AnnotationKind::Footnote
            )
        })
        .filter_map(|annotation| {
            let index = *block_order.get(&annotation.anchor.block_id)?;
            let text = doc.blocks[index].text.as_str();
            if text.is_empty() {
                return None;
            }
            match annotation.kind {
                AnnotationKind::Note => {
                    let (start, end) =
                        note_range(text, annotation.anchor.start, annotation.anchor.end);
                    (start < end).then_some((index, start, end, annotation))
                }
                AnnotationKind::Footnote => Some((
                    index,
                    snap_down(text, annotation.anchor.start),
                    snap_down(text, annotation.anchor.end),
                    annotation,
                )),
                AnnotationKind::Marginal => Some((
                    index,
                    annotation.anchor.start,
                    annotation.anchor.end,
                    annotation,
                )),
            }
        })
        .collect();
    ordered.sort_by_key(|(index, start, end, annotation)| (*index, *start, *end, annotation.id));

    let mut apparatus = vec![BlockApparatus::default(); doc.blocks.len()];
    let mut footnotes: Vec<(usize, String)> = Vec::new();
    let mut note_number = 0usize;
    let mut footnote_number = 0usize;
    for (index, start, end, annotation) in ordered {
        match annotation.kind {
            AnnotationKind::Marginal => apparatus[index].marginalia.push(annotation.body.clone()),
            AnnotationKind::Note => {
                note_number += 1;
                apparatus[index].notes.push(NoteMark {
                    number: note_number,
                    start,
                    end,
                    body: annotation.body.clone(),
                });
            }
            AnnotationKind::Footnote => {
                footnote_number += 1;
                apparatus[index].footnotes.push((end, footnote_number));
                footnotes.push((footnote_number, annotation.body.clone()));
            }
        }
    }
    apparatus
        .iter_mut()
        .for_each(|block| block.footnotes.sort_unstable());

    (apparatus, footnotes)
}

/// Render the apparatus that follows a block: marginalia asides, then note
/// details. Footnotes are collected at the foot of the document instead.
fn render_apparatus(apparatus: &BlockApparatus) -> String {
    let mut out = String::new();
    for body in &apparatus.marginalia {
        out.push_str(&format!(
            "<aside class=\"marginalia\"><p>{}</p></aside>\n",
            escape(body)
        ));
    }
    for note in &apparatus.notes {
        out.push_str(&format!(
            "<details class=\"note\"><summary>Note {}</summary><p>{}</p></details>\n",
            note.number,
            escape(&note.body)
        ));
    }
    out
}

/// Inline tags the segment renderer can open, outermost first.
#[derive(Clone, PartialEq)]
enum InlineTag {
    Note(usize),
    Bold,
    Italic,
}

impl InlineTag {
    fn open(&self) -> String {
        match self {
            InlineTag::Note(number) => format!("<mark id=\"note{number}\">"),
            InlineTag::Bold => "<strong>".to_string(),
            InlineTag::Italic => "<em>".to_string(),
        }
    }

    fn close(&self) -> &'static str {
        match self {
            InlineTag::Note(_) => "</mark>",
            InlineTag::Bold => "</strong>",
            InlineTag::Italic => "</em>",
        }
    }
}

/// Render one block's text with spans, note marks and footnote markers applied.
///
/// The text is cut at every span edge, note edge and footnote offset, and the
/// active tag set is taken per segment — the same segmentation the Markdown
/// exporter uses. Tags close inside-out at each boundary and reopen only for
/// what still covers the following text, so `<em>`/`<strong>` stay properly
/// nested even when spans overlap. Notes open outside spans, so a highlighted
/// phrase can carry emphasis without producing crossed tags.
fn render_inline(block: &Block, notes: &[NoteMark], footnotes: &[(usize, usize)]) -> String {
    let text = block.text.as_str();

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

    let mut boundaries =
        Vec::with_capacity(spans.len() * 2 + notes.len() * 2 + footnotes.len() + 2);
    boundaries.push(0);
    boundaries.push(text.len());
    for (start, end, _) in &spans {
        boundaries.push(*start);
        boundaries.push(*end);
    }
    for note in notes {
        boundaries.push(note.start);
        boundaries.push(note.end);
    }
    for (offset, _) in footnotes {
        boundaries.push(*offset);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut out = String::with_capacity(text.len() + boundaries.len() * 4);
    let mut open: Vec<InlineTag> = Vec::new();
    for window in boundaries.windows(2) {
        let (start, end) = (window[0], window[1]);
        if start >= end {
            continue;
        }
        let mut wanted: Vec<InlineTag> = Vec::new();
        for note in notes {
            if note.start <= start && note.end >= end {
                wanted.push(InlineTag::Note(note.number));
            }
        }
        for style in [SpanStyle::Bold, SpanStyle::Italic] {
            let covered = spans
                .iter()
                .any(|(s, e, covered)| *covered == style && *s <= start && *e >= end);
            if covered {
                wanted.push(match style {
                    SpanStyle::Bold => InlineTag::Bold,
                    SpanStyle::Italic => InlineTag::Italic,
                });
            }
        }

        // Close from the inside out, but only what stopped covering the text:
        // an outer tag that still covers the segment stays open rather than
        // being reopened into an adjacency that would nest wrongly.
        while let Some(innermost) = open.last() {
            if wanted.contains(innermost) {
                break;
            }
            out.push_str(innermost.close());
            open.pop();
        }
        // A marker at a tag boundary belongs after the closing and before the
        // opening tags, where the superscript visually sits.
        for (_, number) in footnotes.iter().filter(|(offset, _)| *offset == start) {
            out.push_str(&footnote_marker(*number));
        }
        for tag in &wanted {
            if !open.contains(tag) {
                out.push_str(&tag.open());
                open.push(tag.clone());
            }
        }
        out.push_str(&escape(&text[start..end]));
    }
    while let Some(tag) = open.pop() {
        out.push_str(tag.close());
    }
    // Markers past the last glyph have no segment of their own to sit in.
    for (_, number) in footnotes.iter().filter(|(offset, _)| *offset == text.len()) {
        out.push_str(&footnote_marker(*number));
    }
    out
}

/// The inline superscript marker, linked to the footnote list.
fn footnote_marker(number: usize) -> String {
    format!(
        "<sup class=\"footnote-ref\" id=\"fnref{number}\"><a href=\"#fn{number}\">{number}</a></sup>"
    )
}

/// Escape the three characters that would otherwise be read as markup.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(character),
        }
    }
    out
}

/// Snap a note's anchor to char boundaries, then pull it in past whitespace so
/// the highlight sits against glyphs. Returns an empty range if nothing is left.
fn note_range(text: &str, start: usize, end: usize) -> (usize, usize) {
    let start = snap_up(text, start);
    let end = snap_down(text, end);
    trim_whitespace_edges(text, start, end)
}

/// Pull a snapped `(start, end)` range in past leading and trailing whitespace.
fn trim_whitespace_edges(text: &str, mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end {
        match text[start..end].chars().next() {
            Some(character) if character.is_whitespace() => start += character.len_utf8(),
            _ => break,
        }
    }
    while end > start {
        match text[start..end].chars().next_back() {
            Some(character) if character.is_whitespace() => end -= character.len_utf8(),
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
        Anchor, Annotation, AnnotationKind, Block, BlockId, BlockKind, Document, Span, SpanStyle,
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
        let annotated = paragraph("Anchored text");
        let annotated_id = annotated.id;
        let bullet = block(BlockKind::ListItem { ordered: false }, "Bullet");
        let ordered = block(BlockKind::ListItem { ordered: true }, "First");
        document(
            vec![
                block(BlockKind::Heading { level: 1 }, "A Title"),
                styled,
                annotated,
                bullet,
                ordered,
            ],
            vec![
                annotation(AnnotationKind::Marginal, annotated_id, 0, 8, "margin"),
                annotation(AnnotationKind::Note, annotated_id, 9, 13, "note body"),
                annotation(
                    AnnotationKind::Footnote,
                    annotated_id,
                    0,
                    8,
                    "Footnote body.",
                ),
            ],
        )
    }

    #[test]
    fn marginalia_becomes_aside() {
        let d = rich_document();
        let html = to_html(&d);
        assert!(html.contains("<aside class=\"marginalia\">"));
    }

    #[test]
    fn footnotes_have_anchor_links() {
        let d = rich_document();
        let html = to_html(&d);
        assert!(html.contains("<a href=\"#fn1\">"));
    }

    #[test]
    fn html_is_deterministic() {
        let d = rich_document();
        assert_eq!(to_html(&d), to_html(&d));
    }

    #[test]
    fn empty_document_exports_valid_shell() {
        let html = to_html(&Document::new(""));
        assert!(!html.is_empty());
        assert!(html.starts_with("<!DOCTYPE") || html.starts_with("<html"));
    }

    #[test]
    fn headings_paragraphs_and_lists_export() {
        let html = to_html(&rich_document());
        assert!(html.contains("<h1>A Title</h1>"));
        assert!(html.contains("<p><strong>Hello</strong> <em>world</em></p>"));
        assert!(html.contains("<ul>"));
        assert!(html.contains("<ol>"));
        assert!(html.contains("<li>Bullet</li>"));
        assert!(html.contains("<li>First</li>"));
    }

    #[test]
    fn consecutive_list_items_share_one_list() {
        let doc = document(
            vec![
                block(BlockKind::ListItem { ordered: false }, "One"),
                block(BlockKind::ListItem { ordered: false }, "Two"),
                block(BlockKind::ListItem { ordered: true }, "First"),
                block(BlockKind::ListItem { ordered: true }, "Second"),
            ],
            Vec::new(),
        );
        let html = to_html(&doc);
        assert_eq!(html.matches("<ul>").count(), 1);
        assert_eq!(html.matches("</ul>").count(), 1);
        assert_eq!(html.matches("<ol>").count(), 1);
        assert_eq!(html.matches("</ol>").count(), 1);
        assert!(html.contains("<ul>\n<li>One</li>\n<li>Two</li>\n</ul>"));
        assert!(html.contains("<ol>\n<li>First</li>\n<li>Second</li>\n</ol>"));
    }

    #[test]
    fn notes_become_marks_and_details() {
        let html = to_html(&rich_document());
        assert!(html.contains("<mark id=\"note1\">text</mark>"));
        assert!(html.contains("<details class=\"note\">"));
        assert!(html.contains("<summary>Note 1</summary>"));
        assert!(html.contains("<p>note body</p>"));
    }

    #[test]
    fn marginalia_sits_after_the_annotated_block() {
        let html = to_html(&rich_document());
        let paragraph_end = html.find("</p>").expect("paragraph closes");
        let aside = html.find("<aside class=\"marginalia\">").expect("aside");
        assert!(aside > paragraph_end);
        assert!(html.contains("<aside class=\"marginalia\"><p>margin</p></aside>"));
    }

    #[test]
    fn footnotes_collect_in_a_list_at_the_foot() {
        let html = to_html(&rich_document());
        assert!(html.contains("<ol class=\"footnotes\">"));
        assert!(html.contains("<li id=\"fn1\">Footnote body."));
        assert!(html.contains("href=\"#fnref1\""));
    }

    #[test]
    fn footnote_numbers_follow_document_order_not_storage_order() {
        let alpha = paragraph("Alpha");
        let beta = paragraph("Beta");
        let (alpha_id, beta_id) = (alpha.id, beta.id);
        let doc = document(
            vec![alpha, beta],
            vec![
                annotation(AnnotationKind::Footnote, beta_id, 0, 5, "second"),
                annotation(AnnotationKind::Footnote, alpha_id, 0, 5, "first"),
            ],
        );
        let html = to_html(&doc);
        assert!(html.contains(
            "Alpha<sup class=\"footnote-ref\" id=\"fnref1\"><a href=\"#fn1\">1</a></sup>"
        ));
        assert!(html.contains(
            "Beta<sup class=\"footnote-ref\" id=\"fnref2\"><a href=\"#fn2\">2</a></sup>"
        ));
        assert!(html.contains("<li id=\"fn1\">first"));
        assert!(html.contains("<li id=\"fn2\">second"));
    }

    #[test]
    fn text_and_title_are_escaped() {
        let doc = Document {
            id: Uuid::new_v4(),
            title: "A & B <draft>".to_string(),
            blocks: vec![paragraph("a < b & c > d")],
            annotations: Vec::new(),
        };
        let html = to_html(&doc);
        assert!(html.contains("a &lt; b &amp; c &gt; d"));
        assert!(!html.contains("a < b"));
        assert!(html.contains("<title>A &amp; B &lt;draft&gt;</title>"));
    }

    #[test]
    fn stylesheet_carries_the_house_typography() {
        let html = to_html(&Document::new("Title"));
        assert!(html.contains("<style>"));
        assert!(html.contains("Crimson Pro"));
        assert!(html.contains("EB Garamond"));
        assert!(html.contains("text-align: justify"));
        assert!(html.contains("font-variant-numeric: oldstyle-nums"));
    }

    #[test]
    fn heading_levels_are_clamped() {
        let low = to_html(&document(
            vec![block(BlockKind::Heading { level: 0 }, "Low")],
            Vec::new(),
        ));
        let high = to_html(&document(
            vec![block(BlockKind::Heading { level: 9 }, "High")],
            Vec::new(),
        ));
        assert!(low.contains("<h1>Low</h1>"));
        assert!(high.contains("<h6>High</h6>"));
    }

    #[test]
    fn overlapping_spans_stay_nested() {
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
        assert!(to_html(&document(vec![p], Vec::new()))
            .contains("<em>abcde<strong>fghijklmno</strong></em>p"));
    }

    #[test]
    fn span_edges_are_trimmed_past_whitespace() {
        let mut trailing = paragraph("Hello world");
        trailing.spans = vec![Span {
            start: 0,
            end: 6,
            style: SpanStyle::Bold,
        }];
        let html = to_html(&document(vec![trailing], Vec::new()));
        assert!(html.contains("<p><strong>Hello</strong> world</p>"));
    }

    #[test]
    fn orphan_and_empty_block_annotations_are_skipped() {
        let body = paragraph("Body");
        let empty = paragraph("");
        let empty_id = empty.id;
        let doc = document(
            vec![body, empty],
            vec![
                annotation(AnnotationKind::Marginal, Uuid::new_v4(), 0, 4, "orphan"),
                annotation(AnnotationKind::Footnote, empty_id, 0, 0, "nowhere"),
            ],
        );
        let html = to_html(&doc);
        assert!(!html.contains("orphan"));
        assert!(!html.contains("nowhere"));
        assert!(!html.contains("<aside"));
        assert!(!html.contains("<ol class=\"footnotes\">"));
    }

    #[test]
    fn whitespace_only_note_range_is_dropped() {
        let p = paragraph("Hello world");
        let id = p.id;
        let doc = document(
            vec![p],
            vec![annotation(AnnotationKind::Note, id, 5, 6, "spaces only")],
        );
        let html = to_html(&doc);
        assert!(!html.contains("<mark"));
        assert!(!html.contains("<details"));
        assert!(!html.contains("spaces only"));
    }

    #[test]
    fn emoji_and_multibyte_anchors_do_not_panic() {
        let mut p = paragraph("Ünïcode 😀 text");
        p.spans = vec![Span {
            start: 0,
            end: 999,
            style: SpanStyle::Italic,
        }];
        let id = p.id;
        let doc = document(
            vec![p],
            vec![
                annotation(AnnotationKind::Footnote, id, 0, 4, "snapped"),
                annotation(AnnotationKind::Note, id, 0, 999, "wide"),
            ],
        );
        let html = to_html(&doc);
        assert!(html.contains("<mark id=\"note1\">"));
        assert!(html.contains("<li id=\"fn1\">snapped"));
    }

    #[test]
    fn export_is_stable_across_round_trip() {
        let d = rich_document();
        let json = serde_json::to_string(&d).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(to_html(&d), to_html(&back));
    }
}
