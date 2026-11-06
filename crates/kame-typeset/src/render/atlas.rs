//! Glyph rasterization: cosmic-text runs painted with swash into egui's premultiplied image.

use std::collections::HashMap;

use crate::layout::BlockLayout;
use cosmic_text::{
    CacheKey, CacheKeyFlags, FontSystem, LayoutGlyph, PhysicalGlyph, SwashContent, SwashImage,
};
use egui::{Color32, ColorImage};
#[cfg(test)]
use image::RgbaImage;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Transform, Vector};

/// Display density, in physical pixels per point, from which glyphs are
/// antialiased in grayscale rather than LCD subpixels.
///
/// Below it, LCD coverage buys real horizontal resolution. From here up the
/// pixels are fine enough that stems stay crisp without it, and the subpixel
/// colour only shows as fringing.
const GRAYSCALE_MIN_SCALE: f32 = 2.0;

/// The mask format glyphs are rasterized in at `scale`.
fn mask_format(scale: f32) -> Format {
    if scale >= GRAYSCALE_MIN_SCALE {
        Format::Alpha
    } else {
        Format::Subpixel
    }
}

/// Glyph mask cache and the RGBA canvas painted by [`GlyphAtlas::rasterize`].
///
/// Unlike cosmic-text's `SwashCache`, the mask format follows the display
/// density (see [`mask_format`]). A [`CacheKey`] already includes the size and
/// subpixel bin but not the format, and one physical size can be reached at
/// either density, so the format is part of the cache key too.
pub struct GlyphAtlas {
    context: ScaleContext,
    cache: HashMap<(CacheKey, bool), Option<SwashImage>>,
}

impl Default for GlyphAtlas {
    fn default() -> Self {
        Self::new()
    }
}

impl GlyphAtlas {
    pub fn new() -> Self {
        GlyphAtlas {
            context: ScaleContext::new(),
            cache: HashMap::new(),
        }
    }

    /// Drops every cached glyph mask.
    ///
    /// The cache never evicts entries on its own, so a long-lived process that
    /// keeps arranging new text would grow without bound. Callers clear it
    /// periodically; the masks are cheap to regenerate. The `ScaleContext` is
    /// reused — its scratch is bounded per glyph, not a growing cache.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Rasterizes one glyph for `scale` (or returns the cached image).
    fn get_image(
        &mut self,
        fs: &mut FontSystem,
        cache_key: CacheKey,
        scale: f32,
    ) -> Option<&SwashImage> {
        let format = mask_format(scale);
        let Self { context, cache } = self;
        cache
            .entry((cache_key, matches!(format, Format::Subpixel)))
            .or_insert_with(|| rasterize_mask(fs, context, cache_key, format))
            .as_ref()
    }

    /// Rasterize `layout` into an image for egui, with premultiplied alpha, returning the image
    /// together with its block-relative offset `(left, top)`.
    ///
    /// Glyphs are painted in `ink` over `background` (the paper colour). LCD
    /// coverage is composited per-channel so the colour fringing is correct; a
    /// grayscale mask needs no background. `highlights` lists byte ranges whose
    /// background is a note wash instead of the paper, so highlighted prose
    /// keeps its tint behind the ink; each entry is `(start, end, background)`.
    ///
    /// The canvas is cropped to the ink of the shaped glyphs, so its size is
    /// the glyph bounding box rather than the full layout column; a block with
    /// no drawable glyphs (empty text, or spaces only) yields a 1×1 transparent
    /// image at offset `(0, 0)` so callers can always upload a texture.
    ///
    /// `scale` is the display's physical pixels per layout unit. The layout stays
    /// in logical units, but the glyphs are rasterized at physical size, so the
    /// image and the returned offset are in physical pixels and the image must
    /// be drawn 1:1 with the screen (see `BlockTexture::rect`). A mask made at
    /// any other size has to be resampled: nearest-sampled at 1.5, every third
    /// row and column doubles and the stems come out uneven.
    ///
    /// Glyph positions come from [`place_glyph`].
    ///
    /// Glyphs are composited with straight alpha, as each needs what's under it, and each pixel
    /// painted is written to the image in egui's form as it is, so the image needs no pass of its
    /// own to convert.
    pub fn rasterize_premultiplied(
        &mut self,
        fs: &mut FontSystem,
        layout: &BlockLayout,
        ink: [u8; 3],
        background: [u8; 3],
        highlights: &[(usize, usize, [u8; 3])],
        scale: f32,
    ) -> (ColorImage, i32, i32) {
        let painting = self.painting(fs, layout, ink, background, highlights, scale);
        self.draw_premultiplied(fs, &painting)
    }

    /// [`GlyphAtlas::rasterize_premultiplied`] with straight alpha, the image it converts.
    #[cfg(test)]
    pub fn rasterize(
        &mut self,
        fs: &mut FontSystem,
        layout: &BlockLayout,
        ink: [u8; 3],
        background: [u8; 3],
        highlights: &[(usize, usize, [u8; 3])],
        scale: f32,
    ) -> (RgbaImage, i32, i32) {
        let painting = self.painting(fs, layout, ink, background, highlights, scale);
        self.draw_into(fs, &painting, RgbaImage::new)
            .unwrap_or_else(|| (RgbaImage::new(1, 1), 0, 0))
    }

    /// The image of `painting`, as [`GlyphAtlas::rasterize_premultiplied`] gives it.
    pub fn draw_premultiplied(
        &mut self,
        fs: &mut FontSystem,
        painting: &Painting,
    ) -> (ColorImage, i32, i32) {
        match self.draw_into(fs, painting, Premultiplied::new) {
            Some((canvas, left, top)) => (canvas.image, left, top),
            None => (ColorImage::filled([1, 1], Color32::TRANSPARENT), 0, 0),
        }
    }

    /// Paints `painting` into a canvas `make` makes at the size of its ink, or `None` if it has
    /// no ink.
    fn draw_into<C: Canvas>(
        &mut self,
        fs: &mut FontSystem,
        painting: &Painting,
        make: impl FnOnce(u32, u32) -> C,
    ) -> Option<(C, i32, i32)> {
        let (min_x, min_y, max_x, max_y) = painting.bounds?;
        let width = (max_x - min_x).max(1) as u32;
        let height = (max_y - min_y).max(1) as u32;
        let mut canvas = make(width, height);
        self.paint(
            fs,
            painting.lines.iter().flatten(),
            &mut canvas,
            (min_x, min_y),
            painting.scale,
        );
        Some((canvas, min_x, min_y))
    }

