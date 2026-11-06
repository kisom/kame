//! egui presentation: fonts, glyph textures, caret geometry, and the keyboard
//! map from egui events to editor commands.
//!
//! Nothing here owns a window. The renderer works off a [`FontSystem`], a
//! [`LayoutConfig`] and an egui [`Context`], so the texture cache and the pure
//! geometry/command helpers can be exercised headlessly in tests.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;

use cosmic_text::{fontdb, FontSystem};
use egui::{
    ColorImage, Context, Event, FontData, FontFamily, Key, Modifiers, Pos2, Rect, TextureHandle,
    TextureOptions, Vec2,
};
pub use kame_core::text::{next_boundary, next_word, prev_boundary, prev_word, word_at};

use crate::layout::{layout_block_marked, layout_block_with_align, BlockLayout, LayoutConfig};
use crate::model::{AnnotationId, AnnotationKind, Block, BlockId, BlockKind};
use crate::render::{GlyphAtlas, Painting, Patch};

/// Every face the layout can ask for, compiled into the binary.
///
/// Loading these into the runtime [`FontSystem`] is not optional: headings
/// resolve `Crimson Pro` and body text `EB Garamond`, and a missing heading
/// face silently falls back to the body family. Embedding the files (rather
/// than reading `fonts/` at startup) keeps a binary copied elsewhere working
/// while still sourcing exactly the files in `fonts/`.
///
/// No full static of Crimson Pro's bold italic is published at the version of the other faces, 1.003,
/// so that one is Google Fonts' variable italic set at weight 700 and named to match.
const FONTS: &[&[u8]] = &[
    include_bytes!("../../fonts/EBGaramond-Regular.otf"),
    include_bytes!("../../fonts/EBGaramond-Bold.otf"),
    include_bytes!("../../fonts/EBGaramond-Italic.otf"),
    include_bytes!("../../fonts/EBGaramond-BoldItalic.otf"),
    include_bytes!("../../fonts/EBGaramond-SemiBold.otf"),
    include_bytes!("../../fonts/CrimsonPro-Regular.ttf"),
    include_bytes!("../../fonts/CrimsonPro-Bold.ttf"),
    include_bytes!("../../fonts/CrimsonPro-Italic.ttf"),
    include_bytes!("../../fonts/CrimsonPro-BoldItalic.ttf"),
    include_bytes!("../../fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../../fonts/IBMPlexMono-Bold.ttf"),
    include_bytes!("../../fonts/IBMPlexMono-Italic.ttf"),
    include_bytes!("../../fonts/IBMPlexMono-BoldItalic.ttf"),
];

/// A font system that sets a document in the bundled faces, with the system's other fonts there
/// for text those can't set, such as Chinese or emoji.
pub fn font_system() -> FontSystem {
    let (locale, mut db) = FontSystem::new().into_locale_and_db();
    load_bundled(&mut db, FONTS);
    // Built again because it notes which faces are monospaced as it's built, and those have changed.
    FontSystem::new_with_locale_and_db(locale, db)
}

/// Loads `faces` and takes out every other face of their families. Installed fonts come first in
/// the database and the first of equal matches wins, so a copy on this machine would otherwise set
/// the document, and it might not be the same version.
fn load_bundled(db: &mut fontdb::Database, faces: &[&[u8]]) {
    for bytes in faces {
        db.load_font_data(bytes.to_vec());
    }
    let families: HashSet<String> = db
        .faces()
        .filter(|face| is_bundled(face))
        .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
        .collect();
    let installed: Vec<fontdb::ID> = db
        .faces()
        .filter(|face| !is_bundled(face))
        .filter(|face| {
            face.families
                .iter()
                .any(|(name, _)| families.contains(name))
        })
        .map(|face| face.id)
        .collect();
    for id in installed {
        db.remove_face(id);
    }
}

/// Whether a face came from the binary, rather than from a file on this machine.
fn is_bundled(face: &fontdb::FaceInfo) -> bool {
    matches!(face.source, fontdb::Source::Binary(_))
}

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
/// [`GlyphAtlas::rasterize_premultiplied`].
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

/// A byte range of a block and the colour behind it.
pub type Highlight = (usize, usize, [u8; 3]);

/// Which blocks [`TextureCache::sync`] draws, and which of those it has drawn it keeps, by index
/// into the blocks given. The blocks run down the page, so each is one stretch of them, and a
/// block drawn counts as kept.
#[derive(Clone, Debug, PartialEq)]
pub struct Wanted {
    pub draw: Range<usize>,
    pub keep: Range<usize>,
}

impl Wanted {
    /// Every one of `blocks` blocks.
    pub fn all(blocks: usize) -> Wanted {
        Wanted {
            draw: 0..blocks,
            keep: 0..blocks,
        }
    }
}

/// Per-block texture cache, rebuilt only when a block's content changes.
///
/// The key is a hash of the block kind, text, and spans: any edit invalidates
/// just that block, and only the rows of lines drawn differently are drawn
/// again, when the image keeps its size and place. Textures for blocks that
/// left the document are dropped (and their GPU resources released) on the next
/// [`TextureCache::sync`].
pub struct TextureCache {
    entries: HashMap<BlockId, CachedTexture>,
}

struct CachedTexture {
    fingerprint: u64,
    block: BlockTexture,
    /// What its image was painted from, to tell what a new layout paints differently.
    painting: Painting,
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

