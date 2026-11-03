//! Headless block layout: cosmic-text shaping and byte-offset -> Y mapping.

use std::borrow::Borrow;

use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, Weight, Wrap,
};

use crate::model::{Block, BlockId, BlockKind, SpanStyle};

/// Body face (paragraphs, list items); loaded by the caller into the
/// `FontSystem`.
const BODY_FAMILY: &str = "EB Garamond";
/// Heading face; loaded by the caller into the `FontSystem`.
const HEADING_FAMILY: &str = "Crimson Pro";
/// Code face; loaded by the caller into the `FontSystem`.
const CODE_FAMILY: &str = "IBM Plex Mono";
/// The fixed colour a URL renders in. Chosen to read on both the light and the
/// dark palette, so it can be baked into the layout without a palette swap
/// invalidating the layout cache.
pub const LINK_COLOR: Color = Color::rgb(59, 130, 246);

/// Metrics and column widths for headless layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutConfig {
    pub body_width: f32,
    pub body_size: f32,
    pub body_line_height: f32,
    pub heading_size: f32,
    pub margin_width: f32,
    pub margin_size: f32,
    pub list_indent: f32,
}

/// The body size every other font-derived size is a ratio of.
pub const DEFAULT_BODY_SIZE: f32 = 16.0;
/// Page geometry at the default size.
const DEFAULT_BODY_WIDTH: f32 = 640.0;
const DEFAULT_MARGIN_WIDTH: f32 = 180.0;
/// Ratios of `body_size`, kept in one place so a single font-size choice scales
/// the whole type system (body leading, headings, margin apparatus, list gutter).
const LINE_HEIGHT_RATIO: f32 = 1.5;
const HEADING_RATIO: f32 = 1.75;
const MARGIN_RATIO: f32 = 0.75;
const LIST_INDENT_RATIO: f32 = 1.75;
/// How far a block quote is indented from the measure, as a ratio of
/// `body_size`.
const QUOTE_INDENT_RATIO: f32 = 1.5;
/// Columns between tab stops in a code or prose block.
const VERBATIM_TAB_WIDTH: u16 = 4;
/// A monospaced face reads large beside the serif at the same size.
const CODE_RATIO: f32 = 0.84;

impl LayoutConfig {
    /// A config whose font-derived sizes are ratios of `body_size`, at the
    /// default page geometry.
    pub fn with_body_size(body_size: f32) -> LayoutConfig {
        LayoutConfig {
            body_width: DEFAULT_BODY_WIDTH,
            body_size,
            body_line_height: body_size * LINE_HEIGHT_RATIO,
            heading_size: body_size * HEADING_RATIO,
            margin_width: DEFAULT_MARGIN_WIDTH,
            margin_size: body_size * MARGIN_RATIO,
            list_indent: body_size * LIST_INDENT_RATIO,
        }
    }
}

impl Default for LayoutConfig {
    fn default() -> Self {
        LayoutConfig::with_body_size(DEFAULT_BODY_SIZE)
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

/// One visual line as it is drawn. cosmic-text won't let a shaped line be changed, and
/// hanging punctuation moves its glyphs, so painting and hit-testing read these.
pub struct LayoutRun {
    /// Index of the `\n`-separated line this belongs to.
    pub line_i: usize,
    pub glyphs: Vec<cosmic_text::LayoutGlyph>,
    /// Y offset to the line's baseline.
    pub line_y: f32,
    /// Y offset to the top of the line.
    pub line_top: f32,
    pub line_height: f32,
}

/// A shaped block. `runs` carries the glyphs as drawn, which is what readers walk to
/// paint or hit-test.
pub struct BlockLayout {
    /// The lines as the engine shaped them, which tests compare against.
    #[cfg(test)]
    pub buffer: Buffer,
    pub runs: Vec<LayoutRun>,
    pub lines: Vec<LayoutLine>,
    pub height: f32,
    /// How far the text is indented from the block's left edge, to leave room
    /// for a list marker. Zero for paragraphs and headings.
    pub left_indent: f32,
    /// The block offset each of the text engine's lines starts at. Its glyphs count
    /// from the start of their own line, so a line break would otherwise restart
    /// the offsets; see [`BlockLayout::line_start`].
    line_starts: Vec<usize>,
}

impl BlockLayout {
    /// The block offset of the start of the text engine's line `line_i`, to add to
    /// the offsets of the glyphs on it.
    pub fn line_start(&self, line_i: usize) -> usize {
        self.line_starts
            .get(line_i)
            .or(self.line_starts.last())
            .copied()
            .unwrap_or(0)
    }
}

/// Where each line of `text` starts, splitting where the text engine does.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let next = match c {
            '\r' => match chars.peek() {
                Some((newline, '\n')) => {
                    let newline = *newline;
                    chars.next();
                    newline + 1
                }
                _ => at + 1,
            },
            '\n' | '\u{1c}'..='\u{1e}' | '\u{85}' | '\u{2029}' => at + c.len_utf8(),
            _ => continue,
        };
        starts.push(next);
    }
    starts
}

/// Lay out one block, shaping it with cosmic-text at the configured column
/// width. Paragraphs are justified; headings and list items are left-aligned.
/// Headings use [`HEADING_FAMILY`]; everything else uses [`BODY_FAMILY`].
/// `block.spans` are applied as bold/italic runs on top of the block's base
/// attributes.
///
/// The returned [`BlockLayout`] owns the cosmic-text [`Buffer`], so callers can
/// walk `runs` for glyphs (remembering that a glyph's `y` is
/// line-relative — add `run.line_y`).
#[cfg(test)]
pub fn layout_block(
    font_system: &mut FontSystem,
    block: &Block,
    config: &LayoutConfig,
) -> BlockLayout {
    layout_block_marked(font_system, block, config, &[])
}

