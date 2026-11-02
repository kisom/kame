//! kame — a distraction-free editor for long-form writing.
//!
//! The binary opens (or creates) one `.kame` SQLite file, loads its document,
//! lays every block out with cosmic-text, rasterizes each block to an egui
//! texture, and paints the result. Keyboard input is translated into editor
//! commands; every command is persisted through the durable op log so history
//! survives a restart.

mod edit;
mod export;
mod layout;
mod model;
mod render;
mod storage;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};

use eframe::egui;
use egui::{Align2, Color32, FontId, Id, Order, Pos2, Rect, Sense, Vec2};

use crate::edit::{Cursor, EditError, Editor, Op, Selection};
use crate::layout::reading_order;
use crate::layout::{
    place_footnotes, place_marginalia, BlockLayout, FootnotePlacement, LayoutConfig,
    MarginalPlacement, FOOTNOTE_GAP, FOOTNOTE_LINE_HEIGHT,
};
use crate::model::{
    Anchor, Annotation, AnnotationId, AnnotationKind, Block, BlockId, BlockKind, Document,
};
use crate::render::egui_render::{
    caret_line, caret_x, command_for_event, next_boundary, offset_at, prev_boundary, range_rects,
    word_at, AnnotationTextureCache, Command, GlyphRenderer, LayoutCache, TextureCache,
};
use crate::storage::{Db, DbError, SnapshotMeta};

/// Paper-coloured canvas behind the page.
const BACKGROUND: Color32 = Color32::from_rgb(247, 243, 235);
/// Warm near-black ink used for rasterized glyphs.
const BODY_INK: [u8; 3] = [32, 29, 25];
/// Lighter ink for the apparatus in the margin and at the foot.
const APPARATUS_INK: [u8; 3] = [86, 79, 69];
/// Caret colour: a muted vermilion that reads against both faces.
const CARET_INK: Color32 = Color32::from_rgb(176, 58, 46);
/// Selection band (translucent, so the prose still reads through it).
const SELECTION_FILL: Color32 = Color32::from_rgba_premultiplied(58, 78, 112, 62);
/// A note's anchored range: a faint amber wash.
const NOTE_HIGHLIGHT: Color32 = Color32::from_rgba_premultiplied(178, 138, 38, 38);
/// A footnote's anchored range and superscript marker.
const FOOTNOTE_HIGHLIGHT: Color32 = Color32::from_rgba_premultiplied(54, 96, 132, 34);
/// Ink for the footnote superscript number.
const FOOTNOTE_MARK: Color32 = Color32::from_rgb(54, 96, 132);
/// A marginal note's anchored range.
const MARGINAL_HIGHLIGHT: Color32 = Color32::from_rgba_premultiplied(96, 96, 96, 26);
/// Hairline rule above the footnotes.
const RULE_INK: Color32 = Color32::from_rgb(150, 141, 126);
/// Margin marker for a Note.
const NOTE_MARK: Color32 = Color32::from_rgb(140, 104, 40);
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
/// Veil painted over the blocks that are not being written in paragraph-focus
/// mode. It is a wash toward the page colour, not a darker tint, so dimmed
/// prose fades into the paper instead of turning grey.
const DIM_VEIL: Color32 = Color32::from_rgba_unmultiplied_const(247, 243, 235, 168);
/// Ink for a failed write, in the status line and in the always-on indicator.
const ERROR_INK: Color32 = Color32::from_rgb(168, 32, 32);
/// Ink for the confirmation of a file command (an export path, a snapshot id).
const NOTICE_INK: Color32 = Color32::from_rgb(46, 96, 60);
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
    /// Hide the top panel: toolbar, status line and help text.
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

