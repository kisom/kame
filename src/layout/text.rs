//! Headless block layout: cosmic-text shaping and byte-offset -> Y mapping.

use std::borrow::Borrow;

use cosmic_text::{
    Align, Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, Weight, Wrap,
};

use crate::model::{Block, BlockId, BlockKind, SpanStyle};

/// Body face (paragraphs, list items); loaded by the caller into the
/// `FontSystem`.
const BODY_FAMILY: &str = "EB Garamond";
/// Heading face; loaded by the caller into the `FontSystem`.
const HEADING_FAMILY: &str = "Crimson Pro";

/// Metrics and column widths for headless layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutConfig {
    pub body_width: f32,
    pub body_size: f32,
    pub body_line_height: f32,
    pub heading_size: f32,
    pub margin_width: f32,
    pub margin_size: f32,
    /// Horizontal indent reserved for a list item's marker (bullet or number).
    pub list_indent: f32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        LayoutConfig {
            body_width: 640.0,
            body_size: 16.0,
            body_line_height: 24.0,
            heading_size: 28.0,
            margin_width: 180.0,
            margin_size: 12.0,
            list_indent: 28.0,
        }
    }
}

/// One visual line of a laid-out block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutLine {
    /// Top of the line, in block-relative pixels (Y grows downward).
    pub y: f32,
    pub height: f32,
    /// Byte range of the block text covered by this line.
    pub start: usize,
    pub end: usize,
}

/// A shaped block. `buffer` carries the glyphs; readers walk
/// `buffer.layout_runs()` to paint or hit-test.
pub struct BlockLayout {
    pub buffer: Buffer,
    pub lines: Vec<LayoutLine>,
    pub height: f32,
    /// How far the text is indented from the block's left edge, to leave room
    /// for a list marker. Zero for paragraphs and headings.
    pub left_indent: f32,
}

/// Lay out one block, shaping it with cosmic-text at the configured column
/// width. Paragraphs are justified; headings and list items are left-aligned.
/// Headings use [`HEADING_FAMILY`]; everything else uses [`BODY_FAMILY`].
/// `block.spans` are applied as bold/italic runs on top of the block's base
/// attributes.
///
/// The returned [`BlockLayout`] owns the cosmic-text [`Buffer`], so callers can
/// walk `buffer.layout_runs()` for glyphs (remembering that a glyph's `y` is
/// line-relative — add `run.line_y`).
pub fn layout_block(
    font_system: &mut FontSystem,
    block: &Block,
    config: &LayoutConfig,
) -> BlockLayout {
    let (family, size, line_height, align) = match block.kind {
        BlockKind::Paragraph => (
            BODY_FAMILY,
            config.body_size,
            config.body_line_height,
            Align::Justified,
        ),
        BlockKind::Heading { .. } => (
            HEADING_FAMILY,
            config.heading_size,
            heading_line_height(config),
            Align::Left,
        ),
        BlockKind::ListItem { .. } => (
            BODY_FAMILY,
            config.body_size,
            config.body_line_height,
            Align::Left,
        ),
    };

    // List items reserve a gutter on the left for their marker, and wrap their
    // text in the remaining column so continuation lines hang under the text
    // rather than under the marker.
    let left_indent = match block.kind {
        BlockKind::ListItem { .. } => config.list_indent,
        _ => 0.0,
    };
    let width = (config.body_width - left_indent).max(1.0);

    let mut buffer = Buffer::new(font_system, Metrics::new(size, line_height));
    buffer.set_size(Some(width), None);
    let attrs = Attrs::new()
        .family(Family::Name(family))
        .weight(Weight::NORMAL)
        .style(Style::Normal);
    let spans = styled_spans(block, &attrs);
    buffer.set_rich_text(spans, &attrs, Shaping::Advanced, Some(align));
    buffer.set_wrap(Wrap::Word);
    buffer.shape_until_scroll(font_system, false);

    let mut lines = Vec::new();
    for run in buffer.layout_runs() {
        // A run with no glyphs is the placeholder cosmic-text emits for an
        // empty visual line; it carries no byte range to anchor against.
        let (first, last) = match (run.glyphs.first(), run.glyphs.last()) {
            (Some(first), Some(last)) if first.start < last.end => (first, last),
            _ => continue,
        };
        lines.push(LayoutLine {
            y: run.line_top,
            height: run.line_height,
            start: first.start,
            end: last.end,
        });
    }
    let height = lines.last().map_or(0.0, |line| line.y + line.height);

    BlockLayout {
        buffer,
        lines,
        height,
        left_indent,
    }
}

