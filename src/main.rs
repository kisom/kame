//! kame — a distraction-free editor for long-form writing.
//!
//! The binary opens (or creates) one `.kame` SQLite file, loads its document,
//! lays every block out with cosmic-text, rasterizes each block to an egui
//! texture, and paints the result. Keyboard input is translated into editor
//! commands; every command is persisted through the durable op log so history
//! survives a restart.

mod edit;
mod export;
mod import;
mod layout;
mod model;
mod render;
mod storage;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};

use eframe::egui;
use egui::{Align2, Color32, FontId, Id, Order, Pos2, Rect, Sense, TextureHandle, Vec2};

use crate::edit::{Cursor, EditError, Editor, Op, Selection};
use crate::import::markdown::{from_markdown, Imported};
use crate::layout::reading_order;
use crate::layout::{
    heading_size_for, link_ranges, place_footnotes, place_marginalia, BlockLayout,
    FootnotePlacement, LayoutConfig, MarginalPlacement, DEFAULT_BODY_SIZE, FOOTNOTE_GAP,
    FOOTNOTE_LINE_HEIGHT, FOOTNOTE_MARK_EM,
};
use crate::model::{
    is_script_url, Anchor, Annotation, AnnotationId, AnnotationKind, Block, BlockId, BlockKind,
    Document, Metadata, SpanStyle,
};
use crate::render::egui_render::{
    caret_line, caret_x, command_for_event, install_fonts, next_boundary, next_word, offset_at,
    prev_boundary, prev_word, range_rects, word_at, AnnotationTextureCache, Command, GlyphRenderer,
    LayoutCache, TextureCache,
};
use crate::storage::{Db, DbError, SnapshotMeta};

/// Theme colours. One palette is chosen at launch (or on the dark-mode
/// toggle), and every surface reads from it — the rasterized text inks and the
/// painted chrome must always agree.
#[derive(Clone, Copy)]
struct Palette {
    background: Color32,
    body_ink: [u8; 3],
    apparatus_ink: [u8; 3],
    caret_ink: Color32,
    selection_fill: Color32,
    note_highlight: Color32,
    footnote_highlight: Color32,
    footnote_mark: Color32,
    marginal_highlight: Color32,
    rule_ink: Color32,
    note_mark: Color32,
    list_marker_ink: Color32,
    dim_veil: Color32,
    error_ink: Color32,
    notice_ink: Color32,
}

impl Palette {
    /// Warm paper and near-black ink.
    const LIGHT: Palette = Palette {
        background: Color32::from_rgb(247, 243, 235),
        body_ink: [32, 29, 25],
        apparatus_ink: [86, 79, 69],
        caret_ink: Color32::from_rgb(176, 58, 46),
        selection_fill: Color32::from_rgba_premultiplied(58, 78, 112, 62),
        note_highlight: Color32::from_rgba_unmultiplied_const(255, 170, 60, 70),
        footnote_highlight: Color32::from_rgba_unmultiplied_const(54, 96, 132, 44),
        footnote_mark: Color32::from_rgb(54, 96, 132),
        marginal_highlight: Color32::from_rgba_unmultiplied_const(96, 96, 96, 34),
        rule_ink: Color32::from_rgb(150, 141, 126),
        note_mark: Color32::from_rgb(140, 104, 40),
        list_marker_ink: Color32::from_rgb(32, 29, 25),
        dim_veil: Color32::from_rgba_unmultiplied_const(247, 243, 235, 168),
        error_ink: Color32::from_rgb(168, 32, 32),
        notice_ink: Color32::from_rgb(46, 96, 60),
    };

    /// Dark paper with warm light ink.
    const DARK: Palette = Palette {
        background: Color32::from_rgb(30, 28, 25),
        body_ink: [222, 216, 205],
        apparatus_ink: [168, 160, 148],
        caret_ink: Color32::from_rgb(226, 92, 70),
        selection_fill: Color32::from_rgba_premultiplied(92, 112, 150, 66),
        note_highlight: Color32::from_rgba_unmultiplied_const(196, 128, 40, 80),
        footnote_highlight: Color32::from_rgba_unmultiplied_const(92, 132, 168, 60),
        footnote_mark: Color32::from_rgb(120, 160, 190),
        marginal_highlight: Color32::from_rgba_unmultiplied_const(120, 120, 120, 44),
        rule_ink: Color32::from_rgb(110, 104, 96),
        note_mark: Color32::from_rgb(188, 148, 68),
        list_marker_ink: Color32::from_rgb(222, 216, 205),
        dim_veil: Color32::from_rgba_unmultiplied_const(30, 28, 25, 168),
        error_ink: Color32::from_rgb(226, 92, 92),
        notice_ink: Color32::from_rgb(120, 180, 130),
    };

    /// The palette for a light or dark surface.
    fn for_mode(dark: bool) -> Palette {
        if dark {
            Palette::DARK
        } else {
            Palette::LIGHT
        }
    }
}

/// The opaque colour a translucent annotation wash becomes over the paper.
///
/// The glyph atlas bakes subpixel antialiasing against this colour so that
/// highlighted text stays crisp against the wash. egui stores colours
/// premultiplied, so this composites `overlay` over `base` in premultiplied
/// space; `base` is the opaque paper, so the result is already straight RGB.
fn overlay_on(base: Color32, overlay: Color32) -> [u8; 3] {
    let a = overlay.a() as f32 / 255.0;
    let over = |b: u8, o: u8| (o as f32 + b as f32 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
    [
        over(base.r(), overlay.r()),
        over(base.g(), overlay.g()),
        over(base.b(), overlay.b()),
    ]
}

/// Each block's footnote offsets, where its superscripts need room.
fn footnote_marks(document: &Document) -> HashMap<BlockId, Vec<usize>> {
    let mut marks: HashMap<BlockId, Vec<usize>> = HashMap::new();
    for annotation in &document.annotations {
        if annotation.kind != AnnotationKind::Footnote {
            continue;
        }
        if let Some((_, end)) = annotation.anchor.byte_range() {
            marks
                .entry(annotation.anchor.block_id())
                .or_default()
                .push(end);
        }
    }
    for offsets in marks.values_mut() {
        offsets.sort_unstable();
    }
    marks
}

/// Per-block byte ranges whose background is an annotation wash (already
/// composited to opaque), keyed by the block the anchor pins to. The glyph
/// atlas bakes these into the subpixel antialiasing of the marked prose.
fn highlight_map(
    document: &Document,
    palette: &Palette,
) -> HashMap<BlockId, Vec<(usize, usize, [u8; 3])>> {
    let mut map: HashMap<BlockId, Vec<(usize, usize, [u8; 3])>> = HashMap::new();
    for annotation in &document.annotations {
        // Block-scoped anchors (marginalia) have no body range to wash.
        let Some((start, end)) = annotation.anchor.byte_range() else {
            continue;
        };
        let wash = match annotation.kind {
            AnnotationKind::Marginal => palette.marginal_highlight,
            AnnotationKind::Note => palette.note_highlight,
            AnnotationKind::Footnote => palette.footnote_highlight,
        };
        map.entry(annotation.anchor.block_id()).or_default().push((
            start,
            end,
            overlay_on(palette.background, wash),
        ));
    }
    map
}
/// Gap between a list marker's right edge and its item's text.
const LIST_MARKER_GAP: f32 = 6.0;
/// Font-size step for the Ctrl + / Ctrl - chords, and the clamp bounds (body
/// size in px). Everything else scales off the body size.
const FONT_SIZE_STEP: f32 = 1.0;
const FONT_SIZE_MIN: f32 = 10.0;
const FONT_SIZE_MAX: f32 = 32.0;
/// Space above the first block and below the last.
const PAGE_PADDING: f32 = 36.0;
/// Vertical space between blocks.
const BLOCK_GAP: f32 = 14.0;
/// Width the vertical scroll bar takes out of the scroll area's inner rect.
///
/// The measure is computed from the panel width (so the scroll bar's presence
/// cannot feed back into the wrap), which means its width has to be reserved
/// here or the page's right edge is clipped by the bar at the threshold.
const SCROLLBAR_ALLOWANCE: f32 = 16.0;
/// Annotation text is set a little smaller than the prose.
const ANNOTATION_SCALE: f32 = 0.82;
/// Height of the failure strip drawn across the top of the central panel.
const ERROR_STRIP_HEIGHT: f32 = 3.0;

type AppResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

// --- distraction-free modes ------------------------------------------------

/// The distraction-free toggles. Pure view state: none of it is persisted and
/// none of it touches the document, so every mode can be switched at any time
/// without an undo step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Modes {
    fullscreen: bool,
    /// Hide the menu bar and the status row.
    hide_chrome: bool,
    /// Collapse the margin gutter so the measure re-centres on the window.
    hide_margin: bool,
    /// Dim every block but the one holding the caret.
    paragraph_focus: bool,
    /// Keep the caret's line at the vertical centre of the window.
    typewriter: bool,
}

/// One toggleable view mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModeCommand {
    Fullscreen,
    HideChrome,
    HideMargin,
    ParagraphFocus,
    Typewriter,
}

impl Modes {
    fn is_on(&self, mode: ModeCommand) -> bool {
        match mode {
            ModeCommand::Fullscreen => self.fullscreen,
            ModeCommand::HideChrome => self.hide_chrome,
            ModeCommand::HideMargin => self.hide_margin,
            ModeCommand::ParagraphFocus => self.paragraph_focus,
            ModeCommand::Typewriter => self.typewriter,
        }
    }

    /// Flips `mode` and returns its new state.
    fn toggle(&mut self, mode: ModeCommand) -> bool {
        let slot = match mode {
            ModeCommand::Fullscreen => &mut self.fullscreen,
            ModeCommand::HideChrome => &mut self.hide_chrome,
            ModeCommand::HideMargin => &mut self.hide_margin,
            ModeCommand::ParagraphFocus => &mut self.paragraph_focus,
            ModeCommand::Typewriter => &mut self.typewriter,
        };
        *slot = !*slot;
        *slot
    }
}

/// Keyboard map for the distraction-free modes. `command` is Ctrl on
/// Linux/Windows and Cmd on macOS — the same modifier the editor's own map
/// uses — so the chords travel between platforms. F11 is the platform
/// fullscreen key and needs no modifier. None of these collide with the
/// editor map (`command_for_key`), which claims plain keys, undo/redo, and
/// arrows.
fn mode_for_key(key: egui::Key, modifiers: egui::Modifiers) -> Option<ModeCommand> {
    if key == egui::Key::F11 {
        return Some(ModeCommand::Fullscreen);
    }
    if !(modifiers.command && modifiers.shift) {
        return None;
    }
    match key {
        egui::Key::H => Some(ModeCommand::HideChrome),
        egui::Key::M => Some(ModeCommand::HideMargin),
        egui::Key::P => Some(ModeCommand::ParagraphFocus),
        egui::Key::T => Some(ModeCommand::Typewriter),
        _ => None,
    }
}

/// The mode chords in a frame's event stream. Reading the events (rather than
/// `key_pressed` plus the frame's live modifiers) keeps each chord matched with
/// the modifiers that were down when its key went down, and ignoring repeats
/// stops a held toggle key from flapping the mode.
fn mode_commands_for_events(events: &[egui::Event]) -> Vec<ModeCommand> {
    events
        .iter()
        .filter_map(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => mode_for_key(*key, *modifiers),
            _ => None,
        })
        .collect()
}

/// True when the dark-mode chord (Ctrl/Cmd+Shift+D) went down this frame. Dark
/// mode is a persisted preference, not a transient view mode, so it has its own
/// chord rather than living in the [`Modes`] set.
fn dark_toggle_for_events(events: &[egui::Event]) -> bool {
    events.iter().any(|event| match event {
        egui::Event::Key {
            key: egui::Key::D,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } => modifiers.command && modifiers.shift,
        _ => false,
    })
}

// --- file, export and snapshot commands ------------------------------------

/// The document-level commands that sit outside the editing history: two
/// exports, taking a snapshot, and going back to one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileCommand {
    ExportMarkdown,
    ExportHtml,
    SnapshotNow,
    RestoreSnapshot,
}

/// Keyboard map for the file commands. Every chord is Ctrl (Cmd on macOS) +
/// Shift + a letter, so none of them can be mistaken for prose, and none
/// collides with the editor map (`command_for_key`) or the mode map
/// (`mode_for_key`) — both of which are asserted against this map in the
/// tests. `E` exports the Markdown, `B` the page you would open in a browser,
/// `S` snapshots and `R` restores.
fn file_command_for_key(key: egui::Key, modifiers: egui::Modifiers) -> Option<FileCommand> {
    if !(modifiers.command && modifiers.shift) {
        return None;
    }
    match key {
        egui::Key::E => Some(FileCommand::ExportMarkdown),
        egui::Key::B => Some(FileCommand::ExportHtml),
        egui::Key::S => Some(FileCommand::SnapshotNow),
        egui::Key::R => Some(FileCommand::RestoreSnapshot),
        _ => None,
    }
}

/// The file-command chords in a frame's event stream. Reading the events keeps
/// each chord matched with the modifiers that were down when its key went
/// down, and ignoring repeats stops a held key from exporting twice.
fn file_commands_for_events(events: &[egui::Event]) -> Vec<FileCommand> {
    events
        .iter()
        .filter_map(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => file_command_for_key(*key, *modifiers),
            _ => None,
        })
        .collect()
}

/// Where an export of `source` lands: `<stem>.<extension>` in the same
/// directory as the `.kame` file it was opened from.
///
/// A bare file name (no directory part) resolves in the current working
/// directory, which is also where a source with no usable stem falls back to —
/// an unnamed document exports as `document.md` rather than `..md`.
fn export_path(source: &Path, extension: &str) -> PathBuf {
    let name = match source.file_stem().filter(|stem| !stem.is_empty()) {
        Some(stem) => format!("{}.{extension}", stem.to_string_lossy()),
        None => format!("document.{extension}"),
    };
    match source.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

/// Current Unix time, used to describe how old a snapshot is.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// A coarse relative age for the snapshot picker: snapshots are chosen as "the
/// one from before lunch", not to the second.
fn format_age(timestamp: i64, now: i64) -> String {
    let age = now.saturating_sub(timestamp).max(0);
    if age < 60 {
        "just now".to_string()
    } else if age < 60 * 60 {
        format!("{}m ago", age / 60)
    } else if age < 24 * 60 * 60 {
        format!("{}h ago", age / 3600)
    } else {
        format!("{}d ago", age / 86_400)
    }
}

/// One line for a snapshot in the picker: id, age, and the label it was taken
/// with (or a word saying it has none).
fn describe_snapshot(meta: &SnapshotMeta, now: i64) -> String {
    let label = meta
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .unwrap_or("unlabelled");
    format!(
        "#{} · {} · {label}",
        meta.id,
        format_age(meta.timestamp, now)
    )
}

/// The "Snapshot Now" label prompt.
struct SnapshotPrompt {
    /// The label being typed. Empty means an unlabelled snapshot.
    label: String,
    /// Set for the frame the prompt opens on, so the field takes the keyboard
    /// without a click.
    focus: bool,
}

/// The "Open" path prompt (Ctrl-O): a text field for the `.kame` path to open.
struct OpenPrompt {
    /// The path being typed.
    path: String,
    /// Set for the frame the prompt opens on, so the field takes the keyboard.
    focus: bool,
    /// True once the path is entered but the author must still confirm
    /// discarding unsaved changes (a failed save), so the prompt offers
    /// "open anyway" instead of opening straight away.
    confirming: bool,
}

/// Resolves a path typed into the Open prompt. A leading `~` expands to the
/// home directory, and a relative path resolves against the current document's
/// directory, so `foo.kame` opens beside the document already on screen.
fn resolve_open_path(typed: &str, base: &Path) -> PathBuf {
    let trimmed = typed.trim();
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let path = if let Some(rest) = trimmed.strip_prefix("~/") {
        home.map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(trimmed))
    } else if trimmed == "~" {
        home.unwrap_or_else(|| PathBuf::from(trimmed))
    } else {
        PathBuf::from(trimmed)
    };
    if path.is_absolute() {
        path
    } else if let Some(dir) = base.parent() {
        dir.join(path)
    } else {
        path
    }
}

/// Whether `path` names a Markdown file to import rather than a `.kame` document.
fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

/// How many of `document`'s images point at a local file that is not there. A
/// remote image is not counted, since whether it exists is unknowable here.
fn missing_images(document: &Document) -> usize {
    document
        .blocks
        .iter()
        .filter(|block| match &block.kind {
            BlockKind::Image { path } => !path.contains("://") && !Path::new(path).exists(),
            _ => false,
        })
        .count()
}

/// The native file browser for a Markdown file to import, or `None` when the
/// author cancels it. Blocks the frame like [`pick_save_path`], which is correct
/// for a modal dialog.
fn pick_markdown_path() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Import Markdown")
        .add_filter("Markdown", &["md", "markdown"])
        .pick_file()
}

/// The native "save as" file browser, or `None` when the author cancels it.
///
/// `rfd` is built with the XDG desktop portal backend (not GTK), so it asks the
/// compositor's file chooser — the GTK one on this desktop — over D-Bus. That
/// is a full file browser: directories, navigation and a file-name field, the
/// same dialog a GTK application shows. The synchronous API blocks the frame
/// thread while the (modal) dialog is up, which is correct for a save prompt.
fn pick_save_path(suggested_name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Save kame document")
        .add_filter("kame document", &["kame"])
        .set_file_name(suggested_name)
        .save_file()
}

/// The "Insert image" path prompt: a text field for the image file to embed.
struct ImagePrompt {
    /// The path being typed.
    path: String,
    /// Set for the frame the prompt opens on, so the field takes the keyboard.
    focus: bool,
}

/// The "Add link" URL prompt (C-k): the range being linked and the URL being
/// typed.
struct LinkPrompt {
    block_id: BlockId,
    start: usize,
    end: usize,
    /// The URL being typed, pre-filled with the existing link's URL, if any.
    url: String,
    /// Set for the frame the prompt opens on, so the field takes the keyboard.
    focus: bool,
}

/// The "Restore Snapshot" picker: the listed snapshots (newest first), which
/// one is chosen, and which of its two steps is showing. The second step is the
/// confirmation, because restoring discards whatever is in the document now.
struct RestorePicker {
    snapshots: Vec<SnapshotMeta>,
    selected: Option<i64>,
    confirming: bool,
}

impl RestorePicker {
    fn new(snapshots: Vec<SnapshotMeta>) -> RestorePicker {
        // The newest snapshot is preselected: a picker that opens with the
        // likely choice already made is one Enter away from a restore.
        let selected = snapshots.first().map(|meta| meta.id);
        RestorePicker {
            snapshots,
            selected,
            confirming: false,
        }
    }

    /// Moves the choice one row, clamped to the ends.
    fn move_selection(&mut self, down: bool) {
        if self.snapshots.is_empty() {
            return;
        }
        let Some(index) = self
            .selected
            .and_then(|id| self.snapshots.iter().position(|meta| meta.id == id))
        else {
            self.selected = self.snapshots.first().map(|meta| meta.id);
            return;
        };
        let target = if down {
            (index + 1).min(self.snapshots.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.selected = self.snapshots.get(target).map(|meta| meta.id);
    }
}

/// The narrowest measure the page shrinks to. Below this the window is simply
/// too small for legible prose, and the page is allowed to clip rather than
/// wrap every line to three words.
const MIN_MEASURE: f32 = 320.0;

/// Fractions of the window the page takes. The body holds the middle half, an
/// unused quarter gutter sits to its left, and the right quarter is the margin
/// — inset by [`MARGIN_EDGE`] so marginalia never crowd the window's edge.
const BODY_FRACTION: f32 = 0.5;
const GUTTER_FRACTION: f32 = 0.25;
const MARGIN_FRACTION: f32 = 0.25;
const MARGIN_EDGE: f32 = 0.05;

/// True when the window is wide enough to hold the designed page — a half-width
/// body beside its gutter and margin — without the measure falling below
/// [`MIN_MEASURE`].
fn page_fits(available_width: f32) -> bool {
    available_width * BODY_FRACTION >= MIN_MEASURE
}

/// The measure and margin column a window of `available_width` shows.
///
/// The page is `gutter + body + margin`, nothing scrolls horizontally, and a
/// window narrower than twice [`MIN_MEASURE`] cannot fit the design, so there
/// the margin collapses and the prose fills the window.
fn page_measure(hide_margin: bool, available_width: f32) -> (f32, f32) {
    let width = available_width.max(1.0);
    if page_fits(width) {
        let body = width * BODY_FRACTION;
        let margin = if hide_margin {
            0.0
        } else {
            width * (MARGIN_FRACTION - MARGIN_EDGE)
        };
        (body, margin)
    } else {
        (width.max(MIN_MEASURE), 0.0)
    }
}

/// Page geometry for one frame, all as offsets from the painter's top-left.
///
/// `config` is the measured config for this frame: `page_measure` has already
/// chosen the measure and margin column, so this only positions the body — a
/// fixed quarter-gutter from the left when the page fits, flush against the
/// edge when it has collapsed to fill a narrow window.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PageGeometry {
    /// Distance from the painter's left edge to the body text.
    body_left: f32,
    /// Width of the margin column right of the measure.
    margin_width: f32,
}

/// Positions the body in `available_width`: [`GUTTER_FRACTION`] of the way in
/// from the left. The margin column (`config.margin_width` wide) runs from the
/// body's right edge toward the window's, leaving [`MARGIN_EDGE`] clear at the
/// far edge for the marginals to breathe.
fn page_geometry(config: LayoutConfig, available_width: f32) -> PageGeometry {
    let width = available_width.max(1.0);
    let body_left = if page_fits(width) {
        width * GUTTER_FRACTION
    } else {
        0.0
    };
    PageGeometry {
        body_left,
        margin_width: config.margin_width,
    }
}

/// The veil for block `index` in paragraph-focus mode, or `None` when the block
/// is the one being written in (or the mode is off, or no caret block resolved).
fn dim_veil(
    veil: Color32,
    enabled: bool,
    index: usize,
    caret_index: Option<usize>,
) -> Option<Color32> {
    let caret_index = caret_index?;
    (enabled && index != caret_index).then_some(veil)
}

/// Scroll offset that puts `caret_top` (page-local) at the vertical centre of a
/// viewport, clamped so the page never scrolls past its own edges.
fn typewriter_scroll_offset(caret_top: f32, viewport_height: f32, content_height: f32) -> f32 {
    let furthest = (content_height - viewport_height).max(0.0);
    (caret_top - viewport_height * 0.5).clamp(0.0, furthest)
}

/// ASCII-fast, Unicode-correct case-insensitive character equality.
fn char_eq_ignore_case(a: char, b: char) -> bool {
    if a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(&b)
    } else {
        a.to_lowercase().collect::<String>() == b.to_lowercase().collect::<String>()
    }
}

/// Case-insensitive matches of `needle` across every block, in document order,
/// as `(block_id, byte_start, byte_end)` in the original text's byte offsets.
/// Overlapping matches are reported; an empty needle matches nothing.
fn find_matches(document: &Document, needle: &str) -> Vec<(BlockId, usize, usize)> {
    let mut matches = Vec::new();
    if needle.is_empty() {
        return matches;
    }
    let needle: Vec<char> = needle.chars().collect();
    for block in &document.blocks {
        // Match over the text's own char boundaries so byte offsets stay exact
        // even when a query or text char lowercases to a different length.
        let chars: Vec<(usize, char)> = block.text.char_indices().collect();
        // `windows` yields nothing when the block is shorter than the needle,
        // so a block with fewer chars (an empty block) never panics the way an
        // open `0..=saturating_sub` range would.
        for window in chars.windows(needle.len()) {
            if window
                .iter()
                .zip(&needle)
                .all(|(&(_, c), &n)| char_eq_ignore_case(c, n))
            {
                let start = window[0].0;
                let end = window
                    .last()
                    .map(|&(offset, c)| offset + c.len_utf8())
                    .unwrap_or(start);
                matches.push((block.id, start, end));
            }
        }
    }
    matches
}

/// The window command a mode toggle sends, or `None` when the mode lives
/// entirely inside the frame. Fullscreen is the only mode that leaves the
/// drawing surface, so it is the only one the window has to act on — and it
/// must be sent in both directions: sending only the "on" edge is how a
/// fullscreen mode enters and then cannot leave.
fn viewport_effect(mode: ModeCommand, on: bool) -> Option<egui::ViewportCommand> {
    match mode {
        ModeCommand::Fullscreen => Some(egui::ViewportCommand::Fullscreen(on)),
        ModeCommand::HideChrome
        | ModeCommand::HideMargin
        | ModeCommand::ParagraphFocus
        | ModeCommand::Typewriter => None,
    }
}

/// The failure message the always-on indicator must show, if any. Hide-chrome
/// removes the status panel, so a failed write needs a signal that does not
/// live in that panel; a blank message is not a failure.
fn error_indicator(last_error: Option<&str>) -> Option<&str> {
    last_error.filter(|error| !error.trim().is_empty())
}

/// Paints the failure signal into the page: a thin strip along the top of the
/// central panel and a corner dot whose tooltip carries the message. It is
/// drawn from the central panel, never the status panel, so hide-chrome cannot
/// leave the author typing into a document that failed to save with no signal.
///
/// Returns whether the author clicked it. For a failed write the strip is the
/// retry affordance: a transient write failure (a full disk that was emptied)
/// must be recoverable in place, not by quitting and losing the session.
///
/// `retry` marks that failed-write class. Every other message (a degraded open,
/// a failed export) has no pending write to retry, so its strip dismisses the
/// message instead of offering a write that would do nothing.
fn draw_error_indicator(ui: &mut egui::Ui, error: &str, retry: bool, error_ink: Color32) -> bool {
    let rect = ui.max_rect();
    {
        let painter = ui.painter();
        let strip = Rect::from_min_max(
            Pos2::new(rect.left(), rect.top()),
            Pos2::new(rect.right(), rect.top() + ERROR_STRIP_HEIGHT),
        );
        painter.rect_filled(strip, 0.0, error_ink);
        painter.circle_filled(
            Pos2::new(rect.right() - 12.0, rect.top() + 12.0),
            4.0,
            error_ink,
        );
    }
    // The painted strip is a hairline; the click target reaches down into the
    // page's top padding, above the first line of prose, so it can be hit.
    let band = Rect::from_min_max(
        Pos2::new(rect.left(), rect.top()),
        Pos2::new(rect.right(), rect.top() + 12.0),
    );
    let hint = if retry {
        "click to retry the save · close again to quit without saving"
    } else {
        "click to dismiss"
    };
    let strip = ui
        .interact(band, Id::unique("kame-error-strip"), Sense::click())
        .on_hover_text(format!("{error}\n\n{hint}"));
    let dot = ui
        .interact(
            Rect::from_center_size(
                Pos2::new(rect.right() - 12.0, rect.top() + 12.0),
                Vec2::splat(24.0),
            ),
            Id::unique("kame-error-indicator"),
            Sense::click(),
        )
        .on_hover_text(format!("{error}\n\n{hint}"));
    strip.clicked() || dot.clicked()
}

/// The window's close guard against a failed write, without ever trapping the
/// author.
///
/// The first close request is cancelled: quitting there would silently discard
/// everything typed since the failed write, and the error strip is the retry
/// affordance. A second request is allowed through — if the write cannot land
/// at all (the file was yanked, the volume went read-only), the session must
/// still be closable, and the edits lost were unsaveable anyway.
#[derive(Default)]
struct CloseGuard {
    /// Close requests already cancelled while the last write was failing.
    refused: u32,
}

impl CloseGuard {
    /// Records one frame's close state and says whether to cancel the close.
    fn cancel_close(&mut self, save_failed: bool, close_requested: bool) -> bool {
        if !save_failed {
            // A healthy session closes at once; a later failure starts the
            // guard's count over.
            self.refused = 0;
            return false;
        }
        if !close_requested {
            return false;
        }
        self.refused += 1;
        self.refused < 2
    }
}

/// The status row's badge for each active mode, as `(abbreviation, full name)`.
/// Hide-chrome has none: while it is on, the row itself is gone.
fn mode_badges(modes: &Modes) -> Vec<(&'static str, &'static str)> {
    [
        (ModeCommand::Fullscreen, "FS", "fullscreen"),
        (ModeCommand::HideMargin, "M", "margin hidden"),
        (ModeCommand::ParagraphFocus, "F", "paragraph focus"),
        (ModeCommand::Typewriter, "T", "typewriter"),
    ]
    .into_iter()
    .filter(|(mode, _, _)| modes.is_on(*mode))
    .map(|(_, short, name)| (short, name))
    .collect()
}

/// The status row's word count, with thousands separated.
fn word_count_label(words: usize) -> String {
    let digits = words.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    let noun = if words == 1 { "word" } else { "words" };
    format!("{grouped} {noun}")
}

/// A shortcut as the menu shows it. Chords are written with `Ctrl`, which is Cmd
/// on macOS (see `Modifiers::command`).
fn shortcut_label(chord: &str, mac: bool) -> String {
    if mac {
        chord.replace("Ctrl", "Cmd")
    } else {
        chord.to_string()
    }
}

/// Clears a notice on the next edit, so it does not sit under the status row.
///
/// A notice set in the same frame as a history change (restoring a snapshot) is
/// not cleared by it: the change is what the notice is reporting.
#[derive(Default)]
struct NoticeTracker {
    /// The notice last seen, and the history it was seen with.
    seen: Option<String>,
    history: (usize, usize),
}

impl NoticeTracker {
    fn update(&mut self, notice: &mut Option<String>, history: (usize, usize)) {
        if *notice != self.seen {
            self.seen = notice.clone();
        } else if history != self.history {
            *notice = None;
            self.seen = None;
        }
        self.history = history;
    }
}

/// One menu on the menu bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarMenu {
    File,
    Annotate,
    View,
    Help,
}

