//! egui presentation: fonts, glyph textures, caret geometry, and the keyboard
//! map from egui events to editor commands.
//!
//! Nothing here owns a window. The renderer works off a [`FontSystem`], a
//! [`LayoutConfig`] and an egui [`Context`], so the texture cache and the pure
//! geometry/command helpers can be exercised headlessly in tests.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use cosmic_text::FontSystem;
use egui::{
    ColorImage, Context, Event, FontData, FontFamily, Key, Modifiers, Pos2, Rect, TextureHandle,
    TextureOptions, Vec2,
};
use image::RgbaImage;

use crate::layout::{layout_block_marked, layout_block_with_align, BlockLayout, LayoutConfig};
use crate::model::{AnnotationId, AnnotationKind, Block, BlockId, BlockKind};
use crate::render::GlyphAtlas;

/// Every face the layout can ask for, compiled into the binary.
///
/// Loading these into the runtime [`FontSystem`] is not optional: headings
/// resolve `Crimson Pro` and body text `EB Garamond`, and a missing heading
/// face silently falls back to the body family. Embedding the files (rather
/// than reading `fonts/` at startup) keeps a binary copied elsewhere working
/// while still sourcing exactly the files in `fonts/`.
const FONTS: &[&[u8]] = &[
    include_bytes!("../../fonts/EBGaramond-Regular.otf"),
    include_bytes!("../../fonts/EBGaramond-Bold.otf"),
    include_bytes!("../../fonts/EBGaramond-Italic.otf"),
    include_bytes!("../../fonts/EBGaramond-SemiBold.otf"),
    include_bytes!("../../fonts/CrimsonPro-Regular.ttf"),
    include_bytes!("../../fonts/CrimsonPro-Bold.ttf"),
    include_bytes!("../../fonts/CrimsonPro-Italic.ttf"),
];

/// Loads the bundled faces into egui so the immediate-mode text — list markers,
/// footnote superscripts, and the UI chrome — renders in the same families as
/// the cosmic-text page. The proportional family is re-pointed at EB Garamond,
/// so `FontId::proportional` picks it up everywhere without call-site changes;
/// egui's own faces stay as fallbacks for glyphs the serif lacks.
pub fn install_fonts(ctx: &Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut face = |name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(FontData::from_static(bytes)),
        );
    };
    face(
        "EB Garamond",
        include_bytes!("../../fonts/EBGaramond-Regular.otf"),
    );
    face(
        "EB Garamond Bold",
        include_bytes!("../../fonts/EBGaramond-Bold.otf"),
    );
    face(
        "EB Garamond Italic",
        include_bytes!("../../fonts/EBGaramond-Italic.otf"),
    );
    face(
        "Crimson Pro",
        include_bytes!("../../fonts/CrimsonPro-Regular.ttf"),
    );
    face(
        "Crimson Pro Bold",
        include_bytes!("../../fonts/CrimsonPro-Bold.ttf"),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "EB Garamond".to_owned());
    ctx.set_fonts(fonts);
}

/// How many block rasterizations may run before the glyph image cache is
/// dropped. The atlas's `HashMap` never evicts on its own, so without this the
/// cache grows for the lifetime of the process.
const RASTERIZES_PER_CACHE_CLEAR: u32 = 512;

/// A block's uploaded glyph texture plus the ink offset returned by
/// [`GlyphAtlas::rasterize`].
///
/// The image is cropped to the glyph ink, so it must be drawn at
/// `block_origin + (left, top)` — never at the block origin itself. The image
/// and its offset are in physical pixels, rasterized at `scale` pixels per point.
pub struct BlockTexture {
    pub texture: TextureHandle,
    pub left: i32,
    pub top: i32,
    pub scale: f32,
}

impl BlockTexture {
    /// Screen-space quad, in points, for a block whose layout origin (the
    /// top-left of its layout box, Y down) is `origin`.
    pub fn rect(&self, origin: Pos2) -> Rect {
        pixel_aligned_quad(
            origin,
            (self.left, self.top),
            self.texture.size(),
            self.scale,
        )
    }
}

/// The quad, in points, that draws a `size`-pixel texture exactly 1:1 with the
/// screen at `scale` pixels per point, whose ink starts `(left, top)` pixels from
/// the layout `origin`.
///
/// The origin is snapped to a whole physical pixel. Scroll offsets and the
/// centred page put it anywhere, and at a fractional position a nearest-sampled
/// quad drops or doubles rows and columns even at the right size.
fn pixel_aligned_quad(origin: Pos2, (left, top): (i32, i32), size: [usize; 2], scale: f32) -> Rect {
    let snap = |at: f32, offset: i32| ((at * scale).round() + offset as f32) / scale;
    Rect::from_min_size(
        Pos2::new(snap(origin.x, left), snap(origin.y, top)),
        Vec2::new(size[0] as f32 / scale, size[1] as f32 / scale),
    )
}

/// Per-block texture cache, rebuilt only when a block's content changes.
///
/// The key is a hash of the block kind, text, and spans: any edit invalidates
/// just that block. Textures for blocks that left the document are dropped
/// (and their GPU resources released) on the next [`TextureCache::sync`].
pub struct TextureCache {
    entries: HashMap<BlockId, CachedTexture>,
}

