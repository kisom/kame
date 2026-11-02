//! Glyph rasterization: cosmic-text runs painted into an RGBA bitmap with swash.

use std::collections::HashMap;

use crate::layout::BlockLayout;
use cosmic_text::{CacheKey, CacheKeyFlags, FontSystem, SwashContent, SwashImage};
use image::RgbaImage;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Transform, Vector};

/// Glyph mask cache and the RGBA canvas painted by [`GlyphAtlas::rasterize`].
///
/// Unlike cosmic-text's `SwashCache`, glyphs are rasterized with the subpixel
/// (RGB) format rather than a grayscale alpha mask, so the page gets LCD
/// subpixel antialiasing — the crisp ClearType-style look. The cache is keyed
/// by [`CacheKey`], which already includes the subpixel bin.
pub struct GlyphAtlas {
    context: ScaleContext,
    cache: HashMap<CacheKey, Option<SwashImage>>,
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

    /// Rasterizes one glyph (or returns the cached image), in subpixel format.
    fn get_image(&mut self, fs: &mut FontSystem, cache_key: CacheKey) -> Option<&SwashImage> {
        let Self { context, cache } = self;
        cache
            .entry(cache_key)
            .or_insert_with(|| rasterize_subpixel(fs, context, cache_key))
            .as_ref()
    }

    /// Rasterize `layout` into an `RgbaImage`, returning the image together
    /// with its block-relative offset `(left, top)`.
    ///
    /// Glyphs are painted in `ink` over `background` (the paper colour), using
    /// LCD subpixel coverage, and composited per-channel so the colour fringing
    /// is correct. `highlights` lists byte ranges whose background is a note
    /// wash instead of the paper, so highlighted prose keeps its tint behind
    /// the ink; each entry is `(start, end, background)`.
    ///
    /// The canvas is cropped to the ink of the shaped glyphs, so its size is
    /// the glyph bounding box rather than the full layout column; a block with
    /// no drawable glyphs (empty text, or spaces only) yields a 1×1 transparent
    /// image at offset `(0, 0)` so callers can always upload a texture.
    ///
    /// Glyph positions come from `glyph.physical((0.0, run.line_y), 1.0)` — the
    /// run's baseline must be added to each glyph, otherwise every visual line
    /// is painted on top of the first.
    pub fn rasterize(
        &mut self,
        fs: &mut FontSystem,
        layout: &BlockLayout,
        ink: [u8; 3],
        background: [u8; 3],
        highlights: &[(usize, usize, [u8; 3])],
    ) -> (RgbaImage, i32, i32) {
        let Some((min_x, min_y, max_x, max_y)) = self.ink_bounds(fs, layout) else {
            return (RgbaImage::new(1, 1), 0, 0);
        };

        let width = (max_x - min_x).max(1) as u32;
        let height = (max_y - min_y).max(1) as u32;
        let mut canvas = RgbaImage::new(width, height);
        self.blit(
            fs,
            layout,
            &mut canvas,
            (min_x, min_y),
            ink,
            background,
            highlights,
        );

        (canvas, min_x, min_y)
    }