/// The measure and gutter a window of `available_width` can actually show.
///
/// The page is `body + 2 * margin` wide and nothing scrolls horizontally, so a
/// window narrower than that would put the right end of every line out of
/// reach. The measure therefore follows the window: it shrinks first, and once
/// it would fall below [`MIN_MEASURE`] the margin column is dropped instead and
/// the prose takes the whole width.
fn page_measure(
    body_width: f32,
    margin_width: f32,
    hide_margin: bool,
    available_width: f32,
) -> (f32, f32) {
    let gutter = if hide_margin { 0.0 } else { margin_width };
    let available = available_width.max(1.0);
    if available - 2.0 * gutter >= MIN_MEASURE {
        (
            body_width.min(available - 2.0 * gutter).max(MIN_MEASURE),
            gutter,
        )
    } else {
        (body_width.min(available).max(MIN_MEASURE), 0.0)
    }
}

/// Page geometry for one frame, all as offsets from the painter's top-left.
///
/// `config` is the measured config for this frame: `page_measure` has already
/// shrunk the measure (and collapsed the gutter) to what the window can show,
/// so this only centres the page.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PageGeometry {
    /// Width of the page: the measure plus both margins (or just the measure
    /// when the margin column is hidden or collapsed).
    page_width: f32,
    /// Distance from the painter's left edge to the body text.
    body_left: f32,
    /// Width of the margin gutter on each side of the measure.
    margin_width: f32,
}

/// Centres the page in `available_width`. A zero gutter means the margin
/// apparatus has nowhere to sit, so callers skip it.
fn page_geometry(config: LayoutConfig, available_width: f32) -> PageGeometry {
    let margin_width = config.margin_width;
    let page_width = config.body_width + 2.0 * margin_width;
    let page_left = ((available_width - page_width) * 0.5).max(0.0);
    PageGeometry {
        page_width,
        body_left: page_left + margin_width,
        margin_width,
    }
}

