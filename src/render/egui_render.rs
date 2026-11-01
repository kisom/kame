//! egui presentation: fonts, glyph textures, caret geometry, and the keyboard
//! map from egui events to editor commands.
//!
//! Nothing here owns a window. The renderer works off a [`FontSystem`], a
//! [`LayoutConfig`] and an egui [`Context`], so the texture cache and the pure
//! geometry/command helpers can be exercised headlessly in tests.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use cosmic_text::FontSystem;
use egui::{ColorImage, Context, Event, Key, Modifiers, Pos2, Rect, TextureHandle, TextureOptions};
use image::RgbaImage;

use crate::layout::{layout_block, BlockLayout, LayoutConfig};
use crate::model::{Block, BlockId};
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

/// How many block rasterizations may run before the swash mask cache is
/// dropped. `SwashCache` never evicts on its own, so without this the cache
/// grows for the lifetime of the process.
const RASTERIZES_PER_CACHE_CLEAR: u32 = 512;

/// A block's uploaded glyph texture plus the ink offset returned by
/// [`GlyphAtlas::rasterize`].
///
/// The image is cropped to the glyph ink, so it must be drawn at
/// `block_origin + (left, top)` — never at the block origin itself.
pub struct BlockTexture {
    pub texture: TextureHandle,
    pub left: i32,
    pub top: i32,
}

impl BlockTexture {
    /// Screen-space quad for a block whose layout origin (the top-left of its
    /// layout box, Y down) is `origin`.
    pub fn rect(&self, origin: Pos2) -> Rect {
        Rect::from_min_size(
            Pos2::new(origin.x + self.left as f32, origin.y + self.top as f32),
            self.texture.size_vec2(),
        )
    }
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

    /// Rasterizes and uploads every block whose content changed, and drops
    /// textures of blocks no longer present.
    ///
    /// `blocks` and `layouts` are parallel, as produced by
    /// [`GlyphRenderer::layout`]. Uploads use
    /// [`ColorImage::from_rgba_unmultiplied`] because the rasterizer emits
    /// straight (unpremultiplied) alpha, and [`TextureOptions::NEAREST`] so
    /// glyph edges stay crisp at 1:1 scale.
    pub fn sync(
        &mut self,
        ctx: &Context,
        renderer: &mut GlyphRenderer,
        blocks: &[Block],
        layouts: &[BlockLayout],
        color: [u8; 3],
    ) {
        let mut live = HashSet::with_capacity(blocks.len());
        for (block, layout) in blocks.iter().zip(layouts) {
            live.insert(block.id);
            let fingerprint = block_fingerprint(block);
            let up_to_date = self
                .entries
                .get(&block.id)
                .is_some_and(|cached| cached.fingerprint == fingerprint);
            if up_to_date {
                continue;
            }

            let (image, left, top) = renderer.rasterize(layout, color);
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
                    block: BlockTexture { texture, left, top },
                },
            );
        }
        self.entries.retain(|id, _| live.contains(id));
    }

    pub fn get(&self, block_id: BlockId) -> Option<&BlockTexture> {
        self.entries.get(&block_id).map(|cached| &cached.block)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Content hash used to decide whether a block's texture is stale.
fn block_fingerprint(block: &Block) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", block.kind).hash(&mut hasher);
    block.text.hash(&mut hasher);
    for span in &block.spans {
        span.start.hash(&mut hasher);
        span.end.hash(&mut hasher);
        format!("{:?}", span.style).hash(&mut hasher);
    }
    hasher.finish()
}