    /// Rasterizes and uploads every block `wanted` draws whose content or display scale
    /// changed, and drops the textures of blocks no longer present or too far to keep.
    /// A long document holds images only of what's near the screen, as each is
    /// as large as its block is on the screen.
    ///
    /// `blocks` and `layouts` are parallel, as produced by
    /// [`GlyphRenderer::layout`]. Uploads use [`TextureOptions::NEAREST`] because
    /// [`BlockTexture::rect`] maps each texel onto exactly one screen pixel.
    ///
    /// The scale is part of what makes a texture current: a window's first frame
    /// runs before the compositor reports its scale, and a window can move
    /// between monitors.
    // What it takes to draw is simply this many things; a struct would only rename them.
    #[allow(clippy::too_many_arguments)]
    pub fn sync(
        &mut self,
        ctx: &Context,
        renderer: &mut GlyphRenderer,
        blocks: &[Block],
        layouts: &[&BlockLayout],
        ink: [u8; 3],
        background: [u8; 3],
        highlights: &HashMap<BlockId, Vec<Highlight>>,
        wanted: &Wanted,
    ) {
        let scale = ctx.pixels_per_point();
        let mut live = HashSet::with_capacity(wanted.keep.len());
        for (at, (block, layout)) in blocks.iter().zip(layouts.iter().copied()).enumerate() {
            let draw = wanted.draw.contains(&at);
            if draw || wanted.keep.contains(&at) {
                live.insert(block.id);
            }
            if !draw {
                continue;
            }
            let block_highlights = highlights.get(&block.id).map(Vec::as_slice).unwrap_or(&[]);
            let fingerprint = block_fingerprint(block, block_highlights);
            let up_to_date = self.entries.get(&block.id).is_some_and(|cached| {
                cached.fingerprint == fingerprint && cached.block.scale == scale
            });
            if up_to_date {
                continue;
            }

            let painting = renderer.painting(layout, ink, background, block_highlights, scale);
            // An edit usually changes a few of a block's lines, and only those rows are drawn
            // again and uploaded into the texture it has.
            if let Some(cached) = self.entries.get_mut(&block.id) {
                if let Some(patches) = renderer.redraw(&cached.painting, &painting) {
                    for patch in patches {
                        cached.block.texture.set_partial(
                            [0, patch.top],
                            patch.image,
                            TextureOptions::NEAREST,
                        );
                    }
                    cached.fingerprint = fingerprint;
                    cached.painting = painting;
                    continue;
                }
            }
            let (image, left, top) = renderer.draw(&painting);
            let texture = ctx.load_texture(
                format!("kame-block-{}", block.id),
                image,
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
                    painting,
                },
            );
        }
        self.entries.retain(|id, _| live.contains(id));
    }

    pub fn get(&self, block_id: BlockId) -> Option<&BlockTexture> {
        self.entries.get(&block_id).map(|cached| &cached.block)
    }

    /// What the image of `block_id` was last painted from, which changes whenever it is drawn
    /// again, whole or in part. It's public for the app's tests, which can't otherwise tell a
    /// texture drawn again in place from one left as it was.
    pub fn painting(&self, block_id: BlockId) -> Option<&Painting> {
        self.entries.get(&block_id).map(|cached| &cached.painting)
    }

    /// How many blocks it holds an image of.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drops every cached texture, so the next `sync` re-rasterizes against the
    /// current metrics. Used when the measure changes with the window.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Per-block shaping cache, rebuilt only when a block's content changes.
///
/// Keyed by the same [`block_fingerprint`] as the texture cache, so a frame
/// with no edits shapes nothing and an edit re-shapes only the block it
/// touched. Layouts of blocks that left the document are dropped.
pub struct LayoutCache {
    entries: HashMap<BlockId, CachedLayout>,
    /// How many blocks it has fingerprinted.
    #[cfg(test)]
    fingerprints: u64,
}