/// Split the block text into `(text, attrs)` runs at span boundaries, layering
/// `block.spans` (Bold → `Weight::BOLD`, Italic → `Style::Italic`) over `base`.
///
/// Runs are produced in text order and concatenate to exactly `block.text`, so
/// cosmic-text's offsets stay aligned with the block's byte offsets. Span
/// boundaries that fall inside a UTF-8 sequence are ignored rather than
/// panicking on a non-char-boundary slice.
fn styled_spans<'a>(block: &'a Block, base: &Attrs<'a>) -> Vec<(&'a str, Attrs<'a>)> {
    let text = block.text.as_str();
    if text.is_empty() || block.spans.is_empty() {
        return vec![(text, base.clone())];
    }

    let mut bounds = Vec::with_capacity(block.spans.len() * 2 + 2);
    bounds.push(0);
    bounds.push(text.len());
    for span in &block.spans {
        for bound in [span.start, span.end] {
            let bound = bound.min(text.len());
            if text.is_char_boundary(bound) {
                bounds.push(bound);
            }
        }
    }
    bounds.sort_unstable();
    bounds.dedup();

    bounds
        .windows(2)
        .filter(|pair| pair[0] < pair[1])
        .map(|pair| {
            let (start, end) = (pair[0], pair[1]);
            let covers = |style: SpanStyle| {
                block
                    .spans
                    .iter()
                    .any(|span| span.style == style && span.start <= start && span.end >= end)
            };
            let mut attrs = base.clone();
            if covers(SpanStyle::Bold) {
                attrs = attrs.weight(Weight::BOLD);
            }
            if covers(SpanStyle::Italic) {
                attrs = attrs.style(Style::Italic);
            }
            (&text[start..end], attrs)
        })
        .collect()
}

/// Line height for headings, keeping the body's leading ratio so a heading
/// occupies proportionally more vertical space than a body line.
fn heading_line_height(config: &LayoutConfig) -> f32 {
    if config.body_size > 0.0 {
        config.heading_size * (config.body_line_height / config.body_size)
    } else {
        config.heading_size * 1.2
    }
}