struct CachedTexture {
    fingerprint: u64,
    block: BlockTexture,
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TextureCache {
    pub fn new() -> TextureCache {
        TextureCache {
            entries: HashMap::new(),
        }
    }

    /// Rasterizes and uploads every block whose content or display scale
    /// changed, and drops textures of blocks no longer present.
    ///
    /// `blocks` and `layouts` are parallel, as produced by
    /// [`GlyphRenderer::layout`]. Uploads use
    /// [`ColorImage::from_rgba_unmultiplied`] because the rasterizer emits
    /// straight (unpremultiplied) alpha, and [`TextureOptions::NEAREST`] because
    /// [`BlockTexture::rect`] maps each texel onto exactly one screen pixel.
    ///
    /// The scale is part of what makes a texture current: a window's first frame
    /// runs before the compositor reports its scale, and a window can move
    /// between monitors.
    pub fn sync(
        &mut self,
        ctx: &Context,
        renderer: &mut GlyphRenderer,
        blocks: &[Block],
        layouts: &[&BlockLayout],
        ink: [u8; 3],
        background: [u8; 3],
        highlights: &HashMap<BlockId, Vec<(usize, usize, [u8; 3])>>,
    ) {
        let scale = ctx.pixels_per_point();
        let mut live = HashSet::with_capacity(blocks.len());
        for (block, layout) in blocks.iter().zip(layouts.iter().copied()) {
            live.insert(block.id);
            let block_highlights = highlights.get(&block.id).map(Vec::as_slice).unwrap_or(&[]);
            let fingerprint = block_fingerprint(block, block_highlights);
            let up_to_date = self.entries.get(&block.id).is_some_and(|cached| {
                cached.fingerprint == fingerprint && cached.block.scale == scale
            });
            if up_to_date {
                continue;
            }

            let (image, left, top) =
                renderer.rasterize(layout, ink, background, block_highlights, scale);
            let size = [image.width() as usize, image.height() as usize];
            let color_image = ColorImage::from_rgba_unmultiplied(size, image.as_raw());
            let texture = ctx.load_texture(
                format!("kame-block-{}", block.id),
                color_image,
                TextureOptions::NEAREST,
            );
            self.entries.insert(
                block.id,
                CachedTexture {
                    fingerprint,
                    block: BlockTexture {
                        texture,
                        left,
                        top,
                        scale,
                    },
                },
            );
        }
        self.entries.retain(|id, _| live.contains(id));
    }

    pub fn get(&self, block_id: BlockId) -> Option<&BlockTexture> {
        self.entries.get(&block_id).map(|cached| &cached.block)
    }

    /// Drops every cached texture, so the next `sync` re-rasterizes against the
    /// current metrics. Used when the measure changes with the window.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Per-block shaping cache, rebuilt only when a block's content changes.
///
/// Keyed by the same [`block_fingerprint`] as the texture cache, so a frame
/// with no edits shapes nothing and an edit re-shapes only the block it
/// touched. Layouts of blocks that left the document are dropped.
pub struct LayoutCache {
    entries: HashMap<BlockId, CachedLayout>,
}

struct CachedLayout {
    fingerprint: u64,
    layout: BlockLayout,
}

impl Default for LayoutCache {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutCache {
    pub fn new() -> LayoutCache {
        LayoutCache {
            entries: HashMap::new(),
        }
    }

    /// Drops every cached layout, so the next `sync` shapes against the current
    /// metrics. Used when the measure changes with the window.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub fn sync(&mut self, renderer: &mut GlyphRenderer, blocks: &[Block]) -> Vec<&BlockLayout> {
        self.sync_marked(renderer, blocks, &HashMap::new())
    }

    /// Returns one layout per block, in document order, shaping only blocks
    /// whose fingerprint changed since the last call. `marks` holds each block's
    /// footnote offsets, which a block needs room for.
    pub fn sync_marked(
        &mut self,
        renderer: &mut GlyphRenderer,
        blocks: &[Block],
        marks: &HashMap<BlockId, Vec<usize>>,
    ) -> Vec<&BlockLayout> {
        let mut live = HashSet::with_capacity(blocks.len());
        for block in blocks {
            live.insert(block.id);
            let block_marks = marks.get(&block.id).map(Vec::as_slice).unwrap_or(&[]);
            let as_ranges: Vec<_> = block_marks
                .iter()
                .map(|mark| (*mark, *mark, [0; 3]))
                .collect();
            let fingerprint = block_fingerprint(block, &as_ranges);
            let up_to_date = self
                .entries
                .get(&block.id)
                .is_some_and(|cached| cached.fingerprint == fingerprint);
            if up_to_date {
                continue;
            }
            let layout = renderer.layout_marked(block, block_marks);
            self.entries.insert(
                block.id,
                CachedLayout {
                    fingerprint,
                    layout,
                },
            );
        }
        self.entries.retain(|id, _| live.contains(id));
        blocks
            .iter()
            .map(|block| &self.entries[&block.id].layout)
            .collect()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Per-annotation texture cache for margin and footnote text.
///
/// Annotation text is not a document block: it is shaped in its own column
/// (the margin width, or the body measure for a footnote) at a smaller size,
/// so it cannot share [`TextureCache`]. The key is the annotation id and the
/// fingerprint covers the rendered string plus the column and size, so a
/// renumbered footnote re-rasterizes while an idle frame uploads nothing.
pub struct AnnotationTextureCache {
    entries: HashMap<AnnotationId, AnnotationEntry>,
}

/// One cached annotation texture plus the shaped height of its text.
///
/// The height is the layout box's height (every wrapped line included), not
/// the ink rectangle: margin stacking clears whole lines, so a body that wraps
/// to six lines must reserve six lines even where the glyphs stop early.
struct AnnotationEntry {
    fingerprint: u64,
    block: BlockTexture,
    height: f32,
}

impl Default for AnnotationTextureCache {
    fn default() -> Self {
        Self::new()
    }
}

impl AnnotationTextureCache {
    pub fn new() -> AnnotationTextureCache {
        AnnotationTextureCache {
            entries: HashMap::new(),
        }
    }

    /// Shapes, rasterizes and uploads `text` for `annotation_id` when its
    /// content or metrics changed. Empty text is skipped: there is nothing to
    /// paint, and an empty raster has no ink rectangle to place.
    pub fn sync(
        &mut self,
        ctx: &Context,
        renderer: &mut GlyphRenderer,
        annotation_id: AnnotationId,
        text: &str,
        width: f32,
        size: f32,
        color: [u8; 3],
        background: [u8; 3],
    ) {
        if text.trim().is_empty() {
            self.entries.remove(&annotation_id);
            return;
        }
        let scale = ctx.pixels_per_point();
        let fingerprint = annotation_fingerprint(text, width, size, scale);
        let up_to_date = self
            .entries
            .get(&annotation_id)
            .is_some_and(|cached| cached.fingerprint == fingerprint);
        if up_to_date {
            return;
        }

        let block = Block {
            id: annotation_id,
            kind: crate::model::BlockKind::Paragraph,
            text: text.to_string(),
            spans: Vec::new(),
        };
        let layout = renderer.layout_in_column(&block, width, size);
        let body_height = layout.height;
        let (image, left, top) = renderer.rasterize(&layout, color, background, &[], scale);
        let size_px = [image.width() as usize, image.height() as usize];
        if size_px[0] == 0 || size_px[1] == 0 {
            self.entries.remove(&annotation_id);
            return;
        }
        let color_image = ColorImage::from_rgba_unmultiplied(size_px, image.as_raw());
        let texture = ctx.load_texture(
            format!("kame-annotation-{annotation_id}"),
            color_image,
            TextureOptions::NEAREST,
        );
        self.entries.insert(
            annotation_id,
            AnnotationEntry {
                fingerprint,
                block: BlockTexture {
                    texture,
                    left,
                    top,
                    scale,
                },
                height: body_height,
            },
        );
    }

    pub fn get(&self, annotation_id: AnnotationId) -> Option<&BlockTexture> {
        self.entries.get(&annotation_id).map(|cached| &cached.block)
    }

    /// Shaped height of the annotation body, in layout pixels. Callers use it
    /// to stack margin notes by the space they actually occupy.
    pub fn body_height(&self, annotation_id: AnnotationId) -> Option<f32> {
        self.entries.get(&annotation_id).map(|cached| cached.height)
    }

    /// Drops textures for annotations that left the document.
    pub fn retain(&mut self, live: &HashSet<AnnotationId>) {
        self.entries.retain(|id, _| live.contains(id));
    }

    /// Drops every cached texture, so the next `sync` re-rasterizes against
    /// the current palette.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Content hash for annotation text plus the metrics it was shaped at and the
/// display scale it was rasterized for.
fn annotation_fingerprint(text: &str, width: f32, size: f32, scale: f32) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    width.to_bits().hash(&mut hasher);
    size.to_bits().hash(&mut hasher);
    scale.to_bits().hash(&mut hasher);
    hasher.finish()
}

/// Content hash used to decide whether a block's texture or layout is stale.
/// `highlights` are the note washes baked into this block's text, so moving a
/// note re-rasterizes the prose it marks.
fn block_fingerprint(block: &Block, highlights: &[(usize, usize, [u8; 3])]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", block.kind).hash(&mut hasher);
    block.text.hash(&mut hasher);
    for span in &block.spans {
        span.start.hash(&mut hasher);
        span.end.hash(&mut hasher);
        format!("{:?}", span.style).hash(&mut hasher);
    }
    for (start, end, color) in highlights {
        start.hash(&mut hasher);
        end.hash(&mut hasher);
        color.hash(&mut hasher);
    }
    hasher.finish()
}

/// Owns the runtime [`FontSystem`] and the glyph atlas.
pub struct GlyphRenderer {
    font_system: FontSystem,
    atlas: GlyphAtlas,
    config: LayoutConfig,
    rasterizes_since_clear: u32,
    #[cfg(test)]
    layout_calls: u64,
}

impl GlyphRenderer {
    /// A renderer with every [`FONTS`] face registered.
    pub fn new(config: LayoutConfig) -> GlyphRenderer {
        let mut font_system = FontSystem::new();
        for bytes in FONTS {
            font_system.db_mut().load_font_data(bytes.to_vec());
        }
        GlyphRenderer {
            font_system,
            atlas: GlyphAtlas::new(),
            config,
            rasterizes_since_clear: 0,
            #[cfg(test)]
            layout_calls: 0,
        }
    }

    pub fn config(&self) -> LayoutConfig {
        self.config
    }

    /// Replaces the layout metrics. Returns whether anything changed.
    ///
    /// The measure follows the window, so the caller recomputes it every frame
    /// and calls this when it differs. Any change invalidates every shaped
    /// layout and glyph texture; callers must drop their caches or the page
    /// will keep drawing at the old width.
    pub fn set_config(&mut self, config: LayoutConfig) -> bool {
        if self.config == config {
            return false;
        }
        self.config = config;
        true
    }

    /// Shapes one block with the configured column width. Callers keep the
    /// returned layouts for the frame to map carets and clicks.
    #[cfg(test)]
    pub fn layout(&mut self, block: &Block) -> BlockLayout {
        self.layout_marked(block, &[])
    }

    /// [`Self::layout`] leaving room for a footnote's superscript at each of
    /// `marks`.
    pub fn layout_marked(&mut self, block: &Block, marks: &[usize]) -> BlockLayout {
        #[cfg(test)]
        {
            self.layout_calls += 1;
        }
        layout_block_marked(&mut self.font_system, block, &self.config, marks)
    }

    /// Shapes one block in a narrower column at a smaller body size, for
    /// annotation text (margin notes and footnote entries) that does not use
    /// the prose measure.
    ///
    /// The line height scales with `size`, keeping the leading proportional to
    /// the prose setting rather than to the absolute body line height. The text
    /// is left-aligned: justifying a narrow margin column produces rivers of
    /// whitespace (especially around short words like "a"), which reads as
    /// mangled even though every glyph is intact.
    pub fn layout_in_column(&mut self, block: &Block, width: f32, size: f32) -> BlockLayout {
        let mut config = self.config;
        config.body_width = width.max(24.0);
        config.body_size = size.max(6.0);
        config.body_line_height = (size * 1.4).max(size + 2.0);
        config.heading_size = config.body_size;
        layout_block_with_align(
            &mut self.font_system,
            block,
            &config,
            cosmic_text::Align::Left,
        )
    }

    #[cfg(test)]
    pub fn layout_calls(&self) -> u64 {
        self.layout_calls
    }

    /// Paints `layout` into an ink-cropped RGBA image and returns it with its
    /// block-relative `(left, top)` offset, in physical pixels. `ink` is the text
    /// colour, `background` the paper it is baked against for subpixel
    /// antialiasing, `highlights` the note washes whose ranges get a different
    /// background, and `scale` the display's physical pixels per point.
    pub fn rasterize(
        &mut self,
        layout: &BlockLayout,
        ink: [u8; 3],
        background: [u8; 3],
        highlights: &[(usize, usize, [u8; 3])],
        scale: f32,
    ) -> (RgbaImage, i32, i32) {
        self.rasterizes_since_clear += 1;
        if self.rasterizes_since_clear >= RASTERIZES_PER_CACHE_CLEAR {
            self.atlas.clear_cache();
            self.rasterizes_since_clear = 0;
        }
        self.atlas.rasterize(
            &mut self.font_system,
            layout,
            ink,
            background,
            highlights,
            scale,
        )
    }
}

// --- caret geometry --------------------------------------------------------

/// Block-relative pen X of byte `offset`.
///
/// A caret at a glyph's start sits on its pen position; a caret at the end of
/// a run sits after the last advance. Offsets that fall inside a multi-byte
/// cluster resolve to the cluster's start. Empty layouts report `0.0`.
pub fn caret_x(layout: &BlockLayout, offset: usize) -> f32 {
    for run in layout.buffer.layout_runs() {
        let (Some(first), Some(last)) = (run.glyphs.first(), run.glyphs.last()) else {
            continue;
        };
        // Glyph offsets count from the start of their own line.
        let base = layout.line_start(run.line_i);
        if offset < base + first.start || offset > base + last.end {
            continue;
        }
        if offset == base + last.end {
            return last.x + last.w;
        }
        for glyph in run.glyphs.iter() {
            if offset >= base + glyph.start && offset < base + glyph.end {
                return glyph.x;
            }
        }
        // Inside a cluster that has no exact start/end match (e.g. a ligature
        // boundary): fall back to the next glyph's pen, then the run end.
    }
    0.0
}

/// The visual line covering `offset` (block-relative), falling forward to the
/// next line and back to the last one at the end of the block.
pub fn caret_line(layout: &BlockLayout, offset: usize) -> Option<crate::layout::LayoutLine> {
    let inside = |line: &&crate::layout::LayoutLine| offset >= line.start && offset < line.end;
    // At a line's end only if the next line does not start there, as it does where a
    // line wraps. The end of a line before a break, and a blank line, are their own.
    let at_end = |index: usize, line: &crate::layout::LayoutLine| {
        line.end == offset
            && layout
                .lines
                .get(index + 1)
                .is_none_or(|next| next.start > offset)
    };
    layout
        .lines
        .iter()
        .find(inside)
        .or_else(|| {
            layout
                .lines
                .iter()
                .enumerate()
                .find(|(index, line)| at_end(*index, line))
                .map(|(_, line)| line)
        })
        .or_else(|| layout.lines.iter().find(|line| line.start > offset))
        .or_else(|| layout.lines.last())
        .copied()
}

/// The byte offset in a block closest to block-relative point (`x`, `y`).
///
/// The nearest visual line is chosen by vertical distance, then the glyph whose
/// advance the click falls in. The caret lands at that glyph's *start* — so a
/// click anywhere on a letter places the caret before it, rather than snapping
/// to the next letter over when the pointer is past the glyph's midpoint. An
/// empty layout (or a blank line) maps to 0, and a click past the last glyph of
/// a line maps to that line's end.
pub fn offset_at(layout: &BlockLayout, x: f32, y: f32, text_len: usize) -> usize {
    let mut best: Option<(f32, usize)> = None;
    for run in layout.buffer.layout_runs() {
        let centre = run.line_top + run.line_height * 0.5;
        let distance = (y - centre).abs();
        if best.is_some_and(|(best_distance, _)| best_distance <= distance) {
            continue;
        }

        // Glyph offsets count from the start of their own line, and a blank line has
        // only that start.
        let base = layout.line_start(run.line_i);
        let Some(first) = run.glyphs.first() else {
            best = Some((distance, base));
            continue;
        };

        // The caret belongs to the glyph under the pointer: walk the run and
        // stop at the first glyph whose right edge is past `x`. That glyph's
        // start is the boundary the caret precedes. Falling off the end means
        // the click is past the line, so the caret goes to the line's end.
        let mut offset = base + first.start;
        for glyph in run.glyphs.iter() {
            if x < glyph.x + glyph.w {
                offset = base + glyph.start;
                break;
            }
            offset = base + glyph.end;
        }
        best = Some((distance, offset));
    }
    // The line after a trailing break exists only in `lines`.
    if let Some(last) = layout.lines.last().filter(|line| line.start == line.end) {
        let distance = (y - (last.y + last.height * 0.5)).abs();
        if best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, last.start));
        }
    }
    best.map_or(0, |(_, offset)| offset.min(text_len))
}

/// Previous character boundary before `offset`, or `None` at the start.
pub fn prev_boundary(text: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(text.len());
    text[..offset].char_indices().next_back().map(|(i, _)| i)
}

/// Next character boundary after `offset`, or `None` at the end.
pub fn next_boundary(text: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(text.len());
    text[offset..].chars().next().map(|c| offset + c.len_utf8())
}

/// A word character for cursor movement and selection: alphanumeric plus the
/// apostrophe forms, so "don't" moves as one word.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '’'
}

