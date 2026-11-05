//! kame — a distraction-free editor for long-form writing.
//!
//! The binary opens (or creates) one `.kame` SQLite file, loads its document,
//! lays every block out with cosmic-text, rasterizes each block to an egui
//! texture, and paints the result. Keyboard input is translated into editor
//! commands; every command is persisted through the durable op log so history
//! survives a restart.

mod external;
#[cfg(target_os = "macos")]
mod macos;
mod notebook;
mod publish;
mod references;

use kame_core::links::{
    image_file, resolve_open_path, split_document_target, stored_image_path, way_within_notebook,
};
use kame_core::references::{cited_key, with_citation_spans};
use kame_core::session::{is_markdown_path, Clipped, LinkUnder, Session};
use kame_core::{edit, model, storage};
use kame_typeset::{layout, render};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use eframe::egui;
use egui::{Align2, Color32, FontId, Id, Order, Pos2, Rect, Sense, TextureHandle, Vec2};

use kame_check::{Background, Exceptions, Finding, FindingKind};

use crate::edit::Cursor;
use crate::layout::reading_order;
use crate::layout::{
    heading_size_for, place_footnotes, place_marginalia, BlockLayout, FootnotePlacement,
    LayoutConfig, MarginalPlacement, DEFAULT_BODY_SIZE, FOOTNOTE_GAP, FOOTNOTE_LINE_HEIGHT,
    FOOTNOTE_MARK_EM,
};
use crate::model::{
    bare_link_ranges, is_script_url, list_labels, list_positions, AcceptedMark, Anchor, Annotation,
    AnnotationId, AnnotationKind, Block, BlockId, BlockKind, Document, ImageAlign, Metadata,
    SpanStyle,
};
use crate::render::egui_render::{
    adjacent_line, caret_line, caret_x, command_for_event, install_fonts, offset_at, range_rects,
    x_on_line, AnnotationTextureCache, Command, EscapePrefix, GlyphRenderer, LayoutCache,
    TextureCache,
};
use crate::storage::{Db, SnapshotMeta};

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
    /// The wavy lines under a likely misspelling, and under anything else the checker marks.
    spelling_mark: Color32,
    grammar_mark: Color32,
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
        spelling_mark: Color32::from_rgb(190, 66, 56),
        grammar_mark: Color32::from_rgb(64, 110, 180),
        apparatus_ink: [86, 79, 69],
        caret_ink: Color32::from_rgb(176, 58, 46),
        selection_fill: Color32::from_rgba_unmultiplied_const(58, 78, 112, 62),
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
        spelling_mark: Color32::from_rgb(226, 112, 98),
        grammar_mark: Color32::from_rgb(120, 162, 224),
        apparatus_ink: [168, 160, 148],
        caret_ink: Color32::from_rgb(226, 92, 70),
        selection_fill: Color32::from_rgba_unmultiplied_const(92, 112, 150, 66),
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
/// The document to start with: the one named, or else the last one, if it is still
/// there. Opening a missing path would create it, which only a request should do.
/// The arguments for the site tool, if that's what was asked for: `kame site ...`, or this program
/// run under the name `kame-site`, as through a link to it. A file named `site` can be opened as
/// `./site`.
fn site_arguments(program: &Path, args: &[String]) -> Option<Vec<String>> {
    if program.file_stem().is_some_and(|name| name == "kame-site") {
        return Some(args.to_vec());
    }
    (args.first().map(String::as_str) == Some("site")).then(|| args[1..].to_vec())
}

/// The file named on the command line, as a full path. A relative one would later be read against
/// whichever document was open, so coming back to it could open or make the wrong file.
fn launch_path(argument: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let path = PathBuf::from(argument?);
    Some(std::path::absolute(&path).unwrap_or(path))
}

fn document_to_open(argument: Option<PathBuf>, last: Option<PathBuf>) -> Option<PathBuf> {
    argument.or_else(|| last.filter(|path| path.is_file()))
}

/// Takes the lock that marks this as the first running kame, holding it until the
/// process exits. Without a lock to take there is nothing to tell apart, so that counts
/// as being first.
fn hold_instance_lock(path: &Path) -> bool {
    let Ok(file) = std::fs::File::create(path) else {
        return true;
    };
    match file.try_lock() {
        Ok(()) => {
            std::mem::forget(file);
            true
        }
        Err(std::fs::TryLockError::WouldBlock) => false,
        Err(std::fs::TryLockError::Error(_)) => true,
    }
}

/// Turns the Tab key into a typed tab. Shift-Tab has nothing to do in a block that takes
/// tabs, so it goes too, or it would still move the keyboard focus.
fn tabs_to_text(events: &mut Vec<egui::Event>) {
    events.retain_mut(|event| {
        let egui::Event::Key {
            key: egui::Key::Tab,
            pressed: true,
            modifiers,
            ..
        } = event
        else {
            return true;
        };
        if modifiers.shift_only() {
            return false;
        }
        if !modifiers.any() {
            *event = egui::Event::Text("\t".to_string());
        }
        true
    });
}

/// The document to reopen on the next launch that names none.
struct LastDocument {
    path: Option<PathBuf>,
    /// The session path `path` was last taken from.
    seen: Option<PathBuf>,
}

impl LastDocument {
    fn new(path: Option<PathBuf>) -> LastDocument {
        LastDocument { path, seen: None }
    }

    /// Notes the file behind the session. Returns whether that changed anything to save.
    fn note(&mut self, current: &Path) -> bool {
        if self.seen.as_deref() == Some(current) {
            return false;
        }
        // A launch can name a relative path, which means nothing to the next one.
        self.path = Some(std::path::absolute(current).unwrap_or_else(|_| current.to_path_buf()));
        self.seen = Some(current.to_path_buf());
        true
    }

    /// A new document has no file to come back to, so the next launch starts empty.
    fn forget(&mut self) {
        self.path = None;
        self.seen = None;
    }

    /// Whether this is the document at `path`, which may have gone.
    fn is(&self, path: &Path) -> bool {
        self.path.as_deref().is_some_and(|at| same_place(at, path))
    }

    /// The document moved, so the next launch finds it there. Returns whether that changed
    /// anything to save.
    fn moved(&mut self, from: &Path, to: &Path) -> bool {
        if !self.is(from) {
            return false;
        }
        self.path = Some(std::path::absolute(to).unwrap_or_else(|_| to.to_path_buf()));
        self.seen = Some(to.to_path_buf());
        true
    }
}

/// The Tab presses in `events`, in order: true for Tab and false for Shift-Tab.
fn tab_presses(events: &[egui::Event]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|event| match event {
            egui::Event::Key {
                key: egui::Key::Tab,
                pressed: true,
                modifiers,
                ..
            } if !modifiers.any() => Some(true),
            egui::Event::Key {
                key: egui::Key::Tab,
                pressed: true,
                modifiers,
                ..
            } if modifiers.shift_only() => Some(false),
            _ => None,
        })
        .collect()
}

/// Where to return to after following a document link.
#[derive(Clone, Debug, PartialEq)]
struct BackStop {
    /// The document, or `None` for one with no file yet.
    path: Option<PathBuf>,
    caret: Cursor,
    scroll: f32,
}

/// The way back through followed links, most recent last.
#[derive(Debug, Default)]
struct BackStack {
    stops: Vec<BackStop>,
}

impl BackStack {
    /// Forgets stops in a document that's gone.
    fn forget(&mut self, path: &Path) {
        self.stops
            .retain(|stop| !stop.path.as_deref().is_some_and(|at| same_place(at, path)));
    }

    /// Stops in a document that has moved now lead to where it went.
    fn moved(&mut self, from: &Path, to: &Path) {
        for path in self.stops.iter_mut().filter_map(|stop| stop.path.as_mut()) {
            follow_move(path, from, to);
        }
    }

    fn push(&mut self, stop: BackStop) {
        if self.stops.len() == MAX_BACK_STOPS {
            self.stops.remove(0);
        }
        self.stops.push(stop);
    }

    fn pop(&mut self) -> Option<BackStop> {
        self.stops.pop()
    }
}

/// What to say when another machine's words have arrived through the logs.
fn logs_notice(from: Option<&str>, notes: usize) -> String {
    let from = from.unwrap_or("another machine");
    match notes {
        0 => format!("merged from {from}"),
        1 => format!("merged from {from}, with 1 note to look at"),
        notes => format!("merged from {from}, with {notes} notes to look at"),
    }
}

/// What a merge says in the status row. A note is a sentence both versions changed.
fn merged_notice(notes: usize) -> String {
    match notes {
        0 => "merged".to_string(),
        1 => "merged, with 1 note to look at".to_string(),
        notes => format!("merged, with {notes} notes to look at"),
    }
}

/// Whether two paths are one document, however they were written: a launch can name a relative
/// path, and a later open a full one. Two documents with no file are taken as the same one, as
/// there is only ever one open.
fn same_document(a: Option<&Path>, b: Option<&Path>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        },
        _ => false,
    }
}

/// Whether two paths name one place, whether or not there's anything there now, as after a move or
/// a delete. The folder's what's compared, since it's still there to ask.
fn same_place(a: &Path, b: &Path) -> bool {
    let place = |path: &Path| Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?));
    same_document(Some(a), Some(b)) || place(a).is_some_and(|place_a| Some(place_a) == place(b))
}

/// A path naming what has moved now names where it went.
fn follow_move(path: &mut PathBuf, from: &Path, to: &Path) {
    if same_place(path, from) {
        *path = to.to_path_buf();
    }
}

/// The help dialog's id, which stays the same so that its scroll position does.
fn help_id() -> Id {
    Id::unique("kame-help")
}

/// The most the help dialog is wide, and the room it leaves at the window's edges.
const HELP_MAX_WIDTH: f32 = 1040.0;
const HELP_EDGE: f32 = 16.0;
/// What a column of the help needs to be worth having another, and what its dialog takes beyond
/// the rows and the window's margin: the title bar and the Close button below the rows.
const HELP_COLUMN_WIDTH: f32 = 340.0;
const HELP_CHROME: f32 = 72.0;

/// What the help dialog lists, as keys and what they do, in the order shown.
const HELP_ROWS: &[(&str, &str)] = &[
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
    ("Ctrl-A", "select the block; again, the whole document"),
    (
        "Enter",
        "split a block; on an empty list item, end the list",
    ),
    (
        "Ctrl-N",
        "a new document, asking first if there is unsaved work",
    ),
    ("Ctrl-O", "open a document, replacing this one"),
    ("Ctrl-S", "save (prompts for a file if unsaved)"),
    ("Ctrl-Shift-S", "snapshot now"),
    ("Ctrl-Shift-R", "restore a snapshot"),
    ("Ctrl-Shift-E", "export Markdown"),
    ("Ctrl-Shift-B", "export HTML"),
    ("Ctrl-F", "find"),
    (
        "F7",
        "select the next spelling or grammar mark, when checking is on",
    ),
    ("Ctrl-[", "back to where a link was followed from"),
    ("Ctrl-L", "jump to a document, in a notebook"),
    ("Ctrl-Shift-L", "a notebook's contents page"),
    ("Ctrl-R", "search a notebook"),
    ("Ctrl-Shift-O", "a notebook's browser"),
    (
        "right-click in the browser",
        "rename, move or delete a document",
    ),
    ("Ctrl-Shift-N", "note (highlight) on the selection or word"),
    ("Ctrl-Shift-F", "footnote on the selection or word"),
    ("Ctrl-Shift-C", "cite a reference, in a notebook"),
    ("Ctrl-Shift-1/2/3", "heading level 1 / 2 / 3"),
    ("Ctrl-Shift-0", "back to body text"),
    ("#  ##  ###", "heading, typed at the start of a line"),
    ("-  *  1.", "list item, typed at the start of a line"),
    (">", "block quote, typed at the start of a line"),
    ("|", "prose block, typed at the start of a line"),
    (
        "```",
        "code block: three backticks and a language if wanted, then Enter",
    ),
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
];

/// Ctrl-[, or Cmd-[ on a Mac, as browsers on a Mac use.
fn is_back_chord(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key: egui::Key::OpenBracket,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } if modifiers.command && !modifiers.shift
    )
}

/// Removes the keys egui moves the focus on, for when there is nowhere useful to move it.
fn drop_tabs(events: &mut Vec<egui::Event>) {
    events.retain(|event| {
        !matches!(
            event,
            egui::Event::Key {
                key: egui::Key::Tab,
                pressed: true,
                modifiers,
                ..
            } if !modifiers.any() || modifiers.shift_only()
        )
    });
}

/// Whether a press at `at` is the second of a double click: soon after the last click and
/// about where it was. A drag from there selects by word.
fn is_second_press(
    click: Option<(std::time::Instant, Pos2)>,
    now: std::time::Instant,
    at: Pos2,
) -> bool {
    click.is_some_and(|(when, where_it_was)| {
        now.saturating_duration_since(when).as_secs_f32() <= DOUBLE_CLICK_SECONDS
            && where_it_was.distance(at) <= DOUBLE_CLICK_REACH
    })
}

/// Ctrl-N, or Cmd-N on a Mac. With Shift it adds a note.
fn is_new_chord(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key: egui::Key::N,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } if modifiers.command && !modifiers.shift
    )
}

/// How many links back can be followed.
const MAX_BACK_STOPS: usize = 64;
/// How long after a click a second press still counts as a double click.
const DOUBLE_CLICK_SECONDS: f32 = 0.4;
/// How far from a click a second press can land and still count as a double click.
const DOUBLE_CLICK_REACH: f32 = 6.0;
/// How often to look for the open file having been replaced on disk.
const FILE_CHECK_SECONDS: f32 = 1.0;
/// How often to look for a sync tool's conflict copies beside the open file.
const COPY_CHECK_SECONDS: f32 = 5.0;
/// How often to look at a logged document's revision logs.
const LOG_CHECK_SECONDS: f32 = 3.0;
/// How long a pause in typing has to be before a revision is written or a merge shows up.
const LOG_PAUSE_SECONDS: f32 = 5.0;
/// A gap between frames this long means the machine slept or the window was hidden.
const LOG_RETURN_SECONDS: f32 = 10.0;
/// How far up from the bottom of its line a document link's underline sits, as a share
/// of the line's height.
const DOCUMENT_LINK_UNDERLINE: f32 = 0.2;
/// The colour of [`LINK_COLOR`] as egui takes it.
const DOCUMENT_LINK_INK: Color32 = Color32::from_rgb(59, 130, 246);
/// How much of the status row's free space a message can take from the title.
const STATUS_MESSAGE_SHARE: f32 = 0.65;
/// How much of the page to keep in view past the caret when following it.
const CARET_ROOM: f32 = 48.0;
/// How much of a code line to keep in view past the caret when scrolling it sideways.
const CODE_CARET_ROOM: f32 = 24.0;
/// The most a list in Document Properties shows before it scrolls.
const PROPERTIES_LIST_HEIGHT: f32 = 130.0;
/// The shape of a mark's wavy underline.
const WAVE_STEP: f32 = 2.0;
const WAVE_HEIGHT: f32 = 1.0;
/// How far up from the bottom of its line a mark sits, as a share of the line's height.
const MARK_LIFT: f32 = 0.16;
/// How far outside a code block's column to draw the edge that says it carries on.
const CODE_EDGE_GAP: f32 = 4.0;
/// The least time between writes of the caret and scroll to the document.
const POSITION_SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// A horizontal rule's width, as a share of the measure.
const RULE_WIDTH: f32 = 0.25;
/// Gap between a list marker's right edge and its item's text.
const LIST_MARKER_GAP: f32 = 6.0;
/// The bullet at each level of nesting, starting over past the last.
const LIST_BULLETS: [&str; 3] = ["•", "◦", "▪"];
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

/// A native file dialog starting in `directory`, if there is one.
fn file_dialog(title: &str, directory: Option<&Path>) -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new().set_title(title);
    match directory {
        Some(directory) => dialog.set_directory(directory),
        None => dialog,
    }
}

/// The native file browser for a Markdown file to import, or `None` when the
/// author cancels it. Blocks the frame like [`pick_save_path`], which is correct
/// for a modal dialog.
fn pick_markdown_path(directory: Option<&Path>) -> Option<PathBuf> {
    file_dialog("Import Markdown", directory)
        .add_filter("Markdown", &["md", "markdown"])
        .pick_file()
}

/// The native file browser for a document to open, or `None` when the author
/// cancels it.
fn pick_open_path(directory: Option<&Path>) -> Option<PathBuf> {
    file_dialog("Open", directory)
        .add_filter("kame or Markdown", &["kame", "md", "markdown"])
        .add_filter("kame document", &["kame"])
        .add_filter("Markdown", &["md", "markdown"])
        .pick_file()
}

/// The native file browser for the folder of a new notebook, or `None` if it is cancelled.
fn pick_notebook_folder(directory: Option<&Path>) -> Option<PathBuf> {
    file_dialog("New Notebook", directory).pick_folder()
}

/// The native file browser for a document to link to, or `None` if it is cancelled.
fn pick_document_link_path(directory: Option<&Path>) -> Option<PathBuf> {
    file_dialog("Link to Document", directory)
        .add_filter("kame or Markdown", &["kame", "md", "markdown"])
        .add_filter("kame document", &["kame"])
        .add_filter("Markdown", &["md", "markdown"])
        .pick_file()
}

/// The native file browser for an image to insert, or `None` if it is cancelled.
fn pick_image_path(directory: Option<&Path>) -> Option<PathBuf> {
    file_dialog("Insert Image", directory)
        .add_filter(
            "Image",
            &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"],
        )
        .pick_file()
}

/// The native "save as" browser for an export, starting at `suggestion`, or `None`
/// when the author cancels it.
fn pick_export_path(
    title: &str,
    kind: &str,
    extension: &str,
    suggestion: &Path,
) -> Option<PathBuf> {
    let directory = suggestion
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty());
    let mut dialog = file_dialog(title, directory).add_filter(kind, &[extension]);
    if let Some(name) = suggestion.file_name().and_then(|name| name.to_str()) {
        dialog = dialog.set_file_name(name);
    }
    dialog.save_file()
}

/// The native "save as" file browser, or `None` when the author cancels it.
///
/// `rfd` is built with the XDG desktop portal backend (not GTK), so it asks the
/// compositor's file chooser — the GTK one on this desktop — over D-Bus. That
/// is a full file browser: directories, navigation and a file-name field, the
/// same dialog a GTK application shows. The synchronous API blocks the frame
/// thread while the (modal) dialog is up, which is correct for a save prompt.
#[cfg(not(test))]
fn pick_save_path(suggested_name: &str, directory: Option<&Path>) -> Option<PathBuf> {
    file_dialog("Save kame document", directory)
        .add_filter("kame document", &["kame"])
        .set_file_name(suggested_name)
        .save_file()
}

/// A test has no one to answer a native dialog, so asking for one is a failure.
#[cfg(test)]
fn pick_save_path(_suggested_name: &str, _directory: Option<&Path>) -> Option<PathBuf> {
    panic!("a test asked for the save dialog");
}

/// The link a right-click landed on.
#[derive(Clone, Debug, PartialEq)]
struct LinkMenu {
    /// A link on text, which can be edited. An image's link has only `url`.
    link: Option<LinkUnder>,
    url: String,
}

/// The link prompt (C-k): the range being linked, its words and the URL being typed.
struct LinkPrompt {
    block_id: BlockId,
    start: usize,
    end: usize,
    /// The words the link shows. Left empty, it shows its URL.
    words: String,
    /// The URL being typed, pre-filled with the existing link's URL, if any.
    url: String,
    /// Whether there's a link already, rather than one being added.
    editing: bool,
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
/// The space either side of the prose once a narrow window has collapsed the page,
/// which is also where punctuation hangs.
const COLLAPSED_GUTTER: f32 = 20.0;

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
        ((width - 2.0 * COLLAPSED_GUTTER).max(MIN_MEASURE), 0.0)
    }
}

/// Page geometry for one frame, all as offsets from the painter's top-left.
///
/// `config` is the measured config for this frame: `page_measure` has already
/// chosen the measure and margin column, so this only positions the body — a
/// fixed quarter-gutter from the left when the page fits, and
/// [`COLLAPSED_GUTTER`] when it has collapsed to fill a narrow window.
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
        COLLAPSED_GUTTER
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

/// The scroll offset that brings `top..bottom` on page into view with a little room
/// around it, moving no further than it has to, or `None` if it is already in view.
fn scroll_to_show(
    top: f32,
    bottom: f32,
    scroll: f32,
    viewport_height: f32,
    content_height: f32,
) -> Option<f32> {
    let room = CARET_ROOM.min(viewport_height * 0.25);
    let target = if bottom + room > scroll + viewport_height {
        bottom + room - viewport_height
    } else if top - room < scroll {
        top - room
    } else {
        return None;
    };
    let furthest = (content_height - viewport_height).max(0.0);
    Some(target.clamp(0.0, furthest)).filter(|target| (target - scroll).abs() > 0.5)
}

/// Where to look for a line for the caret once the wheel has taken it off the page: as far in
/// as the keys would leave it. `None` if it's still on the page.
fn caret_aim_after_scroll(
    caret_top: f32,
    caret_bottom: f32,
    page_top: f32,
    page_bottom: f32,
) -> Option<f32> {
    let room = CARET_ROOM.min((page_bottom - page_top) * 0.25);
    if caret_top < page_top {
        Some(page_top + room)
    } else if caret_bottom > page_bottom {
        Some(page_bottom - room)
    } else {
        None
    }
}

/// The sideways scroll that keeps `x` in a code block's column, moving no further than needed.
fn code_scroll_to_show(scroll: f32, x: f32, column: f32) -> f32 {
    let room = CODE_CARET_ROOM.min(column * 0.25);
    if x < scroll + room {
        (x - room).max(0.0)
    } else if x > scroll + column - room {
        x + room - column
    } else {
        scroll
    }
}

/// A code block's sideways scroll, kept to what it can actually scroll.
fn clamp_code_scroll(scroll: f32, column: f32, content: f32) -> f32 {
    scroll.clamp(0.0, (content - column).max(0.0))
}

/// Whether a code block has more to see past its left and right edges.
fn code_overflow_edges(scroll: f32, column: f32, content: f32) -> (bool, bool) {
    // Within a pixel of the end counts as the end, since scrolling is in fractions.
    (scroll > 0.5, content - column - scroll > 0.5)
}

/// What the checker has said about the document, and what it's been asked.
#[derive(Default)]
struct Marks {
    worker: Option<Background>,
    /// Each block as it was checked, and what was found in it.
    checked: HashMap<BlockId, (Block, Vec<Finding>)>,
    asked: HashMap<BlockId, Block>,
    /// What's waiting to be asked about, and since when, to wait until typing stops.
    waiting: Option<(u64, std::time::Instant)>,
}

/// How long after typing stops to check what changed.
const CHECK_AFTER_SECONDS: f32 = 0.5;

/// The findings to draw for `block`. None while the block has changed since it was checked, whose
/// offsets would be wrong, and none for a word that the caret is at the end of, which may
/// not be finished.
fn visible_marks(
    marks: &Marks,
    exceptions: &Exceptions,
    block: &Block,
    caret: Option<usize>,
) -> Vec<Finding> {
    let Some((checked, findings)) = marks.checked.get(&block.id) else {
        return Vec::new();
    };
    if checked.text != block.text {
        return Vec::new();
    }
    findings
        .iter()
        .filter(|finding| exceptions.allows(finding, &block.text))
        .filter(|finding| caret != Some(finding.end))
        .cloned()
        .collect()
}

/// The finding that `offset` falls in.
fn mark_at(findings: &[Finding], offset: usize) -> Option<&Finding> {
    findings
        .iter()
        .find(|finding| finding.start <= offset && offset <= finding.end)
}

/// The next mark after a caret, going round to the first, as (place in the list).
fn next_mark(marks: &[(usize, usize)], caret: (usize, usize)) -> Option<usize> {
    marks
        .iter()
        .position(|mark| *mark > caret)
        .or((!marks.is_empty()).then_some(0))
}

/// The points of a wavy underline from `left` to `right`.
fn wavy_line(left: f32, right: f32, y: f32) -> Vec<Pos2> {
    let mut points = vec![Pos2::new(left, y)];
    let mut x = left;
    let mut up = true;
    while x < right {
        x = (x + WAVE_STEP).min(right);
        let height = if up { -WAVE_HEIGHT } else { WAVE_HEIGHT };
        points.push(Pos2::new(x, y + height));
        up = !up;
    }
    points
}

/// Whether the find bar should close because its field lost the keyboard. Going back to the text
/// closes it. Its own buttons don't, and nor does switching to another window.
fn find_bar_closes(lost_focus: bool, window_focused: bool, pointer_in_bar: bool) -> bool {
    lost_focus && window_focused && !pointer_in_bar
}