    /// Bounding box of every rasterized glyph mask, in block-relative pixels:
    /// `(min_x, min_y, max_x, max_y)` with the maxima exclusive. `None` when the
    /// layout draws nothing.
    fn ink_bounds(
        &mut self,
        fs: &mut FontSystem,
        layout: &BlockLayout,
    ) -> Option<(i32, i32, i32, i32)> {
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for run in layout.buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let phys = glyph.physical((0.0, run.line_y), 1.0);
                let Some(image) = self.get_image(fs, phys.cache_key) else {
                    continue;
                };
                if image.placement.width == 0 || image.placement.height == 0 {
                    continue;
                }
                let left = phys.x + image.placement.left;
                let top = phys.y - image.placement.top;
                let right = left + image.placement.width as i32;
                let bottom = top + image.placement.height as i32;

                bounds = Some(match bounds {
                    None => (left, top, right, bottom),
                    Some((min_x, min_y, max_x, max_y)) => (
                        min_x.min(left),
                        min_y.min(top),
                        max_x.max(right),
                        max_y.max(bottom),
                    ),
                });
            }
        }
        bounds
    }

    /// Paint every glyph mask onto `canvas`, whose top-left corner sits at
    /// block-relative `(origin_x, origin_y)`. Glyphs whose byte range falls in
    /// a highlight are painted over that wash colour instead of the paper.
    fn blit(
        &mut self,
        fs: &mut FontSystem,
        layout: &BlockLayout,
        canvas: &mut RgbaImage,
        (origin_x, origin_y): (i32, i32),
        ink: [u8; 3],
        background: [u8; 3],
        highlights: &[(usize, usize, [u8; 3])],
    ) {
        let canvas_w = canvas.width() as i32;
        let canvas_h = canvas.height() as i32;

        for run in layout.buffer.layout_runs() {
            // `glyph.y` is line-relative: the run's baseline is the anchor.
            for glyph in run.glyphs.iter() {
                let phys = glyph.physical((0.0, run.line_y), 1.0);
                let Some(image) = self.get_image(fs, phys.cache_key) else {
                    continue;
                };
                if image.placement.width == 0 || image.placement.height == 0 {
                    continue;
                }

                // A glyph shaped with an explicit colour (a link) keeps it;
                // everything else takes the caller's ink.
                let glyph_ink = glyph.color_opt.map_or(ink, |c| [c.r(), c.g(), c.b()]);
                // A note/marginal/footnote wash behind this glyph becomes the
                // background the subpixel coverage is baked against.
                let glyph_background = highlights
                    .iter()
                    .find(|(start, end, _)| glyph.start < *end && *start < glyph.end)
                    .map(|(_, _, color)| *color)
                    .unwrap_or(background);

                let left = phys.x + image.placement.left - origin_x;
                let top = phys.y - image.placement.top - origin_y;
                let mask_w = image.placement.width as i32;
                let mask_h = image.placement.height as i32;

                for row in 0..mask_h {
                    let y = top + row;
                    if y < 0 || y >= canvas_h {
                        continue;
                    }
                    for col in 0..mask_w {
                        let x = left + col;
                        if x < 0 || x >= canvas_w {
                            continue;
                        }
                        let index = (row * mask_w + col) as usize;
                        let (src, alpha) = sample_pixel(
                            image.content,
                            &image.data,
                            index,
                            glyph_ink,
                            glyph_background,
                        );
                        if alpha == 0 {
                            continue;
                        }
                        let dst = canvas.get_pixel_mut(x as u32, y as u32);
                        blend(dst, src, alpha);
                    }
                }
            }
        }
    }
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

/// Rasterizes one glyph with the swash scaler in the subpixel (RGB) format.
///
/// This mirrors cosmic-text's `swash_image` but with [`Format::Subpixel`]
/// instead of `Format::Alpha`, so the returned mask carries three coverage
/// samples per pixel.
fn rasterize_subpixel(
    fs: &mut FontSystem,
    context: &mut ScaleContext,
    cache_key: CacheKey,
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
    .format(Format::Subpixel)
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

/// Source-over composite a straight-alpha `src`/`alpha` onto `dst`.
fn blend(dst: &mut image::Rgba<u8>, src: [u8; 3], alpha: u8) {
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
    use crate::model::{Block, BlockKind};
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
        let (image, _, _) = atlas.rasterize(&mut fs, &layout, color, background, &[]);

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
        let (image, left, top) = atlas.rasterize(&mut fs, &layout, [0, 0, 0], [255, 255, 255], &[]);

        let canvas_right = left + image.width() as i32;
        let canvas_bottom = top + image.height() as i32;

        // Every glyph's *placed ink* rectangle must lie inside the returned
        // `[left, left + width) x [top, top + height)` window. The rectangle is
        // the pen plus the mask's left side bearing and size — not the pen
        // origin, which sits left of the ink whenever the bearing is positive.
        let mut glyphs = 0;
        for run in layout.buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let phys = glyph.physical((0.0, run.line_y), 1.0);
                let Some(mask) = atlas.get_image(&mut fs, phys.cache_key) else {
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
        let (image, _, top) = atlas.rasterize(&mut fs, &layout, [0, 0, 0], [255, 255, 255], &[]);

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
    fn rasterize_empty_block_is_blank() {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = paragraph("");
        let layout = layout_block(&mut fs, &block, &config());
        let mut atlas = GlyphAtlas::new();
        let (image, _, _) = atlas.rasterize(&mut fs, &layout, [0, 0, 0], [255, 255, 255], &[]);
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
        let (a, _, _) = atlas.rasterize(&mut fs, &l1, [0, 0, 0], [255, 255, 255], &[]);

        let second = paragraph("World");
        let l2 = layout_block(&mut fs, &second, &config);
        let (b, _, _) = atlas.rasterize(&mut fs, &l2, [0, 0, 0], [255, 255, 255], &[]);

        assert!(ink_pixels(&a) > 0 && ink_pixels(&b) > 0);
        assert_ne!(
            a.as_raw(),
            b.as_raw(),
            "the second rasterize reused the first canvas"
        );
    }
}