/// The byte index of the first character of the next word at or after `offset`,
/// or `None` when no word follows. A word is a maximal run of [`is_word_char`]
/// characters; the current run (word or not) is skipped first, so moving right
/// lands on the next word rather than the next character.
pub fn next_word(text: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(text.len());
    let chars: Vec<(usize, char)> = text[offset..]
        .char_indices()
        .map(|(i, c)| (offset + i, c))
        .collect();
    let n = chars.len();
    if n == 0 {
        return None;
    }
    // Skip the current run (word or non-word).
    let mut k = 0;
    let in_word = is_word_char(chars[0].1);
    while k < n && (is_word_char(chars[k].1) == in_word) {
        k += 1;
    }
    // Skip non-word characters to the next word's start.
    while k < n && !is_word_char(chars[k].1) {
        k += 1;
    }
    (k < n).then(|| chars[k].0)
}

/// The byte index of the first character of the previous word before `offset`,
/// or `None` when no word precedes it. A caret inside (or at the end of) a word
/// moves to that word's start; a caret after whitespace/punctuation skips them
/// and moves to the previous word's start.
pub fn prev_word(text: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(text.len());
    if offset == 0 {
        return None;
    }
    let chars: Vec<(usize, char)> = text[..offset].char_indices().collect();
    let n = chars.len();
    let mut k = n;
    if is_word_char(chars[n - 1].1) {
        // Inside (or at the end of) a word: walk left to its start.
        while k > 0 && is_word_char(chars[k - 1].1) {
            k -= 1;
        }
        return Some(chars[k].0);
    }
    // After whitespace/punctuation: skip them, then find the word before.
    while k > 0 && !is_word_char(chars[k - 1].1) {
        k -= 1;
    }
    if k == 0 {
        return None;
    }
    let mut start = k - 1;
    while start > 0 && is_word_char(chars[start - 1].1) {
        start -= 1;
    }
    Some(chars[start].0)
}