/// Takes an Alt+letter menu chord out of `events`, with the text egui-winit sends
/// for the same keypress, which would otherwise land in the prose.
///
/// There are none on macOS, where Option plus a letter types a character.
fn take_menu_chord(events: &mut Vec<egui::Event>, mac: bool) -> Option<BarMenu> {
    if mac {
        return None;
    }
    let (index, menu, letter) = events.iter().enumerate().find_map(|(index, event)| {
        let egui::Event::Key {
            key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } = event
        else {
            return None;
        };
        if !modifiers.alt || modifiers.ctrl || modifiers.shift || modifiers.command {
            return None;
        }
        let (menu, letter) = match key {
            egui::Key::F => (BarMenu::File, "f"),
            egui::Key::A => (BarMenu::Annotate, "a"),
            egui::Key::V => (BarMenu::View, "v"),
            egui::Key::H => (BarMenu::Help, "h"),
            _ => return None,
        };
        Some((index, menu, letter))
    })?;
    events.remove(index);
    let text = events[index..].iter().position(
        |event| matches!(event, egui::Event::Text(text) if text.eq_ignore_ascii_case(letter)),
    );
    if let Some(offset) = text {
        events.remove(index + offset);
    }
    Some(menu)
}

/// Something chosen from the menu bar, applied once the panel closes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum MenuAction {
    Open,
    Save,
    ImportMarkdown,
    Properties,
    File(FileCommand),
    Annotate(AnnotationKind),
    TypeSize(f32),
    ToggleDark,
    Mode(ModeCommand),
    Help,
}

/// A menu entry showing its shortcut, if it has one. Clicking closes the menu.
fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: Option<&str>, selected: bool) -> bool {
    menu_entry(ui, label, shortcut, selected).clicked()
}

/// [`menu_item`], returning the response so a caller can add a hover.
fn menu_entry(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: Option<&str>,
    selected: bool,
) -> egui::Response {
    let mut button = egui::Button::new(label).selected(selected);
    if let Some(shortcut) = shortcut {
        button = button.shortcut_text(shortcut_label(shortcut, cfg!(target_os = "macos")));
    }
    let response = ui.add(button);
    if response.clicked() {
        ui.close();
    }
    response
}

/// `when` as a `YYYY-MM-DD` day, in UTC.
fn iso_date(when: std::time::SystemTime) -> String {
    let secs = when
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    // Howard Hinnant's civil-from-days, on years that start in March so the leap
    // day falls at the end of one.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The tags typed into the properties dialog.
fn parse_tags(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

/// Today, as a metadata date.
fn today() -> String {
    iso_date(std::time::SystemTime::now())
}

/// Metadata for a file that has none stored: its title if it was ever given one,
/// and its own date as the created date.
fn default_metadata(title: &str, path: &Path) -> Metadata {
    let created = std::fs::metadata(path)
        .ok()
        .and_then(|file| file.created().or_else(|_| file.modified()).ok())
        .map_or_else(today, iso_date);
    Metadata {
        title: if title == "Untitled" {
            String::new()
        } else {
            title.to_string()
        },
        created,
        ..Metadata::default()
    }
}

/// The Document Properties dialog: its fields as text until they are saved.
struct MetadataPrompt {
    title: String,
    author: String,
    created: String,
    published: String,
    tags: String,
    /// Set for the frame the dialog opens on, so the first field takes the
    /// keyboard.
    focus: bool,
}

/// Whether `event` is the macOS quit shortcut. `mac_cmd` is never set elsewhere,
/// so this binds no quit key on Linux.
fn is_quit_chord(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key: egui::Key::Q,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } if modifiers.mac_cmd
    )
}

/// Whether closing the window must stop and ask about an unsaved document.
///
/// Only the prompt's own answer may let unsaved work go. A close request arriving
/// while it is open is refused too, and Don't Save has to mark the close it sends
/// as answered, or that close would simply reopen the prompt.
#[derive(Default)]
struct CloseFlow {
    /// The "save?" prompt is on screen.
    prompting: bool,
    /// The author answered the prompt by closing.
    confirmed: bool,
}

impl CloseFlow {
    /// Records a close request and says whether to cancel it. `unsaved` is
    /// whether the document has work with nowhere to go.
    fn on_close_request(&mut self, unsaved: bool) -> bool {
        if self.confirmed || !unsaved {
            return false;
        }
        self.prompting = true;
        true
    }

    /// The author chose to keep editing.
    fn cancel(&mut self) {
        self.prompting = false;
    }

    /// The author chose to close: the close request this sends must go through.
    fn confirm(&mut self) {
        self.prompting = false;
        self.confirmed = true;
    }
}

// --- kame config ------------------------------------------------------------

/// App-level preferences, stored beside the documents rather than inside one.
///
/// The type size and dark mode are kame settings: they belong to the editor,
/// not to any single document, so they are read once at launch and written
/// back whenever they change. Documents never carry them.
#[derive(serde::Serialize, serde::Deserialize)]
struct KameConfig {
    #[serde(default = "default_body_size")]
    body_size: f32,
    #[serde(default)]
    dark: bool,
}

fn default_body_size() -> f32 {
    DEFAULT_BODY_SIZE
}

impl Default for KameConfig {
    fn default() -> Self {
        KameConfig {
            body_size: DEFAULT_BODY_SIZE,
            dark: false,
        }
    }
}

/// The directory kame keeps its config in: `$XDG_CONFIG_HOME/kame` (or
/// `~/.config/kame`) on Unix, `~/Library/Application Support/kame` on macOS.
fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let base = if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty()) {
        PathBuf::from(xdg)
    } else {
        home.join(".config")
    };
    Some(base.join("kame"))
}

fn config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("config.json"))
}

/// Reads the app config, falling back to defaults when the file is absent or
/// unreadable. A stored value is clamped to the legal range, never trusted.
fn load_config() -> KameConfig {
    let mut config = config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|json| serde_json::from_str::<KameConfig>(&json).ok())
        .unwrap_or_default();
    config.body_size = config.body_size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
    config
}

/// Writes the app config, best-effort: a preference that fails to persist is
/// not worth blocking the editor over.
fn save_config(config: &KameConfig) {
    let Some(path) = config_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(&path, json);
    }
}

fn main() -> eframe::Result {
    // No argument means an anonymous document: the author starts typing at once
    // and the first save prompts for a file. With an argument, open that file.
    let (session, notice) = match std::env::args_os().nth(1).map(PathBuf::from) {
        None => (
            Session::anonymous().unwrap_or_else(|err| {
                eprintln!("kame: {err}");
                std::process::exit(1);
            }),
            None,
        ),
        Some(path) => {
            // Open the document before the window exists, so a failure is
            // reported as "your file is the problem" rather than an opaque
            // app-creation error that reads as "kame is broken". A failed open
            // still shows the author an editable (in-memory) document and the
            // reason in the status panel, since a desktop launch has no
            // terminal to print to.
            match Session::open_any(&path) {
                Ok(opened) => opened,
                Err(err) => {
                    let message = format!("could not open {} — {err}", path.display());
                    let session = Session::blank(&path, message).unwrap_or_else(|err| {
                        eprintln!("kame: {err}");
                        std::process::exit(1);
                    });
                    (session, None)
                }
            }
        }
    };

    // The type size and theme are kame preferences, read once here and applied
    // to every document the app opens.
    let config = load_config();
    let body_size = config.body_size;
    let dark = config.dark;

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 860.0])
            .with_min_inner_size([560.0, 360.0])
            .with_title("kame"),
        // Cmd-Q in winit's default menu calls AppKit's `terminate:`, which exits
        // with no window close request for the unsaved-document prompt to refuse.
        // With no menu it arrives as a key press instead (see `is_quit_chord`).
        // This also gives up the menu's Hide items, and Quit from the Dock still
        // goes straight to `terminate:`.
        #[cfg(target_os = "macos")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            builder.with_default_menu(false);
        })),
        ..Default::default()
    };

    eframe::run_native(
        "kame",
        native_options,
        Box::new(move |cc| Ok(Box::new(KameApp::new(cc, session, notice, body_size, dark)))),
    )
}

/// Maps a failure to open a document into an error a user can act on: it
/// blames the file (corrupt, foreign, or from a newer build) instead of
/// leaking a serde/sqlite detail that reads as "kame is broken".
fn open_error(path: &Path, err: DbError) -> std::io::Error {
    let reason = match err {
        DbError::Json(_) => {
            "the document is corrupt or was written by a different version of kame".to_string()
        }
        DbError::SchemaTooNew { found, supported } => format!(
            "the document was written by a newer version of kame \
             (schema {found}; this build supports {supported})"
        ),
        DbError::Sqlite(err) => {
            let detail = err.to_string();
            let lower = detail.to_ascii_lowercase();
            if lower.contains("locked") || lower.contains("busy") {
                format!("the document is already open in another kame instance ({detail})")
            } else if lower.contains("unable to open") {
                // Usually a wrong path or a missing directory, not corruption.
                format!("the file could not be opened — is the path correct? ({detail})")
            } else {
                format!("the file is not a valid kame document ({detail})")
            }
        }
        other => other.to_string(),
    };
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("cannot open {}: {reason}", path.display()),
    )
}

// --- session ---------------------------------------------------------------

/// The document, its durable history, and the caret — everything that exists
/// without a window.
///
/// Every mutation goes through one of the command methods, which apply the
/// edit, persist it, and move the caret. That keeps the GUI layer free of
/// persistence rules and lets the whole editing loop be tested headlessly.
struct Session {
    editor: Editor,
    db: Db,
    caret: Cursor,
    /// A dragged/shift-clicked range, oriented from where the drag began.
    /// `caret` is always the moving end; `selection == None` means a bare
    /// caret.
    selection: Option<Selection>,
    last_error: Option<String>,
    /// Set when a write failed. Further mutations are refused until a write
    /// succeeds, so the in-memory undo cursor cannot drift from the DB cursor.
    save_failed: bool,
    /// The name of the file this document was imported from.
    source_stem: Option<String>,
    /// What the document says about itself. Saved directly, not an undo step.
    metadata: Metadata,
    /// The file this session is persisted to, or `None` for an anonymous
    /// (in-memory) document that has not been saved yet. Exports land beside
    /// it, and the snapshots it prompts for live in the database it names.
    path: Option<PathBuf>,
}

impl Session {
    /// Opens the database at `path`, creating a fresh document when it holds
    /// none yet, and resumes the editor at the persisted undo cursor.
    ///
    /// An unreadable op log is a degraded open, not a failure: the head
    /// document is the user's text, and history is only how it got there. One
    /// row written by a future build (or a partially written one) must not make
    /// the document unopenable, so the session starts with no history and a
    /// non-fatal warning, and the durable state is reset to match that empty
    /// history.
    fn open(path: &Path) -> AppResult<Session> {
        let mut db = Db::open(path).map_err(|err| open_error(path, err))?;
        let mut warning = None;
        // Set when the degrade path could not reset the database, which leaves
        // it unable to write rather than merely historyless.
        let mut reset_error: Option<DbError> = None;
        let editor = match db.load_document() {
            Ok(document) => match db.load_ops() {
                Ok(ops) => {
                    let ops: Vec<Op> = ops.into_iter().map(|(_, op)| op).collect();
                    let cursor = db.cursor()?;
                    // Resume with the persisted cursor, never with_history: the
                    // saved head may already have ops undone, and treating the
                    // whole log as applied would offer those ops for undo a
                    // second time.
                    Editor::with_cursor(document, ops, cursor)
                }
                Err(err) => {
                    warning = Some(format!(
                        "history unavailable — {err}; opening the document with no undo history"
                    ));
                    let editor = Editor::new(document);
                    // An in-memory editor with no history beside a DB cursor
                    // still pointing into the unreadable log would desync on the
                    // first edit: the edit truncates the log as a redo tail
                    // (shifting the sequence numbers the cursor counts), and the
                    // next undo writes the in-memory count as the DB cursor, so
                    // a reopen puts a surviving op on the redo stack and redo
                    // re-applies it to text it never touched. The unreadable ops
                    // are unrecoverable anyway — the first edit would drop them —
                    // so reset the DB to the degraded editor in one transaction:
                    // head, cursor 0, empty log.
                    if let Err(err) = db.restore_state(editor.document()) {
                        reset_error = Some(err);
                    }
                    editor
                }
            },
            Err(DbError::NoDocument) => {
                let title = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("Untitled");
                let document = Document::new(title);
                db.save_document(&document)?;
                Editor::new(document)
            }
            Err(err) => return Err(Box::new(open_error(path, err))),
        };

        let caret = editor
            .document()
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .ok_or("document has no blocks")?;
        // A reset that failed leaves the unreadable log on disk beside an
        // editor with no history. Do not open a session whose history could
        // diverge from the DB: open it read-only, so the first edit cannot
        // shift the log under the cursor.
        let (last_error, save_failed) = match reset_error {
            Some(err) => (Some(format!("save failed — editing disabled: {err}")), true),
            None => (warning, false),
        };
        let metadata = match db.load_metadata() {
            Ok(Some(metadata)) => metadata,
            // Missing or unreadable: the text is what matters, so fall back, and
            // keep the defaults for next time if the file will take them.
            _ => {
                let metadata = default_metadata(&editor.document().title, path);
                let _ = db.save_metadata(&metadata);
                metadata
            }
        };
        Ok(Session {
            editor,
            db,
            caret,
            selection: None,
            last_error,
            save_failed,
            source_stem: None,
            metadata,
            path: Some(path.to_path_buf()),
        })
    }

    /// A session for a document that could not be opened: a fresh, in-memory
    /// document so the author still has somewhere to type, with `error` shown
    /// in the status panel. The failed file is never touched, so a locked or
    /// corrupt document cannot be overwritten by a stray save.
    fn blank(path: &Path, error: String) -> AppResult<Session> {
        let db = Db::open_in_memory()?;
        let title = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled");
        let editor = Editor::new(Document::new(title));
        let caret = editor
            .document()
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .ok_or("document has no blocks")?;
        Ok(Session {
            editor,
            db,
            caret,
            selection: None,
            last_error: Some(error),
            save_failed: false,
            source_stem: None,
            metadata: Metadata {
                created: today(),
                ..Metadata::default()
            },
            path: None,
        })
    }

    /// A fresh anonymous document for a launch with no file argument: an
    /// in-memory database with no path, so the author starts typing at once and
    /// the first save prompts for a file (see [`Session::save_as`]).
    fn anonymous() -> AppResult<Session> {
        let db = Db::open_in_memory()?;
        let editor = Editor::new(Document::new("Untitled"));
        let caret = editor
            .document()
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .ok_or("document has no blocks")?;
        Ok(Session {
            editor,
            db,
            caret,
            selection: None,
            last_error: None,
            save_failed: false,
            source_stem: None,
            metadata: Metadata {
                created: today(),
                ..Metadata::default()
            },
            path: None,
        })
    }

    /// Opens whatever `path` names: Markdown is imported, anything else is a
    /// `.kame` document. An import also returns a line for the author saying
    /// what happened to it.
    fn open_any(path: &Path) -> AppResult<(Session, Option<String>)> {
        if is_markdown_path(path) {
            let (session, note) = Session::import(path)?;
            Ok((session, Some(note)))
        } else {
            Ok((Session::open(path)?, None))
        }
    }

    /// A new anonymous document holding the Markdown file at `path`, which is
    /// only read. The first save asks where the `.kame` file goes.
    fn import(path: &Path) -> AppResult<(Session, String)> {
        let source = std::fs::read_to_string(path)?;
        // Made absolute because an anonymous document has no directory of its
        // own for a relative image path to resolve against.
        let path = std::path::absolute(path)?;
        let base_dir = path.parent().unwrap_or(Path::new(""));
        let title = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled");
        let Imported {
            document,
            mut metadata,
            report,
        } = from_markdown(&source, base_dir, title);
        if metadata.title.is_empty() {
            metadata.title = document.title.clone();
        }
        metadata.created = today();

        let mut note = format!(
            "imported {} — not saved yet",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        if let Some(summary) = report.summary() {
            note.push_str(&format!("; {summary}"));
        }
        let missing = missing_images(&document);
        if missing > 0 {
            let noun = if missing == 1 { "image" } else { "images" };
            note.push_str(&format!("; {missing} {noun} not found"));
        }

        let mut db = Db::open_in_memory()?;
        db.save_metadata(&metadata)?;
        let editor = Editor::new(document);
        let caret = editor
            .document()
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .ok_or("document has no blocks")?;
        let session = Session {
            editor,
            db,
            caret,
            selection: None,
            last_error: None,
            save_failed: false,
            source_stem: Some(title.to_string()),
            metadata,
            path: None,
        };
        Ok((session, note))
    }

    /// Whether replacing this document would throw away work: edits a failed
    /// save left only in memory, or an anonymous document with no file at all.
    fn would_lose_work(&self) -> bool {
        self.save_failed || (self.is_anonymous() && self.has_content())
    }

    /// The name to show for this document: its metadata's title, or its file's
    /// name if it has none.
    fn display_title(&self) -> String {
        if !self.metadata.title.is_empty() {
            return self.metadata.title.clone();
        }
        self.path()
            .and_then(Path::file_stem)
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled")
            .to_string()
    }

    /// Replaces the metadata and saves it at once. It is not an edit: it takes no
    /// part in undo, and a failure to write costs nothing but the report.
    fn set_metadata(&mut self, metadata: Metadata) {
        self.metadata = metadata;
        if let Err(err) = self.db.save_metadata(&self.metadata) {
            self.last_error = Some(format!("could not save document properties — {err}"));
        }
    }

    /// The metadata as an export publishes it, with the title the document goes by.
    fn published_metadata(&self) -> Metadata {
        let title = if self.metadata.title.is_empty() && self.is_anonymous() {
            String::new()
        } else {
            self.display_title()
        };
        Metadata {
            title,
            ..self.metadata.clone()
        }
    }

    /// What the first save suggests calling the file: the source's name for an
    /// import, whatever title its front matter gave the document.
    fn suggested_file_name(&self) -> String {
        format!("{}.kame", self.source_stem.as_deref().unwrap_or("Untitled"))
    }

    fn document(&self) -> &Document {
        self.editor.document()
    }

    /// The `.kame` file this session is persisted to, or `None` for an
    /// anonymous (unsaved) document.
    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether this document has no file yet (launched with no argument, or a
    /// failed open) — the signal that saving must ask for a path first.
    fn is_anonymous(&self) -> bool {
        self.path.is_none()
    }

    /// Whether an anonymous document has anything worth saving: any prose or
    /// apparatus. An empty Untitled document can close without a prompt.
    fn has_content(&self) -> bool {
        let doc = self.editor.document();
        doc.blocks.iter().any(|block| {
            !block.text.trim().is_empty() || matches!(block.kind, BlockKind::Image { .. })
        }) || !doc.annotations.is_empty()
    }

    fn history(&self) -> (usize, usize) {
        self.editor.history()
    }

    // --- files, exports and snapshots --------------------------------------

    /// The path exports should land beside: the saved file's directory, or the
    /// current directory (as `document.*`) for an anonymous document.
    fn export_base(&self) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(|| PathBuf::from("document.kame"))
    }

    /// Writes the document as Markdown beside the `.kame` file and returns the
    /// path written.
    ///
    /// A failed export is reported through `last_error` but never sets
    /// `save_failed`: the document itself is still stored, so the author can
    /// keep writing and take the export again.
    fn export_markdown(&mut self) -> Option<PathBuf> {
        let path = export_path(&self.export_base(), "md");
        let markdown =
            crate::export::to_markdown_with(self.editor.document(), &self.published_metadata());
        self.write_export(&path, &markdown)
    }

    /// Writes the document as a standalone HTML page beside the `.kame` file
    /// and returns the path written.
    fn export_html(&mut self) -> Option<PathBuf> {
        let path = export_path(&self.export_base(), "html");
        let html = crate::export::to_html_with(self.editor.document(), &self.published_metadata());
        self.write_export(&path, &html)
    }

    /// Persists an anonymous document to `path` for the first time, carrying
    /// the full edit history with it, and switches the session to that file so
    /// subsequent edits land on disk.
    fn save_as(&mut self, path: &Path) -> AppResult<()> {
        let doc = self.editor.document().clone();
        let cursor = self.editor.cursor();
        let ops = self.db.load_ops()?;
        let mut db = Db::open(path)?;
        for (_, op) in ops {
            db.append_op(&op)?;
        }
        db.save_state(&doc, cursor)?;
        db.save_metadata(&self.metadata)?;
        self.db = db;
        self.path = Some(path.to_path_buf());
        self.clear_save_error();
        Ok(())
    }

    /// The one write path for both exports.
    fn write_export(&mut self, path: &Path, contents: &str) -> Option<PathBuf> {
        match std::fs::write(path, contents) {
            Ok(()) => {
                self.clear_error_from("export failed");
                Some(path.to_path_buf())
            }
            Err(err) => {
                self.last_error = Some(format!("export failed — {}: {err}", path.display()));
                None
            }
        }
    }

    /// Stores the current document as a snapshot, labelled `label` when it has
    /// any text, and returns the new id.
    ///
    /// The snapshot is the inverse of [`Session::restore`]: it captures the
    /// text exactly as it stands, independent of the head document and the op
    /// log, so undo and redo cannot move it.
    fn snapshot(&mut self, label: Option<&str>) -> Option<i64> {
        let label = label.map(str::trim).filter(|label| !label.is_empty());
        match self.db.create_snapshot(self.editor.document(), label) {
            Ok(id) => {
                self.clear_error_from("snapshot failed");
                Some(id)
            }
            Err(err) => {
                self.last_error = Some(format!("snapshot failed — {err}"));
                None
            }
        }
    }

    /// The stored snapshots, newest first, or `None` when they could not be
    /// listed (the failure lands in `last_error`).
    fn snapshots(&mut self) -> Option<Vec<SnapshotMeta>> {
        match self.db.list_snapshots() {
            Ok(snapshots) => {
                self.clear_error_from("could not list snapshots");
                Some(snapshots)
            }
            Err(err) => {
                self.last_error = Some(format!("could not list snapshots — {err}"));
                None
            }
        }
    }

    /// Replaces the document with snapshot `id` and persists it.
    ///
    /// A restore is deliberately not an op: the snapshot is the undo point, so
    /// the restored document starts a fresh history and the op log that
    /// described the replaced text is dropped. Returns whether the restore
    /// landed; on failure the in-memory document is untouched.
    fn restore(&mut self, id: i64) -> bool {
        let document = match self.db.restore_snapshot(id) {
            Ok(document) => document,
            Err(err) => {
                self.last_error = Some(format!("restore failed — {err}"));
                return false;
            }
        };
        let Some(caret) = document.blocks.first().map(|block| Cursor {
            block_id: block.id,
            offset: 0,
        }) else {
            self.last_error = Some("restore failed — the snapshot holds no blocks".to_string());
            return false;
        };
        // One transaction writes the restored head, cursor 0 and an empty op
        // log. Splitting these would let a crash or I/O error land the restored
        // text beside the pre-restore log; on reopen `Editor::with_cursor`
        // would then put those stale ops on the redo stack and Ctrl-Shift-Z
        // would replay them against a document they never touched.
        if let Err(err) = self.db.restore_state(&document) {
            self.mark_save_failed(err);
            return false;
        }
        self.editor = Editor::new(document);
        self.caret = caret;
        self.selection = None;
        self.save_failed = false;
        self.last_error = None;
        true
    }

    /// Clears `last_error` when it came from the command named by `prefix`, so
    /// a retried command does not leave its old failure on screen. A save
    /// failure is never cleared here: it is a different class of problem.
    fn clear_error_from(&mut self, prefix: &str) {
        if self
            .last_error
            .as_deref()
            .is_some_and(|error| error.starts_with(prefix))
        {
            self.last_error = None;
        }
    }

    /// Persists the head document and the undo cursor together, the write
    /// path for undo/redo. Returns whether the write landed.
    fn save_state(&mut self) -> bool {
        match self
            .db
            .save_state(self.editor.document(), self.editor.cursor())
        {
            Ok(()) => {
                self.clear_save_error();
                true
            }
            Err(err) => {
                self.mark_save_failed(err);
                false
            }
        }
    }

    /// Persists one applied edit (op + head + cursor in one transaction).
    /// Returns whether the write landed.
    fn record(&mut self, op: &Op) -> bool {
        match self.db.record_edit(self.editor.document(), op) {
            Ok(_) => {
                self.clear_save_error();
                true
            }
            Err(err) => {
                self.mark_save_failed(err);
                false
            }
        }
    }

    /// A failed write makes the session read-only: the in-memory history can
    /// no longer be trusted to match the DB, so further mutations would write
    /// an op log that disagrees with the head cursor.
    fn mark_save_failed(&mut self, err: impl std::fmt::Display) {
        self.save_failed = true;
        self.last_error = Some(format!("save failed — editing disabled: {err}"));
    }

    /// A later successful write re-establishes the head/log relationship.
    /// "A later successful write" here means `record`/`save_state`; commands
    /// are suppressed while the flag is set, so the recovery path is the
    /// error strip's retry (`save_state`) — and the window refuses to close
    /// until it lands.
    fn clear_save_error(&mut self) {
        if self.save_failed {
            self.save_failed = false;
            self.last_error = None;
        }
    }

    /// The error strip's click: retry a failed write, or dismiss a message.
    ///
    /// Only a failed write has something to save, so only it retries. Every
    /// other message — the degraded-open warning, a failed export or snapshot —
    /// has no pending write: retrying there would write state the author never
    /// changed, and `clear_save_error` would not clear it anyway, leaving a
    /// permanently painted strip. Those messages are dismissed instead.
    fn retry_or_dismiss(&mut self) -> bool {
        if self.save_failed {
            self.save_state()
        } else {
            self.last_error = None;
            true
        }
    }

    /// Applies `result` from an editor command and persists it.
    fn commit(&mut self, result: Result<Op, EditError>) -> bool {
        match result {
            Ok(op) => self.record(&op),
            Err(err) => {
                self.last_error = Some(err.to_string());
                false
            }
        }
    }

    /// The caret, repaired against the current document (an undo can remove
    /// the block it points into).
    fn valid_caret(&mut self) -> Cursor {
        self.caret = clamp_cursor(self.editor.document(), self.caret);
        self.caret
    }

    /// Places a bare caret, dropping any selection.
    fn set_caret(&mut self, caret: Cursor) {
        self.caret = clamp_cursor(self.editor.document(), caret);
        self.selection = None;
    }

    /// Extends the selection's moving end to `caret`, keeping the edge where
    /// the selection began. With no selection yet, the current caret becomes
    /// the fixed edge, so a drag selects the span it covers. Collapsing back
    /// onto the fixed edge clears the selection.
    fn select_to(&mut self, caret: Cursor) {
        let origin = self
            .selection
            .map_or(self.caret, |selection| selection.start);
        let caret = clamp_cursor(self.editor.document(), caret);
        self.caret = caret;
        self.selection = if origin == caret {
            None
        } else {
            Some(Selection {
                start: origin,
                end: caret,
            })
        };
    }

    /// The selection with its ends in document order, or `None` without one.
    fn ordered_selection(&self) -> Option<(Cursor, Cursor)> {
        self.ordered(self.selection?)
    }

    /// Puts the two ends of `selection` in document order.
    fn ordered(&self, selection: Selection) -> Option<(Cursor, Cursor)> {
        let start = self.block_index(selection.start.block_id)?;
        let end = self.block_index(selection.end.block_id)?;
        if (start, selection.start.offset) <= (end, selection.end.offset) {
            Some((selection.start, selection.end))
        } else {
            Some((selection.end, selection.start))
        }
    }

    /// Deletes the selected range, leaving the caret where it began.
    ///
    /// Returns `true` when there was nothing to delete or the delete landed in
    /// the database, `false` only on a failed write.
    fn delete_selected(&mut self) -> bool {
        let Some(selection) = self.selection.take() else {
            return true;
        };
        let Some((start, end)) = self.ordered(selection) else {
            return true;
        };
        if start == end {
            return true;
        }
        let result = self.editor.delete_selection(&Selection { start, end });
        if self.commit(result) {
            self.caret = start;
            true
        } else {
            false
        }
    }

    fn block_index(&self, block_id: crate::model::BlockId) -> Option<usize> {
        self.editor
            .document()
            .blocks
            .iter()
            .position(|block| block.id == block_id)
    }

    // --- editing commands --------------------------------------------------

    fn insert(&mut self, text: &str) -> bool {
        if self.save_failed {
            return false;
        }
        // Typing over a selection replaces it.
        if self.selection.is_some() && !self.delete_selected() {
            return false;
        }
        let caret = self.valid_caret();
        match self.editor.insert_text(&caret, text) {
            Ok(op) => {
                let saved = self.record(&op);
                self.caret = Cursor {
                    block_id: caret.block_id,
                    offset: caret.offset + text.len(),
                };
                saved
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                false
            }
        }
    }

    /// Applies a markdown-style prefix at the start of the caret's block (typed
    /// `# `, `## `, `### `, `- `, `* `, `1. `): the prefix is stripped, the
    /// block kind changes, and the caret returns to the start of the block.
    fn apply_markdown_shortcut(&mut self) {
        let caret = self.valid_caret();
        if let Some(op) = self.editor.apply_markdown_prefix(caret.block_id) {
            self.record(&op);
            self.caret = Cursor {
                block_id: caret.block_id,
                offset: 0,
            };
        }
    }

    /// Applies inline emphasis the caret just closed (typed `*x*`, `**x**`,
    /// `_x_`, `__x__`): the markers are stripped, the wrapped text takes a
    /// bold/italic span, and the caret lands after the emphasised text.
    fn apply_inline_markdown(&mut self) {
        let caret = self.valid_caret();
        if let Some((op, offset)) = self.editor.apply_inline_markdown(&caret) {
            self.record(&op);
            self.caret = Cursor {
                block_id: caret.block_id,
                offset,
            };
        }
    }

    /// Applies a markdown link the caret just closed (typed `[text](url)`): the
    /// wrapping is stripped, the anchor text takes a link span, and the caret
    /// lands after the anchor.
    fn apply_markdown_link(&mut self) {
        let caret = self.valid_caret();
        if let Some((op, offset)) = self.editor.apply_markdown_link(&caret) {
            self.record(&op);
            self.caret = Cursor {
                block_id: caret.block_id,
                offset,
            };
        }
    }

    /// Deletes `selection` and parks the caret at `caret_after`.
    fn delete_range(&mut self, selection: Selection, caret_after: Cursor) -> bool {
        let result = self.editor.delete_selection(&selection);
        if self.commit(result) {
            self.caret = caret_after;
            true
        } else {
            false
        }
    }

    fn backspace(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() {
            return self.delete_selected();
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };

        let previous = {
            let document = self.editor.document();
            prev_boundary(&document.blocks[index].text, caret.offset)
        };
        if let Some(offset) = previous {
            let target = Cursor {
                block_id: caret.block_id,
                offset,
            };
            return self.delete_range(
                Selection {
                    start: target,
                    end: caret,
                },
                target,
            );
        }
        if index == 0 {
            return false;
        }

        // At the start of a block: join it to the previous one.
        let (previous_id, previous_len) = {
            let document = self.editor.document();
            let previous = &document.blocks[index - 1];
            (previous.id, previous.text.len())
        };
        let at_end = Cursor {
            block_id: previous_id,
            offset: previous_len,
        };
        let result = self.editor.merge_with_next(&at_end);
        if self.commit(result) {
            self.caret = at_end;
            true
        } else {
            false
        }
    }

    fn delete_forward(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() {
            return self.delete_selected();
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };

        let next = {
            let document = self.editor.document();
            next_boundary(&document.blocks[index].text, caret.offset)
        };
        if let Some(offset) = next {
            let end = Cursor {
                block_id: caret.block_id,
                offset,
            };
            return self.delete_range(Selection { start: caret, end }, caret);
        }
        if index + 1 >= self.editor.document().blocks.len() {
            return false;
        }
        // At the end of a block: pull the next block's text up.
        let result = self.editor.merge_with_next(&caret);
        self.commit(result)
    }

    fn split(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() && !self.delete_selected() {
            return false;
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };
        let result = self.editor.split_paragraph(&caret);
        if !self.commit(result) {
            return false;
        }
        if let Some(next) = self.editor.document().blocks.get(index + 1) {
            self.caret = Cursor {
                block_id: next.id,
                offset: 0,
            };
            true
        } else {
            false
        }
    }

    /// Inserts an image block after the caret and moves the caret into it.
    fn insert_image(&mut self, path: String) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() && !self.delete_selected() {
            return false;
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };
        let result = self.editor.insert_image_block(&caret, path);
        if !self.commit(result) {
            return false;
        }
        if let Some(next) = self.editor.document().blocks.get(index + 1) {
            self.caret = Cursor {
                block_id: next.id,
                offset: 0,
            };
            true
        } else {
            false
        }
    }