struct CachedLayout {
    fingerprint: u64,
    /// The revision the layout was last found current at.
    current_at: Option<u64>,
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
            #[cfg(test)]
            fingerprints: 0,
        }
    }

    /// Drops every cached layout, so the next `sync` shapes against the current
    /// metrics. Used when the measure changes with the window.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub fn sync(&mut self, renderer: &mut GlyphRenderer, blocks: &[Block]) -> Vec<&BlockLayout> {
        self.sync_marked(renderer, blocks, &HashMap::new(), None)
    }

    /// Returns one layout per block, in document order, shaping only blocks
    /// whose fingerprint changed since the last call. `marks` holds each block's
    /// footnote offsets, which a block needs room for.
    ///
    /// `revision` is a number the caller changes whenever `blocks` or `marks` may have, and a
    /// block already found current at it isn't fingerprinted again. With `None`, every block is.
    pub fn sync_marked(
        &mut self,
        renderer: &mut GlyphRenderer,
        blocks: &[Block],
        marks: &HashMap<BlockId, Vec<usize>>,
        revision: Option<u64>,
    ) -> Vec<&BlockLayout> {
        let mut live = HashSet::with_capacity(blocks.len());
        for block in blocks {
            live.insert(block.id);
            if let Some(cached) = self.entries.get(&block.id) {
                if revision.is_some() && cached.current_at == revision {
                    continue;
                }
            }
            #[cfg(test)]
            {
                self.fingerprints += 1;
            }
            let block_marks = marks.get(&block.id).map(Vec::as_slice).unwrap_or(&[]);
            let as_ranges: Vec<_> = block_marks
                .iter()
                .map(|mark| (*mark, *mark, [0; 3]))
                .collect();
            let fingerprint = block_fingerprint(block, &as_ranges);
            if let Some(cached) = self.entries.get_mut(&block.id) {
                if cached.fingerprint == fingerprint {
                    cached.current_at = revision;
                    continue;
                }
            }
            let layout = renderer.layout_marked(block, block_marks);
            self.entries.insert(
                block.id,
                CachedLayout {
                    fingerprint,
                    current_at: revision,
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
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The layout of `block_id` as last synced, if it has one.
    pub fn get(&self, block_id: BlockId) -> Option<&BlockLayout> {
        self.entries.get(&block_id).map(|cached| &cached.layout)
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
    // What it takes to draw is simply this many things; a struct would only rename them.
    #[allow(clippy::too_many_arguments)]
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
        if image.size[0] == 0 || image.size[1] == 0 {
            self.entries.remove(&annotation_id);
            return;
        }
        let texture = ctx.load_texture(
            format!("kame-annotation-{annotation_id}"),
            image,
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
    pub(crate) fn len(&self) -> usize {
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
        GlyphRenderer {
            font_system: font_system(),
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

    /// Paints `layout` into an ink-cropped image for egui and returns it with its
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
    ) -> (ColorImage, i32, i32) {
        let painting = self.painting(layout, ink, background, highlights, scale);
        self.draw(&painting)
    }

    /// What [`Self::rasterize`] would paint, for drawing it whole or in part.
    pub fn painting(
        &mut self,
        layout: &BlockLayout,
        ink: [u8; 3],
        background: [u8; 3],
        highlights: &[(usize, usize, [u8; 3])],
        scale: f32,
    ) -> Painting {
        self.rasterizes_since_clear += 1;
        if self.rasterizes_since_clear >= RASTERIZES_PER_CACHE_CLEAR {
            self.atlas.clear_cache();
            self.rasterizes_since_clear = 0;
        }
        self.atlas.painting(
            &mut self.font_system,
            layout,
            ink,
            background,
            highlights,
            scale,
        )
    }

    /// The image of `painting` and its offset, as [`Self::rasterize`] gives them.
    pub fn draw(&mut self, painting: &Painting) -> (ColorImage, i32, i32) {
        self.atlas
            .draw_premultiplied(&mut self.font_system, painting)
    }

    /// The bands of `before`'s image that make it `after`'s, or `None` if it must be drawn
    /// whole. See [`GlyphAtlas::redraw`].
    pub fn redraw(&mut self, before: &Painting, after: &Painting) -> Option<Vec<Patch>> {
        self.atlas.redraw(&mut self.font_system, before, after)
    }
}

// --- caret geometry --------------------------------------------------------

/// Block-relative pen X of byte `offset`.
///
/// A caret at a glyph's start sits on its pen position; a caret at the end of
/// a run sits after the last advance. Offsets that fall inside a multi-byte
/// cluster resolve to the cluster's start. Empty layouts report `0.0`.
pub fn caret_x(layout: &BlockLayout, offset: usize) -> f32 {
    for (index, run) in layout.runs.iter().enumerate() {
        let (Some(first), Some(last)) = (run.glyphs.first(), run.glyphs.last()) else {
            continue;
        };
        // Glyph offsets count from the start of their own line.
        let base = layout.line_start(run.line_i);
        if offset < base + first.start || offset > base + last.end {
            continue;
        }
        // Where a line wraps with no space between, the same offset ends this line and
        // starts the next, and `caret_line` says the caret belongs to the next.
        let next_starts_here = layout.runs.get(index + 1).is_some_and(|next| {
            next.line_i == run.line_i
                && next
                    .glyphs
                    .first()
                    .is_some_and(|glyph| base + glyph.start == offset)
        });
        if next_starts_here {
            continue;
        }
        return x_in_run(run, base, offset);
    }
    0.0
}

/// Where `offset` falls on `line`, even if it's the end of a line that the next carries on from.
pub fn x_on_line(layout: &BlockLayout, line: &crate::layout::LayoutLine, offset: usize) -> f32 {
    layout
        .runs
        .iter()
        .find(|run| !run.glyphs.is_empty() && run.line_top == line.y)
        .map_or_else(
            || caret_x(layout, offset),
            |run| x_in_run(run, layout.line_start(run.line_i), offset),
        )
}

/// The x of `offset` within one run: on a glyph's pen, or after the last glyph's advance.
fn x_in_run(run: &crate::layout::LayoutRun, base: usize, offset: usize) -> f32 {
    let Some(last) = run.glyphs.last() else {
        return 0.0;
    };
    if offset == base + last.end {
        return last.x + last.w;
    }
    run.glyphs
        .iter()
        .find(|glyph| offset >= base + glyph.start && offset < base + glyph.end)
        .map_or(0.0, |glyph| glyph.x)
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
    for run in &layout.runs {
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
        let x0 = x_on_line(layout, line, lo);
        let x1 = x_on_line(layout, line, hi);
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

// --- input mapping ---------------------------------------------------------

/// What a keyboard/clipboard event asks the editor to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Insert(String),
    Paste(String),
    Copy,
    Cut,
    Backspace,
    Delete,
    SplitParagraph,
    /// A line break that never leaves the block.
    LineBreak,
    Undo,
    Redo,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MovePageUp,
    MovePageDown,
    SelectPageUp,
    SelectPageDown,
    MoveLineStart,
    MoveLineEnd,
    /// Ctrl-arrow: move by whole word rather than by character.
    MoveWordLeft,
    MoveWordRight,
    SelectWordLeft,
    SelectWordRight,
    /// Esc-Backspace or Alt-Backspace.
    DeleteWordBack,
    /// Esc-<: the very start of the first block.
    MoveDocumentStart,
    /// Esc->: the very end of the last block.
    MoveDocumentEnd,
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
    /// Move the caret's list item, or the selected ones, a place up or down in the list.
    MoveListItem {
        up: bool,
    },
}

/// How long after Escape the next key still counts as following it.
pub const ESCAPE_PREFIX_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// Escape as a prefix, as in Emacs: Esc-<, Esc-> and Esc-Backspace.
#[derive(Default)]
pub struct EscapePrefix {
    armed_at: Option<std::time::Instant>,
}

impl EscapePrefix {
    pub fn arm(&mut self, now: std::time::Instant) {
        self.armed_at = Some(now);
    }

    /// The command `event` makes as the key after Escape, if it does. Keys the surface
    /// ignores leave the prefix armed, because Shift-comma reaches here as a key that
    /// means nothing and only then as the text `<`. Anything else uses it up.
    pub fn command_for(&mut self, event: &Event, now: std::time::Instant) -> Option<Command> {
        let armed_at = self.armed_at?;
        if now.saturating_duration_since(armed_at) > ESCAPE_PREFIX_WINDOW {
            self.armed_at = None;
            return None;
        }
        match event {
            Event::Text(text) => {
                self.armed_at = None;
                match text.as_str() {
                    "<" => Some(Command::MoveDocumentStart),
                    ">" => Some(Command::MoveDocumentEnd),
                    _ => None,
                }
            }
            Event::Key {
                key: Key::Backspace,
                pressed: true,
                ..
            } => {
                self.armed_at = None;
                Some(Command::DeleteWordBack)
            }
            other => {
                if command_for_event(other).is_some() {
                    self.armed_at = None;
                }
                None
            }
        }
    }
}

/// The line above or below the caret's within its block. `None` at the block's edge,
/// and in a block with no lines at all.
pub fn adjacent_line(
    layout: &BlockLayout,
    offset: usize,
    down: bool,
) -> Option<crate::layout::LayoutLine> {
    let line = caret_line(layout, offset)?;
    let index = layout
        .lines
        .iter()
        .position(|candidate| candidate.y == line.y)?;
    let target = if down {
        index + 1
    } else {
        index.checked_sub(1)?
    };
    layout.lines.get(target).copied()
}

/// Maps one egui event to an editor command, or `None` for events the surface
/// ignores (pointer motion, key releases, shortcuts we do not bind).
///
/// `Event::Text` is the source of typed characters; Enter and Backspace never
/// arrive as text (see egui's `Event` docs), so they are handled as keys.
pub fn command_for_event(event: &Event) -> Option<Command> {
    match event {
        Event::Text(text) if !text.is_empty() => Some(Command::Insert(text.clone())),
        // What an input method finishes composing, such as an accent and its letter, is typed.
        Event::Ime(egui::ImeEvent::Commit(text)) if !text.is_empty() => {
            Some(Command::Insert(text.clone()))
        }
        Event::Paste(text) if !text.is_empty() => Some(Command::Paste(text.clone())),
        Event::Copy => Some(Command::Copy),
        Event::Cut => Some(Command::Cut),
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
            block_kind_for_key(physical)?;
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
        Key::Num1 => Some(BlockKind::Heading {
            level: 1,
            centered: false,
        }),
        Key::Num2 => Some(BlockKind::Heading {
            level: 2,
            centered: false,
        }),
        Key::Num3 => Some(BlockKind::Heading {
            level: 3,
            centered: false,
        }),
        // Shift+1 is `!` on most layouts, and `Key::from_name("!")` names it,
        // so egui-winit hands us this instead of the digit.
        Key::Exclamationmark => Some(BlockKind::Heading {
            level: 1,
            centered: false,
        }),
        _ => None,
    }
}

/// Keyboard map. `Ctrl` is the command modifier on Linux/macOS windows alike
/// (`Modifiers::command`), so Ctrl-Z / Cmd-Z both undo.
pub fn command_for_key(key: Key, modifiers: Modifiers) -> Option<Command> {
    let command = modifiers.command;
    let word = command || modifiers.alt;
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
        Key::Enter if modifiers.shift => Some(Command::LineBreak),
        Key::Enter => Some(Command::SplitParagraph),
        // Ctrl-Shift-N / Ctrl-Shift-F add a note (a highlight) or a footnote at
        // the selection, and Ctrl-Shift-M a marginal on its block.
        Key::N if command && modifiers.shift => Some(Command::AddAnnotation(AnnotationKind::Note)),
        Key::M if command && modifiers.shift => {
            Some(Command::AddAnnotation(AnnotationKind::Marginal))
        }
        Key::F if command && modifiers.shift => {
            Some(Command::AddAnnotation(AnnotationKind::Footnote))
        }
        Key::Backspace if word => Some(Command::DeleteWordBack),
        Key::Backspace => Some(Command::Backspace),
        Key::Delete => Some(Command::Delete),
        // Ctrl/Cmd-arrow moves by word, as does Alt-arrow, which is the Mac's habit;
        // with Shift it extends the selection a whole word at a time.
        Key::ArrowLeft if word && modifiers.shift => Some(Command::SelectWordLeft),
        Key::ArrowLeft if word => Some(Command::MoveWordLeft),
        Key::ArrowRight if word && modifiers.shift => Some(Command::SelectWordRight),
        Key::ArrowRight if word => Some(Command::MoveWordRight),
        Key::ArrowLeft if modifiers.shift && !command => Some(Command::SelectLeft),
        Key::ArrowLeft if !command => Some(Command::MoveLeft),
        Key::ArrowRight if modifiers.shift && !command => Some(Command::SelectRight),
        Key::ArrowRight if !command => Some(Command::MoveRight),
        // Ctrl-Shift-Up/Down move the caret's list item, or the selected ones, in the list.
        Key::ArrowUp if command && modifiers.shift => Some(Command::MoveListItem { up: true }),
        Key::ArrowDown if command && modifiers.shift => Some(Command::MoveListItem { up: false }),
        Key::ArrowUp if modifiers.shift && !command => Some(Command::SelectUp),
        Key::ArrowUp if !command => Some(Command::MoveUp),
        Key::ArrowDown if modifiers.shift && !command => Some(Command::SelectDown),
        Key::ArrowDown if !command => Some(Command::MoveDown),
        Key::PageUp if modifiers.shift && !command => Some(Command::SelectPageUp),
        Key::PageUp if !command => Some(Command::MovePageUp),
        Key::PageDown if modifiers.shift && !command => Some(Command::SelectPageDown),
        Key::PageDown if !command => Some(Command::MovePageDown),
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
    use crate::model::{AnnotationKind, BlockKind, Span, SpanStyle};
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
            hyphenate: false,
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
            kind: BlockKind::Heading {
                level: 1,
                centered: false,
            },
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    fn named<'a>(db: &'a fontdb::Database, family: &str) -> Vec<&'a fontdb::FaceInfo> {
        db.faces()
            .filter(|face| face.families.iter().any(|(name, _)| name == family))
            .collect()
    }

    #[test]
    fn an_installed_copy_of_a_bundled_family_gives_way() {
        let fonts = concat!(env!("CARGO_MANIFEST_DIR"), "/fonts");
        let mut db = fontdb::Database::new();
        db.load_font_file(format!("{fonts}/IBMPlexMono-Bold.ttf"))
            .unwrap();
        load_bundled(
            &mut db,
            &[include_bytes!("../../fonts/IBMPlexMono-Regular.ttf")],
        );
        let plex = named(&db, "IBM Plex Mono");
        assert_eq!(plex.len(), 1);
        assert!(is_bundled(plex[0]));
    }

    #[test]
    fn an_installed_font_of_another_family_stays_for_fallback() {
        let fonts = concat!(env!("CARGO_MANIFEST_DIR"), "/fonts");
        let mut db = fontdb::Database::new();
        db.load_font_file(format!("{fonts}/EBGaramond-Regular.otf"))
            .unwrap();
        load_bundled(
            &mut db,
            &[include_bytes!("../../fonts/IBMPlexMono-Regular.ttf")],
        );
        assert_eq!(named(&db, "EB Garamond").len(), 1);
    }

    #[test]
    fn every_family_the_page_uses_comes_from_the_binary() {
        let renderer = GlyphRenderer::new(config());
        let db = renderer.font_system.db();
        for family in ["EB Garamond", "Crimson Pro", "IBM Plex Mono"] {
            let faces = named(db, family);
            assert!(!faces.is_empty(), "{family}");
            for face in faces {
                assert!(
                    is_bundled(face),
                    "{}: {:?}",
                    face.post_script_name,
                    face.source
                );
            }
        }
    }

    /// Bold italic is a face of its own in every family, rather than an italic thickened.
    #[test]
    fn every_family_has_a_true_bold_italic() {
        let renderer = GlyphRenderer::new(config());
        for family in ["EB Garamond", "Crimson Pro", "IBM Plex Mono"] {
            let id = face(&renderer, family, Weight::BOLD, Style::Italic);
            let found = renderer.font_system.db().face(id).unwrap();
            assert_eq!(found.weight, Weight::BOLD, "{family}");
            assert_eq!(found.style, Style::Italic, "{family}");
            assert!(is_bundled(found), "{family}");
        }
    }

    #[test]
    fn text_both_bold_and_italic_shapes_in_the_bold_italic_face() {
        let mut renderer = GlyphRenderer::new(config());
        let bold_italic = face(&renderer, "EB Garamond", Weight::BOLD, Style::Italic);
        let mut block = paragraph("Strong words");
        for style in [SpanStyle::Bold, SpanStyle::Italic] {
            block.spans.push(Span {
                start: 0,
                end: block.text.len(),
                style,
            });
        }
        let layout = renderer.layout(&block);
        let glyphs: Vec<_> = layout.runs.iter().flat_map(|run| &run.glyphs).collect();
        assert!(!glyphs.is_empty());
        assert!(glyphs.iter().all(|glyph| glyph.font_id == bold_italic));
    }

    /// A document read from a damaged file, with bold running past the end of its text, a note
    /// starting inside "é" and a footnote past the end, is drawn as it fits.
    #[test]
    fn a_damaged_document_is_drawn_as_it_fits() {
        let mut doc = crate::model::Document::new("Damaged");
        let mut block = paragraph("Café au lait");
        block.spans = vec![Span {
            start: 6,
            end: 40,
            style: SpanStyle::Bold,
        }];
        let range = |kind, start, end| crate::model::Annotation {
            id: Uuid::new_v4(),
            kind,
            anchor: crate::model::Anchor::range(block.id, start, end),
            body: String::new(),
        };
        doc.annotations = vec![
            range(AnnotationKind::Note, 4, 8),
            range(AnnotationKind::Footnote, 40, 40),
        ];
        doc.blocks = vec![block];
        let mut db = kame_core::storage::Db::open_in_memory().unwrap();
        db.save_document(&doc).unwrap();
        let doc = db.load_document().unwrap();
        let block = &doc.blocks[0];
        let anchor = |kind| {
            doc.annotations
                .iter()
                .find(|annotation| annotation.kind == kind)
                .and_then(|annotation| annotation.anchor.byte_range())
                .unwrap()
        };
        let (start, end) = anchor(AnnotationKind::Note);
        assert_eq!((start, end), (3, 8));
        let mark = anchor(AnnotationKind::Footnote).1;
        assert_eq!(mark, block.text.len());

        let mut renderer = GlyphRenderer::new(config());
        let layout = renderer.layout_marked(block, &[mark]);
        let bold = face(&renderer, "EB Garamond", Weight::BOLD, Style::Normal);
        let glyphs: Vec<_> = layout
            .runs
            .iter()
            .flat_map(|run| {
                let base = layout.line_start(run.line_i);
                run.glyphs
                    .iter()
                    .map(move |glyph| (base + glyph.start, glyph))
            })
            .collect();
        assert_eq!(glyphs.len(), "Café au lait".chars().count());
        for (offset, glyph) in glyphs {
            assert_eq!(glyph.font_id == bold, offset >= 6, "at {offset}");
        }
        let (image, _, _) = renderer.rasterize(
            &layout,
            [0, 0, 0],
            [255, 255, 255],
            &[(start, end, [250, 240, 200])],
            2.0,
        );
        assert!(image.size[0] > 0 && image.size[1] > 0);
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
            &Wanted::all(blocks.len()),
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
                &Wanted::all(blocks.len()),
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
            &Wanted::all(blocks.len()),
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
            &Wanted::all(blocks.len()),
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
            &Wanted::all(blocks.len()),
        );
        assert_eq!(cache.len(), 1, "the removed block's texture is dropped");
    }

    /// A letter typed in a long paragraph changes some of its lines, and only the rows of those
    /// are uploaded, into the texture it has. Laid over the image first uploaded, they make the
    /// image drawing it whole gives.
    #[test]
    fn a_letter_typed_updates_only_part_of_its_paragraphs_texture() {
        let ctx = Context::default();
        let mut renderer = GlyphRenderer::new(config());
        let mut blocks = vec![paragraph(
            &"Every machine that writes for us is a promise about who does the thinking, and a \
              promise is a thing that can be kept or broken quietly. "
                .repeat(4),
        )];
        let mut cache = TextureCache::new();
        let sync = |cache: &mut TextureCache, renderer: &mut GlyphRenderer, blocks: &[Block]| {
            let layouts: Vec<BlockLayout> = blocks.iter().map(|b| renderer.layout(b)).collect();
            let refs: Vec<&BlockLayout> = layouts.iter().collect();
            cache.sync(
                &ctx,
                renderer,
                blocks,
                &refs,
                [10, 10, 10],
                [255, 255, 255],
                &HashMap::new(),
                &Wanted::all(blocks.len()),
            );
            // No backend applies these uploads in a headless test.
            let mut delta = ctx.tex_manager().write().take_delta();
            let uploads: HashMap<egui::TextureId, Vec<egui::epaint::ImageDelta>> = delta
                .set
                .drain()
                .map(|(id, deltas)| (id, deltas.into_vec()))
                .collect();
            delta.clear();
            (layouts, uploads)
        };
        let (_, delta) = sync(&mut cache, &mut renderer, &blocks);
        let id = cache.get(blocks[0].id).unwrap().texture.id();
        let [whole] = delta[&id].as_slice() else {
            panic!("one upload of the new texture");
        };
        assert!(whole.is_whole());
        let egui::ImageData::Color(image) = &whole.image;
        let mut image = (**image).clone();

        let at = blocks[0].text.len() / 2;
        blocks[0].text.insert(at, 'x');
        let (layouts, delta) = sync(&mut cache, &mut renderer, &blocks);
        let texture = cache.get(blocks[0].id).unwrap();
        assert_eq!(texture.texture.id(), id, "the texture is kept");
        let parts = &delta[&id];
        let mut rows = 0;
        for part in parts {
            let [0, top] = part.pos.expect("only part is uploaded") else {
                panic!("a band starts at the left edge");
            };
            let egui::ImageData::Color(band) = &part.image;
            assert_eq!(band.size[0], image.size[0]);
            let start = top * image.size[0];
            image.pixels[start..start + band.pixels.len()].copy_from_slice(&band.pixels);
            rows += band.size[1];
        }
        assert!(rows < image.size[1] / 2, "{rows} of {} rows", image.size[1]);
        let (drawn, left, top) =
            renderer.rasterize(&layouts[0], [10, 10, 10], [255, 255, 255], &[], 1.0);
        assert_eq!((left, top), (texture.left, texture.top));
        assert!(
            drawn.pixels == image.pixels,
            "the bands make the whole image"
        );
    }

    #[test]
    fn texture_cache_draws_only_the_blocks_wanted_and_keeps_only_those_near() {
        let ctx = Context::default();
        let mut renderer = GlyphRenderer::new(config());
        let blocks: Vec<Block> = (0..6).map(|n| paragraph(&format!("Block {n}"))).collect();
        let layouts: Vec<BlockLayout> = blocks.iter().map(|b| renderer.layout(b)).collect();
        let refs: Vec<&BlockLayout> = layouts.iter().collect();
        let mut cache = TextureCache::new();
        let mut sync = |cache: &mut TextureCache, draw, keep| {
            cache.sync(
                &ctx,
                &mut renderer,
                &blocks,
                &refs,
                [0, 0, 0],
                [255, 255, 255],
                &HashMap::new(),
                &Wanted { draw, keep },
            );
        };
        let held = |cache: &TextureCache| -> Vec<usize> {
            (0..blocks.len())
                .filter(|at| cache.get(blocks[*at].id).is_some())
                .collect()
        };

        sync(&mut cache, 2..4, 1..5);
        assert_eq!(held(&cache), [2, 3]);

        sync(&mut cache, 4..6, 3..6);
        assert_eq!(
            held(&cache),
            [3, 4, 5],
            "2 has gone too far, and 3 is still near"
        );

        sync(&mut cache, 0..1, 0..6);
        assert_eq!(held(&cache), [0, 3, 4, 5]);
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

    /// A block found current at a revision isn't looked at again until the revision changes, or
    /// the cache is cleared.
    #[test]
    fn layout_cache_looks_at_nothing_again_at_the_same_revision() {
        let mut renderer = GlyphRenderer::new(config());
        let mut blocks = vec![paragraph("Hello"), paragraph("World")];
        let marks = HashMap::new();
        let mut cache = LayoutCache::new();

        cache.sync_marked(&mut renderer, &blocks, &marks, Some(1));
        assert_eq!(cache.fingerprints, 2);
        assert_eq!(renderer.layout_calls(), 2);
        cache.sync_marked(&mut renderer, &blocks, &marks, Some(1));
        assert_eq!(cache.fingerprints, 2, "the same revision looks at no block");

        // At the same revision the blocks are promised to be the same, so a change isn't seen.
        blocks[0].text = "Hello, much longer line".to_string();
        let layouts = cache.sync_marked(&mut renderer, &blocks, &marks, Some(1));
        assert_eq!(layouts[0].lines.last().unwrap().end, "Hello".len());
        assert_eq!(renderer.layout_calls(), 2);

        let layouts = cache.sync_marked(&mut renderer, &blocks, &marks, Some(2));
        assert_eq!(
            layouts[0].lines.last().unwrap().end,
            "Hello, much longer line".len()
        );
        assert_eq!(cache.fingerprints, 4, "a new revision looks at every block");
        assert_eq!(renderer.layout_calls(), 3, "and shapes only what changed");

        cache.sync_marked(&mut renderer, &blocks, &marks, None);
        assert_eq!(cache.fingerprints, 6, "no revision looks at every block");
        cache.sync_marked(&mut renderer, &blocks, &marks, None);
        assert_eq!(cache.fingerprints, 8, "every time");

        cache.clear();
        cache.sync_marked(&mut renderer, &blocks, &marks, Some(2));
        assert_eq!(renderer.layout_calls(), 5, "a cleared cache shapes again");
    }

    /// A paragraph and a measure where a line ends in a drawn hyphen, and that line's index.
    fn hyphenated() -> (Block, BlockLayout, usize) {
        let text = "Characteristically unexceptionable compositions accommodate extraordinarily \
disproportionate considerations notwithstanding misunderstandings about internationalization \
while straightforward arrangements occasionally demonstrate remarkably counterintuitive properties.";
        let block = paragraph(text);
        for width in (200..500).step_by(5) {
            let mut renderer = GlyphRenderer::new(LayoutConfig {
                body_width: width as f32,
                hyphenate: true,
                ..config()
            });
            let layout = renderer.layout(&block);
            let found = layout
                .runs
                .iter()
                .position(|run| run.glyphs.last().is_some_and(|g| g.start == g.end));
            if let Some(index) = found {
                return (block, layout, index);
            }
        }
        panic!("no width gave a hyphen");
    }

    #[test]
    fn a_caret_at_a_hyphen_sits_at_the_start_of_the_next_line() {
        let (_, layout, index) = hyphenated();
        let offset = layout.lines[index].end;
        assert_eq!(layout.lines[index + 1].start, offset);
        let line = caret_line(&layout, offset).unwrap();
        assert_eq!(line.y, layout.lines[index + 1].y);
        assert_eq!(caret_x(&layout, offset), layout.runs[index + 1].glyphs[0].x);
    }

    #[test]
    fn a_selection_across_a_hyphen_covers_it() {
        let (_, layout, index) = hyphenated();
        let offset = layout.lines[index].end;
        let rects = range_rects(&layout, offset - 2, offset + 2);
        assert_eq!(rects.len(), 2, "{rects:?}");
        let hyphen = layout.runs[index].glyphs.last().unwrap();
        assert!((rects[0].right() - (hyphen.x + hyphen.w)).abs() < 0.01);
        assert_eq!(rects[0].top(), layout.lines[index].y);
        assert_eq!(rects[1].top(), layout.lines[index + 1].y);
    }

    #[test]
    fn a_click_on_a_hyphen_lands_at_the_break() {
        let (block, layout, index) = hyphenated();
        let hyphen = layout.runs[index].glyphs.last().unwrap();
        let offset = offset_at(
            &layout,
            hyphen.x + hyphen.w * 0.5,
            layout.lines[index].y + 1.0,
            block.text.len(),
        );
        assert_eq!(offset, layout.lines[index].end);
    }

    #[test]
    fn a_line_wrapped_after_a_typed_hyphen_keeps_the_caret_on_the_next_line() {
        let block = paragraph("aaaaaaaa-bbbbbbbb-cccccccc-dddddddd-eeeeeeee-ffffffff-gggggggg");
        let mut renderer = GlyphRenderer::new(LayoutConfig {
            body_width: 150.0,
            ..config()
        });
        let layout = renderer.layout(&block);
        assert!(layout.lines.len() >= 2);
        let offset = layout.lines[0].end;
        assert_eq!(layout.lines[1].start, offset);
        assert_eq!(caret_line(&layout, offset).unwrap().y, layout.lines[1].y);
        assert_eq!(caret_x(&layout, offset), layout.runs[1].glyphs[0].x);
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

        let first_line = layout.runs.first().unwrap();
        let first_glyph = first_line.glyphs.first().unwrap();
        assert_eq!(caret_x(&layout, 0), first_glyph.x);

        let second = layout.runs.get(1).unwrap();
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
        let run = layout.runs.first().unwrap();

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
        let run = layout.runs.first().unwrap();
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
        let second = layout.runs.get(1).unwrap();
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
        // In the last word there's no next one to land on, so it's that word's end,
        // past any punctuation and short of any spaces.
        assert_eq!(next_word(text, 16), Some(19));
        assert_eq!(next_word(text, 17), Some(19));
        assert_eq!(next_word("fox.", 0), Some(4), "it goes past the period");
        assert_eq!(next_word("fox.", 3), Some(4), "and from just before it");
        assert_eq!(next_word("fox.", 4), None);
        assert_eq!(
            next_word("fox. \u{a0}", 0),
            Some(4),
            "it stops short of spaces"
        );
        assert_eq!(next_word("fox. ", 4), None);
        assert_eq!(next_word("  ", 0), None, "only spaces is nowhere to stop");
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
            command_for_key(Key::PageUp, none),
            Some(Command::MovePageUp)
        );
        assert_eq!(
            command_for_key(Key::PageDown, none),
            Some(Command::MovePageDown)
        );
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
            command_for_key(Key::PageUp, shift),
            Some(Command::SelectPageUp)
        );
        assert_eq!(
            command_for_key(Key::PageDown, shift),
            Some(Command::SelectPageDown)
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
            command_for_key(Key::M, ctrl_shift),
            Some(Command::AddAnnotation(AnnotationKind::Marginal))
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
    fn ctrl_shift_up_and_down_move_a_list_item() {
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            command_for_key(Key::ArrowUp, ctrl_shift),
            Some(Command::MoveListItem { up: true })
        );
        assert_eq!(
            command_for_key(Key::ArrowDown, ctrl_shift),
            Some(Command::MoveListItem { up: false })
        );
        // Cmd on a Mac.
        let cmd_shift = Modifiers {
            mac_cmd: true,
            shift: true,
            command: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            command_for_key(Key::ArrowUp, cmd_shift),
            Some(Command::MoveListItem { up: true })
        );
        let ctrl = Modifiers {
            shift: false,
            ..ctrl_shift
        };
        assert_eq!(command_for_key(Key::ArrowUp, ctrl), None);
        assert_eq!(command_for_key(Key::ArrowDown, ctrl), None);
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
            Some(Command::SetBlockKind(BlockKind::Heading {
                level: 1,
                centered: false
            }))
        );
        assert_eq!(
            command_for_key(Key::Num2, ctrl_shift),
            Some(Command::SetBlockKind(BlockKind::Heading {
                level: 2,
                centered: false
            }))
        );
        assert_eq!(
            command_for_key(Key::Num3, ctrl_shift),
            Some(Command::SetBlockKind(BlockKind::Heading {
                level: 3,
                centered: false
            }))
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
            (
                "!",
                Key::Num1,
                BlockKind::Heading {
                    level: 1,
                    centered: false,
                },
            ),
            (
                "@",
                Key::Num2,
                BlockKind::Heading {
                    level: 2,
                    centered: false,
                },
            ),
            (
                "#",
                Key::Num3,
                BlockKind::Heading {
                    level: 3,
                    centered: false,
                },
            ),
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
            Some(Command::SetBlockKind(BlockKind::Heading {
                level: 1,
                centered: false
            })),
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

    fn text_event(text: &str) -> Event {
        Event::Text(text.to_string())
    }

    fn key_event(key: Key, pressed: bool) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn alt_arrows_move_and_select_by_word() {
        let alt = Modifiers::ALT;
        let alt_shift = Modifiers { shift: true, ..alt };
        assert_eq!(
            command_for_key(Key::ArrowLeft, alt),
            Some(Command::MoveWordLeft)
        );
        assert_eq!(
            command_for_key(Key::ArrowRight, alt),
            Some(Command::MoveWordRight)
        );
        assert_eq!(
            command_for_key(Key::ArrowLeft, alt_shift),
            Some(Command::SelectWordLeft)
        );
        assert_eq!(
            command_for_key(Key::ArrowRight, alt_shift),
            Some(Command::SelectWordRight)
        );
    }

    #[test]
    fn a_finished_composition_is_typed_and_one_in_progress_is_not() {
        use egui::ImeEvent;
        assert_eq!(
            command_for_event(&Event::Ime(ImeEvent::Commit("é".to_string()))),
            Some(Command::Insert("é".to_string()))
        );
        let composing = ImeEvent::Preedit {
            text: "´".to_string(),
            active_range_chars: None,
        };
        assert_eq!(command_for_event(&Event::Ime(composing)), None);
        assert_eq!(
            command_for_event(&Event::Ime(ImeEvent::Commit(String::new()))),
            None
        );
    }

    #[test]
    fn ctrl_backspace_deletes_a_word_as_ctrl_moves_by_one() {
        // Ctrl on Linux, and Cmd on a Mac, are both the command key here.
        for modifiers in [
            Modifiers::CTRL | Modifiers::COMMAND,
            Modifiers::MAC_CMD | Modifiers::COMMAND,
        ] {
            assert_eq!(
                command_for_key(Key::Backspace, modifiers),
                Some(Command::DeleteWordBack)
            );
        }
    }

    #[test]
    fn alt_backspace_deletes_a_word_and_plain_backspace_does_not() {
        assert_eq!(
            command_for_key(Key::Backspace, Modifiers::ALT),
            Some(Command::DeleteWordBack)
        );
        assert_eq!(
            command_for_key(Key::Backspace, Modifiers::NONE),
            Some(Command::Backspace)
        );
    }

    #[test]
    fn escape_then_angle_brackets_go_to_the_document_edges() {
        let now = std::time::Instant::now();
        let mut prefix = EscapePrefix::default();
        prefix.arm(now);
        // Shift-comma arrives as a key the surface ignores, then as text.
        assert_eq!(prefix.command_for(&key_event(Key::Comma, true), now), None);
        assert_eq!(
            prefix.command_for(&text_event("<"), now),
            Some(Command::MoveDocumentStart)
        );
        prefix.arm(now);
        assert_eq!(
            prefix.command_for(&text_event(">"), now),
            Some(Command::MoveDocumentEnd)
        );
    }

    #[test]
    fn escape_then_backspace_deletes_a_word() {
        let now = std::time::Instant::now();
        let mut prefix = EscapePrefix::default();
        prefix.arm(now);
        assert_eq!(
            prefix.command_for(&key_event(Key::Backspace, true), now),
            Some(Command::DeleteWordBack)
        );
    }

    #[test]
    fn the_escape_prefix_is_used_up_by_one_key() {
        let now = std::time::Instant::now();
        let mut prefix = EscapePrefix::default();
        prefix.arm(now);
        assert!(prefix.command_for(&text_event("<"), now).is_some());
        assert_eq!(prefix.command_for(&text_event("<"), now), None);
        prefix.arm(now);
        assert_eq!(prefix.command_for(&text_event("a"), now), None);
        assert_eq!(prefix.command_for(&text_event(">"), now), None);
        prefix.arm(now);
        assert_eq!(
            prefix.command_for(&key_event(Key::ArrowLeft, true), now),
            None
        );
        assert_eq!(prefix.command_for(&text_event("<"), now), None);
    }

    #[test]
    fn angle_brackets_are_only_text_without_a_recent_escape() {
        let now = std::time::Instant::now();
        let mut prefix = EscapePrefix::default();
        assert_eq!(prefix.command_for(&text_event("<"), now), None);
        prefix.arm(now);
        let later = now + ESCAPE_PREFIX_WINDOW + std::time::Duration::from_millis(1);
        assert_eq!(prefix.command_for(&text_event(">"), later), None);
        assert_eq!(
            prefix.command_for(&key_event(Key::Backspace, true), later),
            None
        );
    }

    #[test]
    fn an_empty_block_has_no_line_to_step_to() {
        let mut renderer = GlyphRenderer::new(config());
        let layout = renderer.layout(&paragraph(""));
        assert!(adjacent_line(&layout, 0, true).is_none());
        assert!(adjacent_line(&layout, 0, false).is_none());
    }

    #[test]
    fn stepping_between_lines_stops_at_the_block_edges() {
        let mut renderer = GlyphRenderer::new(LayoutConfig {
            body_width: 160.0,
            ..config()
        });
        let block = paragraph(
            "Typography is the craft of endowing human language with a durable visual form.",
        );
        let layout = renderer.layout(&block);
        assert!(layout.lines.len() >= 3, "expected a wrapped paragraph");
        assert!(adjacent_line(&layout, 0, false).is_none());
        assert_eq!(
            adjacent_line(&layout, 0, true).unwrap().start,
            layout.lines[1].start
        );
        assert!(adjacent_line(&layout, block.text.len(), true).is_none());
    }

    #[test]
    fn a_tab_in_code_advances_four_columns() {
        let mut renderer = GlyphRenderer::new(config());
        let code = |text: &str| Block {
            kind: BlockKind::Code { language: None },
            ..paragraph(text)
        };
        let tab = renderer.layout(&code("\tx"));
        let spaces = renderer.layout(&code("    x"));
        let (with_tab, with_spaces) = (caret_x(&tab, 1), caret_x(&spaces, 4));
        assert!(
            (with_tab - with_spaces).abs() < 0.5,
            "{with_tab} against {with_spaces}"
        );
    }

    #[test]
    fn copy_and_cut_events_are_commands() {
        assert_eq!(command_for_event(&Event::Copy), Some(Command::Copy));
        assert_eq!(command_for_event(&Event::Cut), Some(Command::Cut));
    }
}