/// Block-relative rectangles covering the byte range `start..end`, one per
/// visual line it crosses.
///
/// Used for selection bands and annotation highlights. The rectangles are
/// line-height tall and `x1 > x0` is guaranteed: an empty range yields no
/// rectangles (a caret is drawn separately) and a reversed range is
/// normalized. Offsets beyond the layout's lines are ignored.
pub fn range_rects(layout: &BlockLayout, start: usize, end: usize) -> Vec<Rect> {
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if start == end {
        return Vec::new();
    }

    let mut rects = Vec::new();
    for line in &layout.lines {
        let lo = start.max(line.start);
        let hi = end.min(line.end);
        if lo >= hi {
            continue;
        }
        let x0 = caret_x(layout, lo);
        let x1 = caret_x(layout, hi);
        if x1 <= x0 {
            continue;
        }
        rects.push(Rect::from_min_max(
            Pos2::new(x0, line.y),
            Pos2::new(x1, line.y + line.height),
        ));
    }
    rects
}

/// The word around `offset`, as a byte range, for anchoring an annotation
/// placed with a bare caret.
///
/// A caret inside a word selects that word, a caret at its end selects the
/// word before it, and a caret at its start (or on punctuation and spaces)
/// selects the word after it. With no word either side the range is empty at
/// `offset`, which is still a valid zero-width anchor.
pub fn word_at(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '\'' || c == '’';

    let before = text[..offset].chars().next_back().is_some_and(is_word);
    if before {
        let start = text[..offset]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word(*c))
            .last()
            .map_or(offset, |(index, _)| index);
        let end = offset
            + text[offset..]
                .chars()
                .take_while(|c| is_word(*c))
                .map(char::len_utf8)
                .sum::<usize>();
        (start, end)
    } else {
        let forward = text[offset..]
            .chars()
            .take_while(|c| is_word(*c))
            .map(char::len_utf8)
            .sum::<usize>();
        if forward > 0 {
            (offset, offset + forward)
        } else {
            (offset, offset)
        }
    }
}

// --- input mapping ---------------------------------------------------------

/// What a keyboard/clipboard event asks the editor to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Insert(String),
    Paste(String),
    Backspace,
    Delete,
    SplitParagraph,
    Undo,
    Redo,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveLineStart,
    MoveLineEnd,
    /// Ctrl-arrow: move by whole word rather than by character.
    MoveWordLeft,
    MoveWordRight,
    SelectWordLeft,
    SelectWordRight,
    /// Shift-arrow selection: move the caret's edge while keeping the edge where
    /// the selection began.
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    SelectLineStart,
    SelectLineEnd,
    /// Select the caret's whole block (or the whole document when the block is
    /// already selected).
    SelectAll,
    /// Change the caret block's kind (heading level, paragraph, list item).
    SetBlockKind(BlockKind),
    /// Add an annotation at the selection or the word under the caret. A note
    /// starts as a bare highlight (empty body) and its comment is added later.
    AddAnnotation(AnnotationKind),
}

/// Maps one egui event to an editor command, or `None` for events the surface
/// ignores (pointer motion, key releases, shortcuts we do not bind).
///
/// `Event::Text` is the source of typed characters; Enter and Backspace never
/// arrive as text (see egui's `Event` docs), so they are handled as keys.
pub fn command_for_event(event: &Event) -> Option<Command> {
    match event {
        Event::Text(text) if !text.is_empty() => Some(Command::Insert(text.clone())),
        Event::Paste(text) if !text.is_empty() => Some(Command::Paste(text.clone())),
        Event::Key {
            key,
            physical_key,
            pressed: true,
            modifiers,
            ..
        } => command_for_key(*key, *modifiers).or_else(|| {
            // Shift turns a digit into a symbol, and egui-winit looks the typed
            // character up with `Key::from_name` before falling back to the
            // physical key: Shift+1 becomes `Exclamationmark`, not `Num1`, so a
            // chord bound to the digit misses. `block_kind_for_key` names the
            // one symbol it knows, and the physical digit catches the rest (a
            // layout where the shifted symbol has no name, or one where Shift+1
            // is not `!` at all). The fallback is limited to the block-kind
            // digits: other chords are letters, and a physical position means
            // something else on a non-QWERTY layout.
            if !(modifiers.command && modifiers.shift) {
                return None;
            }
            let physical = (*physical_key)?;
            if block_kind_for_key(physical).is_none() {
                return None;
            }
            command_for_key(physical, *modifiers)
        }),
        _ => None,
    }
}

/// The block kind bound to `key`, if any: the digits of the
/// Ctrl-Shift-1/2/3/0 chords and the shifted symbols that stand in for them.
fn block_kind_for_key(key: Key) -> Option<BlockKind> {
    match key {
        Key::Num0 => Some(BlockKind::Paragraph),
        Key::Num1 => Some(BlockKind::Heading { level: 1 }),
        Key::Num2 => Some(BlockKind::Heading { level: 2 }),
        Key::Num3 => Some(BlockKind::Heading { level: 3 }),
        // Shift+1 is `!` on most layouts, and `Key::from_name("!")` names it,
        // so egui-winit hands us this instead of the digit.
        Key::Exclamationmark => Some(BlockKind::Heading { level: 1 }),
        _ => None,
    }
}

