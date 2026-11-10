//! Omarchy's current theme, which kame can follow: with the setting on, the page and the
//! interface take its colours, and change when it does. Omarchy keeps the theme's colours in
//! `current/theme/colors.toml` in its folder under `~/.local/state`, and rewrites
//! `current/theme.generation` on every switch. Exports and published sites never see any of it.
//!
//! `colors.toml` is a flat list of `key = "value"` lines: `mode` is `"dark"` or `"light"`, and
//! every other key is a colour, written `"#rrggbb"`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use eframe::egui;

use crate::Palette;

/// A colour as red, green and blue.
pub(crate) type Rgb = [u8; 3];

/// The colours a theme can name, besides `background` and `foreground`.
pub(crate) const COLOURS: [&str; 24] = [
    "accent",
    "selection",
    "selection_foreground",
    "muted",
    "dark_background",
    "darker_background",
    "lighter_background",
    "dark_foreground",
    "light_foreground",
    "bright_foreground",
    "red",
    "yellow",
    "orange",
    "green",
    "cyan",
    "blue",
    "magenta",
    "brown",
    "bright_red",
    "bright_yellow",
    "bright_green",
    "bright_cyan",
    "bright_blue",
    "bright_magenta",
];

/// A theme's colours, as its `colors.toml` gives them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Theme {
    /// Whether it's a dark theme: its `mode`, or with none, whether its background is darker than
    /// its foreground.
    pub(crate) dark: bool,
    pub(crate) background: Rgb,
    pub(crate) foreground: Rgb,
    /// The others it gives, by the names in [`COLOURS`].
    colours: HashMap<&'static str, Rgb>,
}

impl Theme {
    /// The colour the theme calls `name`, if it gives one.
    pub(crate) fn colour(&self, name: &str) -> Option<Rgb> {
        debug_assert!(COLOURS.contains(&name), "{name} isn't a theme's colour");
        self.colours.get(name).copied()
    }
}

/// The theme in `text`, a `colors.toml`. A line that can't be read and a key that isn't known are
/// passed over, so a theme with something newer in it still reads. Without a background and a
/// foreground it isn't a theme.
pub(crate) fn parse(text: &str) -> Option<Theme> {
    let mut mode = None;
    let (mut background, mut foreground) = (None, None);
    let mut colours = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(value) = quoted(value) else {
            continue;
        };
        match key.trim() {
            "mode" => match value {
                "dark" => mode = Some(true),
                "light" => mode = Some(false),
                _ => {}
            },
            "background" => background = colour(value).or(background),
            "foreground" => foreground = colour(value).or(foreground),
            key => {
                if let (Some(name), Some(colour)) =
                    (COLOURS.iter().find(|name| **name == key), colour(value))
                {
                    colours.insert(*name, colour);
                }
            }
        }
    }
    let (background, foreground) = (background?, foreground?);
    Some(Theme {
        dark: mode.unwrap_or_else(|| luminance(background) < luminance(foreground)),
        background,
        foreground,
        colours,
    })
}

/// What's between the quotes of `value`, the part of a line after its `=`, with nothing after
/// the closing quote but a comment.
fn quoted(value: &str) -> Option<&str> {
    let rest = value.trim().strip_prefix('"')?;
    let (inside, after) = rest.split_once('"')?;
    let after = after.trim();
    (after.is_empty() || after.starts_with('#')).then_some(inside)
}

/// The colour `text` writes as `#rgb` or `#rrggbb`, in either case.
fn colour(text: &str) -> Option<Rgb> {
    let digits = text.strip_prefix('#')?;
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    match digits.len() {
        3 => {
            let channel = |shift: u32| ((value >> shift) & 0xf) as u8 * 0x11;
            Some([channel(8), channel(4), channel(0)])
        }
        6 => Some([(value >> 16) as u8, (value >> 8) as u8, value as u8]),
        _ => None,
    }
}

/// The least contrast with the paper for words: the footnotes' numbers, and the status row's
/// messages.
pub(crate) const TEXT_CONTRAST: f32 = 4.5;
/// The least contrast with the paper for a mark: the caret, the wavy lines under a mistake, and a
/// note's dot.
pub(crate) const MARK_CONTRAST: f32 = 3.0;

/// How far apart `a` and `b` look, from 1 for the same to 21 for black and white, as WCAG works it
/// out.
pub(crate) fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `share` of the way from `from` to `toward`.
pub(crate) fn mix(from: Rgb, toward: Rgb, share: f32) -> Rgb {
    let channel = |at: usize| {
        let (from, toward) = (from[at] as f32, toward[at] as f32);
        (from + (toward - from) * share).round().clamp(0.0, 255.0) as u8
    };
    [channel(0), channel(1), channel(2)]
}

/// `ink`, or as little of the way to `toward` as it takes, in twentieths, to have at least `least`
/// contrast with each of `papers`. Short of that, `toward` itself.
pub(crate) fn legible(ink: Rgb, toward: Rgb, papers: &[Rgb], least: f32) -> Rgb {
    (0..=20)
        .map(|step| mix(ink, toward, step as f32 / 20.0))
        .find(|ink| papers.iter().all(|paper| contrast(*ink, *paper) >= least))
        .unwrap_or(toward)
}