/// Owns the runtime [`FontSystem`] and the glyph atlas.
pub struct GlyphRenderer {
    font_system: FontSystem,
    atlas: GlyphAtlas,
    config: LayoutConfig,
    rasterizes_since_clear: u32,
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
        }
    }

    pub fn config(&self) -> LayoutConfig {
        self.config
    }

    /// Shapes one block with the configured column width. Callers keep the
    /// returned layouts for the frame to map carets and clicks.
    pub fn layout(&mut self, block: &Block) -> BlockLayout {
        layout_block(&mut self.font_system, block, &self.config)
    }

    /// Paints `layout` into an ink-cropped RGBA image and returns it with its
    /// block-relative `(left, top)` offset.
    pub fn rasterize(&mut self, layout: &BlockLayout, color: [u8; 3]) -> (RgbaImage, i32, i32) {
        self.rasterizes_since_clear += 1;
        if self.rasterizes_since_clear >= RASTERIZES_PER_CACHE_CLEAR {
            self.atlas.clear_cache();
            self.rasterizes_since_clear = 0;
        }
        self.atlas.rasterize(&mut self.font_system, layout, color)
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
        if offset < first.start || offset > last.end {
            continue;
        }
        if offset == last.end {
            return last.x + last.w;
        }
        for glyph in run.glyphs.iter() {
            if offset >= glyph.start && offset < glyph.end {
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
    layout
        .lines
        .iter()
        .find(|line| offset >= line.start && offset < line.end)
        .or_else(|| layout.lines.iter().find(|line| line.start > offset))
        .or_else(|| layout.lines.last())
        .copied()
}

/// The byte offset in a block closest to block-relative point (`x`, `y`).
///
/// The nearest visual line is chosen by vertical distance, then the nearest
/// glyph boundary on that line. An empty layout (or a blank line) maps to 0.
pub fn offset_at(layout: &BlockLayout, x: f32, y: f32, text_len: usize) -> usize {
    let mut best: Option<(f32, usize)> = None;
    for run in layout.buffer.layout_runs() {
        let Some(first) = run.glyphs.first() else {
            continue;
        };
        let centre = run.line_top + run.line_height * 0.5;
        let distance = (y - centre).abs();
        if best.is_some_and(|(best_distance, _)| best_distance <= distance) {
            continue;
        }

        let mut candidates: Vec<(f32, usize)> = Vec::with_capacity(run.glyphs.len() + 1);
        candidates.push((first.x, first.start));
        for glyph in run.glyphs.iter() {
            candidates.push((glyph.x + glyph.w, glyph.end));
        }
        let mut offset = first.start;
        let mut nearest = f32::INFINITY;
        for (pen, candidate) in candidates {
            let dx = (x - pen).abs();
            if dx < nearest {
                nearest = dx;
                offset = candidate;
            }
        }
        best = Some((distance, offset));
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
            pressed: true,
            modifiers,
            ..
        } => command_for_key(*key, *modifiers),
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
        Key::Enter => Some(Command::SplitParagraph),
        Key::Backspace => Some(Command::Backspace),
        Key::Delete => Some(Command::Delete),
        Key::ArrowLeft if !command => Some(Command::MoveLeft),
        Key::ArrowRight if !command => Some(Command::MoveRight),
        Key::ArrowUp if !command => Some(Command::MoveUp),
        Key::ArrowDown if !command => Some(Command::MoveDown),
        Key::Home if !command => Some(Command::MoveLineStart),
        Key::End if !command => Some(Command::MoveLineEnd),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BlockKind;
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

    /// The runtime renderer must shape a heading with Crimson Pro, not fall
    /// back to the body face.
    #[test]
    fn runtime_headings_shape_with_crimson_pro() {
        let mut renderer = GlyphRenderer::new(config());
        let crimson = face(&renderer, "Crimson Pro", Weight::NORMAL, Style::Normal);
        let garamond = face(&renderer, "EB Garamond", Weight::NORMAL, Style::Normal);
        let block = heading("On the Shape of Letters");
        let layout = renderer.layout(&block);
        let run = layout.buffer.layout_runs().next().expect("one shaped run");
        assert!(!run.glyphs.is_empty());
        assert!(
            run.glyphs.iter().all(|glyph| glyph.font_id == crimson),
            "heading glyphs did not use the Crimson Pro face"
        );
        assert!(
            run.glyphs.iter().all(|glyph| glyph.font_id != garamond),
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

        cache.sync(&ctx, &mut renderer, &blocks, &layouts, [10, 10, 10]);
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

    #[test]
    fn texture_cache_replaces_changed_blocks_and_drops_removed_ones() {
        let ctx = Context::default();
        let mut renderer = GlyphRenderer::new(config());
        let mut blocks = vec![paragraph("Hello"), paragraph("World")];
        let mut layouts: Vec<BlockLayout> = blocks.iter().map(|b| renderer.layout(b)).collect();
        let mut cache = TextureCache::new();
        cache.sync(&ctx, &mut renderer, &blocks, &layouts, [0, 0, 0]);
        let first_size = cache.get(blocks[0].id).unwrap().texture.size();

        blocks[0].text = "Hello, much longer line".to_string();
        layouts = blocks.iter().map(|b| renderer.layout(b)).collect();
        cache.sync(&ctx, &mut renderer, &blocks, &layouts, [0, 0, 0]);
        assert_ne!(
            cache.get(blocks[0].id).unwrap().texture.size(),
            first_size,
            "a changed block must be re-rasterized"
        );

        blocks.pop();
        layouts = blocks.iter().map(|b| renderer.layout(b)).collect();
        cache.sync(&ctx, &mut renderer, &blocks, &layouts, [0, 0, 0]);
        assert_eq!(cache.len(), 1, "the removed block's texture is dropped");
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
}