    /// Sets a link span over `start..end`, replacing any link already there; an
    /// empty `url` removes it.
    fn set_link(&mut self, block_id: BlockId, start: usize, end: usize, url: String) -> bool {
        if self.save_failed {
            return false;
        }
        let result = self.editor.set_link(block_id, start, end, url);
        self.commit(result)
    }

    /// The range C-k should link — the current selection, or the link under the
    /// caret when nothing is selected — plus that link's current URL, if any,
    /// so the prompt can pre-fill it for editing.
    fn link_target(&self) -> Option<(BlockId, usize, usize, Option<String>)> {
        if let Some((start, end)) = self.ordered_selection() {
            if start.block_id != end.block_id {
                return None; // a span lives in one block
            }
            let url = self
                .link_span_at(start.block_id, start.offset, end.offset)
                .map(|(_, _, url)| url);
            return Some((start.block_id, start.offset, end.offset, url));
        }
        let caret = self.caret;
        let (start, end, url) = self.link_span_at(caret.block_id, caret.offset, caret.offset)?;
        Some((caret.block_id, start, end, Some(url)))
    }

    /// The stored link span covering `lo..hi` (`lo == hi` means "the link
    /// containing the caret"), returning `(start, end, url)`.
    fn link_span_at(
        &self,
        block_id: BlockId,
        lo: usize,
        hi: usize,
    ) -> Option<(usize, usize, String)> {
        let block = self
            .editor
            .document()
            .blocks
            .iter()
            .find(|block| block.id == block_id)?;
        block
            .spans
            .iter()
            .filter_map(|span| match &span.style {
                SpanStyle::Link { url } if span.start <= lo && hi <= span.end => {
                    Some((span.start, span.end, url.clone()))
                }
                _ => None,
            })
            .next()
    }

    /// Changes `block_id`'s kind, persisted as one undoable edit.
    ///
    /// This is the authoring path for headings and list items; without it every
    /// block a user can create is a paragraph.
    fn set_block_kind(&mut self, block_id: BlockId, kind: BlockKind) -> bool {
        if self.save_failed {
            return false;
        }
        let result = self.editor.set_block_kind(block_id, kind);
        self.commit(result)
    }

    /// Inserts pasted text one line per block: each newline becomes a
    /// paragraph break, so the block model keeps its one-paragraph invariant.
    fn paste(&mut self, text: &str) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() && !self.delete_selected() {
            return false;
        }
        let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut changed = false;
        for (index, line) in normalised.split('\n').enumerate() {
            if index > 0 {
                changed |= self.split();
            }
            if !line.is_empty() {
                changed |= self.insert(line);
            }
        }
        changed
    }

    fn undo(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        let Some(op) = self.editor.undo() else {
            return false;
        };
        self.selection = None;
        self.save_state();
        self.place_caret_from(&op);
        true
    }

    fn redo(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        let Some(op) = self.editor.redo() else {
            return false;
        };
        self.selection = None;
        self.save_state();
        self.place_caret_from(&op);
        true
    }

    /// Moves the caret to the position implied by an applied undo/redo op.
    ///
    /// The editor clamps a stale offset but does not move the caret, so an
    /// undo of text typed mid-block used to leave the caret past the erased
    /// span — the next insert then landed in the wrong place.
    fn place_caret_from(&mut self, op: &Op) {
        if let Some(caret) = caret_for_op(self.editor.document(), op) {
            self.set_caret(caret);
        } else {
            self.caret = clamp_cursor(self.editor.document(), self.caret);
        }
    }

    /// Anchors a new annotation of `kind` over the selection, or over the word
    /// under a bare caret, and persists it.
    ///
    /// Returns the new id so the caller can open its popover. `None` means the
    /// write failed (the session is now read-only) or the caret names no block.
    fn add_annotation(&mut self, kind: AnnotationKind) -> Option<AnnotationId> {
        if self.save_failed {
            return None;
        }
        let anchor = self.annotation_anchor(kind)?;
        let snippet = anchor
            .byte_range()
            .and_then(|(start, end)| {
                self.editor
                    .document()
                    .blocks
                    .iter()
                    .find(|block| block.id == anchor.block_id())
                    .map(|block| {
                        &block.text[start.min(block.text.len())..end.min(block.text.len())]
                    })
            })
            .unwrap_or("");
        let body = default_annotation_body(&kind, snippet);
        match self.editor.add_annotation(kind, anchor, &body) {
            Ok(op) => {
                let id = match &op {
                    Op::AddAnnotation { annotation } => annotation.id,
                    _ => return None,
                };
                if self.record(&op) {
                    Some(id)
                } else {
                    None
                }
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                None
            }
        }
    }

    /// Replaces a note's body and persists it as one undoable edit.
    ///
    /// Returns whether the write landed; a refused edit leaves `last_error` set
    /// so the popover's caller can see why nothing was saved.
    fn set_annotation_body(&mut self, id: AnnotationId, body: &str) -> bool {
        if self.save_failed {
            return false;
        }
        // An emptied marginalia note is removed rather than kept as an empty
        // margin entry.
        let is_empty_marginal = body.trim().is_empty()
            && self.editor.document().annotations.iter().any(|annotation| {
                annotation.id == id && annotation.kind == AnnotationKind::Marginal
            });
        let result = if is_empty_marginal {
            self.editor.remove_annotation(id)
        } else {
            self.editor.set_annotation_body(id, body)
        };
        self.commit(result)
    }

    /// Removes an annotation (a highlight, marginal or footnote) outright,
    /// returning whether the removal was persisted as one undoable edit.
    fn remove_annotation(&mut self, id: AnnotationId) -> bool {
        if self.save_failed {
            return false;
        }
        let result = self.editor.remove_annotation(id);
        self.commit(result)
    }

    /// The anchor for a new annotation of `kind`: marginalia anchor to a whole
    /// block, notes and footnotes to the selection (clamped to its first block,
    /// since an anchor lives in one block) or the word under the caret.
    fn annotation_anchor(&self, kind: AnnotationKind) -> Option<Anchor> {
        let document = self.editor.document();
        let block_id = if let Some((start, _)) = self.ordered_selection() {
            start.block_id
        } else {
            self.caret.block_id
        };
        document.blocks.iter().find(|block| block.id == block_id)?;

        if kind == AnnotationKind::Marginal {
            return Some(Anchor::block(block_id));
        }

        if let Some((start, end)) = self.ordered_selection() {
            let block = document
                .blocks
                .iter()
                .find(|block| block.id == start.block_id)?;
            let end = if end.block_id == start.block_id {
                end.offset
            } else {
                block.text.len()
            };
            let start_offset = start.offset.min(end);
            return Some(Anchor::range(block.id, start_offset, end));
        }

        let caret = self.caret;
        let block = document
            .blocks
            .iter()
            .find(|block| block.id == caret.block_id)?;
        let (start, end) = word_at(&block.text, caret.offset);
        Some(Anchor::range(block.id, start, end))
    }

    fn move_left(&mut self, select: bool) {
        // An arrow collapses a selection to its edge before moving — unless it
        // is extending the selection, which keeps the fixed edge and moves the
        // caret end.
        if !select {
            if let Some((start, _)) = self.ordered_selection() {
                self.caret = start;
                self.selection = None;
                return;
            }
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return;
        };
        let target = {
            let document = self.editor.document();
            let block = &document.blocks[index];
            match prev_boundary(&block.text, caret.offset) {
                Some(offset) => Cursor {
                    block_id: block.id,
                    offset,
                },
                None if index > 0 => {
                    let previous = &document.blocks[index - 1];
                    Cursor {
                        block_id: previous.id,
                        offset: previous.text.len(),
                    }
                }
                None => caret,
            }
        };
        if select {
            self.select_to(target);
        } else {
            self.caret = target;
        }
    }

    fn move_right(&mut self, select: bool) {
        if !select {
            if let Some((_, end)) = self.ordered_selection() {
                self.caret = end;
                self.selection = None;
                return;
            }
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return;
        };
        let target = {
            let document = self.editor.document();
            let block = &document.blocks[index];
            match next_boundary(&block.text, caret.offset) {
                Some(offset) => Cursor {
                    block_id: block.id,
                    offset,
                },
                None => match document.blocks.get(index + 1) {
                    Some(next) => Cursor {
                        block_id: next.id,
                        offset: 0,
                    },
                    None => caret,
                },
            }
        };
        if select {
            self.select_to(target);
        } else {
            self.caret = target;
        }
    }

    /// Ctrl-arrow: move left by a whole word. A caret inside a word jumps to
    /// its start; one between words jumps to the previous word's start. At the
    /// start of a block the caret moves to the end of the previous block.
    fn move_left_word(&mut self, select: bool) {
        if !select {
            if let Some((start, _)) = self.ordered_selection() {
                self.caret = start;
                self.selection = None;
                return;
            }
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return;
        };
        let target = {
            let document = self.editor.document();
            let block = &document.blocks[index];
            match prev_word(&block.text, caret.offset) {
                Some(offset) => Cursor {
                    block_id: block.id,
                    offset,
                },
                None if index > 0 => {
                    let previous = &document.blocks[index - 1];
                    Cursor {
                        block_id: previous.id,
                        offset: previous.text.len(),
                    }
                }
                None => caret,
            }
        };
        if select {
            self.select_to(target);
        } else {
            self.caret = target;
        }
    }

    /// Ctrl-arrow: move right by a whole word, landing on the next word's
    /// start. At the end of a block the caret moves to the start of the next.
    fn move_right_word(&mut self, select: bool) {
        if !select {
            if let Some((_, end)) = self.ordered_selection() {
                self.caret = end;
                self.selection = None;
                return;
            }
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return;
        };
        let target = {
            let document = self.editor.document();
            let block = &document.blocks[index];
            match next_word(&block.text, caret.offset) {
                Some(offset) => Cursor {
                    block_id: block.id,
                    offset,
                },
                None => match document.blocks.get(index + 1) {
                    Some(next) => Cursor {
                        block_id: next.id,
                        offset: 0,
                    },
                    None => caret,
                },
            }
        };
        if select {
            self.select_to(target);
        } else {
            self.caret = target;
        }
    }
}

/// Clamps a caret to a block that exists and to a character boundary inside it.
fn clamp_cursor(document: &Document, cursor: Cursor) -> Cursor {
    match document
        .blocks
        .iter()
        .find(|block| block.id == cursor.block_id)
    {
        Some(block) => {
            let mut offset = cursor.offset.min(block.text.len());
            while offset > 0 && !block.text.is_char_boundary(offset) {
                offset -= 1;
            }
            Cursor {
                block_id: block.id,
                offset,
            }
        }
        None => document
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .unwrap_or(cursor),
    }
}

/// Caret position implied by an applied op: the start of erased text, the end
/// of inserted text, or the start of a block that now exists.
///
/// `Op::Batch` members are scanned in order, skipping members whose block did
/// not survive the operation (undoing a split returns a batch whose first
/// member deletes the block the split created; the text member that follows
/// carries the split point).
fn caret_for_op(document: &Document, op: &Op) -> Option<Cursor> {
    let existing = |block_id: crate::model::BlockId| {
        document
            .blocks
            .iter()
            .any(|block| block.id == block_id)
            .then_some(block_id)
    };
    match op {
        Op::InsertText {
            block_id,
            offset,
            text,
        } => existing(*block_id).map(|block_id| Cursor {
            block_id,
            offset: offset + text.len(),
        }),
        Op::DeleteText {
            block_id, start, ..
        } => existing(*block_id).map(|block_id| Cursor {
            block_id,
            offset: *start,
        }),
        Op::InsertBlock { block, .. } | Op::DeleteBlock { block, .. } => {
            existing(block.id).map(|block_id| Cursor {
                block_id,
                offset: 0,
            })
        }
        Op::Batch { ops } => ops.iter().find_map(|member| caret_for_op(document, member)),
        _ => None,
    }
}

/// Maps a click to a caret in the nearest block, by vertical distance.
///
/// Clicks in the page padding above or below the text, and in the gap between
/// two blocks, used to fall through to `last().end` and jump the caret to the
/// end of the document. The nearest block now wins and the click's X is
/// clamped into that block's column.
fn hit_test(
    blocks: &[Block],
    layouts: &[&BlockLayout],
    tops: &[f32],
    heights: &[f32],
    body_left: f32,
    pos: Pos2,
) -> Option<Cursor> {
    let count = blocks
        .len()
        .min(layouts.len())
        .min(tops.len())
        .min(heights.len());
    let mut nearest: Option<(usize, f32)> = None;
    for index in 0..count {
        let top = tops[index];
        let bottom = top + heights[index];
        let distance = if pos.y < top {
            top - pos.y
        } else if pos.y > bottom {
            pos.y - bottom
        } else {
            0.0
        };
        if nearest.map_or(true, |(_, best)| distance < best) {
            nearest = Some((index, distance));
        }
    }
    let (index, _) = nearest?;
    let block = &blocks[index];
    let x = (pos.x - body_left - layouts[index].left_indent).max(0.0);
    let y = (pos.y - tops[index]).clamp(0.0, heights[index]);
    let offset = offset_at(layouts[index], x, y, block.text.len());
    Some(Cursor {
        block_id: block.id,
        offset,
    })
}

/// The URL under `cursor`, when the caret sits inside a link in that block.
/// Stored `[text](url)` spans take precedence; a bare URL is the fallback.
fn url_at(blocks: &[Block], cursor: &Cursor) -> Option<String> {
    let block = blocks.iter().find(|block| block.id == cursor.block_id)?;
    if let Some(url) = block
        .spans
        .iter()
        .filter_map(|span| match &span.style {
            SpanStyle::Link { url } if span.start <= cursor.offset && cursor.offset < span.end => {
                Some(url.clone())
            }
            _ => None,
        })
        .next()
    {
        return Some(url);
    }
    link_ranges(&block.text)
        .into_iter()
        .find(|(start, end)| cursor.offset >= *start && cursor.offset < *end)
        .map(|(start, end)| block.text[start..end].to_string())
}

/// Opens `url` in the system browser, best-effort: a link that cannot launch is
/// never worth an error dialog in the editor.
fn open_in_browser(url: &str) {
    if is_script_url(url) {
        return;
    }
    #[cfg(target_os = "macos")]
    let (program, arg) = ("open", url);
    #[cfg(not(target_os = "macos"))]
    let (program, arg) = ("xdg-open", url);
    let _ = std::process::Command::new(program).arg(arg).spawn();
}

/// The space below a block of kind `block` when `next` follows it. A heading sits
/// close to what it introduces and far from what it ends, more so the larger it
/// is, and a quote is set off from the prose around it.
fn gap_after(block: &BlockKind, next: &BlockKind) -> f32 {
    let ratio = match (block, next) {
        (BlockKind::Heading { .. }, _) => 0.4,
        (_, BlockKind::Heading { level }) => match level {
            0 | 1 => 2.6,
            2 => 2.2,
            _ => 1.7,
        },
        (BlockKind::BlockQuote, BlockKind::BlockQuote) => 1.0,
        (BlockKind::BlockQuote, _) | (_, BlockKind::BlockQuote) => 1.4,
        _ => 1.0,
    };
    BLOCK_GAP * ratio
}

/// Minimum height of a block, so an empty paragraph still has a line to click
/// and a caret to show.
fn block_height(block: &Block, layout: &BlockLayout, config: &LayoutConfig) -> f32 {
    if let BlockKind::Image { path } = &block.kind {
        return image_height(path, config.body_width);
    }
    let minimum = match block.kind {
        crate::model::BlockKind::Heading { level } => heading_size_for(config, level) * 1.4,
        _ => config.body_line_height,
    };
    layout.height.max(minimum)
}

/// Height of the placeholder drawn for an image that cannot be read.
const MISSING_IMAGE_HEIGHT: f32 = 72.0;

/// Display height of an image at `width`, preserving its aspect ratio. A file
/// that cannot be read (or reports a zero dimension) takes a short placeholder's
/// height, which says what is missing without leaving a blank stretch of page.
fn image_height(path: &str, width: f32) -> f32 {
    match image::image_dimensions(path) {
        Ok((w, h)) if w > 0 && h > 0 => width * h as f32 / w as f32,
        _ => MISSING_IMAGE_HEIGHT,
    }
}

/// Default body for a newly anchored annotation.
///
/// A note starts empty — it is a bare highlight, and its comment is the
/// author's to add in the popover. Marginalia and footnotes quote the anchored
/// words as a starting point (they are painted in place rather than hidden
/// behind a popover).
fn default_annotation_body(kind: &AnnotationKind, snippet: &str) -> String {
    let snippet = shorten(snippet.trim(), 48);
    if snippet.is_empty() {
        return match kind {
            AnnotationKind::Marginal => "New marginal note.".to_string(),
            AnnotationKind::Note => String::new(),
            AnnotationKind::Footnote => "New footnote.".to_string(),
        };
    }
    match kind {
        AnnotationKind::Marginal => format!("Marginal note on “{snippet}”"),
        AnnotationKind::Note => String::new(),
        AnnotationKind::Footnote => format!("Footnote on “{snippet}”"),
    }
}

/// Truncates to `max` characters (not bytes) with an ellipsis.
fn shorten(text: &str, max: usize) -> String {
    let mut out = String::new();
    for (index, character) in text.chars().enumerate() {
        if index == max {
            out.push('…');
            break;
        }
        out.push(character);
    }
    out
}

/// egui id of the popover's body field. `handle_input` looks it up to tell
/// whether keystrokes belong to the note instead of the document.
fn note_body_edit_id() -> Id {
    Id::unique("kame-note-body")
}

/// The open find bar's state. `matches` is the live result of searching
/// `query`; `index` is the match the caret is on.
#[derive(Default)]
struct Find {
    query: String,
    matches: Vec<(BlockId, usize, usize)>,
    index: usize,
    /// Grab focus on the frame the bar first appears.
    focus: bool,
}

/// egui id of the find bar's query field. `handle_input` looks it up to tell
/// whether keystrokes belong to the find bar instead of the document.
fn find_query_id() -> Id {
    Id::unique("kame-find-query")
}

/// Widget id of the snapshot prompt's label field, so the prompt can give it
/// the keyboard on the frame it opens.
fn snapshot_label_edit_id() -> Id {
    Id::unique("kame-snapshot-label")
}

/// Short label for an annotation kind, shown above its body in the popover.
fn annotation_kind_label(kind: &AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Marginal => "marginal note",
        AnnotationKind::Note => "note",
        AnnotationKind::Footnote => "footnote",
    }
}

/// Page position of a margin placement.
///
/// Body-relative X and page Y: the placement's X is measured from the body
/// measure's left edge, its Y is already composed with the anchor block's page
/// top (see [`place_marginalia`]), so only the body's left edge is added here.
fn margin_origin(body_left: f32, placement: &MarginalPlacement) -> Pos2 {
    Pos2::new(body_left + placement.x, placement.y)
}

/// Page position of a footnote entry: footnotes span the measure and their Y
/// is measured from the painter's top.
fn footnote_origin(page_top: f32, body_left: f32, placement: &FootnotePlacement) -> Pos2 {
    Pos2::new(body_left, page_top + placement.y)
}

/// Page position of a footnote's superscript number.
///
/// The number is set at the END of the anchored range — the pen after the last
/// anchored glyph — so it clears the word's ink instead of printing inside it.
/// An anchor that ends exactly at a visual line's end resolves to that line:
/// cosmic-text drops the trailing wrap space from the line's glyph run, so the
/// last word of every wrapped line ends there, and `caret_x` already returns
/// the pen after its last glyph. Only an end that falls outside every line's
/// glyph range (inside the dropped wrap whitespace) falls back to the range
/// start. The mark is raised a tenth of the line down from the line top, which
/// puts a 0.62× numeral near the cap height rather than on the body baseline,
/// and X is clamped into the measure so a word flush to the right edge cannot
/// push the number into the margin.
fn superscript_origin(
    layout: &BlockLayout,
    anchor: &Anchor,
    body_left: f32,
    block_top: f32,
    body_width: f32,
    room: f32,
) -> Option<Pos2> {
    let (anchor_start, anchor_end) = anchor.byte_range()?;
    let end = anchor_end.max(anchor_start);
    let end_line = layout
        .lines
        .iter()
        .find(|line| end >= line.start && end <= line.end);
    let (line, x) = match end_line {
        Some(line) => (*line, caret_x(layout, end)),
        None => (
            caret_line(layout, anchor_start)?,
            caret_x(layout, anchor_start),
        ),
    };
    Some(Pos2::new(
        body_left + layout.left_indent + (x - room).clamp(0.0, body_width),
        block_top + line.y + line.height * 0.1,
    ))
}

// --- eframe application ----------------------------------------------------

struct KameApp {
    session: Session,
    renderer: GlyphRenderer,
    layouts: LayoutCache,
    textures: TextureCache,
    /// Margin bodies and footnote entries, shaped in their own columns.
    annotation_textures: AnnotationTextureCache,
    /// Decoded image textures for `BlockKind::Image` blocks, keyed by path.
    image_textures: HashMap<String, egui::TextureHandle>,
    /// The note whose popover is open, and where its marker was painted.
    open_note: Option<AnnotationId>,
    open_note_pos: Option<Pos2>,
    /// The open note's body while the popover's field holds it. Seeded from the
    /// document when the popover opens and written back to the document on
    /// every change, so the field and the note never drift apart.
    open_note_body: String,
    /// Set when a popover opens, so the body field can grab focus on the frame
    /// it first appears: a note that was just created is a note you meant to
    /// write, and the keystrokes that follow should land in it.
    open_note_focus: bool,
    /// Distraction-free view toggles (never persisted).
    modes: Modes,
    /// The body font size, in px. Every other size (leading, headings, margin
    /// apparatus, list gutter) is a ratio of this, so one number scales the
    /// whole page. A kame setting — persisted to the app config, never to the
    /// document.
    body_size: f32,
    /// Dark mode, persisted as a kame setting.
    dark: bool,
    /// The theme colours derived from `dark`.
    palette: Palette,
    /// The caret position typewriter mode last centred. A new position scrolls
    /// the page so the caret returns to the middle; an unchanged one leaves the
    /// reader free to scroll away.
    typewriter_anchor: Option<(BlockId, usize)>,
    /// Scroll offset requested by last frame's typewriter centring. Applied to
    /// the scroll area on the next frame, when its geometry is known.
    pending_scroll: Option<f32>,
    /// The "Snapshot Now" label prompt, when it is open.
    snapshot_prompt: Option<SnapshotPrompt>,
    /// The "Open" path prompt (Ctrl-O), when it is open.
    open_prompt: Option<OpenPrompt>,
    /// The "Insert image" path prompt, when it is open.
    image_prompt: Option<ImagePrompt>,
    /// The "Add link" URL prompt (C-k), when it is open.
    link_prompt: Option<LinkPrompt>,
    /// The "Restore Snapshot" picker, when it is open. At most one of this and
    /// `snapshot_prompt` is up at a time.
    restore_picker: Option<RestorePicker>,
    /// What the last file command did, shown in the status panel until the
    /// next one. Failures are reported through `Session::last_error` instead.
    notice: Option<String>,
    notice_tracker: NoticeTracker,
    /// The Document Properties dialog, while it is up.
    metadata_prompt: Option<MetadataPrompt>,
    /// A menu chord waiting for the menu bar to be drawn.
    pending_menu: Option<BarMenu>,
    /// Keeps a failed write from being discarded by a stray close, while still
    /// letting the author quit on a second request.
    close: CloseGuard,
    /// The "save this anonymous document before closing?" prompt, when it is
    /// open. Distinct from the failed-write guard: this asks the author whether
    /// to save an unsaved document, the guard refuses a close that would lose
    /// a write that already failed.
    close_flow: CloseFlow,
    /// The find bar, when it is open.
    find: Option<Find>,
    /// Set when a find jump moves the caret to a match that may be off-screen;
    /// the next frame's draw scrolls the caret into view.
    reveal_caret: bool,
    /// The help dialog (usage, keybindings), when it is open.
    show_help: bool,
}