/// What the checker sees of an annotation, if it reads this kind: marginals and footnotes, which
/// are prose for the reader. A note is the writer's own thinking.
fn annotation_block(annotation: &Annotation) -> Option<Block> {
    let prose = matches!(
        annotation.kind,
        AnnotationKind::Marginal | AnnotationKind::Footnote
    );
    (prose && !annotation.body.trim().is_empty()).then(|| Block {
        id: annotation.id,
        kind: BlockKind::Paragraph,
        text: annotation.body.clone(),
        spans: Vec::new(),
    })
}

/// An annotation's body with part of it replaced, or `None` if the range doesn't fit.
fn body_with_replacement(body: &str, start: usize, end: usize, with: &str) -> Option<String> {
    let (before, after) = (body.get(..start)?, body.get(end..)?);
    (start <= end).then(|| format!("{before}{with}{after}"))
}

/// Where in the text a footnote belongs: just after what it's anchored to.
fn footnote_return_cursor(annotation: &Annotation) -> Option<Cursor> {
    (annotation.kind == AnnotationKind::Footnote).then(|| Cursor {
        block_id: annotation.anchor.block_id(),
        offset: annotation.anchor.byte_range().map_or(0, |(_, end)| end),
    })
}

/// Where the number of a foot entry is drawn, given the layout of its text and its origin.
fn footnote_number_rect(layout: &BlockLayout, number: usize, origin: Pos2) -> Option<Rect> {
    // A digit is tiny, so the target reaches a little past it, as a superscript's does.
    let shown = format!("{number}.").len();
    range_rects(layout, 0, shown)
        .first()
        .map(|rect| rect.translate(origin.to_vec2()).expand(2.0))
}

/// How far a page key moves: a window's height, less two lines so the eye keeps its place.
fn page_step(viewport_height: f32, line_height: f32) -> f32 {
    (viewport_height - 2.0 * line_height).max(line_height)
}

/// The scroll offset after a page key: the page moves with the caret, so the caret stays
/// where it was on screen, unless the caret was off the page or the page has run out.
fn scroll_after_page_key(
    scroll: f32,
    step: f32,
    caret_top: f32,
    caret_bottom: f32,
    viewport_height: f32,
    content_height: f32,
) -> f32 {
    let furthest = (content_height - viewport_height).max(0.0);
    let moved = (scroll + step).clamp(0.0, furthest);
    scroll_to_show(
        caret_top,
        caret_bottom,
        moved,
        viewport_height,
        content_height,
    )
    .unwrap_or(moved)
}