/// Top Y of the line in `block_id` that covers byte `offset`.
///
/// `blocks` and `layouts` are parallel: `layouts[i]` is the layout of
/// `blocks[i]`. Offsets that sit in inter-line whitespace fall forward to the
/// next line, and an offset at the very end of the block maps to the last
/// line (where a cursor after the final character belongs).
pub fn anchor_to_y<L: Borrow<BlockLayout>>(
    blocks: &[Block],
    layouts: &[L],
    block_id: BlockId,
    offset: usize,
) -> Option<f32> {
    let index = blocks.iter().position(|block| block.id == block_id)?;
    let block = &blocks[index];
    let layout = layouts.get(index)?.borrow();

    if let Some(line) = layout
        .lines
        .iter()
        .find(|line| offset >= line.start && offset < line.end)
    {
        return Some(line.y);
    }
    if let Some(line) = layout.lines.iter().find(|line| line.start > offset) {
        return Some(line.y);
    }
    if offset == block.text.len() {
        return layout.lines.last().map(|line| line.y);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BlockKind, Span};
    use cosmic_text::{Family, FontSystem};
    use std::path::PathBuf;
    use uuid::Uuid;

    const PARA: &str = "Typography is the craft of endowing human language with a durable \
visual form, and thus with an independent existence. Its heartwood is calligraphy: the \
dance, on a page, of pen, eye and hand.";

    fn font_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fonts")
            .join(name)
    }

    fn load_fonts(fs: &mut FontSystem) {
        for file in [
            "EBGaramond-Regular.otf",
            "EBGaramond-Bold.otf",
            "EBGaramond-Italic.otf",
            "CrimsonPro-Regular.ttf",
        ] {
            fs.db_mut()
                .load_font_file(font_path(file))
                .unwrap_or_else(|e| panic!("load {file}: {e}"));
        }
    }

    /// Resolve a registered face by family/weight/style.
    fn face(
        fs: &FontSystem,
        family: &str,
        weight: Weight,
        style: Style,
    ) -> cosmic_text::fontdb::ID {
        fs.db()
            .query(&cosmic_text::fontdb::Query {
                families: &[Family::Name(family)],
                weight,
                style,
                ..Default::default()
            })
            .unwrap_or_else(|| {
                panic!(
                    "{family} ({weight:?}, {style:?}) is registered after loading the font files"
                )
            })
    }

    fn config() -> LayoutConfig {
        LayoutConfig {
            body_width: 300.0,
            body_size: 16.0,
            body_line_height: 24.0,
            heading_size: 28.0,
            margin_width: 120.0,
            margin_size: 12.0,
            list_indent: 28.0,
        }
    }

    fn paragraph(text: &str) -> Block {
        Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    /// Width of `line`, read off the matching layout run's pen position.
    fn line_width(layout: &BlockLayout, line: &LayoutLine) -> f32 {
        layout
            .buffer
            .layout_runs()
            .find(|run| !run.glyphs.is_empty() && (run.line_top - line.y).abs() < 0.01)
            .map(|run| run.line_w)
            .expect("layout line has a matching run")
    }

    #[test]
    fn justified_lines_fill_body_width() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph(PARA);
        let config = config();
        let l = layout_block(&mut fs, &block, &config);
        assert!(
            l.lines.len() >= 3,
            "expected several lines, got {}",
            l.lines.len()
        );
        for line in l.lines.iter().take(l.lines.len() - 1) {
            assert!(
                (line_width(&l, line) - config.body_width).abs() < 1.0,
                "line at y {} is {} wide, expected {}",
                line.y,
                line_width(&l, line),
                config.body_width
            );
        }
        // The final line is not stretched to the full column.
        let last = l.lines.last().unwrap();
        assert!(line_width(&l, last) < config.body_width - 1.0);
    }

    #[test]
    fn anchor_to_y_finds_line() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph(PARA);
        let config = config();
        let l = layout_block(&mut fs, &block, &config);
        assert!(l.lines.len() >= 3);
        let target = l.lines[1];
        let offset = (target.start + target.end) / 2;
        assert!(offset >= target.start && offset < target.end);

        let blocks = vec![block];
        let layouts = vec![l];
        let y = anchor_to_y(&blocks, &layouts, blocks[0].id, offset).unwrap();
        assert_eq!(y, target.y);
    }

    #[test]
    fn anchor_to_y_ignores_unknown_block() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph(PARA);
        let config = config();
        let l = layout_block(&mut fs, &block, &config);
        let layouts = vec![l];
        assert_eq!(anchor_to_y(&[block], &layouts, Uuid::new_v4(), 0), None);
    }

    #[test]
    fn line_ranges_are_ordered_and_cover_text() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph(PARA);
        let config = config();
        let l = layout_block(&mut fs, &block, &config);

        assert_eq!(l.lines[0].start, 0);
        let mut prev_end = 0;
        for line in &l.lines {
            assert!(
                line.start >= prev_end,
                "line ranges overlap or go backwards"
            );
            assert!(line.end > line.start, "empty byte range");
            prev_end = line.end;
        }
        assert_eq!(l.lines.last().unwrap().end, block.text.len());
        assert!((l.height - l.lines.len() as f32 * config.body_line_height).abs() < 0.01);
    }

    #[test]
    fn body_text_shapes_with_eb_garamond() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let face = fs
            .db()
            .query(&cosmic_text::fontdb::Query {
                families: &[Family::Name("EB Garamond")],
                ..Default::default()
            })
            .expect("EB Garamond is registered after loading the font files");

        let block = paragraph("Garamond");
        let l = layout_block(&mut fs, &block, &config());
        let run = l.buffer.layout_runs().next().expect("one shaped run");
        assert!(!run.glyphs.is_empty());
        assert!(
            run.glyphs.iter().all(|glyph| glyph.font_id == face),
            "glyphs fell back to another face"
        );
    }

    #[test]
    fn heading_shapes_with_crimson_pro() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let crimson = face(&fs, "Crimson Pro", Weight::NORMAL, Style::Normal);
        let garamond = face(&fs, "EB Garamond", Weight::NORMAL, Style::Normal);
        assert_ne!(crimson, garamond, "headings and body must not share a face");

        let block = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Heading { level: 1 },
            text: "On the Shape of Letters".to_string(),
            spans: Vec::new(),
        };
        let l = layout_block(&mut fs, &block, &config());
        let run = l.buffer.layout_runs().next().expect("one shaped run");
        assert!(!run.glyphs.is_empty());
        assert!(
            run.glyphs.iter().all(|glyph| glyph.font_id == crimson),
            "heading glyphs did not resolve to the Crimson Pro face"
        );
        assert!(
            run.glyphs.iter().all(|glyph| glyph.font_id != garamond),
            "heading glyphs fell back to EB Garamond"
        );

        // Body text keeps the EB Garamond face.
        let body = paragraph("Garamond");
        let l = layout_block(&mut fs, &body, &config());
        let run = l.buffer.layout_runs().next().expect("one shaped run");
        assert!(run.glyphs.iter().all(|glyph| glyph.font_id == garamond));
    }

    #[test]
    fn bold_span_shapes_with_bold_weight() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let bold_face = face(&fs, "EB Garamond", Weight::BOLD, Style::Normal);
        let regular_face = face(&fs, "EB Garamond", Weight::NORMAL, Style::Normal);

        let mut block = paragraph("plain bold plain");
        block.spans = vec![Span {
            start: 6,
            end: 10,
            style: SpanStyle::Bold,
        }];
        let l = layout_block(&mut fs, &block, &config());
        let run = l.buffer.layout_runs().next().expect("one shaped run");
        assert!(!run.glyphs.is_empty());

        let mut bold_glyphs = 0;
        let mut normal_glyphs = 0;
        for glyph in run.glyphs.iter() {
            if glyph.start >= 6 && glyph.end <= 10 {
                bold_glyphs += 1;
                assert_eq!(
                    glyph.font_weight,
                    Weight::BOLD,
                    "span glyph kept normal weight"
                );
                assert_eq!(
                    glyph.font_id, bold_face,
                    "bold span did not use the bold face"
                );
            } else {
                normal_glyphs += 1;
                assert_eq!(glyph.font_weight, Weight::NORMAL, "plain glyph became bold");
                assert_eq!(glyph.font_id, regular_face);
            }
        }
        assert!(bold_glyphs > 0, "no glyphs shaped inside the bold span");
        assert!(normal_glyphs > 0, "no glyphs shaped outside the bold span");
    }

    #[test]
    fn italic_span_shapes_with_italic_face() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let italic_face = face(&fs, "EB Garamond", Weight::NORMAL, Style::Italic);
        let regular_face = face(&fs, "EB Garamond", Weight::NORMAL, Style::Normal);

        let mut block = paragraph("plain italic plain");
        block.spans = vec![Span {
            start: 6,
            end: 12,
            style: SpanStyle::Italic,
        }];
        let l = layout_block(&mut fs, &block, &config());
        let run = l.buffer.layout_runs().next().expect("one shaped run");
        assert!(!run.glyphs.is_empty());

        let mut italic_glyphs = 0;
        let mut normal_glyphs = 0;
        for glyph in run.glyphs.iter() {
            if glyph.start >= 6 && glyph.end <= 12 {
                italic_glyphs += 1;
                assert_eq!(glyph.font_weight, Weight::NORMAL);
                assert_eq!(
                    glyph.font_id, italic_face,
                    "italic span did not use the italic face"
                );
            } else {
                normal_glyphs += 1;
                assert_eq!(glyph.font_id, regular_face, "plain glyph became italic");
            }
        }
        assert!(italic_glyphs > 0, "no glyphs shaped inside the italic span");
        assert!(
            normal_glyphs > 0,
            "no glyphs shaped outside the italic span"
        );
    }

    #[test]
    fn empty_block_layouts_without_panic() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph("");
        let config = config();
        let id = block.id;
        let l = layout_block(&mut fs, &block, &config);
        assert!(l.lines.is_empty());
        assert_eq!(l.height, 0.0);
        assert_eq!(anchor_to_y(&[block], &[l], id, 0), None);
    }

    #[test]
    fn heading_produces_nonzero_height() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Heading { level: 1 },
            text: "On the Shape of Letters".to_string(),
            spans: Vec::new(),
        };
        let l = layout_block(&mut fs, &block, &config());
        assert!(l.height > 0.0);
        assert!(!l.lines.is_empty());
    }

    #[test]
    fn list_item_produces_nonzero_height() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::ListItem { ordered: true },
            text: "A short list item.".to_string(),
            spans: Vec::new(),
        };
        let l = layout_block(&mut fs, &block, &config());
        assert!(l.height > 0.0);
        assert_eq!(l.lines.len(), 1);
    }

    #[test]
    fn list_items_indent_but_paragraphs_do_not() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let config = config();
        let list = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::ListItem { ordered: false },
            text: "item".to_string(),
            spans: Vec::new(),
        };
        let paragraph = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "para".to_string(),
            spans: Vec::new(),
        };
        assert_eq!(
            layout_block(&mut fs, &list, &config).left_indent,
            config.list_indent
        );
        assert_eq!(layout_block(&mut fs, &paragraph, &config).left_indent, 0.0);
    }
}