impl KameApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        session: Session,
        notice: Option<String>,
        body_size: f32,
        dark: bool,
    ) -> KameApp {
        cc.egui_ctx.set_visuals(if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        install_fonts(&cc.egui_ctx);
        KameApp {
            session,
            renderer: GlyphRenderer::new(LayoutConfig::default()),
            layouts: LayoutCache::new(),
            textures: TextureCache::new(),
            annotation_textures: AnnotationTextureCache::new(),
            image_textures: HashMap::new(),
            open_note: None,
            open_note_pos: None,
            open_note_body: String::new(),
            open_note_focus: false,
            modes: Modes::default(),
            body_size,
            dark,
            palette: Palette::for_mode(dark),
            typewriter_anchor: None,
            pending_scroll: None,
            snapshot_prompt: None,
            open_prompt: None,
            image_prompt: None,
            link_prompt: None,
            restore_picker: None,
            notice,
            notice_tracker: NoticeTracker::default(),
            metadata_prompt: None,
            pending_menu: None,
            close: CloseGuard::default(),
            close_flow: CloseFlow::default(),
            find: None,
            reveal_caret: false,
            show_help: false,
        }
    }

    /// Opens `id`'s popover (or closes it with `None`), seeding the editable
    /// body from the document.
    ///
    /// Opening is the one moment the field has to agree with the document: after
    /// that the field is the editor for that note and every change is committed
    /// as it is typed.
    fn set_open_note(&mut self, id: Option<AnnotationId>) {
        let body = id.and_then(|id| {
            self.session
                .document()
                .annotations
                .iter()
                .find(|annotation| annotation.id == id)
                .map(|annotation| annotation.body.clone())
        });
        // A note that no longer exists cannot be edited: an undo that removed it
        // closes the popover instead of leaving a field pointing at nothing.
        self.open_note = match (id, body.is_some()) {
            (Some(id), true) => Some(id),
            _ => None,
        };
        self.open_note_body = body.unwrap_or_default();
        self.open_note_focus = self.open_note.is_some();
    }

    /// Applies the mode chords pressed this frame, and mirrors the window's
    /// actual fullscreen state so a window-manager toggle cannot leave the
    /// flag disagreeing with the window.
    fn handle_modes(&mut self, ctx: &egui::Context) {
        if let Some(fullscreen) = ctx.input(|input| input.viewport().fullscreen) {
            self.modes.fullscreen = fullscreen;
        }
        // Read the chords from the event stream rather than from
        // `key_pressed`: each event carries the modifiers that were down when
        // the key went down, so a chord is never mistaken for the bare letter.
        let toggles = ctx.input(|input| mode_commands_for_events(&input.events));
        for mode in toggles {
            self.apply_mode(ctx, mode);
        }
        if ctx.input(|input| dark_toggle_for_events(&input.events)) {
            self.apply_dark_toggle(ctx);
        }
    }

    /// Flips a view mode, from its chord or the menu.
    fn apply_mode(&mut self, ctx: &egui::Context, mode: ModeCommand) {
        let on = self.modes.toggle(mode);
        // Most modes are painted from the flag alone; fullscreen is the one
        // that has to ask the window, in both directions.
        if let Some(command) = viewport_effect(mode, on) {
            ctx.send_viewport_cmd(command);
        }
        // Entering typewriter mode recentres at once, even if the caret
        // has not moved since the last keystroke.
        if mode == ModeCommand::Typewriter && on {
            self.typewriter_anchor = None;
        }
    }

    /// Flips dark mode and repaints the widgets to match.
    fn apply_dark_toggle(&mut self, ctx: &egui::Context) {
        self.toggle_dark();
        ctx.set_visuals(if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
    }

    /// Whether a file dialog is up. While one is, it owns the keyboard.
    fn dialog_open(&self) -> bool {
        self.snapshot_prompt.is_some()
            || self.metadata_prompt.is_some()
            || self.open_prompt.is_some()
            || self.image_prompt.is_some()
            || self.link_prompt.is_some()
            || self.restore_picker.is_some()
    }

    /// Closes whichever file dialog is open. Returns whether one was closed, so
    /// Escape can fall through to the note popover only when it was not.
    fn dismiss_dialog(&mut self) -> bool {
        let open = self.dialog_open();
        self.metadata_prompt = None;
        self.snapshot_prompt = None;
        self.open_prompt = None;
        self.image_prompt = None;
        self.link_prompt = None;
        self.restore_picker = None;
        open
    }

    /// Runs the file-command chords pressed this frame. A chord is never read
    /// while a dialog is open: the dialog owns those keystrokes.
    fn handle_file_commands(&mut self, ctx: &egui::Context) {
        if self.dialog_open() {
            return;
        }
        let commands = ctx.input(|input| file_commands_for_events(&input.events));
        for command in commands {
            self.run_file_command(command);
        }
    }

    /// The one entry point for the four file commands, shared by the chords and
    /// the toolbar buttons so the two cannot drift apart.
    fn run_file_command(&mut self, command: FileCommand) {
        match command {
            FileCommand::ExportMarkdown => {
                self.notice = self
                    .session
                    .export_markdown()
                    .map(|path| format!("exported Markdown to {}", path.display()));
            }
            FileCommand::ExportHtml => {
                self.notice = self
                    .session
                    .export_html()
                    .map(|path| format!("exported HTML to {}", path.display()));
            }
            FileCommand::SnapshotNow => {
                self.restore_picker = None;
                self.notice = None;
                self.snapshot_prompt = Some(SnapshotPrompt {
                    label: String::new(),
                    focus: true,
                });
            }
            FileCommand::RestoreSnapshot => {
                let Some(snapshots) = self.session.snapshots() else {
                    // `last_error` carries the reason; the notice would only
                    // contradict it.
                    self.notice = None;
                    return;
                };
                self.snapshot_prompt = None;
                if snapshots.is_empty() {
                    self.notice = Some("no snapshots yet — take one with Ctrl-Shift-S".into());
                } else {
                    self.notice = None;
                    self.restore_picker = Some(RestorePicker::new(snapshots));
                }
            }
        }
    }

    /// Restores snapshot `id`, after the picker's confirmation step.
    fn restore_snapshot(&mut self, id: i64) {
        self.restore_picker = None;
        if !self.session.restore(id) {
            self.notice = None;
            return;
        }
        self.notice = Some(format!("restored snapshot #{id}"));
        // The popover's note and the typewriter's centring both pointed into
        // the text that was just replaced, so that view state goes with it.
        self.set_open_note(None);
        self.typewriter_anchor = None;
        self.pending_scroll = None;
    }

    /// Applies a block-kind change to the caret's block. The toolbar and the
    /// Ctrl-Shift chords share this one path so they cannot drift apart.
    fn set_block_kind(&mut self, kind: BlockKind) {
        let caret = self.session.valid_caret();
        self.session.set_block_kind(caret.block_id, kind);
    }

    /// Sets the body font size, clamping to the legal range and persisting it
    /// as a kame setting so the choice survives a restart.
    fn set_body_size(&mut self, size: f32) {
        let size = size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
        if size != self.body_size {
            self.body_size = size;
            self.persist();
        }
    }

    /// Toggles dark mode, re-theming the page and the chrome together.
    fn toggle_dark(&mut self) {
        self.dark = !self.dark;
        self.palette = Palette::for_mode(self.dark);
        // Rasterized glyphs were inked with the old palette; drop them so the
        // next frame re-rasterizes in the new ink.
        self.layouts.clear();
        self.textures.clear();
        self.annotation_textures.clear();
        self.persist();
    }

    /// Writes the current preferences to the app config, best-effort.
    fn persist(&self) {
        save_config(&KameConfig {
            body_size: self.body_size,
            dark: self.dark,
        });
    }

    /// Steps the body font size, from which every other size is derived.
    fn adjust_font_size(&mut self, step: f32) {
        self.set_body_size(self.body_size + step);
    }

    /// Opens or closes the find bar.
    fn toggle_find(&mut self) {
        if self.find.take().is_some() {
            return;
        }
        // The find bar and the note popover share the keyboard; only one owns it.
        self.set_open_note(None);
        self.find = Some(Find {
            focus: true,
            ..Find::default()
        });
    }

    /// Recomputes the matches for the current query and selects the first.
    fn refresh_find(&mut self) {
        {
            let Some(find) = &mut self.find else { return };
            find.matches = find_matches(self.session.document(), &find.query);
            find.index = 0;
        }
        self.jump_to_match();
    }

    /// Moves to the next (or previous) match, wrapping, and selects it. The
    /// matches are recomputed from the live document so edits made while the
    /// bar is open cannot leave the caret jumping to stale offsets.
    fn step_find(&mut self, forward: bool) {
        {
            let Some(find) = &mut self.find else { return };
            find.matches = find_matches(self.session.document(), &find.query);
            if find.matches.is_empty() {
                return;
            }
            find.index = if forward {
                (find.index + 1) % find.matches.len()
            } else {
                find.index.checked_sub(1).unwrap_or(find.matches.len() - 1)
            };
        }
        self.jump_to_match();
    }

    /// Puts the caret on the current match, selects it, and asks the page to
    /// reveal it next frame.
    fn jump_to_match(&mut self) {
        let (block_id, start, end) = match &self.find {
            Some(find) => match find.matches.get(find.index) {
                Some(&m) => m,
                None => return,
            },
            None => return,
        };
        self.session.set_caret(Cursor {
            block_id,
            offset: end,
        });
        self.session.select_to(Cursor {
            block_id,
            offset: start,
        });
        self.reveal_caret = true;
    }

    /// The find bar: a query field, a match count, and next/previous/close.
    fn show_find_bar(&mut self, ui: &mut egui::Ui) {
        if self.find.is_none() {
            return;
        }
        // Snapshot the state into locals so the closure borrows them, not
        // `self`, and the write-back happens once the panel closes.
        let (mut query, focus, count) = {
            let find = self.find.as_ref().expect("find is open");
            let count = if find.matches.is_empty() {
                if find.query.is_empty() {
                    String::new()
                } else {
                    "no matches".to_string()
                }
            } else {
                format!("{} of {}", find.index + 1, find.matches.len())
            };
            (find.query.clone(), find.focus, count)
        };

        let mut changed = false;
        let mut next = false;
        let mut prev = false;
        let mut close = false;
        egui::Panel::top("kame-find").show(ui, |ui| {
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut query)
                        .id(find_query_id())
                        .hint_text("find")
                        .desired_width(220.0),
                );
                changed = field.changed();
                if focus {
                    field.request_focus();
                }
                if !count.is_empty() {
                    ui.label(egui::RichText::new(&count).weak());
                }
                prev = ui
                    .small_button("↑")
                    .on_hover_text("previous (Shift-Enter)")
                    .clicked();
                next = ui.small_button("↓").on_hover_text("next (Enter)").clicked();
                close = ui.small_button("×").on_hover_text("close (Esc)").clicked();
            });
        });

        if changed {
            if let Some(find) = &mut self.find {
                find.query = query;
                find.focus = false;
            }
            self.refresh_find();
        } else if let Some(find) = &mut self.find {
            find.focus = false;
        }
        if close {
            self.find = None;
            return;
        }
        if next {
            self.step_find(true);
        } else if prev {
            self.step_find(false);
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        // Read first, so the chord's text is gone before anything can type it.
        if !self.dialog_open() && !self.modes.hide_chrome {
            self.pending_menu = ctx
                .input_mut(|input| take_menu_chord(&mut input.events, cfg!(target_os = "macos")));
        }
        // Modes and file commands are window-level, so they are read before the
        // note field's focus check: a chord should work whether the prose or a
        // note has the keyboard.
        self.handle_modes(ctx);
        self.handle_file_commands(ctx);
        // Ctrl-F toggles the find bar, a window-level chord like the mode and
        // file chords (so it works whether the prose or a note has focus).
        let find_chord = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::F,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if modifiers.command && !modifiers.shift
                )
            })
        });
        if find_chord {
            self.toggle_find();
        }
        // Ctrl-O opens the path prompt, replacing the current document. It is a
        // window-level chord like find, so it works whether prose or a note has
        // focus, and it clears any other file dialog so the prompt is the one
        // thing on screen.
        let open_chord = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::O,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if modifiers.command && !modifiers.shift
                )
            })
        });
        if open_chord {
            self.begin_open();
        }
        // Ctrl-K opens the link prompt on the current selection (or the link
        // under the caret), so a link can be added or edited from the keyboard.
        let link_chord = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::K,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if modifiers.command && !modifiers.shift
                )
            })
        });
        if link_chord {
            self.begin_link();
        }
        // Ctrl-S saves. A document already on disk is saved continuously, so
        // this is a no-op there; an anonymous document prompts for a file first.
        let save_chord = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::S,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if modifiers.command && !modifiers.shift
                )
            })
        });
        if save_chord {
            self.save_action(ctx, false);
        }
        // F1 toggles the help dialog (the usage and keybinding reference).
        if ctx.input(|input| input.key_pressed(egui::Key::F1)) {
            self.show_help = !self.show_help;
        }
        // Ctrl + / Ctrl - / Ctrl 0 step the body font size (every other size is
        // a ratio of it). `+` is Shift+= on most layouts, so both are accepted.
        let (font_inc, font_dec, font_reset) = ctx.input(|input| {
            let mut inc = false;
            let mut dec = false;
            let mut reset = false;
            for event in &input.events {
                let egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } = event
                else {
                    continue;
                };
                if !modifiers.command {
                    continue;
                }
                if *key == egui::Key::Plus || (*key == egui::Key::Equals && modifiers.shift) {
                    inc = true;
                }
                if *key == egui::Key::Minus {
                    dec = true;
                }
                if *key == egui::Key::Num0 && !modifiers.shift {
                    reset = true;
                }
            }
            (inc, dec, reset)
        });
        if font_reset {
            self.set_body_size(DEFAULT_BODY_SIZE);
        } else if font_inc {
            self.adjust_font_size(FONT_SIZE_STEP);
        } else if font_dec {
            self.adjust_font_size(-FONT_SIZE_STEP);
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            // Escape closes help, then the find bar, then a dialog, then the note.
            if self.show_help {
                self.show_help = false;
                return;
            }
            if self.find.take().is_some() {
                return;
            }
            if !self.dismiss_dialog() {
                self.set_open_note(None);
            }
        }
        // A file dialog owns the keyboard while it is up: its label field would
        // otherwise also type into the document behind it.
        if self.dialog_open() {
            return;
        }
        // So does an open menu, whose arrow keys and Enter choose an item.
        if egui::Popup::is_any_open(ctx) {
            return;
        }
        // While the popover's body field has focus the keystrokes are the
        // note's, not the document's: without this the same character would be
        // inserted into both the note and the prose behind it.
        if ctx.memory(|memory| memory.has_focus(note_body_edit_id())) {
            return;
        }
        // While the find bar's query field has focus, Enter steps through the
        // matches and the document gets no keystrokes.
        if self.find.is_some() && ctx.memory(|memory| memory.has_focus(find_query_id())) {
            let next =
                ctx.input(|input| input.key_pressed(egui::Key::Enter) && !input.modifiers.shift);
            let prev =
                ctx.input(|input| input.key_pressed(egui::Key::Enter) && input.modifiers.shift);
            if next {
                self.step_find(true);
            } else if prev {
                self.step_find(false);
            }
            return;
        }
        let events = ctx.input(|input| input.events.clone());
        for event in &events {
            let Some(command) = command_for_event(event) else {
                continue;
            };
            match command {
                Command::Insert(text) => {
                    if self.session.insert(&text) {
                        self.session.apply_markdown_shortcut();
                        self.session.apply_inline_markdown();
                        self.session.apply_markdown_link();
                    }
                }
                Command::Paste(text) => {
                    self.session.paste(&text);
                }
                Command::Backspace => {
                    self.session.backspace();
                }
                Command::Delete => {
                    self.session.delete_forward();
                }
                Command::SplitParagraph => {
                    self.session.split();
                }
                Command::Undo => {
                    // Undo can rewrite this note's body (and can remove the note
                    // outright), so the open popover is reseeded from the
                    // document rather than left holding stale prose.
                    if self.session.undo() {
                        self.set_open_note(self.open_note);
                    }
                }
                Command::Redo => {
                    if self.session.redo() {
                        self.set_open_note(self.open_note);
                    }
                }
                Command::MoveLeft => self.session.move_left(false),
                Command::SelectLeft => self.session.move_left(true),
                Command::MoveRight => self.session.move_right(false),
                Command::SelectRight => self.session.move_right(true),
                Command::MoveWordLeft => self.session.move_left_word(false),
                Command::SelectWordLeft => self.session.move_left_word(true),
                Command::MoveWordRight => self.session.move_right_word(false),
                Command::SelectWordRight => self.session.move_right_word(true),
                Command::MoveUp => self.move_vertically(false, false),
                Command::SelectUp => self.move_vertically(false, true),
                Command::MoveDown => self.move_vertically(true, false),
                Command::SelectDown => self.move_vertically(true, true),
                Command::MoveLineStart => self.move_to_line_edge(false, false),
                Command::SelectLineStart => self.move_to_line_edge(false, true),
                Command::MoveLineEnd => self.move_to_line_edge(true, false),
                Command::SelectLineEnd => self.move_to_line_edge(true, true),
                Command::SelectAll => self.select_all(),
                Command::SetBlockKind(kind) => {
                    let caret = self.session.valid_caret();
                    self.session.set_block_kind(caret.block_id, kind);
                }
                Command::AddAnnotation(kind) => {
                    if let Some(id) = self.session.add_annotation(kind) {
                        self.set_open_note(Some(id));
                        self.open_note_pos = None;
                    }
                }
            }
        }
    }

    /// Up/Down move by visual line, keeping the caret's horizontal position.
    /// At the first or last line of a block they cross into the adjacent block
    /// (first line down, last line up), so paragraphs are not dead ends.
    fn move_vertically(&mut self, down: bool, select: bool) {
        let caret = self.session.valid_caret();
        let Some(index) = self.session.block_index(caret.block_id) else {
            return;
        };
        let block = self.session.document().blocks[index].clone();
        let layout = self.layout_of(&block);
        let Some(line) = caret_line(&layout, caret.offset) else {
            return;
        };
        let Some(line_index) = layout
            .lines
            .iter()
            .position(|candidate| candidate.y == line.y)
        else {
            return;
        };
        let x = caret_x(&layout, caret.offset);

        let target = if down {
            layout.lines.get(line_index + 1)
        } else {
            line_index
                .checked_sub(1)
                .and_then(|previous| layout.lines.get(previous))
        };
        if let Some(target) = target {
            let offset = offset_at(&layout, x, target.y + 1.0, block.text.len());
            self.move_caret(
                Cursor {
                    block_id: block.id,
                    offset,
                },
                select,
            );
            return;
        }

        // At the block's edge: cross into the adjacent block at the same column.
        let adjacent = if down {
            self.session.document().blocks.get(index + 1).cloned()
        } else {
            index
                .checked_sub(1)
                .and_then(|i| self.session.document().blocks.get(i).cloned())
        };
        let Some(adjacent) = adjacent else {
            return;
        };
        let adj_layout = self.layout_of(&adjacent);
        let line = if down {
            adj_layout.lines.first()
        } else {
            adj_layout.lines.last()
        };
        let offset = match line {
            Some(line) => offset_at(&adj_layout, x, line.y + 1.0, adjacent.text.len()),
            None => 0,
        };
        self.move_caret(
            Cursor {
                block_id: adjacent.id,
                offset,
            },
            select,
        );
    }

    /// Moves the caret to `target`, extending the selection when `select`.
    fn move_caret(&mut self, target: Cursor, select: bool) {
        if select {
            self.session.select_to(target);
        } else {
            self.session.set_caret(target);
        }
    }

    /// C-a selects the caret's whole block; pressing it again while the block
    /// is already selected widens to the whole document.
    fn select_all(&mut self) {
        let caret = self.session.valid_caret();
        let Some(index) = self.session.block_index(caret.block_id) else {
            return;
        };
        let (block_id, block_len, first, last) = {
            let document = self.session.document();
            let block = &document.blocks[index];
            (
                block.id,
                block.text.len(),
                document.blocks.first().map(|b| Cursor {
                    block_id: b.id,
                    offset: 0,
                }),
                document.blocks.last().map(|b| Cursor {
                    block_id: b.id,
                    offset: b.text.len(),
                }),
            )
        };
        let block_selected = matches!(
            self.session.ordered_selection(),
            Some((start, end))
                if start.block_id == block_id
                    && start.offset == 0
                    && end.block_id == block_id
                    && end.offset == block_len
        );
        let (anchor, moving) = if block_selected {
            let (Some(first), Some(last)) = (first, last) else {
                return;
            };
            (last, first)
        } else {
            (
                Cursor {
                    block_id,
                    offset: block_len,
                },
                Cursor {
                    block_id,
                    offset: 0,
                },
            )
        };
        self.session.set_caret(anchor);
        self.session.select_to(moving);
    }

    fn move_to_line_edge(&mut self, end: bool, select: bool) {
        let caret = self.session.valid_caret();
        let Some(index) = self.session.block_index(caret.block_id) else {
            return;
        };
        let block = self.session.document().blocks[index].clone();
        let layout = self.layout_of(&block);
        let Some(line) = caret_line(&layout, caret.offset) else {
            return;
        };
        let offset = if end { line.end } else { line.start };
        self.move_caret(
            Cursor {
                block_id: block.id,
                offset,
            },
            select,
        );
    }

    fn show_status(&mut self, ui: &mut egui::Ui) {
        let title = self.session.display_title();
        let blocks = self.session.document().blocks.len();
        let words: usize = self
            .session
            .document()
            .blocks
            .iter()
            .map(|block| block.text.split_whitespace().count())
            .sum();
        let annotations = self.session.document().annotations.len();
        let (undo, redo) = self.session.history();
        let path = self
            .session
            .path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "not saved to a file yet".to_string());
        let not_saved = self.session.is_anonymous() && self.session.has_content();
        let error = self.session.last_error.clone();
        let notice = self.notice.clone();
        // Where the two exports would land, named on hover so the author knows
        // the file name before clicking.
        let export_base = self.session.export_base();
        let markdown_path = export_path(&export_base, "md").display().to_string();
        let html_path = export_path(&export_base, "html").display().to_string();
        // Applied after the panel closes: some of these block on a native dialog
        // or replace the session the panel is reading.
        let mut action: Option<MenuAction> = None;
        let pending_menu = self.pending_menu.take();

        egui::Panel::top("kame-status").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                let file_menu = ui.menu_button("File", |ui| {
                    ui.set_min_width(260.0);
                    if menu_item(ui, "Open…", Some("Ctrl-O"), false) {
                        action = Some(MenuAction::Open);
                    }
                    if menu_item(ui, "Save", Some("Ctrl-S"), false) {
                        action = Some(MenuAction::Save);
                    }
                    ui.separator();
                    if menu_item(ui, "Import Markdown…", None, false) {
                        action = Some(MenuAction::ImportMarkdown);
                    }
                    if menu_entry(ui, "Export Markdown", Some("Ctrl-Shift-E"), false)
                        .on_hover_text(format!("write {markdown_path}"))
                        .clicked()
                    {
                        action = Some(MenuAction::File(FileCommand::ExportMarkdown));
                    }
                    if menu_entry(ui, "Export HTML", Some("Ctrl-Shift-B"), false)
                        .on_hover_text(format!("write {html_path}"))
                        .clicked()
                    {
                        action = Some(MenuAction::File(FileCommand::ExportHtml));
                    }
                    ui.separator();
                    if menu_item(ui, "Snapshot Now…", Some("Ctrl-Shift-S"), false) {
                        action = Some(MenuAction::File(FileCommand::SnapshotNow));
                    }
                    if menu_item(ui, "Restore Snapshot…", Some("Ctrl-Shift-R"), false) {
                        action = Some(MenuAction::File(FileCommand::RestoreSnapshot));
                    }
                    ui.separator();
                    if menu_item(ui, "Document Properties…", None, false) {
                        action = Some(MenuAction::Properties);
                    }
                    ui.separator();
                    let plural = |n: usize| if n == 1 { "" } else { "s" };
                    ui.label(
                        egui::RichText::new(format!(
                            "{blocks} block{} · {annotations} annotation{} · undo {undo} · redo {redo}",
                            plural(blocks),
                            plural(annotations),
                        ))
                        .small()
                        .weak(),
                    );
                });
                let annotate_menu = ui.menu_button("Annotate", |ui| {
                    ui.set_min_width(200.0);
                    for (label, kind, shortcut) in [
                        ("Note", AnnotationKind::Note, Some("Ctrl-Shift-N")),
                        ("Marginal", AnnotationKind::Marginal, None),
                        ("Footnote", AnnotationKind::Footnote, Some("Ctrl-Shift-F")),
                    ] {
                        if menu_item(ui, label, shortcut, false) {
                            action = Some(MenuAction::Annotate(kind));
                        }
                    }
                });
                let view_menu = ui.menu_button("View", |ui| {
                    ui.set_min_width(240.0);
                    ui.horizontal(|ui| {
                        ui.label(format!("Type size {:.0}", self.body_size));
                        // These stay open, so the size can be walked to taste.
                        if ui.small_button("−").on_hover_text("smaller").clicked() {
                            action = Some(MenuAction::TypeSize(-FONT_SIZE_STEP));
                        }
                        if ui.small_button("+").on_hover_text("larger").clicked() {
                            action = Some(MenuAction::TypeSize(FONT_SIZE_STEP));
                        }
                    });
                    if menu_item(ui, "Dark mode", Some("Ctrl-Shift-D"), self.dark) {
                        action = Some(MenuAction::ToggleDark);
                    }
                    ui.separator();
                    for (label, mode, shortcut) in [
                        ("Fullscreen", ModeCommand::Fullscreen, "F11"),
                        ("Hide chrome", ModeCommand::HideChrome, "Ctrl-Shift-H"),
                        ("Hide margin", ModeCommand::HideMargin, "Ctrl-Shift-M"),
                        ("Paragraph focus", ModeCommand::ParagraphFocus, "Ctrl-Shift-P"),
                        ("Typewriter", ModeCommand::Typewriter, "Ctrl-Shift-T"),
                    ] {
                        if menu_item(ui, label, Some(shortcut), self.modes.is_on(mode)) {
                            action = Some(MenuAction::Mode(mode));
                        }
                    }
                });
                let help_menu = ui.menu_button("Help", |ui| {
                    ui.set_min_width(160.0);
                    if menu_item(ui, "Help", Some("F1"), false) {
                        action = Some(MenuAction::Help);
                    }
                });
                for (menu, response) in [
                    (BarMenu::File, &file_menu.response),
                    (BarMenu::Annotate, &annotate_menu.response),
                    (BarMenu::View, &view_menu.response),
                    (BarMenu::Help, &help_menu.response),
                ] {
                    if pending_menu == Some(menu) {
                        egui::Popup::toggle_id(ui.ctx(), egui::Popup::menu(response).get_id());
                        ui.ctx().request_repaint();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(word_count_label(words)).weak());
                    // Laid out right to left, so reversed to read in the usual order.
                    for (short, name) in mode_badges(&self.modes).into_iter().rev() {
                        ui.label(egui::RichText::new(short).small().strong())
                            .on_hover_text(name);
                    }
                    if not_saved {
                        ui.label(egui::RichText::new("not saved yet").small().weak());
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(title).strong()).truncate())
                            .on_hover_text(path.as_str());
                    });
                });
            });
            // Its own row, wrapping: an import's report can be long, and it must
            // be readable at any window width rather than clip off the row's end.
            if let Some(notice) = &notice {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(notice)
                            .small()
                            .color(self.palette.notice_ink),
                    )
                    .wrap(),
                );
            }
            if let Some(error) = error {
                // Selectable, so an author can copy a path or reason out of a
                // desktop launch where there is no terminal to scroll.
                ui.add(
                    egui::Label::new(egui::RichText::new(error).color(self.palette.error_ink))
                        .selectable(true),
                );
            }
        });
        if let Some(action) = action {
            self.run_menu_action(ui.ctx(), action);
        }
    }

    /// Carries out a menu choice through the same path its shortcut uses.
    fn run_menu_action(&mut self, ctx: &egui::Context, action: MenuAction) {
        match action {
            MenuAction::Open => self.begin_open(),
            MenuAction::Save => self.save_action(ctx, false),
            MenuAction::ImportMarkdown => self.import_markdown(),
            MenuAction::Properties => self.begin_properties(),
            MenuAction::File(command) => self.run_file_command(command),
            MenuAction::Annotate(kind) => {
                if let Some(id) = self.session.add_annotation(kind) {
                    self.set_open_note(Some(id));
                    self.open_note_pos = None;
                }
            }
            MenuAction::TypeSize(step) => self.adjust_font_size(step),
            MenuAction::ToggleDark => self.apply_dark_toggle(ctx),
            MenuAction::Mode(mode) => self.apply_mode(ctx, mode),
            MenuAction::Help => self.show_help = true,
        }
    }

    fn draw_surface(&mut self, ui: &mut egui::Ui) {
        let config = self.renderer.config();
        let focus = self.modes.paragraph_focus;
        let typewriter = self.modes.typewriter;

        // Decode image blocks first, before the layout/texture caches borrow
        // `self` for the rest of the frame, so the prose loop can draw them
        // with an immutable borrow.
        let image_paths: Vec<String> = self
            .session
            .document()
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                BlockKind::Image { path } => Some(path.clone()),
                _ => None,
            })
            .collect();
        for path in &image_paths {
            self.image_texture_handle(ui.ctx(), path);
        }

        // Shape only blocks whose content changed since the last frame; an
        // idle repaint shapes nothing, and an edit re-shapes one block.
        let marks = footnote_marks(self.session.document());
        let layouts =
            self.layouts
                .sync_marked(&mut self.renderer, &self.session.document().blocks, &marks);

        let paper: [u8; 3] = [
            self.palette.background.r(),
            self.palette.background.g(),
            self.palette.background.b(),
        ];
        let highlights = highlight_map(&self.session.document(), &self.palette);
        self.textures.sync(
            ui.ctx(),
            &mut self.renderer,
            &self.session.document().blocks,
            &layouts,
            self.palette.body_ink,
            paper,
            &highlights,
        );

        let document = self.session.document();
        let blocks = &document.blocks;
        let caret = self.session.caret;

        // List markers: a bullet for unordered items, a 1-based number for a
        // run of ordered items. The counter restarts when a run is interrupted.
        let mut ordered = 0usize;
        let list_markers: Vec<Option<String>> = blocks
            .iter()
            .map(|block| match block.kind {
                BlockKind::ListItem { ordered: true } => {
                    ordered += 1;
                    Some(format!("{ordered}."))
                }
                BlockKind::ListItem { ordered: false } => {
                    ordered = 0;
                    Some("•".to_string())
                }
                _ => {
                    ordered = 0;
                    None
                }
            })
            .collect();

        // Footnote numbering is reading order, not creation order: the stored
        // annotation vector records when a note was made, which is unrelated
        // to where it sits in the prose.
        let annotations = reading_order(&document.annotations, blocks);

        let heights: Vec<f32> = blocks
            .iter()
            .zip(layouts.iter())
            .map(|(block, layout)| block_height(block, layout, &config))
            .collect();
        let gap_below = |index: usize| {
            blocks
                .get(index + 1)
                .map_or(0.0, |next| gap_after(&blocks[index].kind, &next.kind))
        };
        let body_bottom = PAGE_PADDING
            + heights.iter().sum::<f32>()
            + (0..blocks.len()).map(gap_below).sum::<f32>();

        // Footnote entries decide how tall the page must be, and a wrapped
        // entry must push the next one past all of its lines. Shape them before
        // the page is sized (they span the measure, so they need no geometry)
        // and stack by their rendered height. The Y values are page-local
        // (body_bottom is measured from the painter's top), so the painter
        // origin is added when drawing.
        let annotation_size = config.body_size * ANNOTATION_SCALE;
        let footnote_number: HashMap<AnnotationId, usize> = annotations
            .iter()
            .filter(|annotation| annotation.kind == AnnotationKind::Footnote)
            .enumerate()
            .map(|(index, annotation)| (annotation.id, index + 1))
            .collect();
        for annotation in &annotations {
            if annotation.kind == AnnotationKind::Footnote {
                let number = footnote_number.get(&annotation.id).copied().unwrap_or(1);
                let entry = format!("{number}. {}", annotation.body);
                self.annotation_textures.sync(
                    ui.ctx(),
                    &mut self.renderer,
                    annotation.id,
                    &entry,
                    config.body_width,
                    annotation_size,
                    self.palette.apparatus_ink,
                    paper,
                );
            }
        }
        let footnote_heights: Vec<(AnnotationId, f32)> = annotations
            .iter()
            .filter(|annotation| annotation.kind == AnnotationKind::Footnote)
            .filter_map(|annotation| {
                self.annotation_textures
                    .body_height(annotation.id)
                    .map(|height| (annotation.id, height))
            })
            .collect();
        let footnotes = place_footnotes(&annotations, body_bottom, &footnote_heights);
        let footnote_bottom = footnotes
            .last()
            .map(|placement| {
                footnote_heights
                    .iter()
                    .find(|(id, _)| *id == placement.annotation_id)
                    .map(|(_, height)| placement.y + height)
                    .unwrap_or(placement.y + FOOTNOTE_LINE_HEIGHT)
            })
            .unwrap_or(body_bottom);

        let content_height = (footnote_bottom + PAGE_PADDING).max(ui.available_height());

        let (response, painter) = ui.allocate_painter(
            Vec2::new(ui.available_width(), content_height),
            Sense::click_and_drag(),
        );
        let geometry = page_geometry(config, response.rect.width());
        // A zero gutter means the margin column has no room (hidden, or too
        // narrow to sit beside a readable measure): marginalia cannot be set.
        let show_margin = geometry.margin_width > 0.0;
        let body_left = response.rect.left() + geometry.body_left;

        let mut tops = Vec::with_capacity(blocks.len());
        let mut y = response.rect.top() + PAGE_PADDING;
        for (index, height) in heights.iter().enumerate() {
            tops.push(y);
            y += height + gap_below(index);
        }

        // --- annotation text -------------------------------------------------
        // Margin bodies sit in the gutter right of the measure. Footnotes were
        // shaped before the page was sized (they span the measure, so they
        // needed no geometry); this pass only shapes the margin column.
        let margin_column = (geometry.margin_width - config.margin_size).max(40.0);
        let mut live = HashSet::with_capacity(annotations.len());
        for annotation in &annotations {
            live.insert(annotation.id);
            match annotation.kind {
                // The margin column is gone in hide-margin mode, so marginal
                // bodies have nowhere to be set; skipping their textures also
                // lets the cache drop them until the column returns.
                AnnotationKind::Marginal if show_margin => self.annotation_textures.sync(
                    ui.ctx(),
                    &mut self.renderer,
                    annotation.id,
                    &annotation.body,
                    margin_column,
                    annotation_size,
                    self.palette.apparatus_ink,
                    paper,
                ),
                // Footnotes were shaped above; a Note keeps its body behind the
                // margin marker, so it has no texture until the popover opens
                // (which uses egui text); a marginal in hide-margin mode is
                // skipped above.
                AnnotationKind::Footnote | AnnotationKind::Marginal | AnnotationKind::Note => {}
            }
        }
        self.annotation_textures.retain(&live);

        // --- anchored ranges -------------------------------------------------
        // A wash marks what every annotation is attached to. Drawn *before*
        // the glyphs so it colours the paper behind the text and the ink stays
        // dark — a highlight must not dull the words it marks.
        for annotation in &annotations {
            // Block-scoped anchors (marginalia) have no body range to wash.
            let Some((start, end)) = annotation.anchor.byte_range() else {
                continue;
            };
            let Some(index) = blocks
                .iter()
                .position(|block| block.id == annotation.anchor.block_id())
            else {
                continue;
            };
            let color = match annotation.kind {
                AnnotationKind::Marginal => self.palette.marginal_highlight,
                AnnotationKind::Note => self.palette.note_highlight,
                AnnotationKind::Footnote => self.palette.footnote_highlight,
            };
            for rect in range_rects(layouts[index], start, end) {
                painter.rect_filled(
                    rect.translate(Vec2::new(
                        body_left + layouts[index].left_indent,
                        tops[index],
                    )),
                    1.0,
                    color,
                );
            }
        }

        // --- prose -----------------------------------------------------------
        // Paragraph-focus mode veils every block but the caret's: the veil is
        // paper at partial alpha, so dimmed prose fades and the margins stay
        // paper-coloured.
        let caret_index = blocks.iter().position(|block| block.id == caret.block_id);
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        for (index, block) in blocks.iter().enumerate() {
            if let BlockKind::Image { path } = &block.kind {
                // An image spans the measure at body width; its height keeps
                // the source's aspect ratio (already baked into `tops`).
                let height = image_height(path, config.body_width);
                let rect = Rect::from_min_size(
                    Pos2::new(body_left, tops[index]),
                    Vec2::new(config.body_width, height),
                );
                if let Some(texture) = self.image_textures.get(path) {
                    painter.image(texture.id(), rect, uv, Color32::WHITE);
                } else {
                    painter.rect_stroke(
                        rect,
                        2.0,
                        egui::Stroke::new(1.0, self.palette.rule_ink),
                        egui::StrokeKind::Inside,
                    );
                    let galley = painter.layout(
                        format!("Missing image: {path}"),
                        FontId::proportional(config.body_size * 0.8),
                        self.palette.list_marker_ink,
                        (rect.width() - 24.0).max(40.0),
                    );
                    let at = rect.center() - galley.size() * 0.5;
                    painter.galley(at, galley, self.palette.list_marker_ink);
                }
                if let Some(veil) = dim_veil(self.palette.dim_veil, focus, index, caret_index) {
                    painter.rect_filled(rect, 0.0, veil);
                }
                continue;
            }
            let indent = layouts[index].left_indent;
            // A list item's marker sits in the gutter before its text, its
            // right edge held a fixed gap short of the text so numbers align.
            if let Some(marker) = &list_markers[index] {
                painter.text(
                    Pos2::new(
                        body_left + indent - LIST_MARKER_GAP,
                        tops[index] + config.body_line_height * 0.5,
                    ),
                    Align2::RIGHT_CENTER,
                    marker,
                    FontId::proportional(config.body_size),
                    self.palette.list_marker_ink,
                );
            }
            if let Some(texture) = self.textures.get(block.id) {
                let origin = Pos2::new(body_left + indent, tops[index]);
                let rect = texture.rect(origin);
                painter.image(texture.texture.id(), rect, uv, Color32::WHITE);
                if let Some(veil) = dim_veil(self.palette.dim_veil, focus, index, caret_index) {
                    painter.rect_filled(rect, 0.0, veil);
                }
            }
        }

        // --- selection -------------------------------------------------------
        if let Some((start, end)) = self.session.ordered_selection() {
            let start_index = blocks.iter().position(|block| block.id == start.block_id);
            let end_index = blocks.iter().position(|block| block.id == end.block_id);
            if let (Some(start_index), Some(end_index)) = (start_index, end_index) {
                for index in start_index..=end_index {
                    let lo = if index == start_index {
                        start.offset
                    } else {
                        0
                    };
                    let hi = if index == end_index {
                        end.offset
                    } else {
                        blocks[index].text.len()
                    };
                    if lo >= hi {
                        continue;
                    }
                    for rect in range_rects(layouts[index], lo, hi) {
                        painter.rect_filled(
                            rect.translate(Vec2::new(
                                body_left + layouts[index].left_indent,
                                tops[index],
                            )),
                            0.0,
                            self.palette.selection_fill,
                        );
                    }
                }
            }
        }

        // --- caret -----------------------------------------------------------
        let mut caret_rect: Option<Rect> = None;
        if let Some(index) = caret_index {
            let (line_y, line_height) = caret_line(layouts[index], caret.offset)
                .map(|line| (line.y, line.height))
                .unwrap_or((0.0, config.body_line_height));
            let x = caret_x(layouts[index], caret.offset);
            let top = tops[index] + line_y;
            let rect = Rect::from_min_size(
                Pos2::new(body_left + layouts[index].left_indent + x, top),
                Vec2::new(1.5, line_height),
            );
            painter.rect_filled(rect, 0.0, self.palette.caret_ink);
            caret_rect = Some(rect);
        }

        // --- reveal the caret (find navigation) ------------------------------
        // A find jump may land outside the viewport; the page scrolls to the
        // caret next frame, exactly as typewriter centring does.
        if self.reveal_caret {
            if let Some(rect) = caret_rect {
                self.pending_scroll = Some(typewriter_scroll_offset(
                    rect.top() - response.rect.top(),
                    ui.clip_rect().height(),
                    content_height,
                ));
            }
            self.reveal_caret = false;
        }

        // --- typewriter centring ---------------------------------------------
        // The offset is computed here, where the caret's page position and the
        // content height are known, and applied by the scroll area next frame.
        // Only a moved caret recentres: pinning it every frame would make the
        // page unscrollable while the reader is idle.
        if typewriter {
            if let Some(rect) = caret_rect {
                let anchor = (caret.block_id, caret.offset);
                if self.typewriter_anchor != Some(anchor) {
                    self.typewriter_anchor = Some(anchor);
                    self.pending_scroll = Some(typewriter_scroll_offset(
                        rect.top() - response.rect.top(),
                        ui.clip_rect().height(),
                        content_height,
                    ));
                }
            }
        } else {
            self.typewriter_anchor = None;
        }

        // --- marginalia and note markers -------------------------------------
        // `place_marginalia` stacks on page Y, so it is given each block's page
        // top and returns Y already composed with it.
        //
        // Stacking needs the height each margin body actually renders to, not a
        // one-line guess: a body that wraps to six lines must push the next
        // placement past all six, or the notes overprint.
        let body_heights: Vec<(AnnotationId, f32)> = annotations
            .iter()
            .filter(|annotation| annotation.kind == AnnotationKind::Marginal)
            .filter_map(|annotation| {
                self.annotation_textures
                    .body_height(annotation.id)
                    .map(|height| (annotation.id, height))
            })
            .collect();
        let margins = if !show_margin {
            // No gutter to set notes in; their anchored ranges still show, so
            // the prose keeps its marks without the apparatus column.
            Vec::new()
        } else {
            place_marginalia(
                &annotations,
                blocks,
                &layouts,
                &body_heights,
                &tops,
                &config,
            )
        };
        let by_id: HashMap<AnnotationId, &Annotation> = annotations
            .iter()
            .map(|annotation| (annotation.id, annotation))
            .collect();
        let mut markers: Vec<(AnnotationId, Rect)> = Vec::new();
        for placement in &margins {
            let Some(annotation) = by_id.get(&placement.annotation_id) else {
                continue;
            };
            let origin = margin_origin(body_left, placement);
            match annotation.kind {
                AnnotationKind::Marginal => {
                    // A leader ties the note to its block: a thin line runs
                    // from the measure's right edge at the anchor, then drops
                    // to the note's stacked position, so a long note that
                    // pushes the next one down still shows which block each
                    // belongs to.
                    let anchor_y = placement.anchor_y;
                    let margin_x = origin.x - 7.0; // align with the rule below
                    let body_right = body_left + config.body_width;
                    if placement.y > anchor_y + 0.5 {
                        painter.line_segment(
                            [
                                Pos2::new(margin_x, anchor_y),
                                Pos2::new(margin_x, placement.y),
                            ],
                            egui::Stroke::new(1.0, self.palette.rule_ink),
                        );
                    }
                    painter.line_segment(
                        [
                            Pos2::new(body_right, anchor_y),
                            Pos2::new(margin_x, anchor_y),
                        ],
                        egui::Stroke::new(1.0, self.palette.rule_ink),
                    );
                    // A thin rule opens the margin block, the way a printed
                    // marginal note is set off from the measure.
                    painter.rect_filled(
                        Rect::from_min_size(
                            Pos2::new(origin.x - 7.0, origin.y + 2.0),
                            Vec2::new(1.0, 12.0),
                        ),
                        0.0,
                        self.palette.rule_ink,
                    );
                    if let Some(texture) = self.annotation_textures.get(annotation.id) {
                        let rect = texture.rect(origin);
                        painter.image(texture.texture.id(), rect, uv, Color32::WHITE);
                        // The body is the hit target, so a marginal can be
                        // reopened and revised once its popover has closed.
                        markers.push((annotation.id, rect));
                    }
                }
                AnnotationKind::Note => {
                    let center =
                        Pos2::new(origin.x + 4.0, origin.y + config.body_line_height * 0.5);
                    let open = self.open_note == Some(annotation.id);
                    // A note with no body yet is a bare highlight: a hollow ring
                    // rather than a filled dot, so "something written" reads at
                    // a glance and the marker stays a click target either way.
                    if !annotation.body.trim().is_empty() {
                        painter.circle_filled(center, 3.5, self.palette.note_mark);
                    }
                    painter.circle_stroke(
                        center,
                        if open { 7.5 } else { 6.0 },
                        egui::Stroke::new(1.0, self.palette.note_mark.gamma_multiply(0.65)),
                    );
                    markers.push((
                        annotation.id,
                        Rect::from_center_size(center, Vec2::splat(18.0)),
                    ));
                }
                AnnotationKind::Footnote => {}
            }
        }

        // --- footnotes -------------------------------------------------------
        // A short rule, then numbered entries stacked below the body. The
        // entry texture carries "N. body", so the number and its text are one
        // rasterized run.
        if let Some(first) = footnotes.first() {
            let rule_y = response.rect.top() + first.y - FOOTNOTE_GAP * 0.5;
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(body_left, rule_y),
                    Pos2::new(body_left + config.body_width * 0.34, rule_y + 1.0),
                ),
                0.0,
                self.palette.rule_ink,
            );
        }
        for placement in &footnotes {
            if let Some(texture) = self.annotation_textures.get(placement.annotation_id) {
                let origin = footnote_origin(response.rect.top(), body_left, placement);
                let rect = texture.rect(origin);
                painter.image(texture.texture.id(), rect, uv, Color32::WHITE);
                // The foot entry is the hit target for reopening its popover.
                markers.push((placement.annotation_id, rect));
            }
        }

        // Superscript number at each footnote's anchor. Drawn after the range's
        // last glyph (see `superscript_origin`) so it sits beside the anchored
        // word rather than overprinting its ink.
        for placement in &footnotes {
            let Some(annotation) = by_id.get(&placement.annotation_id) else {
                continue;
            };
            let Some(index) = blocks
                .iter()
                .position(|block| block.id == annotation.anchor.block_id())
            else {
                continue;
            };
            let Some(origin) = superscript_origin(
                &layouts[index],
                &annotation.anchor,
                body_left,
                tops[index],
                config.body_width,
                FOOTNOTE_MARK_EM * config.body_size,
            ) else {
                continue;
            };
            painter.text(
                origin,
                Align2::LEFT_TOP,
                placement.number.to_string(),
                FontId::proportional(config.body_size * 0.62),
                self.palette.footnote_mark,
            );
        }

        // --- pointer ---------------------------------------------------------
        let shift = ui.input(|input| input.modifiers.shift);
        let mut caret_action: Option<(Cursor, bool)> = None;
        let mut marker_click: Option<(AnnotationId, Rect)> = None;
        let pointer = response.interact_pointer_pos();
        if response.drag_started() {
            if let Some(pos) = pointer {
                if let Some(cursor) = hit_test(blocks, &layouts, &tops, &heights, body_left, pos) {
                    caret_action = Some((cursor, shift));
                }
            }
        } else if response.dragged() {
            if let Some(pos) = pointer {
                if let Some(cursor) = hit_test(blocks, &layouts, &tops, &heights, body_left, pos) {
                    caret_action = Some((cursor, true));
                }
            }
        }
        if response.clicked() {
            if let Some(pos) = pointer {
                if let Some(hit) = markers.iter().find(|(_, rect)| rect.contains(pos)).copied() {
                    marker_click = Some(hit);
                } else if let Some(cursor) =
                    hit_test(blocks, &layouts, &tops, &heights, body_left, pos)
                {
                    // A click on a link opens it instead of moving the caret.
                    if let Some(url) = url_at(blocks, &cursor) {
                        open_in_browser(&url);
                    } else {
                        caret_action = Some((cursor, shift));
                    }
                }
            }
        }

        // `blocks` borrows the document; every caret move is applied after the
        // borrow's last use.
        if let Some((id, rect)) = marker_click {
            let open = if self.open_note == Some(id) {
                None
            } else {
                Some(id)
            };
            self.set_open_note(open);
            self.open_note_pos = Some(rect.right_top() + Vec2::new(10.0, -6.0));
        } else if let Some((cursor, extend)) = caret_action {
            if extend {
                self.session.select_to(cursor);
            } else {
                self.session.set_caret(cursor);
            }
            self.set_open_note(None);
        }

        // --- context menu -----------------------------------------------------
        // Block formatting and annotation actions live here, on the page's
        // right-click, rather than as rows of buttons in the status bar.
        response.context_menu(|ui| {
            ui.set_min_width(180.0);
            for (label, kind) in [
                ("Body", BlockKind::Paragraph),
                ("Heading 1", BlockKind::Heading { level: 1 }),
                ("Heading 2", BlockKind::Heading { level: 2 }),
                ("Heading 3", BlockKind::Heading { level: 3 }),
                ("Bulleted list", BlockKind::ListItem { ordered: false }),
                ("Numbered list", BlockKind::ListItem { ordered: true }),
                ("Block quote", BlockKind::BlockQuote),
            ] {
                if ui.button(label).clicked() {
                    self.set_block_kind(kind);
                    ui.close();
                }
            }
            ui.separator();
            for (label, kind) in [
                ("Note", AnnotationKind::Note),
                ("Marginal", AnnotationKind::Marginal),
                ("Footnote", AnnotationKind::Footnote),
            ] {
                if ui.button(label).clicked() {
                    if let Some(id) = self.session.add_annotation(kind) {
                        self.set_open_note(Some(id));
                        self.open_note_pos = None;
                    }
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Insert image…").clicked() {
                self.image_prompt = Some(ImagePrompt {
                    path: String::new(),
                    focus: true,
                });
                ui.close();
            }
        });

        self.show_note_popover(ui.ctx());
    }

    /// Draws the open note in a floating popover next to its marker, with its
    /// body as an editable field.
    ///
    /// The field is seeded from the note when the popover opens and committed
    /// back through `Session::set_annotation_body` on every change, so the prose
    /// painted in the margin or at the foot updates as it is typed and each
    /// commit lands in the op log as one undoable step.
    fn show_note_popover(&mut self, ctx: &egui::Context) {
        let Some(id) = self.open_note else {
            return;
        };
        let Some(annotation) = self
            .session
            .document()
            .annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .cloned()
        else {
            self.set_open_note(None);
            return;
        };

        let screen = ctx.content_rect();
        let anchor = self
            .open_note_pos
            .unwrap_or_else(|| Pos2::new(screen.left() + 48.0, screen.top() + 140.0));
        let pos = Pos2::new(
            anchor.x.clamp(
                screen.left() + 8.0,
                (screen.right() - 280.0).max(screen.left() + 8.0),
            ),
            anchor.y.clamp(
                screen.top() + 8.0,
                (screen.bottom() - 120.0).max(screen.top() + 8.0),
            ),
        );

        // The field edits a local copy so the closure borrows one buffer rather
        // than `self`; the copy is committed and stored back once the area closes.
        let mut buffer = self.open_note_body.clone();
        let focus = self.open_note_focus;
        let mut changed = false;
        let mut close = false;
        let mut remove = false;
        egui::Area::new(Id::unique("kame-note-popover"))
            .order(Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(260.0);
                    ui.label(
                        egui::RichText::new(annotation_kind_label(&annotation.kind))
                            .small()
                            .strong(),
                    );
                    let field = ui.add(
                        egui::TextEdit::multiline(&mut buffer)
                            .id(note_body_edit_id())
                            .desired_width(240.0)
                            .desired_rows(3)
                            .hint_text("Write the note…"),
                    );
                    changed = field.changed();
                    if focus {
                        field.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("Remove")
                            .on_hover_text("remove this note and its highlight")
                            .clicked()
                        {
                            remove = true;
                        }
                        if ui
                            .small_button("Close")
                            .on_hover_text("close (Ctrl/Cmd-Enter)")
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
            });

        self.open_note_focus = false;
        if remove {
            self.session.remove_annotation(id);
            self.set_open_note(None);
            return;
        }
        // Ctrl/Cmd-Enter closes the popover, like the Close button. The
        // multiline field may have already appended a newline for that key, so
        // trailing newlines are dropped before committing.
        let command_enter = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
        if command_enter {
            while buffer.ends_with('\n') {
                buffer.pop();
            }
            close = true;
        }
        if changed || command_enter {
            self.open_note_body = buffer;
            self.session.set_annotation_body(id, &self.open_note_body);
        }
        if close {
            self.set_open_note(None);
        }
    }

    // --- file dialogs -------------------------------------------------------

    /// Draws whichever file dialog is open.
    ///
    /// Called from the app root rather than from the page so hide-chrome cannot
    /// hide a dialog that its own chord can still open.
    fn show_dialogs(&mut self, ctx: &egui::Context) {
        // Enter is the primary action of both dialogs. It is taken from the
        // event stream with repeats filtered out, so holding it down steps
        // once: the restore confirmation must never be skipped over.
        let enter = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        pressed: true,
                        repeat: false,
                        ..
                    }
                )
            })
        });
        if self.snapshot_prompt.is_some() {
            self.show_snapshot_prompt(ctx, enter);
        } else if self.metadata_prompt.is_some() {
            self.show_metadata_prompt(ctx, enter);
        } else if self.open_prompt.is_some() {
            self.show_open_prompt(ctx, enter);
        } else if self.image_prompt.is_some() {
            self.show_image_prompt(ctx, enter);
        } else if self.link_prompt.is_some() {
            self.show_link_prompt(ctx, enter);
        } else if self.restore_picker.is_some() {
            self.show_restore_picker(ctx, enter);
        }
        if self.show_help {
            self.show_help_dialog(ctx);
        }
    }

    /// The usage and keybinding reference, in a modal window. This is where the
    /// "how to use it" text lives; the status bar stays to counts and toggles.
    fn show_help_dialog(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut close_clicked = false;
        egui::Window::new("kame — help")
            .id(Id::unique("kame-help"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("kame-help-grid")
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for (keys, what) in [
                            ("F1", "this help"),
                            (
                                "type / click / drag",
                                "type to write · click to place the caret · drag to select",
                            ),
                            ("Backspace / Delete", "delete a character, or the selection"),
                            ("Ctrl-Z / Ctrl-Shift-Z", "undo / redo"),
                            (
                                "Ctrl-Left / Right",
                                "move by word · add Shift to select words",
                            ),
                            (
                                "Enter",
                                "split a block; on an empty list item, end the list",
                            ),
                            ("Ctrl-F", "find"),
                            ("Ctrl-Shift-N", "note (highlight) on the selection or word"),
                            ("Ctrl-Shift-F", "footnote on the selection or word"),
                            ("Ctrl-Shift-1/2/3", "heading level 1 / 2 / 3"),
                            ("Ctrl-Shift-0", "back to body text"),
                            ("#  ##  ###", "heading, typed at the start of a line"),
                            ("-  *  1.", "list item, typed at the start of a line"),
                            (">", "block quote, typed at the start of a line"),
                            ("*x*  **x**", "italic / bold emphasis, typed around a word"),
                            ("[text](url)", "a hyperlink, typed around the anchor text"),
                            ("Ctrl-K", "add or edit a link on the selection"),
                            ("Ctrl +  Ctrl -", "larger / smaller type"),
                            ("Ctrl-Shift-D", "dark mode"),
                            ("F11", "fullscreen"),
                            (
                                "Ctrl-Shift-H/M/P/T",
                                "hide chrome / margin / focus / typewriter",
                            ),
                            ("Ctrl-Shift-E", "export Markdown"),
                            ("Ctrl-Shift-B", "export HTML"),
                            ("Ctrl-S", "save (prompts for a file if unsaved)"),
                            ("Ctrl-Shift-S", "snapshot now"),
                            ("Ctrl-Shift-R", "restore a snapshot"),
                            ("Ctrl-O", "open a document, replacing this one"),
                        ] {
                            ui.monospace(keys);
                            ui.label(what);
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                if ui.button("Close").clicked() {
                    close_clicked = true;
                }
            });
        self.show_help = open && !close_clicked;
    }

    /// The "Snapshot Now" label prompt: a text field, a button, and Enter.
    fn show_snapshot_prompt(&mut self, ctx: &egui::Context, enter: bool) {
        let Some(prompt) = &self.snapshot_prompt else {
            return;
        };
        let mut buffer = prompt.label.clone();
        let focus = prompt.focus;
        // A chord and an Enter arriving in the same frame would otherwise take
        // a snapshot of a label nobody has typed yet.
        let mut submit = enter && !focus;
        let mut cancel = false;

        egui::Window::new("Snapshot Now")
            .id(Id::unique("kame-snapshot-prompt"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.label("Name this snapshot (optional):");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut buffer)
                        .id(snapshot_label_edit_id())
                        .desired_width(260.0)
                        .hint_text("e.g. before the rewrite"),
                );
                if focus {
                    field.request_focus();
                }
                ui.horizontal(|ui| {
                    if ui.button("Take snapshot").clicked() {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if let Some(prompt) = self.snapshot_prompt.as_mut() {
            prompt.label = buffer;
            prompt.focus = false;
        }
        if cancel {
            self.dismiss_dialog();
            return;
        }
        if !submit {
            return;
        }

        let label = self
            .snapshot_prompt
            .as_ref()
            .map(|prompt| prompt.label.trim().to_string())
            .unwrap_or_default();
        // Closing the prompt before the write keeps the notice truthful: a
        // failed snapshot leaves its reason in `last_error`, not a stale dialog.
        self.snapshot_prompt = None;
        self.notice = self.session.snapshot(Some(&label)).map(|id| {
            if label.is_empty() {
                format!("snapshot #{id} taken")
            } else {
                format!("snapshot #{id} \"{label}\" taken")
            }
        });
    }

    /// The "Open" path prompt (Ctrl-O): type a `.kame` path, then open it in
    /// place of the current document. A failed save first asks for a discard
    /// confirmation, because opening a new document drops those edits.
    fn show_open_prompt(&mut self, ctx: &egui::Context, enter: bool) {
        let Some(prompt) = &self.open_prompt else {
            return;
        };
        let mut buffer = prompt.path.clone();
        let focus = prompt.focus;
        let confirming = prompt.confirming;
        let unsaved = self.session.would_lose_work();
        // A chord and an Enter arriving in the same frame would otherwise open
        // a path nobody has finished typing.
        let mut submit = enter && !focus;
        let mut cancel = false;
        let mut open = false;

        egui::Window::new("Open")
            .id(Id::unique("kame-open-prompt"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                if confirming {
                    ui.label("The current document has unsaved changes.");
                    ui.label("Open a new document and discard them?");
                } else {
                    ui.label("Path of the document to open:");
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut buffer)
                            .id(Id::unique("kame-open-path"))
                            .desired_width(360.0)
                            .hint_text("~/data/writing/essay.kame"),
                    );
                    if focus {
                        field.request_focus();
                    }
                }
                ui.horizontal(|ui| {
                    if ui
                        .button(if confirming { "Open anyway" } else { "Open" })
                        .clicked()
                    {
                        if confirming {
                            open = true;
                        } else {
                            submit = true;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if let Some(prompt) = self.open_prompt.as_mut() {
            prompt.path = buffer;
            prompt.focus = false;
        }
        if cancel {
            self.open_prompt = None;
            return;
        }
        if open {
            // The author confirmed discarding; fall through to opening the
            // path they already entered.
            let path = self
                .open_prompt
                .as_ref()
                .map(|prompt| prompt.path.trim().to_string())
                .unwrap_or_default();
            self.open_prompt = None;
            self.open_document(path);
            return;
        }
        if !submit {
            return;
        }
        if unsaved && !confirming {
            // Hold the path and ask before discarding the failed edits.
            if let Some(prompt) = self.open_prompt.as_mut() {
                prompt.confirming = true;
            }
            return;
        }
        let path = self
            .open_prompt
            .as_ref()
            .map(|prompt| prompt.path.trim().to_string())
            .unwrap_or_default();
        self.open_prompt = None;
        self.open_document(path);
    }

    /// Shapes `block` as the page does, footnote room included, so caret movement
    /// and clicks agree with what is painted.
    fn layout_of(&mut self, block: &Block) -> BlockLayout {
        let marks = footnote_marks(self.session.document());
        let block_marks = marks.get(&block.id).map(Vec::as_slice).unwrap_or(&[]);
        self.renderer.layout_marked(block, block_marks)
    }

    /// Opens the Document Properties dialog on the current metadata.
    fn begin_properties(&mut self) {
        let metadata = &self.session.metadata;
        self.metadata_prompt = Some(MetadataPrompt {
            title: metadata.title.clone(),
            author: metadata.author.clone(),
            created: metadata.created.clone(),
            published: metadata.published.clone(),
            tags: metadata.tags.join(", "),
            focus: true,
        });
    }

    /// The Document Properties dialog. Saving writes straight to the document's
    /// file; there is nothing to undo.
    fn show_metadata_prompt(&mut self, ctx: &egui::Context, enter: bool) {
        let Some(prompt) = self.metadata_prompt.as_mut() else {
            return;
        };
        let focus = prompt.focus;
        prompt.focus = false;
        // A chord and an Enter arriving in the same frame would otherwise save a
        // dialog nobody has read.
        let mut save = enter && !focus;
        let mut cancel = false;
        egui::Window::new("Document Properties")
            .id(Id::unique("kame-metadata-prompt"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                egui::Grid::new("kame-metadata-fields")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        for (index, (label, value, hint)) in [
                            ("Title", &mut prompt.title, "the file's name"),
                            ("Author", &mut prompt.author, ""),
                            ("Created", &mut prompt.created, "YYYY-MM-DD"),
                            ("Published", &mut prompt.published, "YYYY-MM-DD"),
                            ("Tags", &mut prompt.tags, "separated by commas"),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            ui.label(label);
                            let field = ui.add(
                                egui::TextEdit::singleline(value)
                                    .desired_width(320.0)
                                    .hint_text(hint),
                            );
                            if focus && index == 0 {
                                field.request_focus();
                            }
                            ui.end_row();
                        }
                    });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            self.metadata_prompt = None;
        } else if save {
            if let Some(prompt) = self.metadata_prompt.take() {
                self.session.set_metadata(Metadata {
                    title: prompt.title.trim().to_string(),
                    author: prompt.author.trim().to_string(),
                    created: prompt.created.trim().to_string(),
                    published: prompt.published.trim().to_string(),
                    tags: parse_tags(&prompt.tags),
                });
            }
        }
    }

    /// Opens the path prompt, clearing any other file dialog so it is the one
    /// thing on screen.
    fn begin_open(&mut self) {
        self.snapshot_prompt = None;
        self.restore_picker = None;
        self.open_prompt = Some(OpenPrompt {
            path: String::new(),
            focus: true,
            confirming: false,
        });
    }

    /// File > Import Markdown: asks for a file, then opens it the way the
    /// Open prompt does, holding at the same confirmation if work would be lost.
    fn import_markdown(&mut self) {
        let Some(path) = pick_markdown_path() else {
            return;
        };
        let path = path.display().to_string();
        if self.session.would_lose_work() {
            self.open_prompt = Some(OpenPrompt {
                path,
                focus: false,
                confirming: true,
            });
        } else {
            self.open_document(path);
        }
    }

    /// Replaces the current session with the document at `path`, resetting the
    /// view state that pointed into the old document (open note, find bar,
    /// typewriter anchor, caches). A failed open keeps the current document and
    /// surfaces the reason through `Session::last_error`.
    fn open_document(&mut self, typed: String) {
        if typed.is_empty() {
            self.notice = None;
            self.session.last_error = Some("no path given".to_string());
            return;
        }
        let base = self
            .session
            .path()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let path = resolve_open_path(&typed, &base);
        match Session::open_any(&path) {
            Ok((session, note)) => {
                self.session = session;
                self.layouts = LayoutCache::new();
                self.textures = TextureCache::new();
                self.annotation_textures = AnnotationTextureCache::new();
                self.open_note = None;
                self.open_note_pos = None;
                self.open_note_body = String::new();
                self.open_note_focus = false;
                self.find = None;
                self.typewriter_anchor = None;
                self.pending_scroll = None;
                self.reveal_caret = false;
                self.image_textures.clear();
                self.notice = Some(note.unwrap_or_else(|| format!("opened {}", path.display())));
            }
            Err(err) => {
                self.notice = None;
                self.session.last_error =
                    Some(format!("could not open {} — {err}", path.display()));
            }
        }
    }

    /// Returns the decoded GPU texture for the image at `path`, loading and
    /// caching it on first use. A path that cannot be decoded returns `None`,
    /// and the block simply draws nothing (its height is still reserved).
    fn image_texture_handle(&mut self, ctx: &egui::Context, path: &str) -> Option<TextureHandle> {
        if let Some(texture) = self.image_textures.get(path) {
            return Some(texture.clone());
        }
        let decoded = image::open(path).ok()?.to_rgba8();
        let size = [decoded.width() as usize, decoded.height() as usize];
        let color = egui::ColorImage::from_rgba_unmultiplied(size, decoded.as_raw());
        let texture = ctx.load_texture(path, color, egui::TextureOptions::LINEAR);
        self.image_textures
            .insert(path.to_string(), texture.clone());
        Some(texture)
    }

    /// The Save action (Ctrl-S, or "Save…" in the close prompt). A document
    /// already on disk is saved continuously, so it is a no-op; an anonymous
    /// document opens the native save-as browser and, on success, is persisted
    /// to the chosen file. `close_after` closes the window once the save lands.
    fn save_action(&mut self, ctx: &egui::Context, close_after: bool) {
        if !self.session.is_anonymous() {
            self.notice = Some("saved".to_string());
            return;
        }
        let Some(path) = pick_save_path(&self.session.suggested_file_name()) else {
            // The author cancelled the save; the close prompt stays open so
            // they can still discard or cancel explicitly.
            return;
        };
        match self.session.save_as(&path) {
            Ok(()) => {
                self.notice = Some(format!("saved {}", path.display()));
                if close_after {
                    self.close_flow.confirm();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Err(err) => {
                self.session.last_error =
                    Some(format!("could not save {} — {err}", path.display()));
            }
        }
    }

    /// The "save this anonymous document before closing?" prompt: Save… opens
    /// the native save-as browser, Don't Save closes without saving, and Cancel
    /// keeps editing.
    fn show_close_prompt(&mut self, ctx: &egui::Context) {
        if !self.close_flow.prompting {
            return;
        }
        let mut save = false;
        let mut discard = false;
        let mut cancel = false;
        egui::Window::new("Unsaved document")
            .id(Id::unique("kame-close-prompt"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.label("This document has not been saved to a file.");
                ui.horizontal(|ui| {
                    if ui.button("Save…").clicked() {
                        save = true;
                    }
                    if ui.button("Don't Save").clicked() {
                        discard = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            self.close_flow.cancel();
            return;
        }
        if discard {
            self.close_flow.confirm();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if save {
            self.save_action(ctx, true);
        }
    }

    /// Opens the link prompt on the current selection, or the link under the
    /// caret when nothing is selected — the mechanism for both adding a link
    /// and editing (or removing) one.
    fn begin_link(&mut self) {
        let Some((block_id, start, end, current)) = self.session.link_target() else {
            self.notice = Some("select text to link, or place the caret inside a link".into());
            return;
        };
        self.snapshot_prompt = None;
        self.open_prompt = None;
        self.image_prompt = None;
        self.restore_picker = None;
        self.link_prompt = Some(LinkPrompt {
            block_id,
            start,
            end,
            url: current.unwrap_or_default(),
            focus: true,
        });
    }

    /// The "Add link" URL prompt: type the URL, then set it on the target range.
    /// An empty URL removes the link.
    fn show_link_prompt(&mut self, ctx: &egui::Context, enter: bool) {
        let Some(prompt) = &self.link_prompt else {
            return;
        };
        let mut buffer = prompt.url.clone();
        let focus = prompt.focus;
        let mut submit = enter && !focus;
        let mut cancel = false;

        egui::Window::new("Add link")
            .id(Id::unique("kame-link-prompt"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.label("Link URL:");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut buffer)
                        .id(Id::unique("kame-link-url"))
                        .desired_width(360.0)
                        .hint_text("https://example.com"),
                );
                if focus {
                    field.request_focus();
                }
                ui.horizontal(|ui| {
                    if ui.button("Set link").clicked() {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if let Some(prompt) = self.link_prompt.as_mut() {
            prompt.url = buffer;
            prompt.focus = false;
        }
        if cancel {
            self.link_prompt = None;
            return;
        }
        if !submit {
            return;
        }
        let (block_id, start, end) = self
            .link_prompt
            .as_ref()
            .map(|prompt| (prompt.block_id, prompt.start, prompt.end))
            .unwrap_or((BlockId::nil(), 0, 0));
        let url = self
            .link_prompt
            .as_ref()
            .map(|prompt| prompt.url.trim().to_string())
            .unwrap_or_default();
        self.link_prompt = None;
        self.session.set_link(block_id, start, end, url);
    }

    /// The "Insert image" path prompt: type an image file path, then embed it
    /// as an image block sized to the body width. A failed path still inserts
    /// the block (the image simply draws nothing) so the author can fix it.
    fn show_image_prompt(&mut self, ctx: &egui::Context, enter: bool) {
        let Some(prompt) = &self.image_prompt else {
            return;
        };
        let mut buffer = prompt.path.clone();
        let focus = prompt.focus;
        // A chord and an Enter arriving in the same frame would otherwise
        // insert a path nobody has finished typing.
        let mut submit = enter && !focus;
        let mut cancel = false;

        egui::Window::new("Insert image")
            .id(Id::unique("kame-image-prompt"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.label("Path of the image file:");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut buffer)
                        .id(Id::unique("kame-image-path"))
                        .desired_width(360.0)
                        .hint_text("~/Pictures/diagram.png"),
                );
                if focus {
                    field.request_focus();
                }
                ui.horizontal(|ui| {
                    if ui.button("Insert").clicked() {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if let Some(prompt) = self.image_prompt.as_mut() {
            prompt.path = buffer;
            prompt.focus = false;
        }
        if cancel {
            self.image_prompt = None;
            return;
        }
        if !submit {
            return;
        }
        let path = self
            .image_prompt
            .as_ref()
            .map(|prompt| prompt.path.trim().to_string())
            .unwrap_or_default();
        self.image_prompt = None;
        if path.is_empty() {
            return;
        }
        let base = self
            .session
            .path()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        self.session
            .insert_image(resolve_open_path(&path, &base).display().to_string());
    }

    /// The "Restore Snapshot" picker: choose a snapshot, then confirm that the
    /// current text is discarded.
    fn show_restore_picker(&mut self, ctx: &egui::Context, enter: bool) {
        let (up, down) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
            )
        });
        let Some(picker) = self.restore_picker.as_mut() else {
            return;
        };
        // The picker has no text field, so the frame's arrow keys are its own.
        // Arrows do not walk the confirmation step.
        if !picker.confirming && up != down {
            picker.move_selection(down);
        }
        let confirming = picker.confirming;
        let mut picked = picker.selected;
        let snapshots = picker.snapshots.clone();
        let now = now_unix();

        let mut advance = enter;
        let mut back = false;
        let mut cancel = false;
        egui::Window::new(if confirming {
            "Restore snapshot?"
        } else {
            "Restore snapshot"
        })
        .id(Id::unique("kame-restore-picker"))
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .order(Order::Foreground)
        .show(ctx, |ui| {
            if confirming {
                let Some(meta) = picked.and_then(|id| snapshots.iter().find(|meta| meta.id == id))
                else {
                    cancel = true;
                    return;
                };
                ui.label(format!("Restore {}?", describe_snapshot(meta, now)));
                ui.label(
                    egui::RichText::new(
                        "The text in the document now is discarded and the restored version \
                         starts a fresh history. The snapshot stays in the list, so restoring \
                         again is how this is undone.",
                    )
                    .small(),
                );
                ui.horizontal(|ui| {
                    if ui.button("Restore and discard the current text").clicked() {
                        advance = true;
                    }
                    if ui.button("Back").clicked() {
                        back = true;
                    }
                });
            } else {
                ui.label(
                    egui::RichText::new("Choose a snapshot (newest first):")
                        .small()
                        .weak(),
                );
                for meta in &snapshots {
                    if ui
                        .selectable_label(picked == Some(meta.id), describe_snapshot(meta, now))
                        .clicked()
                    {
                        picked = Some(meta.id);
                    }
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(picked.is_some(), egui::Button::new("Restore…"))
                        .clicked()
                    {
                        advance = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            }
        });

        if let Some(picker) = self.restore_picker.as_mut() {
            picker.selected = picked;
            if back {
                picker.confirming = false;
                return;
            }
        }
        if cancel {
            self.dismiss_dialog();
            return;
        }
        if !advance {
            return;
        }
        if confirming {
            if let Some(id) = picked {
                self.restore_snapshot(id);
            }
        } else if let Some(picker) = self.restore_picker.as_mut() {
            picker.confirming = true;
        }
    }
}

impl eframe::App for KameApp {
    /// An anonymous document with content has not been saved anywhere: a close
    /// request opens a "save?" prompt instead of closing. The prompt is rendered
    /// by `show_close_prompt`; a document already on disk, or an empty Untitled,
    /// closes without asking.
    ///
    /// This runs here rather than in `ui` because eframe skips `ui` while the
    /// window is minimized or occluded, and a close nothing cancels there exits.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|input| input.events.iter().any(is_quit_chord)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        let unsaved =
            !self.session.save_failed && self.session.is_anonymous() && self.session.has_content();
        if close_requested && self.close_flow.on_close_request(unsaved) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            // A hidden window has to come forward for its prompt to be answered.
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_input(ui.ctx());
        // A failed write is not a reason to lose the session: the first close
        // request is refused while the flag is set, and the error strip retries
        // the save. A second request is allowed through, so a write that can
        // never land cannot make the window unclosable. (egui delivers one
        // close request per X click, so `cancel_close` counts clicks, not
        // frames.)
        let close_requested = ui.ctx().input(|input| input.viewport().close_requested());
        if self
            .close
            .cancel_close(self.session.save_failed, close_requested)
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.show_close_prompt(ui.ctx());
        // The measure follows the window *before* the scroll area is built: the
        // scroll bar's presence would otherwise feed back into the wrap width
        // and make the page oscillate at the threshold. The panel width does not
        // depend on it.
        let base = LayoutConfig::with_body_size(self.body_size);
        let available = (ui.available_width() - SCROLLBAR_ALLOWANCE).max(1.0);
        let (body_width, margin_width) = page_measure(self.modes.hide_margin, available);
        let config = LayoutConfig {
            body_width,
            margin_width,
            ..base
        };
        if self.renderer.set_config(config) {
            // Everything cached was shaped and rasterized at the old measure.
            self.layouts.clear();
            self.textures.clear();
        }
        let error = error_indicator(self.session.last_error.as_deref()).map(str::to_owned);
        // The find bar is transient and only exists because the author opened it
        // (Ctrl-F), so it shows even when the status panel is hidden.
        self.show_find_bar(ui);
        let history = self.session.history();
        self.notice_tracker.update(&mut self.notice, history);
        // Hide-chrome mode drops the menu bar and the status row together.
        if !self.modes.hide_chrome {
            self.show_status(ui);
        }
        // The typewriter's centring offset was measured last frame, against the
        // same geometry, and is applied here where the scroll area is built.
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(offset) = self.pending_scroll.take() {
            scroll = scroll.vertical_scroll_offset(offset);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(self.palette.background))
            .show(ui, |ui| {
                scroll.show(ui, |ui| self.draw_surface(ui));
                // The save-failure signal belongs to the page, not the chrome:
                // it is painted in this panel so it outlives hide-chrome.
                if let Some(error) = &error {
                    // A failed write's strip retries the save; any other message
                    // (a degraded open, a failed export) is dismissed, so it
                    // cannot sit there offering a write that does nothing.
                    let retry = self.session.save_failed;
                    if draw_error_indicator(ui, error, retry, self.palette.error_ink) {
                        self.session.retry_or_dismiss();
                    }
                }
            });
        // Dialogs are drawn last, from the root: above the page, and available
        // even while the chrome (and with it the buttons) is hidden.
        self.show_dialogs(ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kame-gui-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("doc.kame")
    }

    fn open(path: &Path) -> Session {
        Session::open(path).expect("open a session")
    }

    fn first_block_id(session: &Session) -> crate::model::BlockId {
        session.document().blocks[0].id
    }

    // --- distraction-free modes ---------------------------------------------

    const ALL_MODES: [ModeCommand; 5] = [
        ModeCommand::Fullscreen,
        ModeCommand::HideChrome,
        ModeCommand::HideMargin,
        ModeCommand::ParagraphFocus,
        ModeCommand::Typewriter,
    ];

    #[test]
    fn modes_start_off_and_toggle_independently() {
        for command in ALL_MODES {
            let mut modes = Modes::default();
            assert!(!modes.is_on(command), "{command:?} starts off");
            assert!(modes.toggle(command), "{command:?} turns on");
            for other in ALL_MODES {
                assert_eq!(
                    modes.is_on(other),
                    other == command,
                    "toggling {command:?} must not touch {other:?}"
                );
            }
            assert!(!modes.toggle(command), "{command:?} turns back off");
            assert!(!modes.is_on(command));
        }
    }

    #[test]
    fn mode_keys_are_chords_that_do_not_collide_with_the_editor() {
        let chord = egui::Modifiers {
            command: true,
            shift: true,
            ..egui::Modifiers::default()
        };
        assert_eq!(
            mode_for_key(egui::Key::F11, egui::Modifiers::NONE),
            Some(ModeCommand::Fullscreen)
        );
        // F11 stays fullscreen even if a modifier is held.
        assert_eq!(
            mode_for_key(egui::Key::F11, chord),
            Some(ModeCommand::Fullscreen)
        );
        assert_eq!(
            mode_for_key(egui::Key::H, chord),
            Some(ModeCommand::HideChrome)
        );
        assert_eq!(
            mode_for_key(egui::Key::M, chord),
            Some(ModeCommand::HideMargin)
        );
        assert_eq!(
            mode_for_key(egui::Key::P, chord),
            Some(ModeCommand::ParagraphFocus)
        );
        assert_eq!(
            mode_for_key(egui::Key::T, chord),
            Some(ModeCommand::Typewriter)
        );

        // A bare letter is prose, a command-only chord is the editor's
        // (Ctrl-T/undo/redo stay untouched), and unrelated chords are ignored.
        assert_eq!(mode_for_key(egui::Key::H, egui::Modifiers::NONE), None);
        assert_eq!(
            mode_for_key(
                egui::Key::T,
                egui::Modifiers {
                    command: true,
                    ..egui::Modifiers::default()
                }
            ),
            None
        );
        assert_eq!(mode_for_key(egui::Key::Z, chord), None);
    }

    #[test]
    fn only_pressed_key_events_count_as_mode_chords() {
        let chord = egui::Modifiers {
            command: true,
            shift: true,
            ..egui::Modifiers::default()
        };
        let events = vec![
            egui::Event::Key {
                key: egui::Key::P,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: chord,
            },
            // The key-up must not toggle the mode a second time.
            egui::Event::Key {
                key: egui::Key::P,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: chord,
            },
            // Nor may a held key, which repeats: one press is one toggle.
            egui::Event::Key {
                key: egui::Key::P,
                physical_key: None,
                pressed: true,
                repeat: true,
                modifiers: chord,
            },
            egui::Event::Text("p".to_string()),
        ];
        assert_eq!(
            mode_commands_for_events(&events),
            vec![ModeCommand::ParagraphFocus]
        );
    }

    #[test]
    fn dark_toggle_chord_and_palette_selection() {
        let chord = egui::Modifiers {
            command: true,
            shift: true,
            ..Default::default()
        };
        let key = |pressed: bool, repeat: bool, modifiers: egui::Modifiers| egui::Event::Key {
            key: egui::Key::D,
            physical_key: None,
            pressed,
            repeat,
            modifiers,
        };
        assert!(dark_toggle_for_events(&[key(true, false, chord)]));
        assert!(
            !dark_toggle_for_events(&[key(true, true, chord)]),
            "a held key must not flap the toggle"
        );
        assert!(
            !dark_toggle_for_events(&[key(true, false, egui::Modifiers::NONE)]),
            "the chord is required"
        );

        // Light and dark are distinct palettes; each selects its own paper.
        assert_ne!(
            Palette::for_mode(false).background,
            Palette::for_mode(true).background
        );
        assert_eq!(
            Palette::for_mode(false).background,
            Palette::LIGHT.background
        );
        assert_eq!(Palette::for_mode(true).background, Palette::DARK.background);
    }

    /// FIX 2: the mode→window dispatch is pure, so both directions of every
    /// mode are asserted here instead of relying on an interactive Xvfb smoke —
    /// the missing "off" edge is exactly how enter-but-not-leave shipped.
    #[test]
    fn viewport_effects_send_fullscreen_both_ways_and_nothing_else() {
        use egui::ViewportCommand;
        assert_eq!(
            viewport_effect(ModeCommand::Fullscreen, true),
            Some(ViewportCommand::Fullscreen(true)),
            "the first F11 asks the window to go fullscreen"
        );
        assert_eq!(
            viewport_effect(ModeCommand::Fullscreen, false),
            Some(ViewportCommand::Fullscreen(false)),
            "the second F11 must ask the window to leave fullscreen"
        );
        // The other four modes are painted from their flags and must leave the
        // window alone in both directions.
        for mode in [
            ModeCommand::HideChrome,
            ModeCommand::HideMargin,
            ModeCommand::ParagraphFocus,
            ModeCommand::Typewriter,
        ] {
            assert_eq!(
                viewport_effect(mode, true),
                None,
                "{mode:?} stays inside the frame"
            );
            assert_eq!(
                viewport_effect(mode, false),
                None,
                "{mode:?} stays inside the frame"
            );
        }
    }

    /// FIX 1: hide-chrome removes the whole status panel, so a failed write
    /// would otherwise be completely silent and the author would keep typing
    /// into a read-only document. The indicator message is read from the error
    /// alone — never from the chrome flag — because it is painted in the
    /// central panel, where hide-chrome cannot reach it.
    #[test]
    fn a_failed_save_keeps_its_indicator_when_the_chrome_is_hidden() {
        let error = "save failed — editing disabled: disk full";
        assert_eq!(error_indicator(Some(error)), Some(error));

        let mut modes = Modes::default();
        assert!(modes.toggle(ModeCommand::HideChrome));
        assert!(modes.hide_chrome, "the status panel is gone in this mode");
        assert_eq!(
            error_indicator(Some(error)),
            Some(error),
            "the failure must still have a message to paint"
        );

        // A healthy session has nothing to show, and a blank message is not a
        // failure.
        assert_eq!(error_indicator(None), None);
        assert_eq!(error_indicator(Some("")), None);
        assert_eq!(error_indicator(Some("   ")), None);
    }

    #[test]
    fn a_close_is_refused_once_and_then_yields() {
        let mut guard = CloseGuard::default();
        assert!(
            !guard.cancel_close(true, false),
            "no close request, nothing to cancel"
        );
        assert!(
            guard.cancel_close(true, true),
            "a failed write must keep the window open"
        );
        assert!(
            !guard.cancel_close(true, false),
            "a frame without a request neither cancels nor counts"
        );
        assert!(
            !guard.cancel_close(true, true),
            "a second close goes through: a write that can never land must not trap the session"
        );

        // A healthy session closes on the first request.
        let mut guard = CloseGuard::default();
        assert!(!guard.cancel_close(false, true), "a healthy session closes");

        // A successful write re-arms the guard for a later failure.
        let mut guard = CloseGuard::default();
        assert!(guard.cancel_close(true, true));
        assert!(!guard.cancel_close(false, true), "the write landed: close");
        assert!(
            guard.cancel_close(true, true),
            "a later failure gets its own refusal"
        );
    }

    #[test]
    fn hiding_the_margin_drops_the_column_but_keeps_the_body_put() {
        let width = 1400.0;
        let config = LayoutConfig {
            body_width: width * BODY_FRACTION,
            margin_width: width * (MARGIN_FRACTION - MARGIN_EDGE),
            ..LayoutConfig::default()
        };
        let shown = page_geometry(config, width);
        let hidden = page_geometry(
            LayoutConfig {
                margin_width: 0.0,
                ..config
            },
            width,
        );

        assert_eq!(shown.body_left, width * GUTTER_FRACTION);
        assert_eq!(shown.margin_width, config.margin_width);
        assert_eq!(hidden.margin_width, 0.0);
        // Hiding the margin only empties the right column; the body does not
        // move, so the author's place does not jump.
        assert_eq!(shown.body_left, hidden.body_left);
    }

    #[test]
    fn the_page_is_proportional_and_narrow_windows_collapse_the_margin() {
        // Body is half the window (exact); the margin is the right quarter
        // less the 5% edge padding.
        let (body, margin) = page_measure(false, 1000.0);
        assert_eq!(body, 500.0);
        assert!((margin - 200.0).abs() < 0.01, "margin {margin}");
        let (wide_body, wide_margin) = page_measure(false, 1400.0);
        assert_eq!(wide_body, 700.0);
        assert!((wide_margin - 280.0).abs() < 0.01, "margin {wide_margin}");

        // Hiding the margin keeps the half-width measure but drops the column.
        assert_eq!(page_measure(true, 1000.0), (500.0, 0.0));

        // Below twice the minimum measure the page cannot fit its gutter and
        // margin, so the prose fills the window and the body is flush left.
        assert_eq!(page_measure(false, 560.0), (560.0, 0.0));
        assert_eq!(
            page_geometry(
                LayoutConfig {
                    body_width: 560.0,
                    margin_width: 0.0,
                    ..LayoutConfig::default()
                },
                560.0,
            )
            .body_left,
            0.0
        );
    }

    #[test]
    fn kame_config_round_trips_and_defaults_the_body_size() {
        let config = KameConfig {
            body_size: 21.0,
            dark: true,
        };
        let json = serde_json::to_string(&config).unwrap();
        let read = serde_json::from_str::<KameConfig>(&json).unwrap();
        assert_eq!(read.body_size, 21.0);
        assert!(read.dark);

        // A config written by an older build (no body_size, no dark) still
        // parses and defaults to light, body-size 16.
        let old = serde_json::from_str::<KameConfig>("{}").unwrap();
        assert_eq!(old.body_size, DEFAULT_BODY_SIZE);
        assert!(!old.dark);
    }

    #[test]
    fn paragraph_focus_veils_every_block_but_the_carets() {
        let veil = Palette::LIGHT.dim_veil;
        assert_eq!(dim_veil(veil, false, 0, Some(1)), None, "mode off");
        assert_eq!(dim_veil(veil, true, 1, Some(1)), None, "the caret's block");
        assert_eq!(
            dim_veil(veil, true, 0, Some(1)),
            Some(veil),
            "every other block"
        );
        // Without a resolved caret block nothing is dimmed, rather than the
        // whole page going faint.
        assert_eq!(dim_veil(veil, true, 0, None), None);
    }

    #[test]
    fn typewriter_offset_centres_the_caret_and_clamps_to_the_page() {
        // Caret in the middle of a long page: centre it.
        assert_eq!(typewriter_scroll_offset(600.0, 400.0, 2000.0), 400.0);
        // Near the top the page cannot scroll above its own start.
        assert_eq!(typewriter_scroll_offset(40.0, 400.0, 2000.0), 0.0);
        // Near the bottom it stops at the last full viewport.
        assert_eq!(typewriter_scroll_offset(1900.0, 400.0, 2000.0), 1600.0);
        // Content shorter than the viewport never scrolls.
        assert_eq!(typewriter_scroll_offset(300.0, 400.0, 300.0), 0.0);
    }

    fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn alt_letters_open_their_menus() {
        for (key, menu) in [
            (egui::Key::F, BarMenu::File),
            (egui::Key::A, BarMenu::Annotate),
            (egui::Key::V, BarMenu::View),
            (egui::Key::H, BarMenu::Help),
        ] {
            let mut events = vec![key_event(key, egui::Modifiers::ALT)];
            assert_eq!(take_menu_chord(&mut events, false), Some(menu));
        }
    }

    /// egui-winit sends the letter as text too, which would land in the prose.
    #[test]
    fn a_chord_swallows_its_own_key_and_text() {
        let mut events = vec![
            key_event(egui::Key::F, egui::Modifiers::ALT),
            egui::Event::Text("f".to_string()),
        ];
        assert_eq!(take_menu_chord(&mut events, false), Some(BarMenu::File));
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_chord_leaves_other_input_alone() {
        let mut events = vec![
            egui::Event::Text("f".to_string()),
            key_event(egui::Key::F, egui::Modifiers::ALT),
            egui::Event::Text("f".to_string()),
            egui::Event::Text("x".to_string()),
        ];
        assert_eq!(take_menu_chord(&mut events, false), Some(BarMenu::File));
        assert_eq!(
            events,
            vec![
                egui::Event::Text("f".to_string()),
                egui::Event::Text("x".to_string()),
            ]
        );
    }

    /// Option plus a letter is how a Mac types characters like ƒ and å.
    #[test]
    fn alt_letters_keep_their_meaning_on_macos() {
        let mut events = vec![
            key_event(egui::Key::F, egui::Modifiers::ALT),
            egui::Event::Text("ƒ".to_string()),
        ];
        let before = events.clone();
        assert_eq!(take_menu_chord(&mut events, true), None);
        assert_eq!(events, before);
    }

    #[test]
    fn only_a_plain_alt_letter_is_a_chord() {
        let ctrl_alt = egui::Modifiers {
            alt: true,
            ctrl: true,
            ..egui::Modifiers::NONE
        };
        let shift_alt = egui::Modifiers {
            alt: true,
            shift: true,
            ..egui::Modifiers::NONE
        };
        for event in [
            key_event(egui::Key::F, ctrl_alt),
            key_event(egui::Key::F, shift_alt),
            key_event(egui::Key::F, egui::Modifiers::NONE),
            key_event(egui::Key::F, egui::Modifiers::CTRL),
            key_event(egui::Key::Q, egui::Modifiers::ALT),
            egui::Event::Key {
                key: egui::Key::F,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::ALT,
            },
            egui::Event::Key {
                key: egui::Key::F,
                physical_key: None,
                pressed: true,
                repeat: true,
                modifiers: egui::Modifiers::ALT,
            },
        ] {
            let mut events = vec![event.clone()];
            assert_eq!(take_menu_chord(&mut events, false), None, "{event:?}");
            assert_eq!(events, vec![event]);
        }
    }

    /// An empty heading still needs a line, at its own level's size: a shared
    /// minimum would leave small headings in blocks sized for the largest.
    #[test]
    fn a_blocks_minimum_height_follows_its_heading_level() {
        let config = LayoutConfig::with_body_size(16.0);
        let mut renderer = GlyphRenderer::new(config);
        let mut height = |level| {
            let block = Block {
                id: uuid::Uuid::new_v4(),
                kind: BlockKind::Heading { level },
                text: String::new(),
                spans: Vec::new(),
            };
            let layout = renderer.layout(&block);
            block_height(&block, &layout, &config)
        };
        let (one, two, three) = (height(1), height(2), height(3));
        assert!(one > two && two > three, "{one} {two} {three}");
    }

    #[test]
    fn the_first_save_of_an_import_suggests_its_source_name() {
        let dir = std::env::temp_dir().join(format!("kame-name-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.md");
        std::fs::write(&path, "Title: Something Else\n\nWords.\n").unwrap();
        let (session, _) = Session::open_any(&path).unwrap();
        assert_eq!(session.suggested_file_name(), "notes.kame");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_document_suggests_untitled() {
        let session = Session::anonymous().unwrap();
        assert_eq!(session.suggested_file_name(), "Untitled.kame");
    }

    /// A premultiplied channel above alpha adds light instead of tinting: over
    /// the paper it clamped to white, the look of a pasted patch.
    #[test]
    fn every_wash_is_a_valid_premultiplied_colour() {
        for dark in [false, true] {
            let palette = Palette::for_mode(dark);
            for wash in [
                palette.note_highlight,
                palette.footnote_highlight,
                palette.marginal_highlight,
            ] {
                let a = wash.a();
                assert!(
                    wash.r() <= a && wash.g() <= a && wash.b() <= a,
                    "{wash:?} in dark={dark} is not premultiplied"
                );
            }
        }
    }

    #[test]
    fn a_wash_tints_the_paper_rather_than_whitening_it() {
        let palette = Palette::for_mode(false);
        let paper = palette.background;
        for wash in [palette.footnote_highlight, palette.marginal_highlight] {
            let [r, g, b] = overlay_on(paper, wash);
            assert!(
                r <= paper.r() && g <= paper.g() && b <= paper.b(),
                "wash {wash:?} over the paper gave {:?}",
                [r, g, b]
            );
            assert_ne!(
                [r, g, b],
                [paper.r(), paper.g(), paper.b()],
                "wash is invisible"
            );
        }
    }

    fn heading_kind(level: u8) -> BlockKind {
        BlockKind::Heading { level }
    }

    #[test]
    fn a_heading_sits_closer_to_what_it_introduces() {
        let body = BlockKind::Paragraph;
        assert!(gap_after(&heading_kind(2), &body) < BLOCK_GAP);
        assert!(gap_after(&body, &heading_kind(2)) > BLOCK_GAP);
        assert!(gap_after(&body, &heading_kind(2)) > gap_after(&heading_kind(2), &body) * 2.0);
    }

    #[test]
    fn a_larger_heading_takes_more_space_above() {
        let body = BlockKind::Paragraph;
        assert!(gap_after(&body, &heading_kind(1)) > gap_after(&body, &heading_kind(2)));
        assert!(gap_after(&body, &heading_kind(2)) > gap_after(&body, &heading_kind(3)));
    }

    #[test]
    fn a_quote_is_set_off_from_the_prose_around_it() {
        let body = BlockKind::Paragraph;
        let quote = BlockKind::BlockQuote;
        assert!(gap_after(&body, &quote) > BLOCK_GAP);
        assert!(gap_after(&quote, &body) > BLOCK_GAP);
        assert_eq!(
            gap_after(&quote, &quote),
            BLOCK_GAP,
            "one quote's paragraphs stay together"
        );
    }

    #[test]
    fn plain_prose_keeps_its_gap() {
        let body = BlockKind::Paragraph;
        assert_eq!(gap_after(&body, &body), BLOCK_GAP);
    }

    #[test]
    fn a_missing_image_reserves_a_short_placeholder() {
        let height = image_height("/does/not/exist.png", 600.0);
        assert_eq!(height, MISSING_IMAGE_HEIGHT);
        assert!(
            height < 120.0,
            "a placeholder is not a blank page: {height}"
        );
    }

    #[test]
    fn a_footnote_needs_room_at_the_end_of_its_range_only_for_footnotes() {
        let mut document = Document::new("t");
        let block_id = document.blocks[0].id;
        document.blocks[0].text = "One two three".to_string();
        let annotation = |kind, start, end| Annotation {
            id: uuid::Uuid::new_v4(),
            kind,
            anchor: Anchor::range(block_id, start, end),
            body: "b".to_string(),
        };
        document.annotations = vec![
            annotation(AnnotationKind::Footnote, 8, 13),
            annotation(AnnotationKind::Footnote, 3, 3),
            annotation(AnnotationKind::Note, 4, 7),
        ];
        assert_eq!(footnote_marks(&document)[&block_id], vec![3, 13]);
    }

    #[test]
    fn the_mark_is_drawn_in_its_room_before_the_next_glyph() {
        let config = LayoutConfig::with_body_size(16.0);
        let mut renderer = GlyphRenderer::new(config);
        let block = Block {
            id: uuid::Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "A claim stands.".to_string(),
            spans: Vec::new(),
        };
        let plain = renderer.layout(&block);
        let marked = renderer.layout_marked(&block, &[7]);
        let anchor = Anchor::range(block.id, 7, 7);
        let room = FOOTNOTE_MARK_EM * config.body_size;
        let word_end = superscript_origin(&plain, &anchor, 0.0, 0.0, config.body_width, 0.0)
            .unwrap()
            .x;
        let mark = superscript_origin(&marked, &anchor, 0.0, 0.0, config.body_width, room)
            .unwrap()
            .x;
        assert!(
            (mark - word_end).abs() < 1.0,
            "{mark} vs the word's end {word_end}"
        );
    }

    /// A document keeps the title it was created with, so one started as Untitled
    /// and saved under a name would otherwise be Untitled for good.
    #[test]
    fn a_saved_untitled_document_is_named_by_its_file() {
        let path = temp_db_path().with_file_name("Document.kame");
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        assert_eq!(session.display_title(), "Untitled");
        session.save_as(&path).unwrap();
        assert_eq!(session.display_title(), "Document");
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(
            reopened.document().title,
            "Untitled",
            "the stored title is untouched"
        );
        assert_eq!(reopened.display_title(), "Document");
    }

    #[test]
    fn a_real_title_outranks_the_file_name() {
        let dir = std::env::temp_dir().join(format!("kame-title-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("notes.md");
        std::fs::write(&source, "Title: A Real Title\n\nWords.\n").unwrap();
        let (mut session, _) = Session::open_any(&source).unwrap();
        assert_eq!(session.display_title(), "A Real Title");
        session.save_as(&dir.join("notes.kame")).unwrap();
        assert_eq!(session.display_title(), "A Real Title");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// What double-clicking a file depends on, kept in step across the packaging.
    #[test]
    fn the_desktop_entry_and_mime_type_agree() {
        let desktop = include_str!("../packaging/kame.desktop");
        let mime = include_str!("../packaging/kame.xml");
        assert!(
            desktop.contains("MimeType=application/x-kame;"),
            "{desktop}"
        );
        assert!(desktop.contains("Exec=@PREFIX@/kame %f"), "{desktop}");
        assert!(mime.contains(r#"type="application/x-kame""#), "{mime}");
        assert!(mime.contains(r#"pattern="*.kame""#), "{mime}");
    }

    #[test]
    fn dates_are_written_as_iso_days() {
        use std::time::{Duration, UNIX_EPOCH};
        let day = |secs: u64| iso_date(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(951_782_400), "2000-02-29");
        assert_eq!(day(1_793_577_600), "2026-11-02");
        assert_eq!(day(1_735_603_200), "2024-12-31");
        assert_eq!(day(4_107_542_400), "2100-03-01");
    }

    #[test]
    fn tags_split_on_commas_and_drop_blanks() {
        assert_eq!(parse_tags(" life, work ,,"), vec!["life", "work"]);
        assert!(parse_tags("  ").is_empty());
    }

    #[test]
    fn a_new_document_is_created_today_and_goes_by_untitled() {
        let session = Session::anonymous().unwrap();
        assert_eq!(
            session.metadata.created,
            iso_date(std::time::SystemTime::now())
        );
        assert_eq!(session.metadata.title, "");
        assert_eq!(session.display_title(), "Untitled");
    }

    #[test]
    fn metadata_persists_across_a_reopen_outside_the_undo_log() {
        let path = temp_db_path();
        let mut session = open(&path);
        let before = session.history();
        session.set_metadata(Metadata {
            title: "Stored".to_string(),
            author: "K".to_string(),
            ..session.metadata.clone()
        });
        assert_eq!(session.history(), before, "not an undo step");
        assert_eq!(session.display_title(), "Stored");
        drop(session);
        let reopened = open(&path);
        assert_eq!(reopened.metadata.title, "Stored");
        assert_eq!(reopened.metadata.author, "K");
    }

    #[test]
    fn save_as_carries_metadata_to_the_file() {
        let path = temp_db_path();
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        session.set_metadata(Metadata {
            title: "Carried".to_string(),
            ..session.metadata.clone()
        });
        session.save_as(&path).unwrap();
        drop(session);
        assert_eq!(open(&path).metadata.title, "Carried");
    }

    #[test]
    fn restoring_a_snapshot_leaves_metadata_alone() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("Words");
        let id = session.snapshot(None).expect("a snapshot");
        session.set_metadata(Metadata {
            title: "After".to_string(),
            ..session.metadata.clone()
        });
        assert!(session.restore(id));
        assert_eq!(session.metadata.title, "After");
    }

    /// A file from before the metadata table takes its title from the document,
    /// unless that was never named, and its date from the file.
    #[test]
    fn an_older_file_gets_reasonable_defaults() {
        let path = temp_db_path().with_file_name("Essay.kame");
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        session.save_as(&path).unwrap();
        drop(session);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE metadata").ok();
        drop(conn);
        let reopened = open(&path);
        assert_eq!(reopened.metadata.title, "", "Untitled was never a name");
        assert_eq!(reopened.display_title(), "Essay");
        assert!(!reopened.metadata.created.is_empty());
        assert_eq!(reopened.metadata.author, "");
    }

    #[test]
    fn an_import_keeps_its_front_matter() {
        let dir = std::env::temp_dir().join(format!("kame-meta-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("notes.md");
        std::fs::write(&source, "Title: T\nAuthor: K\nTags: a, b\n\nWords.\n").unwrap();
        let (session, _) = Session::open_any(&source).unwrap();
        assert_eq!(session.metadata.title, "T");
        assert_eq!(session.metadata.author, "K");
        assert_eq!(session.metadata.tags, vec!["a", "b"]);
        assert!(!session.metadata.created.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exports_publish_the_metadata() {
        let path = temp_db_path().with_file_name("Pub.kame");
        let mut session = open(&path);
        session.insert("Words");
        session.set_metadata(Metadata {
            author: "K".to_string(),
            ..session.metadata.clone()
        });
        let markdown_path = session.export_markdown().expect("markdown written");
        let markdown = std::fs::read_to_string(markdown_path).unwrap();
        assert!(markdown.contains("Author: K\n"), "{markdown}");
        let html_path = session.export_html().expect("html written");
        let html = std::fs::read_to_string(html_path).unwrap();
        assert!(
            html.contains(r#"<meta name="author" content="K">"#),
            "{html}"
        );
    }

    #[test]
    fn only_active_modes_get_a_badge() {
        let mut modes = Modes::default();
        assert!(mode_badges(&modes).is_empty(), "no modes, no clutter");
        modes.toggle(ModeCommand::ParagraphFocus);
        modes.toggle(ModeCommand::Typewriter);
        assert_eq!(
            mode_badges(&modes),
            vec![("F", "paragraph focus"), ("T", "typewriter")]
        );
    }

    #[test]
    fn every_visible_mode_has_its_own_badge() {
        let mut modes = Modes::default();
        modes.toggle(ModeCommand::Fullscreen);
        modes.toggle(ModeCommand::HideMargin);
        modes.toggle(ModeCommand::ParagraphFocus);
        modes.toggle(ModeCommand::Typewriter);
        let short: Vec<&str> = mode_badges(&modes)
            .iter()
            .map(|(short, _)| *short)
            .collect();
        assert_eq!(short, vec!["FS", "M", "F", "T"]);
    }

    #[test]
    fn hide_chrome_has_no_badge_because_the_row_is_gone() {
        let mut modes = Modes::default();
        modes.toggle(ModeCommand::HideChrome);
        assert!(mode_badges(&modes).is_empty());
    }

    #[test]
    fn word_counts_read_naturally() {
        assert_eq!(word_count_label(0), "0 words");
        assert_eq!(word_count_label(1), "1 word");
        assert_eq!(word_count_label(999), "999 words");
        assert_eq!(word_count_label(1255), "1,255 words");
        assert_eq!(word_count_label(1234567), "1,234,567 words");
    }

    #[test]
    fn shortcuts_name_the_platforms_modifier() {
        assert_eq!(shortcut_label("Ctrl-Shift-E", false), "Ctrl-Shift-E");
        assert_eq!(shortcut_label("Ctrl-Shift-E", true), "Cmd-Shift-E");
        assert_eq!(shortcut_label("F11", true), "F11");
    }

    #[test]
    fn a_notice_stays_until_the_next_edit() {
        let mut tracker = NoticeTracker::default();
        let mut notice = Some("exported".to_string());
        tracker.update(&mut notice, (3, 0));
        tracker.update(&mut notice, (3, 0));
        assert_eq!(notice.as_deref(), Some("exported"), "idle frames keep it");
        tracker.update(&mut notice, (4, 0));
        assert_eq!(notice, None, "an edit clears it");
    }

    #[test]
    fn an_undo_also_clears_a_notice() {
        let mut tracker = NoticeTracker::default();
        let mut notice = Some("exported".to_string());
        tracker.update(&mut notice, (3, 0));
        tracker.update(&mut notice, (2, 1));
        assert_eq!(notice, None);
    }

    /// Restoring a snapshot changes the history in the same frame that it sets
    /// its notice, and an edit must not be read into that.
    #[test]
    fn a_notice_set_with_a_history_change_survives_it() {
        let mut tracker = NoticeTracker::default();
        let mut notice = None;
        tracker.update(&mut notice, (9, 0));
        notice = Some("restored snapshot #2".to_string());
        tracker.update(&mut notice, (0, 0));
        assert_eq!(notice.as_deref(), Some("restored snapshot #2"));
        tracker.update(&mut notice, (0, 0));
        assert_eq!(notice.as_deref(), Some("restored snapshot #2"));
        tracker.update(&mut notice, (1, 0));
        assert_eq!(notice, None);
    }

    #[test]
    fn a_new_notice_replaces_the_old_without_being_cleared() {
        let mut tracker = NoticeTracker::default();
        let mut notice = Some("first".to_string());
        tracker.update(&mut notice, (3, 0));
        notice = Some("second".to_string());
        tracker.update(&mut notice, (3, 0));
        assert_eq!(notice.as_deref(), Some("second"));
    }

    #[test]
    fn only_cmd_q_is_the_quit_chord() {
        let key = |key, pressed, repeat, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers,
        };
        let cmd = egui::Modifiers::MAC_CMD;
        assert!(is_quit_chord(&key(egui::Key::Q, true, false, cmd)));
        // Ctrl-Q on Linux is not a quit chord; kame binds no quit key there.
        assert!(!is_quit_chord(&key(
            egui::Key::Q,
            true,
            false,
            egui::Modifiers::CTRL
        )));
        assert!(!is_quit_chord(&key(
            egui::Key::Q,
            true,
            false,
            egui::Modifiers::NONE
        )));
        assert!(!is_quit_chord(&key(egui::Key::W, true, false, cmd)));
        assert!(!is_quit_chord(&key(egui::Key::Q, false, false, cmd)));
        assert!(!is_quit_chord(&key(egui::Key::Q, true, true, cmd)));
    }

    #[test]
    fn a_close_request_with_unsaved_work_opens_the_prompt() {
        let mut flow = CloseFlow::default();
        assert!(flow.on_close_request(true), "the close must be cancelled");
        assert!(flow.prompting);
    }

    #[test]
    fn a_close_request_with_nothing_at_stake_goes_through() {
        let mut flow = CloseFlow::default();
        assert!(!flow.on_close_request(false));
        assert!(!flow.prompting);
    }

    #[test]
    fn dont_save_lets_its_own_close_request_through() {
        let mut flow = CloseFlow::default();
        assert!(flow.on_close_request(true));
        flow.confirm();
        assert!(
            !flow.on_close_request(true),
            "Don't Save was refused by the prompt it answered"
        );
        assert!(!flow.prompting, "the prompt came back after Don't Save");
    }

    #[test]
    fn cancel_keeps_the_guard_for_the_next_close() {
        let mut flow = CloseFlow::default();
        assert!(flow.on_close_request(true));
        flow.cancel();
        assert!(!flow.prompting);
        assert!(
            flow.on_close_request(true),
            "after Cancel the next close must ask again"
        );
        assert!(flow.prompting);
    }

    #[test]
    fn a_second_close_while_the_prompt_is_open_does_not_discard_the_work() {
        let mut flow = CloseFlow::default();
        assert!(flow.on_close_request(true));
        assert!(
            flow.on_close_request(true),
            "only the prompt's own answer may discard unsaved work"
        );
        assert!(flow.prompting);
    }

    #[test]
    fn markdown_paths_are_recognised_by_extension() {
        for path in ["a.md", "a.markdown", "a.MD", "/x/y.Markdown"] {
            assert!(is_markdown_path(Path::new(path)), "{path}");
        }
        for path in ["a.kame", "md", "a.md.kame", "a"] {
            assert!(!is_markdown_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn opening_markdown_imports_it_as_an_unsaved_document() {
        let dir = std::env::temp_dir().join(format!("kame-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.md");
        let source = "# Hello\n\nBody with `code`.\n\n![](missing.png)\n";
        std::fs::write(&path, source).unwrap();

        let (session, note) = Session::open_any(&path).unwrap();

        assert!(session.is_anonymous(), "the first save must ask for a file");
        assert_eq!(session.document().title, "notes");
        assert_eq!(session.document().blocks[0].text, "Hello");
        assert_eq!(
            session.history(),
            (0, 0),
            "an import starts with no history"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        let note = note.expect("the import is announced");
        assert!(note.contains("notes.md"), "{note}");
        assert!(note.contains("1 inline code"), "{note}");
        assert!(note.contains("1 image not found"), "{note}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn opening_a_missing_markdown_file_fails_without_creating_it() {
        let path = std::env::temp_dir().join(format!("kame-missing-{}.md", uuid::Uuid::new_v4()));
        assert!(Session::open_any(&path).is_err());
        assert!(
            !path.exists(),
            "a failed import must not leave a file behind"
        );
    }

    #[test]
    fn opening_a_kame_path_still_opens_a_document() {
        let path = std::env::temp_dir().join(format!("kame-any-{}.kame", uuid::Uuid::new_v4()));
        let (session, note) = Session::open_any(&path).unwrap();
        assert!(!session.is_anonymous());
        assert!(note.is_none());
        drop(session);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn replacing_a_document_warns_about_work_with_nowhere_to_go() {
        let empty = Session::anonymous().unwrap();
        assert!(
            !empty.would_lose_work(),
            "an empty document has nothing to lose"
        );

        let dir = std::env::temp_dir().join(format!("kame-lose-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let prose = dir.join("prose.md");
        std::fs::write(&prose, "Unsaved words.\n").unwrap();
        let (imported, _) = Session::open_any(&prose).unwrap();
        assert!(imported.would_lose_work(), "an import has no file yet");

        let image = dir.join("image.md");
        std::fs::write(&image, "![](a.png)\n").unwrap();
        let (only_image, _) = Session::open_any(&image).unwrap();
        assert!(only_image.would_lose_work(), "an image is content too");

        let saved = dir.join("saved.kame");
        let (on_disk, _) = Session::open_any(&saved).unwrap();
        assert!(!on_disk.would_lose_work(), "saved work is safe on disk");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_paths_resolve_home_and_relative_to_the_document() {
        let base = Path::new("/home/kyle/data/writing/essay.kame");
        // An absolute path passes through untouched.
        assert_eq!(
            resolve_open_path("/tmp/other.kame", base),
            PathBuf::from("/tmp/other.kame")
        );
        // A relative path resolves beside the current document.
        assert_eq!(
            resolve_open_path("next.kame", base),
            PathBuf::from("/home/kyle/data/writing/next.kame")
        );
        assert_eq!(
            resolve_open_path("notes/next.kame", base),
            PathBuf::from("/home/kyle/data/writing/notes/next.kame")
        );
        // `~` expands to the home directory regardless of the base.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/kyle".into());
        assert_eq!(
            resolve_open_path("~/essays/new.kame", base),
            PathBuf::from(home).join("essays/new.kame")
        );
    }

    #[test]
    fn image_height_preserves_aspect_ratio_and_falls_back() {
        // Write a real 200×100 image to test the aspect ratio.
        let dir = std::env::temp_dir().join(format!("kame-img-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wide.png");
        image::RgbaImage::from_pixel(200, 100, image::Rgba([0, 0, 0, 255]))
            .save(&path)
            .unwrap();

        // A 2:1 image at 600px wide is 300px tall.
        assert_eq!(image_height(path.to_str().unwrap(), 600.0), 300.0);
        // An unreadable path reserves a 3:2 box instead of collapsing.
    }

    #[test]
    fn fresh_file_creates_a_document() {
        let path = temp_db_path();
        let session = open(&path);
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "");
        assert_eq!(session.document().title, "doc");
        assert_eq!(session.caret.offset, 0);
        assert!(path.exists(), "opening creates the .kame file");
    }

    #[test]
    fn a_blank_session_surfaces_the_error_and_stays_editable() {
        let path = temp_db_path();
        let mut session = Session::blank(&path, "could not open — corrupt".into()).unwrap();
        assert_eq!(
            session.last_error.as_deref(),
            Some("could not open — corrupt"),
            "the reason is shown in the GUI, not just the console"
        );
        assert_eq!(session.document().blocks.len(), 1);
        assert!(session.insert("scratch"), "the blank document is editable");
        assert_eq!(session.document().blocks[0].text, "scratch");
        assert!(!path.exists(), "a blank session never creates the bad path");
    }

    #[test]
    fn an_anonymous_session_starts_untitled_with_no_path() {
        let mut session = Session::anonymous().unwrap();
        assert!(session.path().is_none());
        assert!(session.is_anonymous());
        assert_eq!(session.document().title, "Untitled");
        assert!(
            !session.has_content(),
            "a fresh Untitled has nothing to save"
        );

        assert!(
            session.insert("scratch"),
            "the anonymous document is editable"
        );
        assert_eq!(session.document().blocks[0].text, "scratch");
        assert!(session.has_content(), "prose is worth saving");
    }

    #[test]
    fn save_as_persists_the_document_and_its_history() {
        let mut session = Session::anonymous().unwrap();
        session.insert("Hello");
        session.insert(" world");
        assert_eq!(session.history(), (2, 0));

        let path = temp_db_path();
        assert!(!path.exists());
        session.save_as(&path).unwrap();

        assert_eq!(session.path(), Some(path.as_path()));
        assert!(!session.is_anonymous(), "the document now has a file");
        assert!(path.exists(), "save-as wrote the file");

        // The file round-trips: same text, and the undo history travelled too.
        drop(session);
        let reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "Hello world");
        assert_eq!(reopened.history(), (2, 0));
    }

    #[test]
    fn link_target_prefers_selection_then_caret_link() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("click here");
        let id = first_block_id(&session);

        // Nothing selected, no link: no target.
        assert!(session.link_target().is_none());

        // A caret inside a link (no selection) edits that link, pre-filling its URL.
        session.set_link(id, 0, 5, "http://a.com".into());
        session.set_caret(Cursor {
            block_id: id,
            offset: 2,
        });
        assert_eq!(
            session.link_target(),
            Some((id, 0, 5, Some("http://a.com".to_string())))
        );

        // A selection over "here" becomes the target with no pre-fill.
        session.set_caret(Cursor {
            block_id: id,
            offset: 6,
        });
        session.select_to(Cursor {
            block_id: id,
            offset: 10,
        });
        assert_eq!(session.link_target(), Some((id, 6, 10, None)));
    }

    #[test]
    fn typing_persists_and_the_caret_advances() {
        let path = temp_db_path();
        let mut session = open(&path);
        assert!(session.insert("Hello"));
        assert!(session.insert(" world"));
        assert_eq!(session.document().blocks[0].text, "Hello world");
        assert_eq!(session.caret.offset, 11);
        assert_eq!(session.history(), (2, 0));
        drop(session);

        let mut reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "Hello world");
        assert_eq!(reopened.history(), (2, 0));
        // Undo after reopening must undo one insert, not re-apply the log.
        assert!(reopened.undo());
        assert_eq!(reopened.document().blocks[0].text, "Hello");
        assert_eq!(reopened.history(), (1, 1));
    }

    #[test]
    fn split_creates_a_paragraph_and_puts_the_caret_in_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("one two");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 3,
        });
        assert!(session.split());

        assert_eq!(session.document().blocks.len(), 2);
        assert_eq!(session.document().blocks[0].text, "one");
        assert_eq!(session.document().blocks[1].text, " two");
        assert_eq!(session.caret.block_id, session.document().blocks[1].id);
        assert_eq!(session.caret.offset, 0);

        session.insert("new");
        assert_eq!(session.document().blocks[1].text, "new two");
        assert_eq!(session.caret.offset, 3);
        drop(session);

        let reopened = open(&path);
        assert_eq!(reopened.document().blocks.len(), 2);
        assert_eq!(reopened.document().blocks[1].text, "new two");
    }

    #[test]
    fn backspace_at_a_block_start_joins_the_previous_paragraph() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("first");
        session.split();
        session.insert("second");
        assert_eq!(session.document().blocks.len(), 2);
        assert_eq!(session.caret.offset, 6);

        assert!(session.backspace());
        assert_eq!(session.document().blocks[1].text, "secon");
        assert_eq!(session.caret.offset, 5);

        session.set_caret(Cursor {
            block_id: session.caret.block_id,
            offset: 0,
        });
        assert!(session.backspace());
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "firstsecon");
        assert_eq!(session.caret.offset, 5, "caret lands at the join");
        assert_eq!(session.caret.block_id, first_block_id(&session));
    }

    #[test]
    fn delete_at_a_block_end_pulls_the_next_paragraph_up() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("one");
        session.split();
        session.insert("two");
        session.set_caret(Cursor {
            block_id: first_block_id(&session),
            offset: 3,
        });
        assert!(session.delete_forward());
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "onetwo");
        assert_eq!(session.caret.offset, 3);
    }

    #[test]
    fn backspace_deletes_a_whole_utf8_character() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("aé");
        assert_eq!(session.caret.offset, 3);
        assert!(session.backspace());
        assert_eq!(session.document().blocks[0].text, "a");
        assert_eq!(session.caret.offset, 1);
    }

    #[test]
    fn undo_and_redo_persist_head_and_cursor_across_a_reopen() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("ab");
        session.insert("cd");
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, "ab");
        drop(session);

        let mut reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "ab");
        assert_eq!(reopened.history(), (1, 1));
        assert!(reopened.redo());
        assert_eq!(reopened.document().blocks[0].text, "abcd");
        assert_eq!(reopened.history(), (2, 0));
    }

    #[test]
    fn paste_splits_crlf_text_into_paragraphs() {
        let path = temp_db_path();
        let mut session = open(&path);
        assert!(session.paste("one\r\ntwo\r\nthree"));
        let texts: Vec<&str> = session
            .document()
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect();
        assert_eq!(texts, vec!["one", "two", "three"]);
        assert_eq!(session.caret.offset, 5);
        assert_eq!(session.caret.block_id, session.document().blocks[2].id);
    }

    #[test]
    fn arrows_cross_block_boundaries() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("ab");
        session.split();
        session.insert("cd");
        session.set_caret(Cursor {
            block_id: session.caret.block_id,
            offset: 0,
        });
        session.move_left(false);
        assert_eq!(session.caret.offset, 2);
        assert_eq!(session.caret.block_id, first_block_id(&session));
        session.move_right(false);
        assert_eq!(session.caret.offset, 0);
        assert_eq!(session.caret.block_id, session.document().blocks[1].id);
    }

    #[test]
    fn shift_arrow_extends_the_selection() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("word");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 4,
        });

        // Shift-Left from the end selects the previous character, anchored at
        // the start (offset 4).
        session.move_left(true);
        assert_eq!(
            session.ordered_selection(),
            Some((
                Cursor {
                    block_id,
                    offset: 3
                },
                Cursor {
                    block_id,
                    offset: 4
                },
            ))
        );

        // Shift-Right back onto the anchor collapses the selection to nothing.
        session.move_right(true);
        assert_eq!(session.ordered_selection(), None);
    }

    #[test]
    fn clamping_repairs_a_stale_caret_after_undo() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("abc");
        session.split();
        let second = session.document().blocks[1].id;
        session.insert("ghost");
        session.set_caret(Cursor {
            block_id: second,
            offset: 5,
        });
        assert!(session.undo()); // removes "ghost"
        assert_eq!(session.caret.block_id, second);
        assert_eq!(session.caret.offset, 0, "offset clamps to the empty block");
        // Undo the split as well: the block disappears and the caret moves home.
        assert!(session.undo());
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.caret.block_id, first_block_id(&session));
        assert_eq!(session.caret.offset, 0);
    }

    /// FIX 1: undo/redo derive the caret from the op they applied, so typing
    /// after Ctrl-Z inserts where the erased text was.
    #[test]
    fn undo_and_redo_move_the_caret_to_the_affected_span() {
        let path = temp_db_path();
        let mut session = open(&path);
        let block_id = first_block_id(&session);
        session.insert("abc");
        session.set_caret(Cursor {
            block_id,
            offset: 1,
        });
        session.insert("XY");
        assert_eq!(session.document().blocks[0].text, "aXYbc");
        assert_eq!(session.caret.offset, 3);

        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, "abc");
        assert_eq!(
            session.caret.offset, 1,
            "undo parks the caret at the start of the erased span"
        );

        assert!(session.redo());
        assert_eq!(session.document().blocks[0].text, "aXYbc");
        assert_eq!(
            session.caret.offset, 3,
            "redo parks the caret after the reinserted text"
        );
    }

    /// FIX 2: clicks in the page padding or between blocks go to the nearest
    /// block, never to the end of the document.
    #[test]
    fn clicks_in_padding_or_gaps_pick_the_nearest_block() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("one");
        session.split();
        session.insert("two");
        session.split();
        session.insert("three");
        assert_eq!(session.document().blocks.len(), 3);

        let config = LayoutConfig::default();
        let mut renderer = GlyphRenderer::new(config);
        let blocks = session.document().blocks.clone();
        let layouts: Vec<BlockLayout> = blocks.iter().map(|block| renderer.layout(block)).collect();
        let layout_refs: Vec<&BlockLayout> = layouts.iter().collect();
        let heights: Vec<f32> = blocks
            .iter()
            .zip(layouts.iter())
            .map(|(block, layout)| block_height(block, layout, &config))
            .collect();
        let mut tops = Vec::new();
        let mut y = PAGE_PADDING;
        for height in &heights {
            tops.push(y);
            y += height + BLOCK_GAP;
        }
        let body_left = 100.0;

        // Top page padding: the old hit_test fell through to last().end.
        let cursor = hit_test(
            &blocks,
            &layout_refs,
            &tops,
            &heights,
            body_left,
            Pos2::new(body_left, 1.0),
        )
        .expect("a click in the padding still lands somewhere");
        assert_eq!(cursor.block_id, blocks[0].id);
        assert_eq!(cursor.offset, 0);

        // The gap between blocks one and two belongs to a neighbour, never to
        // the last block in the document.
        let gap_y = tops[0] + heights[0] + BLOCK_GAP * 0.5;
        let cursor = hit_test(
            &blocks,
            &layout_refs,
            &tops,
            &heights,
            body_left,
            Pos2::new(body_left, gap_y),
        )
        .expect("a click in the gap still lands somewhere");
        assert_ne!(cursor.block_id, blocks[2].id);
        assert!(cursor.block_id == blocks[0].id || cursor.block_id == blocks[1].id);
    }

    /// FIX 4: a failed write makes the session read-only instead of letting
    /// memory and DB history diverge; the flag clears on the next good write.
    #[test]
    fn a_save_failure_suppresses_edits_until_a_write_succeeds() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("abc");
        let block_id = first_block_id(&session);
        assert_eq!(session.history(), (1, 0));

        // Simulate the state a failed record_edit leaves behind.
        session.save_failed = true;
        session.last_error = Some("save failed — editing disabled: disk full".into());

        assert!(!session.insert("d"), "insert is refused while save failed");
        assert!(!session.backspace());
        assert!(!session.split());
        assert!(!session.undo());
        assert_eq!(session.document().blocks[0].text, "abc");
        assert_eq!(session.history(), (1, 0), "history did not advance");
        assert!(session.last_error.is_some(), "the error stays visible");

        // Movement is not a mutation and is still allowed.
        session.set_caret(Cursor {
            block_id,
            offset: 3,
        });
        session.move_left(false);
        assert_eq!(session.caret.offset, 2);
        session.set_caret(Cursor {
            block_id,
            offset: 3,
        });

        // A successful write resolves the failure and re-enables editing.
        assert!(session.save_state());
        assert!(!session.save_failed);
        assert!(session.last_error.is_none());
        assert!(session.insert("d"));
        assert_eq!(session.document().blocks[0].text, "abcd");
    }

    #[test]
    fn a_block_kind_change_commits_as_an_undoable_edit() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("Title");
        let block_id = first_block_id(&session);
        assert_eq!(session.document().blocks[0].kind, BlockKind::Paragraph);

        assert!(session.set_block_kind(block_id, BlockKind::Heading { level: 1 }));
        assert_eq!(
            session.document().blocks[0].kind,
            BlockKind::Heading { level: 1 }
        );
        assert_eq!(session.history(), (2, 0), "the change is one undoable op");

        // Undo restores the previous kind: the op carries it.
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].kind, BlockKind::Paragraph);
        assert!(session.redo());

        // The kind is persisted with the head document, so a reopen sees it.
        drop(session);
        let reopened = open(&path);
        assert_eq!(
            reopened.document().blocks[0].kind,
            BlockKind::Heading { level: 1 }
        );
    }

    #[test]
    fn an_unreadable_op_row_degrades_to_a_writable_session() {
        let path = temp_db_path();
        {
            let mut session = open(&path);
            assert!(session.insert("kept"));
        }
        // A row serde cannot read — a field a future build added without
        // `serde(default)`, or a half-written row.
        {
            let conn = rusqlite::Connection::open(&path).expect("open the db directly");
            conn.execute(
                "INSERT INTO ops (json) VALUES ('{\"future\":\"variant\"}')",
                [],
            )
            .expect("write a bad op row");
        }

        let mut session = open(&path);
        assert_eq!(
            session.document().blocks[0].text,
            "kept",
            "the head document opens even when history does not"
        );
        assert_eq!(session.history(), (0, 0), "history is unavailable");
        let warning = session.last_error.clone().expect("a warning is shown");
        assert!(
            warning.contains("history unavailable"),
            "the warning names the degradation, got {warning:?}"
        );
        assert!(!session.save_failed, "a bad read is not a failed write");

        // The log cannot be resumed from, and leaving it beside an editor with
        // no history desyncs the two: the first edit truncates the log as a
        // redo tail (shifting the sequence numbers the cursor counts), the next
        // undo writes the in-memory count as the cursor, and a reopen offers a
        // surviving op as redo against text it never touched. So the open
        // resets the durable state to the degraded editor: empty log, cursor 0.
        {
            let conn = rusqlite::Connection::open(&path).expect("open the db directly");
            let rows: i64 = conn
                .query_row("SELECT COUNT(*) FROM ops", [], |row| row.get(0))
                .expect("count ops");
            assert_eq!(rows, 0, "the unresumable log is dropped with the reset");
            let cursor: String = conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'undo_cursor'",
                    [],
                    |row| row.get(0),
                )
                .expect("read the cursor");
            assert_eq!(cursor, "0", "the cursor no longer points into the log");
        }

        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 4,
        });
        assert!(session.insert("!"), "the session stays writable");
        assert_eq!(session.document().blocks[0].text, "kept!");
        assert_eq!(session.history(), (1, 0));
        assert!(session.undo(), "the new edit is undoable in this session");
        assert_eq!(session.document().blocks[0].text, "kept");
        drop(session);

        // The regression this guards: with the DB left as it was, the first edit
        // truncated the log under a cursor that still counted the pre-degrade
        // op, and the undo then wrote the in-memory count, so the reopen put the
        // pre-degrade op on the redo stack and redo re-applied it to text that
        // already contained it ("kept" -> "keptkept"). After the reset the log
        // holds only the edit made since the degrade, so the only thing redo can
        // re-apply is the author's own undone edit.
        let mut reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "kept");
        assert_eq!(
            reopened.history(),
            (0, 1),
            "the log holds one post-degrade op and no older one"
        );
        assert!(reopened.redo(), "the author's own undone edit is redoable");
        assert_eq!(
            reopened.document().blocks[0].text,
            "kept!",
            "redo re-applies the typed edit exactly once, never the pre-degrade op"
        );
        assert!(!reopened.redo(), "nothing else is on the redo stack");
        assert_eq!(reopened.document().blocks[0].text, "kept!");
    }

    /// FIX 3: a message that has no pending write (the degraded-open warning, a
    /// failed export) is dismissed by the strip's click, not retried: retrying
    /// writes state the author never changed and never clears the message.
    #[test]
    fn a_message_with_no_pending_write_is_dismissed_not_retried() {
        let path = temp_db_path();
        let mut session = open(&path);
        assert!(session.insert("draft"));

        session.last_error = Some("export failed — no such directory".into());
        assert!(
            !session.save_failed,
            "the export failure is not a save failure"
        );
        assert!(session.retry_or_dismiss(), "a click clears the message");
        assert!(session.last_error.is_none(), "the strip goes away");
        assert!(!session.save_failed);
        assert_eq!(session.history(), (1, 0), "nothing was rewritten");

        // A failed write is the other class: the same click retries it, and it
        // stays until a write lands.
        session.save_failed = true;
        session.last_error = Some("save failed — editing disabled: disk full".into());
        assert!(session.retry_or_dismiss(), "the retry lands");
        assert!(!session.save_failed);
        assert!(session.last_error.is_none());
    }

    // --- Task 12: annotations ------------------------------------------------
    #[test]
    fn placement_composition_adds_the_body_left_edge() {
        // place_marginalia reports body-relative x and page y; the page
        // position adds only the body's left edge.
        let marginal = MarginalPlacement {
            annotation_id: uuid::Uuid::new_v4(),
            x: 652.0,
            y: 324.0,
            anchor_y: 324.0,
        };
        assert_eq!(margin_origin(200.0, &marginal), Pos2::new(852.0, 324.0));

        // Footnote Y is measured from the painter's top and spans the body.
        let footnote = FootnotePlacement {
            annotation_id: uuid::Uuid::new_v4(),
            number: 2,
            y: 900.0,
        };
        assert_eq!(
            footnote_origin(50.0, 200.0, &footnote),
            Pos2::new(200.0, 950.0)
        );
    }

    #[test]
    fn a_footnote_superscript_clears_the_anchored_word() {
        let mut renderer = GlyphRenderer::new(LayoutConfig::default());
        let body_width = renderer.config().body_width;
        let block = Block {
            id: uuid::Uuid::new_v4(),
            kind: crate::model::BlockKind::Paragraph,
            text: "Craft, not content: the making is the meaning.".to_string(),
            spans: Vec::new(),
        };
        let layout = renderer.layout(&block);
        let body_left = 200.0;
        let block_top = 60.0;

        // The mark for "Craft" (0..5) belongs after the "t", at the pen of byte
        // 5 — not at the pen of byte 0, which is inside the word's ink.
        let anchor = Anchor::range(block.id, 0, 5);
        let origin = superscript_origin(&layout, &anchor, body_left, block_top, body_width, 0.0)
            .expect("a footnote on a laid-out block has a superscript position");
        let start_x = caret_x(&layout, 0);
        let end_x = caret_x(&layout, 5);
        assert!(
            end_x > start_x,
            "the anchor covers more than its first glyph"
        );
        assert!(
            (origin.x - (body_left + end_x)).abs() < 0.01,
            "the number sits at the range end, got {} wanted {}",
            origin.x,
            body_left + end_x
        );
        assert!(
            origin.x >= body_left + start_x,
            "the number must not fall back inside the word"
        );

        // Raised off the line top so a 0.62× numeral reads as a superscript.
        let line = layout.lines[0];
        assert!((origin.y - (block_top + line.y + line.height * 0.1)).abs() < 0.01);
        assert!(origin.y > block_top + line.y, "the mark is lifted");

        // An anchor ending exactly at a visual line's end — which is where the
        // last word of every wrapped line ends, since cosmic-text drops the
        // trailing wrap space — resolves to that line and puts the mark after
        // its last glyph. It must not fall back to the line start and print
        // over the first word.
        let long = "Typography is the craft of endowing human language with a durable \
visual form, and thus with an independent existence. Its heartwood is calligraphy."
            .to_string();
        let wrapped = Block {
            id: uuid::Uuid::new_v4(),
            kind: crate::model::BlockKind::Paragraph,
            text: long,
            spans: Vec::new(),
        };
        let layout = renderer.layout(&wrapped);
        assert!(layout.lines.len() >= 2, "the paragraph wraps");
        let first_line = layout.lines[0];
        let anchor = Anchor::range(wrapped.id, first_line.start, first_line.end);
        let origin = superscript_origin(&layout, &anchor, body_left, block_top, body_width, 0.0)
            .expect("position");
        let last_glyph_x = caret_x(&layout, first_line.end - 1);
        let line_end_x = caret_x(&layout, first_line.end);
        assert!(
            line_end_x > last_glyph_x,
            "the pen after the line's last glyph is past its ink"
        );
        assert!(
            (origin.x - (body_left + line_end_x)).abs() < 0.01,
            "an anchor ending at a line end sits after the last glyph, got {} wanted {}",
            origin.x,
            body_left + line_end_x
        );
        assert!(
            origin.x > body_left + last_glyph_x,
            "the mark clears the last glyph instead of overprinting it"
        );
        assert!(
            origin.x > body_left + caret_x(&layout, first_line.start),
            "the mark is not back at the line start"
        );
        assert!(
            (origin.y - (block_top + first_line.y + first_line.height * 0.1)).abs() < 0.01,
            "the mark stays on the line the anchor ends on"
        );
        assert!(
            origin.x <= body_left + body_width,
            "the mark is clamped inside the measure"
        );
    }

    #[test]
    fn adding_a_note_at_a_caret_anchors_its_word_and_persists() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("The quick brown fox");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 6,
        });

        let id = session
            .add_annotation(AnnotationKind::Note)
            .expect("a note over the word at the caret");
        assert_eq!(session.document().annotations.len(), 1);
        let annotation = &session.document().annotations[0];
        assert_eq!(annotation.id, id);
        assert_eq!(annotation.kind, AnnotationKind::Note);
        assert_eq!(
            annotation.anchor,
            Anchor::range(block_id, 4, 9),
            "a caret anchors the word around it"
        );
        assert_eq!(annotation.body, "", "a new note starts as a bare highlight");
        drop(session);

        let reopened = open(&path);
        assert_eq!(reopened.document().annotations.len(), 1);
        assert_eq!(
            reopened.document().annotations[0]
                .anchor
                .byte_range()
                .unwrap()
                .0,
            4
        );
    }

    #[test]
    fn removing_a_note_clears_its_highlight_and_undo_restores_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("The quick brown fox");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 6,
        });
        let id = session
            .add_annotation(AnnotationKind::Note)
            .expect("a note to remove");

        assert!(session.remove_annotation(id), "the removal persists");
        assert!(
            session.document().annotations.is_empty(),
            "the highlight is gone"
        );

        session.undo();
        assert_eq!(
            session.document().annotations.len(),
            1,
            "undo restores the note and its highlight"
        );
    }

    #[test]
    fn marginalia_anchor_to_the_block_and_notes_to_the_selection() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("The quick brown fox");
        let block_id = first_block_id(&session);

        // Drag backwards: the anchor still comes out in document order.
        session.set_caret(Cursor {
            block_id,
            offset: 15,
        });
        session.select_to(Cursor {
            block_id,
            offset: 4,
        });

        // A marginal anchors to the whole block, selection or not.
        let id = session
            .add_annotation(AnnotationKind::Marginal)
            .expect("a marginal over the selection");
        let annotation = session
            .document()
            .annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .expect("the new annotation");
        assert_eq!(annotation.kind, AnnotationKind::Marginal);
        assert_eq!(
            annotation.anchor,
            Anchor::block(block_id),
            "marginalia anchor to the whole block, not the selection"
        );

        // A note anchors to the selected range, in document order.
        let id = session
            .add_annotation(AnnotationKind::Note)
            .expect("a note over the selection");
        let annotation = session
            .document()
            .annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .expect("the new note");
        assert_eq!(annotation.kind, AnnotationKind::Note);
        assert_eq!(
            annotation.anchor,
            Anchor::range(block_id, 4, 15),
            "the selected bytes become the anchor, never reversed"
        );
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("hello world");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 6,
        });
        session.select_to(Cursor {
            block_id,
            offset: 11,
        });

        assert!(session.insert("there"));
        assert_eq!(session.document().blocks[0].text, "hello there");
        assert_eq!(session.caret.offset, 11);
        assert!(session.selection.is_none());
    }

    #[test]
    fn backspace_over_a_selection_deletes_the_range() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("hello world");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 5,
        });
        session.select_to(Cursor {
            block_id,
            offset: 11,
        });

        assert!(session.backspace());
        assert_eq!(session.document().blocks[0].text, "hello");
        assert_eq!(session.caret.offset, 5);
    }

    #[test]
    fn a_note_anchor_survives_typing_above_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("The quick brown fox");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 6,
        });
        let id = session
            .add_annotation(AnnotationKind::Footnote)
            .expect("a footnote on 'quick'");

        // Type at the very start: every later anchor rebases by the insert.
        session.set_caret(Cursor {
            block_id,
            offset: 0,
        });
        assert!(session.insert("Very "));

        let annotation = session
            .document()
            .annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .expect("the footnote survives the edit");
        assert_eq!(
            annotation.anchor,
            Anchor::range(block_id, 9, 14),
            "the anchor moved with its word, keeping the same range"
        );
    }

    #[test]
    fn annotation_add_is_refused_after_a_failed_save() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("abc");
        session.save_failed = true;
        session.last_error = Some("save failed — editing disabled: disk full".into());

        assert!(session.add_annotation(AnnotationKind::Footnote).is_none());
        assert!(session.document().annotations.is_empty());
    }

    #[test]
    fn annotation_bodies_truncate_multibyte_text_on_a_char_boundary() {
        assert_eq!(shorten("abc", 8), "abc");
        assert_eq!(shorten("abcdef", 3), "abc…");
        assert_eq!(shorten("éééé", 2), "éé…");
        assert_eq!(
            default_annotation_body(&AnnotationKind::Footnote, "  word  "),
            "Footnote on “word”"
        );
    }

    /// An authored body is document state, not a display default: it has to
    /// survive close/reopen through the op log, and undo after the reopen must
    /// put the generated body back.
    #[test]
    fn an_authored_body_persists_across_reopen() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("The quick brown fox");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 6,
        });
        let id = session
            .add_annotation(AnnotationKind::Note)
            .expect("a note on 'quick'");
        let generated = session.document().annotations[0].body.clone();
        assert_eq!(generated, "", "a new note starts as a bare highlight");

        assert!(
            session.set_annotation_body(id, "Show that the fox is not asleep."),
            "the body edit is applied and persisted"
        );
        assert_eq!(
            session.document().annotations[0].body,
            "Show that the fox is not asleep."
        );
        drop(session);

        let mut reopened = open(&path);
        assert_eq!(reopened.document().annotations.len(), 1);
        assert_eq!(
            reopened.document().annotations[0].body,
            "Show that the fox is not asleep.",
            "the authored body comes back from the head document"
        );

        // The edit is in the log, so undo after the reopen restores the body
        // the note was created with.
        assert!(reopened.undo(), "the body edit is undoable after a reopen");
        assert_eq!(reopened.document().annotations[0].body, generated);
        assert!(reopened.redo(), "and redoable");
        assert_eq!(
            reopened.document().annotations[0].body,
            "Show that the fox is not asleep."
        );
    }

    /// The popover commits keystroke by keystroke, so an empty body is a real
    /// state (the author cleared the note) rather than a reference to the
    /// default, and it has to round trip as such. A note with no body is a
    /// plain highlight, so it survives.
    #[test]
    fn clearing_a_note_body_persists_as_an_empty_body() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("word");
        let id = session
            .add_annotation(AnnotationKind::Note)
            .expect("a note on 'word'");

        assert!(session.set_annotation_body(id, ""));
        assert_eq!(session.document().annotations[0].body, "");
        drop(session);

        let reopened = open(&path);
        assert_eq!(reopened.document().annotations[0].body, "");
    }

    /// Clearing a marginal's body removes it: an empty margin entry has
    /// nothing to say, so it goes away instead of lingering.
    #[test]
    fn clearing_a_marginal_body_removes_the_marginal() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("word");
        let id = session
            .add_annotation(AnnotationKind::Marginal)
            .expect("a marginal on 'word'");

        assert!(session.set_annotation_body(id, ""));
        assert!(session.document().annotations.is_empty());
    }

    /// Backspacing the space before a footnote re-anchors the footnote instead
    /// of dropping it: a caret on whitespace anchors a point, and that point
    /// sits on the delete's end boundary.
    #[test]
    fn backspacing_before_a_point_anchored_footnote_reanchors_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("a  b");
        let block_id = first_block_id(&session);
        // Caret in the second space: `word_at` returns a point anchor there.
        session.set_caret(Cursor {
            block_id,
            offset: 2,
        });
        let id = session
            .add_annotation(AnnotationKind::Footnote)
            .expect("a footnote on the whitespace");
        assert_eq!(
            session.document().annotations[0].anchor.byte_range(),
            Some((2, 2)),
            "a caret on whitespace anchors a point"
        );

        // Backspace deletes the first space [1, 2); the point at 2 is the
        // delete's end boundary and must close up to 1, not drop.
        assert!(session.backspace());
        assert_eq!(session.document().blocks[0].text, "a b");
        assert_eq!(
            session.document().annotations.len(),
            1,
            "the footnote survives"
        );
        assert_eq!(
            session.document().annotations[0].anchor.byte_range(),
            Some((1, 1)),
            "the point anchor closes up by one"
        );
        assert_eq!(session.document().annotations[0].id, id);
    }

    // --- search -----------------------------------------------------------

    #[test]
    fn find_matches_is_case_insensitive_and_byte_safe() {
        let mut document = Document::new("find");
        document.blocks[0].text = "The quick Quick fox".to_string();
        let second = Block {
            id: BlockId::new_v4(),
            kind: BlockKind::Paragraph,
            text: "end —quick".to_string(),
            spans: vec![],
        };
        document.blocks.push(second.clone());

        let matches = find_matches(&document, "quick");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0], (document.blocks[0].id, 4, 9));
        assert_eq!(matches[1], (document.blocks[0].id, 10, 15));
        // "—" is a three-byte codepoint, so "quick" starts at byte 7.
        assert_eq!(matches[2], (second.id, 7, 12));
    }

    #[test]
    fn find_matches_reports_overlaps_and_nothing_for_an_empty_needle() {
        let mut document = Document::new("find");
        document.blocks[0].text = "aaa".to_string();
        let matches = find_matches(&document, "aa");
        assert_eq!(
            matches,
            vec![(document.blocks[0].id, 0, 2), (document.blocks[0].id, 1, 3),]
        );
        assert!(find_matches(&document, "").is_empty());
    }

    #[test]
    fn find_matches_skips_blocks_shorter_than_the_needle() {
        // A fresh document ends on an empty block; searching must not panic on
        // a block with fewer chars than the query.
        let mut document = Document::new("find");
        document.blocks[0].text = "find me".to_string();
        let empty = Block {
            id: BlockId::new_v4(),
            kind: BlockKind::Paragraph,
            text: String::new(),
            spans: vec![],
        };
        document.blocks.push(empty);

        let matches = find_matches(&document, "find");
        assert_eq!(matches, vec![(document.blocks[0].id, 0, 4)]);
        assert!(
            find_matches(&document, "zz").is_empty(),
            "no match, no panic"
        );
    }

    // --- file, export and snapshot commands --------------------------------

    fn file_chord() -> egui::Modifiers {
        egui::Modifiers {
            command: true,
            shift: true,
            ..egui::Modifiers::default()
        }
    }

    const FILE_KEYS: [egui::Key; 4] = [egui::Key::E, egui::Key::B, egui::Key::S, egui::Key::R];

    /// A chord claimed by two maps acts twice, so the file chords have to be
    /// unclaimed by both the mode map and the editor's own map.
    #[test]
    fn file_chords_are_disjoint_from_the_mode_and_editor_maps() {
        let chord = file_chord();
        assert_eq!(
            file_command_for_key(egui::Key::E, chord),
            Some(FileCommand::ExportMarkdown)
        );
        assert_eq!(
            file_command_for_key(egui::Key::B, chord),
            Some(FileCommand::ExportHtml)
        );
        assert_eq!(
            file_command_for_key(egui::Key::S, chord),
            Some(FileCommand::SnapshotNow)
        );
        assert_eq!(
            file_command_for_key(egui::Key::R, chord),
            Some(FileCommand::RestoreSnapshot)
        );

        // A bare key is prose and a command-only chord is the editor's: Ctrl-S
        // must not silently write a file, and Ctrl-Z stays undo.
        for key in FILE_KEYS {
            assert_eq!(file_command_for_key(key, egui::Modifiers::NONE), None);
            assert_eq!(
                file_command_for_key(
                    key,
                    egui::Modifiers {
                        command: true,
                        ..egui::Modifiers::default()
                    }
                ),
                None
            );
            assert_eq!(mode_for_key(key, chord), None, "{key:?} is not a mode");
            assert_eq!(
                crate::render::egui_render::command_for_key(key, chord),
                None,
                "{key:?} is not an editing command"
            );
        }
        // Every mode chord is still a mode, and never a file command.
        for key in [egui::Key::H, egui::Key::M, egui::Key::P, egui::Key::T] {
            assert!(mode_for_key(key, chord).is_some());
            assert_eq!(file_command_for_key(key, chord), None);
        }
    }

    #[test]
    fn only_pressed_non_repeating_events_are_file_chords() {
        let chord = file_chord();
        let key = |pressed: bool, repeat: bool| egui::Event::Key {
            key: egui::Key::E,
            physical_key: None,
            pressed,
            repeat,
            modifiers: chord,
        };
        assert_eq!(
            file_commands_for_events(&[key(true, false)]),
            vec![FileCommand::ExportMarkdown]
        );
        assert!(
            file_commands_for_events(&[key(true, true)]).is_empty(),
            "a held key exports once, not once per repeat"
        );
        assert!(
            file_commands_for_events(&[key(false, false)]).is_empty(),
            "the key-up is not a second export"
        );
        assert!(file_commands_for_events(&[egui::Event::Text("e".to_string())]).is_empty());
    }

    #[test]
    fn exports_land_beside_the_source_and_fall_back_to_the_cwd() {
        assert_eq!(
            export_path(Path::new("/tmp/notes/draft.kame"), "md"),
            PathBuf::from("/tmp/notes/draft.md")
        );
        assert_eq!(
            export_path(Path::new("/tmp/notes/draft.kame"), "html"),
            PathBuf::from("/tmp/notes/draft.html")
        );
        // A bare name has no directory part, so it resolves in the working
        // directory — the fallback for a session whose path is just a file name.
        assert_eq!(
            export_path(Path::new("draft.kame"), "md"),
            PathBuf::from("draft.md")
        );
        // A path with no stem at all still exports to something nameable.
        assert_eq!(
            export_path(Path::new(".."), "md"),
            PathBuf::from("document.md")
        );
    }

    #[test]
    fn exporting_markdown_writes_the_document_beside_the_database() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("A paragraph.");
        session.split();
        session.insert("Another one.");
        let expected =
            crate::export::to_markdown_with(session.document(), &session.published_metadata());
        assert!(expected.contains("A paragraph."));
        assert!(expected.contains("Another one."));

        let written = session.export_markdown().expect("a markdown export");
        assert_eq!(written, path.with_extension("md"));
        let contents = std::fs::read_to_string(&written).expect("read the export");
        assert_eq!(
            contents, expected,
            "the file is exactly to_markdown's output, not a re-render"
        );
        assert!(session.last_error.is_none());
    }

    #[test]
    fn exporting_html_writes_a_page_beside_the_database() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("A paragraph.");
        let expected =
            crate::export::to_html_with(session.document(), &session.published_metadata());

        let written = session.export_html().expect("an html export");
        assert_eq!(written, path.with_extension("html"));
        let contents = std::fs::read_to_string(&written).expect("read the export");
        assert_eq!(contents, expected, "the file is exactly to_html's output");
        assert!(contents.starts_with("<!DOCTYPE html>"), "{contents:.40}");
        assert!(contents.contains("<p>A paragraph.</p>"), "{contents}");
    }

    #[test]
    fn a_failed_export_reports_itself_without_locking_the_document() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("some text");
        // A directory where the Markdown file belongs makes the write fail.
        std::fs::create_dir(path.with_extension("md")).unwrap();

        assert_eq!(session.export_markdown(), None);
        let error = session.last_error.as_deref().expect("an export error");
        assert!(error.starts_with("export failed"), "{error}");
        assert!(
            !session.save_failed,
            "an export failure is not a save failure: the document is still stored"
        );
        assert!(session.insert(" more"), "editing carries on");
        assert!(
            session.export_html().is_some(),
            "the other format still writes"
        );
        assert!(
            session.last_error.is_none(),
            "a later successful export clears the old export error"
        );
    }

    #[test]
    fn snapshots_list_newest_first_with_their_labels() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("first draft");
        let first = session.snapshot(Some("  first  ")).expect("a snapshot");
        session.insert(" again");
        let second = session.snapshot(Some("")).expect("a second snapshot");

        let listed = session.snapshots().expect("list the snapshots");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, second);
        assert_eq!(listed[1].id, first);
        assert_eq!(
            listed[1].label.as_deref(),
            Some("first"),
            "labels are trimmed and kept"
        );
        assert_eq!(listed[0].label, None, "an empty label stores no label");
    }

    #[test]
    fn restoring_a_snapshot_drops_the_edits_made_after_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("the version I kept");
        let id = session.snapshot(Some("kept")).expect("a snapshot");
        session.insert(" and the text I regret");
        assert!(session.document().blocks[0].text.ends_with("regret"));

        assert!(session.restore(id));
        assert_eq!(session.document().blocks[0].text, "the version I kept");
        // The snapshot is the undo point, so the restore is not itself an edit:
        // there is nothing to undo afterwards.
        assert_eq!(session.history(), (0, 0));
        assert!(!session.undo(), "the restore is not undone by Ctrl-Z");
        drop(session);

        // The head, not just the view, moved: reopen sees the restored text.
        let mut reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "the version I kept");
        assert_eq!(reopened.history(), (0, 0), "the old op log was dropped");
        assert_eq!(
            reopened.db.load_ops().unwrap().len(),
            0,
            "the log describes a document that no longer exists"
        );
        assert_eq!(
            reopened.snapshots().unwrap().len(),
            1,
            "restoring keeps the snapshots it read from"
        );
        // The restored document is a clean start for new edits.
        assert!(reopened.insert("!"));
        assert_eq!(reopened.history(), (1, 0));
    }

    #[test]
    fn restoring_a_missing_snapshot_leaves_the_document_alone() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("still here");

        assert!(!session.restore(9_999), "there is no such snapshot");
        assert_eq!(session.document().blocks[0].text, "still here");
        let error = session.last_error.as_deref().expect("a restore error");
        assert!(error.starts_with("restore failed"), "{error}");
        assert!(!session.save_failed, "a failed read is not a failed write");
        assert!(session.insert("!"), "editing carries on");
    }

    #[test]
    fn snapshot_ages_are_coarse_and_never_negative() {
        assert_eq!(format_age(1_000, 1_000), "just now");
        assert_eq!(format_age(1_000, 1_059), "just now");
        assert_eq!(format_age(1_000, 1_060), "1m ago");
        assert_eq!(format_age(1_000, 1_000 + 59 * 60), "59m ago");
        assert_eq!(format_age(1_000, 1_000 + 3_600), "1h ago");
        assert_eq!(format_age(1_000, 1_000 + 86_400), "1d ago");
        // A clock that moved backwards must not read as a future snapshot.
        assert_eq!(format_age(2_000, 1_000), "just now");
    }

    #[test]
    fn snapshot_lines_carry_the_id_the_age_and_the_label() {
        let labelled = SnapshotMeta {
            id: 7,
            timestamp: 1_000,
            label: Some("before the cut".to_string()),
        };
        assert_eq!(
            describe_snapshot(&labelled, 1_000),
            "#7 · just now · before the cut"
        );
        let blank = SnapshotMeta {
            id: 8,
            timestamp: 1_000,
            label: Some("   ".to_string()),
        };
        assert_eq!(
            describe_snapshot(&blank, 1_000),
            "#8 · just now · unlabelled"
        );
    }

    #[test]
    fn the_restore_picker_preselects_the_newest_and_clamps_its_selection() {
        let metas = vec![
            SnapshotMeta {
                id: 3,
                timestamp: 30,
                label: Some("c".to_string()),
            },
            SnapshotMeta {
                id: 2,
                timestamp: 20,
                label: Some("b".to_string()),
            },
            SnapshotMeta {
                id: 1,
                timestamp: 10,
                label: None,
            },
        ];
        let mut picker = RestorePicker::new(metas);
        assert_eq!(picker.selected, Some(3), "the newest is preselected");
        assert!(
            !picker.confirming,
            "the picker opens on its chooser step, never on the confirmation"
        );
        picker.move_selection(true);
        assert_eq!(picker.selected, Some(2));
        picker.move_selection(true);
        picker.move_selection(true);
        assert_eq!(picker.selected, Some(1), "the last row is the floor");
        picker.move_selection(false);
        assert_eq!(picker.selected, Some(2));
    }
}