/// [`layout_block`] with an explicit alignment, so callers shaping apparatus
/// text (margin notes, footnote entries) can left-align it even though the
/// block kind is a paragraph.
pub fn layout_block_with_align(
    font_system: &mut FontSystem,
    block: &Block,
    config: &LayoutConfig,
    align: Align,
) -> BlockLayout {
    layout_block_marked_with_align(font_system, block, config, align, &[])
}

/// [`layout_block_marked`] with an explicit alignment.
pub fn layout_block_marked_with_align(
    font_system: &mut FontSystem,
    block: &Block,
    config: &LayoutConfig,
    align: Align,
    marks: &[usize],
) -> BlockLayout {
    let (family, size, line_height, style) = match block.kind {
        BlockKind::Paragraph => (
            BODY_FAMILY,
            config.body_size,
            config.body_line_height,
            Style::Normal,
        ),
        BlockKind::Heading { level } => {
            let size = heading_size_for(config, level);
            (
                HEADING_FAMILY,
                size,
                heading_line_height(config, size),
                Style::Normal,
            )
        }
        BlockKind::ListItem { .. } => (
            BODY_FAMILY,
            config.body_size,
            config.body_line_height,
            Style::Normal,
        ),
        BlockKind::BlockQuote => (
            BODY_FAMILY,
            config.body_size,
            config.body_line_height,
            Style::Italic,
        ),
        BlockKind::Code { .. } => (
            CODE_FAMILY,
            config.body_size * CODE_RATIO,
            config.body_line_height * CODE_RATIO,
            Style::Normal,
        ),
        BlockKind::Prose => (
            BODY_FAMILY,
            config.body_size,
            config.body_line_height,
            Style::Normal,
        ),
        // An image or a rule carries no prose: its text is empty, so this arm shapes
        // an empty buffer and `block_height` supplies the block's real height.
        BlockKind::Image { .. } | BlockKind::Rule => (
            BODY_FAMILY,
            config.body_size,
            config.body_line_height,
            Style::Normal,
        ),
    };

    // List items reserve a gutter on the left for their marker, and wrap their
    // text in the remaining column so continuation lines hang under the text
    // rather than under the marker. A block quote is indented with no marker:
    // its whole column shifts right and wraps in the remaining width.
    let left_indent = match block.kind {
        BlockKind::ListItem { depth, .. } => {
            config.list_indent * f32::from(depth.min(crate::model::MAX_LIST_DEPTH) + 1)
        }
        BlockKind::BlockQuote | BlockKind::Code { .. } | BlockKind::Prose => {
            config.body_size * QUOTE_INDENT_RATIO
        }
        _ => 0.0,
    };
    let width = (config.body_width - left_indent).max(1.0);

    let mut buffer = Buffer::new(font_system, Metrics::new(size, line_height));
    buffer.set_size(Some(width), None);
    if block.kind.is_verbatim() {
        buffer.set_tab_width(VERBATIM_TAB_WIDTH);
    }
    let attrs = Attrs::new()
        .family(Family::Name(family))
        .weight(if matches!(block.kind, BlockKind::Heading { .. }) {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(style);
    let spans = styled_spans(
        block,
        &attrs,
        marks,
        Metrics::new(size * CODE_RATIO, line_height),
    );
    buffer.set_rich_text(spans, &attrs, Shaping::Advanced, Some(align));
    buffer.set_wrap(Wrap::Word);
    buffer.shape_until_scroll(font_system, false);

    let line_starts = line_starts(&block.text);
    let mut lines = Vec::new();
    for run in buffer.layout_runs() {
        let base = line_starts.get(run.line_i).copied().unwrap_or(0);
        let (start, end) = match (run.glyphs.first(), run.glyphs.last()) {
            (Some(first), Some(last)) if first.start < last.end => {
                (base + first.start, base + last.end)
            }
            // A blank line between line breaks still holds a caret. The placeholder
            // for a wholly empty block carries nothing to anchor against.
            _ if block.text.is_empty() => continue,
            _ => (base, base),
        };
        lines.push(LayoutLine {
            y: run.line_top,
            height: run.line_height,
            start,
            end,
        });
    }
    // The text engine makes no line for a trailing break, but the caret goes there.
    if block.text.ends_with('\n') {
        if let Some(last) = lines.last().copied() {
            lines.push(LayoutLine {
                y: last.y + last.height,
                height: last.height,
                start: block.text.len(),
                end: block.text.len(),
            });
        }
    }
    let height = lines.last().map_or(0.0, |line| line.y + line.height);

    let runs = drawn_runs(&buffer, block, width, align);

    BlockLayout {
        #[cfg(test)]
        buffer,
        runs,
        lines,
        height,
        left_indent,
        line_starts,
    }
}

/// The shaped lines as they are drawn, with their punctuation hung if `hang`.
fn drawn_runs(buffer: &Buffer, block: &Block, width: f32, align: Align) -> Vec<LayoutRun> {
    let hang = !block.kind.is_verbatim();
    let starts = line_starts(&block.text);
    let shaped: Vec<_> = buffer.layout_runs().collect();
    shaped
        .iter()
        .enumerate()
        .map(|(index, run)| {
            let mut glyphs = run.glyphs.to_vec();
            if hang && !run.rtl {
                // The engine stretches every line of a paragraph but its last, so only
                // those have a right edge for punctuation to hang past.
                let last_of_paragraph = shaped
                    .get(index + 1)
                    .is_none_or(|next| next.line_i != run.line_i);
                let justified = align == Align::Justified && !last_of_paragraph;
                let base = starts.get(run.line_i).copied().unwrap_or(0);
                let exact = |glyph: &cosmic_text::LayoutGlyph| {
                    block.spans.iter().any(|span| {
                        span.style == SpanStyle::Code
                            && span.start <= base + glyph.start
                            && base + glyph.end <= span.end
                    })
                };
                hang_line(&mut glyphs, run.text, width, justified, exact);
            }
            LayoutRun {
                line_i: run.line_i,
                glyphs,
                line_y: run.line_y,
                line_top: run.line_top,
                line_height: run.line_height,
            }
        })
        .collect()
}

/// How much of an opening mark's width hangs left of the measure.
fn left_hang(mark: char) -> f32 {
    match mark {
        '\u{201c}' | '\u{2018}' | '"' | '\'' => 0.7,
        '(' | '[' | '{' => 0.2,
        _ => 0.0,
    }
}

/// How much of a closing mark's width hangs right of the measure. The lightest marks
/// leave the biggest bite out of the edge, so they hang furthest.
fn right_hang(mark: char) -> f32 {
    match mark {
        '.' | ',' | '-' | '\u{2010}' | '\u{201d}' | '\u{2019}' | '"' | '\'' => 0.7,
        ':' | ';' | '\u{2013}' => 0.4,
        '!' | '?' | '\u{2014}' => 0.25,
        ')' | ']' | '}' => 0.2,
        _ => 0.0,
    }
}

/// Moves a line's glyphs so its punctuation hangs outside the measure, which keeps the
/// edge of the text looking straight. A justified line is stretched by what was hung, so
/// that its words still reach both edges.
fn hang_line(
    glyphs: &mut [cosmic_text::LayoutGlyph],
    text: &str,
    width: f32,
    justified: bool,
    exact: impl Fn(&cosmic_text::LayoutGlyph) -> bool,
) {
    let glyph_text = |glyph: &cosmic_text::LayoutGlyph| &text[glyph.start..glyph.end];
    let Some(last) = glyphs
        .iter()
        .rposition(|glyph| !glyph_text(glyph).trim().is_empty())
    else {
        return;
    };
    if glyphs.iter().any(|glyph| glyph.level.is_rtl()) {
        return;
    }
    let left = if exact(&glyphs[0]) {
        0.0
    } else {
        glyph_text(&glyphs[0]).chars().next().map_or(0.0, left_hang) * glyphs[0].w
    };
    // Two marks can end a line together, as in a period and a closing quote.
    let right: f32 = if justified {
        glyphs[..=last]
            .iter()
            .rev()
            .take(2)
            .take_while(|glyph| {
                !exact(glyph)
                    && glyph_text(glyph)
                        .chars()
                        .next_back()
                        .is_some_and(|mark| right_hang(mark) > 0.0)
            })
            .map(|glyph| {
                glyph_text(glyph)
                    .chars()
                    .next_back()
                    .map_or(0.0, right_hang)
                    * glyph.w
            })
            .sum()
    } else {
        0.0
    };
    if left == 0.0 && right == 0.0 {
        return;
    }
    let spaces = glyphs[..last]
        .iter()
        .filter(|glyph| glyph_text(glyph).trim().is_empty())
        .count();
    // What the line has to grow by, shared between its spaces. A single word has none,
    // and keeps its right edge. A ragged line has no need to grow.
    let spread = if justified && spaces > 0 {
        let end = glyphs[last].x + glyphs[last].w;
        (width - end + left + right) / spaces as f32
    } else {
        0.0
    };
    let mut moved = -left;
    for (index, glyph) in glyphs.iter_mut().enumerate() {
        glyph.x += moved;
        if index < last && glyph_text(glyph).trim().is_empty() {
            glyph.w += spread;
            moved += spread;
        }
    }
}

/// The bare URLs in `block` that read as links. Code is literal, so a URL in a code
/// block or in inline code is only text.
pub fn bare_link_ranges(block: &Block) -> Vec<(usize, usize)> {
    if matches!(block.kind, BlockKind::Code { .. }) {
        return Vec::new();
    }
    link_ranges(&block.text)
        .into_iter()
        .filter(|(start, end)| {
            !block
                .spans
                .iter()
                .any(|span| span.style == SpanStyle::Code && span.start < *end && *start < span.end)
        })
        .collect()
}

/// Byte ranges of URLs in `text`, each extended to the next whitespace and
/// with trailing punctuation (`.,;:!?)]}`) trimmed. Recognises `http://`,
/// `https://` and `www.` forms, so a pasted or typed link becomes a clickable
/// range without the author marking it up.
pub fn link_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let scheme = ["https://", "http://", "www."]
            .into_iter()
            .find(|prefix| rest.starts_with(prefix));
        let Some(scheme) = scheme else {
            i += text[i..].chars().next().map_or(0, char::len_utf8);
            continue;
        };
        let start = i;
        // The link runs to the next whitespace.
        let mut end = text.len();
        for (j, c) in text[i + scheme.len()..].char_indices() {
            if c.is_whitespace() {
                end = i + scheme.len() + j;
                break;
            }
        }
        // Trim trailing punctuation that belongs to the sentence, not the URL.
        while end > start + scheme.len() {
            let last = text[..end].chars().next_back().unwrap();
            if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}') {
                end -= last.len_utf8();
            } else {
                break;
            }
        }
        ranges.push((start, end));
        i = end;
    }
    ranges
}