    /// What `layout` paints: every glyph mask, where it lands and in what colours, a line at a
    /// time, and the bounds of their ink. Glyphs whose byte range falls in one of `highlights`
    /// are painted over that wash colour instead of the paper.
    pub fn painting(
        &mut self,
        fs: &mut FontSystem,
        layout: &BlockLayout,
        ink: [u8; 3],
        background: [u8; 3],
        highlights: &[(usize, usize, [u8; 3])],
        scale: f32,
    ) -> Painting {
        let mut lines: Vec<Vec<Mark>> = Vec::with_capacity(layout.runs.len());
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        // Each run is one line as it is drawn.
        for run in &layout.runs {
            let mut line = Vec::with_capacity(run.glyphs.len());
            let base = layout.line_start(run.line_i);
            // `glyph.y` is line-relative: the run's baseline is the anchor.
            for glyph in run.glyphs.iter() {
                let phys = place_glyph(glyph, run.line_y, scale);
                let Some(image) = self.get_image(fs, phys.cache_key, scale) else {
                    continue;
                };
                if image.placement.width == 0 || image.placement.height == 0 {
                    continue;
                }
                let mark = Mark {
                    key: phys.cache_key,
                    left: phys.x + image.placement.left,
                    top: phys.y - image.placement.top,
                    width: image.placement.width,
                    height: image.placement.height,
                    // A glyph shaped with an explicit colour (a link) keeps it;
                    // everything else takes the caller's ink.
                    ink: glyph.color_opt.map_or(ink, |c| [c.r(), c.g(), c.b()]),
                    // A note/marginal/footnote wash behind this glyph becomes the
                    // background the subpixel coverage is baked against.
                    background: highlights
                        .iter()
                        .find(|(start, end, _)| {
                            base + glyph.start < *end && *start < base + glyph.end
                        })
                        .map(|(_, _, color)| *color)
                        .unwrap_or(background),
                };
                let (right, bottom) = mark.right_bottom();
                bounds = Some(match bounds {
                    None => (mark.left, mark.top, right, bottom),
                    Some((min_x, min_y, max_x, max_y)) => (
                        min_x.min(mark.left),
                        min_y.min(mark.top),
                        max_x.max(right),
                        max_y.max(bottom),
                    ),
                });
                line.push(mark);
            }
            lines.push(line);
        }
        Painting {
            lines,
            bounds,
            scale,
        }
    }

    /// The bands of `before`'s image to paint again to make it `after`'s, or `None` if the
    /// image must be drawn whole: when the two differ in their scale, the bounds of their ink or
    /// their number of lines, or nothing is drawn.
    ///
    /// Each pixel is what the masks over it make, painted in order, so where both paint the
    /// same masks in the same order the pixels are the same. Those differ only in the rows where
    /// a line's masks differ, before or after, and each band takes in every mask that reaches
    /// it, from whichever line, so a descender or an accent from the next line is painted too.
    pub fn redraw(
        &mut self,
        fs: &mut FontSystem,
        before: &Painting,
        after: &Painting,
    ) -> Option<Vec<Patch>> {
        let (min_x, min_y, max_x, _) = after.bounds?;
        if before.scale != after.scale
            || before.bounds != after.bounds
            || before.lines.len() != after.lines.len()
        {
            return None;
        }
        let mut rows: Vec<(i32, i32)> = before
            .lines
            .iter()
            .zip(&after.lines)
            .filter(|(old, new)| old != new)
            .flat_map(|(old, new)| old.iter().chain(new))
            .map(|mark| (mark.top - min_y, mark.right_bottom().1 - min_y))
            .collect();
        rows.sort_unstable();
        let mut bands: Vec<(i32, i32)> = Vec::new();
        for (top, bottom) in rows {
            match bands.last_mut() {
                Some(band) if top <= band.1 => band.1 = band.1.max(bottom),
                _ => bands.push((top, bottom)),
            }
        }
        let width = (max_x - min_x).max(1) as u32;
        let patches = bands
            .into_iter()
            .map(|(top, bottom)| {
                let mut canvas = Premultiplied::new(width, (bottom - top) as u32);
                let reaching = after.lines.iter().flatten().filter(|mark| {
                    mark.top - min_y < bottom && mark.right_bottom().1 - min_y > top
                });
                self.paint(fs, reaching, &mut canvas, (min_x, min_y + top), after.scale);
                Patch {
                    top: top as usize,
                    image: canvas.image,
                }
            })
            .collect();
        Some(patches)
    }

    /// Paints `marks`, in order, onto `canvas`, whose top-left corner sits at block-relative
    /// `(origin_x, origin_y)`. What falls outside the canvas is left out.
    fn paint<'a>(
        &mut self,
        fs: &mut FontSystem,
        marks: impl Iterator<Item = &'a Mark>,
        canvas: &mut impl Canvas,
        (origin_x, origin_y): (i32, i32),
        scale: f32,
    ) {
        let (canvas_w, canvas_h) = canvas.size();
        let (canvas_w, canvas_h) = (canvas_w as i32, canvas_h as i32);
        for mark in marks {
            let Some(image) = self.get_image(fs, mark.key, scale) else {
                continue;
            };
            let left = mark.left - origin_x;
            let top = mark.top - origin_y;
            let mask_w = image.placement.width as i32;
            let mask_h = image.placement.height as i32;
            for row in (-top).max(0)..mask_h.min(canvas_h - top) {
                let y = top + row;
                for col in (-left).max(0)..mask_w.min(canvas_w - left) {
                    let x = left + col;
                    let index = (row * mask_w + col) as usize;
                    let (src, alpha) =
                        sample_pixel(image.content, &image.data, index, mark.ink, mark.background);
                    if alpha == 0 {
                        continue;
                    }
                    canvas.paint(x as u32, y as u32, src, alpha);
                }
            }
        }
    }
}

/// What a block's image is painted from: each glyph mask in the order it's painted, a line at a
/// time, and the bounds of their ink, in block-relative physical pixels. Two paintings of a
/// block whose lines differ in only some places differ in their image only in the rows those
/// lines' ink covers, which is what [`GlyphAtlas::redraw`] repaints.
#[derive(Clone, Debug, PartialEq)]
pub struct Painting {
    lines: Vec<Vec<Mark>>,
    /// `(min_x, min_y, max_x, max_y)` with the maxima exclusive, or `None` if nothing is drawn.
    bounds: Option<(i32, i32, i32, i32)>,
    scale: f32,
}

/// A band of a block's image, as wide as the image, whose first row is `top`.
pub struct Patch {
    pub top: usize,
    pub image: ColorImage,
}

/// One glyph mask as a block's image paints it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mark {
    key: CacheKey,
    /// The block-relative pixel its mask's top-left corner lands on.
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    ink: [u8; 3],
    /// What its coverage is baked against: the paper, or a wash.
    background: [u8; 3],
}

impl Mark {
    /// Just past its mask's right and bottom edges.
    fn right_bottom(&self) -> (i32, i32) {
        (self.left + self.width as i32, self.top + self.height as i32)
    }
}