/// How bright `colour` looks, from 0 for black to 1 for white, as WCAG works it out.
pub(crate) fn luminance(colour: Rgb) -> f32 {
    let linear = |channel: u8| {
        let c = channel as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(colour[0]) + 0.7152 * linear(colour[1]) + 0.0722 * linear(colour[2])
}

/// The interface around the page in `theme`'s colours, beside the page in `palette`, the one
/// made from it: egui's own light or dark look for the theme's mode, with the theme's colours
/// in place of egui's greys.
pub(crate) fn visuals(theme: &Theme, palette: &Palette) -> egui::Visuals {
    let mut visuals = if theme.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let rgb = |[r, g, b]: Rgb| egui::Color32::from_rgb(r, g, b);
    let ink = rgb(theme.foreground);
    let bright = theme.colour("bright_foreground").map_or(ink, rgb);
    // The panels and windows, a button at rest, and one under the pointer.
    let panel = rgb(theme.colour("dark_background").unwrap_or(theme.background));
    let raised = theme.colour("lighter_background").map_or(panel, rgb);
    let hovered = theme.colour("selection").map_or(raised, rgb);
    visuals.panel_fill = panel;
    visuals.window_fill = panel;
    visuals.faint_bg_color = raised;
    visuals.code_bg_color = raised;
    // A field is the page's paper, with the page's caret, and what's chosen in it looks as it
    // does on the page.
    visuals.extreme_bg_color = rgb(theme.background);
    visuals.text_cursor.stroke.color = palette.caret_ink;
    visuals.selection.bg_fill = rgb(crate::overlay_on(
        palette.background,
        palette.selection_fill,
    ));
    visuals.selection.stroke.color = ink;
    if let Some(accent) = theme.colour("accent") {
        visuals.hyperlink_color = rgb(accent);
    }
    visuals.error_fg_color = palette.error_ink;
    if let Some(orange) = theme.colour("orange") {
        visuals.warn_fg_color = rgb(orange);
    }
    let widgets = &mut visuals.widgets;
    for (look, fill, text) in [
        (&mut widgets.noninteractive, panel, ink),
        (&mut widgets.inactive, raised, ink),
        (&mut widgets.hovered, hovered, bright),
        (&mut widgets.active, hovered, bright),
        (&mut widgets.open, raised, ink),
    ] {
        look.bg_fill = fill;
        look.weak_bg_fill = fill;
        look.fg_stroke.color = text;
    }
    // The lines between things, and around the windows.
    if let Some(muted) = theme.colour("muted") {
        visuals.window_stroke.color = rgb(muted);
        visuals.widgets.noninteractive.bg_stroke.color = rgb(muted);
    }
    visuals
}

/// Omarchy's current theme, as kame keeps up with it.
pub(crate) struct Omarchy {
    /// Where Omarchy keeps what's current, if there's a home to look in.
    folder: Option<PathBuf>,
    /// Whether it has a theme folder, which is what offers the setting.
    pub(crate) present: bool,
    /// What said which theme it was when it was last read.
    seen: Option<Stamp>,
    /// The theme last read, if one has been.
    pub(crate) theme: Option<Theme>,
    /// Whether the window had the keyboard when kame last looked, so that getting it back looks
    /// again.
    pub(crate) focused: bool,
}

/// What tells one theme from the next without reading it: the token Omarchy rewrites on every
/// switch, or with none, when the colours' file was written and how long it is.
#[derive(Clone, Debug, PartialEq)]
enum Stamp {
    Token(Vec<u8>),
    Written(Option<SystemTime>, u64),
}

impl Omarchy {
    pub(crate) fn new(folder: Option<PathBuf>) -> Omarchy {
        Omarchy {
            folder,
            present: false,
            seen: None,
            theme: None,
            focused: true,
        }
    }

    /// Looks for the theme folder only, which is all there is to do while kame isn't following
    /// the theme. What was read before is forgotten, so that following it again reads it afresh.
    pub(crate) fn look_for_folder(&mut self) {
        self.present = self.theme_folder().is_some_and(|folder| folder.is_dir());
        self.seen = None;
        self.theme = None;
    }

    /// Looks at the theme, reading its colours only if the token says they have changed, and
    /// returns whether the theme in use has. Colours that can't be read leave the theme as it
    /// was, to be read again next time: a switch can be caught half done. So can a missing theme
    /// folder, which Omarchy removes before it moves the next one in, so a theme already read is
    /// kept, and the setting with it. If Omarchy really has gone, that theme stays until kame is
    /// started again or the setting is turned off. With none read, there's no Omarchy.
    ///
    /// Omarchy writes the token after the colours are in place, so the token has to be read
    /// first: one read after the colours could be a later switch's, whose colours would then
    /// never be read.
    pub(crate) fn look(&mut self) -> bool {
        let Some(folder) = self.theme_folder().filter(|folder| folder.is_dir()) else {
            self.seen = None;
            self.present = self.theme.is_some();
            return false;
        };
        self.present = true;
        let colours = folder.join("colors.toml");
        let stamp = self.stamp(&colours);
        if stamp.is_some() && stamp == self.seen {
            return false;
        }
        let Some(theme) = std::fs::read_to_string(&colours)
            .ok()
            .and_then(|text| parse(&text))
        else {
            return false;
        };
        self.seen = stamp;
        let changed = self.theme.as_ref() != Some(&theme);
        self.theme = Some(theme);
        changed
    }

    fn theme_folder(&self) -> Option<PathBuf> {
        self.folder.as_ref().map(|folder| folder.join("theme"))
    }

    /// What tells this theme from the last: the token, or the colours' file's time and length.
    fn stamp(&self, colours: &Path) -> Option<Stamp> {
        let folder = self.folder.as_ref()?;
        if let Ok(token) = std::fs::read(folder.join("theme.generation")) {
            return Some(Stamp::Token(token));
        }
        let written = std::fs::metadata(colours).ok()?;
        Some(Stamp::Written(written.modified().ok(), written.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        omarchy_dir, overlay_on, test_temp_dir, Headless, KameApp, KameConfig, MenuAction,
        FILE_CHECK_SECONDS,
    };
    use eframe::egui::Color32;
    use kame_core::session::Session;

    const NORD: &str = include_str!("../tests/omarchy/nord-dark.colors.toml");
    const LATTE: &str = include_str!("../tests/omarchy/catppuccin-latte-light.colors.toml");

    fn rgb(hex: u32) -> Rgb {
        [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
    }

    /// Every colour `theme` gives, by name, with its background and foreground.
    fn every_colour(theme: &Theme) -> Vec<(&'static str, Rgb)> {
        let mut all = vec![
            ("background", theme.background),
            ("foreground", theme.foreground),
        ];
        all.extend(
            COLOURS
                .iter()
                .filter_map(|name| theme.colour(name).map(|colour| (*name, colour))),
        );
        all
    }

    #[test]
    fn the_nord_theme_reads_with_every_colour() {
        let theme = parse(NORD).expect("a theme");
        assert!(theme.dark);
        let expected = [
            ("background", 0x2e3440),
            ("foreground", 0xd8dee9),
            ("accent", 0x81a1c1),
            ("selection", 0x434c5e),
            ("muted", 0x4c566a),
            ("dark_background", 0x222730),
            ("darker_background", 0x191c23),
            ("lighter_background", 0x3b4252),
            ("dark_foreground", 0x667080),
            ("light_foreground", 0xadb5c4),
            ("bright_foreground", 0xd8dee9),
            ("red", 0xbf616a),
            ("yellow", 0xebcb8b),
            ("orange", 0xd5967a),
            ("green", 0xa3be8c),
            ("cyan", 0x88c0d0),
            ("blue", 0x81a1c1),
            ("magenta", 0xb48ead),
            ("brown", 0x6a4b3d),
            ("bright_red", 0xbf616a),
            ("bright_yellow", 0xebcb8b),
            ("bright_green", 0xa3be8c),
            ("bright_cyan", 0x8fbcbb),
            ("bright_blue", 0x81a1c1),
            ("bright_magenta", 0xb48ead),
        ];
        let mut expected: Vec<(&str, Rgb)> = expected
            .into_iter()
            .map(|(name, hex)| (name, rgb(hex)))
            .collect();
        let mut read = every_colour(&theme);
        expected.sort();
        read.sort();
        assert_eq!(read, expected);
        assert_eq!(theme.colour("selection_foreground"), None, "nord has none");
    }

    #[test]
    fn the_latte_theme_reads_with_every_colour() {
        let theme = parse(LATTE).expect("a theme");
        assert!(!theme.dark);
        let expected = [
            ("background", 0xeff1f5),
            ("foreground", 0x4c4f69),
            ("accent", 0x1e66f5),
            ("selection", 0xccd0da),
            ("muted", 0xacb0be),
            ("dark_background", 0xe3e4e8),
            ("darker_background", 0xd7d8dc),
            ("lighter_background", 0xdce0e8),
            ("dark_foreground", 0x9ca0b0),
            ("light_foreground", 0x5c5f77),
            ("bright_foreground", 0x4c4f69),
            ("red", 0xd20f39),
            ("yellow", 0xdf8e1d),
            ("orange", 0xd84e2b),
            ("green", 0x40a02b),
            ("cyan", 0x179299),
            ("blue", 0x1e66f5),
            ("magenta", 0xea76cb),
            ("brown", 0x6c2715),
            ("bright_red", 0xd20f39),
            ("bright_yellow", 0xdf8e1d),
            ("bright_green", 0x40a02b),
            ("bright_cyan", 0x179299),
            ("bright_blue", 0x1e66f5),
            ("bright_magenta", 0xea76cb),
        ];
        let mut expected: Vec<(&str, Rgb)> = expected
            .into_iter()
            .map(|(name, hex)| (name, rgb(hex)))
            .collect();
        let mut read = every_colour(&theme);
        expected.sort();
        read.sort();
        assert_eq!(read, expected);
    }

    #[test]
    fn comments_blank_lines_and_unknown_keys_are_passed_over() {
        let theme = parse(
            "# a theme of my own\n\
             \n\
             mode = \"dark\"\n\
             \t\n\
             background = \"#101010\"\n\
             # foreground = \"#000000\"\n\
             foreground = \"#f0f0f0\"\n\
             sparkle = \"#ff00ff\"\n\
             name = \"mine\"\n",
        )
        .expect("a theme");
        assert_eq!(theme.background, [0x10; 3]);
        assert_eq!(theme.foreground, [0xf0; 3]);
        assert_eq!(every_colour(&theme).len(), 2, "nothing else is kept");
    }

    #[test]
    fn a_line_that_cant_be_read_is_passed_over() {
        let theme = parse(
            "background = \"#101010\"\n\
             foreground = \"#f0f0f0\"\n\
             accent \"#ff0000\"\n\
             red = #ff0000\n\
             blue = \"#12345\"\n\
             green = \"#gg0000\"\n\
             cyan = \"#00ffff\n\
             yellow = \"#ffff00\" # the sun\n\
             = \"#00ff00\"\n",
        )
        .expect("a theme");
        assert_eq!(theme.colour("accent"), None);
        assert_eq!(theme.colour("red"), None);
        assert_eq!(theme.colour("blue"), None);
        assert_eq!(theme.colour("green"), None);
        assert_eq!(theme.colour("cyan"), None);
        assert_eq!(
            theme.colour("yellow"),
            Some([0xff, 0xff, 0]),
            "a comment after"
        );
    }

    #[test]
    fn a_short_colour_and_upper_case_read() {
        let theme = parse(
            "background = \"#FFF\"\n\
             foreground = \"#1A2B3C\"\n\
             accent = \"#a5C\"\n\
             mode = \"light\"\n",
        )
        .expect("a theme");
        assert_eq!(theme.background, [0xff, 0xff, 0xff]);
        assert_eq!(theme.foreground, [0x1a, 0x2b, 0x3c]);
        assert_eq!(theme.colour("accent"), Some([0xaa, 0x55, 0xcc]));
    }

    #[test]
    fn with_no_mode_a_background_darker_than_the_foreground_is_dark() {
        let dark = parse("background = \"#202020\"\nforeground = \"#e0e0e0\"\n").unwrap();
        assert!(dark.dark);
        let light = parse("background = \"#f8f4ec\"\nforeground = \"#302820\"\n").unwrap();
        assert!(!light.dark);
        let unknown = "mode = \"sepia\"\nbackground = \"#f8f4ec\"\nforeground = \"#302820\"\n";
        assert!(!parse(unknown).unwrap().dark, "a mode it doesn't know");
        let said = "mode = \"light\"\nbackground = \"#202020\"\nforeground = \"#e0e0e0\"\n";
        assert!(!parse(said).unwrap().dark, "the mode said is taken");
    }

    #[test]
    fn without_a_background_or_a_foreground_it_isnt_a_theme() {
        assert_eq!(
            parse("foreground = \"#e0e0e0\"\naccent = \"#ff0000\"\n"),
            None
        );
        assert_eq!(parse("background = \"#202020\"\n"), None);
        assert_eq!(
            parse("background = \"#2020\"\nforeground = \"#e0e0e0\"\n"),
            None
        );
        assert_eq!(parse(""), None);
        assert_eq!(parse("\n\n# nothing\n"), None);
    }

    fn opaque(hex: u32) -> Color32 {
        let [r, g, b] = rgb(hex);
        Color32::from_rgb(r, g, b)
    }

    fn washed(hex: u32, alpha: u8) -> Color32 {
        let [r, g, b] = rgb(hex);
        Color32::from_rgba_unmultiplied(r, g, b, alpha)
    }

    fn rgb_of(colour: Color32) -> Rgb {
        [colour.r(), colour.g(), colour.b()]
    }

    #[test]
    fn the_nord_palette_is_the_themes_colours() {
        let palette = Palette::from_theme(&parse(NORD).unwrap());
        assert_eq!(palette.background, opaque(0x2e3440));
        assert_eq!(palette.body_ink, rgb(0xd8dee9));
        assert_eq!(palette.apparatus_ink, rgb(0xa8aeba), "28% toward the paper");
        assert_eq!(palette.caret_ink, opaque(0x81a1c1));
        assert_eq!(palette.selection_fill, washed(0x81a1c1, 66));
        assert_eq!(palette.spelling_mark, opaque(0xbf616a));
        assert_eq!(palette.grammar_mark, opaque(0x81a1c1));
        // Nord's yellow is light, and at kame's own 80 the words on it would be a little short of
        // 4.5.
        assert_eq!(palette.note_highlight, washed(0xebcb8b, 77));
        assert_eq!(palette.footnote_highlight, washed(0x88c0d0, 60));
        assert_eq!(palette.footnote_mark, opaque(0x88c0d0));
        assert_eq!(palette.marginal_highlight, washed(0xd8dee9, 44));
        assert_eq!(palette.rule_ink, opaque(0x7b818c), "55% toward the paper");
        assert_eq!(palette.note_mark, opaque(0xebcb8b));
        assert_eq!(palette.list_marker_ink, opaque(0xd8dee9));
        assert_eq!(palette.dim_veil, washed(0x2e3440, 168));
        // Nord's red is 3.05 on its paper, which is enough for a wavy line and not for words:
        // the messages have it 35% toward the foreground.
        assert_eq!(palette.error_ink, opaque(0xc88d96));
        assert_eq!(palette.notice_ink, opaque(0xa3be8c));
    }

    #[test]
    fn the_latte_palette_is_the_themes_colours() {
        let palette = Palette::from_theme(&parse(LATTE).unwrap());
        assert_eq!(palette.background, opaque(0xeff1f5));
        assert_eq!(palette.body_ink, rgb(0x4c4f69));
        assert_eq!(palette.apparatus_ink, rgb(0x7a7c90));
        assert_eq!(palette.caret_ink, opaque(0x1e66f5));
        assert_eq!(palette.selection_fill, washed(0x1e66f5, 62));
        assert_eq!(palette.spelling_mark, opaque(0xd20f39));
        assert_eq!(palette.grammar_mark, opaque(0x1e66f5));
        assert_eq!(palette.note_highlight, washed(0xdf8e1d, 70));
        assert_eq!(palette.footnote_highlight, washed(0x179299, 44));
        // Latte's teal is 3.31 on its paper, too faint for the footnotes' numbers, and its
        // yellow 2.31, too faint for a note's dot: each is taken toward the foreground, 40% and
        // 25%, until it isn't.
        assert_eq!(palette.footnote_mark, opaque(0x2c7786));
        assert_eq!(palette.marginal_highlight, washed(0x4c4f69, 34));
        assert_eq!(palette.rule_ink, opaque(0xa6a8b6));
        assert_eq!(palette.note_mark, opaque(0xba7e30));
        assert_eq!(palette.list_marker_ink, opaque(0x4c4f69));
        assert_eq!(palette.dim_veil, washed(0xeff1f5, 168));
        // The messages are on the panels as well as the page, and the panels are darker: the red
        // goes 10% toward the foreground and the green, 2.63 on them, 60%.
        assert_eq!(palette.error_ink, opaque(0xc5153e));
        assert_eq!(palette.notice_ink, opaque(0x476f50));
    }

    #[test]
    fn a_colour_the_theme_leaves_out_is_kames_own() {
        for (mode, own) in [("dark", Palette::DARK), ("light", Palette::LIGHT)] {
            let text =
                format!("mode = \"{mode}\"\nbackground = \"#102030\"\nforeground = \"#f0f0f0\"\n");
            let palette = Palette::from_theme(&parse(&text).unwrap());
            assert_eq!(palette.background, opaque(0x102030));
            assert_eq!(palette.body_ink, [0xf0; 3]);
            assert_eq!(palette.list_marker_ink, opaque(0xf0f0f0));
            assert_eq!(palette.apparatus_ink, mix([0xf0; 3], rgb(0x102030), 0.28));
            let [r, g, b] = mix([0xf0; 3], rgb(0x102030), 0.55);
            assert_eq!(palette.rule_ink, Color32::from_rgb(r, g, b));
            let alpha = |colour: Color32| colour.a();
            assert_eq!(
                alpha(palette.marginal_highlight),
                alpha(own.marginal_highlight)
            );
            assert_eq!(palette.dim_veil, washed(0x102030, alpha(own.dim_veil)));
            assert_eq!(palette.caret_ink, own.caret_ink, "{mode}");
            assert_eq!(palette.selection_fill, own.selection_fill);
            assert_eq!(palette.spelling_mark, own.spelling_mark);
            assert_eq!(palette.grammar_mark, own.grammar_mark);
            assert_eq!(palette.note_highlight, own.note_highlight);
            assert_eq!(palette.note_mark, own.note_mark);
            assert_eq!(palette.footnote_highlight, own.footnote_highlight);
            assert_eq!(palette.footnote_mark, own.footnote_mark);
            assert_eq!(palette.error_ink, own.error_ink);
            assert_eq!(palette.notice_ink, own.notice_ink);
        }
    }

    #[test]
    fn either_themes_words_and_marks_can_be_seen_on_its_paper() {
        for text in [NORD, LATTE] {
            let theme = parse(text).unwrap();
            let palette = Palette::from_theme(&theme);
            let paper = rgb_of(palette.background);
            let panel = theme.colour("dark_background").unwrap();
            let body = palette.body_ink;
            assert!(contrast(body, paper) >= 4.5, "body text");
            assert!(contrast(palette.apparatus_ink, paper) >= 3.0, "notes");
            for wash in [
                palette.selection_fill,
                palette.note_highlight,
                palette.footnote_highlight,
                palette.marginal_highlight,
            ] {
                let under = overlay_on(palette.background, wash);
                assert!(contrast(body, under) >= 4.5, "words on {wash:?}");
            }
            for mark in [
                palette.caret_ink,
                palette.spelling_mark,
                palette.grammar_mark,
                palette.note_mark,
            ] {
                assert!(contrast(rgb_of(mark), paper) >= 3.0, "{mark:?}");
            }
            assert!(contrast(rgb_of(palette.footnote_mark), paper) >= 4.5);
            for ink in [palette.error_ink, palette.notice_ink] {
                for under in [paper, panel] {
                    assert!(contrast(rgb_of(ink), under) >= 4.5, "{ink:?}");
                }
            }
        }
    }

    #[test]
    fn the_interface_takes_the_themes_colours() {
        let theme = parse(NORD).unwrap();
        let palette = Palette::from_theme(&theme);
        let visuals = visuals(&theme, &palette);
        assert!(visuals.dark_mode);
        let panel = opaque(0x222730);
        assert_eq!(visuals.panel_fill, panel, "the dark background");
        assert_eq!(visuals.window_fill, panel);
        assert_eq!(visuals.widgets.noninteractive.bg_fill, panel);
        assert_eq!(
            visuals.extreme_bg_color,
            opaque(0x2e3440),
            "fields are the paper"
        );
        assert_eq!(visuals.text_edit_bg_color, None);
        let ink = opaque(0xd8dee9);
        assert_eq!(visuals.widgets.noninteractive.fg_stroke.color, ink);
        assert_eq!(visuals.widgets.inactive.fg_stroke.color, ink);
        assert_eq!(
            visuals.widgets.hovered.fg_stroke.color, ink,
            "bright foreground"
        );
        assert_eq!(visuals.widgets.inactive.weak_bg_fill, opaque(0x3b4252));
        assert_eq!(visuals.widgets.hovered.weak_bg_fill, opaque(0x434c5e));
        assert_eq!(visuals.widgets.active.weak_bg_fill, opaque(0x434c5e));
        assert_eq!(visuals.hyperlink_color, opaque(0x81a1c1));
        assert_eq!(visuals.text_cursor.stroke.color, palette.caret_ink);
        // What's chosen looks as it does on the page, with the words in the foreground.
        let [r, g, b] = overlay_on(palette.background, palette.selection_fill);
        assert_eq!(visuals.selection.bg_fill, Color32::from_rgb(r, g, b));
        assert_eq!(visuals.selection.stroke.color, ink);
        let muted = opaque(0x4c566a);
        assert_eq!(visuals.window_stroke.color, muted);
        assert_eq!(visuals.widgets.noninteractive.bg_stroke.color, muted);
        assert_eq!(visuals.error_fg_color, palette.error_ink);

        let latte = parse(LATTE).unwrap();
        let visuals = super::visuals(&latte, &Palette::from_theme(&latte));
        assert!(!visuals.dark_mode);
        assert_eq!(visuals.panel_fill, opaque(0xe3e4e8));
        assert_eq!(
            visuals.widgets.noninteractive.fg_stroke.color,
            opaque(0x4c4f69)
        );
    }

    #[test]
    fn a_theme_with_only_its_background_and_foreground_colours_the_interface() {
        let theme = parse("background = \"#102030\"\nforeground = \"#f0f0f0\"\n").unwrap();
        let visuals = visuals(&theme, &Palette::from_theme(&theme));
        assert_eq!(visuals.panel_fill, opaque(0x102030));
        assert_eq!(visuals.window_fill, opaque(0x102030));
        assert_eq!(visuals.widgets.inactive.weak_bg_fill, opaque(0x102030));
        assert_eq!(visuals.widgets.hovered.fg_stroke.color, opaque(0xf0f0f0));
        let own = egui::Visuals::dark();
        assert_eq!(visuals.hyperlink_color, own.hyperlink_color);
        assert_eq!(visuals.window_stroke, own.window_stroke);
    }

    /// Puts `colours` in this test's own Omarchy folder as the current theme's, and `token` as
    /// its generation, leaving either out with `None`. Returns the folder.
    fn put_theme(colours: Option<&str>, token: Option<&str>) -> std::path::PathBuf {
        let current = omarchy_dir().unwrap();
        std::fs::create_dir_all(current.join("theme")).unwrap();
        let put = |path: &Path, text: Option<&str>| match text {
            Some(text) => std::fs::write(path, text).unwrap(),
            None => {
                let _ = std::fs::remove_file(path);
            }
        };
        put(&current.join("theme").join("colors.toml"), colours);
        put(&current.join("theme.generation"), token);
        current
    }

    /// kame on a document with some words, as it launches with the setting `follow`.
    fn launch(follow: bool) -> Headless {
        let folder = test_temp_dir().join(format!("theme-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut session = Session::open(&folder.join("doc.kame")).unwrap();
        session.insert("Some words.");
        launch_with(
            session,
            KameConfig {
                follow_omarchy: follow,
                ..KameConfig::default()
            },
        )
    }

    fn launch_with(session: Session, config: KameConfig) -> Headless {
        let mut page = Headless::new(Session::anonymous().unwrap());
        let creation = eframe::CreationContext::_new_kittest(page.ctx.clone());
        page.app = KameApp::new(&creation, session, None, config);
        page.frame(Vec::new());
        page
    }

    /// The frame kame wakes for when nothing else is happening, once it's time to look again.
    fn wake(page: &mut Headless) {
        let due = std::time::Duration::from_secs_f32(FILE_CHECK_SECONDS + 1.0);
        page.app.last_file_check = std::time::Instant::now().checked_sub(due).unwrap();
        page.frame(Vec::new());
    }

    /// What the first block was last drawn as.
    fn drawn(page: &Headless) -> Vec<egui::TextureId> {
        let block = page.app.session.document().blocks[0].id;
        page.app.textures.get(block).expect("drawn").texture_ids()
    }

    fn themed(text: &str) -> Palette {
        Palette::from_theme(&parse(text).unwrap())
    }

    #[test]
    fn following_the_theme_colours_the_page_and_the_interface() {
        put_theme(Some(NORD), Some("3622320-1793907493360538"));
        let page = launch(true);
        assert_eq!(page.app.palette, themed(NORD));
        let visuals = page.ctx.global_style().visuals.clone();
        assert!(visuals.dark_mode);
        assert_eq!(visuals.panel_fill, opaque(0x222730));
        assert!(!page.app.dark, "kame's own choice is kept");
    }

    #[test]
    fn a_switch_is_taken_up_at_the_next_wake_and_the_page_inked_again() {
        let current = put_theme(Some(NORD), Some("3622320-1793907493360538"));
        let mut page = launch(true);
        let before = drawn(&page);

        put_theme(Some(LATTE), Some("3622321-1793907500000000"));
        page.frame(Vec::new());
        assert_eq!(page.app.palette, themed(NORD), "not until the wake");
        wake(&mut page);

        assert_eq!(page.app.palette, themed(LATTE));
        assert!(!page.ctx.global_style().visuals.dark_mode);
        assert_ne!(drawn(&page), before, "drawn again, in the new ink");
        assert!(current.join("theme.generation").exists());
    }

    /// One frame as `ui` runs it, as far as the theme goes: what the panels drawn this frame are
    /// coloured with, which egui fixes as the frame begins, then the frame's input, and how soon
    /// it asks for the next.
    fn panels_and_next_frame(page: &mut Headless) -> (Color32, std::time::Duration) {
        page.time += 0.02;
        let input = egui::RawInput {
            time: Some(page.time),
            ..Default::default()
        };
        let app = &mut page.app;
        let mut panels = Color32::TRANSPARENT;
        let output = page.ctx.run_ui(input, |ui| {
            panels = ui.visuals().panel_fill;
            app.handle_input(ui.ctx());
        });
        let next = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        output.drop_without_applying_deltas();
        (panels, next)
    }

    #[test]
    fn a_switch_in_the_background_reaches_the_interface_on_the_very_next_frame() {
        put_theme(Some(NORD), Some("1"));
        let mut page = launch(true);
        // Frames with nothing to look at, so that nothing else is asking for another. egui asks
        // for one after the first frame drawn this way, telling the window its theme.
        panels_and_next_frame(&mut page);
        let (_, idle) = panels_and_next_frame(&mut page);
        let wake_in = std::time::Duration::from_secs_f32(FILE_CHECK_SECONDS);
        assert_eq!(idle, wake_in);
        put_theme(Some(LATTE), Some("2"));

        let due = std::time::Duration::from_secs_f32(FILE_CHECK_SECONDS + 1.0);
        page.app.last_file_check = std::time::Instant::now().checked_sub(due).unwrap();
        let (panels, next) = panels_and_next_frame(&mut page);
        // The page takes the theme in this frame, and the panels, already begun, can't.
        assert_eq!(page.app.palette, themed(LATTE));
        assert_eq!(panels, opaque(0x222730), "nord's, fixed as the frame began");
        assert_eq!(
            next,
            std::time::Duration::ZERO,
            "the next frame is asked for at once"
        );

        let (panels, _) = panels_and_next_frame(&mut page);
        assert_eq!(panels, opaque(0xe3e4e8), "latte's");
    }

    #[test]
    fn with_the_same_token_nothing_is_read_or_dropped() {
        put_theme(Some(NORD), Some("3622320-1793907493360538"));
        let mut page = launch(true);
        let before = drawn(&page);

        // Only the token is read, so a file changed under the same one isn't.
        put_theme(Some(LATTE), Some("3622320-1793907493360538"));
        wake(&mut page);
        wake(&mut page);

        assert_eq!(page.app.palette, themed(NORD));
        assert_eq!(drawn(&page), before, "nothing dropped");
    }

    #[test]
    fn colours_that_cant_be_read_after_a_switch_keep_the_theme_until_they_can() {
        put_theme(Some(NORD), Some("3622320-1793907493360538"));
        let mut page = launch(true);
        let before = drawn(&page);

        put_theme(
            Some("mode = \"light\"\nbackgr"),
            Some("3622321-1793907500000000"),
        );
        wake(&mut page);
        assert_eq!(page.app.palette, themed(NORD), "half written");
        assert_eq!(drawn(&page), before);
        put_theme(None, Some("3622321-1793907500000000"));
        wake(&mut page);
        assert_eq!(page.app.palette, themed(NORD), "not there at all");

        put_theme(Some(LATTE), Some("3622321-1793907500000000"));
        wake(&mut page);
        assert_eq!(page.app.palette, themed(LATTE), "read once it can be");
    }

    #[test]
    fn without_a_token_the_colours_file_says_when_it_has_changed() {
        put_theme(Some(NORD), None);
        let mut page = launch(true);
        assert_eq!(page.app.palette, themed(NORD));
        let before = drawn(&page);
        wake(&mut page);
        assert_eq!(drawn(&page), before, "nothing changed");

        // The two files are a byte apart in length, whenever they were written.
        put_theme(Some(LATTE), None);
        wake(&mut page);
        assert_eq!(page.app.palette, themed(LATTE));
    }

    #[test]
    fn with_no_theme_to_read_kame_keeps_its_own_colours_and_keeps_looking() {
        put_theme(None, Some("1"));
        let mut page = launch(true);
        assert_eq!(page.app.palette, Palette::LIGHT);
        assert!(!page.ctx.global_style().visuals.dark_mode);
        assert!(page.app.theme_in_use().is_none());

        put_theme(Some(NORD), Some("2"));
        wake(&mut page);
        assert_eq!(page.app.palette, themed(NORD));
    }

    #[test]
    fn a_theme_folder_gone_mid_switch_keeps_the_theme_until_the_next_is_in() {
        let current = put_theme(Some(NORD), Some("3622320-1793907493360538"));
        let mut omarchy = Omarchy::new(Some(current.clone()));
        assert!(omarchy.look());
        assert_eq!(omarchy.theme, parse(NORD));

        // Omarchy removes the theme folder before it moves the next one in.
        std::fs::remove_dir_all(current.join("theme")).unwrap();
        assert!(!omarchy.look());
        assert_eq!(omarchy.theme, parse(NORD), "kept");
        assert!(omarchy.present, "the setting is still there to turn off");

        put_theme(Some(LATTE), Some("3622321-1793907500000000"));
        assert!(omarchy.look());
        assert_eq!(omarchy.theme, parse(LATTE));

        // With no theme read yet, a missing folder is no Omarchy.
        let mut none = Omarchy::new(Some(test_temp_dir().join("no-omarchy").join("current")));
        assert!(!none.look());
        assert!(!none.present);
        assert_eq!(none.theme, None);
    }

    #[test]
    fn getting_the_keyboard_back_looks_at_once() {
        put_theme(Some(NORD), Some("1"));
        let mut page = launch(true);
        put_theme(Some(LATTE), Some("2"));

        // The last frame was in the background, and this one has the keyboard.
        page.app.omarchy.focused = false;
        page.frame(Vec::new());

        assert_eq!(page.app.palette, themed(LATTE));
    }

    #[test]
    fn a_followed_theme_wakes_kame_without_a_file_to_check() {
        put_theme(Some(NORD), Some("1"));
        let mut page = launch_with(
            Session::anonymous().unwrap(),
            KameConfig {
                follow_omarchy: true,
                ..KameConfig::default()
            },
        );
        let delay = |page: &mut Headless| {
            let app = &mut page.app;
            let output = page
                .ctx
                .run_ui(egui::RawInput::default(), |ui| app.handle_input(ui.ctx()));
            let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            output.drop_without_applying_deltas();
            delay
        };
        let wake_in = std::time::Duration::from_secs_f32(FILE_CHECK_SECONDS);
        assert!(delay(&mut page) <= wake_in);

        page.app
            .run_menu_action(&page.ctx.clone(), MenuAction::ToggleOmarchy);
        // The frame that draws the interface in kame's own colours, and then the next.
        assert_eq!(delay(&mut page), std::time::Duration::ZERO);
        assert!(delay(&mut page) > wake_in, "nothing to look at");
    }

    #[test]
    fn with_the_setting_off_the_colours_are_kames_own_and_it_switches_at_once() {
        put_theme(Some(NORD), Some("1"));
        let mut page = launch(false);
        assert_eq!(page.app.palette, Palette::LIGHT);
        wake(&mut page);
        assert_eq!(page.app.palette, Palette::LIGHT, "whatever the files say");
        let before = drawn(&page);

        let ctx = page.ctx.clone();
        page.app.run_menu_action(&ctx, MenuAction::ToggleOmarchy);
        assert!(page.app.follow_omarchy);
        assert_eq!(page.app.palette, themed(NORD), "at once");
        assert!(ctx.global_style().visuals.dark_mode);
        page.frame(Vec::new());
        assert_ne!(drawn(&page), before);

        page.app.run_menu_action(&ctx, MenuAction::ToggleOmarchy);
        assert!(!page.app.follow_omarchy);
        assert_eq!(page.app.palette, Palette::LIGHT, "back at once");
        assert!(!ctx.global_style().visuals.dark_mode);
    }

    #[test]
    fn the_setting_is_kept_and_followed_again_at_the_next_launch() {
        put_theme(Some(NORD), Some("1"));
        let mut page = launch(false);
        let ctx = page.ctx.clone();
        page.app.run_menu_action(&ctx, MenuAction::ToggleOmarchy);

        let written = serde_json::to_string(&page.app.settings()).unwrap();
        let read: KameConfig = serde_json::from_str(&written).unwrap();
        assert!(read.follow_omarchy);
        let next = launch_with(Session::anonymous().unwrap(), read);
        assert!(next.app.follow_omarchy);
        assert_eq!(next.app.palette, themed(NORD));
    }

    #[test]
    fn a_settings_file_without_the_setting_reads_with_it_off() {
        let old: KameConfig = serde_json::from_str(r#"{"body_size":18.0,"dark":true}"#).unwrap();
        assert!(!old.follow_omarchy);
        assert!(old.dark);
    }
}