/// Case-insensitive matches of `needle` across every block, in document order,
/// as `(block_id, byte_start, byte_end)` in the original text's byte offsets.
/// Overlapping matches are reported; an empty needle matches nothing.
fn find_matches(document: &Document, needle: &str) -> Vec<(BlockId, usize, usize)> {
    document
        .blocks
        .iter()
        .flat_map(|block| {
            kame_core::text::find_in(&block.text, needle)
                .into_iter()
                .map(|(start, end)| (block.id, start, end))
        })
        .collect()
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
    /// A notice about to be set is news even if it reads the same as the last one, as a second
    /// merge's does. Without this, the change it reports would clear it at once.
    fn announced(&mut self) {
        self.seen = None;
    }

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
    Site,
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
            egui::Key::S => (BarMenu::Site, "s"),
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
/// What a mark's right-click menu can do.
enum MarkAction {
    Replace(String),
    Accept,
    AddToDocument,
    AddToMine,
    TurnOffRule,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum MenuAction {
    NewNotebook,
    ToggleBrowser,
    NotebookHome,
    Jump,
    SearchNotebook,
    MergeConflictCopies,
    Back,
    CompactHistory,
    LinkDocument,
    SiteBuild,
    SiteSync,
    New,
    Open,
    OpenPath,
    Save,
    SaveAs,
    ImportMarkdown,
    Properties,
    File(FileCommand),
    Annotate(AnnotationKind),
    Cite,
    References,
    TypeSize(f32),
    ToggleDark,
    ToggleHyphenate,
    ToggleCheck,
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

/// Whether a block of kind `current` is in the right-click menu's `style`. A
/// list item's depth and a code block's language are not styles of their own,
/// so they don't take a block out of the row it belongs to.
fn is_block_style(current: &BlockKind, style: &BlockKind) -> bool {
    match (current, style) {
        (BlockKind::Heading { level: a }, BlockKind::Heading { level: b }) => a == b,
        (BlockKind::ListItem { ordered: a, .. }, BlockKind::ListItem { ordered: b, .. }) => a == b,
        _ => std::mem::discriminant(current) == std::mem::discriminant(style),
    }
}

/// The tags typed into the properties dialog.
fn parse_tags(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

/// Sorts a word list for reading, ignoring case.
fn sort_words(words: &mut [String]) {
    words.sort_by_key(|word| word.to_lowercase());
}

/// Adds what was typed to a word list, which may be several words separated by commas, keeping it
/// sorted and without repeats.
fn add_words(words: &mut Vec<String>, typed: &str) {
    for word in parse_tags(typed) {
        if !words.iter().any(|known| known.eq_ignore_ascii_case(&word)) {
            words.push(word);
        }
    }
    sort_words(words);
}

/// The document's metadata as the dialog leaves it.
fn metadata_from_prompt(prompt: &MetadataPrompt) -> Metadata {
    Metadata {
        title: prompt.title.trim().to_string(),
        author: prompt.author.trim().to_string(),
        created: prompt.created.trim().to_string(),
        published: prompt.published.trim().to_string(),
        tags: parse_tags(&prompt.tags),
        publish: prompt.publish,
        words: {
            let mut words = prompt.words.clone();
            add_words(&mut words, &prompt.new_word);
            words
        },
        accepted: prompt.accepted.clone(),
    }
}

/// The Document Properties dialog: its fields as text until they are saved.
struct MetadataPrompt {
    title: String,
    author: String,
    created: String,
    published: String,
    tags: String,
    /// This document's words for the spell checker, and one being typed to add.
    words: Vec<String>,
    new_word: String,
    /// The marks accepted in this document, which can be taken back here.
    accepted: Vec<AcceptedMark>,
    /// Make this author the one new documents start with.
    default_author: bool,
    /// Whether a notebook's site publishes this document.
    publish: bool,
    /// Set for the frame the dialog opens on, so the first field takes the
    /// keyboard.
    focus: bool,
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
    /// The prompt is up for a new document, rather than for closing the window.
    for_new: bool,
}

impl CloseFlow {
    /// Records a close request and says whether to cancel it. `unsaved` is
    /// whether the document has work with nowhere to go.
    fn on_close_request(&mut self, unsaved: bool) -> bool {
        if self.confirmed || !unsaved {
            return false;
        }
        self.prompting = true;
        // Closing the window is the larger request, so it takes over the prompt.
        self.for_new = false;
        true
    }

    /// Puts up the prompt on the way to a new document.
    fn request_new(&mut self) {
        self.prompting = true;
        self.for_new = true;
    }

    /// The author chose to keep editing.
    fn cancel(&mut self) {
        self.prompting = false;
        self.for_new = false;
    }

    /// The author chose to leave the document. Returns true if that is for a new
    /// document, which leaves the window's own close guarded as before.
    fn finish(&mut self) -> bool {
        if self.for_new {
            self.cancel();
            true
        } else {
            self.confirm();
            false
        }
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
    /// The document open when kame last ran, to reopen on a launch that names none.
    #[serde(default)]
    last_document: Option<PathBuf>,
    /// The author new documents start with.
    #[serde(default)]
    author: String,
    /// Whether justified paragraphs can hyphenate words.
    #[serde(default = "default_hyphenate")]
    hyphenate: bool,
    /// Whether to mark likely mistakes on the page.
    #[serde(default)]
    check_spelling: bool,
    /// Words that are right in any document.
    #[serde(default)]
    words: Vec<String>,
    /// The checker's rules to leave out.
    #[serde(default)]
    rules_off: Vec<String>,
    /// What this machine's revision logs are called, chosen when first needed and then kept.
    #[serde(default)]
    device: String,
    /// Whether a notebook's browser is showing, and which of its folders are open.
    #[serde(default)]
    browser_open: bool,
    #[serde(default)]
    browser_folders: Vec<String>,
}

fn default_body_size() -> f32 {
    DEFAULT_BODY_SIZE
}

fn default_hyphenate() -> bool {
    true
}

impl Default for KameConfig {
    fn default() -> Self {
        KameConfig {
            body_size: DEFAULT_BODY_SIZE,
            dark: false,
            last_document: None,
            author: String::new(),
            hyphenate: true,
            check_spelling: false,
            words: Vec::new(),
            rules_off: Vec::new(),
            device: String::new(),
            browser_open: false,
            browser_folders: Vec::new(),
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

/// The app's icon. eframe otherwise sets its own at runtime, which on macOS replaces
/// the bundle's in the Dock and the application switcher.
fn app_icon() -> egui::IconData {
    let image = image::load_from_memory(include_bytes!("../packaging/kame.png"))
        .map(|image| image.into_rgba8());
    match image {
        Ok(image) => egui::IconData {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        },
        // Empty means no icon of its own, leaving the bundle's in place.
        Err(_) => egui::IconData::default(),
    }
}

fn main() -> eframe::Result {
    // The site tool comes with the app, which opens no window for it.
    let mut arguments = std::env::args_os().map(|arg| arg.to_string_lossy().into_owned());
    let program = arguments.next().unwrap_or_default();
    let rest: Vec<String> = arguments.collect();
    if let Some(site) = site_arguments(Path::new(&program), &rest) {
        std::process::exit(kame_site::run(&site));
    }
    // The type size and theme are kame preferences, read once here and applied
    // to every document the app opens.
    let config = load_config();
    let body_size = config.body_size;
    let dark = config.dark;
    let config_hyphenate = config.hyphenate;
    let config_check = config.check_spelling;
    let config_words = config.words.clone();
    let config_rules_off = config.rules_off.clone();
    let config_device = config.device.clone();
    let config_browser_open = config.browser_open;
    let config_browser_folders = config.browser_folders.clone();
    // Two windows on one file would overwrite each other's edits, so only the first
    // running kame goes back to the last document.
    let first = config_dir().is_none_or(|dir| {
        let _ = std::fs::create_dir_all(&dir);
        hold_instance_lock(&dir.join("instance.lock"))
    });
    let mut last_document = config.last_document;
    let default_author = config.author;
    let config_author = default_author.clone();
    let last = last_document.clone().filter(|_| first);

    // With an argument, open that file, and with none go back to the last document.
    // Failing that, an anonymous document: the author starts typing at once and the
    // first save prompts for a file.
    // A notebook's folder opens its contents page, or just its browser if it has none.
    let argument = launch_path(std::env::args_os().nth(1));
    let folder_notebook = argument
        .as_deref()
        .filter(|path| path.is_dir())
        .and_then(|folder| kame_notebook::Notebook::open(folder).ok());
    let (argument, last) = match &folder_notebook {
        Some(notebook) => (notebook.index(), None),
        None => (argument, last),
    };
    let (session, notice) = match document_to_open(argument, last) {
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
            let _ = kame_sync::logs::build_from_logs(&path);
            match Session::open_any(&path) {
                Ok(opened) => {
                    // An import has no file yet, so it is like a new document.
                    if opened.0.is_anonymous() {
                        last_document = None;
                        save_config(&KameConfig {
                            body_size,
                            dark,
                            last_document: None,
                            author: config_author.clone(),
                            hyphenate: config_hyphenate,
                            check_spelling: config_check,
                            words: config_words.clone(),
                            rules_off: config_rules_off.clone(),
                            device: config_device.clone(),
                            browser_open: config_browser_open,
                            browser_folders: config_browser_folders.clone(),
                        });
                    }
                    opened
                }
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

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 860.0])
            .with_min_inner_size([560.0, 360.0])
            .with_icon(app_icon())
            .with_title("kame"),
        ..Default::default()
    };

    let app_creator: eframe::AppCreator<'_> = Box::new(move |cc| {
        let mut app = KameApp::new(
            cc,
            session,
            notice,
            KameConfig {
                body_size,
                dark,
                last_document,
                author: default_author,
                hyphenate: config_hyphenate,
                check_spelling: config_check,
                words: config_words,
                rules_off: config_rules_off,
                device: config_device,
                browser_open: config_browser_open,
                browser_folders: config_browser_folders,
            },
        );
        if let Some(notebook) = folder_notebook {
            if app.session.is_anonymous() {
                app.start_in_notebook(notebook);
            }
        }
        Ok(Box::new(app))
    });

    // On macOS, kame's application events have to be in place before the event loop
    // runs, or a file opened from Finder at launch is missed, so it owns the loop there.
    #[cfg(target_os = "macos")]
    {
        let event_loop =
            winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event().build()?;
        macos::install_app_events();
        let mut app = eframe::create_native("kame", native_options, app_creator, &event_loop);
        event_loop.run_app(&mut app)?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    eframe::run_native("kame", native_options, app_creator)
}

// --- session ---------------------------------------------------------------

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
    sideways: &[f32],
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
        if nearest.is_none_or(|(_, best)| distance < best) {
            nearest = Some((index, distance));
        }
    }
    let (index, _) = nearest?;
    let block = &blocks[index];
    let scrolled = sideways.get(index).copied().unwrap_or(0.0);
    let x = (pos.x - body_left - layouts[index].left_indent + scrolled).max(0.0);
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
    if let BlockKind::Image { link, .. } = &block.kind {
        return link.clone();
    }
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
    bare_link_ranges(block)
        .into_iter()
        .find(|(start, end)| cursor.offset >= *start && cursor.offset < *end)
        .map(|(start, end)| block.text[start..end].to_string())
}

/// The target of the document link under `cursor`, if there is one.
fn document_link_at(blocks: &[Block], cursor: &Cursor) -> Option<String> {
    let block = blocks.iter().find(|block| block.id == cursor.block_id)?;
    block.spans.iter().find_map(|span| match &span.style {
        SpanStyle::DocumentLink { target }
            if span.start <= cursor.offset && cursor.offset < span.end =>
        {
            Some(target.clone())
        }
        _ => None,
    })
}

/// The target for a link to the file at `path` from the document at `document`. Within the
/// notebook at `root` it is the way from the document's folder, which holds when the notebook
/// moves. Anywhere else it is written from `~` when under home, and whole otherwise.
fn document_link_target(
    path: &Path,
    home: Option<&Path>,
    document: Option<&Path>,
    root: Option<&Path>,
) -> String {
    if let Some(way) = document
        .zip(root)
        .and_then(|(document, root)| way_within_notebook(path, document, root))
    {
        return way;
    }
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// What to call the document at `path` in a link to it: its title, or its file name if
/// it has none worth using or can't be read. A Markdown file is left unread.
fn document_title(path: &Path) -> String {
    let stem = || {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    };
    if is_markdown_path(path) {
        return stem();
    }
    let Ok(db) = Db::open(path) else {
        return stem();
    };
    db.load_metadata()
        .ok()
        .flatten()
        .map(|metadata| metadata.title)
        .into_iter()
        .chain(db.load_document().ok().map(|document| document.title))
        .find(|title| !title.is_empty() && title != "Untitled")
        .unwrap_or_else(stem)
}

/// Whether a clicked link may be handed to the system's opener. Local files stay
/// openable, so a link can reach a PDF or an image. A leading dash would be read as
/// an option to the opener rather than as the link.
fn may_open(url: &str) -> bool {
    let url = url.trim_start();
    !url.is_empty() && !url.starts_with('-') && !is_script_url(url)
}

/// Opens `url` in the system browser, best-effort: a link that cannot launch is
/// never worth an error dialog in the editor.
fn open_in_browser(url: &str) {
    if !may_open(url) {
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
    let set_off = |kind: &BlockKind| {
        matches!(
            kind,
            BlockKind::BlockQuote | BlockKind::Code { .. } | BlockKind::Prose | BlockKind::Rule
        )
    };
    let ratio = match (block, next) {
        (BlockKind::Heading { .. }, _) => 0.4,
        (_, BlockKind::Heading { level }) => match level {
            0 | 1 => 2.6,
            2 => 2.2,
            _ => 1.7,
        },
        (block, next) if std::mem::discriminant(block) == std::mem::discriminant(next) => 1.0,
        (block, next) if set_off(block) || set_off(next) => 1.4,
        _ => 1.0,
    };
    BLOCK_GAP * ratio
}

/// Minimum height of a block, so an empty paragraph still has a line to click
/// and a caret to show.
fn block_height(
    block: &Block,
    layout: &BlockLayout,
    config: &LayoutConfig,
    document: Option<&Path>,
) -> f32 {
    if let BlockKind::Image { path, .. } = &block.kind {
        return image_size(&image_file(path, document), config.body_width).y;
    }
    let minimum = match block.kind {
        crate::model::BlockKind::Heading { level } => heading_size_for(config, level) * 1.4,
        _ => config.body_line_height,
    };
    layout.height.max(minimum)
}

/// Height of the placeholder drawn for an image that cannot be read.
const MISSING_IMAGE_HEIGHT: f32 = 72.0;

/// How large an image is drawn in a column `width` across. One that fits is shown at its own
/// size, a pixel to a point, as a browser would, and a wider one is scaled down to the column,
/// keeping its shape. A file that can't be read (or reports a zero dimension) takes the whole
/// column at a short placeholder's height, which says what is missing without leaving a blank
/// stretch of page.
/// Where an image `width` wide starts, in a column starting at `left` and `column` wide.
fn image_left(left: f32, column: f32, width: f32, align: ImageAlign) -> f32 {
    match align {
        ImageAlign::Left => left,
        ImageAlign::Center => left + (column - width) / 2.0,
        ImageAlign::Right => left + column - width,
    }
}

fn image_size(path: &Path, width: f32) -> Vec2 {
    match image::image_dimensions(path) {
        Ok((w, h)) if w > 0 && h > 0 => {
            let (w, h) = (w as f32, h as f32);
            let scale = (width / w).min(1.0);
            Vec2::new(w * scale, h * scale)
        }
        _ => Vec2::new(width, MISSING_IMAGE_HEIGHT),
    }
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

/// Where to scroll so a footnote at page position `footnote_y` lands a third of
/// the way down the window, with the entries before it still in view.
fn footnote_scroll_offset(footnote_y: f32, viewport_height: f32) -> f32 {
    (footnote_y - viewport_height / 3.0).max(0.0)
}

/// The page position of the footnote whose superscript is under `pos`. `marks`
/// pairs each superscript's rectangle with its footnote's position. A digit is
/// tiny, so the target reaches a little past it.
fn footnote_mark_at(marks: &[(Rect, f32)], pos: Pos2) -> Option<f32> {
    marks
        .iter()
        .find(|(rect, _)| rect.expand(3.0).contains(pos))
        .map(|(_, footnote_y)| *footnote_y)
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
        Some(line) => (*line, x_on_line(layout, line, end)),
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
    /// Whether to mark likely mistakes, and what's been said not to mark.
    check_spelling: bool,
    my_words: Vec<String>,
    rules_off: Vec<String>,
    /// What this machine's revision logs are called, or empty until one is needed.
    device: String,
    /// The notebook this document is in, and what's showing of it.
    nb: notebook::NotebookUi,
    publishing: publish::Publishing,
    /// The notebook's references.
    refs: references::ReferencesUi,
    /// The open document's revision logs, if its folder has them turned on.
    logged: Option<kame_sync::logs::Logged>,
    /// The file `logged` is for, and whether a problem with its logs has been mentioned.
    logged_path: Option<PathBuf>,
    orphan_mentioned: bool,
    stranded_mentioned: bool,
    unreadable_mentioned: bool,
    next_log_check: std::time::Instant,
    last_typed: std::time::Instant,
    was_focused: bool,
    last_log_tick: std::time::Instant,
    marks: Marks,
    /// The mark the page's right-click menu is about, while it's open.
    mark_menu: Option<(BlockId, Finding)>,
    /// The link the page's menu was opened on, if any.
    link_menu: Option<LinkMenu>,
    /// What was last copied here, with its formatting. It's kept here and not in the session, so
    /// that it can be pasted in another document.
    clipped: Option<Clipped>,
    /// The image the page's menu was opened on. The menu's other rows change the caret's block,
    /// but a right-click doesn't move the caret.
    image_menu: Option<BlockId>,
    /// Whether justified paragraphs can hyphenate words.
    hyphenate: bool,
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
    /// A key moved the caret or changed the text, so the page should keep it in view.
    follow_caret: bool,
    /// How far each code block is scrolled sideways.
    code_scroll: HashMap<BlockId, f32>,
    /// Where the caret was last frame, to tell when it has moved.
    code_caret_seen: Option<Cursor>,
    /// A page key waiting for the page's geometry, as (down, select).
    page_key: Option<(bool, bool)>,
    /// The heading a followed document link named, until its document opens.
    pending_heading: Option<String>,
    escape_prefix: EscapePrefix,
    /// Whether the open note was opened while a dialog was up, and so is over it.
    note_over_dialog: bool,
    /// How far the page was scrolled on the last frame.
    page_scroll: f32,
    /// The caret and scroll last written to the document.
    saved_position: Option<(Cursor, f32)>,
    position_saved_at: std::time::Instant,
    last_document: LastDocument,
    /// The way back through followed links.
    back: BackStack,
    /// When and where the page was last clicked, for telling a double click's second press.
    last_click: Option<(std::time::Instant, Pos2)>,
    /// Where a drag by words began, while one is going.
    word_drag_start: Option<Cursor>,
    /// When the open file was last checked for having been replaced.
    last_file_check: std::time::Instant,
    /// Asking what to do about a file replaced on disk.
    replaced_prompt: bool,
    /// When to look for conflict copies next, and what was there last time.
    next_copy_check: std::time::Instant,
    conflict_copies: Vec<PathBuf>,
    /// The author new documents start with.
    default_author: String,
    /// The "Snapshot Now" label prompt, when it is open.
    snapshot_prompt: Option<SnapshotPrompt>,
    /// The "Open" path prompt (Ctrl-O), when it is open.
    open_prompt: Option<OpenPrompt>,
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
        preferences: KameConfig,
    ) -> KameApp {
        let KameConfig {
            body_size,
            dark,
            last_document,
            author: default_author,
            hyphenate,
            check_spelling,
            words,
            rules_off,
            device,
            browser_open,
            browser_folders,
        } = preferences;
        cc.egui_ctx.set_visuals(if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        install_fonts(&cc.egui_ctx);
        external::set_waker(cc.egui_ctx.clone());
        let mut session = session;
        session.apply_default_author(&default_author);
        KameApp {
            session,
            renderer: GlyphRenderer::new(LayoutConfig::default()),
            hyphenate,
            check_spelling,
            my_words: words,
            rules_off,
            device,
            nb: notebook::NotebookUi::new(browser_open, browser_folders),
            publishing: publish::Publishing::default(),
            refs: references::ReferencesUi::default(),
            logged: None,
            logged_path: None,
            orphan_mentioned: false,
            stranded_mentioned: false,
            unreadable_mentioned: false,
            next_log_check: std::time::Instant::now(),
            last_typed: std::time::Instant::now(),
            was_focused: true,
            last_log_tick: std::time::Instant::now(),
            marks: Marks::default(),
            mark_menu: None,
            link_menu: None,
            clipped: None,
            image_menu: None,
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
            follow_caret: false,
            code_scroll: HashMap::new(),
            code_caret_seen: None,
            page_key: None,
            pending_heading: None,
            escape_prefix: EscapePrefix::default(),
            note_over_dialog: false,
            page_scroll: 0.0,
            saved_position: None,
            position_saved_at: std::time::Instant::now(),
            last_document: LastDocument::new(last_document),
            back: BackStack::default(),
            last_click: None,
            word_drag_start: None,
            last_file_check: std::time::Instant::now(),
            replaced_prompt: false,
            next_copy_check: std::time::Instant::now(),
            conflict_copies: Vec::new(),
            default_author,
            snapshot_prompt: None,
            open_prompt: None,
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
        let before = self.open_note;
        // A marginal left as it was added, with nothing written in it, goes.
        if let Some(open) = before.filter(|open| Some(*open) != id) {
            self.session.discard_unwritten_marginal(open);
        }
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
        if self.open_note != before {
            self.note_over_dialog = self.open_note.is_some() && self.dialog_open();
        }
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
        self.other_dialog_open() || self.refs.window_open()
    }

    /// Whether a dialog other than the windows for references is up. These open over those.
    fn other_dialog_open(&self) -> bool {
        self.snapshot_prompt.is_some()
            || self.metadata_prompt.is_some()
            || self.open_prompt.is_some()
            || self.link_prompt.is_some()
            || self.restore_picker.is_some()
            || self.replaced_prompt
            || self.nb.list_open()
            || self.nb.file_op.is_some()
            || self.nb.move_report.is_some()
            || self.publishing.report.is_some()
    }

    /// Closes whichever file dialog is open, leaving a window for references that it was over.
    /// Returns whether one was closed, so Escape can fall through to the note popover only when
    /// it was not.
    fn dismiss_dialog(&mut self) -> bool {
        let open = self.dialog_open();
        if !self.other_dialog_open() {
            self.refs.dismiss();
        }
        self.nb.close_lists();
        self.nb.file_op = None;
        self.nb.move_report = None;
        self.metadata_prompt = None;
        self.snapshot_prompt = None;
        self.open_prompt = None;
        self.link_prompt = None;
        self.restore_picker = None;
        self.publishing.report = None;
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
                let suggestion = self.session.export_suggestion("md");
                let Some(path) = pick_export_path("Export Markdown", "Markdown", "md", &suggestion)
                else {
                    return;
                };
                let exported = self.session.export_markdown_to(&path, &self.refs.list);
                self.notice = exported.map(|path| {
                    self.export_notice(format!("exported Markdown to {}", path.display()), false)
                });
            }
            FileCommand::ExportHtml => {
                let suggestion = self.session.export_suggestion("html");
                let Some(path) = pick_export_path("Export HTML", "HTML", "html", &suggestion)
                else {
                    return;
                };
                let exported = self.session.export_html_to(&path, &self.refs.list);
                self.notice = exported.map(|path| {
                    self.export_notice(format!("exported HTML to {}", path.display()), true)
                });
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
            last_document: self.last_document.path.clone(),
            author: self.default_author.clone(),
            hyphenate: self.hyphenate,
            check_spelling: self.check_spelling,
            words: self.my_words.clone(),
            rules_off: self.rules_off.clone(),
            device: self.device.clone(),
            browser_open: self.nb.browser_open,
            browser_folders: self.nb.browser_folders.iter().cloned().collect(),
        });
    }

    /// Notes the current document as the one to come back to. Every way of getting a
    /// file behind the session passes through here, whether opening or a first save. A
    /// document with no file leaves the last one standing.
    fn remember_document(&mut self) {
        let Some(path) = self.session.path() else {
            return;
        };
        if self.last_document.note(path) {
            self.persist();
        }
    }

    /// Writes the caret and scroll to the document once they have changed, at most
    /// every couple of seconds so that scrolling does not write on every frame.
    fn keep_position(&mut self, ctx: &egui::Context) {
        let current = (self.session.caret, self.page_scroll);
        if self.session.is_anonymous() || self.saved_position == Some(current) {
            return;
        }
        let since = self.position_saved_at.elapsed();
        if since < POSITION_SAVE_INTERVAL {
            // An idle window draws no more frames, so ask for the one that writes it.
            ctx.request_repaint_after(POSITION_SAVE_INTERVAL - since);
            return;
        }
        self.save_position_now();
    }

    /// Writes the caret and scroll now, for when the document is about to be left.
    fn save_position_now(&mut self) {
        self.session.save_position(self.page_scroll);
        self.saved_position = Some((self.session.caret, self.page_scroll));
        self.position_saved_at = std::time::Instant::now();
    }

    /// Decides what Tab does this frame. A dialog keeps it, for moving between its
    /// fields. Anywhere else it would only walk the focus through the menu bar, so it
    /// types a tab in a block that takes one, moves a list item in or out, and otherwise
    /// does nothing.
    fn claim_tabs(&mut self, ctx: &egui::Context, events: &mut Vec<egui::Event>) {
        if self.dialog_open() || self.close_flow.prompting {
            return;
        }
        if self.prose_takes_tabs(ctx) {
            tabs_to_text(events);
            return;
        }
        if self.page_has_keyboard(ctx) {
            for indent in tab_presses(events) {
                self.session.indent_list_item(indent);
            }
        }
        drop_tabs(events);
    }

    /// Whether Tab goes to the page: the caret is in a verbatim block and nothing else
    /// has the keyboard, the same conditions `handle_input` gives the page its keys.
    fn prose_takes_tabs(&self, ctx: &egui::Context) -> bool {
        let caret = self.session.caret;
        let verbatim = self
            .session
            .block_index(caret.block_id)
            .is_some_and(|index| self.session.document().blocks[index].kind.is_verbatim());
        verbatim && self.page_has_keyboard(ctx)
    }

    /// Whether keys go to the page rather than a dialog, a menu or a field.
    fn page_has_keyboard(&self, ctx: &egui::Context) -> bool {
        !self.dialog_open()
            && !self.close_flow.prompting
            && !egui::Popup::is_any_open(ctx)
            && !ctx.memory(|memory| {
                memory.has_focus(note_body_edit_id()) || memory.has_focus(find_query_id())
            })
    }

    /// Ctrl-N: asks about unsaved work if there is any, then starts an empty document.
    fn request_new(&mut self) {
        // The text in a document that could not be written is only in this window.
        if self.session.save_failed && !self.session.newer {
            self.notice = Some("the last save failed, so retry it first".to_string());
            return;
        }
        if self.session.would_lose_work() {
            self.close_flow.request_new();
        } else {
            self.start_new();
        }
    }

    /// Replaces the current document with an empty one.
    fn start_new(&mut self) {
        if self.held_by_rename() {
            return;
        }
        self.save_position_now();
        match Session::anonymous() {
            Ok(session) => {
                self.adopt_session(session);
                self.notice = None;
                self.last_document.forget();
                self.persist();
            }
            Err(err) => {
                self.session.last_error = Some(format!("could not start a document — {err}"));
            }
        }
    }

    /// The close prompt was answered by leaving this document, so do what asked.
    fn finish_close(&mut self, ctx: &egui::Context) {
        if self.close_flow.finish() {
            self.start_new();
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Replaces the session, resetting the view state that pointed into the old
    /// document (open note, find bar, typewriter anchor, caches).
    fn adopt_session(&mut self, session: Session) {
        // The last of this document's words go in its logs before it's left. Not if `session` is
        // on the same file: it has read that file already, and a merge now would leave it stale,
        // so whoever is reopening a file syncs first.
        let left_untouched = self.on_untouched_fresh();
        if !same_document(self.session.path(), session.path()) {
            self.sync_logs();
        }
        self.logged = None;
        self.logged_path = None;
        self.orphan_mentioned = false;
        self.stranded_mentioned = false;
        self.unreadable_mentioned = false;
        self.session = session;
        self.clear_left_fresh(left_untouched);
        self.conflict_copies.clear();
        self.next_copy_check = std::time::Instant::now();
        self.session.apply_default_author(&self.default_author);
        self.layouts = LayoutCache::new();
        self.textures = TextureCache::new();
        self.annotation_textures = AnnotationTextureCache::new();
        self.open_note = None;
        self.open_note_pos = None;
        self.open_note_body = String::new();
        self.open_note_focus = false;
        self.find = None;
        self.refs.close_windows();
        self.typewriter_anchor = None;
        // The scroll area remembers its offset, which belonged to the old document.
        self.pending_scroll = Some(0.0);
        self.page_scroll = 0.0;
        self.saved_position = None;
        self.position_saved_at = std::time::Instant::now();
        self.reveal_caret = false;
        self.image_textures.clear();
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
        let mut lost_focus = false;
        let bar = egui::Panel::top("kame-find").show(ui, |ui| {
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut query)
                        .id(find_query_id())
                        // Enter steps through matches, and shouldn't take the keyboard away.
                        .return_key(None)
                        .hint_text("find")
                        .desired_width(220.0),
                );
                changed = field.changed();
                lost_focus = field.lost_focus();
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
        let (window_focused, pointer_in_bar) = ui.input(|input| {
            (
                input.viewport().focused.unwrap_or(true),
                input
                    .pointer
                    .latest_pos()
                    .is_some_and(|at| bar.response.rect.contains(at)),
            )
        });
        if close || find_bar_closes(lost_focus, window_focused, pointer_in_bar) {
            self.find = None;
            return;
        }
        if next {
            self.step_find(true);
        } else if prev {
            self.step_find(false);
        }
        // Its own controls take the keyboard from the field, and it should go straight back.
        if lost_focus {
            if let Some(find) = &mut self.find {
                find.focus = true;
            }
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
        // Ctrl-O opens the system's file browser, replacing the current document. It
        // is a window-level chord like find, so it works whether prose or a note has
        // focus, and it clears any other file dialog first.
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
            self.open_with_dialog();
        }
        self.handle_notebook_keys(ctx);
        let back_chord = ctx.input(|input| input.events.iter().any(is_back_chord));
        if back_chord && !self.dialog_open() && !self.close_flow.prompting {
            self.go_back();
        }
        // Every write checks too, so this is only for saying so promptly.
        if self.last_file_check.elapsed().as_secs_f32() >= FILE_CHECK_SECONDS {
            self.last_file_check = std::time::Instant::now();
            if self.session.check_file() {
                self.replaced_prompt = true;
            }
        }
        if !self.session.is_anonymous() {
            ctx.request_repaint_after(std::time::Duration::from_secs_f32(FILE_CHECK_SECONDS));
        }
        self.look_for_conflict_copies();
        let new_chord = ctx.input(|input| input.events.iter().any(is_new_chord));
        if new_chord && !self.dialog_open() && !self.close_flow.prompting {
            self.request_new();
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
        // Ctrl-Shift-C cites, on the page only: in a dialog's field or a note's box the keys are
        // that field's.
        let cite_chord = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::C,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if modifiers.command && modifiers.shift
                )
            })
        });
        if cite_chord && self.page_has_keyboard(ctx) {
            self.begin_cite();
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
        if ctx.input(|input| input.key_pressed(egui::Key::F7)) && self.check_spelling {
            self.go_to_next_mark();
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
            // Escape closes help, then the find bar, then a note over a dialog, then a dialog, then
            // the note.
            if self.show_help {
                self.show_help = false;
                return;
            }
            if self.find.take().is_some() {
                return;
            }
            // A note opened over a dialog is on top of it, so it goes first.
            if self.open_note.is_some() && self.note_over_dialog {
                self.set_open_note(None);
                return;
            }
            if !self.dismiss_dialog() {
                // With nothing to close, Escape is a prefix for the next key.
                if self.open_note.is_none() {
                    self.escape_prefix.arm(std::time::Instant::now());
                }
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
        let now = std::time::Instant::now();
        for event in &events {
            let prefixed = self.escape_prefix.command_for(event, now);
            let Some(command) = prefixed.or_else(|| command_for_event(event)) else {
                continue;
            };
            self.follow_caret = true;
            match command {
                Command::DeleteWordBack => {
                    self.session.delete_word_back();
                }
                Command::MoveDocumentStart | Command::MoveDocumentEnd => {
                    let end = matches!(command, Command::MoveDocumentEnd);
                    if let Some(edge) = self.session.document_edge(end) {
                        self.session.set_caret(edge);
                        self.reveal_caret = true;
                    }
                }
                Command::Insert(text) => {
                    if self.session.insert(&text) {
                        self.session.apply_markdown_shortcut();
                        self.session.apply_inline_markdown();
                        self.session.apply_markdown_link();
                        if text == "]" {
                            self.note_closed_citation();
                        }
                    }
                }
                Command::Paste(text) => {
                    self.session.paste_clipped(&text, self.clipped.as_ref());
                }
                // With nothing selected these leave the clipboard as it was.
                Command::Copy | Command::Cut => {
                    if let Some(clipped) = self.session.clipped() {
                        ctx.copy_text(clipped.text.clone());
                        self.clipped = Some(clipped);
                        if matches!(command, Command::Cut) {
                            self.session.backspace();
                        }
                    }
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
                Command::LineBreak => {
                    self.session.line_break();
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
                Command::MovePageUp => self.page_key = Some((false, false)),
                Command::SelectPageUp => self.page_key = Some((false, true)),
                Command::MovePageDown => self.page_key = Some((true, false)),
                Command::SelectPageDown => self.page_key = Some((true, true)),
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
        let x = caret_x(&layout, caret.offset);

        // A block with no lines, which is any empty one, falls through to crossing.
        if let Some(target) = adjacent_line(&layout, caret.offset, down) {
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
        let title = match &self.nb.notebook {
            Some(notebook) => format!("{} — {}", notebook.title(), self.session.display_title()),
            None => self.session.display_title(),
        };
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
        // Applied after the panel closes: some of these block on a native dialog
        // or replace the session the panel is reading.
        let mut action: Option<MenuAction> = None;
        let pending_menu = self.pending_menu.take();

        egui::Panel::top("kame-status").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                let file_menu = ui.menu_button("File", |ui| {
                    ui.set_min_width(260.0);
                    if menu_item(ui, "New", Some("Ctrl-N"), false) {
                        action = Some(MenuAction::New);
                    }
                    if menu_item(ui, "New Notebook…", None, false) {
                        action = Some(MenuAction::NewNotebook);
                    }
                    if menu_item(ui, "Open…", Some("Ctrl-O"), false) {
                        action = Some(MenuAction::Open);
                    }
                    if menu_item(ui, "Open Path…", None, false) {
                        action = Some(MenuAction::OpenPath);
                    }
                    if menu_item(ui, "Save", Some("Ctrl-S"), false) {
                        action = Some(MenuAction::Save);
                    }
                    if menu_item(ui, "Save As…", None, false) {
                        action = Some(MenuAction::SaveAs);
                    }
                    ui.separator();
                    if !self.conflict_copies.is_empty()
                        && menu_item(ui, "Merge Conflict Copies", None, false)
                    {
                        action = Some(MenuAction::MergeConflictCopies);
                    }
                    if menu_item(ui, "Compact History", None, false) {
                        action = Some(MenuAction::CompactHistory);
                    }
                    if menu_item(ui, "Back", Some("Ctrl-["), false) {
                        action = Some(MenuAction::Back);
                    }
                    if menu_item(ui, "Link to Document…", None, false) {
                        action = Some(MenuAction::LinkDocument);
                    }
                    if menu_item(ui, "Import Markdown…", None, false) {
                        action = Some(MenuAction::ImportMarkdown);
                    }
                    if menu_item(ui, "Export Markdown…", Some("Ctrl-Shift-E"), false) {
                        action = Some(MenuAction::File(FileCommand::ExportMarkdown));
                    }
                    if menu_item(ui, "Export HTML…", Some("Ctrl-Shift-B"), false) {
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
                    ui.separator();
                    let in_notebook = self.nb.notebook.is_some();
                    let cite = ui.add_enabled_ui(in_notebook, |ui| {
                        menu_entry(ui, "Cite…", Some("Ctrl-Shift-C"), false)
                            .on_disabled_hover_text("Citing needs a notebook.")
                    });
                    if cite.inner.clicked() {
                        action = Some(MenuAction::Cite);
                    }
                    if in_notebook && menu_item(ui, "References…", None, false) {
                        action = Some(MenuAction::References);
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
                    if menu_item(ui, "Hyphenate", None, self.hyphenate) {
                        action = Some(MenuAction::ToggleHyphenate);
                    }
                    if self.nb.notebook.is_some() {
                        if menu_item(ui, "Notebook browser", Some("Ctrl-Shift-O"), self.nb.browser_open) {
                            action = Some(MenuAction::ToggleBrowser);
                        }
                        if menu_item(ui, "Notebook contents", Some("Ctrl-Shift-L"), false) {
                            action = Some(MenuAction::NotebookHome);
                        }
                        if menu_item(ui, "Jump to document", Some("Ctrl-L"), false) {
                            action = Some(MenuAction::Jump);
                        }
                        if menu_item(ui, "Search this notebook", Some("Ctrl-R"), false) {
                            action = Some(MenuAction::SearchNotebook);
                        }
                        ui.separator();
                    }
                    if menu_item(ui, "Check spelling", None, self.check_spelling) {
                        action = Some(MenuAction::ToggleCheck);
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
                let mut site_menu = None;
                if let Some(notebook) = &self.nb.notebook {
                    let working = self.publishing.working();
                    let has_remote = notebook
                        .settings
                        .remote
                        .as_deref()
                        .is_some_and(|remote| !remote.trim().is_empty());
                    let menu = ui.menu_button("Site", |ui| {
                        ui.set_min_width(160.0);
                        let build = ui.add_enabled_ui(!working, |ui| {
                            menu_item(ui, "Build", None, false)
                        });
                        if build.inner {
                            action = Some(MenuAction::SiteBuild);
                        }
                        let sync = ui.add_enabled_ui(!working && has_remote, |ui| {
                            menu_entry(ui, "Sync", None, false)
                                .on_disabled_hover_text("Set remote in notebook.yaml to send the site")
                        });
                        if sync.inner.clicked() {
                            action = Some(MenuAction::SiteSync);
                        }
                    });
                    site_menu = Some(menu.response);
                }
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
                ]
                .into_iter()
                .chain(site_menu.as_ref().map(|response| (BarMenu::Site, response)))
                {
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
                    // In this row, cut to fit, so a message never changes the height of
                    // the chrome and moves the page. An error goes first and can be
                    // selected, to copy a path or reason out of a desktop launch.
                    let message = error
                        .as_ref()
                        .map(|error| (error, self.palette.error_ink, true))
                        .or(notice
                            .as_ref()
                            .map(|notice| (notice, self.palette.notice_ink, false)));
                    if let Some((text, ink, selectable)) = message {
                        let room = ui.available_width() * STATUS_MESSAGE_SHARE;
                        ui.allocate_ui(Vec2::new(room, ui.available_height()), |ui| {
                            ui.add(
                                egui::Label::new(egui::RichText::new(text).small().color(ink))
                                    .truncate()
                                    .selectable(selectable),
                            )
                            .on_hover_text(text.as_str());
                        });
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(title).strong()).truncate())
                            .on_hover_text(path.as_str());
                    });
                });
            });
        });
        if let Some(action) = action {
            self.run_menu_action(ui.ctx(), action);
        }
    }

    /// Carries out a menu choice through the same path its shortcut uses.
    fn run_menu_action(&mut self, ctx: &egui::Context, action: MenuAction) {
        match action {
            MenuAction::MergeConflictCopies => self.merge_conflict_copies(),
            MenuAction::ToggleBrowser => self.toggle_browser(),
            MenuAction::NotebookHome => self.go_home(),
            MenuAction::Jump => self.toggle_jump(),
            MenuAction::SearchNotebook => self.toggle_search(),
            MenuAction::Back => self.go_back(),
            MenuAction::CompactHistory => match self.session.compact_history() {
                Ok((before, after)) if before == after => {
                    self.notice = Some("the history is already compact".to_string());
                }
                Ok((before, after)) => {
                    self.notice = Some(format!(
                        "compacted the history from {before} entries to {after}"
                    ));
                }
                Err(err) => self.session.last_error = Some(err),
            },
            MenuAction::LinkDocument => self.link_document_with_dialog(),
            MenuAction::New => self.request_new(),
            MenuAction::NewNotebook => self.new_notebook(),
            MenuAction::Open => self.open_with_dialog(),
            MenuAction::OpenPath => self.begin_open(),
            MenuAction::Save => self.save_action(ctx, false),
            MenuAction::SaveAs => self.save_with_dialog(ctx, false),
            MenuAction::ImportMarkdown => self.import_markdown(),
            MenuAction::Properties => self.begin_properties(),
            MenuAction::File(command) => self.run_file_command(command),
            MenuAction::Annotate(kind) => {
                if let Some(id) = self.session.add_annotation(kind) {
                    self.set_open_note(Some(id));
                    self.open_note_pos = None;
                }
            }
            MenuAction::Cite => self.begin_cite(),
            MenuAction::References => self.open_references(),
            MenuAction::TypeSize(step) => self.adjust_font_size(step),
            MenuAction::ToggleDark => self.apply_dark_toggle(ctx),
            MenuAction::ToggleCheck => {
                self.check_spelling = !self.check_spelling;
                self.persist();
            }
            MenuAction::ToggleHyphenate => {
                // The next frame's config differs, which drops everything shaped.
                self.hyphenate = !self.hyphenate;
                self.persist();
            }
            MenuAction::Mode(mode) => self.apply_mode(ctx, mode),
            MenuAction::Help => self.show_help = true,
            MenuAction::SiteBuild => self.start_site_job(ctx, publish::SiteJob::Build),
            MenuAction::SiteSync => {
                let ask = kame_site::notebook::GoAhead::Ask;
                self.start_site_job(ctx, publish::SiteJob::Send(ask));
            }
        }
    }

    /// Brings in what the checker has found, and asks about whatever has changed once typing stops.
    fn update_marks(&mut self, ctx: &egui::Context) {
        if !self.check_spelling {
            return;
        }
        let worker = self.marks.worker.get_or_insert_with(|| {
            let ctx = ctx.clone();
            Background::new(move || ctx.request_repaint())
        });
        for (block, findings) in worker.answers() {
            if self.marks.asked.get(&block.id) == Some(&block) {
                self.marks.asked.remove(&block.id);
            }
            self.marks.checked.insert(block.id, (block, findings));
        }
        let document = self.session.document();
        let notes: Vec<Block> = document
            .annotations
            .iter()
            .filter_map(annotation_block)
            .collect();
        let blocks = &document.blocks;
        self.marks.checked.retain(|id, _| {
            blocks.iter().any(|block| block.id == *id) || notes.iter().any(|note| note.id == *id)
        });
        let stale: Vec<&Block> = blocks
            .iter()
            .chain(notes.iter())
            .filter(|block| {
                self.marks.checked.get(&block.id).map(|(seen, _)| seen) != Some(*block)
                    && self.marks.asked.get(&block.id) != Some(*block)
            })
            .collect();
        if stale.is_empty() {
            self.marks.waiting = None;
            return;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for block in &stale {
            block.text.hash(&mut hasher);
            block.spans.len().hash(&mut hasher);
        }
        let key = hasher.finish();
        let since = match self.marks.waiting {
            Some((waiting, since)) if waiting == key => since,
            _ => std::time::Instant::now(),
        };
        self.marks.waiting = Some((key, since));
        let waited = since.elapsed().as_secs_f32();
        if waited < CHECK_AFTER_SECONDS {
            ctx.request_repaint_after(std::time::Duration::from_secs_f32(
                CHECK_AFTER_SECONDS - waited,
            ));
            return;
        }
        for block in stale {
            worker.ask(block.clone());
            self.marks.asked.insert(block.id, block.clone());
        }
        self.marks.waiting = None;
    }

    /// Selects the next mark after the caret, going round to the first.
    fn go_to_next_mark(&mut self) {
        let metadata = self.session.metadata.clone();
        let exceptions = Exceptions {
            words: &self.my_words,
            document_words: &metadata.words,
            accepted: &metadata.accepted,
            rules_off: &self.rules_off,
        };
        let blocks = &self.session.document().blocks;
        let mut places = Vec::new();
        let mut targets = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            for finding in visible_marks(&self.marks, &exceptions, block, None) {
                places.push((index, finding.start));
                targets.push((block.id, finding.start, finding.end));
            }
        }
        let caret = self.session.valid_caret();
        let here = self.session.block_index(caret.block_id).unwrap_or(0);
        let Some(next) = next_mark(&places, (here, caret.offset)) else {
            self.notice = Some("nothing marked".to_string());
            return;
        };
        let (block_id, start, end) = targets[next];
        self.session.set_caret(Cursor {
            block_id,
            offset: start,
        });
        self.session.select_to(Cursor {
            block_id,
            offset: end,
        });
        self.reveal_caret = true;
    }

    /// Does what the mark's menu was asked for.
    fn apply_mark_action(&mut self, action: MarkAction, block_id: BlockId, finding: &Finding) {
        // A mark is on a block's text, or on the body of the annotation with that id.
        let document = self.session.document();
        let body = document
            .annotations
            .iter()
            .find(|annotation| annotation.id == block_id)
            .map(|annotation| annotation.body.clone());
        let text = body.clone().or_else(|| {
            self.session
                .block_index(block_id)
                .map(|index| document.blocks[index].text.clone())
        });
        let shown = text
            .as_deref()
            .and_then(|text| text.get(finding.start..finding.end))
            .unwrap_or_default()
            .to_string();
        match action {
            MarkAction::Replace(with) => match body {
                Some(body) => {
                    if let Some(new) =
                        body_with_replacement(&body, finding.start, finding.end, &with)
                    {
                        self.session.set_annotation_body(block_id, &new);
                    }
                }
                None => {
                    self.session
                        .replace_text(block_id, finding.start, finding.end, &with);
                }
            },
            MarkAction::Accept => {
                let mut metadata = self.session.metadata.clone();
                metadata.accepted.push(AcceptedMark {
                    rule: finding.rule.clone(),
                    text: shown,
                });
                self.session.set_metadata(metadata);
            }
            MarkAction::AddToDocument => {
                let mut metadata = self.session.metadata.clone();
                metadata.words.push(shown);
                self.session.set_metadata(metadata);
            }
            MarkAction::AddToMine => {
                self.my_words.push(shown);
                self.persist();
            }
            MarkAction::TurnOffRule => {
                self.rules_off.push(finding.rule.clone());
                self.persist();
            }
        }
    }

    fn draw_surface(&mut self, ui: &mut egui::Ui) {
        self.update_marks(ui.ctx());
        let dialog_open = self.dialog_open();
        // Under these, a click goes no further than the caret: it would otherwise open what's in
        // their way, or leave this document and them with it.
        let references_up = self.refs.window_open();
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
                BlockKind::Image { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        for path in &image_paths {
            self.image_texture_handle(ui.ctx(), path);
        }

        // Shape only blocks whose content changed since the last frame; an
        // idle repaint shapes nothing, and an edit re-shapes one block.
        let marks = footnote_marks(self.session.document());
        let page = references::page_blocks(&self.session.document().blocks, &self.refs.list);
        let layouts = self.layouts.sync_marked(&mut self.renderer, &page, &marks);

        let paper: [u8; 3] = [
            self.palette.background.r(),
            self.palette.background.g(),
            self.palette.background.b(),
        ];
        let highlights = highlight_map(self.session.document(), &self.palette);
        self.textures.sync(
            ui.ctx(),
            &mut self.renderer,
            &page,
            &layouts,
            self.palette.body_ink,
            paper,
            &highlights,
        );

        let document = self.session.document();
        let blocks: &[Block] = &page;
        let caret = self.session.caret;

        // List markers: a bullet that changes with the level, or a number.
        let list_markers: Vec<Option<String>> = list_positions(blocks, false)
            .into_iter()
            .zip(list_labels(blocks))
            .map(|(position, label)| {
                position.map(|position| {
                    label.unwrap_or_else(|| {
                        LIST_BULLETS[usize::from(position.depth) % LIST_BULLETS.len()].to_string()
                    })
                })
            })
            .collect();

        // Footnote numbering is reading order, not creation order: the stored
        // annotation vector records when a note was made, which is unrelated
        // to where it sits in the prose.
        let annotations = reading_order(&document.annotations, blocks);

        let heights: Vec<f32> = blocks
            .iter()
            .zip(layouts.iter())
            .map(|(block, layout)| block_height(block, layout, &config, self.session.path()))
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

        let backlinks_height =
            notebook::backlinks_height(self.nb.backlinks.len(), config.margin_size);
        let content_height =
            (footnote_bottom + backlinks_height + PAGE_PADDING).max(ui.available_height());

        let (response, painter) = ui.allocate_painter(
            Vec2::new(ui.available_width(), content_height),
            Sense::click_and_drag(),
        );
        let mut geometry = page_geometry(config, response.rect.width());
        if let Some(left) = self.nb.page_left {
            geometry.body_left = left;
        }
        // A zero gutter means the margin column has no room (hidden, or too
        // narrow to sit beside a readable measure): marginalia cannot be set.
        let show_margin = geometry.margin_width > 0.0;
        let body_left = response.rect.left() + geometry.body_left;
        let backlink = notebook::draw_backlinks(
            &painter,
            &response,
            &self.nb.backlinks,
            Pos2::new(body_left, response.rect.top() + footnote_bottom),
            config.margin_size,
            Color32::from_rgb(
                self.palette.apparatus_ink[0],
                self.palette.apparatus_ink[1],
                self.palette.apparatus_ink[2],
            ),
        );
        if backlink.is_some() && !references_up {
            self.nb.pending_open = backlink;
        }

        let mut tops = Vec::with_capacity(blocks.len());
        let mut y = response.rect.top() + PAGE_PADDING;
        for (index, height) in heights.iter().enumerate() {
            tops.push(y);
            y += height + gap_below(index);
        }

        // --- code blocks scroll sideways ---------------------------------------
        // The wheel scrolls the block under the pointer, and a caret that has moved brings its
        // block along. Shift and the wheel, or a trackpad, is what makes horizontal scrolling.
        let wheel_x = ui.input(|input| input.smooth_scroll_delta().x);
        let hovered = ui.input(|input| input.pointer.hover_pos());
        let moved_caret = self.code_caret_seen != Some(caret);
        self.code_caret_seen = Some(caret);
        let mut sideways = vec![0.0_f32; blocks.len()];
        for (index, block) in blocks.iter().enumerate() {
            if !matches!(block.kind, BlockKind::Code { .. }) {
                continue;
            }
            let column = (config.body_width - layouts[index].left_indent).max(1.0);
            let mut scroll = self.code_scroll.get(&block.id).copied().unwrap_or(0.0);
            let under_pointer = hovered
                .is_some_and(|at| (tops[index]..=tops[index] + heights[index]).contains(&at.y));
            if wheel_x != 0.0 && under_pointer {
                scroll -= wheel_x;
            }
            if moved_caret && block.id == caret.block_id {
                let x = caret_x(layouts[index], caret.offset);
                scroll = code_scroll_to_show(scroll, x, column);
            }
            scroll = clamp_code_scroll(scroll, column, layouts[index].content_width());
            sideways[index] = scroll;
            self.code_scroll.insert(block.id, scroll);
        }
        let block_painter = |index: usize| {
            if matches!(blocks[index].kind, BlockKind::Code { .. }) {
                let left = body_left + layouts[index].left_indent;
                let edge = |x: f32| Pos2::new(x, ui.clip_rect().top());
                let far = Pos2::new(body_left + config.body_width + 1.0, ui.clip_rect().bottom());
                painter.with_clip_rect(Rect::from_min_max(edge(left - 1.0), far))
            } else {
                painter.clone()
            }
        };

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
                block_painter(index).rect_filled(
                    rect.translate(Vec2::new(
                        body_left + layouts[index].left_indent - sideways[index],
                        tops[index],
                    )),
                    1.0,
                    color,
                );
            }
        }

        // --- document links --------------------------------------------------
        // Underlined, which sets them apart from a link that leaves kame.
        for (index, block) in blocks.iter().enumerate() {
            for span in &block.spans {
                if !matches!(span.style, SpanStyle::DocumentLink { .. }) {
                    continue;
                }
                for rect in range_rects(layouts[index], span.start, span.end) {
                    let rect = rect.translate(Vec2::new(
                        body_left + layouts[index].left_indent - sideways[index],
                        tops[index],
                    ));
                    let y = rect.bottom() - rect.height() * DOCUMENT_LINK_UNDERLINE;
                    block_painter(index).line_segment(
                        [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                        egui::Stroke::new(1.0, DOCUMENT_LINK_INK),
                    );
                }
            }
        }

        // --- prose -----------------------------------------------------------
        // Paragraph-focus mode veils every block but the caret's: the veil is
        // paper at partial alpha, so dimmed prose fades and the margins stay
        // paper-coloured.
        let caret_index = blocks.iter().position(|block| block.id == caret.block_id);
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        for (index, block) in blocks.iter().enumerate() {
            if block.kind == BlockKind::Rule {
                let centre = body_left + config.body_width * 0.5;
                let half = config.body_width * RULE_WIDTH * 0.5;
                let y = tops[index] + heights[index] * 0.5;
                painter.line_segment(
                    [Pos2::new(centre - half, y), Pos2::new(centre + half, y)],
                    egui::Stroke::new(1.0, self.palette.rule_ink),
                );
                continue;
            }
            if let BlockKind::Image { path, align, .. } = &block.kind {
                // At its own size if it fits the measure, and scaled to it if not. Its
                // height is already baked into `tops`.
                let file = image_file(path, self.session.path());
                let size = image_size(&file, config.body_width);
                let left = image_left(body_left, config.body_width, size.x, *align);
                let rect = Rect::from_min_size(Pos2::new(left, tops[index]), size);
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
                let origin = Pos2::new(body_left + indent - sideways[index], tops[index]);
                let rect = texture.rect(origin);
                let clipped = block_painter(index);
                clipped.image(texture.texture.id(), rect, uv, Color32::WHITE);
                if let Some(veil) = dim_veil(self.palette.dim_veil, focus, index, caret_index) {
                    clipped.rect_filled(rect, 0.0, veil);
                }
                // A thin mark under a block that scrolls says how much there is, and where.
                let column = (config.body_width - indent).max(1.0);
                let content = layouts[index].content_width();
                if matches!(block.kind, BlockKind::Code { .. }) && content > column {
                    let y = tops[index] + heights[index] + 2.0;
                    let left = body_left + indent + column * sideways[index] / content;
                    let length = column * column / content;
                    painter.line_segment(
                        [Pos2::new(left, y), Pos2::new(left + length, y)],
                        egui::Stroke::new(1.5, self.palette.rule_ink),
                    );
                    // A solid edge wherever the lines carry on past the column.
                    let (more_left, more_right) =
                        code_overflow_edges(sideways[index], column, content);
                    let edge = |x: f32| {
                        painter.line_segment(
                            [
                                Pos2::new(x, tops[index]),
                                Pos2::new(x, tops[index] + heights[index]),
                            ],
                            egui::Stroke::new(1.5, self.palette.rule_ink),
                        );
                    };
                    if more_left {
                        edge(body_left + indent - CODE_EDGE_GAP);
                    }
                    if more_right {
                        edge(body_left + indent + column + CODE_EDGE_GAP);
                    }
                }
            }
        }

        // --- likely mistakes ---------------------------------------------------
        if self.check_spelling {
            let exceptions = Exceptions {
                words: &self.my_words,
                document_words: &self.session.metadata.words,
                accepted: &self.session.metadata.accepted,
                rules_off: &self.rules_off,
            };
            for (index, block) in blocks.iter().enumerate() {
                let at_caret = (block.id == caret.block_id).then_some(caret.offset);
                for finding in visible_marks(&self.marks, &exceptions, block, at_caret) {
                    let color = match finding.kind {
                        FindingKind::Spelling => self.palette.spelling_mark,
                        FindingKind::Grammar => self.palette.grammar_mark,
                    };
                    for rect in range_rects(layouts[index], finding.start, finding.end) {
                        let rect = rect.translate(Vec2::new(
                            body_left + layouts[index].left_indent - sideways[index],
                            tops[index],
                        ));
                        let y = rect.bottom() - rect.height() * MARK_LIFT;
                        block_painter(index).add(egui::Shape::line(
                            wavy_line(rect.left(), rect.right(), y),
                            egui::Stroke::new(1.0, color),
                        ));
                    }
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
                        block_painter(index).rect_filled(
                            rect.translate(Vec2::new(
                                body_left + layouts[index].left_indent - sideways[index],
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
                Pos2::new(
                    body_left + layouts[index].left_indent + x - sideways[index],
                    top,
                ),
                Vec2::new(1.5, line_height),
            );
            block_painter(index).rect_filled(rect, 0.0, self.palette.caret_ink);
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

        // --- following the caret ----------------------------------------------
        // A key that takes the caret off the page brings the page with it, no further
        // than it has to. A mouse or wheel leaves the page where the reader put it.
        if std::mem::take(&mut self.follow_caret) && !typewriter {
            if let Some(rect) = caret_rect {
                let page_top = response.rect.top();
                if let Some(offset) = scroll_to_show(
                    rect.top() - page_top,
                    rect.bottom() - page_top,
                    self.page_scroll,
                    ui.clip_rect().height(),
                    content_height,
                ) {
                    self.pending_scroll = Some(offset);
                    ui.ctx().request_repaint();
                }
            }
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
        // Each checked annotation as drawn: its layout's origin, the column it's set in, and what
        // comes before its body.
        let mut drawn_notes: Vec<(Block, Pos2, f32, String, Rect)> = Vec::new();
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
                        if let Some(block) = annotation_block(annotation) {
                            drawn_notes.push((block, origin, margin_column, String::new(), rect));
                        }
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
        let pointer_at = ui.input(|input| input.pointer.hover_pos());
        let mut footnote_number_under = None;
        for placement in &footnotes {
            if let Some(texture) = self.annotation_textures.get(placement.annotation_id) {
                let origin = footnote_origin(response.rect.top(), body_left, placement);
                let rect = texture.rect(origin);
                painter.image(texture.texture.id(), rect, uv, Color32::WHITE);
                // The foot entry is the hit target for reopening its popover.
                markers.push((placement.annotation_id, rect));
                let annotation = by_id.get(&placement.annotation_id);
                // Its number goes back to the text instead. This shapes the entry again, which
                // is why it waits until the pointer is on it.
                if let (Some(at), Some(annotation)) =
                    (pointer_at.filter(|at| rect.contains(*at)), annotation)
                {
                    let entry = Block {
                        id: annotation.id,
                        kind: BlockKind::Paragraph,
                        text: format!("{}. {}", placement.number, annotation.body),
                        spans: Vec::new(),
                    };
                    let layout =
                        self.renderer
                            .layout_in_column(&entry, config.body_width, annotation_size);
                    if footnote_number_rect(&layout, placement.number, origin)
                        .is_some_and(|number| number.contains(at))
                    {
                        footnote_number_under = footnote_return_cursor(annotation);
                    }
                }
                if let Some(block) = annotation.and_then(|annotation| annotation_block(annotation))
                {
                    let prefix = format!("{}. ", placement.number);
                    drawn_notes.push((block, origin, config.body_width, prefix, rect));
                }
            }
        }

        // --- likely mistakes in marginals and footnotes ---------------------------
        // Their text is drawn as an image, so finding a word on it takes the same layout.
        let mut note_layouts: Vec<Option<BlockLayout>> = Vec::with_capacity(drawn_notes.len());
        for (block, origin, width, prefix, _) in &drawn_notes {
            let exceptions = Exceptions {
                words: &self.my_words,
                document_words: &self.session.metadata.words,
                accepted: &self.session.metadata.accepted,
                rules_off: &self.rules_off,
            };
            let shown = if self.check_spelling {
                visible_marks(&self.marks, &exceptions, block, None)
            } else {
                Vec::new()
            };
            if shown.is_empty() {
                note_layouts.push(None);
                continue;
            }
            let entry = Block {
                text: format!("{prefix}{}", block.text),
                ..block.clone()
            };
            let layout = self
                .renderer
                .layout_in_column(&entry, *width, annotation_size);
            for finding in &shown {
                let color = match finding.kind {
                    FindingKind::Spelling => self.palette.spelling_mark,
                    FindingKind::Grammar => self.palette.grammar_mark,
                };
                let (start, end) = (finding.start + prefix.len(), finding.end + prefix.len());
                for rect in range_rects(&layout, start, end) {
                    let rect = rect.translate(origin.to_vec2());
                    let y = rect.bottom() - rect.height() * MARK_LIFT;
                    painter.add(egui::Shape::line(
                        wavy_line(rect.left(), rect.right(), y),
                        egui::Stroke::new(1.0, color),
                    ));
                }
            }
            note_layouts.push(Some(layout));
        }

        let mut footnote_marks_drawn: Vec<(Rect, f32)> = Vec::new();
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
                layouts[index],
                &annotation.anchor,
                body_left,
                tops[index],
                config.body_width,
                FOOTNOTE_MARK_EM * config.body_size,
            ) else {
                continue;
            };
            let mark = painter.text(
                origin,
                Align2::LEFT_TOP,
                placement.number.to_string(),
                FontId::proportional(config.body_size * 0.62),
                self.palette.footnote_mark,
            );
            footnote_marks_drawn.push((mark, placement.y));
        }

        // --- pointer ---------------------------------------------------------
        let shift = ui.input(|input| input.modifiers.shift);
        let mut caret_action: Option<(Cursor, bool)> = None;
        let mut word_drag_to = None;
        let mut followed_link = None;
        let mut word_selection = None;
        let mut marker_click: Option<(AnnotationId, Rect)> = None;
        let pointer = response.interact_pointer_pos();
        if response.clicked() {
            if let Some(pos) = pointer {
                self.last_click = Some((std::time::Instant::now(), pos));
            }
        }
        if response.drag_started() {
            // Where the press began, since the pointer has moved by the time this is a drag.
            let origin = ui.input(|input| input.pointer.press_origin()).or(pointer);
            let second_press = origin
                .is_some_and(|at| is_second_press(self.last_click, std::time::Instant::now(), at));
            self.word_drag_start = None;
            if let Some(pos) = origin {
                if let Some(cursor) =
                    hit_test(blocks, &layouts, &tops, &heights, &sideways, body_left, pos)
                {
                    if second_press && !shift {
                        self.word_drag_start = Some(cursor);
                    } else {
                        caret_action = Some((cursor, shift));
                    }
                }
            }
        }
        // --- a mark's menu -----------------------------------------------------
        if response.secondary_clicked() {
            self.mark_menu = None;
            self.image_menu = None;
            self.link_menu = None;
            if let Some(pos) = response.interact_pointer_pos() {
                if let Some(cursor) =
                    hit_test(blocks, &layouts, &tops, &heights, &sideways, body_left, pos)
                {
                    self.link_menu = self.session.link_under(cursor).map(|link| LinkMenu {
                        url: link.url.clone(),
                        link: Some(link),
                    });
                    let image_link = blocks
                        .iter()
                        .find(|block| block.id == cursor.block_id)
                        .and_then(|block| match &block.kind {
                            BlockKind::Image { link, .. } => link.clone(),
                            _ => None,
                        });
                    if let Some(url) = image_link {
                        self.link_menu = Some(LinkMenu { link: None, url });
                    }
                    if references_up {
                        self.link_menu = None;
                    }
                    self.image_menu = blocks
                        .iter()
                        .find(|block| block.id == cursor.block_id)
                        .filter(|block| matches!(block.kind, BlockKind::Image { .. }))
                        .map(|block| block.id);
                    let exceptions = Exceptions {
                        words: &self.my_words,
                        document_words: &self.session.metadata.words,
                        accepted: &self.session.metadata.accepted,
                        rules_off: &self.rules_off,
                    };
                    if let Some(block) = blocks.iter().find(|block| block.id == cursor.block_id) {
                        let shown = visible_marks(&self.marks, &exceptions, block, None);
                        if let Some(finding) = mark_at(&shown, cursor.offset) {
                            self.mark_menu = Some((block.id, finding.clone()));
                        }
                    }
                }
                // A marginal or a footnote under the pointer is what the click was on.
                for (note, layout) in drawn_notes.iter().zip(&note_layouts) {
                    let (block, origin, _, prefix, rect) = note;
                    let Some(layout) = layout.as_ref().filter(|_| rect.contains(pos)) else {
                        continue;
                    };
                    let exceptions = Exceptions {
                        words: &self.my_words,
                        document_words: &self.session.metadata.words,
                        accepted: &self.session.metadata.accepted,
                        rules_off: &self.rules_off,
                    };
                    let shown = visible_marks(&self.marks, &exceptions, block, None);
                    let at = pos - *origin;
                    let offset = offset_at(layout, at.x, at.y, block.text.len() + prefix.len());
                    self.mark_menu = offset
                        .checked_sub(prefix.len())
                        .and_then(|offset| mark_at(&shown, offset))
                        .map(|finding| (block.id, finding.clone()));
                }
            }
        }

        // --- the wheel ---------------------------------------------------------
        // Scrolling brings the caret along, so that it's on the page you're looking at. Typing
        // then happens there, and a selection stays what was selected. The typewriter keeps
        // the caret where it was: there, the page can be scrolled while idle.
        let wheeled = ui.input(|input| input.smooth_scroll_delta().y != 0.0);
        if wheeled
            && caret_action.is_none()
            && !typewriter
            && self.session.selection.is_none()
            && !dialog_open
        {
            if let Some(rect) = caret_rect {
                let page = ui.clip_rect();
                let aim =
                    caret_aim_after_scroll(rect.top(), rect.bottom(), page.top(), page.bottom());
                if let Some(y) = aim {
                    let aim = Pos2::new(rect.left(), y);
                    if let Some(cursor) =
                        hit_test(blocks, &layouts, &tops, &heights, &sideways, body_left, aim)
                    {
                        caret_action = Some((cursor, false));
                    }
                }
            }
        }

        // --- page keys ---------------------------------------------------------
        // The caret goes a window up or down at the same column. When that finds nowhere new,
        // the caret is on the first or last line, so it goes to the very start or end.
        if let (Some((down, select)), Some(rect)) = (self.page_key.take(), caret_rect) {
            let distance = page_step(ui.clip_rect().height(), config.body_line_height);
            let step = if down { distance } else { -distance };
            let aim = Pos2::new(rect.left(), rect.center().y + step);
            let found = hit_test(blocks, &layouts, &tops, &heights, &sideways, body_left, aim);
            let target = found.filter(|cursor| *cursor != caret).or_else(|| {
                let edge = if down { blocks.last() } else { blocks.first() }?;
                Some(Cursor {
                    block_id: edge.id,
                    offset: if down { edge.text.len() } else { 0 },
                })
            });
            if let Some(target) = target {
                caret_action = Some((target, select));
                let place = blocks.iter().position(|block| block.id == target.block_id);
                let line = place.and_then(|index| {
                    let line = caret_line(layouts[index], target.offset)?;
                    Some((tops[index] + line.y - response.rect.top(), line.height))
                });
                if let (Some((top, height)), false) = (line, typewriter) {
                    self.pending_scroll = Some(scroll_after_page_key(
                        self.page_scroll,
                        step,
                        top,
                        top + height,
                        ui.clip_rect().height(),
                        content_height,
                    ));
                }
                ui.ctx().request_repaint();
            }
        }
        if response.dragged() {
            if let Some(pos) = pointer {
                if let Some(cursor) =
                    hit_test(blocks, &layouts, &tops, &heights, &sideways, body_left, pos)
                {
                    if self.word_drag_start.is_some() {
                        word_drag_to = Some(cursor);
                    } else {
                        caret_action = Some((cursor, true));
                    }
                }
            }
        }
        if response
            .hover_pos()
            .is_some_and(|pos| footnote_mark_at(&footnote_marks_drawn, pos).is_some())
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if footnote_number_under.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let mut footnote_return = None;
        if response.clicked() {
            if let Some(pos) = pointer {
                if footnote_number_under.is_some() {
                    footnote_return = footnote_number_under;
                } else if let Some(footnote_y) = footnote_mark_at(&footnote_marks_drawn, pos) {
                    // A click on a footnote's number drops down to the footnote.
                    self.pending_scroll =
                        Some(footnote_scroll_offset(footnote_y, ui.clip_rect().height()));
                    ui.ctx().request_repaint();
                } else if let Some(hit) =
                    markers.iter().find(|(_, rect)| rect.contains(pos)).copied()
                {
                    marker_click = Some(hit);
                } else if let Some(cursor) =
                    hit_test(blocks, &layouts, &tops, &heights, &sideways, body_left, pos)
                {
                    // A double click selects a word, even in a link: its first click has
                    // already opened the link. A click on a link opens it instead of moving
                    // the caret.
                    if response.double_clicked() {
                        word_selection = Some(cursor);
                    } else if references_up {
                        caret_action = Some((cursor, shift));
                    } else if let Some(target) = document_link_at(blocks, &cursor) {
                        followed_link = Some(target);
                    } else if let Some(url) = url_at(blocks, &cursor) {
                        if let Some(key) =
                            cited_key(&url).filter(|key| self.refs.list.contains(key))
                        {
                            self.refs.show(key, pos);
                        } else if self.nb.notebook.is_some() && notebook::is_relative_link(&url) {
                            followed_link = Some(url);
                        } else {
                            open_in_browser(&url);
                        }
                    } else {
                        caret_action = Some((cursor, shift));
                    }
                }
            }
        }

        // `blocks` borrows the document; every caret move is applied after the
        // borrow's last use.
        if let (Some(start), Some(to)) = (self.word_drag_start, word_drag_to) {
            let anchor = self.session.word_range_at(start);
            self.session.select_words_between(anchor, to);
        } else if let Some(at) = word_selection {
            self.session.select_word_at(at);
            self.set_open_note(None);
            self.refs.shown = None;
        } else if let Some(target) = followed_link {
            self.follow_document_link(&target);
        } else if let Some(cursor) = footnote_return {
            self.session.set_caret(cursor);
            self.reveal_caret = true;
            self.set_open_note(None);
        } else if let Some((id, rect)) = marker_click {
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
        let mut link_document = false;
        let mut insert_image = false;
        let mut cite = false;
        let mut mark_action = None;
        let mark_menu = self.mark_menu.clone();
        let link_menu = self.link_menu.clone();
        let mut open_link = false;
        let mut edit_link = false;
        response.context_menu(|ui| {
            ui.set_min_width(180.0);
            if let Some(menu) = &link_menu {
                if ui.button("Open link").clicked() {
                    open_link = true;
                    ui.close();
                }
                if ui.button("Copy link").clicked() {
                    ui.ctx().copy_text(menu.url.clone());
                    ui.close();
                }
                if menu.link.is_some() && ui.button("Edit link…").clicked() {
                    edit_link = true;
                    ui.close();
                }
                ui.separator();
            }
            if let Some((_, finding)) = &mark_menu {
                ui.label(egui::RichText::new(&finding.message).weak());
                for suggestion in &finding.suggestions {
                    let label = if suggestion.is_empty() {
                        "Remove it"
                    } else {
                        suggestion
                    };
                    if ui.button(label).clicked() {
                        mark_action = Some(MarkAction::Replace(suggestion.clone()));
                        ui.close();
                    }
                }
                for (label, action) in [
                    ("Accept this", MarkAction::Accept),
                    ("Add to this document's words", MarkAction::AddToDocument),
                    ("Add to my words", MarkAction::AddToMine),
                    ("Turn off this rule", MarkAction::TurnOffRule),
                ] {
                    if ui.button(label).clicked() {
                        mark_action = Some(action);
                        ui.close();
                    }
                }
                ui.separator();
            }
            let image = self.image_menu.and_then(|id| {
                let index = self.session.block_index(id)?;
                match self.session.document().blocks[index].kind {
                    BlockKind::Image { align, .. } => Some((id, align)),
                    _ => None,
                }
            });
            if let Some((id, current)) = image {
                ui.menu_button("Align", |ui| {
                    for (label, align) in [
                        ("Left", ImageAlign::Left),
                        ("Center", ImageAlign::Center),
                        ("Right", ImageAlign::Right),
                    ] {
                        let mut row = egui::Button::new(label);
                        if align == current {
                            row = row.right_text("\u{2714}");
                        }
                        if ui.add(row).clicked() {
                            self.session.set_image_align(id, align);
                            ui.close();
                        }
                    }
                });
                ui.separator();
            }
            // The caret's block, since that is the one these rows change.
            let caret = self.session.valid_caret();
            let current = self
                .session
                .block_index(caret.block_id)
                .map(|index| self.session.document().blocks[index].kind.clone());
            for (label, kind) in [
                ("Body", BlockKind::Paragraph),
                ("Heading 1", BlockKind::Heading { level: 1 }),
                ("Heading 2", BlockKind::Heading { level: 2 }),
                ("Heading 3", BlockKind::Heading { level: 3 }),
                (
                    "Bulleted list",
                    BlockKind::ListItem {
                        ordered: false,
                        depth: 0,
                    },
                ),
                (
                    "Numbered list",
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                ),
                ("Block quote", BlockKind::BlockQuote),
                ("Code block", BlockKind::Code { language: None }),
                ("Prose block", BlockKind::Prose),
            ] {
                let mut row = egui::Button::new(label);
                if current.as_ref().is_some_and(|c| is_block_style(c, &kind)) {
                    // U+2714: egui's default fonts have no U+2713.
                    row = row.right_text("\u{2714}");
                }
                if ui.add(row).clicked() {
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
            if self.nb.notebook.is_some() && ui.button("Cite…").clicked() {
                cite = true;
                ui.close();
            }
            ui.separator();
            if ui.button("Insert image…").clicked() {
                insert_image = true;
                ui.close();
            }
            if ui.button("Link to document…").clicked() {
                link_document = true;
                ui.close();
            }
        });
        if let Some(menu) = link_menu.filter(|_| open_link) {
            let document = menu.link.as_ref().is_some_and(|link| link.document);
            let in_notebook = self.nb.notebook.is_some() && notebook::is_relative_link(&menu.url);
            if document || in_notebook {
                self.follow_document_link(&menu.url);
            } else {
                open_in_browser(&menu.url);
            }
        } else if let Some(link) = self
            .link_menu
            .clone()
            .filter(|_| edit_link)
            .and_then(|menu| menu.link)
        {
            self.open_link_prompt(link.block_id, link.start, link.end, Some(link.url));
        }
        if cite {
            self.begin_cite();
        }
        if insert_image {
            self.insert_image_with_dialog();
        }
        if link_document {
            self.link_document_with_dialog();
        }
        if let (Some(action), Some((block_id, finding))) = (mark_action, mark_menu) {
            self.apply_mark_action(action, block_id, &finding);
        }

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
        } else if self.link_prompt.is_some() {
            self.show_link_prompt(ctx, enter);
        } else if self.restore_picker.is_some() {
            self.show_restore_picker(ctx, enter);
        } else if self.nb.file_op.is_some() {
            self.show_file_op(ctx, enter);
        } else if self.nb.move_report.is_some() {
            self.show_move_report(ctx);
        }
        if self.show_help {
            self.show_help_dialog(ctx);
        }
    }

    /// The usage and keybinding reference, in a modal window. This is where the
    /// "how to use it" text lives; the status bar stays to counts and toggles.
    ///
    /// It is as wide as the window allows, up to a limit, with the rows in as many columns as
    /// fit, and scrolls when the window is too short for them. The Close button stays below.
    fn show_help_dialog(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut close_clicked = false;
        let screen = ctx.content_rect();
        let margin = ctx.global_style().spacing.window_margin;
        let frame = Vec2::new(
            f32::from(margin.left) + f32::from(margin.right),
            f32::from(margin.top) + f32::from(margin.bottom),
        );
        let width = (screen.width() - HELP_EDGE * 2.0).clamp(160.0, HELP_MAX_WIDTH);
        let inner = width - frame.x - 2.0;
        // Room for the title bar, the Close button and the space around them.
        let scroll_height = (screen.height() - HELP_EDGE * 2.0 - frame.y - HELP_CHROME).max(40.0);
        let columns = ((inner / HELP_COLUMN_WIDTH) as usize).clamp(1, 3);
        let per_column = HELP_ROWS.len().div_ceil(columns);
        egui::Window::new("kame — help")
            .id(help_id())
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(inner);
                egui::ScrollArea::vertical()
                    .id_salt("kame-help-rows")
                    .max_height(scroll_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.columns(columns, |cells| {
                            for (cell, rows) in cells.iter_mut().zip(HELP_ROWS.chunks(per_column)) {
                                let keys_width = (cell.available_width() * 0.4).floor();
                                let what_width =
                                    (cell.available_width() - keys_width - 8.0).max(40.0);
                                for (keys, what) in rows {
                                    cell.horizontal_top(|ui| {
                                        ui.spacing_mut().item_spacing.x = 8.0;
                                        ui.allocate_ui_with_layout(
                                            Vec2::new(keys_width, 0.0),
                                            egui::Layout::top_down(egui::Align::Min),
                                            |ui| {
                                                ui.set_width(keys_width);
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(*keys).monospace(),
                                                    )
                                                    .wrap(),
                                                );
                                            },
                                        );
                                        ui.allocate_ui_with_layout(
                                            Vec2::new(what_width, 0.0),
                                            egui::Layout::top_down(egui::Align::Min),
                                            |ui| {
                                                ui.set_width(what_width);
                                                ui.add(egui::Label::new(*what).wrap());
                                            },
                                        );
                                    });
                                }
                            }
                        });
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
        self.renderer
            .layout_marked(&with_citation_spans(block, &self.refs.list), block_marks)
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
            words: {
                let mut words = metadata.words.clone();
                sort_words(&mut words);
                words
            },
            new_word: String::new(),
            accepted: metadata.accepted.clone(),
            default_author: false,
            publish: metadata.publish,
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
                ui.label("Words that are right in this document");
                let mut forget_word = None;
                egui::ScrollArea::vertical()
                    .id_salt("kame-metadata-words")
                    .max_height(PROPERTIES_LIST_HEIGHT)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if prompt.words.is_empty() {
                            ui.label(egui::RichText::new("none yet").weak());
                        }
                        for (index, word) in prompt.words.iter().enumerate() {
                            ui.horizontal(|ui| {
                                if ui
                                    .small_button("×")
                                    .on_hover_text("mark it again")
                                    .clicked()
                                {
                                    forget_word = Some(index);
                                }
                                ui.label(word);
                            });
                        }
                    });
                if let Some(index) = forget_word {
                    prompt.words.remove(index);
                }
                ui.horizontal(|ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut prompt.new_word)
                            .return_key(None)
                            .desired_width(240.0)
                            .hint_text("add a word"),
                    );
                    let adding = !prompt.new_word.trim().is_empty();
                    let add = ui.add_enabled(adding, egui::Button::new("Add")).clicked();
                    // Enter here adds the word, rather than saving the dialog around it.
                    if add || (enter && adding && field.has_focus()) {
                        let typed = std::mem::take(&mut prompt.new_word);
                        add_words(&mut prompt.words, &typed);
                        save = false;
                        field.request_focus();
                    }
                });
                if !prompt.accepted.is_empty() {
                    ui.add_space(4.0);
                    ui.label("Accepted marks");
                    let mut forget = None;
                    egui::ScrollArea::vertical()
                        .id_salt("kame-metadata-accepted")
                        .max_height(PROPERTIES_LIST_HEIGHT)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (index, mark) in prompt.accepted.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    if ui
                                        .small_button("×")
                                        .on_hover_text("mark it again")
                                        .clicked()
                                    {
                                        forget = Some(index);
                                    }
                                    ui.label(&mark.text);
                                    ui.label(egui::RichText::new(&mark.rule).weak());
                                });
                            }
                        });
                    if let Some(index) = forget {
                        prompt.accepted.remove(index);
                    }
                    ui.add_space(4.0);
                }
                ui.checkbox(
                    &mut prompt.default_author,
                    "Use this author for new documents",
                );
                if self.nb.notebook.is_some() {
                    ui.checkbox(&mut prompt.publish, "Publish on this notebook's site");
                }
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
                if prompt.default_author {
                    self.default_author = prompt.author.trim().to_string();
                    self.persist();
                }
                self.session.set_metadata(metadata_from_prompt(&prompt));
                if let Some(warning) = kame_core::session::date_warning(&self.session.metadata) {
                    self.notice = Some(warning);
                }
            }
        }
    }

    /// Opens the path prompt, clearing any other file dialog so it is the one
    /// thing on screen.
    fn begin_open(&mut self) {
        if self.say_if_reference_form_up() {
            return;
        }
        self.pending_heading = None;
        self.snapshot_prompt = None;
        self.restore_picker = None;
        self.open_prompt = Some(OpenPrompt {
            path: String::new(),
            focus: true,
            confirming: false,
        });
    }

    /// File > Import Markdown: asks for a Markdown file and opens it.
    fn import_markdown(&mut self) {
        let directory = self.session.dialog_directory();
        if let Some(path) = pick_markdown_path(directory.as_deref()) {
            self.open_picked(path);
        }
    }

    /// File > Open: asks for a document in the system's file browser and opens it.
    fn open_with_dialog(&mut self) {
        if self.say_if_reference_form_up() {
            return;
        }
        self.snapshot_prompt = None;
        self.restore_picker = None;
        self.open_prompt = None;
        let directory = self.session.dialog_directory();
        if let Some(path) = pick_open_path(directory.as_deref()) {
            self.open_picked(path);
        }
    }

    /// Asks for an image, starting in the notebook's `images/` if there is one, and inserts it
    /// as a block after the caret's. One from outside the notebook is copied into it first.
    fn insert_image_with_dialog(&mut self) {
        let notebook = self.nb.notebook.clone();
        let directory = notebook
            .as_ref()
            .map(|notebook| notebook.root.join(kame_notebook::images::IMAGES))
            .filter(|folder| folder.is_dir())
            .or_else(|| self.session.dialog_directory());
        let Some(file) = pick_image_path(directory.as_deref()) else {
            return;
        };
        let document = self.session.path().map(Path::to_path_buf);
        let stored = match (&document, &notebook) {
            (Some(document), Some(notebook)) => {
                match kame_notebook::images::bring_in(notebook, document, &file) {
                    Ok(kept) => {
                        if kept != file {
                            self.notice = Some(format!("copied the image to {}", kept.display()));
                        }
                        stored_image_path(&kept, document, Some(&notebook.root))
                    }
                    Err(err) => {
                        self.session.last_error = Some(err);
                        return;
                    }
                }
            }
            (Some(document), None) => stored_image_path(&file, document, None),
            (None, _) => file.display().to_string(),
        };
        self.session.insert_image(stored);
    }

    /// Asks for a document, then links to it from the caret or selection.
    fn link_document_with_dialog(&mut self) {
        if let Some(refusal) = self.session.document_link_refusal() {
            self.notice = Some(refusal.to_string());
            return;
        }
        let directory = self.session.dialog_directory();
        let Some(path) = pick_document_link_path(directory.as_deref()) else {
            return;
        };
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let document = self.session.path().map(Path::to_path_buf);
        let root = self
            .nb
            .notebook
            .as_ref()
            .map(|notebook| notebook.root.clone());
        let target =
            document_link_target(&path, home.as_deref(), document.as_deref(), root.as_deref());
        let title = document_title(&path);
        if let Err(refusal) = self.session.link_to_document(target, &title) {
            self.notice = Some(refusal.to_string());
        }
    }

    /// Goes where a document link leads. A link that has rotted says so and opens
    /// nothing. Leaving unsaved work holds at the Open prompt's confirmation, and the
    /// heading waits for the open.
    fn follow_document_link(&mut self, target: &str) {
        let (path, heading) = split_document_target(target);
        let here = BackStop {
            path: self.session.path().map(Path::to_path_buf),
            caret: self.session.caret,
            scroll: self.page_scroll,
        };
        if path.is_empty() {
            if heading.is_some_and(|heading| self.session.heading_cursor(heading).is_some()) {
                self.back.push(here);
            }
            self.go_to_heading(heading);
            return;
        }
        let found = match self.follow_in_notebook(path, heading) {
            Some(Some(found)) => found,
            Some(None) => return,
            None => {
                let found = kame_sync::logs::find_or_build(path, &self.session.link_base());
                let Ok(Some(found)) = found else {
                    self.notice = Some(format!("no document at {path}"));
                    return;
                };
                found
            }
        };
        let same = self
            .session
            .path()
            .and_then(|current| current.canonicalize().ok())
            .is_some_and(|current| found.canonicalize().is_ok_and(|found| found == current));
        if same {
            if heading.is_some_and(|heading| self.session.heading_cursor(heading).is_some()) {
                self.back.push(here);
            }
            self.go_to_heading(heading);
            return;
        }
        // Refused before a stop is left that leads back from nowhere.
        if self.held_by_rename() {
            return;
        }
        // A document with no file has nowhere to come back to.
        if here.path.is_some() {
            self.back.push(here);
        }
        self.pending_heading = heading.map(str::to_string);
        self.open_or_confirm(found);
    }

    /// Returns to where the last followed link was followed from. Another document reopens
    /// where it was left, since leaving it saved its place.
    fn go_back(&mut self) {
        let Some(stop) = self.back.pop() else {
            self.notice = Some("no link to go back from".to_string());
            return;
        };
        if same_document(stop.path.as_deref(), self.session.path()) {
            self.session.set_caret(stop.caret);
            self.pending_scroll = Some(stop.scroll);
            self.notice = None;
            return;
        }
        let Some(path) = stop.path else {
            return;
        };
        // Opening a missing path would create it.
        if !path.is_file() {
            self.notice = Some(format!("{} is no longer there", path.display()));
            return;
        }
        self.pending_heading = None;
        self.open_or_confirm(path);
    }

    /// Moves to the heading a document link named, and says so if it isn't there.
    fn go_to_heading(&mut self, heading: Option<&str>) {
        let Some(heading) = heading else {
            return;
        };
        match self.session.heading_cursor(heading) {
            Some(cursor) => {
                self.session.set_caret(cursor);
                self.notice = None;
                // The place this document was left doesn't apply when a link names one.
                self.session.restored_scroll = None;
                self.reveal_caret = true;
            }
            None => self.notice = Some(format!("no heading \"{heading}\" in this document")),
        }
    }

    /// Opens a file chosen in a dialog, holding at the Open prompt's confirmation
    /// if work would be lost.
    fn open_picked(&mut self, path: PathBuf) {
        self.pending_heading = None;
        self.open_or_confirm(path);
    }

    fn open_or_confirm(&mut self, path: PathBuf) {
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
        let heading = self.pending_heading.take();
        if self.held_by_rename() {
            return;
        }
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
        // Before the open, which may be of this same file.
        self.save_position_now();
        self.sync_logs();
        if let Err(err) = kame_sync::logs::build_from_logs(&path) {
            self.session.last_error = Some(format!(
                "could not build {} from its logs — {err}",
                path.display()
            ));
            return;
        }
        match Session::open_any(&path) {
            Ok((session, note)) => {
                self.adopt_session(session);
                // An import has no file yet, so it is like a new document.
                if self.session.is_anonymous() {
                    self.last_document.forget();
                    self.persist();
                }
                self.notice = Some(note.unwrap_or_else(|| format!("opened {}", path.display())));
                self.note_fresh_opened();
                self.go_to_heading(heading.as_deref());
                self.apply_pending_place();
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
        let decoded = image::open(image_file(path, self.session.path()))
            .ok()?
            .to_rgba8();
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
        self.save_with_dialog(ctx, close_after);
    }

    /// Asks where to save the document, then saves it there. Cancelling changes
    /// nothing, and leaves the close prompt up if it was what asked.
    fn save_with_dialog(&mut self, ctx: &egui::Context, close_after: bool) {
        if self.held_by_rename() {
            return;
        }
        let directory = self.session.dialog_directory();
        let name = self.session.suggested_file_name();
        let Some(path) = pick_save_path(&name, directory.as_deref()) else {
            // The author cancelled the save; the close prompt stays open so
            // they can still discard or cancel explicitly.
            return;
        };
        self.save_to(ctx, &path, close_after);
    }

    /// Saves the document at `path`, and says its images and links again from there.
    fn save_to(&mut self, ctx: &egui::Context, path: &Path, close_after: bool) {
        let from = self.session.path().map(Path::to_path_buf);
        match self.session.save_as(path) {
            Ok(()) => {
                let notebook = kame_notebook::Notebook::find(path).ok().flatten();
                self.session.rehome_images(
                    from.as_deref(),
                    notebook.as_ref().map(|nb| nb.root.as_path()),
                );
                self.session.rehome_links(from.as_deref());
                self.notice = Some(format!("saved {}", path.display()));
                if close_after {
                    self.finish_close(ctx);
                }
            }
            Err(err) => {
                self.session.last_error =
                    Some(format!("could not save {} — {err}", path.display()));
            }
        }
    }

    /// Keeps the revision logs up to date, on a pause in typing or on leaving or coming back to
    /// the window, so that a merge never lands mid-word and nothing interrupts a dialog or an open
    /// note.
    fn tick_logs(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        let typing = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Text(_)
                        | egui::Event::Key { .. }
                        | egui::Event::Paste(_)
                        | egui::Event::Cut
                        | egui::Event::Ime(_)
                )
            })
        });
        if typing {
            self.last_typed = now;
        }
        let focused = ctx.input(|input| input.focused);
        let focus_changed = self.was_focused != focused;
        self.was_focused = focused;
        // A long gap means the machine slept or the window was hidden, so this is a return too. The
        // copy left open is about to be written in, which shouldn't start from stale text.
        let returned = now.duration_since(self.last_log_tick).as_secs_f32() > LOG_RETURN_SECONDS;
        self.last_log_tick = now;
        if now < self.next_log_check && !focus_changed && !returned {
            return;
        }
        self.next_log_check = now + std::time::Duration::from_secs_f32(LOG_CHECK_SECONDS);
        let paused = self.last_typed.elapsed().as_secs_f32() >= LOG_PAUSE_SECONDS;
        if (paused || focus_changed || returned) && !self.dialog_open() && self.open_note.is_none()
        {
            self.sync_logs();
        }
    }

    /// Works out this machine's name in revision logs, the first time it's wanted.
    fn ensure_device(&mut self) {
        if self.device.is_empty() {
            self.device = kame_sync::logs::device_name(&kame_sync::logs::this_host());
            self.persist();
        }
    }

    /// Writes what's here to the document's logs and merges whatever has arrived there.
    fn sync_logs(&mut self) {
        let Some(path) = self.session.path().map(Path::to_path_buf) else {
            return;
        };
        // An untouched new document may yet be taken away, so it leaves nothing to take back.
        if self.on_untouched_fresh() {
            return;
        }
        if kame_sync::logs::orphaned_logs(&path) && !self.orphan_mentioned {
            self.orphan_mentioned = true;
            self.notice = Some(format!(
                "this document has revision logs, but no {} above it, so it may have stopped \
                 syncing",
                kame_sync::logs::MARKER
            ));
        }
        if self.logged_path.as_deref() != Some(&path) || self.logged.is_none() {
            self.logged = None;
            if kame_sync::logs::logging_root(&path).is_some() {
                self.ensure_device();
                match kame_sync::logs::Logged::try_open(&self.session, &self.device) {
                    Ok(logged) => self.logged = Some(logged),
                    Err(err) if !self.unreadable_mentioned => {
                        self.unreadable_mentioned = true;
                        self.notice_tracker.announced();
                        self.notice = Some(format!(
                            "this document's revision logs can't be read, so it isn't syncing: \
                             {err}"
                        ));
                    }
                    Err(_) => {}
                }
            }
            self.logged_path = Some(path);
        }
        let Some(mut logged) = self.logged.take() else {
            return;
        };
        if logged.needs_sync(&self.session) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs() as i64);
            match logged.sync(&mut self.session, now) {
                Ok(outcome) => {
                    if outcome.stranded && !self.stranded_mentioned {
                        self.stranded_mentioned = true;
                        self.notice_tracker.announced();
                        self.notice = Some(
                            "this document was moved or deleted on another machine, and isn't \
                             syncing"
                                .to_string(),
                        );
                    }
                    if outcome.others_logs && !self.stranded_mentioned {
                        self.stranded_mentioned = true;
                        self.notice_tracker.announced();
                        self.notice = Some(
                            "the logs at this path may be another document's, so this one isn't \
                             syncing. Save As gives it logs of its own"
                                .to_string(),
                        );
                    }
                    if outcome.merged {
                        self.set_open_note(None);
                        self.notice_tracker.announced();
                        self.notice = Some(logs_notice(outcome.from.as_deref(), outcome.notes));
                    }
                }
                Err(err) => {
                    self.session.last_error =
                        Some(format!("could not update this document's logs — {err}"));
                }
            }
        }
        self.logged = Some(logged);
    }

    /// Says so when a sync tool leaves a conflict copy of this document beside it.
    fn look_for_conflict_copies(&mut self) {
        if std::time::Instant::now() < self.next_copy_check {
            return;
        }
        self.next_copy_check =
            std::time::Instant::now() + std::time::Duration::from_secs_f32(COPY_CHECK_SECONDS);
        let Some(path) = self.session.path().map(Path::to_path_buf) else {
            return;
        };
        let found = kame_sync::files::conflict_copies(&path);
        if found
            .iter()
            .any(|copy| !self.conflict_copies.contains(copy))
        {
            self.notice = Some(
                "another version of this document is here: File > Merge Conflict Copies"
                    .to_string(),
            );
        }
        self.conflict_copies = found;
    }

    /// Merges every conflict copy into this document. Each is one undo step, after a snapshot.
    fn merge_conflict_copies(&mut self) {
        let (mut merged, mut notes) = (0, 0);
        for copy in std::mem::take(&mut self.conflict_copies) {
            match kame_sync::files::merge_copy_into(&mut self.session, &copy) {
                Ok(result) => {
                    merged += 1;
                    notes += result.notes;
                }
                Err(err) => {
                    self.session.last_error =
                        Some(format!("could not merge {} — {err}", copy.display()));
                }
            }
        }
        self.set_open_note(None);
        if merged > 0 {
            self.notice_tracker.announced();
            self.notice = Some(merged_notice(notes));
        }
    }

    /// Merges what is on the screen with the file that replaced this one, and carries on in that
    /// file. On failure these words stay on the screen, with the prompt still up.
    fn merge_with_replacement(&mut self) {
        if self.held_by_rename() {
            return;
        }
        match kame_sync::files::merge_replaced(&self.session) {
            Ok((session, result)) => {
                let (caret, scroll) = (self.session.caret, self.page_scroll);
                self.adopt_session(session);
                self.session.set_caret(caret);
                self.pending_scroll = Some(scroll);
                self.replaced_prompt = false;
                self.notice = Some(merged_notice(result.notes));
            }
            Err(err) => self.session.last_error = Some(format!("could not merge — {err}")),
        }
    }

    /// What to do about a file replaced on disk: merge the two, keep these words under a new
    /// name, or take what is there now. Each leaves the version on disk where it is, so none can
    /// lose both, and the error strip brings this back after Not Now.
    fn show_replaced_prompt(&mut self, ctx: &egui::Context) {
        if !self.replaced_prompt {
            return;
        }
        let Some(path) = self.session.path().map(Path::to_path_buf) else {
            self.replaced_prompt = false;
            return;
        };
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let there = path.is_file();
        let mut merge = false;
        let mut save = false;
        let mut open = false;
        let mut later = false;
        egui::Window::new("File changed on disk")
            .id(Id::unique("kame-replaced-prompt"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.label(if there {
                    format!("{name} was replaced, probably by a sync from another machine.")
                } else {
                    format!("{name} is no longer there.")
                });
                ui.label(
                    "Nothing more can be saved to it. What is on this screen is safe for now.",
                );
                ui.horizontal(|ui| {
                    if there {
                        merge = ui.button("Merge them").clicked();
                    }
                    save = ui.button("Save mine as…").clicked();
                    if there {
                        open = ui.button("Open the one on disk").clicked();
                    }
                    later = ui.button("Not now").clicked();
                });
            });
        if merge {
            self.merge_with_replacement();
        } else if save {
            self.save_with_dialog(ctx, false);
            self.replaced_prompt = self.session.replaced;
        } else if open {
            self.replaced_prompt = false;
            self.open_document(path.display().to_string());
        } else if later {
            self.replaced_prompt = false;
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
            self.finish_close(ctx);
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
        self.open_link_prompt(block_id, start, end, current);
    }

    /// Opens the link prompt on `start..end`, with the words there and the link's URL, if any.
    fn open_link_prompt(
        &mut self,
        block_id: BlockId,
        start: usize,
        end: usize,
        current: Option<String>,
    ) {
        let words = self
            .session
            .block_index(block_id)
            .and_then(|index| self.session.document().blocks[index].text.get(start..end))
            .unwrap_or_default()
            .to_string();
        self.snapshot_prompt = None;
        self.open_prompt = None;
        self.restore_picker = None;
        self.link_prompt = Some(LinkPrompt {
            block_id,
            start,
            end,
            words,
            editing: current.is_some(),
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
        let mut words = prompt.words.clone();
        let title = if prompt.editing {
            "Edit link"
        } else {
            "Add link"
        };
        let focus = prompt.focus;
        let mut submit = enter && !focus;
        let mut cancel = false;

        egui::Window::new(title)
            .id(Id::unique("kame-link-prompt"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.label("Text:");
                ui.add(
                    egui::TextEdit::singleline(&mut words)
                        .id(Id::unique("kame-link-words"))
                        .desired_width(360.0)
                        .hint_text("The link itself, if left empty"),
                );
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
            prompt.words = words;
            prompt.focus = false;
        }
        if cancel {
            self.link_prompt = None;
            return;
        }
        if !submit {
            return;
        }
        if let Some(prompt) = self.link_prompt.take() {
            self.session.edit_link(
                prompt.block_id,
                prompt.start,
                prompt.end,
                &prompt.words,
                &prompt.url,
            );
        }
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
    /// egui moves the keyboard focus on Tab before any of kame's code sees the frame,
    /// so a block that takes tabs has to claim the key here.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.claim_tabs(ctx, &mut raw_input.events);
    }

    fn on_exit(&mut self) {
        self.sync_logs();
        if self.on_untouched_fresh() {
            if let Ok(empty) = Session::anonymous() {
                self.session = empty;
                self.clear_left_fresh(true);
            }
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // The OS asking to quit goes through the same close request as the window's
        // own button, and so through the unsaved-document prompt.
        let requests = external::take();
        if requests.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // kame shows one document, so of several files opened at once the first wins.
        if let Some(path) = requests.open.into_iter().next() {
            self.open_picked(path);
        }
        // Here because eframe skips `ui` while the window is hidden, and before it so that a key
        // pressed on coming back lands in text that's up to date.
        self.tick_logs(ctx);
        self.notebook_tick(ctx);
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested {
            self.save_position_now();
        }
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
        if self.close.cancel_close(
            self.session.save_failed && !self.session.newer,
            close_requested,
        ) {
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
            hyphenate: self.hyphenate,
            ..base
        };
        if self.renderer.set_config(config) {
            // Everything cached was shaped and rasterized at the old measure.
            self.layouts.clear();
            self.textures.clear();
        }
        let browser_showing =
            self.nb.browser_open && self.nb.notebook.is_some() && !self.modes.hide_chrome;
        let beside_browser = browser_showing.then(|| {
            notebook::page_left_beside_browser(
                page_geometry(config, available).body_left,
                body_width,
                margin_width,
                available,
            )
        });
        self.nb.page_left = beside_browser.flatten();
        self.nb.page_hidden = beside_browser.is_some_and(|left| left.is_none());
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
        self.show_browser(ui);
        // The typewriter's centring offset was measured last frame, against the
        // same geometry, and is applied here where the scroll area is built.
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(offset) = self.session.restored_scroll.take() {
            self.pending_scroll = Some(offset);
        }
        if let Some(offset) = self.pending_scroll.take() {
            scroll = scroll.vertical_scroll_offset(offset);
        }
        if !self.nb.page_hidden {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(self.palette.background))
                .show(ui, |ui| {
                    self.page_scroll = scroll
                        .show(ui, |ui| self.draw_surface(ui))
                        .state
                        .clamped_offset()
                        .y;
                    // The save-failure signal belongs to the page, not the chrome:
                    // it is painted in this panel so it outlives hide-chrome.
                    if let Some(error) = &error {
                        // A failed write's strip retries the save; any other message
                        // (a degraded open, a failed export) is dismissed, so it
                        // cannot sit there offering a write that does nothing.
                        let retry = self.session.save_failed && !self.session.newer;
                        if draw_error_indicator(ui, error, retry, self.palette.error_ink) {
                            self.session.retry_or_dismiss();
                            self.replaced_prompt = self.session.replaced;
                        }
                    }
                });
        }
        // Dialogs are drawn last, from the root: above the page, and available
        // even while the chrome (and with it the buttons) is hidden. The windows for references
        // come first, to see what's over them before an Enter closes it.
        self.show_reference_windows(ui.ctx());
        self.show_dialogs(ui.ctx());
        self.show_notebook_lists(ui.ctx());
        self.open_pending_backlink();
        self.show_replaced_prompt(ui.ctx());
        self.show_site_report(ui.ctx());
        self.show_citation(ui.ctx());
        self.remember_document();
        self.keep_position(ui.ctx());
    }
}

/// Where tests put their files, gone when the test process ends.
#[cfg(test)]
pub(crate) fn test_temp_dir() -> std::path::PathBuf {
    kame_test_dir::root().to_path_buf()
}

/// An app with no window, driven one frame at a time. It draws the page and runs what a frame
/// runs around it, leaving out what writes kame's settings.
#[cfg(test)]
pub(crate) struct Headless {
    pub(crate) app: KameApp,
    ctx: egui::Context,
    time: f64,
    /// What the last frame drew, as a screen reader is told it, for finding a window's buttons.
    drawn: Option<egui::accesskit::TreeUpdate>,
    /// How tall the window is. It is `HEIGHT` unless a test shrinks it.
    height: f32,
}

#[cfg(test)]
impl Headless {
    const WIDTH: f32 = 1000.0;
    const HEIGHT: f32 = 800.0;

    pub(crate) fn new(session: Session) -> Headless {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let creation = eframe::CreationContext::_new_kittest(ctx.clone());
        let app = KameApp::new(&creation, session, None, KameConfig::default());
        Headless {
            app,
            ctx,
            time: 0.0,
            drawn: None,
            height: Self::HEIGHT,
        }
    }

    /// One frame, in the order `logic` and `ui` run things, with `events` having just happened.
    pub(crate) fn frame(&mut self, events: Vec<egui::Event>) {
        self.time += 0.02;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(Self::WIDTH, self.height),
            )),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let app = &mut self.app;
        app.notebook_tick(&self.ctx);
        let mut output = self.ctx.run_ui(input, |ui| {
            app.handle_input(ui.ctx());
            let history = app.session.history();
            app.notice_tracker.update(&mut app.notice, history);
            app.show_browser(ui);
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
                    if let Some(offset) = app.pending_scroll.take() {
                        scroll = scroll.vertical_scroll_offset(offset);
                    }
                    app.page_scroll = scroll
                        .show(ui, |ui| app.draw_surface(ui))
                        .state
                        .clamped_offset()
                        .y;
                });
            app.show_reference_windows(ui.ctx());
            app.show_dialogs(ui.ctx());
            app.show_notebook_lists(ui.ctx());
            app.show_citation(ui.ctx());
        });
        self.drawn = output.platform_output.accesskit_update.take();
        output.drop_without_applying_deltas();
    }

    /// The window on top of the others.
    pub(crate) fn top_layer(&self) -> Option<egui::LayerId> {
        self.ctx
            .memory(|memory| memory.areas().top_layer_id(egui::Order::Foreground))
    }

    /// A press and a release at `pos`, with the pointer having come there first.
    fn press_and_release(&mut self, pos: Pos2) {
        self.frame(vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
    }

    /// A click at `pos`, long enough after the last one not to make a double click.
    pub(crate) fn click(&mut self, pos: Pos2) {
        self.time += 1.0;
        self.press_and_release(pos);
    }

    pub(crate) fn double_click(&mut self, pos: Pos2) {
        self.click(pos);
        self.press_and_release(pos);
    }

    /// Whether the last frame drew words holding `text`.
    pub(crate) fn sees(&self, text: &str) -> bool {
        self.drawn
            .iter()
            .flat_map(|tree| &tree.nodes)
            .any(|(_, node)| {
                [node.label(), node.value()]
                    .into_iter()
                    .flatten()
                    .any(|words| words.contains(text))
            })
    }

    /// Whether the last frame drew something saying `label` that can be pressed.
    pub(crate) fn can_press(&self, label: &str) -> bool {
        self.drawn
            .iter()
            .flat_map(|tree| &tree.nodes)
            .any(|(_, node)| {
                node.label() == Some(label) && node.bounds().is_some() && !node.is_disabled()
            })
    }

    /// Clicks the button saying `label`, wherever it's drawn. A centred window goes by its size
    /// on the frame before, so it takes a couple to settle after growing.
    pub(crate) fn press(&mut self, label: &str) {
        for _ in 0..2 {
            self.frame(Vec::new());
        }
        let bounds = self
            .drawn
            .iter()
            .flat_map(|tree| &tree.nodes)
            .find_map(|(_, node)| node.bounds().filter(|_| node.label() == Some(label)));
        let Some(bounds) = bounds else {
            panic!("no button saying {label:?}");
        };
        self.click(Pos2::new(
            (bounds.x0 + bounds.x1) as f32 / 2.0,
            (bounds.y0 + bounds.y1) as f32 / 2.0,
        ));
    }

    /// Where the last frame drew something saying `label`, as (top, bottom).
    pub(crate) fn drawn_at(&self, label: &str) -> Option<(f32, f32)> {
        self.drawn
            .iter()
            .flat_map(|tree| &tree.nodes)
            .find_map(|(_, node)| node.bounds().filter(|_| node.label() == Some(label)))
            .map(|bounds| (bounds.y0 as f32, bounds.y1 as f32))
    }

    /// Right-clicks what says `label`, wherever it's drawn, for the menu it has.
    pub(crate) fn right_click(&mut self, label: &str) {
        self.frame(Vec::new());
        let bounds = self
            .drawn
            .iter()
            .flat_map(|tree| &tree.nodes)
            .find_map(|(_, node)| node.bounds().filter(|_| node.label() == Some(label)));
        let Some(bounds) = bounds else {
            panic!("nothing saying {label:?}");
        };
        let pos = Pos2::new(
            (bounds.x0 + bounds.x1) as f32 / 2.0,
            (bounds.y0 + bounds.y1) as f32 / 2.0,
        );
        self.time += 1.0;
        self.frame(vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
        self.frame(Vec::new());
    }

    /// Where the text starts across the page.
    pub(crate) fn body_left(&self) -> f32 {
        page_geometry(self.app.renderer.config(), Self::WIDTH).body_left
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kame_core::links::find_document;

    fn temp_db_path() -> PathBuf {
        let dir = test_temp_dir().join(format!("kame-gui-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("doc.kame")
    }

    fn open(path: &Path) -> Session {
        Session::open(path).expect("open a session")
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
        assert_eq!(
            page_measure(false, 560.0),
            (560.0 - 2.0 * COLLAPSED_GUTTER, 0.0)
        );
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
            COLLAPSED_GUTTER
        );
    }

    #[test]
    fn kame_config_round_trips_and_defaults_the_body_size() {
        let config = KameConfig {
            body_size: 21.0,
            dark: true,
            last_document: Some(PathBuf::from("/notes/essay.kame")),
            author: "K. Isom".to_string(),
            hyphenate: false,
            check_spelling: true,
            words: vec!["kame".to_string()],
            rules_off: vec!["Rule".to_string()],
            device: "talos-3f9a".to_string(),
            browser_open: true,
            browser_folders: vec!["maritain".to_string()],
        };
        let json = serde_json::to_string(&config).unwrap();
        let read = serde_json::from_str::<KameConfig>(&json).unwrap();
        assert_eq!(read.body_size, 21.0);
        assert!(read.dark);
        assert_eq!(read.last_document, config.last_document);
        assert_eq!(read.author, "K. Isom");
        assert!(!read.hyphenate);
        assert!(read.check_spelling);
        assert_eq!(read.words, vec!["kame"]);
        assert_eq!(read.rules_off, vec!["Rule"]);

        // A config written by an older build (no body_size, no dark) still
        // parses and defaults to light, body-size 16, hyphenating.
        let old = serde_json::from_str::<KameConfig>("{}").unwrap();
        assert_eq!(old.body_size, DEFAULT_BODY_SIZE);
        assert!(!old.dark);
        assert!(old.hyphenate);
        assert!(!old.check_spelling);
        assert!(old.words.is_empty());
        assert!(KameConfig::default().hyphenate);
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
            block_height(&block, &layout, &config, None)
        };
        let (one, two, three) = (height(1), height(2), height(3));
        assert!(one > two && two > three, "{one} {two} {three}");
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
                palette.selection_fill,
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
        for wash in [
            palette.footnote_highlight,
            palette.marginal_highlight,
            palette.selection_fill,
        ] {
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
    fn a_menu_style_is_checked_only_for_its_own_kind() {
        let body = BlockKind::Paragraph;
        assert!(is_block_style(&body, &BlockKind::Paragraph));
        assert!(!is_block_style(&body, &BlockKind::BlockQuote));
        assert!(!is_block_style(&body, &BlockKind::Prose));
    }

    #[test]
    fn a_heading_checks_only_the_style_of_its_level() {
        assert!(is_block_style(&heading_kind(2), &heading_kind(2)));
        assert!(!is_block_style(&heading_kind(2), &heading_kind(1)));
    }

    #[test]
    fn a_nested_list_item_checks_the_style_of_its_list() {
        let nested = BlockKind::ListItem {
            ordered: true,
            depth: 2,
        };
        let numbered = BlockKind::ListItem {
            ordered: true,
            depth: 0,
        };
        let bulleted = BlockKind::ListItem {
            ordered: false,
            depth: 0,
        };
        assert!(is_block_style(&nested, &numbered));
        assert!(!is_block_style(&nested, &bulleted));
    }

    #[test]
    fn a_code_block_in_a_named_language_checks_the_code_style() {
        let rust = BlockKind::Code {
            language: Some("rust".into()),
        };
        assert!(is_block_style(&rust, &BlockKind::Code { language: None }));
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

    /// The page leaves room by this height, so reading the file from the wrong folder runs the
    /// next block over the picture.
    #[test]
    fn a_relative_image_takes_its_own_height_on_the_page() {
        let dir = test_temp_dir().join(format!("kame-img-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("images")).unwrap();
        image::RgbaImage::from_pixel(40, 200, image::Rgba([0, 0, 0, 255]))
            .save(dir.join("images/tall.png"))
            .unwrap();
        let config = LayoutConfig::with_body_size(16.0);
        let mut renderer = GlyphRenderer::new(config);
        let block = Block {
            id: uuid::Uuid::new_v4(),
            kind: BlockKind::Image {
                path: "images/tall.png".to_string(),
                link: None,
                align: Default::default(),
            },
            text: String::new(),
            spans: Vec::new(),
        };
        let layout = renderer.layout(&block);
        let document = dir.join("essay.kame");
        assert_eq!(
            block_height(&block, &layout, &config, Some(&document)),
            200.0
        );
        std::fs::remove_dir_all(&dir).unwrap();
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
    fn a_relative_image_is_measured_from_the_documents_folder() {
        let dir = test_temp_dir().join(format!("kame-img-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("static")).unwrap();
        std::fs::create_dir_all(dir.join("maritain")).unwrap();
        image::RgbaImage::from_pixel(200, 100, image::Rgba([0, 0, 0, 255]))
            .save(dir.join("static/wide.png"))
            .unwrap();
        let document = dir.join("maritain/essay.kame");
        let file = image_file("../static/wide.png", Some(&document));
        assert_eq!(image_size(&file, 600.0), Vec2::new(200.0, 100.0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_image_reserves_a_short_placeholder() {
        let height = image_size(Path::new("/does/not/exist.png"), 600.0).y;
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
    fn tags_split_on_commas_and_drop_blanks() {
        assert_eq!(parse_tags(" life, work ,,"), vec!["life", "work"]);
        assert!(parse_tags("  ").is_empty());
    }

    #[test]
    fn a_footnote_lands_in_the_upper_part_of_the_window() {
        assert_eq!(footnote_scroll_offset(3000.0, 900.0), 2700.0);
    }

    #[test]
    fn a_footnote_near_the_top_of_the_page_does_not_scroll_above_it() {
        assert_eq!(footnote_scroll_offset(100.0, 900.0), 0.0);
    }

    #[test]
    fn clicking_a_superscript_finds_its_footnote() {
        let marks = [
            (
                Rect::from_min_max(Pos2::new(10.0, 10.0), Pos2::new(16.0, 20.0)),
                500.0,
            ),
            (
                Rect::from_min_max(Pos2::new(40.0, 10.0), Pos2::new(46.0, 20.0)),
                600.0,
            ),
        ];
        assert_eq!(footnote_mark_at(&marks, Pos2::new(42.0, 15.0)), Some(600.0));
        assert_eq!(footnote_mark_at(&marks, Pos2::new(12.0, 15.0)), Some(500.0));
    }

    /// A digit is tiny, so the target reaches a little past it.
    #[test]
    fn a_superscript_is_forgiving_to_click() {
        let marks = [(
            Rect::from_min_max(Pos2::new(10.0, 10.0), Pos2::new(16.0, 20.0)),
            500.0,
        )];
        assert_eq!(footnote_mark_at(&marks, Pos2::new(8.0, 8.0)), Some(500.0));
        assert_eq!(footnote_mark_at(&marks, Pos2::new(30.0, 15.0)), None);
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = test_temp_dir().join(format!("kame-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn clicking_a_linked_image_finds_its_link() {
        let block = Block {
            id: uuid::Uuid::new_v4(),
            kind: BlockKind::Image {
                path: "a.png".to_string(),
                link: Some("large.png".to_string()),
                align: Default::default(),
            },
            text: String::new(),
            spans: Vec::new(),
        };
        let cursor = Cursor {
            block_id: block.id,
            offset: 0,
        };
        assert_eq!(url_at(&[block], &cursor), Some("large.png".to_string()));
    }

    #[test]
    fn a_click_opens_ordinary_links_and_local_files() {
        for url in [
            "https://example.com",
            "mailto:k@example.com",
            "paper.pdf",
            "pics/a.png",
            "/files/paper.pdf",
            "file:///home/kyle/paper.pdf",
        ] {
            assert!(may_open(url), "{url}");
        }
    }

    /// A leading dash reads as an option to the program that opens the link.
    #[test]
    fn a_click_refuses_what_would_be_read_as_an_option_or_run_script() {
        for url in ["-a", "--help", "  -a", "\t-a", "javascript:alert(1)", ""] {
            assert!(!may_open(url), "{url:?}");
        }
    }

    #[test]
    fn the_info_plist_declares_kame_documents() {
        let plist = include_str!("../packaging/macos/Info.plist");
        assert!(plist.contains("CFBundleDocumentTypes"), "{plist}");
        assert!(plist.contains("UTExportedTypeDeclarations"), "{plist}");
        assert!(plist.contains("<string>kame</string>"), "{plist}");
    }

    #[test]
    fn the_app_icon_is_kames_own() {
        let icon = app_icon();
        assert!(icon.width > 0 && icon.height > 0);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
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
    fn opening_markdown_imports_it_as_an_unsaved_document() {
        let dir = test_temp_dir().join(format!("kame-import-{}", uuid::Uuid::new_v4()));
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
        assert!(note.contains("1 image not found"), "{note}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replacing_a_document_warns_about_work_with_nowhere_to_go() {
        let empty = Session::anonymous().unwrap();
        assert!(
            !empty.would_lose_work(),
            "an empty document has nothing to lose"
        );

        let dir = test_temp_dir().join(format!("kame-lose-{}", uuid::Uuid::new_v4()));
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
    fn an_image_sits_where_its_alignment_says_across_the_column() {
        assert_eq!(image_left(100.0, 600.0, 200.0, ImageAlign::Left), 100.0);
        assert_eq!(image_left(100.0, 600.0, 200.0, ImageAlign::Center), 300.0);
        assert_eq!(image_left(100.0, 600.0, 200.0, ImageAlign::Right), 500.0);
        // As wide as the column, it has nowhere to go.
        for align in [ImageAlign::Left, ImageAlign::Center, ImageAlign::Right] {
            assert_eq!(image_left(100.0, 600.0, 600.0, align), 100.0);
        }
    }

    #[test]
    fn an_image_is_shown_at_its_own_size_unless_it_is_too_wide() {
        // Write a real 200×100 image to test the aspect ratio.
        let dir = test_temp_dir().join(format!("kame-img-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wide.png");
        image::RgbaImage::from_pixel(200, 100, image::Rgba([0, 0, 0, 255]))
            .save(&path)
            .unwrap();

        // A 2:1 image at 600px wide is 300px tall.
        // Narrower than the column, a 2:1 image is shown at its own size.
        assert_eq!(image_size(&path, 600.0), Vec2::new(200.0, 100.0));
        // Exactly the column's width, it's the same.
        assert_eq!(image_size(&path, 200.0), Vec2::new(200.0, 100.0));
        // Wider than the column, it's scaled to fit and keeps its shape.
        assert_eq!(image_size(&path, 100.0), Vec2::new(100.0, 50.0));
        // An unreadable path reserves a 3:2 box instead of collapsing.
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
            .map(|(block, layout)| block_height(block, layout, &config, None))
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
            &[],
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
            &[],
            body_left,
            Pos2::new(body_left, gap_y),
        )
        .expect("a click in the gap still lands somewhere");
        assert_ne!(cursor.block_id, blocks[2].id);
        assert!(cursor.block_id == blocks[0].id || cursor.block_id == blocks[1].id);
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
        // A wrapped word's end, so no hyphen comes into it.
        let mut renderer = GlyphRenderer::new(LayoutConfig {
            hyphenate: false,
            ..LayoutConfig::default()
        });
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
    fn exporting_markdown_writes_the_document_beside_the_database() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("A paragraph.");
        session.split();
        session.insert("Another one.");
        let expected = kame_core::export::to_markdown_with(
            session.document(),
            &session.published_metadata(),
            &kame_core::references::References::new(),
        );
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

    fn typed(session: &mut Session, text: &str) {
        for c in text.chars() {
            session.insert(&c.to_string());
            session.apply_markdown_shortcut();
        }
    }

    fn only_text(session: &Session, index: usize) -> (BlockKind, String) {
        let block = &session.document().blocks[index];
        (block.kind.clone(), block.text.clone())
    }

    #[test]
    fn a_fence_and_enter_starts_a_code_block() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "```rust");
        session.split();
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(
            only_text(&session, 0),
            (
                BlockKind::Code {
                    language: Some("rust".to_string())
                },
                String::new()
            )
        );
        assert_eq!(session.caret.offset, 0);
        typed(&mut session, "fn main()");
        assert_eq!(session.document().blocks[0].text, "fn main()");
    }

    #[test]
    fn an_argument_outranks_the_last_document() {
        let dir = scratch_dir("startup");
        let last = dir.join("last.kame");
        std::fs::write(&last, "").unwrap();
        let named = PathBuf::from("/named.kame");
        assert_eq!(
            document_to_open(Some(named.clone()), Some(last.clone())),
            Some(named)
        );
        assert_eq!(document_to_open(None, Some(last.clone())), Some(last));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Opening a missing path creates it, which is not what reopening should do.
    #[test]
    fn a_last_document_that_has_gone_is_not_reopened() {
        let gone = test_temp_dir().join(format!("kame-gone-{}.kame", uuid::Uuid::new_v4()));
        assert_eq!(document_to_open(None, Some(gone.clone())), None);
        assert!(!gone.exists());
        assert_eq!(document_to_open(None, None), None);
    }

    #[test]
    fn an_older_config_has_no_last_document() {
        let config: KameConfig = serde_json::from_str(r#"{"body_size":18.0,"dark":true}"#).unwrap();
        assert_eq!(config.last_document, None);
        assert_eq!(config.body_size, 18.0);
    }

    #[test]
    fn only_cmd_n_is_the_new_chord() {
        let key = |key, pressed, repeat, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers,
        };
        let command = egui::Modifiers::COMMAND;
        let shift = egui::Modifiers {
            shift: true,
            ..command
        };
        assert!(is_new_chord(&key(egui::Key::N, true, false, command)));
        assert!(
            !is_new_chord(&key(egui::Key::N, true, false, shift)),
            "that adds a note"
        );
        assert!(!is_new_chord(&key(
            egui::Key::N,
            true,
            false,
            egui::Modifiers::NONE
        )));
        assert!(!is_new_chord(&key(egui::Key::N, false, false, command)));
        assert!(!is_new_chord(&key(egui::Key::N, true, true, command)));
        assert!(!is_new_chord(&key(egui::Key::M, true, false, command)));
    }

    #[test]
    fn a_new_document_only_takes_over_the_prompt_until_it_is_answered() {
        let mut flow = CloseFlow::default();
        flow.request_new();
        assert!(flow.prompting);
        assert!(flow.finish(), "the answer goes to the new document");
        assert!(!flow.prompting);
        assert!(!flow.confirmed);
        assert!(
            flow.on_close_request(true),
            "starting a document must not disarm the guard on closing"
        );
        assert!(!flow.finish(), "this answer closes the window");
        assert!(flow.confirmed);
    }

    #[test]
    fn cancelling_a_new_document_forgets_it() {
        let mut flow = CloseFlow::default();
        flow.request_new();
        flow.cancel();
        flow.on_close_request(true);
        assert!(
            !flow.finish(),
            "a later close must not start a document instead"
        );
    }

    #[test]
    fn closing_the_window_takes_over_a_prompt_for_a_new_document() {
        let mut flow = CloseFlow::default();
        flow.request_new();
        assert!(flow.on_close_request(true));
        assert!(!flow.finish());
    }

    #[test]
    fn only_the_first_kame_holds_the_instance_lock() {
        let dir = scratch_dir("instance");
        let lock = dir.join("instance.lock");
        assert!(hold_instance_lock(&lock));
        assert!(!hold_instance_lock(&lock));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn type_marked(session: &mut Session, text: &str) {
        for c in text.chars() {
            session.insert(&c.to_string());
            session.apply_inline_markdown();
            session.apply_markdown_link();
        }
    }

    #[test]
    fn a_tab_key_becomes_a_typed_tab() {
        let key = |pressed, modifiers| egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        let shift = egui::Modifiers::SHIFT;
        let mut events = vec![
            key(true, egui::Modifiers::NONE),
            key(false, egui::Modifiers::NONE),
            key(true, shift),
            key(true, egui::Modifiers::COMMAND),
            egui::Event::Text("x".to_string()),
        ];
        tabs_to_text(&mut events);
        assert_eq!(
            events,
            vec![
                egui::Event::Text("\t".to_string()),
                key(false, egui::Modifiers::NONE),
                key(true, egui::Modifiers::COMMAND),
                egui::Event::Text("x".to_string()),
            ]
        );
    }

    #[test]
    fn a_tab_key_can_be_dropped_so_it_moves_no_focus() {
        let key = |pressed, modifiers| egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        let mut events = vec![
            key(true, egui::Modifiers::NONE),
            key(true, egui::Modifiers::SHIFT),
            key(false, egui::Modifiers::NONE),
            key(true, egui::Modifiers::COMMAND),
            egui::Event::Text("x".to_string()),
        ];
        drop_tabs(&mut events);
        assert_eq!(
            events,
            vec![
                key(false, egui::Modifiers::NONE),
                key(true, egui::Modifiers::COMMAND),
                egui::Event::Text("x".to_string()),
            ]
        );
    }

    #[test]
    fn a_new_document_forgets_the_last_one() {
        let mut last = LastDocument::new(Some(PathBuf::from("/notes/essay.kame")));
        last.forget();
        assert_eq!(last.path, None);
    }

    #[test]
    fn the_same_file_is_remembered_again_after_a_new_document() {
        let mut last = LastDocument::new(None);
        let file = Path::new("/notes/essay.kame");
        assert!(last.note(file));
        assert!(!last.note(file), "nothing to save the second time");
        last.forget();
        assert!(last.note(file), "reopening it has to count");
        assert_eq!(last.path.as_deref(), Some(file));
    }

    #[test]
    fn a_noted_path_is_made_absolute() {
        let mut last = LastDocument::new(None);
        last.note(Path::new("essay.kame"));
        assert!(last.path.unwrap().is_absolute());
    }

    #[test]
    fn a_click_finds_a_document_link_and_not_a_url() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "see [[a.kame|it]] now");
        let block_id = session.document().blocks[0].id;
        let at = |offset| Cursor { block_id, offset };
        let blocks = &session.document().blocks;
        assert_eq!(document_link_at(blocks, &at(4)), Some("a.kame".to_string()));
        assert_eq!(document_link_at(blocks, &at(3)), None);
        assert_eq!(document_link_at(blocks, &at(6)), None);
        assert_eq!(url_at(blocks, &at(4)), None);
    }

    #[test]
    fn the_same_notice_again_for_a_new_change_is_not_taken_for_the_old_one() {
        let mut tracker = NoticeTracker::default();
        let mut notice = Some("merged from titan".to_string());
        tracker.update(&mut notice, (1, 0));
        tracker.update(&mut notice, (1, 0));
        // Another merge, saying just the same, and changing the history again.
        tracker.announced();
        notice = Some("merged from titan".to_string());
        tracker.update(&mut notice, (2, 0));
        assert_eq!(notice.as_deref(), Some("merged from titan"));
        tracker.update(&mut notice, (2, 0));
        assert_eq!(notice.as_deref(), Some("merged from titan"));
        tracker.update(&mut notice, (3, 0));
        assert_eq!(notice, None, "the next edit still clears it");
    }

    #[test]
    fn a_path_named_at_launch_is_made_full_so_it_means_the_same_later() {
        let full = launch_path(Some("essay.kame".into())).unwrap();
        assert!(full.is_absolute());
        assert!(full.ends_with("essay.kame"));
        assert_eq!(
            launch_path(Some("/already/full.kame".into())),
            Some(PathBuf::from("/already/full.kame"))
        );
        assert_eq!(launch_path(None), None);
    }

    fn site_args(program: &str, args: &[&str]) -> Option<Vec<String>> {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        site_arguments(program.as_ref(), &args)
    }

    #[test]
    fn the_site_tool_is_reached_by_a_word_or_by_the_programs_name() {
        let build = || Some(vec!["build".to_string(), "dir".to_string()]);
        assert_eq!(site_args("kame", &["site", "build", "dir"]), build());
        assert_eq!(
            site_args("/a/kame.app/Contents/MacOS/kame", &["site", "build", "dir"]),
            build()
        );
        assert_eq!(site_args("kame-site", &["build", "dir"]), build());
        assert_eq!(
            site_args("/home/k/.local/bin/kame-site", &["build", "dir"]),
            build()
        );
        assert_eq!(site_args("kame-site", &[]), Some(Vec::new()));
        assert_eq!(site_args("kame", &["site"]), Some(Vec::new()));
    }

    #[test]
    fn anything_else_opens_the_app() {
        assert_eq!(site_args("kame", &[]), None);
        assert_eq!(site_args("kame", &["essay.kame"]), None);
        assert_eq!(
            site_args("kame", &["./site"]),
            None,
            "a file named site, said plainly"
        );
        assert_eq!(site_args("kame", &["essay.kame", "site"]), None);
    }

    #[test]
    fn a_notice_says_where_merged_words_came_from_and_counts_notes() {
        assert_eq!(logs_notice(Some("titan"), 0), "merged from titan");
        assert_eq!(
            logs_notice(Some("titan"), 1),
            "merged from titan, with 1 note to look at"
        );
        assert_eq!(
            logs_notice(None, 2),
            "merged from another machine, with 2 notes to look at"
        );
    }

    #[test]
    fn a_document_target_splits_at_its_first_hash() {
        assert_eq!(split_document_target("a.kame"), ("a.kame", None));
        assert_eq!(split_document_target("a#B c"), ("a", Some("B c")));
        assert_eq!(split_document_target("#B"), ("", Some("B")));
        assert_eq!(split_document_target("a#C#"), ("a", Some("C#")));
        assert_eq!(split_document_target("a#"), ("a", None));
    }

    #[test]
    fn a_document_is_found_from_the_linking_documents_folder() {
        let dir = scratch_dir("links");
        let base = dir.join("from.kame");
        std::fs::create_dir(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes/a.kame"), "").unwrap();
        std::fs::write(dir.join("notes/v1.2.kame"), "").unwrap();
        std::fs::write(dir.join("notes/b.md"), "").unwrap();
        std::fs::write(dir.join("notes/plain"), "").unwrap();
        let notes = dir.join("notes");
        assert_eq!(
            find_document("notes/a.kame", &base),
            Some(notes.join("a.kame"))
        );
        assert_eq!(find_document("notes/a", &base), Some(notes.join("a.kame")));
        assert_eq!(
            find_document("notes/v1.2", &base),
            Some(notes.join("v1.2.kame"))
        );
        assert_eq!(find_document("notes/b", &base), Some(notes.join("b.md")));
        assert_eq!(
            find_document("notes/plain", &base),
            Some(notes.join("plain"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Opening a missing path creates it, which a rotted link must not do.
    #[test]
    fn a_rotted_link_finds_nothing_and_creates_nothing() {
        let dir = scratch_dir("rot");
        let base = dir.join("from.kame");
        assert_eq!(find_document("gone", &base), None);
        assert_eq!(find_document("gone.kame", &base), None);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_style_that_no_longer_fits_its_text_does_not_stop_the_page_drawing() {
        let mut fs = cosmic_text::FontSystem::new();
        let block = Block {
            id: uuid::Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "été".to_string(),
            spans: vec![
                crate::model::Span {
                    start: 1,
                    end: 3,
                    style: SpanStyle::Bold,
                },
                crate::model::Span {
                    start: 2,
                    end: 400,
                    style: SpanStyle::Italic,
                },
                crate::model::Span {
                    start: 300,
                    end: 400,
                    style: SpanStyle::Bold,
                },
            ],
        };
        let config = layout::LayoutConfig::with_body_size(16.0);
        let shaped = layout::layout_block_marked(&mut fs, &block, &config, &[]);
        assert!(!shaped.lines.is_empty());
    }

    #[test]
    fn a_picked_document_under_home_links_from_tilde() {
        let home = Path::new("/home/kyle");
        assert_eq!(
            document_link_target(Path::new("/home/kyle/notes/a.kame"), Some(home), None, None),
            "~/notes/a.kame"
        );
        assert_eq!(
            document_link_target(Path::new("/srv/notes/a.kame"), Some(home), None, None),
            "/srv/notes/a.kame"
        );
        assert_eq!(
            document_link_target(Path::new("/home/kylie/a.kame"), Some(home), None, None),
            "/home/kylie/a.kame"
        );
        assert_eq!(
            document_link_target(Path::new("/srv/a.kame"), None, None, None),
            "/srv/a.kame"
        );
    }

    #[test]
    fn a_link_to_a_document_still_leads_there_after_save_as() {
        let root = kame_test_dir::folder("app-link-save-as")
            .canonicalize()
            .unwrap();
        kame_notebook::init(&root).unwrap();
        std::fs::create_dir_all(root.join("one")).unwrap();
        std::fs::create_dir_all(root.join("two")).unwrap();
        std::fs::create_dir_all(root.join("three/deep")).unwrap();
        let picked = root.join("two/d.kame");
        drop(open(&picked));
        let mut session = open(&root.join("one/a.kame"));
        session.insert("word");
        let mut page = Headless::new(session);
        page.frame(Vec::new());
        page.app
            .session
            .link_to_document("../two/d.kame".to_string(), "d")
            .unwrap();

        let ctx = page.ctx.clone();
        page.app
            .save_to(&ctx, &root.join("three/deep/a.kame"), false);
        page.frame(Vec::new());

        let target = page
            .app
            .session
            .document()
            .blocks
            .iter()
            .flat_map(|block| &block.spans)
            .find_map(|span| match &span.style {
                SpanStyle::DocumentLink { target } => Some(target.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(target, "../../two/d.kame");
        page.app.follow_document_link(&target);
        assert_eq!(page.app.session.path(), Some(picked.as_path()));
        assert!(!root.join("three/two").exists());
    }

    #[test]
    fn a_picked_document_in_the_notebook_links_by_the_way_from_the_document() {
        let root = kame_test_dir::folder("app-link-within")
            .canonicalize()
            .unwrap();
        std::fs::create_dir_all(root.join("one/deep")).unwrap();
        std::fs::create_dir_all(root.join("two")).unwrap();
        let from = root.join("one/a.kame");
        for file in ["one/b.kame", "one/deep/c.kame", "two/d.kame"] {
            std::fs::write(root.join(file), "").unwrap();
        }
        // The home folder holds the notebook, and still isn't what the link goes by.
        let home = root.parent().unwrap();
        let target = |file: &str| {
            document_link_target(&root.join(file), Some(home), Some(&from), Some(&root))
        };
        assert_eq!(target("one/b.kame"), "b.kame");
        assert_eq!(target("one/deep/c.kame"), "deep/c.kame");
        assert_eq!(target("two/d.kame"), "../two/d.kame");

        // Outside the notebook, or with no notebook or no file, it goes by home as it did.
        let outside = kame_test_dir::folder("app-link-outside")
            .canonicalize()
            .unwrap();
        std::fs::write(outside.join("e.kame"), "").unwrap();
        let by_home = |document: Option<&Path>, root: Option<&Path>| {
            document_link_target(&outside.join("e.kame"), Some(home), document, root)
        };
        assert_eq!(
            by_home(Some(&from), Some(&root)),
            document_link_target(&outside.join("e.kame"), Some(home), None, None)
        );
        let inside = root.join("two/d.kame");
        assert_eq!(
            document_link_target(&inside, Some(home), Some(&from), None),
            document_link_target(&inside, Some(home), None, None)
        );
        assert_eq!(
            document_link_target(&inside, Some(home), None, Some(&root)),
            document_link_target(&inside, Some(home), None, None)
        );
    }

    #[test]
    fn a_tilde_target_finds_the_document_it_was_made_from() {
        let dir = std::fs::canonicalize(std::env::var("HOME").unwrap()).unwrap();
        let found = resolve_open_path("~/x.kame", Path::new("/elsewhere/from.kame"));
        assert_eq!(
            found.canonicalize().unwrap_or(dir.join("x.kame")),
            dir.join("x.kame")
        );
    }

    #[test]
    fn a_documents_title_comes_from_its_metadata() {
        let path = temp_db_path();
        let mut session = open(&path);
        let mut metadata = session.metadata.clone();
        metadata.title = "The Essay".to_string();
        session.set_metadata(metadata);
        drop(session);
        assert_eq!(document_title(&path), "The Essay");
    }

    #[test]
    fn a_document_with_no_real_title_goes_by_its_file_name() {
        let dir = scratch_dir("titles");
        let plain = dir.join("my essay.kame");
        let mut db = crate::storage::Db::open(&plain).unwrap();
        db.save_document(&Document::new("Untitled")).unwrap();
        drop(db);
        assert_eq!(document_title(&plain), "my essay");
        let broken = dir.join("broken.kame");
        std::fs::write(&broken, "not a database").unwrap();
        assert_eq!(document_title(&broken), "broken");
        let markdown = dir.join("post.md");
        std::fs::write(&markdown, "# Heading\n").unwrap();
        assert_eq!(document_title(&markdown), "post");
        assert_eq!(
            std::fs::read(&markdown).unwrap(),
            b"# Heading\n",
            "only read"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_wheel_leaves_a_caret_on_the_page_alone() {
        assert_eq!(caret_aim_after_scroll(300.0, 325.0, 100.0, 700.0), None);
    }

    #[test]
    fn the_wheel_brings_a_caret_along_from_above_the_page() {
        let aim = caret_aim_after_scroll(10.0, 35.0, 100.0, 700.0).unwrap();
        assert!(aim > 100.0 && aim < 200.0, "{aim}");
    }

    #[test]
    fn the_wheel_brings_a_caret_along_from_below_the_page() {
        let aim = caret_aim_after_scroll(900.0, 925.0, 100.0, 700.0).unwrap();
        assert!(aim < 700.0 && aim > 600.0, "{aim}");
    }

    #[test]
    fn a_caret_the_wheel_has_placed_is_left_alone_after() {
        // Aiming puts the caret's line around the aim, and the next frame must not move it.
        let aim = caret_aim_after_scroll(900.0, 925.0, 100.0, 700.0).unwrap();
        assert_eq!(
            caret_aim_after_scroll(aim - 12.0, aim + 12.0, 100.0, 700.0),
            None
        );
        let aim = caret_aim_after_scroll(10.0, 35.0, 100.0, 700.0).unwrap();
        assert_eq!(
            caret_aim_after_scroll(aim - 12.0, aim + 12.0, 100.0, 700.0),
            None
        );
    }

    #[test]
    fn a_caret_in_the_column_leaves_a_code_block_alone() {
        assert_eq!(code_scroll_to_show(100.0, 300.0, 400.0), 100.0);
    }

    #[test]
    fn a_caret_past_the_column_scrolls_only_far_enough() {
        // Past the right edge, with a little room beyond it.
        let scroll = code_scroll_to_show(0.0, 700.0, 400.0);
        assert!(scroll > 300.0 && scroll < 400.0, "{scroll}");
        assert!(700.0 - scroll < 400.0);
        // Before the left edge.
        let scroll = code_scroll_to_show(500.0, 300.0, 400.0);
        assert!(scroll < 300.0 && scroll > 200.0, "{scroll}");
    }

    #[test]
    fn the_start_of_a_code_line_scrolls_all_the_way_back() {
        assert_eq!(code_scroll_to_show(500.0, 0.0, 400.0), 0.0);
    }

    #[test]
    fn a_code_block_scrolls_no_further_than_its_widest_line() {
        assert_eq!(clamp_code_scroll(5000.0, 400.0, 1000.0), 600.0);
        assert_eq!(clamp_code_scroll(-20.0, 400.0, 1000.0), 0.0);
        assert_eq!(clamp_code_scroll(50.0, 400.0, 300.0), 0.0);
    }

    #[test]
    fn a_code_block_that_fits_shows_no_edges() {
        assert_eq!(code_overflow_edges(0.0, 400.0, 300.0), (false, false));
        assert_eq!(code_overflow_edges(0.0, 400.0, 400.0), (false, false));
    }

    #[test]
    fn a_code_block_shows_an_edge_where_there_is_more() {
        assert_eq!(code_overflow_edges(0.0, 400.0, 1000.0), (false, true));
        assert_eq!(code_overflow_edges(200.0, 400.0, 1000.0), (true, true));
        assert_eq!(code_overflow_edges(600.0, 400.0, 1000.0), (true, false));
    }

    fn finding_at(start: usize, end: usize) -> Finding {
        Finding {
            start,
            end,
            kind: kame_check::FindingKind::Spelling,
            rule: "Rule".to_string(),
            message: String::new(),
            suggestions: Vec::new(),
        }
    }

    fn checked_block(text: &str) -> (Marks, Block) {
        let block = Block {
            id: uuid::Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: text.to_string(),
            spans: Vec::new(),
        };
        let mut marks = Marks::default();
        marks
            .checked
            .insert(block.id, (block.clone(), vec![finding_at(3, 7)]));
        (marks, block)
    }

    #[test]
    fn a_checked_block_shows_its_marks() {
        let (marks, block) = checked_block("An wrod here.");
        let shown = visible_marks(&marks, &Exceptions::default(), &block, None);
        assert_eq!(shown.len(), 1);
    }

    #[test]
    fn a_block_changed_since_it_was_checked_shows_none() {
        let (marks, mut block) = checked_block("An wrod here.");
        block.text.insert(0, 'X');
        assert!(visible_marks(&marks, &Exceptions::default(), &block, None).is_empty());
    }

    #[test]
    fn a_word_the_caret_is_at_the_end_of_isnt_marked_yet() {
        let (marks, block) = checked_block("An wrod here.");
        let exceptions = Exceptions::default();
        assert!(visible_marks(&marks, &exceptions, &block, Some(7)).is_empty());
        assert_eq!(visible_marks(&marks, &exceptions, &block, Some(5)).len(), 1);
    }

    #[test]
    fn a_listed_word_isnt_marked() {
        let (marks, block) = checked_block("An wrod here.");
        let words = vec!["wrod".to_string()];
        let exceptions = Exceptions {
            words: &words,
            ..Exceptions::default()
        };
        assert!(visible_marks(&marks, &exceptions, &block, None).is_empty());
    }

    #[test]
    fn a_right_click_finds_the_mark_under_it() {
        let findings = [finding_at(3, 7), finding_at(10, 12)];
        assert_eq!(mark_at(&findings, 5).unwrap().start, 3);
        assert_eq!(mark_at(&findings, 12).unwrap().start, 10);
        assert!(mark_at(&findings, 8).is_none());
    }

    #[test]
    fn the_next_mark_goes_round() {
        let marks = [(0, 5), (0, 20), (3, 1)];
        assert_eq!(next_mark(&marks, (0, 0)), Some(0));
        assert_eq!(next_mark(&marks, (0, 5)), Some(1));
        assert_eq!(next_mark(&marks, (1, 0)), Some(2));
        assert_eq!(next_mark(&marks, (3, 1)), Some(0), "back to the first");
        assert_eq!(next_mark(&[], (0, 0)), None);
    }

    #[test]
    fn a_wavy_line_runs_its_whole_length() {
        let points = wavy_line(10.0, 21.0, 100.0);
        assert_eq!(points.first().unwrap().x, 10.0);
        assert_eq!(points.last().unwrap().x, 21.0);
        assert!(points.iter().any(|p| p.y < 100.0) && points.iter().any(|p| p.y > 100.0));
    }

    #[test]
    fn going_back_to_the_text_closes_the_find_bar() {
        assert!(find_bar_closes(true, true, false));
    }

    #[test]
    fn the_find_bar_stays_while_its_field_has_the_keyboard() {
        assert!(!find_bar_closes(false, true, false));
    }

    #[test]
    fn the_find_bars_own_controls_keep_it_open() {
        assert!(!find_bar_closes(true, true, true));
    }

    #[test]
    fn switching_windows_keeps_the_find_bar_open() {
        assert!(!find_bar_closes(true, false, false));
    }

    fn prompt() -> MetadataPrompt {
        MetadataPrompt {
            title: " T ".to_string(),
            author: " A ".to_string(),
            created: "2026-01-01".to_string(),
            published: String::new(),
            tags: "one, two".to_string(),
            words: vec!["kame".to_string(), "Udacity".to_string()],
            new_word: String::new(),
            accepted: vec![AcceptedMark {
                rule: "Rule".to_string(),
                text: "key point".to_string(),
            }],
            default_author: false,
            publish: true,
            focus: false,
        }
    }

    #[test]
    fn the_properties_dialog_saves_the_documents_words() {
        let metadata = metadata_from_prompt(&prompt());
        assert_eq!(metadata.words, vec!["kame", "Udacity"]);
        assert_eq!(metadata.title, "T");
        assert_eq!(metadata.author, "A");
        assert_eq!(metadata.tags, vec!["one", "two"]);
    }

    #[test]
    fn the_properties_dialog_keeps_accepted_marks_it_didnt_forget() {
        let metadata = metadata_from_prompt(&prompt());
        assert_eq!(metadata.accepted.len(), 1);
        let mut forgetting = prompt();
        forgetting.accepted.clear();
        assert!(metadata_from_prompt(&forgetting).accepted.is_empty());
    }

    #[test]
    fn a_word_still_being_typed_is_saved_too() {
        let mut typing = prompt();
        typing.new_word = " MOOCs ".to_string();
        assert_eq!(
            metadata_from_prompt(&typing).words,
            vec!["kame", "MOOCs", "Udacity"]
        );
    }

    #[test]
    fn adding_words_keeps_the_list_sorted_without_repeats() {
        let mut words = vec!["kame".to_string(), "Udacity".to_string()];
        add_words(&mut words, "mantrap");
        assert_eq!(words, vec!["kame", "mantrap", "Udacity"]);
        add_words(&mut words, "KAME");
        assert_eq!(words.len(), 3, "the same word in other capitals");
        add_words(&mut words, " alpha , beta,, ");
        assert_eq!(words, vec!["alpha", "beta", "kame", "mantrap", "Udacity"]);
        add_words(&mut words, "   ");
        assert_eq!(words.len(), 5);
    }

    fn annotation(kind: AnnotationKind, body: &str) -> Annotation {
        Annotation {
            id: uuid::Uuid::new_v4(),
            kind,
            anchor: Anchor::Block(uuid::Uuid::new_v4()),
            body: body.to_string(),
        }
    }

    #[test]
    fn marginals_and_footnotes_are_checked_and_notes_arent() {
        let marginal = annotation(AnnotationKind::Marginal, "A wrod.");
        let block = annotation_block(&marginal).unwrap();
        assert_eq!(block.id, marginal.id);
        assert_eq!(block.text, "A wrod.");
        assert_eq!(block.kind, BlockKind::Paragraph);
        assert!(annotation_block(&annotation(AnnotationKind::Footnote, "A wrod.")).is_some());
        assert!(annotation_block(&annotation(AnnotationKind::Note, "A wrod.")).is_none());
    }

    #[test]
    fn an_empty_annotation_has_nothing_to_check() {
        assert!(annotation_block(&annotation(AnnotationKind::Marginal, "  ")).is_none());
    }

    #[test]
    fn a_suggestion_rewrites_part_of_a_body() {
        assert_eq!(
            body_with_replacement("A wrod here.", 2, 6, "word").as_deref(),
            Some("A word here.")
        );
        assert_eq!(
            body_with_replacement("caf\u{e9}", 4, 5, "x"),
            None,
            "inside a character"
        );
        assert_eq!(body_with_replacement("short", 2, 99, "x"), None);
        assert_eq!(body_with_replacement("short", 3, 2, "x"), None);
    }

    #[test]
    fn a_footnote_returns_to_the_end_of_what_it_marks() {
        let block_id = uuid::Uuid::new_v4();
        let footnote = Annotation {
            anchor: Anchor::range(block_id, 3, 9),
            ..annotation(AnnotationKind::Footnote, "Body.")
        };
        assert_eq!(
            footnote_return_cursor(&footnote),
            Some(Cursor {
                block_id,
                offset: 9
            })
        );
    }

    #[test]
    fn a_footnote_on_a_whole_block_returns_to_its_start() {
        let block_id = uuid::Uuid::new_v4();
        let footnote = Annotation {
            anchor: Anchor::Block(block_id),
            ..annotation(AnnotationKind::Footnote, "Body.")
        };
        assert_eq!(
            footnote_return_cursor(&footnote),
            Some(Cursor {
                block_id,
                offset: 0
            })
        );
    }

    #[test]
    fn a_foot_entrys_number_is_only_its_number() {
        let mut renderer = GlyphRenderer::new(LayoutConfig::default());
        let entry = Block {
            id: uuid::Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "12. The footnote's own words, which aren't the number.".to_string(),
            spans: Vec::new(),
        };
        let layout = renderer.layout_in_column(&entry, 400.0, 12.0);
        let origin = Pos2::new(100.0, 500.0);
        let rect = footnote_number_rect(&layout, 12, origin).expect("a number");
        let number_end = caret_x(&layout, 3);
        assert!(rect.contains(Pos2::new(100.0 + number_end * 0.5, 505.0)));
        assert!(
            !rect.contains(Pos2::new(100.0 + caret_x(&layout, 8), 505.0)),
            "its words"
        );
        assert!(rect.right() < 100.0 + caret_x(&layout, 5));
    }

    #[test]
    fn a_page_key_moves_a_window_less_two_lines() {
        assert_eq!(page_step(600.0, 25.0), 550.0);
    }

    #[test]
    fn a_page_key_in_a_tiny_window_still_moves_a_line() {
        assert_eq!(page_step(30.0, 25.0), 25.0);
    }

    #[test]
    fn a_page_key_moves_the_page_with_the_caret() {
        // The caret went from 300 to 850 with the page 550 further down.
        assert_eq!(
            scroll_after_page_key(100.0, 550.0, 850.0, 875.0, 600.0, 5000.0),
            650.0
        );
        assert_eq!(
            scroll_after_page_key(650.0, -550.0, 300.0, 325.0, 600.0, 5000.0),
            100.0
        );
    }

    #[test]
    fn a_page_key_stops_at_the_edges_of_the_page() {
        assert_eq!(
            scroll_after_page_key(4300.0, 550.0, 4800.0, 4825.0, 600.0, 5000.0),
            4400.0
        );
        assert_eq!(
            scroll_after_page_key(100.0, -550.0, 10.0, 35.0, 600.0, 5000.0),
            0.0
        );
    }

    #[test]
    fn a_page_key_brings_back_a_caret_scrolled_off_the_page() {
        // The reader scrolled away with the wheel, then pressed the key.
        let scroll = scroll_after_page_key(3000.0, 550.0, 700.0, 725.0, 600.0, 5000.0);
        assert!(scroll <= 700.0, "the caret is on the page: {scroll}");
        assert!(
            scroll + 600.0 >= 725.0,
            "the caret is on the page: {scroll}"
        );
    }

    #[test]
    fn a_caret_in_view_leaves_the_page_alone() {
        assert_eq!(scroll_to_show(300.0, 330.0, 100.0, 600.0, 5000.0), None);
    }

    #[test]
    fn a_caret_below_the_page_scrolls_only_far_enough() {
        let offset = scroll_to_show(800.0, 830.0, 100.0, 600.0, 5000.0).unwrap();
        assert_eq!(offset, 830.0 + CARET_ROOM - 600.0);
    }

    #[test]
    fn a_caret_above_the_page_scrolls_only_far_enough() {
        let offset = scroll_to_show(500.0, 530.0, 1000.0, 600.0, 5000.0).unwrap();
        assert_eq!(offset, 500.0 - CARET_ROOM);
    }

    #[test]
    fn following_the_caret_stays_on_the_page() {
        assert_eq!(scroll_to_show(10.0, 40.0, 30.0, 600.0, 5000.0), Some(0.0));
        assert_eq!(
            scroll_to_show(4980.0, 5000.0, 100.0, 600.0, 5000.0),
            Some(4400.0)
        );
        assert_eq!(scroll_to_show(10.0, 40.0, 0.0, 600.0, 300.0), None);
    }

    #[test]
    fn a_click_on_a_url_in_inline_code_opens_nothing() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "`https://a.b`");
        let block_id = session.document().blocks[0].id;
        let at = Cursor {
            block_id,
            offset: 3,
        };
        assert_eq!(url_at(&session.document().blocks, &at), None);
    }

    #[test]
    fn tab_presses_read_in_order_and_ignore_the_rest() {
        let key = |pressed, modifiers| egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        let events = [
            key(true, egui::Modifiers::NONE),
            key(false, egui::Modifiers::NONE),
            key(true, egui::Modifiers::SHIFT),
            key(true, egui::Modifiers::COMMAND),
            egui::Event::Text("x".to_string()),
        ];
        assert_eq!(tab_presses(&events), vec![true, false]);
    }

    // One test, because the queue is a process-wide static and tests run in parallel.
    #[test]
    fn requests_from_the_os_wait_until_they_are_taken() {
        let _ = crate::external::take();
        crate::external::request_open(PathBuf::from("/a.kame"));
        crate::external::request_open(PathBuf::from("/b.kame"));
        crate::external::request_quit();
        let requests = crate::external::take();
        assert_eq!(
            requests.open,
            vec![PathBuf::from("/a.kame"), PathBuf::from("/b.kame")]
        );
        assert!(requests.quit);
        assert_eq!(
            crate::external::take(),
            crate::external::Requests::default()
        );
    }

    fn css_hex(color: Color32) -> String {
        format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
    }

    fn css_hex_of(ink: [u8; 3]) -> String {
        format!("#{:02x}{:02x}{:02x}", ink[0], ink[1], ink[2])
    }

    /// What publishes should look like what was written, so the stylesheet follows the
    /// palette. Change one and this says to change the other.
    #[test]
    fn the_published_stylesheet_uses_kames_palette() {
        let style = kame_core::export::STYLE;
        for palette in [Palette::LIGHT, Palette::DARK] {
            for color in [
                css_hex(palette.background),
                css_hex_of(palette.body_ink),
                css_hex_of(palette.apparatus_ink),
                css_hex(palette.rule_ink),
                css_hex(palette.footnote_mark),
            ] {
                assert!(style.contains(&color), "{color} isn't in the stylesheet");
            }
        }
    }

    #[test]
    fn an_older_config_has_no_default_author() {
        let config: KameConfig = serde_json::from_str(r#"{"body_size":18.0}"#).unwrap();
        assert_eq!(config.author, "");
    }

    fn stop(scroll: f32) -> BackStop {
        BackStop {
            path: None,
            caret: Cursor {
                block_id: uuid::Uuid::nil(),
                offset: 0,
            },
            scroll,
        }
    }

    #[test]
    fn back_returns_through_links_in_reverse() {
        let mut back = BackStack::default();
        back.push(stop(1.0));
        back.push(stop(2.0));
        assert_eq!(back.pop(), Some(stop(2.0)));
        assert_eq!(back.pop(), Some(stop(1.0)));
        assert_eq!(back.pop(), None);
    }

    #[test]
    fn back_stops_follow_a_move() {
        let at = |path: &str| BackStop {
            path: Some(PathBuf::from(path)),
            caret: stop(0.0).caret,
            scroll: 7.0,
        };
        let mut back = BackStack::default();
        back.push(at("/n/a.kame"));
        back.push(at("/n/b.kame"));
        back.push(stop(1.0));
        back.push(at("/n/a.kame"));
        back.moved(Path::new("/n/a.kame"), Path::new("/n/x/c.kame"));
        assert_eq!(
            back.stops,
            [
                at("/n/x/c.kame"),
                at("/n/b.kame"),
                stop(1.0),
                at("/n/x/c.kame")
            ],
        );
    }

    #[test]
    fn a_stop_in_a_moved_document_is_found_by_any_way_of_naming_its_folder() {
        let root = kame_test_dir::folder("app-back-stop-place");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let roundabout = root.join("sub/../gone.kame");
        let mut back = BackStack::default();
        back.push(BackStop {
            path: Some(roundabout),
            caret: stop(0.0).caret,
            scroll: 0.0,
        });
        back.moved(&root.join("gone.kame"), &root.join("sub/here.kame"));
        assert_eq!(
            back.stops[0].path.as_deref(),
            Some(root.join("sub/here.kame").as_path())
        );
        back.forget(&root.join("sub/./here.kame"));
        assert!(back.stops.is_empty());
    }

    #[test]
    fn an_open_import_follows_its_file_when_it_moves() {
        let root = kame_test_dir::folder("app-import-moves")
            .canonicalize()
            .unwrap();
        let (from, to) = (root.join("a.md"), root.join("sub/b.md"));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(&from, "# A\n").unwrap();
        let (mut session, _) = Session::import(&from).unwrap();
        std::fs::rename(&from, &to).unwrap();

        let source = session.source_path.as_mut().unwrap();
        follow_move(source, &root.join("other.md"), &to);
        assert_eq!(session.link_base(), from, "that's a different document");

        let source = session.source_path.as_mut().unwrap();
        follow_move(source, &from, &to);
        assert_eq!(session.link_base(), to);
    }

    #[test]
    fn the_last_document_follows_a_move_only_if_it_was_that_one() {
        let (a, b) = (Path::new("/n/a.kame"), Path::new("/n/b.kame"));
        let mut last = LastDocument::new(Some(a.to_path_buf()));
        assert!(!last.moved(Path::new("/n/other.kame"), b));
        assert_eq!(last.path.as_deref(), Some(a));
        assert!(last.moved(a, b));
        assert_eq!(last.path.as_deref(), Some(b));
        assert!(!last.note(b), "it's already known");
    }

    #[test]
    fn the_way_back_stops_growing_eventually() {
        let mut back = BackStack::default();
        for number in 0..(MAX_BACK_STOPS + 5) {
            back.push(stop(number as f32));
        }
        assert_eq!(back.stops.len(), MAX_BACK_STOPS);
        assert_eq!(back.pop(), Some(stop((MAX_BACK_STOPS + 4) as f32)));
    }

    #[test]
    fn only_cmd_left_bracket_goes_back() {
        let key = |key, pressed, repeat, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers,
        };
        let command = egui::Modifiers::COMMAND;
        assert!(is_back_chord(&key(
            egui::Key::OpenBracket,
            true,
            false,
            command
        )));
        assert!(!is_back_chord(&key(
            egui::Key::OpenBracket,
            true,
            false,
            egui::Modifiers::NONE
        )));
        assert!(!is_back_chord(&key(
            egui::Key::OpenBracket,
            false,
            false,
            command
        )));
        assert!(!is_back_chord(&key(
            egui::Key::OpenBracket,
            true,
            true,
            command
        )));
        assert!(!is_back_chord(&key(
            egui::Key::CloseBracket,
            true,
            false,
            command
        )));
        let shifted = egui::Modifiers {
            shift: true,
            ..command
        };
        assert!(!is_back_chord(&key(
            egui::Key::OpenBracket,
            true,
            false,
            shifted
        )));
    }

    #[test]
    fn the_same_file_by_different_paths_is_the_same_document() {
        let dir = test_temp_dir().join(format!("same-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.kame"), "").unwrap();
        std::fs::write(dir.join("b.kame"), "").unwrap();
        let a = dir.join("a.kame");
        let roundabout = dir.join("sub/../a.kame");
        assert!(same_document(Some(&a), Some(&roundabout)));
        assert!(!same_document(Some(&a), Some(&dir.join("b.kame"))));
        assert!(same_document(None, None), "the same document with no file");
        assert!(!same_document(Some(&a), None));
        assert!(!same_document(
            Some(&dir.join("gone.kame")),
            Some(&dir.join("gone2.kame"))
        ));
    }

    #[test]
    fn a_press_soon_after_a_click_in_the_same_place_is_a_second_press() {
        let now = std::time::Instant::now();
        let soon = now + std::time::Duration::from_millis(200);
        let late = now + std::time::Duration::from_millis(800);
        let here = Pos2::new(100.0, 100.0);
        let close = Pos2::new(102.0, 101.0);
        let far = Pos2::new(160.0, 100.0);
        assert!(is_second_press(Some((now, here)), soon, close));
        assert!(!is_second_press(Some((now, here)), late, close));
        assert!(!is_second_press(Some((now, here)), soon, far));
        assert!(!is_second_press(None, soon, close));
    }

    #[test]
    fn a_merge_says_how_many_notes_it_left() {
        assert_eq!(merged_notice(0), "merged");
        assert_eq!(merged_notice(1), "merged, with 1 note to look at");
        assert_eq!(merged_notice(3), "merged, with 3 notes to look at");
    }

    // --- citations on the page -----------------------------------------------

    /// At a 400 point measure, this hyphenates its key if that's only text, and sets it whole
    /// if it's a link, which moves the lines around it.
    const BREAKS_AROUND_KEY: &str = "Characteristically unexceptionable compositions accommodate \
extraordinarily disproportionate considerations, as in [internationalization] and notwithstanding \
misunderstandings about organization while straightforward arrangements occasionally demonstrate \
remarkably counterintuitive properties, whatever anybody happens to believe about them afterwards.";

    /// A line after the key, which starts somewhere else once the key is a link.
    const CHECKED_LINE: usize = 3;

    fn page_with_breaks_around_key() -> Headless {
        let path = temp_db_path();
        std::fs::write(path.with_file_name("notebook.yaml"), "").unwrap();
        let mut session = open(&path);
        session.insert(BREAKS_AROUND_KEY);
        let mut page = Headless::new(session);
        page.app.renderer.set_config(LayoutConfig {
            body_width: 400.0,
            hyphenate: true,
            ..LayoutConfig::default()
        });
        // Finds the notebook, after which only a change to its files touches the list.
        page.frame(Vec::new());
        page
    }

    fn known_key() -> kame_core::references::References {
        let mut references = kame_core::references::References::new();
        references.insert(
            "internationalization".to_string(),
            kame_core::references::Reference {
                title: "T".to_string(),
                ..Default::default()
            },
        );
        references
    }

    /// Where each line starts, as the caret keys see them.
    fn caret_lines(page: &mut Headless) -> Vec<usize> {
        let block = page.app.session.document().blocks[0].clone();
        let layout = page.app.layout_of(&block);
        layout.lines.iter().map(|line| line.start).collect()
    }

    /// Where a click at the left of the line at `line` puts the caret, which is where the page
    /// has that line starting.
    fn painted_line_start(page: &mut Headless, line: usize) -> usize {
        let block = page.app.session.document().blocks[0].clone();
        let layout = page.app.layout_of(&block);
        let run = &layout.runs[line];
        let y = PAGE_PADDING + run.line_top + run.line_height / 2.0;
        page.click(Pos2::new(1.0, y));
        page.app.session.caret.offset
    }

    #[test]
    fn a_citation_shapes_the_same_for_the_caret_and_the_page() {
        let mut page = page_with_breaks_around_key();
        let as_text = caret_lines(&mut page);

        page.app.refs.list = known_key();
        let as_link = caret_lines(&mut page);
        assert_ne!(
            as_text[CHECKED_LINE], as_link[CHECKED_LINE],
            "the key has to move the line checked"
        );

        page.frame(Vec::new());
        assert_eq!(
            painted_line_start(&mut page, CHECKED_LINE),
            as_link[CHECKED_LINE]
        );
    }

    #[test]
    fn the_page_shapes_again_when_a_key_comes_and_goes() {
        let mut page = page_with_breaks_around_key();
        let as_text = caret_lines(&mut page);
        page.frame(Vec::new());
        assert_eq!(
            painted_line_start(&mut page, CHECKED_LINE),
            as_text[CHECKED_LINE]
        );

        page.app.refs.list = known_key();
        let as_link = caret_lines(&mut page);
        assert_ne!(
            as_text[CHECKED_LINE], as_link[CHECKED_LINE],
            "the key has to move the line checked"
        );
        page.frame(Vec::new());
        assert_eq!(
            painted_line_start(&mut page, CHECKED_LINE),
            as_link[CHECKED_LINE]
        );

        page.app.refs.list = kame_core::references::References::new();
        page.frame(Vec::new());
        assert_eq!(
            painted_line_start(&mut page, CHECKED_LINE),
            as_text[CHECKED_LINE]
        );
    }

    #[test]
    fn closing_a_marginal_nothing_was_written_in_leaves_none() {
        let mut session = Session::anonymous().unwrap();
        session.insert("word");
        let before = session.document().clone();
        let mut page = Headless::new(session);
        let ctx = page.ctx.clone();
        page.app
            .run_menu_action(&ctx, MenuAction::Annotate(AnnotationKind::Marginal));
        page.frame(Vec::new());
        assert_eq!(page.app.session.document().annotations.len(), 1);
        page.press("Close");
        assert!(page.app.session.document().annotations.is_empty());
        assert_eq!(page.app.session.document(), &before);
        assert_eq!(page.app.session.history(), (1, 1));
    }

    /// A page with a word on it and a marginal just added to it, as Annotate > Marginal leaves it.
    fn page_with_unwritten_marginal() -> (Headless, Document) {
        let mut session = Session::anonymous().unwrap();
        session.insert("word");
        let before = session.document().clone();
        let mut page = Headless::new(session);
        let ctx = page.ctx.clone();
        page.app
            .run_menu_action(&ctx, MenuAction::Annotate(AnnotationKind::Marginal));
        page.frame(Vec::new());
        assert_eq!(page.app.session.document().annotations.len(), 1);
        (page, before)
    }

    #[test]
    fn closing_an_unwritten_marginal_with_ctrl_enter_leaves_none() {
        let (mut page, before) = page_with_unwritten_marginal();
        let modifiers = egui::Modifiers {
            command: true,
            ..egui::Modifiers::NONE
        };
        page.frame(vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            },
        ]);
        assert_eq!(page.app.open_note, None);
        assert_eq!(page.app.session.document(), &before);
    }

    #[test]
    fn closing_an_unwritten_marginal_with_escape_leaves_none() {
        let (mut page, before) = page_with_unwritten_marginal();
        page.frame(
            [true, false]
                .map(|pressed| egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                })
                .to_vec(),
        );
        assert_eq!(page.app.open_note, None);
        assert_eq!(page.app.session.document(), &before);
    }

    #[test]
    fn closing_a_marginal_that_was_written_in_keeps_it() {
        let mut session = Session::anonymous().unwrap();
        session.insert("word");
        let mut page = Headless::new(session);
        let ctx = page.ctx.clone();
        page.app
            .run_menu_action(&ctx, MenuAction::Annotate(AnnotationKind::Marginal));
        let id = page.app.open_note.unwrap();
        page.app.session.set_annotation_body(id, "Said something.");
        page.frame(Vec::new());
        page.press("Close");
        assert_eq!(page.app.session.document().annotations.len(), 1);
    }

    // --- the help dialog -------------------------------------------------------

    /// The window the help is in, as it was drawn.
    fn help_window(page: &Headless) -> Rect {
        page.ctx
            .memory(|memory| memory.area_rect(help_id()))
            .expect("the help is up")
    }

    /// Where the last frame drew the help's `words`, as (top, bottom), if it did.
    fn help_row(page: &Headless, words: &str) -> Option<(f32, f32)> {
        page.drawn
            .iter()
            .flat_map(|tree| &tree.nodes)
            .find_map(|(_, node)| node.bounds().filter(|_| node.value() == Some(words)))
            .map(|bounds| (bounds.y0 as f32, bounds.y1 as f32))
    }

    fn help_page() -> Headless {
        let mut page = Headless::new(Session::anonymous().unwrap());
        page.frame(vec![egui::Event::Key {
            key: egui::Key::F1,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        for _ in 0..3 {
            page.frame(Vec::new());
        }
        page
    }

    #[test]
    fn the_help_draws_every_row_inside_the_window() {
        let page = help_page();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(Headless::WIDTH, Headless::HEIGHT));
        let window = help_window(&page);
        assert!(
            screen.contains_rect(window),
            "{window:?} is not on {screen:?}"
        );
        assert!(
            window.width() > window.height(),
            "it is landscape: {window:?}"
        );
        for (keys, what) in HELP_ROWS {
            for words in [keys, what] {
                let (top, bottom) =
                    help_row(&page, words).unwrap_or_else(|| panic!("{words:?} is not drawn"));
                assert!(
                    top >= window.top() && bottom <= window.bottom(),
                    "{words:?} is outside the window"
                );
            }
        }
    }

    #[test]
    fn the_help_lists_what_it_used_to_leave_out() {
        let page = help_page();
        for keys in ["Ctrl-N", "Ctrl-A", "Ctrl-[", "F7", "|", "```"] {
            assert!(
                HELP_ROWS.iter().any(|(row, _)| row.contains(keys)),
                "no row for {keys}"
            );
            assert!(page.sees(keys), "{keys} is not drawn");
        }
    }

    #[test]
    fn the_help_scrolls_to_any_row_in_the_smallest_window() {
        let mut page = Headless::new(Session::anonymous().unwrap());
        page.height = 220.0;
        page.ctx.set_zoom_factor(1.0);
        page.frame(vec![egui::Event::Key {
            key: egui::Key::F1,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        for _ in 0..3 {
            page.frame(Vec::new());
        }
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(Headless::WIDTH, page.height));
        let window = help_window(&page);
        assert!(
            screen.contains_rect(window),
            "{window:?} is not on {screen:?}"
        );
        let last = HELP_ROWS.last().unwrap().1;
        assert!(
            help_row(&page, last).is_none_or(|(top, _)| top >= window.bottom()),
            "the last row starts below the window"
        );
        page.frame(vec![egui::Event::PointerMoved(window.center())]);
        for _ in 0..40 {
            page.frame(vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                delta: Vec2::new(0.0, -5.0),
                modifiers: egui::Modifiers::NONE,
            }]);
        }
        let (top, bottom) = help_row(&page, last).expect("it was scrolled to");
        assert!(
            top >= window.top() && bottom <= window.bottom(),
            "the last row is at {top}..{bottom} in {window:?}"
        );
    }

    #[test]
    fn logs_that_cant_be_read_are_said_once() {
        let root = kame_test_dir::folder("app-unreadable-logs")
            .canonicalize()
            .unwrap();
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        std::fs::write(root.join(kame_sync::logs::MARKER), "").unwrap();
        let path = root.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("Words.");
        // A log that is a folder can't be read.
        std::fs::create_dir_all(kame_sync::logs::log_folder(&path).join("other.log")).unwrap();
        let mut page = Headless::new(session);
        page.app.device = "titan-bbbb".to_string();

        page.app.sync_logs();
        let notice = page
            .app
            .notice
            .take()
            .expect("it says why it isn't syncing");
        assert!(notice.contains("revision logs"), "{notice}");
        assert!(notice.contains("isn't syncing"), "{notice}");

        page.app.sync_logs();
        assert_eq!(page.app.notice, None, "once is enough");
    }

    #[test]
    fn a_known_key_is_coloured_as_a_link_and_drawn_again() {
        let mut page = page_with_breaks_around_key();
        let block = page.app.session.document().blocks[0].clone();
        let coloured = |layout: &BlockLayout| {
            layout
                .runs
                .iter()
                .flat_map(|run| &run.glyphs)
                .any(|glyph| glyph.color_opt.is_some())
        };
        let texture = |page: &Headless| page.app.textures.get(block.id).unwrap().texture.id();
        assert!(!coloured(&page.app.layout_of(&block)));
        let as_text = texture(&page);

        page.app.refs.list = known_key();
        page.frame(Vec::new());

        assert!(coloured(&page.app.layout_of(&block)));
        assert_ne!(texture(&page), as_text);
        assert_eq!(page.app.session.document().blocks[0], block, "only drawn");
    }
}