/// The veil for block `index` in paragraph-focus mode, or `None` when the block
/// is the one being written in (or the mode is off, or no caret block resolved).
fn dim_veil(enabled: bool, index: usize, caret_index: Option<usize>) -> Option<Color32> {
    let caret_index = caret_index?;
    (enabled && index != caret_index).then_some(DIM_VEIL)
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
        for i in 0..=chars.len().saturating_sub(needle.len()) {
            let window = &chars[i..i + needle.len()];
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
fn draw_error_indicator(ui: &mut egui::Ui, error: &str, retry: bool) -> bool {
    let rect = ui.max_rect();
    {
        let painter = ui.painter();
        let strip = Rect::from_min_max(
            Pos2::new(rect.left(), rect.top()),
            Pos2::new(rect.right(), rect.top() + ERROR_STRIP_HEIGHT),
        );
        painter.rect_filled(strip, 0.0, ERROR_INK);
        painter.circle_filled(Pos2::new(rect.right() - 12.0, rect.top() + 12.0), 4.0, ERROR_INK);
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

fn main() -> eframe::Result {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: kame <file.kame>");
        std::process::exit(2);
    };

    // Open the document before the window exists, so a failure is reported as
    // "your file is the problem" rather than an opaque app-creation error that
    // reads as "kame is broken".
    let session = match Session::open(&path) {
        Ok(session) => session,
        Err(err) => {
            eprintln!("kame: {err}");
            std::process::exit(1);
        }
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 860.0])
            .with_min_inner_size([560.0, 360.0])
            .with_title("kame"),
        ..Default::default()
    };

    eframe::run_native(
        "kame",
        native_options,
        Box::new(move |cc| Ok(Box::new(KameApp::new(cc, session)))),
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
        DbError::Sqlite(_) => "the file is not a valid kame document".to_string(),
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
    /// The file this session was opened from. Exports land beside it, and the
    /// snapshots it prompts for live in the database it names.
    path: PathBuf,
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
            Some(err) => (
                Some(format!("save failed — editing disabled: {err}")),
                true,
            ),
            None => (warning, false),
        };
        Ok(Session {
            editor,
            db,
            caret,
            selection: None,
            last_error,
            save_failed,
            path: path.to_path_buf(),
        })
    }

    fn document(&self) -> &Document {
        self.editor.document()
    }

    /// The `.kame` file this session was opened from.
    fn path(&self) -> &Path {
        &self.path
    }

    fn history(&self) -> (usize, usize) {
        self.editor.history()
    }

    // --- files, exports and snapshots --------------------------------------

    /// Writes the document as Markdown beside the `.kame` file and returns the
    /// path written.
    ///
    /// A failed export is reported through `last_error` but never sets
    /// `save_failed`: the document itself is still stored, so the author can
    /// keep writing and take the export again.
    fn export_markdown(&mut self) -> Option<PathBuf> {
        let path = export_path(&self.path, "md");
        let markdown = crate::export::to_markdown(self.editor.document());
        self.write_export(&path, &markdown)
    }

    /// Writes the document as a standalone HTML page beside the `.kame` file
    /// and returns the path written.
    fn export_html(&mut self) -> Option<PathBuf> {
        let path = export_path(&self.path, "html");
        let html = crate::export::to_html(self.editor.document());
        self.write_export(&path, &html)
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
    let x = (pos.x - body_left).max(0.0);
    let y = (pos.y - tops[index]).clamp(0.0, heights[index]);
    let offset = offset_at(layouts[index], x, y, block.text.len());
    Some(Cursor {
        block_id: block.id,
        offset,
    })
}

/// Minimum height of a block, so an empty paragraph still has a line to click
/// and a caret to show.
fn block_height(block: &Block, layout: &BlockLayout, config: &LayoutConfig) -> f32 {
    let minimum = match block.kind {
        crate::model::BlockKind::Heading { .. } => config.heading_size * 1.4,
        _ => config.body_line_height,
    };
    layout.height.max(minimum)
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
) -> Option<Pos2> {
    let (anchor_start, anchor_end) = anchor.byte_range()?;
    let end = anchor_end.max(anchor_start);
    let end_line = layout
        .lines
        .iter()
        .find(|line| end >= line.start && end <= line.end);
    let (line, x) = match end_line {
        Some(line) => (*line, caret_x(layout, end)),
        None => (caret_line(layout, anchor_start)?, caret_x(layout, anchor_start)),
    };
    Some(Pos2::new(
        body_left + x.clamp(0.0, body_width),
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
    /// The caret position typewriter mode last centred. A new position scrolls
    /// the page so the caret returns to the middle; an unchanged one leaves the
    /// reader free to scroll away.
    typewriter_anchor: Option<(BlockId, usize)>,
    /// Scroll offset requested by last frame's typewriter centring. Applied to
    /// the scroll area on the next frame, when its geometry is known.
    pending_scroll: Option<f32>,
    /// The "Snapshot Now" label prompt, when it is open.
    snapshot_prompt: Option<SnapshotPrompt>,
    /// The "Restore Snapshot" picker, when it is open. At most one of this and
    /// `snapshot_prompt` is up at a time.
    restore_picker: Option<RestorePicker>,
    /// What the last file command did, shown in the status panel until the
    /// next one. Failures are reported through `Session::last_error` instead.
    notice: Option<String>,
    /// Keeps a failed write from being discarded by a stray close, while still
    /// letting the author quit on a second request.
    close: CloseGuard,
    /// The find bar, when it is open.
    find: Option<Find>,
    /// Set when a find jump moves the caret to a match that may be off-screen;
    /// the next frame's draw scrolls the caret into view.
    reveal_caret: bool,
}

impl KameApp {
    fn new(cc: &eframe::CreationContext<'_>, session: Session) -> KameApp {
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        KameApp {
            session,
            renderer: GlyphRenderer::new(LayoutConfig::default()),
            layouts: LayoutCache::new(),
            textures: TextureCache::new(),
            annotation_textures: AnnotationTextureCache::new(),
            open_note: None,
            open_note_pos: None,
            open_note_body: String::new(),
            open_note_focus: false,
            modes: Modes::default(),
            typewriter_anchor: None,
            pending_scroll: None,
            snapshot_prompt: None,
            restore_picker: None,
            notice: None,
            close: CloseGuard::default(),
            find: None,
            reveal_caret: false,
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
    }

    /// Whether a file dialog is up. While one is, it owns the keyboard.
    fn dialog_open(&self) -> bool {
        self.snapshot_prompt.is_some() || self.restore_picker.is_some()
    }

    /// Closes whichever file dialog is open. Returns whether one was closed, so
    /// Escape can fall through to the note popover only when it was not.
    fn dismiss_dialog(&mut self) -> bool {
        let open = self.dialog_open();
        self.snapshot_prompt = None;
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
                next = ui
                    .small_button("↓")
                    .on_hover_text("next (Enter)")
                    .clicked();
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
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            // Escape closes the find bar first, then a dialog, then the note.
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
        // While the popover's body field has focus the keystrokes are the
        // note's, not the document's: without this the same character would be
        // inserted into both the note and the prose behind it.
        if ctx.memory(|memory| memory.has_focus(note_body_edit_id())) {
            return;
        }
        // While the find bar's query field has focus, Enter steps through the
        // matches and the document gets no keystrokes.
        if self.find.is_some() && ctx.memory(|memory| memory.has_focus(find_query_id())) {
            let next = ctx.input(|input| input.key_pressed(egui::Key::Enter) && !input.modifiers.shift);
            let prev = ctx.input(|input| input.key_pressed(egui::Key::Enter) && input.modifiers.shift);
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
                    self.session.insert(&text);
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
        let layout = self.renderer.layout(&block);
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
        let adj_layout = self.renderer.layout(&adjacent);
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
                document
                    .blocks
                    .first()
                    .map(|b| Cursor { block_id: b.id, offset: 0 }),
                document
                    .blocks
                    .last()
                    .map(|b| Cursor { block_id: b.id, offset: b.text.len() }),
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
                Cursor { block_id, offset: block_len },
                Cursor { block_id, offset: 0 },
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
        let layout = self.renderer.layout(&block);
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
        let title = self.session.document().title.clone();
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
        let path = self.session.path().display().to_string();
        let error = self.session.last_error.clone();
        let notice = self.notice.clone();
        // Where the two exports would land, spelled out on the buttons so the
        // author knows the file name before clicking.
        let markdown_path = export_path(self.session.path(), "md").display().to_string();
        let html_path = export_path(self.session.path(), "html")
            .display()
            .to_string();

        egui::Panel::top("kame-status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(title);
                ui.separator();
                ui.label(format!(
                    "{blocks} block{}",
                    if blocks == 1 { "" } else { "s" }
                ));
                ui.label(format!(
                    "{annotations} annotation{}",
                    if annotations == 1 { "" } else { "s" }
                ));
                ui.label(format!(
                    "{words} word{}",
                    if words == 1 { "" } else { "s" }
                ));
                ui.label(format!("undo {undo} · redo {redo}"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.monospace(path);
                });
            });
            ui.horizontal(|ui| {
                ui.label("Annotate the selection (or the word under the caret):");
                for (label, kind) in [
                    ("+ Note", AnnotationKind::Note),
                    ("+ Marginal", AnnotationKind::Marginal),
                    ("+ Footnote", AnnotationKind::Footnote),
                ] {
                    if ui.small_button(label).clicked() {
                        if let Some(id) = self.session.add_annotation(kind) {
                            self.set_open_note(Some(id));
                            self.open_note_pos = None;
                        }
                    }
                }
                // The active modes are named here so a hidden-chrome state is
                // discoverable again the moment the panel comes back.
                ui.separator();
                ui.label(egui::RichText::new("modes:").small().weak());
                let mut any = false;
                for (label, mode) in [
                    ("fullscreen", ModeCommand::Fullscreen),
                    ("no chrome", ModeCommand::HideChrome),
                    ("no margin", ModeCommand::HideMargin),
                    ("focus", ModeCommand::ParagraphFocus),
                    ("typewriter", ModeCommand::Typewriter),
                ] {
                    if self.modes.is_on(mode) {
                        any = true;
                        ui.label(egui::RichText::new(label).small().strong());
                    }
                }
                if !any {
                    ui.label(egui::RichText::new("none").small().weak());
                }
            });
            ui.horizontal(|ui| {
                ui.label("Block:");
                // The caret block's kind, so the active style can be shown as
                // selected. Read before the loop: the buttons below mutate the
                // session.
                let current = self
                    .session
                    .document()
                    .blocks
                    .iter()
                    .find(|block| block.id == self.session.caret.block_id)
                    .map(|block| block.kind.clone());
                for (label, kind, hint) in [
                    ("body", BlockKind::Paragraph, "body text · Ctrl-Shift-0"),
                    (
                        "H1",
                        BlockKind::Heading { level: 1 },
                        "heading level 1 · Ctrl-Shift-1",
                    ),
                    (
                        "H2",
                        BlockKind::Heading { level: 2 },
                        "heading level 2 · Ctrl-Shift-2",
                    ),
                    (
                        "H3",
                        BlockKind::Heading { level: 3 },
                        "heading level 3 · Ctrl-Shift-3",
                    ),
                    (
                        "• list",
                        BlockKind::ListItem { ordered: false },
                        "bulleted list item",
                    ),
                    (
                        "1. list",
                        BlockKind::ListItem { ordered: true },
                        "numbered list item",
                    ),
                ] {
                    if ui
                        .selectable_label(current.as_ref() == Some(&kind), label)
                        .on_hover_text(hint)
                        .clicked()
                    {
                        self.set_block_kind(kind);
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("File:");
                if ui
                    .small_button("Export Markdown")
                    .on_hover_text(format!("write {markdown_path} · Ctrl-Shift-E"))
                    .clicked()
                {
                    self.run_file_command(FileCommand::ExportMarkdown);
                }
                if ui
                    .small_button("Export HTML")
                    .on_hover_text(format!("write {html_path} · Ctrl-Shift-B"))
                    .clicked()
                {
                    self.run_file_command(FileCommand::ExportHtml);
                }
                ui.separator();
                if ui
                    .small_button("Snapshot Now")
                    .on_hover_text("capture the text as it stands · Ctrl-Shift-S")
                    .clicked()
                {
                    self.run_file_command(FileCommand::SnapshotNow);
                }
                if ui
                    .small_button("Restore Snapshot")
                    .on_hover_text("replace the text with a stored snapshot · Ctrl-Shift-R")
                    .clicked()
                {
                    self.run_file_command(FileCommand::RestoreSnapshot);
                }
                if let Some(notice) = &notice {
                    ui.separator();
                    ui.label(egui::RichText::new(notice).small().color(NOTICE_INK));
                }
            });
            ui.label(
                egui::RichText::new(
                    "Type to write · Enter new paragraph · Backspace/Delete · \
                     Ctrl-Z undo · Ctrl-Shift-Z redo · Ctrl-Shift-1/2/3 heading · \
                     Ctrl-Shift-0 body · drag to select · \
                     click to place the caret · click a margin marker to read its note",
                )
                .small()
                .weak(),
            );
            ui.label(
                egui::RichText::new(
                    "Modes: F11 fullscreen · Ctrl-Shift-H hide chrome · \
                     Ctrl-Shift-M hide margin · Ctrl-Shift-P paragraph focus · \
                     Ctrl-Shift-T typewriter",
                )
                .small()
                .weak(),
            );
            ui.label(
                egui::RichText::new(
                    "File: Ctrl-Shift-E export Markdown · Ctrl-Shift-B export HTML · \
                     Ctrl-Shift-S snapshot now · Ctrl-Shift-R restore snapshot",
                )
                .small()
                .weak(),
            );
            if let Some(error) = error {
                ui.colored_label(ERROR_INK, error);
            }
        });
    }

    fn draw_surface(&mut self, ui: &mut egui::Ui) {
        let config = self.renderer.config();
        let focus = self.modes.paragraph_focus;
        let typewriter = self.modes.typewriter;

        // Shape only blocks whose content changed since the last frame; an
        // idle repaint shapes nothing, and an edit re-shapes one block.
        let layouts = self
            .layouts
            .sync(&mut self.renderer, &self.session.document().blocks);

        self.textures.sync(
            ui.ctx(),
            &mut self.renderer,
            &self.session.document().blocks,
            &layouts,
            BODY_INK,
        );

        let document = self.session.document();
        let blocks = &document.blocks;
        let caret = self.session.caret;

        // Footnote numbering is reading order, not creation order: the stored
        // annotation vector records when a note was made, which is unrelated
        // to where it sits in the prose.
        let annotations = reading_order(&document.annotations, blocks);

        let heights: Vec<f32> = blocks
            .iter()
            .zip(layouts.iter())
            .map(|(block, layout)| block_height(block, layout, &config))
            .collect();
        let body_bottom = PAGE_PADDING
            + heights.iter().sum::<f32>()
            + BLOCK_GAP * blocks.len().saturating_sub(1) as f32;

        // Footnote Y values are page-local (body_bottom is measured from the
        // painter's top), so the painter origin is added when drawing.
        let footnotes = place_footnotes(&annotations, body_bottom);
        let footnote_bottom = footnotes
            .last()
            .map(|placement| placement.y + FOOTNOTE_LINE_HEIGHT)
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
        for height in &heights {
            tops.push(y);
            y += height + BLOCK_GAP;
        }

        // --- annotation text -------------------------------------------------
        // Margin bodies sit in the gutter right of the measure; footnote
        // entries span the measure. Both are cached separately from block
        // textures because they are shaped at another width and size.
        let annotation_size = config.body_size * ANNOTATION_SCALE;
        let margin_column = (geometry.margin_width - config.margin_size).max(40.0);
        let footnote_number: HashMap<AnnotationId, usize> = footnotes
            .iter()
            .map(|placement| (placement.annotation_id, placement.number))
            .collect();
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
                    APPARATUS_INK,
                ),
                AnnotationKind::Footnote => {
                    let number = footnote_number.get(&annotation.id).copied().unwrap_or(1);
                    let entry = format!("{number}. {}", annotation.body);
                    self.annotation_textures.sync(
                        ui.ctx(),
                        &mut self.renderer,
                        annotation.id,
                        &entry,
                        config.body_width,
                        annotation_size,
                        APPARATUS_INK,
                    );
                }
                // A Note keeps its body behind the margin marker, so it has no
                // texture until the popover opens (which uses egui text); a
                // marginal in hide-margin mode is skipped above.
                AnnotationKind::Marginal | AnnotationKind::Note => {}
            }
        }
        self.annotation_textures.retain(&live);

        // --- prose -----------------------------------------------------------
        // Paragraph-focus mode veils every block but the caret's: the veil is
        // paper at partial alpha, so dimmed prose fades and the margins stay
        // paper-coloured.
        let caret_index = blocks.iter().position(|block| block.id == caret.block_id);
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        for (index, block) in blocks.iter().enumerate() {
            if let Some(texture) = self.textures.get(block.id) {
                let origin = Pos2::new(body_left, tops[index]);
                let rect = texture.rect(origin);
                painter.image(texture.texture.id(), rect, uv, Color32::WHITE);
                if let Some(veil) = dim_veil(focus, index, caret_index) {
                    painter.rect_filled(rect, 0.0, veil);
                }
            }
        }

        // --- anchored ranges -------------------------------------------------
        // A faint wash marks what every annotation is attached to. Drawn after
        // the glyphs so the tint sits over the paper rather than behind it.
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
                AnnotationKind::Marginal => MARGINAL_HIGHLIGHT,
                AnnotationKind::Note => NOTE_HIGHLIGHT,
                AnnotationKind::Footnote => FOOTNOTE_HIGHLIGHT,
            };
            for rect in range_rects(layouts[index], start, end) {
                painter.rect_filled(
                    rect.translate(Vec2::new(body_left, tops[index])),
                    1.0,
                    color,
                );
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
                            rect.translate(Vec2::new(body_left, tops[index])),
                            0.0,
                            SELECTION_FILL,
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
                Pos2::new(body_left + x, top),
                Vec2::new(1.5, line_height),
            );
            painter.rect_filled(rect, 0.0, CARET_INK);
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
                    // A thin rule opens the margin block, the way a printed
                    // marginal note is set off from the measure.
                    painter.rect_filled(
                        Rect::from_min_size(
                            Pos2::new(origin.x - 7.0, origin.y + 2.0),
                            Vec2::new(1.0, 12.0),
                        ),
                        0.0,
                        RULE_INK,
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
                    painter.circle_filled(center, 3.5, NOTE_MARK);
                    painter.circle_stroke(
                        center,
                        if open { 7.5 } else { 6.0 },
                        egui::Stroke::new(1.0, NOTE_MARK.gamma_multiply(0.65)),
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
                RULE_INK,
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
            ) else {
                continue;
            };
            painter.text(
                origin,
                Align2::LEFT_TOP,
                placement.number.to_string(),
                FontId::proportional(config.body_size * 0.62),
                FOOTNOTE_MARK,
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
                    caret_action = Some((cursor, shift));
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
                    if ui.small_button("Close").clicked() {
                        close = true;
                    }
                });
            });

        self.open_note_focus = false;
        if changed {
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
        } else if self.restore_picker.is_some() {
            self.show_restore_picker(ctx, enter);
        }
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
        // The measure follows the window *before* the scroll area is built: the
        // scroll bar's presence would otherwise feed back into the wrap width
        // and make the page oscillate at the threshold. The panel width does not
        // depend on it.
        let base = LayoutConfig::default();
        let available = (ui.available_width() - SCROLLBAR_ALLOWANCE).max(1.0);
        let (body_width, margin_width) = page_measure(
            base.body_width,
            base.margin_width,
            self.modes.hide_margin,
            available,
        );
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
        // Hide-chrome mode drops the whole top panel: it is both the toolbar
        // (annotation buttons) and the status bar, so one toggle clears both.
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
            .frame(egui::Frame::NONE.fill(BACKGROUND))
            .show(ui, |ui| {
                scroll.show(ui, |ui| self.draw_surface(ui));
                // The save-failure signal belongs to the page, not the chrome:
                // it is painted in this panel so it outlives hide-chrome.
                if let Some(error) = &error {
                    // A failed write's strip retries the save; any other message
                    // (a degraded open, a failed export) is dismissed, so it
                    // cannot sit there offering a write that does nothing.
                    let retry = self.session.save_failed;
                    if draw_error_indicator(ui, error, retry) {
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
    fn hiding_the_margin_collapses_the_gutter_and_keeps_the_measure_in_place() {
        let config = LayoutConfig::default();
        let shown = page_geometry(config, 1400.0);
        // Hiding the margin is a collapsed gutter in the measured config.
        let no_gutter = LayoutConfig {
            margin_width: 0.0,
            ..config
        };
        let hidden = page_geometry(no_gutter, 1400.0);

        assert_eq!(
            shown.page_width,
            config.body_width + 2.0 * config.margin_width
        );
        assert_eq!(shown.margin_width, config.margin_width);
        assert_eq!(hidden.page_width, config.body_width);
        assert_eq!(hidden.margin_width, 0.0);
        // Collapsing the gutter re-centres the page but leaves the prose at
        // the same column, so hiding the margin does not move the text.
        assert_eq!(shown.body_left, hidden.body_left);
    }

    #[test]
    fn the_measure_follows_the_window_instead_of_clipping() {
        // A wide window keeps the designed measure and its gutters.
        assert_eq!(page_measure(640.0, 180.0, false, 1400.0), (640.0, 180.0));
        // Exactly fitting is still the full measure.
        assert_eq!(page_measure(640.0, 180.0, false, 1000.0), (640.0, 180.0));
        // Narrower: the measure shrinks, the gutters stay.
        assert_eq!(page_measure(640.0, 180.0, false, 900.0), (540.0, 180.0));
        assert_eq!(page_measure(640.0, 180.0, false, 700.0), (340.0, 180.0));
        // Too narrow for a readable measure beside two gutters: the margin
        // column collapses and the prose takes the window.
        assert_eq!(page_measure(640.0, 180.0, false, 600.0), (600.0, 0.0));

        // Whatever the window, the page is never wider than it — that is the
        // clipping this replaces.
        for width in [560.0f32, 700.0, 900.0, 1000.0, 1400.0] {
            let (body, gutter) = page_measure(640.0, 180.0, false, width);
            assert!(
                body + 2.0 * gutter <= width + 0.01,
                "page {} exceeds window {width}",
                body + 2.0 * gutter
            );
            assert!(body >= MIN_MEASURE);
        }

        // Hiding the margin sets the prose full width.
        assert_eq!(page_measure(640.0, 180.0, true, 800.0), (640.0, 0.0));
        let narrow = page_measure(640.0, 180.0, false, 400.0);
        assert_eq!(narrow, (400.0, 0.0), "below the minimum the prose fills");
        assert_eq!(page_geometry(LayoutConfig { body_width: narrow.0, margin_width: narrow.1, ..LayoutConfig::default() }, 400.0).body_left, 0.0);
    }

    #[test]
    fn paragraph_focus_veils_every_block_but_the_carets() {
        assert_eq!(dim_veil(false, 0, Some(1)), None, "mode off");
        assert_eq!(dim_veil(true, 1, Some(1)), None, "the caret's block");
        assert_eq!(
            dim_veil(true, 0, Some(1)),
            Some(DIM_VEIL),
            "every other block"
        );
        // Without a resolved caret block nothing is dimmed, rather than the
        // whole page going faint.
        assert_eq!(dim_veil(true, 0, None), None);
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

        assert!(session.set_block_kind(
            block_id,
            BlockKind::Heading { level: 1 }
        ));
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
            reopened.document().blocks[0].text, "kept!",
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
        assert!(!session.save_failed, "the export failure is not a save failure");
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
        let origin = superscript_origin(&layout, &anchor, body_left, block_top, body_width)
            .expect("a footnote on a laid-out block has a superscript position");
        let start_x = caret_x(&layout, 0);
        let end_x = caret_x(&layout, 5);
        assert!(end_x > start_x, "the anchor covers more than its first glyph");
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
        let origin = superscript_origin(&layout, &anchor, body_left, block_top, body_width)
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
        assert_eq!(
            annotation.body, "",
            "a new note starts as a bare highlight"
        );
        drop(session);

        let reopened = open(&path);
        assert_eq!(reopened.document().annotations.len(), 1);
        assert_eq!(reopened.document().annotations[0].anchor.byte_range().unwrap().0, 4);
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
            vec![
                (document.blocks[0].id, 0, 2),
                (document.blocks[0].id, 1, 3),
            ]
        );
        assert!(find_matches(&document, "").is_empty());
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
        let expected = crate::export::to_markdown(session.document());
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
        let expected = crate::export::to_html(session.document());

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