/// Where a glyph lands, and which mask it needs, at `scale` physical pixels per
/// layout unit.
///
/// The run's baseline must be added to each glyph, otherwise every visual line
/// is painted on top of the first. `physical` adds its offset after scaling, so
/// the baseline is converted here, and rounded onto a whole device pixel: a
/// fractional baseline would pick a different vertical subpixel mask per line,
/// giving lines of one paragraph visibly different x-heights.
fn place_glyph(glyph: &LayoutGlyph, line_y: f32, scale: f32) -> PhysicalGlyph {
    glyph.physical((0.0, (line_y * scale).round()), scale)
}

/// Colour and coverage of the mask pixel at `index`.
///
/// A grayscale mask carries the caller's ink and its coverage; a subpixel mask
/// holds three horizontal coverage samples and is baked against `background`
/// so each channel is tinted by its own subpixel (the LCD fringing); a colour
/// bitmap (emoji) carries its own RGBA.
fn sample_pixel(
    content: SwashContent,
    data: &[u8],
    index: usize,
    ink: [u8; 3],
    background: [u8; 3],
) -> ([u8; 3], u8) {
    match content {
        SwashContent::Mask => (ink, data.get(index).copied().unwrap_or(0)),
        SwashContent::SubpixelMask => {
            let base = index * 4;
            let cr = data.get(base).copied().unwrap_or(0) as u32;
            let cg = data.get(base + 1).copied().unwrap_or(0) as u32;
            let cb = data.get(base + 2).copied().unwrap_or(0) as u32;
            let c_max = cr.max(cg).max(cb);
            if c_max == 0 {
                return (background, 0);
            }
            // out_channel = mix(background, ink, coverage) per subpixel, then
            // expressed as a straight colour at alpha = max coverage so that a
            // standard source-over composite over the same background lands
            // exactly on the per-channel result.
            let mix = |c: u32, ink: u8, bg: u8| {
                ((bg as u32 * (c_max - c) + ink as u32 * c) / c_max) as u8
            };
            let color = [
                mix(cr, ink[0], background[0]),
                mix(cg, ink[1], background[1]),
                mix(cb, ink[2], background[2]),
            ];
            (color, c_max as u8)
        }
        SwashContent::Color => {
            let base = index * 4;
            (
                [
                    data.get(base).copied().unwrap_or(0),
                    data.get(base + 1).copied().unwrap_or(0),
                    data.get(base + 2).copied().unwrap_or(0),
                ],
                data.get(base + 3).copied().unwrap_or(0),
            )
        }
    }
}

/// Rasterizes one glyph with the swash scaler in `format`.
///
/// This mirrors cosmic-text's `swash_image` but with a choice of format:
/// [`Format::Subpixel`] carries three coverage samples per pixel, and
/// [`Format::Alpha`] one.
fn rasterize_mask(
    fs: &mut FontSystem,
    context: &mut ScaleContext,
    cache_key: CacheKey,
    format: Format,
) -> Option<SwashImage> {
    let font = fs.get_font(cache_key.font_id, cache_key.font_weight)?;
    let mut scaler = context
        .builder(font.as_swash())
        .size(f32::from_bits(cache_key.font_size_bits))
        .hint(!cache_key.flags.contains(CacheKeyFlags::DISABLE_HINTING));
    if let Some(variation) = font
        .as_swash()
        .variations()
        .find_by_tag(swash::Tag::from_be_bytes(*b"wght"))
    {
        scaler = scaler.normalized_coords(font.as_swash().variations().normalized_coords([(
            swash::Tag::from_be_bytes(*b"wght"),
            f32::from(cache_key.font_weight.0).clamp(variation.min_value(), variation.max_value()),
        )]));
    }
    let mut scaler = scaler.build();

    let offset = Vector::new(cache_key.x_bin.as_float(), cache_key.y_bin.as_float());
    Render::new(&[
        Source::ColorOutline(0),
        Source::ColorBitmap(StrikeWith::BestFit),
        Source::Outline,
    ])
    .format(format)
    .offset(offset)
    .transform(if cache_key.flags.contains(CacheKeyFlags::FAKE_ITALIC) {
        Some(Transform::skew(
            Angle::from_degrees(14.0),
            Angle::from_degrees(0.0),
        ))
    } else {
        None
    })
    .render(&mut scaler, cache_key.glyph_id)
}

/// What glyphs are painted into.
trait Canvas {
    /// Its width and height, in pixels.
    fn size(&self) -> (u32, u32);

    /// Composites straight-alpha `src` at `alpha` over the pixel at `(x, y)`.
    fn paint(&mut self, x: u32, y: u32, src: [u8; 3], alpha: u8);
}

#[cfg(test)]
impl Canvas for RgbaImage {
    fn size(&self) -> (u32, u32) {
        self.dimensions()
    }

    fn paint(&mut self, x: u32, y: u32, src: [u8; 3], alpha: u8) {
        blend(&mut self.get_pixel_mut(x, y).0, src, alpha);
    }
}

/// egui's premultiplied image, and the straight colour of each pixel that the next glyph over it
/// is composited with.
struct Premultiplied {
    width: u32,
    straight: Vec<[u8; 4]>,
    image: ColorImage,
}

impl Premultiplied {
    fn new(width: u32, height: u32) -> Premultiplied {
        let size = [width as usize, height as usize];
        Premultiplied {
            width,
            straight: vec![[0; 4]; size[0] * size[1]],
            image: ColorImage::filled(size, Color32::TRANSPARENT),
        }
    }
}

impl Canvas for Premultiplied {
    fn size(&self) -> (u32, u32) {
        (self.width, self.image.size[1] as u32)
    }

    fn paint(&mut self, x: u32, y: u32, src: [u8; 3], alpha: u8) {
        let index = y as usize * self.width as usize + x as usize;
        let straight = &mut self.straight[index];
        blend(straight, src, alpha);
        let [r, g, b, a] = *straight;
        self.image.pixels[index] = Color32::from_rgba_unmultiplied(r, g, b, a);
    }
}