/// Keyboard map. `Ctrl` is the command modifier on Linux/macOS windows alike
/// (`Modifiers::command`), so Ctrl-Z / Cmd-Z both undo.
pub fn command_for_key(key: Key, modifiers: Modifiers) -> Option<Command> {
    let command = modifiers.command;
    match key {
        Key::Z if command && modifiers.shift => Some(Command::Redo),
        Key::Z if command => Some(Command::Undo),
        Key::Y if command => Some(Command::Redo),
        // Ctrl-Shift-1/2/3 make a heading; Ctrl-Shift-0 returns a block to
        // body text. `command` is Ctrl (Cmd) on every platform, so the chords
        // travel. The shifted symbols are the same chords as egui-winit
        // delivers them (see `command_for_event`).
        Key::Num0 | Key::Num1 | Key::Num2 | Key::Num3 | Key::Exclamationmark
            if command && modifiers.shift =>
        {
            block_kind_for_key(key).map(Command::SetBlockKind)
        }
        Key::Enter => Some(Command::SplitParagraph),
        // Ctrl-Shift-N / Ctrl-Shift-F add a note (a highlight) or a footnote at
        // the selection.
        Key::N if command && modifiers.shift => Some(Command::AddAnnotation(AnnotationKind::Note)),
        Key::F if command && modifiers.shift => {
            Some(Command::AddAnnotation(AnnotationKind::Footnote))
        }
        Key::Backspace => Some(Command::Backspace),
        Key::Delete => Some(Command::Delete),
        // Ctrl/Cmd-arrow moves by word; with Shift it extends the selection a
        // whole word at a time.
        Key::ArrowLeft if command && modifiers.shift => Some(Command::SelectWordLeft),
        Key::ArrowLeft if command => Some(Command::MoveWordLeft),
        Key::ArrowRight if command && modifiers.shift => Some(Command::SelectWordRight),
        Key::ArrowRight if command => Some(Command::MoveWordRight),
        Key::ArrowLeft if modifiers.shift && !command => Some(Command::SelectLeft),
        Key::ArrowLeft if !command => Some(Command::MoveLeft),
        Key::ArrowRight if modifiers.shift && !command => Some(Command::SelectRight),
        Key::ArrowRight if !command => Some(Command::MoveRight),
        Key::ArrowUp if modifiers.shift && !command => Some(Command::SelectUp),
        Key::ArrowUp if !command => Some(Command::MoveUp),
        Key::ArrowDown if modifiers.shift && !command => Some(Command::SelectDown),
        Key::ArrowDown if !command => Some(Command::MoveDown),
        Key::Home if modifiers.shift && !command => Some(Command::SelectLineStart),
        Key::Home if !command => Some(Command::MoveLineStart),
        Key::End if modifiers.shift && !command => Some(Command::SelectLineEnd),
        Key::End if !command => Some(Command::MoveLineEnd),
        Key::A if command => Some(Command::SelectAll),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AnnotationKind, BlockKind};
    use cosmic_text::fontdb;
    use cosmic_text::{Family, Style, Weight};
    use uuid::Uuid;

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

    fn heading(text: &str) -> Block {
        Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Heading { level: 1 },
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    fn face(renderer: &GlyphRenderer, family: &str, weight: Weight, style: Style) -> fontdb::ID {
        renderer
            .font_system
            .db()
            .query(&fontdb::Query {
                families: &[Family::Name(family)],
                weight,
                style,
                ..Default::default()
            })
            .unwrap_or_else(|| panic!("{family} ({weight:?}, {style:?}) is not registered"))
    }

    #[test]
    fn runtime_font_system_registers_every_face() {
        let renderer = GlyphRenderer::new(config());
        for (family, weight, style) in [
            ("EB Garamond", Weight::NORMAL, Style::Normal),
            ("EB Garamond", Weight::BOLD, Style::Normal),
            ("EB Garamond", Weight::NORMAL, Style::Italic),
            ("EB Garamond", Weight::SEMIBOLD, Style::Normal),
            ("Crimson Pro", Weight::NORMAL, Style::Normal),
            ("Crimson Pro", Weight::BOLD, Style::Normal),
            ("Crimson Pro", Weight::NORMAL, Style::Italic),
        ] {
            face(&renderer, family, weight, style);
        }
        assert_ne!(
            face(&renderer, "EB Garamond", Weight::NORMAL, Style::Normal),
            face(&renderer, "Crimson Pro", Weight::NORMAL, Style::Normal),
            "body and heading must not share a face"
        );
    }

    /// egui's font backend (ab_glyph) must parse the bundled faces — the TTF
    /// outlines included — or `install_fonts` panics at startup and the UI
    /// falls back to a stock face. This pins the parse at the source.
    #[test]
    fn bundled_faces_parse_for_egui() {
        let faces: &[&'static [u8]] = &[
            include_bytes!("../../fonts/EBGaramond-Regular.otf"),
            include_bytes!("../../fonts/EBGaramond-Bold.otf"),
            include_bytes!("../../fonts/EBGaramond-Italic.otf"),
            include_bytes!("../../fonts/CrimsonPro-Regular.ttf"),
            include_bytes!("../../fonts/CrimsonPro-Bold.ttf"),
        ];
        for &bytes in faces {
            // `from_static` unwraps the parse; a bad face fails here.
            let _ = FontData::from_static(bytes);
        }
    }

    /// The runtime renderer must shape a heading with Crimson Pro, not fall
    /// back to the body face.
    #[test]
    fn runtime_headings_shape_with_crimson_pro() {
        let mut renderer = GlyphRenderer::new(config());
        let crimson = face(&renderer, "Crimson Pro", Weight::BOLD, Style::Normal);
        let body_face = face(&renderer, "EB Garamond", Weight::NORMAL, Style::Normal);
        let block = heading("On the Shape of Letters");
        let layout = renderer.layout(&block);
        let run = layout.buffer.layout_runs().next().expect("one shaped run");
        assert!(!run.glyphs.is_empty());
        assert!(
            run.glyphs.iter().all(|glyph| glyph.font_id == crimson),
            "heading glyphs did not use the Crimson Pro face"
        );
        assert!(
            run.glyphs.iter().all(|glyph| glyph.font_id != body_face),
            "heading glyphs fell back to EB Garamond"
        );
    }

    #[test]
    fn texture_cache_uploads_ink_sized_images_at_their_offset() {
        let ctx = Context::default();
        let mut renderer = GlyphRenderer::new(config());
        let blocks = vec![paragraph("Hello"), paragraph("World")];
        let layouts: Vec<BlockLayout> = blocks.iter().map(|b| renderer.layout(b)).collect();
        let mut cache = TextureCache::new();

        let layout_refs: Vec<&BlockLayout> = layouts.iter().collect();
        cache.sync(
            &ctx,
            &mut renderer,
            &blocks,
            &layout_refs,
            [10, 10, 10],
            [255, 255, 255],
            &HashMap::new(),
        );
        assert_eq!(cache.len(), 2);

        let texture = cache.get(blocks[0].id).expect("first block texture");
        let [width, height] = texture.texture.size();
        assert!(width > 1 && height > 1, "texture has no ink");
        // The quad starts at the block origin plus the ink offset, so its
        // top-left is the placed ink rectangle, not the block box.
        let origin = Pos2::new(100.0, 50.0);
        let rect = texture.rect(origin);
        assert_eq!(rect.min.x, 100.0 + texture.left as f32);
        assert_eq!(rect.min.y, 50.0 + texture.top as f32);
        assert_eq!(rect.width(), width as f32);
    }

    /// Runs one egui pass with the display at `scale`, as a real frame would.
    fn set_scale(ctx: &Context, scale: f32) {
        ctx.set_pixels_per_point(scale);
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        // No backend applies these uploads in a headless test.
        output.textures_delta.clear();
        assert_eq!(ctx.pixels_per_point(), scale);
    }

    #[test]
    fn a_quad_maps_every_texel_onto_one_physical_pixel() {
        for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0, 3.0] {
            for origin in [
                Pos2::new(100.0, 50.0),
                Pos2::new(137.3, 211.7),
                Pos2::new(0.33, 0.66),
            ] {
                let rect = pixel_aligned_quad(origin, (3, -11), [401, 57], scale);
                let on_grid = |points: f32| {
                    let pixels = points * scale;
                    (pixels - pixels.round()).abs() < 1e-3
                };
                assert!(
                    on_grid(rect.min.x) && on_grid(rect.min.y),
                    "quad at {:?} is off the pixel grid at scale {scale}",
                    rect.min
                );
                assert!((rect.width() * scale - 401.0).abs() < 1e-3);
                assert!((rect.height() * scale - 57.0).abs() < 1e-3);
                // Snapping moves the page by under one physical pixel.
                let unsnapped_x = origin.x + 3.0 / scale;
                assert!((rect.min.x - unsnapped_x).abs() <= 0.5 / scale + 1e-3);
            }
        }
    }

    #[test]
    fn a_new_display_scale_replaces_block_textures() {
        // A window's first frame runs at 1.0, before the compositor reports its
        // scale; a texture kept from that frame would be enlarged for good.
        let ctx = Context::default();
        let mut renderer = GlyphRenderer::new(config());
        let blocks = vec![paragraph("Hello")];
        let layouts: Vec<BlockLayout> = blocks.iter().map(|b| renderer.layout(b)).collect();
        let layout_refs: Vec<&BlockLayout> = layouts.iter().collect();
        let mut cache = TextureCache::new();
        let sync = |cache: &mut TextureCache, renderer: &mut GlyphRenderer| {
            cache.sync(
                &ctx,
                renderer,
                &blocks,
                &layout_refs,
                [10, 10, 10],
                [255, 255, 255],
                &HashMap::new(),
            )
        };

        sync(&mut cache, &mut renderer);
        let first = cache.get(blocks[0].id).expect("first frame texture");
        let first_size = first.texture.size();
        let first_id = first.texture.id();

        set_scale(&ctx, 1.5);
        sync(&mut cache, &mut renderer);
        let scaled = cache.get(blocks[0].id).expect("scaled texture");
        assert_ne!(scaled.texture.id(), first_id, "kept the 1x texture");
        assert!(
            scaled.texture.size()[0] > first_size[0],
            "the physical texture should be larger than the 1x one"
        );
        assert_eq!(scaled.scale, 1.5);
        let origin = Pos2::new(100.0, 50.0);
        let quad = scaled.rect(origin);
        assert!((quad.width() * 1.5 - scaled.texture.size()[0] as f32).abs() < 1e-3);

        // An unchanged scale is still a cache hit.
        let stable_id = scaled.texture.id();
        sync(&mut cache, &mut renderer);
        assert_eq!(cache.get(blocks[0].id).unwrap().texture.id(), stable_id);
    }

    #[test]
    fn a_new_display_scale_replaces_annotation_textures() {
        let ctx = Context::default();
        let mut renderer = GlyphRenderer::new(config());
        let id = Uuid::new_v4();
        let mut cache = AnnotationTextureCache::new();
        let sync = |cache: &mut AnnotationTextureCache, renderer: &mut GlyphRenderer| {
            cache.sync(
                &ctx,
                renderer,
                id,
                "A margin note",
                120.0,
                12.0,
                [0, 0, 0],
                [255, 255, 255],
            )
        };

        sync(&mut cache, &mut renderer);
        let first_size = cache.get(id).expect("first texture").texture.size();
        let first_height = cache.body_height(id).expect("first height");

        set_scale(&ctx, 2.0);
        sync(&mut cache, &mut renderer);
        let scaled = cache.get(id).expect("scaled texture");
        assert!(
            scaled.texture.size()[0] > first_size[0],
            "the annotation kept its 1x texture"
        );
        assert_eq!(scaled.scale, 2.0);
        // Page geometry stays logical: the stack does not move with the display.
        assert_eq!(cache.body_height(id), Some(first_height));
    }

    #[test]
    fn texture_cache_replaces_changed_blocks_and_drops_removed_ones() {
        let ctx = Context::default();
        let mut renderer = GlyphRenderer::new(config());
        let mut blocks = vec![paragraph("Hello"), paragraph("World")];
        let mut layouts: Vec<BlockLayout> = blocks.iter().map(|b| renderer.layout(b)).collect();
        let mut cache = TextureCache::new();
        let refs: Vec<&BlockLayout> = layouts.iter().collect();
        cache.sync(
            &ctx,
            &mut renderer,
            &blocks,
            &refs,
            [0, 0, 0],
            [255, 255, 255],
            &HashMap::new(),
        );
        let first_size = cache.get(blocks[0].id).unwrap().texture.size();

        blocks[0].text = "Hello, much longer line".to_string();
        layouts = blocks.iter().map(|b| renderer.layout(b)).collect();
        let refs: Vec<&BlockLayout> = layouts.iter().collect();
        cache.sync(
            &ctx,
            &mut renderer,
            &blocks,
            &refs,
            [0, 0, 0],
            [255, 255, 255],
            &HashMap::new(),
        );
        assert_ne!(
            cache.get(blocks[0].id).unwrap().texture.size(),
            first_size,
            "a changed block must be re-rasterized"
        );

        blocks.pop();
        layouts = blocks.iter().map(|b| renderer.layout(b)).collect();
        let refs: Vec<&BlockLayout> = layouts.iter().collect();
        cache.sync(
            &ctx,
            &mut renderer,
            &blocks,
            &refs,
            [0, 0, 0],
            [255, 255, 255],
            &HashMap::new(),
        );
        assert_eq!(cache.len(), 1, "the removed block's texture is dropped");
    }

    /// FIX 3: shaping is cached by content fingerprint, so a repaint with no
    /// edits re-shapes nothing and an edit re-shapes only its own block.
    #[test]
    fn layout_cache_shapes_only_changed_blocks() {
        let mut renderer = GlyphRenderer::new(config());
        let mut blocks = vec![paragraph("Hello"), paragraph("World")];
        let mut cache = LayoutCache::new();

        let layouts = cache.sync(&mut renderer, &blocks);
        assert_eq!(layouts.len(), 2);
        assert_eq!(
            renderer.layout_calls(),
            2,
            "the first sync lays out every block"
        );

        let layouts = cache.sync(&mut renderer, &blocks);
        assert_eq!(layouts.len(), 2);
        assert_eq!(
            renderer.layout_calls(),
            2,
            "an unchanged frame must not re-shape anything"
        );

        blocks[0].text = "Hello, much longer line".to_string();
        let layouts = cache.sync(&mut renderer, &blocks);
        assert_eq!(layouts.len(), 2);
        assert_eq!(
            renderer.layout_calls(),
            3,
            "only the changed block is re-shaped"
        );
        assert_eq!(cache.len(), 2);

        blocks.pop();
        let layouts = cache.sync(&mut renderer, &blocks);
        assert_eq!(layouts.len(), 1);
        assert_eq!(
            renderer.layout_calls(),
            3,
            "removing a block does not require shaping"
        );
        assert_eq!(
            cache.len(),
            1,
            "the removed block's cached layout is dropped"
        );
    }

    #[test]
    fn caret_x_tracks_the_pen_across_lines() {
        let mut renderer = GlyphRenderer::new(LayoutConfig {
            body_width: 160.0,
            ..config()
        });
        let block = paragraph(
            "Typography is the craft of endowing human language with a durable visual form.",
        );
        let layout = renderer.layout(&block);
        assert!(layout.lines.len() >= 2, "expected a wrapped paragraph");

        let first_line = layout.buffer.layout_runs().next().unwrap();
        let first_glyph = first_line.glyphs.first().unwrap();
        assert_eq!(caret_x(&layout, 0), first_glyph.x);

        let second = layout.buffer.layout_runs().nth(1).unwrap();
        let second_first = second.glyphs.first().unwrap();
        assert_eq!(
            caret_x(&layout, second_first.start),
            second_first.x,
            "a caret at the second line's first glyph sits on that glyph's pen"
        );
        assert!(second_first.x < first_line.glyphs.last().unwrap().x);
    }

    #[test]
    fn offset_at_round_trips_the_caret_pen() {
        let mut renderer = GlyphRenderer::new(config());
        let block = paragraph("Hello world");
        let layout = renderer.layout(&block);
        let run = layout.buffer.layout_runs().next().unwrap();

        for glyph in run.glyphs.iter() {
            let x = caret_x(&layout, glyph.start);
            let offset = offset_at(&layout, x, run.line_top + 1.0, block.text.len());
            assert_eq!(
                offset, glyph.start,
                "clicking the pen of byte {} did not land on it",
                glyph.start
            );
        }
        assert_eq!(offset_at(&layout, -50.0, 0.0, block.text.len()), 0);
        assert_eq!(
            offset_at(&layout, 10_000.0, 0.0, block.text.len()),
            block.text.len()
        );
    }

    #[test]
    fn offset_at_lands_the_caret_before_the_clicked_glyph() {
        let mut renderer = GlyphRenderer::new(config());
        let block = paragraph("Hello world");
        let layout = renderer.layout(&block);
        let run = layout.buffer.layout_runs().next().unwrap();
        let y = run.line_top + 1.0;
        // Click the dead centre of each glyph: the caret must precede it, never
        // the next letter over.
        for glyph in run.glyphs.iter() {
            let midpoint = glyph.x + glyph.w * 0.5;
            let offset = offset_at(&layout, midpoint, y, block.text.len());
            assert_eq!(
                offset, glyph.start,
                "mid-glyph click jumped past the letter under the pointer"
            );
        }
        // Just left of a glyph's right edge still belongs to that glyph.
        let last = run.glyphs.last().unwrap();
        assert_eq!(
            offset_at(&layout, last.x + last.w - 0.5, y, block.text.len()),
            last.start
        );
        // Past the end of the run falls to the line's end.
        assert_eq!(
            offset_at(&layout, last.x + last.w + 50.0, y, block.text.len()),
            last.end
        );
    }

    #[test]
    fn offset_at_finds_the_line_under_the_pointer() {
        let mut renderer = GlyphRenderer::new(LayoutConfig {
            body_width: 160.0,
            ..config()
        });
        let block = paragraph(
            "Typography is the craft of endowing human language with a durable visual form.",
        );
        let layout = renderer.layout(&block);
        let second = layout.buffer.layout_runs().nth(1).unwrap();
        let second_first = second.glyphs.first().unwrap();
        let offset = offset_at(
            &layout,
            second_first.x + 1.0,
            second.line_top + 1.0,
            block.text.len(),
        );
        assert!(
            offset >= second_first.start && offset <= second.glyphs.last().unwrap().end,
            "click on the second line mapped to byte {offset}"
        );
    }

    #[test]
    fn empty_block_geometry_is_safe() {
        let mut renderer = GlyphRenderer::new(config());
        let block = paragraph("");
        let layout = renderer.layout(&block);
        assert_eq!(caret_x(&layout, 0), 0.0);
        assert_eq!(offset_at(&layout, 42.0, 3.0, 0), 0);
        assert!(caret_line(&layout, 0).is_none());
    }

    #[test]
    fn boundaries_respect_utf8() {
        let text = "aé";
        assert_eq!(prev_boundary(text, 3), Some(1));
        assert_eq!(prev_boundary(text, 1), Some(0));
        assert_eq!(prev_boundary(text, 0), None);
        assert_eq!(next_boundary(text, 0), Some(1));
        assert_eq!(next_boundary(text, 1), Some(3));
        assert_eq!(next_boundary(text, 3), None);
    }

    #[test]
    fn word_boundaries_skip_whole_words() {
        let text = "The quick brown fox";
        // next_word skips the current word and any gap, landing on the next.
        assert_eq!(next_word(text, 0), Some(4));
        assert_eq!(next_word(text, 2), Some(4), "inside a word, skip to next");
        assert_eq!(next_word(text, 4), Some(10));
        assert_eq!(next_word(text, 10), Some(16));
        assert_eq!(next_word(text, 19), None);

        // prev_word: inside a word -> its start; between words -> previous.
        assert_eq!(prev_word(text, 2), Some(0), "inside a word -> its start");
        assert_eq!(prev_word(text, 4), Some(0), "after a gap -> previous word");
        assert_eq!(prev_word(text, 10), Some(4));
        assert_eq!(
            prev_word(text, 19),
            Some(16),
            "at end of a word -> its start"
        );
        assert_eq!(prev_word(text, 0), None);

        // Apostrophes stay within a word, so "don't" moves as one unit.
        assert_eq!(next_word("don't stop", 0), Some(6));
        assert_eq!(prev_word("don't stop", 6), Some(0));
    }

    #[test]
    fn range_rects_split_a_range_across_visual_lines() {
        let mut renderer = GlyphRenderer::new(LayoutConfig {
            body_width: 160.0,
            ..config()
        });
        let block = paragraph(
            "Typography is the craft of endowing human language with a durable visual form.",
        );
        let layout = renderer.layout(&block);
        assert!(layout.lines.len() >= 2, "expected a wrapped paragraph");
        let first = layout.lines[0];
        let second = layout.lines[1];

        // From the middle of line one to the middle of line two.
        let start = (first.start + first.end) / 2;
        let end = (second.start + second.end) / 2;
        let rects = range_rects(&layout, start, end);
        assert_eq!(rects.len(), 2, "one band per covered line");
        assert_eq!(rects[0].min.y, first.y);
        assert_eq!(rects[1].min.y, second.y);
        assert!(rects[0].width() > 0.0 && rects[1].width() > 0.0);
        assert!(
            rects[0].max.x <= config().body_width + 1.0,
            "a selection band stays inside the measure"
        );
    }

    #[test]
    fn range_rects_ignore_empty_and_out_of_range_spans() {
        let mut renderer = GlyphRenderer::new(config());
        let block = paragraph("Hello world");
        let layout = renderer.layout(&block);

        assert!(range_rects(&layout, 3, 3).is_empty(), "a caret has no band");
        assert_eq!(
            range_rects(&layout, 8, 2),
            range_rects(&layout, 2, 8),
            "a reversed range is normalized"
        );
        assert!(
            range_rects(&layout, 0, 5).len() == 1,
            "a single-line range is one band"
        );
        assert_eq!(
            range_rects(&layout, 0, 5)[0].max.x,
            caret_x(&layout, 5),
            "the band ends on the caret pen"
        );
    }

    #[test]
    fn word_at_expands_a_caret_to_its_word() {
        let text = "The quick brown fox";
        assert_eq!(word_at(text, 6), (4, 9), "inside 'quick'");
        assert_eq!(word_at(text, 9), (4, 9), "at the end of a word");
        assert_eq!(word_at(text, 4), (4, 9), "at the start of a word");
        assert_eq!(word_at(text, 0), (0, 3));
        assert_eq!(
            word_at("first second", 6),
            (6, 12),
            "a caret on a space takes the following word"
        );
        assert_eq!(
            word_at("aé b", 1),
            (0, 3),
            "a multi-byte word is a full byte range"
        );
        assert_eq!(word_at("", 0), (0, 0));
        assert_eq!(word_at(" -- ", 2), (2, 2), "punctuation is not a word");
    }

    #[test]
    fn annotation_texture_wraps_inside_a_narrow_margin_column() {
        let mut renderer = GlyphRenderer::new(config());
        let block = paragraph("Typography as an independent existence.");
        let layout = renderer.layout_in_column(&block, 168.0, 13.12);
        assert!(
            layout.lines.len() >= 2,
            "a 40-character note must wrap in 168px"
        );
        let widest = layout
            .buffer
            .layout_runs()
            .filter(|run| !run.glyphs.is_empty())
            .map(|run| run.line_w)
            .fold(0.0_f32, f32::max);
        assert!(widest <= 169.0, "a line overflows the column: {widest}");
    }

    #[test]
    fn annotation_texture_reports_the_height_its_body_wraps_to() {
        let ctx = Context::default();
        let mut renderer = GlyphRenderer::new(config());
        let id = Uuid::new_v4();
        let mut cache = AnnotationTextureCache::new();
        let note = "Typography is the craft of endowing human language with a durable \
visual form, and thus with an independent existence.";
        cache.sync(
            &ctx,
            &mut renderer,
            id,
            note,
            120.0,
            12.0,
            [0, 0, 0],
            [255, 255, 255],
        );

        let height = cache
            .body_height(id)
            .expect("a shaped body reports its height");
        let line = 12.0 * 1.4;
        assert!(
            height >= 3.0 * line - 0.01,
            "a body that wraps to several lines must report them all, got {height}"
        );

        // The height tracks the body: a one-line note is shorter than the
        // wrapped one, so margin stacking can tell them apart.
        let short = Uuid::new_v4();
        cache.sync(
            &ctx,
            &mut renderer,
            short,
            "a note",
            120.0,
            12.0,
            [0, 0, 0],
            [255, 255, 255],
        );
        let short_height = cache.body_height(short).expect("height");
        assert!(short_height < height);
        assert!(
            cache.body_height(Uuid::new_v4()).is_none(),
            "unknown ids have no height"
        );
    }

    #[test]
    fn annotation_textures_cache_by_content_and_metrics() {
        let ctx = Context::default();
        let mut renderer = GlyphRenderer::new(config());
        let id = Uuid::new_v4();
        let mut cache = AnnotationTextureCache::new();

        cache.sync(
            &ctx,
            &mut renderer,
            id,
            "A margin note",
            120.0,
            12.0,
            [0, 0, 0],
            [255, 255, 255],
        );
        assert_eq!(cache.len(), 1);
        let size = cache.get(id).expect("annotation texture").texture.size();

        // Same text at a different column width must be re-shaped.
        cache.sync(
            &ctx,
            &mut renderer,
            id,
            "A margin note",
            80.0,
            12.0,
            [0, 0, 0],
            [255, 255, 255],
        );
        assert_eq!(cache.len(), 1);

        // A renumbered footnote entry changes the text.
        cache.sync(
            &ctx,
            &mut renderer,
            id,
            "2. A margin note",
            80.0,
            12.0,
            [0, 0, 0],
            [255, 255, 255],
        );
        assert_ne!(cache.get(id).unwrap().texture.size(), size);
    }

    #[test]
    fn command_map_binds_editing_keys() {
        let none = Modifiers::NONE;
        assert_eq!(
            command_for_event(&Event::Text("a".into())),
            Some(Command::Insert("a".into()))
        );
        assert_eq!(
            command_for_key(Key::Enter, none),
            Some(Command::SplitParagraph)
        );
        assert_eq!(
            command_for_key(Key::Backspace, none),
            Some(Command::Backspace)
        );
        assert_eq!(command_for_key(Key::Delete, none), Some(Command::Delete));
        assert_eq!(
            command_for_key(Key::ArrowLeft, none),
            Some(Command::MoveLeft)
        );
        assert_eq!(
            command_for_key(Key::ArrowRight, none),
            Some(Command::MoveRight)
        );
        assert_eq!(command_for_key(Key::ArrowUp, none), Some(Command::MoveUp));
        assert_eq!(
            command_for_key(Key::ArrowDown, none),
            Some(Command::MoveDown)
        );

        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            command_for_key(Key::ArrowLeft, shift),
            Some(Command::SelectLeft)
        );
        assert_eq!(
            command_for_key(Key::ArrowRight, shift),
            Some(Command::SelectRight)
        );
        assert_eq!(
            command_for_key(Key::ArrowUp, shift),
            Some(Command::SelectUp)
        );
        assert_eq!(
            command_for_key(Key::ArrowDown, shift),
            Some(Command::SelectDown)
        );
        assert_eq!(
            command_for_key(Key::Home, shift),
            Some(Command::SelectLineStart)
        );
        assert_eq!(
            command_for_key(Key::End, shift),
            Some(Command::SelectLineEnd)
        );

        let ctrl = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        assert_eq!(command_for_key(Key::Z, ctrl), Some(Command::Undo));
        assert_eq!(command_for_key(Key::Y, ctrl), Some(Command::Redo));
        let ctrl_shift = Modifiers {
            shift: true,
            ..ctrl
        };
        assert_eq!(command_for_key(Key::Z, ctrl_shift), Some(Command::Redo));
        assert_eq!(
            command_for_key(Key::N, ctrl_shift),
            Some(Command::AddAnnotation(AnnotationKind::Note))
        );
        assert_eq!(
            command_for_key(Key::F, ctrl_shift),
            Some(Command::AddAnnotation(AnnotationKind::Footnote))
        );

        // Key releases and unbound keys do nothing.
        assert_eq!(
            command_for_event(&Event::Key {
                key: Key::Backspace,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: none,
            }),
            None
        );
        assert_eq!(command_for_key(Key::F5, none), None);
    }

    #[test]
    fn block_kind_chords_map_to_the_command() {
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            command_for_key(Key::Num1, ctrl_shift),
            Some(Command::SetBlockKind(BlockKind::Heading { level: 1 }))
        );
        assert_eq!(
            command_for_key(Key::Num2, ctrl_shift),
            Some(Command::SetBlockKind(BlockKind::Heading { level: 2 }))
        );
        assert_eq!(
            command_for_key(Key::Num3, ctrl_shift),
            Some(Command::SetBlockKind(BlockKind::Heading { level: 3 }))
        );
        assert_eq!(
            command_for_key(Key::Num0, ctrl_shift),
            Some(Command::SetBlockKind(BlockKind::Paragraph))
        );

        // Bare digits, and Ctrl-digit without Shift, are not the chord: they
        // must not surprise anyone typing a number or using a window-manager
        // shortcut.
        let none = Modifiers::NONE;
        let ctrl = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        assert_eq!(command_for_key(Key::Num1, none), None);
        assert_eq!(command_for_key(Key::Num1, ctrl), None);
    }

    /// A Ctrl-Shift-<digit> key event exactly as egui-winit builds it: the
    /// typed character goes through `Key::from_name` (Shift+1 types `!`, which
    /// is `Exclamationmark`), and only a character with no key name falls back
    /// to the physical key.
    fn shifted_digit_event(symbol: &str, physical: Key) -> Event {
        Event::Key {
            key: Key::from_name(symbol).unwrap_or(physical),
            physical_key: Some(physical),
            pressed: true,
            repeat: false,
            modifiers: Modifiers {
                ctrl: true,
                shift: true,
                command: true,
                ..Modifiers::NONE
            },
        }
    }

    #[test]
    fn every_block_kind_chord_fires_from_the_event_egui_winit_sends() {
        // Shift+1 is named, so the logical key is `Exclamationmark`, never
        // `Num1` — the case a hand-built `Modifiers` cannot catch.
        assert_eq!(Key::from_name("!"), Some(Key::Exclamationmark));
        for (symbol, physical, kind) in [
            ("!", Key::Num1, BlockKind::Heading { level: 1 }),
            ("@", Key::Num2, BlockKind::Heading { level: 2 }),
            ("#", Key::Num3, BlockKind::Heading { level: 3 }),
            (")", Key::Num0, BlockKind::Paragraph),
        ] {
            assert_eq!(
                command_for_event(&shifted_digit_event(symbol, physical)),
                Some(Command::SetBlockKind(kind)),
                "Ctrl-Shift-{symbol} must set the caret block's kind"
            );
        }

        // The shifted symbols with no key name arrive as the physical digit;
        // the named one must not double up when the physical key is elsewhere
        // on the keyboard.
        assert_eq!(Key::from_name("@"), None);
        assert_eq!(Key::from_name("#"), None);
        assert_eq!(Key::from_name(")"), None);
        assert_eq!(
            command_for_event(&shifted_digit_event("?", Key::Num1)),
            Some(Command::SetBlockKind(BlockKind::Heading { level: 1 })),
            "a layout whose Shift+1 is '?' still makes an H1 via the digit"
        );

        // A press without the chord is still just a typed character.
        let mut bare = shifted_digit_event("!", Key::Num1);
        if let Event::Key { modifiers, .. } = &mut bare {
            *modifiers = Modifiers::NONE;
        }
        assert_eq!(command_for_event(&bare), None);
    }

    /// The OFL asks for its text to travel with the fonts.
    #[test]
    fn every_font_family_has_its_licence() {
        let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts");
        for entry in std::fs::read_dir(&fonts).unwrap() {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".ttf") || name.ends_with(".otf") {
                let family = name.split('-').next().unwrap();
                let licence = fonts.join(format!("{family}-OFL.txt"));
                let text = std::fs::read_to_string(&licence)
                    .unwrap_or_else(|_| panic!("{name} has no {}", licence.display()));
                assert!(text.contains("SIL OPEN FONT LICENSE"), "{name}");
            }
        }
    }

    /// A block's offsets count across its line breaks, whatever the text engine does
    /// with the lines underneath.
    #[test]
    fn caret_geometry_counts_offsets_across_line_breaks() {
        let mut renderer = GlyphRenderer::new(config());
        let layout = renderer.layout(&paragraph("abc\ndef\n\nxyz"));
        assert_eq!(
            layout.lines.len(),
            4,
            "the blank line is a line: {:?}",
            layout.lines
        );
        let (first, second, blank, fourth) = (
            layout.lines[0],
            layout.lines[1],
            layout.lines[2],
            layout.lines[3],
        );
        assert_eq!((first.start, first.end), (0, 3));
        assert_eq!((second.start, second.end), (4, 7));
        assert_eq!((blank.start, blank.end), (8, 8));
        assert_eq!((fourth.start, fourth.end), (9, 12));
        assert!(second.y > first.y && blank.y > second.y && fourth.y > blank.y);

        // The same column on different lines is the same pen, in a mono-free way:
        // the start of each line is flush left and its end past it.
        for start in [0, 4, 9] {
            assert_eq!(caret_x(&layout, start), 0.0, "offset {start}");
            assert!(caret_x(&layout, start + 3) > 0.0, "offset {}", start + 3);
        }
        assert_eq!(caret_x(&layout, 8), 0.0, "the blank line");

        // Each offset's caret sits on its own line, including at a line's end.
        for (offset, line) in [
            (0, first),
            (3, first),
            (4, second),
            (7, second),
            (8, blank),
            (9, fourth),
            (12, fourth),
        ] {
            assert_eq!(caret_line(&layout, offset), Some(line), "offset {offset}");
        }

        // A click maps back to the offset on the line it hit.
        let len = "abc
def

xyz"
        .len();
        assert_eq!(offset_at(&layout, 0.0, second.y + 1.0, len), 4);
        assert_eq!(offset_at(&layout, 500.0, second.y + 1.0, len), 7);
        assert_eq!(offset_at(&layout, 0.0, blank.y + 1.0, len), 8);
        assert_eq!(offset_at(&layout, 0.0, fourth.y + 1.0, len), 9);
    }

    /// Enter at the end of a code block leaves the caret on a new, empty last line.
    #[test]
    fn a_trailing_line_break_makes_a_line_for_the_caret() {
        let mut renderer = GlyphRenderer::new(config());
        let layout = renderer.layout(&paragraph("abc\n"));
        assert_eq!(layout.lines.len(), 2, "{:?}", layout.lines);
        assert_eq!((layout.lines[1].start, layout.lines[1].end), (4, 4));
        assert_eq!(caret_line(&layout, 4), Some(layout.lines[1]));
        assert_eq!(caret_x(&layout, 4), 0.0);
        assert!(layout.height > layout.lines[0].height);
    }

    #[test]
    fn a_highlight_lands_on_the_right_line_after_a_break() {
        let mut renderer = GlyphRenderer::new(config());
        let layout = renderer.layout(&paragraph("abc\ndef"));
        let rects = range_rects(&layout, 4, 7);
        assert_eq!(rects.len(), 1, "{rects:?}");
        assert_eq!(rects[0].min.y, layout.lines[1].y);
    }
}