/// Split the block text into `(text, attrs)` runs at span boundaries, layering
/// `block.spans` (Bold → `Weight::BOLD`, Italic → `Style::Italic`) over `base`,
/// and colouring any URL (see [`link_ranges`]) with [`LINK_COLOR`].
///
/// Runs are produced in text order and concatenate to exactly `block.text`, so
/// cosmic-text's offsets stay aligned with the block's byte offsets. Span
/// boundaries that fall inside a UTF-8 sequence are ignored rather than
/// panicking on a non-char-boundary slice.
fn styled_spans<'a>(
    block: &'a Block,
    base: &Attrs<'a>,
    marks: &[usize],
    code_metrics: Metrics,
) -> Vec<(&'a str, Attrs<'a>)> {
    let text = block.text.as_str();
    if text.is_empty() {
        return vec![(text, base.clone())];
    }
    let links = bare_link_ranges(block);

    let mut bounds = Vec::with_capacity(block.spans.len() * 2 + links.len() * 2 + 2);
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
    for (start, end) in &links {
        if text.is_char_boundary(*start) && text.is_char_boundary(*end) {
            bounds.push(*start);
            bounds.push(*end);
        }
    }
    // Each mark's room is on the character before it, so that character is a run
    // of its own. A mark with no character before it has nothing to widen.
    let marked: Vec<usize> = marks
        .iter()
        .filter(|mark| **mark <= text.len() && text.is_char_boundary(**mark))
        .filter_map(|mark| {
            let before = text[..*mark].chars().next_back()?;
            Some(*mark - before.len_utf8())
        })
        .collect();
    bounds.extend(marked.iter().copied());
    bounds.extend(
        marks
            .iter()
            .copied()
            .filter(|mark| *mark <= text.len() && text.is_char_boundary(*mark)),
    );
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
            // Smaller to sit evenly beside the serif, on the line's own leading, and
            // upright even in an italic quote unless it was itself made italic.
            if covers(SpanStyle::Code) {
                let style = if covers(SpanStyle::Italic) {
                    Style::Italic
                } else {
                    Style::Normal
                };
                attrs = attrs
                    .family(Family::Name(CODE_FAMILY))
                    .style(style)
                    .metrics(code_metrics);
            }
            // A stored hyperlink or a bare URL both read as a link, coloured the
            // same way so the two forms are indistinguishable on the page.
            let is_stored_link = block.spans.iter().any(|span| {
                matches!(
                    span.style,
                    SpanStyle::Link { .. } | SpanStyle::DocumentLink { .. }
                ) && span.start <= start
                    && span.end >= end
            });
            if is_stored_link || links.iter().any(|(ls, le)| *ls <= start && end <= *le) {
                attrs = attrs.color(LINK_COLOR);
            }
            if marked.contains(&start) {
                attrs = attrs.letter_spacing(FOOTNOTE_MARK_EM);
            }
            (&text[start..end], attrs)
        })
        .collect()
}