/// Source-over composite a straight-alpha `src`/`alpha` onto `dst`.
fn blend(dst: &mut [u8; 4], src: [u8; 3], alpha: u8) {
    if alpha == 0 {
        return;
    }
    let sa = alpha as f32 / 255.0;
    let da = dst[3] as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        return;
    }
    for channel in 0..3 {
        let out = (src[channel] as f32 * sa + dst[channel] as f32 * da * (1.0 - sa)) / out_a;
        dst[channel] = out.round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{layout_block, LayoutConfig};
    use crate::model::{Block, BlockKind, Span, SpanStyle};
    use image::Pixel;
    use std::path::PathBuf;
    use uuid::Uuid;

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

    /// Number of pixels carrying any coverage.
    fn ink_pixels(image: &RgbaImage) -> usize {
        image.pixels().filter(|pixel| pixel[3] > 0).count()
    }

    /// Bounding box of painted pixels as `(left, top, right, bottom)` with
    /// exclusive maxima, or `None` when nothing was painted.
    fn painted_extents(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for (x, y, pixel) in image.enumerate_pixels() {
            if pixel[3] == 0 {
                continue;
            }
            bounds = Some(match bounds {
                None => (x, y, x + 1, y + 1),
                Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(x + 1), b.max(y + 1)),
            });
        }
        bounds
    }

    #[test]
    fn rasterize_produces_non_blank_image() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph("Hello");
        let layout = layout_block(&mut fs, &block, &config());
        let mut atlas = GlyphAtlas::new();
        let color = [17, 17, 17];
        let background = [255, 255, 255];
        let (image, _, _) = atlas.rasterize(&mut fs, &layout, color, background, &[], 1.0);

        assert!(image.width() > 0, "canvas has no width");
        assert!(image.height() > 0, "canvas has no height");
        assert!(ink_pixels(&image) > 0, "no glyph pixels were painted");

        // Subpixel antialiasing bakes against the background, so edge pixels
        // fringe toward it; interior pixels are fully covered and must carry
        // the requested ink exactly.
        let mut fully_inked = false;
        for pixel in image.pixels().filter(|pixel| pixel[3] > 0) {
            let channels = pixel.channels();
            for channel in 0..3 {
                let lo = color[channel].min(background[channel]);
                let hi = color[channel].max(background[channel]);
                assert!(
                    (lo..=hi).contains(&channels[channel]),
                    "painted channel {channel} strayed outside the ink/background range"
                );
            }
            if channels[0] == color[0] && channels[1] == color[1] && channels[2] == color[2] {
                fully_inked = true;
            }
        }
        assert!(
            fully_inked,
            "a glyph interior must be fully covered in the requested ink"
        );
    }

    #[test]
    fn rasterize_dimensions_cover_glyphs() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph("Hello");
        let layout = layout_block(&mut fs, &block, &config());
        let mut atlas = GlyphAtlas::new();
        let (image, left, top) =
            atlas.rasterize(&mut fs, &layout, [0, 0, 0], [255, 255, 255], &[], 1.0);

        let canvas_right = left + image.width() as i32;
        let canvas_bottom = top + image.height() as i32;

        // Every glyph's *placed ink* rectangle must lie inside the returned
        // `[left, left + width) x [top, top + height)` window. The rectangle is
        // the pen plus the mask's left side bearing and size — not the pen
        // origin, which sits left of the ink whenever the bearing is positive.
        let mut glyphs = 0;
        for run in layout.buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let phys = place_glyph(glyph, run.line_y, 1.0);
                let Some(mask) = atlas.get_image(&mut fs, phys.cache_key, 1.0) else {
                    continue;
                };
                if mask.placement.width == 0 || mask.placement.height == 0 {
                    continue;
                }
                let ink_left = phys.x + mask.placement.left;
                let ink_top = phys.y - mask.placement.top;
                let ink_right = ink_left + mask.placement.width as i32;
                let ink_bottom = ink_top + mask.placement.height as i32;

                assert!(
                    ink_left >= left && ink_right <= canvas_right,
                    "glyph ink x range [{ink_left}, {ink_right}) outside canvas \
                     [{left}, {canvas_right})"
                );
                assert!(
                    ink_top >= top && ink_bottom <= canvas_bottom,
                    "glyph ink y range [{ink_top}, {ink_bottom}) outside canvas \
                     [{top}, {canvas_bottom})"
                );
                glyphs += 1;
            }
        }
        assert!(glyphs > 0, "no glyphs were shaped");
        assert!(ink_pixels(&image) > 0, "no glyph pixels were painted");

        // Independent check of the returned window, derived from the *layout*
        // (`LayoutRun::line_top` / `line_y` and the `LayoutLine` bands) rather
        // than by recomputing `glyph.physical` the way the implementation does.
        let (ink_l, ink_t, ink_r, ink_b) =
            painted_extents(&image).expect("canvas has no painted pixels");
        let ink_l = left + ink_l as i32;
        let ink_t = top + ink_t as i32;
        let ink_r = left + ink_r as i32;
        let ink_b = top + ink_b as i32;

        assert!(
            ink_l >= 0 && ink_r <= canvas_right && ink_r > ink_l,
            "painted ink columns [{ink_l}, {ink_r}) are not inside the block"
        );

        // "Hello" is one line with no descenders, so its ink must sit between
        // the line's top and its baseline. `line_top`/`line_y` are layout data,
        // not glyph placements.
        let run = layout.buffer.layout_runs().next().unwrap();
        assert!(
            ink_t >= run.line_top.floor() as i32 - 1,
            "painted ink starts at y {ink_t}, above the line top {}",
            run.line_top
        );
        assert!(
            ink_b <= run.line_y.ceil() as i32 + 2,
            "painted ink ends at y {ink_b}, below the baseline {} — `Hello` has no \
             descenders",
            run.line_y
        );

        // …and inside the band the layout reports for that line.
        let line = layout.lines.first().expect("layout reported no lines");
        assert!(
            ink_t as f32 >= line.y - 1.0 && ink_b as f32 <= line.y + line.height + 1.0,
            "painted ink rows [{ink_t}, {ink_b}) fall outside the line band [{}, {})",
            line.y,
            line.y + line.height
        );

        // The canvas is cropped to ink, so its four edges must coincide (within
        // a pixel of antialiasing) with the painted pixels. A block- or
        // line-wide canvas would leave slack at an edge; the old
        // `width() >= line_w` assertion was the reverse of the true relation and
        // only passed because this particular ink happens to be ~35px wide.
        assert!(
            ink_l - left <= 1
                && ink_t - top <= 1
                && canvas_right - ink_r <= 1
                && canvas_bottom - ink_b <= 1,
            "canvas [{left}, {canvas_right}) x [{top}, {canvas_bottom}) is not cropped \
             to the painted ink [{ink_l}, {ink_r}) x [{ink_t}, {ink_b})"
        );
    }

    #[test]
    fn rasterize_paints_every_line() {
        let text = "Typography is the craft of endowing human language with a durable \
visual form, and thus with an independent existence. Its heartwood is calligraphy: the \
dance, on a page, of pen, eye and hand.";
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph(text);
        let layout = layout_block(&mut fs, &block, &config());
        assert!(layout.lines.len() >= 3, "expected a multi-line block");

        let mut atlas = GlyphAtlas::new();
        let (image, _, top) =
            atlas.rasterize(&mut fs, &layout, [0, 0, 0], [255, 255, 255], &[], 1.0);

        for line in &layout.lines {
            let band_top = (line.y - top as f32).round().max(0.0) as u32;
            let band_bottom =
                ((line.y + line.height - top as f32).round() as u32).min(image.height());
            assert!(
                band_bottom > band_top,
                "empty band for line at y {}",
                line.y
            );
            let painted = (band_top..band_bottom)
                .any(|y| (0..image.width()).any(|x| image.get_pixel(x, y)[3] > 0));
            assert!(
                painted,
                "line at y {} painted nothing (glyph runs may have been stacked)",
                line.y
            );
        }
    }

    #[test]
    fn rasterize_draws_at_the_physical_size_of_the_display() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let layout = layout_block(&mut fs, &paragraph("Hello"), &config());
        let mut atlas = GlyphAtlas::new();
        let (one, left_one, top_one) =
            atlas.rasterize(&mut fs, &layout, [0, 0, 0], [255, 255, 255], &[], 1.0);
        let (two, left_two, top_two) =
            atlas.rasterize(&mut fs, &layout, [0, 0, 0], [255, 255, 255], &[], 2.0);

        let close = |scaled: i32, base: i32| (scaled - base * 2).abs() <= 3;
        assert!(
            close(two.width() as i32, one.width() as i32)
                && close(two.height() as i32, one.height() as i32),
            "a 2x canvas should be twice the size: {}x{} vs {}x{}",
            two.width(),
            two.height(),
            one.width(),
            one.height()
        );
        assert!(
            close(left_two, left_one) && close(top_two, top_one),
            "the ink offset is physical, so it doubles too: ({left_two}, {top_two}) vs \
             ({left_one}, {top_one})"
        );
    }

    /// A glyph at 16 units on a 1.5x display is the same mask as one set at 24
    /// units on a 1x display: real rasterization at the device size, rather
    /// than an enlargement of the 16-unit mask. Nearest-enlarging is what made
    /// every third row and column double on a fractionally scaled display.
    #[test]
    fn rasterize_at_a_fractional_scale_matches_setting_the_type_at_that_size() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let scaled_layout = layout_block(&mut fs, &paragraph("H"), &config());
        let (scaled, _, _) = atlas.rasterize(
            &mut fs,
            &scaled_layout,
            [0, 0, 0],
            [255, 255, 255],
            &[],
            1.5,
        );

        let large = LayoutConfig {
            body_size: 24.0,
            body_line_height: 36.0,
            ..config()
        };
        let native_layout = layout_block(&mut fs, &paragraph("H"), &large);
        let (native, _, _) = atlas.rasterize(
            &mut fs,
            &native_layout,
            [0, 0, 0],
            [255, 255, 255],
            &[],
            1.0,
        );

        assert_eq!(
            (scaled.width(), scaled.height()),
            (native.width(), native.height())
        );
        assert_eq!(scaled.as_raw(), native.as_raw());
    }

    #[test]
    fn every_line_of_a_block_sits_on_a_whole_device_pixel_baseline() {
        let text = "Typography is the craft of endowing human language with a durable \
visual form, and thus with an independent existence. Its heartwood is calligraphy: the \
dance, on a page, of pen, eye and hand.";
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        // 1.5 turns the 25-unit leading into a fractional 37.5 pixels, so the
        // baselines of alternate lines would otherwise fall between pixels.
        let layout = layout_block(
            &mut fs,
            &paragraph(text),
            &LayoutConfig {
                body_line_height: 25.0,
                ..config()
            },
        );
        assert!(layout.lines.len() >= 3, "expected a multi-line block");

        for run in layout.buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let phys = place_glyph(glyph, run.line_y, 1.5);
                assert_eq!(
                    phys.cache_key.y_bin,
                    cosmic_text::SubpixelBin::Zero,
                    "a fractional baseline picks a vertical subpixel mask"
                );
            }
        }
    }

    /// Number of painted pixels whose channels differ: the colour fringing of
    /// LCD antialiasing, given black ink on white paper.
    fn coloured_pixels(image: &RgbaImage) -> usize {
        image
            .pixels()
            .filter(|pixel| pixel[3] > 0 && (pixel[0] != pixel[1] || pixel[1] != pixel[2]))
            .count()
    }

    #[test]
    fn glyphs_use_lcd_below_high_density_and_grayscale_from_it() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let layout = layout_block(&mut fs, &paragraph("Hello"), &config());
        let mut atlas = GlyphAtlas::new();
        for (scale, expect_lcd) in [
            (1.0, true),
            (1.5, true),
            (1.99, true),
            (2.0, false),
            (3.0, false),
        ] {
            let (image, _, _) =
                atlas.rasterize(&mut fs, &layout, [0, 0, 0], [255, 255, 255], &[], scale);
            assert!(ink_pixels(&image) > 0, "nothing painted at scale {scale}");
            assert_eq!(
                coloured_pixels(&image) > 0,
                expect_lcd,
                "wrong antialiasing at scale {scale}"
            );
        }
    }

    /// The image egui is given, drawn premultiplied, is the one drawn with straight alpha and then
    /// converted, pixel for pixel: plain text, a link in its own colour, an italic whose glyphs
    /// overlap, and a note's wash, at every density, over the paper and over a coloured ground.
    #[test]
    fn drawing_premultiplied_gives_what_converting_the_straight_image_did() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut block = paragraph(
            "Plain words, then a link to follow, then an italic flourish fff, and a noted \
             phrase that runs on long enough to wrap onto a second line of the measure.",
        );
        let link = block.text.find("a link").unwrap();
        let italic = block.text.find("an italic").unwrap();
        let noted = block.text.find("a noted").unwrap();
        block.spans = vec![
            Span {
                start: link,
                end: link + "a link".len(),
                style: SpanStyle::Link {
                    url: "https://example.com".to_string(),
                },
            },
            Span {
                start: italic,
                end: italic + "an italic flourish fff".len(),
                style: SpanStyle::Italic,
            },
        ];
        let layout = layout_block(&mut fs, &block, &config());
        assert!(
            layout
                .runs
                .iter()
                .flat_map(|run| &run.glyphs)
                .any(|glyph| glyph.color_opt.is_some()),
            "the link has a colour of its own"
        );
        let wash = [(noted, noted + "a noted phrase".len(), [250, 226, 140])];
        let mut atlas = GlyphAtlas::new();
        for scale in [1.0, 1.5, 2.0] {
            for (ink, background) in [
                ([17, 17, 17], [255, 255, 255]),
                ([230, 225, 210], [40, 60, 90]),
            ] {
                let (straight, left, top) =
                    atlas.rasterize(&mut fs, &layout, ink, background, &wash, scale);
                let (premultiplied, new_left, new_top) =
                    atlas.rasterize_premultiplied(&mut fs, &layout, ink, background, &wash, scale);
                let converted = egui::ColorImage::from_rgba_unmultiplied(
                    [straight.width() as usize, straight.height() as usize],
                    straight.as_raw(),
                );
                assert_eq!((new_left, new_top), (left, top));
                assert_eq!(premultiplied.size, converted.size);
                assert!(
                    premultiplied.pixels == converted.pixels,
                    "at scale {scale} over {background:?}"
                );
                if scale < GRAYSCALE_MIN_SCALE {
                    assert!(coloured_pixels(&straight) > 0, "subpixel coverage is drawn");
                }
                let partial = straight
                    .pixels()
                    .filter(|pixel| (1..255).contains(&pixel[3]))
                    .count();
                assert!(partial > 0, "some pixels are partly covered");
            }
        }

        // Nothing to draw is the same too.
        let empty = layout_block(&mut fs, &paragraph(""), &config());
        let (straight, ..) = atlas.rasterize(&mut fs, &empty, [0; 3], [255; 3], &[], 2.0);
        let (premultiplied, ..) =
            atlas.rasterize_premultiplied(&mut fs, &empty, [0; 3], [255; 3], &[], 2.0);
        assert_eq!(premultiplied.size, [1, 1]);
        assert_eq!(
            premultiplied.pixels,
            egui::ColorImage::from_rgba_unmultiplied([1, 1], straight.as_raw()).pixels
        );
    }

    /// 16 units at 2x and 32 units at 1x reach the same physical size, so they
    /// share a `CacheKey`, yet they need different mask formats. Moving a window
    /// between displays must not draw one with the other's mask.
    #[test]
    fn one_physical_size_keeps_a_mask_per_format() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let dense = layout_block(&mut fs, &paragraph("H"), &config());
        let (gray, _, _) = atlas.rasterize(&mut fs, &dense, [0, 0, 0], [255, 255, 255], &[], 2.0);
        assert_eq!(coloured_pixels(&gray), 0, "2x should be grayscale");

        let large = LayoutConfig {
            body_size: 32.0,
            body_line_height: 48.0,
            ..config()
        };
        let plain = layout_block(&mut fs, &paragraph("H"), &large);
        let (lcd, _, _) = atlas.rasterize(&mut fs, &plain, [0, 0, 0], [255, 255, 255], &[], 1.0);
        assert!(coloured_pixels(&lcd) > 0, "1x reused the grayscale mask");
    }

    #[test]
    fn rasterize_empty_block_is_blank() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph("");
        let layout = layout_block(&mut fs, &block, &config());
        let mut atlas = GlyphAtlas::new();
        let (image, _, _) = atlas.rasterize(&mut fs, &layout, [0, 0, 0], [255, 255, 255], &[], 1.0);
        assert_eq!(ink_pixels(&image), 0, "empty block painted ink");
    }

    #[test]
    fn rasterize_is_reusable_across_blocks() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let config = config();

        let first = paragraph("Hello");
        let l1 = layout_block(&mut fs, &first, &config);
        let (a, _, _) = atlas.rasterize(&mut fs, &l1, [0, 0, 0], [255, 255, 255], &[], 1.0);

        let second = paragraph("World");
        let l2 = layout_block(&mut fs, &second, &config);
        let (b, _, _) = atlas.rasterize(&mut fs, &l2, [0, 0, 0], [255, 255, 255], &[], 1.0);

        assert!(ink_pixels(&a) > 0 && ink_pixels(&b) > 0);
        assert_ne!(
            a.as_raw(),
            b.as_raw(),
            "the second rasterize reused the first canvas"
        );
    }

    // --- redrawing only the lines that changed ---

    const PROSE: &str =
        "Every machine that writes for us is a promise about who does the thinking, \
        and a promise is a thing that can be kept or broken quietly, in the margins, over years of \
        small conveniences that nobody would think to refuse. The page waits; the writer types; \
        the words arrive set and justified, and nothing about them says how they got there. Quiet \
        grey paragraphs go by, and the eye, which is lazy, takes the typography for the thought.";

    /// `text` with `insert` put in at byte `at`.
    fn typed(text: &str, at: usize, insert: &str) -> String {
        format!("{}{insert}{}", &text[..at], &text[at..])
    }

    /// What `block` paints at `scale`, in `config`, under `highlights`.
    fn painting_of(
        fs: &mut FontSystem,
        atlas: &mut GlyphAtlas,
        block: &Block,
        config: &LayoutConfig,
        highlights: &[(usize, usize, [u8; 3])],
        scale: f32,
    ) -> (BlockLayout, Painting) {
        let layout = layout_block(fs, block, config);
        let painting = atlas.painting(
            fs,
            &layout,
            [17, 17, 17],
            [250, 248, 240],
            highlights,
            scale,
        );
        (layout, painting)
    }

    /// Draws `before` whole, lays the bands `redraw` gives over it, and checks that this is the
    /// image drawing `after` whole gives, byte for byte. Returns the rows the bands cover.
    fn redrawn_rows(
        fs: &mut FontSystem,
        atlas: &mut GlyphAtlas,
        before: &Painting,
        after: &Painting,
    ) -> Vec<std::ops::Range<usize>> {
        let (mut image, left, top) = atlas.draw_premultiplied(fs, before);
        let patches = atlas
            .redraw(fs, before, after)
            .expect("the image is redrawn in part");
        let width = image.size[0];
        let mut rows = Vec::new();
        for patch in patches {
            assert_eq!(patch.image.size[0], width, "a band is as wide as the image");
            let start = patch.top * width;
            image.pixels[start..start + patch.image.pixels.len()]
                .copy_from_slice(&patch.image.pixels);
            rows.push(patch.top..patch.top + patch.image.size[1]);
        }
        let (whole, new_left, new_top) = atlas.draw_premultiplied(fs, after);
        assert_eq!((new_left, new_top), (left, top));
        assert_eq!(whole.size, image.size);
        assert!(
            whole.pixels == image.pixels,
            "the image redrawn in part is not the one drawn whole"
        );
        rows
    }

    fn rows_in(rows: &[std::ops::Range<usize>]) -> usize {
        rows.iter().map(|range| range.len()).sum()
    }

    /// The rows of the image `painting`'s line `line` puts ink on.
    fn rows_of_line(painting: &Painting, line: usize) -> std::ops::Range<i32> {
        let (_, min_y, _, _) = painting.bounds.unwrap();
        let marks = &painting.lines[line];
        let top = marks.iter().map(|mark| mark.top).min().unwrap();
        let bottom = marks
            .iter()
            .map(|mark| mark.right_bottom().1)
            .max()
            .unwrap();
        top - min_y..bottom - min_y
    }

    /// Paintings of `PROSE` and of it with `insert` at `at(layout)`, at 1x and 2x, each redrawn
    /// in part and checked against drawing it whole. Returns the rows redrawn and the image's
    /// height at each scale.
    fn redraw_typing(at: impl Fn(&BlockLayout) -> usize, insert: &str) -> Vec<(usize, usize)> {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let mut redrawn = Vec::new();
        for scale in [1.0, 2.0] {
            let (layout, before) = painting_of(
                &mut fs,
                &mut atlas,
                &paragraph(PROSE),
                &config(),
                &[],
                scale,
            );
            let text = typed(PROSE, at(&layout), insert);
            let (_, after) = painting_of(
                &mut fs,
                &mut atlas,
                &paragraph(&text),
                &config(),
                &[],
                scale,
            );
            let rows = redrawn_rows(&mut fs, &mut atlas, &before, &after);
            let (_, min_y, _, max_y) = after.bounds.unwrap();
            redrawn.push((rows_in(&rows), (max_y - min_y) as usize));
        }
        redrawn
    }

    #[test]
    fn a_letter_typed_mid_line_redraws_only_some_rows() {
        for (rows, height) in redraw_typing(|layout| layout.lines[3].start + 6, "x") {
            assert!(rows > 0, "the line typed in is redrawn");
            assert!(rows < height / 2, "{rows} of {height} rows were redrawn");
        }
    }

    #[test]
    fn a_letter_typed_at_a_lines_end_redraws_as_drawing_whole_does() {
        let at_end = |layout: &BlockLayout| {
            let line = layout.lines[2];
            PROSE[..line.end].trim_end().len()
        };
        for (rows, height) in redraw_typing(at_end, "s") {
            assert!(
                rows > 0 && rows < height,
                "{rows} of {height} rows were redrawn"
            );
        }
    }

    /// A word typed in the first line pushes a word down into each line after it, and every one
    /// of them is redrawn.
    #[test]
    fn a_word_that_rewraps_the_rest_redraws_as_drawing_whole_does() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        // A last line short enough to take a word more.
        let prose = PROSE.replace(", which is lazy,", "");
        for scale in [1.0, 2.0] {
            let (layout, before) = painting_of(
                &mut fs,
                &mut atlas,
                &paragraph(&prose),
                &config(),
                &[],
                scale,
            );
            let text = typed(&prose, layout.lines[0].start + 6, "obviously ");
            let (_, after) = painting_of(
                &mut fs,
                &mut atlas,
                &paragraph(&text),
                &config(),
                &[],
                scale,
            );
            let changed = (0..before.lines.len())
                .filter(|line| before.lines[*line] != after.lines[*line])
                .count();
            assert!(changed > 3, "only {changed} lines rewrapped");
            redrawn_rows(&mut fs, &mut atlas, &before, &after);
        }
    }

    #[test]
    fn a_multibyte_character_typed_redraws_as_drawing_whole_does() {
        for insert in ["é", "—", "ﬁ"] {
            for (rows, _) in redraw_typing(|layout| layout.lines[4].start + 3, insert) {
                assert!(rows > 0, "typing {insert} redraws nothing");
            }
        }
    }

    /// Lines are broken a line ahead, so an edit in a line can move the break before it, as
    /// deleting a word lets the line before take one more, and the line before is redrawn too.
    #[test]
    fn an_edit_that_moves_a_line_before_it_redraws_that_line() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let (layout, before) =
            painting_of(&mut fs, &mut atlas, &paragraph(PROSE), &config(), &[], 2.0);
        let mut found = 0;
        for (edited, line) in layout.lines.iter().enumerate().skip(1) {
            for at in (line.start..line.end).filter(|at| PROSE.as_bytes()[*at] == b' ') {
                let word = PROSE[at + 1..]
                    .find(' ')
                    .map_or(PROSE.len(), |end| at + 1 + end);
                let deleted = format!("{}{}", &PROSE[..at], &PROSE[word..]);
                for text in [" a", " an", " the", " quite", " already"]
                    .map(|insert| typed(PROSE, at, insert))
                    .into_iter()
                    .chain([deleted])
                {
                    let (_, after) =
                        painting_of(&mut fs, &mut atlas, &paragraph(&text), &config(), &[], 2.0);
                    if after.lines.len() != before.lines.len() || after.bounds != before.bounds {
                        continue;
                    }
                    if (0..edited).any(|line| before.lines[line] != after.lines[line]) {
                        redrawn_rows(&mut fs, &mut atlas, &before, &after);
                        found += 1;
                    }
                }
            }
        }
        assert!(found > 0, "no edit moved a line before it");
    }

    #[test]
    fn a_highlight_added_redraws_the_rows_under_it() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let block = paragraph(PROSE);
        for scale in [1.0, 2.0] {
            let (layout, before) = painting_of(&mut fs, &mut atlas, &block, &config(), &[], scale);
            let line = layout.lines[3];
            let wash = [(line.start + 2, line.start + 12, [250, 226, 140])];
            let (_, after) = painting_of(&mut fs, &mut atlas, &block, &config(), &wash, scale);
            let rows = redrawn_rows(&mut fs, &mut atlas, &before, &after);
            assert_eq!(rows.len(), 1);
            let line_rows = rows_of_line(&after, 3);
            assert_eq!(
                rows[0].start as i32..rows[0].end as i32,
                line_rows,
                "only the line under the wash is redrawn"
            );
        }
    }

    /// Set tight, a line's descenders reach down into the line below and its accents up into the
    /// line above, and italics lean over their neighbours. Redrawing one line must take in what
    /// reaches its rows from the lines next to it.
    #[test]
    fn ink_reaching_over_from_the_next_line_is_redrawn_with_it() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let tight = LayoutConfig {
            body_line_height: 14.0,
            ..config()
        };
        let text = "Ýgg Éclair, jumpy “quips” gyp Ágape; Ëmpty pyg, Öyster grip. ".repeat(6);
        let mut block = paragraph(&text);
        block.spans = vec![Span {
            start: 0,
            end: text.len() / 3,
            style: SpanStyle::Italic,
        }];
        for scale in [1.0, 2.0] {
            let (layout, before) = painting_of(&mut fs, &mut atlas, &block, &tight, &[], scale);
            let line = 2;
            let above = rows_of_line(&before, line - 1);
            let edited = rows_of_line(&before, line);
            let below = rows_of_line(&before, line + 1);
            assert!(
                above.end > edited.start && below.start < edited.end,
                "the lines' ink overlaps"
            );
            let mut typed_in = block.clone();
            let mut at = layout.lines[line].start + 1;
            while !text.is_char_boundary(at) {
                at += 1;
            }
            typed_in.text = typed(&text, at, "q");
            let (_, after) = painting_of(&mut fs, &mut atlas, &typed_in, &tight, &[], scale);
            redrawn_rows(&mut fs, &mut atlas, &before, &after);
        }
    }

    /// A line whose tallest or deepest ink goes away is redrawn over the rows that ink covered,
    /// not only over those its new ink covers, or the accent or the descender would stay on the
    /// page: an "Ó" made "O", and the one "q" of a line with no other descender made "o".
    #[test]
    fn ink_a_line_loses_is_painted_out() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let line = 3;
        // `text` with the first letter of the line's second word as `with`, and as `instead`.
        let mut changed_in = |text: &str, with: &str, instead: &str| {
            let layout = layout_block(&mut fs, &paragraph(text), &config());
            let start = layout.lines[line].start;
            let at = start + text[start..].find(' ').unwrap() + 1;
            let next = at + text[at..].chars().next().unwrap().len_utf8();
            let made = |letter: &str| format!("{}{letter}{}", &text[..at], &text[next..]);
            (made(with), made(instead))
        };
        let accent = changed_in(PROSE, "Ó", "O");
        let no_descenders = "the clocks hum on and the old water runs at the stone wall ".repeat(7);
        let descender = changed_in(&no_descenders, "q", "o");
        for scale in [1.0, 2.0] {
            for (from, to) in [&accent, &descender] {
                let (_, before) =
                    painting_of(&mut fs, &mut atlas, &paragraph(from), &config(), &[], scale);
                let (_, after) =
                    painting_of(&mut fs, &mut atlas, &paragraph(to), &config(), &[], scale);
                let (old, new) = (rows_of_line(&before, line), rows_of_line(&after, line));
                eprintln!(
                    "{scale} {old:?} {new:?} {} {} {:?} {:?}",
                    before.lines.len(),
                    after.lines.len(),
                    before.bounds,
                    after.bounds
                );
                assert!(
                    old.start < new.start || old.end > new.end,
                    "the line's old ink reaches past its new ink: {old:?} against {new:?}"
                );
                let rows = redrawn_rows(&mut fs, &mut atlas, &before, &after);
                assert!(
                    rows.iter()
                        .any(|band| band.start as i32 <= old.start && old.end <= band.end as i32),
                    "the old ink's rows {old:?} are redrawn, in {rows:?}"
                );
            }
        }
    }

    /// The lines with a hyphen drawn at their end, which stands for no text.
    fn hyphenated_lines(layout: &BlockLayout) -> Vec<usize> {
        (0..layout.runs.len())
            .filter(|line| {
                layout.runs[*line]
                    .glyphs
                    .iter()
                    .any(|glyph| glyph.start == glyph.end)
            })
            .collect()
    }

    /// Hyphenated, a line's drawn hyphen comes and goes with its break, and the lines it's on are
    /// redrawn as drawing them whole does.
    #[test]
    fn a_hyphen_that_comes_or_goes_redraws_as_drawing_whole_does() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let config = LayoutConfig {
            body_width: 200.0,
            hyphenate: true,
            ..config()
        };
        for scale in [1.0, 2.0] {
            let block = paragraph(PROSE);
            let (layout, before) = painting_of(&mut fs, &mut atlas, &block, &config, &[], scale);
            let hyphens = hyphenated_lines(&layout);
            assert!(!hyphens.is_empty(), "a line ends in a hyphen");
            let mut found = 0;
            for line in &layout.lines {
                for at in (line.start..line.end).filter(|at| PROSE.as_bytes()[*at] == b' ') {
                    let text = typed(PROSE, at, " a");
                    let (typed_in, after) =
                        painting_of(&mut fs, &mut atlas, &paragraph(&text), &config, &[], scale);
                    if after.lines.len() != before.lines.len() || after.bounds != before.bounds {
                        continue;
                    }
                    let hyphens_after = hyphenated_lines(&typed_in);
                    let crossed = (0..before.lines.len()).any(|line| {
                        before.lines[line] != after.lines[line]
                            && (hyphens.contains(&line) || hyphens_after.contains(&line))
                    });
                    if crossed && hyphens != hyphens_after {
                        redrawn_rows(&mut fs, &mut atlas, &before, &after);
                        found += 1;
                    }
                }
            }
            assert!(found > 0, "no edit moved a hyphen at {scale}x");
        }
    }

    #[test]
    fn the_same_painting_redraws_nothing() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let (_, painting) =
            painting_of(&mut fs, &mut atlas, &paragraph(PROSE), &config(), &[], 2.0);
        let patches = atlas.redraw(&mut fs, &painting, &painting).unwrap();
        assert!(patches.is_empty());
    }

    /// A line more, with the ink's bounds the same, is drawn whole: comparing the lines in pairs
    /// would leave the extra one out, and its ink with it.
    #[test]
    fn a_line_more_inside_the_same_bounds_is_drawn_whole() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let (_, before) = painting_of(&mut fs, &mut atlas, &paragraph(PROSE), &config(), &[], 2.0);
        // The first line's ink again, a line further down, which no layout gives but which
        // keeps within the bounds.
        let mut after = before.clone();
        let again = after.lines[0]
            .iter()
            .map(|mark| Mark {
                top: mark.top + 24 * 2,
                ..*mark
            })
            .collect();
        after.lines.push(again);
        assert_eq!(after.bounds, before.bounds);
        assert!(atlas.redraw(&mut fs, &before, &after).is_none());
    }

    /// A band can only be laid over an image of the same size and place, so a change in the
    /// number of lines, the ink's bounds or the scale is drawn whole.
    #[test]
    fn a_new_crop_line_count_or_scale_is_drawn_whole() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let mut atlas = GlyphAtlas::new();
        let config = config();
        let (_, before) = painting_of(&mut fs, &mut atlas, &paragraph(PROSE), &config, &[], 2.0);

        let longer = format!("{PROSE} And one more sentence goes on to need a line of its own.");
        let (_, more_lines) =
            painting_of(&mut fs, &mut atlas, &paragraph(&longer), &config, &[], 2.0);
        assert_ne!(more_lines.lines.len(), before.lines.len());
        assert!(atlas.redraw(&mut fs, &before, &more_lines).is_none());

        let (_, one_line) = painting_of(
            &mut fs,
            &mut atlas,
            &paragraph("A short line"),
            &config,
            &[],
            2.0,
        );
        let (_, wider) = painting_of(
            &mut fs,
            &mut atlas,
            &paragraph("A short line, then"),
            &config,
            &[],
            2.0,
        );
        assert_eq!(one_line.lines.len(), wider.lines.len());
        assert_ne!(one_line.bounds, wider.bounds);
        assert!(atlas.redraw(&mut fs, &one_line, &wider).is_none());

        let (_, at_one) = painting_of(&mut fs, &mut atlas, &paragraph(PROSE), &config, &[], 1.0);
        assert!(atlas.redraw(&mut fs, &at_one, &before).is_none());

        let (_, empty) = painting_of(&mut fs, &mut atlas, &paragraph(""), &config, &[], 2.0);
        assert!(atlas.redraw(&mut fs, &empty, &empty).is_none());
    }
}