/// The size a heading of `level` is set at: `heading_size` for the largest, then
/// down a 1.25 ratio per level, reaching the body size at level 4, where the
/// heading face alone sets it apart.
pub fn heading_size_for(config: &LayoutConfig, level: u8) -> f32 {
    let ratio = match level {
        0 | 1 => 1.0,
        2 => 0.8,
        3 => 0.64,
        _ => 1.0 / HEADING_RATIO,
    };
    config.heading_size * ratio
}

/// Room reserved for a footnote's superscript, in ems of the body size. It fits
/// one digit with a little air; two digits take some of the word space after them.
pub const FOOTNOTE_MARK_EM: f32 = 0.4;

/// [`layout_block`] leaving room at each byte offset in `marks`, for a footnote's
/// superscript. The room is extra advance on the glyph before the mark, so the
/// text and its offsets are unchanged.
pub fn layout_block_marked(
    font_system: &mut FontSystem,
    block: &Block,
    config: &LayoutConfig,
    marks: &[usize],
) -> BlockLayout {
    // Prose paragraphs justify to the measure; headings, lists and quotes stay
    // left-aligned so a ragged right edge reads naturally in narrow or indented
    // columns.
    let align = match block.kind {
        BlockKind::Paragraph => Align::Justified,
        _ => Align::Left,
    };
    layout_block_marked_with_align(font_system, block, config, align, marks)
}

/// Line height for headings, keeping the body's leading ratio so a heading
/// occupies proportionally more vertical space than a body line.
fn heading_line_height(config: &LayoutConfig, size: f32) -> f32 {
    if config.body_size > 0.0 {
        size * (config.body_line_height / config.body_size)
    } else {
        size * 1.2
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
            "CrimsonPro-Bold.ttf",
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
    fn left_aligned_lines_are_ragged_not_stretched() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph(PARA);
        let config = config();
        let l = layout_block_with_align(&mut fs, &block, &config, Align::Left);
        assert!(l.lines.len() >= 3, "expected several lines");

        // Left-aligned lines wrap at word boundaries and are never stretched to
        // the measure, so at least one non-final line is clearly narrower than
        // the column. Justified text (the paragraph default) stretches every
        // non-final line to fill it, which is the two behaviours we are keeping
        // apart.
        let ragged = l
            .lines
            .iter()
            .take(l.lines.len() - 1)
            .any(|line| line_width(&l, line) < config.body_width - 8.0);
        assert!(ragged, "left-aligned text should leave a ragged right edge");
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
        let crimson = face(&fs, "Crimson Pro", Weight::BOLD, Style::Normal);
        let body_face = face(&fs, "EB Garamond", Weight::NORMAL, Style::Normal);
        assert_ne!(
            crimson, body_face,
            "headings and body must not share a face"
        );

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
            run.glyphs.iter().all(|glyph| glyph.font_id != body_face),
            "heading glyphs fell back to EB Garamond"
        );

        // Body text keeps the EB Garamond face.
        let body = paragraph("Garamond");
        let l = layout_block(&mut fs, &body, &config());
        let run = l.buffer.layout_runs().next().expect("one shaped run");
        assert!(run.glyphs.iter().all(|glyph| glyph.font_id == body_face));
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
    fn link_span_colours_its_glyphs() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);

        let mut block = paragraph("a link here");
        block.spans = vec![Span {
            start: 2,
            end: 6,
            style: SpanStyle::Link {
                url: "http://x.com".into(),
            },
        }];
        let l = layout_block(&mut fs, &block, &config());
        let run = l.buffer.layout_runs().next().expect("one shaped run");

        let mut linked = 0;
        for glyph in run.glyphs.iter() {
            if glyph.start >= 2 && glyph.end <= 6 {
                linked += 1;
                assert_eq!(
                    glyph.color_opt,
                    Some(LINK_COLOR),
                    "link glyph did not carry the link colour"
                );
            } else {
                assert!(
                    glyph.color_opt.is_none(),
                    "plain glyph took the link colour"
                );
            }
        }
        assert!(linked > 0, "no glyphs shaped inside the link span");
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
            kind: BlockKind::ListItem {
                ordered: true,
                depth: 0,
            },
            text: "A short list item.".to_string(),
            spans: Vec::new(),
        };
        let l = layout_block(&mut fs, &block, &config());
        assert!(l.height > 0.0);
        assert_eq!(l.lines.len(), 1);
    }

    #[test]
    fn link_ranges_find_urls_and_trim_punctuation() {
        assert_eq!(
            link_ranges("see https://example.com for more"),
            vec![(4, 23)]
        );
        assert_eq!(link_ranges("go to http://a.b/c now"), vec![(6, 18)]);
        assert_eq!(link_ranges("visit www.example.org today"), vec![(6, 21)]);
        // A trailing sentence period is not part of the link.
        assert_eq!(link_ranges("read https://x.y."), vec![(5, 16)]);
        // Two links in one line.
        assert_eq!(
            link_ranges("a https://a.b and https://c.d"),
            vec![(2, 13), (18, 29)]
        );
        assert!(link_ranges("plain prose").is_empty());
    }

    #[test]
    fn list_items_indent_but_paragraphs_do_not() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let config = config();
        let list = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::ListItem {
                ordered: false,
                depth: 0,
            },
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

    #[test]
    fn block_quotes_indent_from_the_measure() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let config = config();
        let quote = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::BlockQuote,
            text: "To be, or not to be.".to_string(),
            spans: Vec::new(),
        };
        let layout = layout_block(&mut fs, &quote, &config);
        // Indented from the measure, but with no marker to fill the gutter:
        // the whole column shifts right and wraps in the remaining width.
        assert_eq!(layout.left_indent, config.body_size * 1.5);
        assert!(layout.height > 0.0, "a non-empty quote shapes a line");
        assert_eq!(layout.lines.len(), 1);
    }

    /// The pen position of the glyph starting at byte `offset`.
    fn pen_at(layout: &BlockLayout, offset: usize) -> f32 {
        layout
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .find(|glyph| glyph.start == offset)
            .expect("a glyph at that offset")
            .x
    }

    #[test]
    fn a_footnote_mark_makes_room_before_the_following_text() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let config = LayoutConfig::with_body_size(16.0);
        let block = paragraph_block("A claim stands.");
        let plain = layout_block_with_align(&mut fs, &block, &config, Align::Left);
        let marked = layout_block_marked_with_align(&mut fs, &block, &config, Align::Left, &[7]);
        let room = FOOTNOTE_MARK_EM * config.body_size;
        assert!(room > 0.0, "the mark needs room");
        assert!(
            (pen_at(&marked, 2) - pen_at(&plain, 2)).abs() < 0.01,
            "before is untouched"
        );
        assert!(
            (pen_at(&marked, 7) - pen_at(&plain, 7) - room).abs() < 0.01,
            "the space after `claim` moved by {} not {room}",
            pen_at(&marked, 7) - pen_at(&plain, 7)
        );
    }

    #[test]
    fn a_footnote_mark_leaves_the_text_and_its_offsets_alone() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let config = LayoutConfig::with_body_size(16.0);
        let block = paragraph_block("Café claim stands.");
        let marked = layout_block_marked(&mut fs, &block, &config, &[11]);
        let plain = layout_block(&mut fs, &block, &config);
        let offsets = |layout: &BlockLayout| -> Vec<(usize, usize)> {
            layout
                .buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter())
                .map(|glyph| (glyph.start, glyph.end))
                .collect()
        };
        assert_eq!(offsets(&marked), offsets(&plain));
        assert_eq!(marked.lines, plain.lines);
    }

    #[test]
    fn a_mark_out_of_range_or_inside_a_character_is_ignored() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let config = LayoutConfig::with_body_size(16.0);
        let block = paragraph_block("Café");
        // Byte 4 is inside the two-byte é; 0 has no glyph before it.
        let _ = layout_block_marked(&mut fs, &block, &config, &[0, 4, 99]);
    }

    fn paragraph_block(text: &str) -> Block {
        Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    #[test]
    fn every_heading_level_is_set_in_bold_crimson() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let config = LayoutConfig::with_body_size(16.0);
        let bold = face(&fs, "Crimson Pro", Weight::BOLD, Style::Normal);
        let regular = face(&fs, "Crimson Pro", Weight::NORMAL, Style::Normal);
        assert_ne!(bold, regular, "the test needs both faces loaded");
        for level in 1..=6 {
            let block = Block {
                id: Uuid::new_v4(),
                kind: BlockKind::Heading { level },
                text: "Heading".to_string(),
                spans: Vec::new(),
            };
            let layout = layout_block(&mut fs, &block, &config);
            let run = layout.buffer.layout_runs().next().expect("a shaped line");
            for glyph in run.glyphs.iter() {
                assert_eq!(glyph.font_id, bold, "level {level}");
                assert_eq!(glyph.font_weight, Weight::BOLD, "level {level}");
            }
        }
    }

    #[test]
    fn heading_sizes_step_down_by_level() {
        let config = LayoutConfig::with_body_size(16.0);
        let close = |a: f32, b: f32| (a - b).abs() < 0.01;
        assert!(close(heading_size_for(&config, 1), 28.0));
        assert!(close(heading_size_for(&config, 2), 22.4));
        assert!(close(heading_size_for(&config, 3), 17.92));
        assert!(
            close(heading_size_for(&config, 4), 16.0),
            "level 4 is body size"
        );
        assert!(close(heading_size_for(&config, 6), 16.0));
        assert!(
            close(heading_size_for(&config, 0), 28.0),
            "a bad level is not tiny"
        );
    }

    #[test]
    fn a_heading_is_shaped_at_its_levels_size() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let config = LayoutConfig::with_body_size(16.0);
        let mut heights = Vec::new();
        for level in 1..=3 {
            let block = Block {
                id: Uuid::new_v4(),
                kind: BlockKind::Heading { level },
                text: "Heading".to_string(),
                spans: Vec::new(),
            };
            let layout = layout_block(&mut fs, &block, &config);
            let run = layout.buffer.layout_runs().next().expect("a shaped line");
            assert_eq!(
                run.glyphs[0].font_size,
                heading_size_for(&config, level),
                "level {level}"
            );
            heights.push(layout.height);
        }
        assert!(
            heights[0] > heights[1] && heights[1] > heights[2],
            "a smaller heading takes less room: {heights:?}"
        );
    }

    #[test]
    fn with_body_size_scales_every_font_derived_size() {
        let config = LayoutConfig::with_body_size(20.0);
        assert_eq!(config.body_size, 20.0);
        assert_eq!(config.body_line_height, 30.0); // 1.5×
        assert_eq!(config.heading_size, 35.0); // 1.75×
        assert_eq!(config.margin_size, 15.0); // 0.75×
        assert_eq!(config.list_indent, 35.0); // 1.75×
                                              // Page geometry is independent of the type size.
        assert_eq!(config.body_width, 640.0);
        assert_eq!(config.margin_width, 180.0);
        assert_eq!(LayoutConfig::default().body_size, DEFAULT_BODY_SIZE);
    }

    fn kind_block(kind: BlockKind, text: &str) -> Block {
        Block {
            id: Uuid::new_v4(),
            kind,
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    #[test]
    fn code_is_set_in_the_monospaced_face() {
        let mut fs = FontSystem::new();
        fs.db_mut()
            .load_font_file(font_path("IBMPlexMono-Regular.ttf"))
            .unwrap();
        load_fonts(&mut fs);
        let mono = face(&fs, "IBM Plex Mono", Weight::NORMAL, Style::Normal);
        let config = LayoutConfig::with_body_size(16.0);
        let layout = layout_block(
            &mut fs,
            &kind_block(BlockKind::Code { language: None }, "fn main()"),
            &config,
        );
        let run = layout.buffer.layout_runs().next().unwrap();
        assert!(run.glyphs.iter().all(|glyph| glyph.font_id == mono));
        assert!(run.glyphs[0].font_size < config.body_size);
    }

    #[test]
    fn prose_is_upright_body_type_and_not_justified() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let body = face(&fs, "EB Garamond", Weight::NORMAL, Style::Normal);
        let config = LayoutConfig::with_body_size(16.0);
        let text = "one two three four five six seven eight nine ten eleven twelve thirteen \
fourteen fifteen sixteen seventeen eighteen nineteen twenty twenty-one twenty-two twenty-three \
twenty-four twenty-five twenty-six twenty-seven";
        let prose = layout_block(&mut fs, &kind_block(BlockKind::Prose, text), &config);
        let run = prose.buffer.layout_runs().next().unwrap();
        assert!(run.glyphs.iter().all(|glyph| glyph.font_id == body));
        assert!(prose.left_indent > 0.0, "it is set in from the measure");
        assert!(
            run.line_w < config.body_width - prose.left_indent - 1.0 || prose.lines.len() == 1,
            "the first line should not be stretched to the measure"
        );
    }

    #[test]
    fn a_bare_url_in_code_is_not_coloured_as_a_link() {
        let mut fs = FontSystem::new();
        fs.db_mut()
            .load_font_file(font_path("IBMPlexMono-Regular.ttf"))
            .unwrap();
        load_fonts(&mut fs);
        let layout = layout_block(
            &mut fs,
            &kind_block(
                BlockKind::Code { language: None },
                "see https://example.com now",
            ),
            &LayoutConfig::with_body_size(16.0),
        );
        let run = layout.buffer.layout_runs().next().unwrap();
        assert!(run.glyphs.iter().all(|glyph| glyph.color_opt.is_none()));
    }

    #[test]
    fn a_document_link_is_coloured_as_a_link() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut block = kind_block(BlockKind::Paragraph, "See it");
        block.spans = vec![Span {
            start: 4,
            end: 6,
            style: SpanStyle::DocumentLink {
                target: "a.kame".to_string(),
            },
        }];
        let layout = layout_block(&mut fs, &block, &LayoutConfig::with_body_size(16.0));
        let run = layout.buffer.layout_runs().next().unwrap();
        let last = run.glyphs.last().unwrap();
        assert_eq!(last.color_opt, Some(LINK_COLOR));
        assert_eq!(run.glyphs[0].color_opt, None);
    }

    /// The ink of a line: its glyphs without the spaces at its end.
    fn visible<'a>(block: &Block, line: &'a LayoutRun) -> Vec<&'a cosmic_text::LayoutGlyph> {
        let base = line_starts(&block.text)[line.line_i];
        let mut glyphs: Vec<_> = line.glyphs.iter().collect();
        while glyphs
            .last()
            .is_some_and(|g| block.text[base + g.start..base + g.end].trim().is_empty())
        {
            glyphs.pop();
        }
        glyphs
    }

    fn hanging_layout(text: &str, kind: BlockKind, width: f32) -> (Block, BlockLayout, f32) {
        let mut fs = FontSystem::new();
        fs.db_mut()
            .load_font_file(font_path("IBMPlexMono-Regular.ttf"))
            .unwrap();
        load_fonts(&mut fs);
        let mut config = LayoutConfig::with_body_size(16.0);
        config.body_width = width;
        let block = kind_block(kind, text);
        let layout = layout_block_marked(&mut fs, &block, &config, &[]);
        (block, layout, width)
    }

    const WRAPPING: &str = "Typography, is the craft, of endowing human, language with, a durable \
visual form, and thus, with an independent, existence, whose heartwood, is calligraphy, the \
dance, on a page, of pen, eye, and hand, and whose, sapwood is, the rest.";

    #[test]
    fn a_comma_at_the_end_of_a_justified_line_hangs_past_the_measure() {
        let (block, layout, width) = hanging_layout(WRAPPING, BlockKind::Paragraph, 240.0);
        let mut hung = 0;
        for line in &layout.runs[..layout.runs.len() - 1] {
            let glyphs = visible(&block, line);
            let last = glyphs[glyphs.len() - 1];
            let end = last.x + last.w;
            if &block.text[last.start..last.end] == "," {
                assert!(end > width + 0.5, "a comma ending at {end} of {width}");
                hung += 1;
            } else {
                assert!(
                    (end - width).abs() < 0.5,
                    "a word ending at {end} of {width}"
                );
            }
        }
        assert!(hung >= 2, "expected lines ending in commas, got {hung}");
    }

    #[test]
    fn what_comes_before_a_hanging_comma_nearly_reaches_the_measure() {
        let (block, layout, width) = hanging_layout(WRAPPING, BlockKind::Paragraph, 240.0);
        for line in &layout.runs[..layout.runs.len() - 1] {
            let glyphs = visible(&block, line);
            let last = glyphs[glyphs.len() - 1];
            if &block.text[last.start..last.end] == "," {
                let word_end = last.x;
                assert!(word_end < width + 0.5 && word_end > width - last.w);
            }
        }
    }

    #[test]
    fn an_opening_quote_hangs_past_the_left_of_the_measure() {
        let (_, layout, _) = hanging_layout(
            "\u{201c}Words,\u{201d} they said.",
            BlockKind::Paragraph,
            400.0,
        );
        let first = &layout.runs[0].glyphs[0];
        assert!(first.x < -0.5, "the quote starts at {}", first.x);
        assert!(layout.runs[0].glyphs[1].x < first.w, "the word follows it");
    }

    #[test]
    fn a_quote_starting_a_heading_or_a_list_item_hangs_too() {
        for kind in [
            BlockKind::Heading { level: 2 },
            BlockKind::ListItem {
                ordered: false,
                depth: 0,
            },
        ] {
            let (_, layout, _) = hanging_layout("\u{201c}Words\u{201d}", kind, 400.0);
            assert!(layout.runs[0].glyphs[0].x < -0.5);
        }
    }

    #[test]
    fn a_comma_at_a_ragged_edge_is_left_alone() {
        let (block, layout, width) = hanging_layout(WRAPPING, BlockKind::BlockQuote, 240.0);
        for line in &layout.runs {
            let glyphs = visible(&block, line);
            let last = glyphs[glyphs.len() - 1];
            assert!(last.x + last.w <= width + 0.5);
        }
    }

    #[test]
    fn the_last_line_of_a_paragraph_is_not_stretched() {
        let (block, layout, width) = hanging_layout(WRAPPING, BlockKind::Paragraph, 240.0);
        let line = layout.runs.last().unwrap();
        let glyphs = visible(&block, line);
        let last = glyphs[glyphs.len() - 1];
        assert!(
            last.x + last.w < width - 1.0,
            "the rest. ends at {}",
            last.x + last.w
        );
    }

    #[test]
    fn exact_text_does_not_hang() {
        for kind in [BlockKind::Code { language: None }, BlockKind::Prose] {
            let (_, layout, _) = hanging_layout("\u{201c}Words,\u{201d}", kind, 400.0);
            assert_eq!(layout.runs[0].glyphs[0].x, 0.0);
        }
    }

    #[test]
    fn a_line_with_nothing_to_hang_is_exactly_as_shaped() {
        let (_, layout, _) = hanging_layout("Plain words only", BlockKind::Paragraph, 400.0);
        let shaped = layout.buffer.layout_runs().next().unwrap();
        for (drawn, shaped) in layout.runs[0].glyphs.iter().zip(shaped.glyphs) {
            assert_eq!(drawn.x, shaped.x);
            assert_eq!(drawn.w, shaped.w);
        }
    }

    #[test]
    fn hanging_leaves_the_text_offsets_and_the_order_alone() {
        let (_, layout, _) = hanging_layout(WRAPPING, BlockKind::Paragraph, 240.0);
        let shaped: Vec<_> = layout.buffer.layout_runs().collect();
        assert_eq!(shaped.len(), layout.runs.len());
        for (drawn, shaped) in layout.runs.iter().zip(shaped) {
            assert_eq!(drawn.glyphs.len(), shaped.glyphs.len());
            for (a, b) in drawn.glyphs.iter().zip(shaped.glyphs) {
                assert_eq!((a.start, a.end), (b.start, b.end));
            }
            for pair in drawn.glyphs.windows(2) {
                assert!(pair[1].x > pair[0].x, "glyphs out of order");
            }
        }
    }

    fn with_code_span(kind: BlockKind, text: &str, start: usize, end: usize) -> Block {
        let mut block = kind_block(kind, text);
        block.spans = vec![Span {
            start,
            end,
            style: SpanStyle::Code,
        }];
        block
    }

    #[test]
    fn inline_code_is_smaller_and_monospaced_without_moving_the_lines() {
        let mut fs = FontSystem::new();
        fs.db_mut()
            .load_font_file(font_path("IBMPlexMono-Regular.ttf"))
            .unwrap();
        load_fonts(&mut fs);
        let mono = face(&fs, "IBM Plex Mono", Weight::NORMAL, Style::Normal);
        let config = LayoutConfig::with_body_size(16.0);
        let plain = layout_block(
            &mut fs,
            &kind_block(BlockKind::Paragraph, "plain code here"),
            &config,
        );
        let coded = layout_block(
            &mut fs,
            &with_code_span(BlockKind::Paragraph, "plain code here", 6, 10),
            &config,
        );
        let run = coded.buffer.layout_runs().next().unwrap();
        let code: Vec<_> = run
            .glyphs
            .iter()
            .filter(|g| g.start >= 6 && g.end <= 10)
            .collect();
        assert!(code.iter().all(|g| g.font_id == mono));
        assert!(code.iter().all(|g| g.font_size < config.body_size));
        assert_eq!(coded.height, plain.height, "the line's height moved");
        assert_eq!(
            coded.buffer.layout_runs().next().unwrap().line_y,
            plain.buffer.layout_runs().next().unwrap().line_y,
            "the baseline moved"
        );
    }

    #[test]
    fn a_url_in_inline_code_is_not_coloured_as_a_link() {
        let block = with_code_span(BlockKind::Paragraph, "curl https://a.b", 0, 16);
        assert!(bare_link_ranges(&block).is_empty());
        let outside = with_code_span(BlockKind::Paragraph, "a b https://a.b", 0, 1);
        assert_eq!(bare_link_ranges(&outside).len(), 1);
        let code_block = kind_block(BlockKind::Code { language: None }, "https://a.b");
        assert!(bare_link_ranges(&code_block).is_empty());
    }

    #[test]
    fn punctuation_in_inline_code_does_not_hang() {
        let mut fs = FontSystem::new();
        fs.db_mut()
            .load_font_file(font_path("IBMPlexMono-Regular.ttf"))
            .unwrap();
        load_fonts(&mut fs);
        let config = LayoutConfig::with_body_size(16.0);
        let layout = layout_block_marked(
            &mut fs,
            &with_code_span(BlockKind::Paragraph, "\"a\" then", 0, 3),
            &config,
            &[],
        );
        assert_eq!(layout.runs[0].glyphs[0].x, 0.0);
    }

    #[test]
    fn inline_code_in_an_italic_quote_stays_upright() {
        let mut fs = FontSystem::new();
        fs.db_mut()
            .load_font_file(font_path("IBMPlexMono-Regular.ttf"))
            .unwrap();
        load_fonts(&mut fs);
        let mono = face(&fs, "IBM Plex Mono", Weight::NORMAL, Style::Normal);
        let layout = layout_block(
            &mut fs,
            &with_code_span(BlockKind::BlockQuote, "a code b", 2, 6),
            &LayoutConfig::with_body_size(16.0),
        );
        let run = layout.buffer.layout_runs().next().unwrap();
        let code = run.glyphs.iter().find(|g| g.start == 2).unwrap();
        assert_eq!(code.font_id, mono);
    }

    #[test]
    fn a_nested_item_is_set_in_a_level_at_a_time() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let config = LayoutConfig::with_body_size(16.0);
        let indent = |fs: &mut FontSystem, depth| {
            let kind = BlockKind::ListItem {
                ordered: false,
                depth,
            };
            layout_block(fs, &kind_block(kind, "a"), &config).left_indent
        };
        assert_eq!(indent(&mut fs, 0), config.list_indent);
        assert_eq!(indent(&mut fs, 1), config.list_indent * 2.0);
        assert_eq!(indent(&mut fs, 2), config.list_indent * 3.0);
    }

    #[test]
    fn a_nested_item_wraps_within_the_measure() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut config = LayoutConfig::with_body_size(16.0);
        config.body_width = 240.0;
        let kind = BlockKind::ListItem {
            ordered: false,
            depth: 2,
        };
        let layout = layout_block(
            &mut fs,
            &kind_block(
                kind,
                "several words that have to wrap inside what is left of the measure",
            ),
            &config,
        );
        for run in &layout.runs {
            let last = run.glyphs.last().unwrap();
            assert!(layout.left_indent + last.x + last.w <= 240.5);
        }
    }

    #[test]
    fn a_stored_depth_past_the_limit_is_set_at_the_limit() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let config = LayoutConfig::with_body_size(16.0);
        let kind = BlockKind::ListItem {
            ordered: false,
            depth: u8::MAX,
        };
        let layout = layout_block(&mut fs, &kind_block(kind, "a"), &config);
        assert_eq!(
            layout.left_indent,
            config.list_indent * f32::from(crate::model::MAX_LIST_DEPTH + 1)
        );
    }
}
