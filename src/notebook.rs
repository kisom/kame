//! A notebook on the screen: the jump list, search, the browser, and what links here. What these
//! know about a notebook comes from `kame-notebook`, on background threads, since it reads every
//! document. See `docs/superpowers/specs/2026-11-03-notebook-design.md`.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use kame_core::model::{BlockId, Metadata};
use kame_core::session::Session;
use kame_notebook::links::{create_document, linking_to, remove_untouched, Follow, LinkFrom, Made};
use kame_notebook::list::{listed, listed_if_changed, narrow, rows, Entry, Kind, Listed, RowKind};
use kame_notebook::log_folder;
use kame_notebook::manage::{carry_out, check_new_path, delete_document, plan_move, MoveReport};
use kame_notebook::search::{search, Hit};
use kame_notebook::stranded::{apply, decide, find_stranded, Done, Healthy, Steps, Unsettled};
use kame_notebook::Notebook;

use super::{follow_move, now_unix, same_document, same_place, BackStop, KameApp};

/// How wide the browser is.
const BROWSER_WIDTH: f32 = 200.0;
/// The least room between the browser and the text.
const BROWSER_GAP: f32 = 16.0;
const LIST_WIDTH: f32 = 620.0;
const LIST_HEIGHT: f32 = 380.0;
/// How many recently opened documents the jump list remembers.
const RECENT: usize = 20;
/// How often a list on the screen looks for what has arrived, in seconds.
const LIST_CHECK_SECONDS: u64 = 10;
/// How long a database must have been stranded before it's moved or retired, so that a sync still
/// in progress has time to finish.
const STRANDED_WAIT: Duration = Duration::from_secs(600);
/// How often to look again while something's waiting out `STRANDED_WAIT`, so that it's acted on
/// when its time is up.
const STRANDED_RECHECK: Duration = Duration::from_secs(60);

/// The jump list, open.
pub(crate) struct JumpList {
    query: String,
    selected: usize,
    focus: bool,
}

/// The notebook's search, open.
pub(crate) struct SearchList {
    query: String,
    hits: Vec<Hit>,
    selected: usize,
    focus: bool,
    running: Option<Running>,
}

struct Running {
    hits: Receiver<Hit>,
    cancel: Arc<AtomicBool>,
}

/// A document made by following a link, removed again if it's left untouched.
struct Fresh {
    path: PathBuf,
    made: Made,
    metadata: Metadata,
}

/// A document's row was right-clicked, and this is the dialog that opened.
pub(crate) enum FileOp {
    /// A rename or a move, which are one dialog and differ in what's selected when it opens.
    Path {
        from: PathBuf,
        /// The new path, from the notebook's root.
        field: String,
        refusal: Option<String>,
        rename: bool,
        /// Whether it has just opened, and is yet to take the keyboard.
        focus: bool,
    },
    Delete {
        path: PathBuf,
        title: String,
    },
}

/// Something a menu choice from the browser can find already open, since the browser is a side panel
/// and stays clickable under all of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenDialog {
    FileOp,
    Replaced,
    ClosePrompt,
    Metadata,
    Snapshot,
    OpenPath,
    Link,
    JumpList,
    Search,
    MoveReport,
    SiteReport,
    RestorePicker,
}

impl OpenDialog {
    /// Whether this may hold something typed or wants an answer, so that a menu choice has to wait
    /// for it. Anything else has nothing to lose, and is closed to make way.
    fn makes_menu_wait(self) -> bool {
        match self {
            OpenDialog::FileOp
            | OpenDialog::Replaced
            | OpenDialog::ClosePrompt
            | OpenDialog::Metadata
            | OpenDialog::Snapshot
            | OpenDialog::OpenPath
            | OpenDialog::Link => true,
            OpenDialog::JumpList
            | OpenDialog::Search
            | OpenDialog::MoveReport
            | OpenDialog::SiteReport
            | OpenDialog::RestorePicker => false,
        }
    }
}

/// What a clean-up pass read: when each database was first seen stranded, which were healthy, and
/// what to do.
type StrandedPass = (HashMap<PathBuf, Instant>, Healthy, Steps);

#[derive(Default)]
pub(crate) struct NotebookUi {
    pub(crate) notebook: Option<Notebook>,
    /// The file the notebook was last looked for from.
    looked_from: Option<PathBuf>,
    listing: Vec<Listed>,
    listing_coming: Option<Receiver<Vec<Listed>>>,
    listing_checked: Option<std::time::Instant>,
    /// When each database that's looked stranded was first seen so.
    stranded_seen: HashMap<PathBuf, Instant>,
    /// Which databases needn't have their logs read again. It's with the pass while one is reading.
    stranded_healthy: Healthy,
    /// What was last said about databases that couldn't be settled.
    stranded_unsettled: Option<Unsettled>,
    stranded_checked: Option<Instant>,
    /// Whether a pass is wanted whatever the time: a notebook just found, or a different listing.
    stranded_due: bool,
    /// A pass reading on a thread: the notebook it read, and what it saw and decided. The renames
    /// are made when it arrives, on the main thread.
    stranded_coming: Option<(Notebook, Receiver<StrandedPass>)>,
    pub(crate) jump: Option<JumpList>,
    pub(crate) search: Option<SearchList>,
    pub(crate) browser_open: bool,
    pub(crate) browser_folders: HashSet<String>,
    recent: Vec<PathBuf>,
    fresh: Option<Fresh>,
    pub(crate) backlinks: Vec<LinkFrom>,
    backlinks_for: Option<PathBuf>,
    backlinks_coming: Option<(Receiver<Vec<LinkFrom>>, Arc<AtomicBool>)>,
    /// A place in the document about to open, from a search.
    pending_place: Option<(BlockId, usize)>,
    /// Where the page's text starts while the browser takes room, and whether there's no room.
    pub(crate) page_left: Option<f32>,
    pub(crate) page_hidden: bool,
    /// A document clicked on in what links here.
    pub(crate) pending_open: Option<PathBuf>,
    pub(crate) file_op: Option<FileOp>,
    /// What a move couldn't change, waiting to be read.
    pub(crate) move_report: Option<MoveReport>,
}

impl NotebookUi {
    pub(crate) fn new(browser_open: bool, browser_folders: Vec<String>) -> NotebookUi {
        NotebookUi {
            browser_open,
            browser_folders: browser_folders.into_iter().collect(),
            ..NotebookUi::default()
        }
    }

    pub(crate) fn list_open(&self) -> bool {
        self.jump.is_some() || self.search.is_some()
    }

    pub(crate) fn close_lists(&mut self) -> bool {
        let open = self.list_open();
        self.jump = None;
        if let Some(search) = self.search.take() {
            if let Some(running) = search.running {
                running.cancel.store(true, Ordering::Relaxed);
            }
        }
        open
    }

    /// Looks for what has arrived, while something on the screen is showing the list. In a copy
    /// left open while writing elsewhere, that may be the only way a new document would show.
    fn check_listing(&mut self) {
        let showing = self.browser_open || self.jump.is_some();
        let due = self.listing_checked.is_none_or(|checked| {
            checked.elapsed() >= std::time::Duration::from_secs(LIST_CHECK_SECONDS)
        });
        if !showing || !due || self.listing_coming.is_some() {
            return;
        }
        let Some(notebook) = self.notebook.clone() else {
            return;
        };
        self.listing_checked = Some(std::time::Instant::now());
        let known = self.listing.clone();
        let (send, coming) = channel();
        self.listing_coming = Some(coming);
        std::thread::spawn(move || {
            if let Some(listing) = listed_if_changed(&notebook, &known) {
                let _ = send.send(listing);
            }
        });
    }

    fn refresh_listing(&mut self) {
        let Some(notebook) = self.notebook.clone() else {
            return;
        };
        let (send, coming) = channel();
        self.listing_coming = Some(coming);
        std::thread::spawn(move || {
            let _ = send.send(listed(&notebook));
        });
    }
}

/// How long after a look the next pass is due. If something hadn't waited out its time by that
/// look, it's `STRANDED_RECHECK`, so that the pass acting on it comes soon after its time is up.
/// Otherwise that look could have acted on all there is, and what it left has no reason to change
/// soon. A pass still comes every `STRANDED_WAIT`, as a log folder deleted by a sync doesn't change
/// the listing, and the clock on it should start about when it goes.
fn pass_interval(seen: &HashMap<PathBuf, Instant>, checked: Instant) -> Duration {
    let still_waiting = seen
        .values()
        .any(|first| checked.saturating_duration_since(*first) < STRANDED_WAIT);
    if still_waiting {
        STRANDED_RECHECK
    } else {
        STRANDED_WAIT
    }
}

/// Whether a clean-up pass should start: it was asked for, there's never been one, or it's been
/// long enough since the last.
fn pass_due(
    asked: bool,
    seen: &HashMap<PathBuf, Instant>,
    checked: Option<Instant>,
    now: Instant,
) -> bool {
    asked
        || checked.is_none_or(|checked| {
            now.saturating_duration_since(checked) >= pass_interval(seen, checked)
        })
}

/// What a pass reads: what's stranded, when each was first seen so, and what to do about those that
/// have waited long enough. It changes nothing on disk, so it can be on any thread.
fn read_pass(
    notebook: &Notebook,
    mut seen: HashMap<PathBuf, Instant>,
    mut healthy: Healthy,
    now: Instant,
) -> StrandedPass {
    let found = find_stranded(notebook, &mut healthy);
    let ready = stranded_ready(&mut seen, &found, now);
    let steps = decide(notebook, &ready);
    (seen, healthy, steps)
}

/// Notes when each of `found` was first seen stranded, forgetting any that no longer are, and
/// returns those seen so for at least `STRANDED_WAIT`.
pub(crate) fn stranded_ready(
    seen: &mut HashMap<PathBuf, Instant>,
    found: &[PathBuf],
    now: Instant,
) -> Vec<PathBuf> {
    seen.retain(|path, _| found.contains(path));
    for path in found {
        seen.entry(path.clone()).or_insert(now);
    }
    found
        .iter()
        .filter(|path| now.saturating_duration_since(seen[*path]) >= STRANDED_WAIT)
        .cloned()
        .collect()
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// How many things a notice describes before counting the rest.
const NOTICE_NAMES: usize = 3;

/// What a clean-up did, in a notice: a sentence for each of the first few things done, retirements
/// ahead of the rest since they're what a person may need to go and look at, and a count of any more.
pub(crate) fn stranded_notice(done: &[Done]) -> String {
    let mut ordered: Vec<&Done> = done.iter().collect();
    ordered.sort_by_key(|one| !matches!(one, Done::Retired { .. }));
    let mut parts: Vec<String> = ordered
        .iter()
        .take(NOTICE_NAMES)
        .map(|one| what_was_done(one))
        .collect();
    if ordered.len() > NOTICE_NAMES {
        parts.push(format!("and {} more.", ordered.len() - NOTICE_NAMES));
    }
    parts.join(" ")
}

/// What a pass says when it has been stopped from settling what has waited.
pub(crate) fn unsettled_notice(unsettled: &Unsettled) -> String {
    let what = match unsettled.waiting {
        1 => "1 document looks".to_string(),
        count => format!("{count} documents look"),
    };
    format!(
        "{what} left behind, and can't be settled because {}'s logs can't all be read",
        unsettled.because
    )
}

/// What a pass has to say about what it couldn't settle, if that isn't what was said last.
pub(crate) fn new_to_say(
    said: &mut Option<Unsettled>,
    now: Option<Unsettled>,
) -> Option<Unsettled> {
    if *said == now {
        return None;
    }
    said.clone_from(&now);
    now
}

fn what_was_done(done: &Done) -> String {
    match done {
        Done::Moved { from, to } => format!(
            "{} was moved to {}, to follow its document, which was moved on another machine.",
            file_name(from),
            to.display()
        ),
        Done::Returned { from, to } => format!(
            "{} was moved back to {}, since its document's logs have arrived there.",
            file_name(from),
            to.display()
        ),
        Done::Retired {
            from,
            to,
            opened_there,
            unsynced,
        } => {
            let mut text = format!(
                "{} was retired to {}, since its document was moved or deleted on another \
                 machine. ",
                file_name(from),
                to.display()
            );
            text.push_str(if *unsynced {
                "It holds words that never synced"
            } else {
                "It is only a copy of what's in the logs"
            });
            if *opened_there {
                text.push_str(", and its undo history is in the retired file");
            }
            text.push('.');
            text
        }
    }
}

/// What's selected when the rename and move dialog opens on `relative`, in characters as the field
/// counts them: for a rename, the file's name without its extension, and for a move, the folders.
pub(crate) fn path_selection(relative: &str, rename: bool) -> Range<usize> {
    let characters = |text: &str| text.chars().count();
    let (folders, name) = relative.rsplit_once('/').unwrap_or(("", relative));
    if !rename {
        return 0..characters(folders);
    }
    let start = characters(relative) - characters(name);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    start..start + characters(stem)
}

/// What the Move dialog adds for a document at the root. It has no folders to select, so what's typed
/// goes in front of its name, and without a `/` that's a rename.
fn move_hint(rename: bool, from: &Path, root: Option<&Path>) -> Option<&'static str> {
    (!rename && from.parent() == root).then_some("type a folder and / before the name")
}

/// What a delete says. Logs left behind keep the document's row in the browser, so without a word
/// about them it looks as if the delete didn't take, and a second one would remove them.
fn deleted_notice(title: &str, left_logs: bool) -> String {
    if left_logs {
        format!(
            "deleted {title}; the logs at that path may be another document's, \
             and were left"
        )
    } else {
        format!("deleted {title}")
    }
}

fn documents(count: usize) -> String {
    match count {
        1 => "1 document".to_string(),
        count => format!("{count} documents"),
    }
}

/// What a move says when there's nothing in it to read further.
fn moved_notice(relative: &str, changed: usize) -> String {
    match changed {
        0 => format!("moved to {relative}"),
        changed => format!(
            "moved to {relative}, and updated links in {}",
            documents(changed)
        ),
    }
}

/// Why the open document can't be moved. Reopening it after the move would lose whatever is only on
/// the screen. Newer comes first, since a document from a newer version also counts as a failed save.
fn why_not_move(save_failed: bool, newer: bool) -> Option<&'static str> {
    if newer {
        Some("this document has changes from a newer version of kame, so it can't be moved")
    } else if save_failed {
        Some("the last save failed, so retry it before moving this document")
    } else {
        None
    }
}

/// Whether there's a document at `path` to open again, building it if it has only logs. Opening a
/// path with nothing there would make an empty document, which could be taken for this one.
fn there_to_open(path: &Path) -> bool {
    kame_sync::logs::build_from_logs(path).is_ok() && path.exists()
}

fn path_field_id() -> egui::Id {
    egui::Id::unique("kame-file-op-path")
}

/// Gets the field ready to open with `selection` selected. It starts from nothing, so that undo in
/// it can't bring back the path of whatever document the dialog last opened on.
fn select_in_path_field(ctx: &egui::Context, selection: Range<usize>) {
    let mut state = egui::text_edit::TextEditState::default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(selection.start),
            egui::text::CCursor::new(selection.end),
        )));
    state.store(ctx, path_field_id());
}

/// The rename and move dialog's field, taking the keyboard with `selection` selected if it's given.
fn path_field(
    ui: &mut egui::Ui,
    field: &mut String,
    selection: Option<Range<usize>>,
) -> egui::Response {
    // Before the field is drawn, so that it's selected when first seen, as a field drawn without the
    // keyboard collapses its selection onto its end.
    if let Some(selection) = &selection {
        select_in_path_field(ui.ctx(), selection.clone());
        ui.memory_mut(|memory| memory.request_focus(path_field_id()));
    }
    let edit = ui.add(
        egui::TextEdit::singleline(field)
            .id(path_field_id())
            // Enter would take the keyboard away, and a refusal leaves the field there to be
            // corrected.
            .return_key(None)
            .desired_width(420.0),
    );
    // And again after, since drawing it can take the keyboard away, and with that the selection: a new
    // window's first frame only measures, with its widgets disabled, and the click that closed the
    // menu counts as a click outside the field. The next frame then finds both in place.
    if let Some(selection) = selection {
        select_in_path_field(ui.ctx(), selection);
        edit.request_focus();
        ui.ctx().request_repaint();
    }
    edit
}

/// Where the text starts, within the room right of the browser: where it would be anyway if
/// that's clear of the browser, and just past it if not. `None` if the text and its margin don't
/// fit beside the browser, so that the document hides for the moment. Nothing rewraps either way.
pub(crate) fn page_left_beside_browser(
    usual_left: f32,
    body_width: f32,
    margin_width: f32,
    window_width: f32,
) -> Option<f32> {
    let left = (usual_left - BROWSER_WIDTH).max(BROWSER_GAP);
    (BROWSER_WIDTH + left + body_width + margin_width <= window_width).then_some(left)
}

/// Opens a file that isn't a document in the system's viewer.
fn open_in_system_viewer(path: &Path) -> std::io::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(path)
        .spawn()
        .map(|_| ())
}

/// Whether a link's target is a path rather than a web address or the like.
pub(crate) fn is_relative_link(url: &str) -> bool {
    kame_notebook::links::is_relative(url)
}

impl KameApp {
    /// What runs every frame: keeps up with which notebook this is, and with the background threads.
    pub(crate) fn notebook_tick(&mut self, ctx: &egui::Context) {
        if let Some(path) = self.session.path().map(Path::to_path_buf) {
            if self.nb.looked_from.as_deref() != Some(&path) {
                self.nb.notebook = match Notebook::find(&path) {
                    Ok(found) => found,
                    Err(err) => {
                        self.session.last_error = Some(format!("this notebook: {err}"));
                        None
                    }
                };
                if self.nb.notebook.is_none() {
                    self.nb.close_lists();
                }
                self.nb.listing.clear();
                self.nb.refresh_listing();
                self.nb.recent.retain(|recent| *recent != path);
                self.nb.recent.insert(0, path.clone());
                self.nb.recent.truncate(RECENT);
                self.nb.looked_from = Some(path);
                self.nb.stranded_due = true;
                self.start_backlinks();
            }
        }
        self.nb.check_listing();
        if let Some(coming) = &self.nb.listing_coming {
            match coming.try_recv() {
                Ok(listing) => {
                    if !self.nb.listing.is_empty() && self.nb.listing != listing {
                        self.nb.stranded_due = true;
                    }
                    self.nb.listing = listing;
                    self.nb.listing_coming = None;
                }
                Err(TryRecvError::Disconnected) => self.nb.listing_coming = None,
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
            }
        }
        self.stranded_tick(ctx);
        if let Some((coming, _)) = &self.nb.backlinks_coming {
            match coming.try_recv() {
                Ok(links) => {
                    self.nb.backlinks = links;
                    self.nb.backlinks_coming = None;
                }
                Err(TryRecvError::Disconnected) => self.nb.backlinks_coming = None,
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
            }
        }
        if let Some(search) = &mut self.nb.search {
            if let Some(running) = &search.running {
                loop {
                    match running.hits.try_recv() {
                        Ok(hit) => search.hits.push(hit),
                        Err(TryRecvError::Empty) => {
                            ctx.request_repaint_after(std::time::Duration::from_millis(50));
                            break;
                        }
                        Err(TryRecvError::Disconnected) => {
                            search.running = None;
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Starts a clean-up pass of databases left behind by a move or delete on another machine when
    /// one's due, and acts on one that has finished. A pass reads on a thread and changes nothing.
    /// What it decided is done here, with what's open now, since opening a document and building one
    /// from its logs happen here, and a rename in the middle of either would be lost or move a file
    /// from under a session.
    fn stranded_tick(&mut self, ctx: &egui::Context) {
        if let Some((_, coming)) = &self.nb.stranded_coming {
            match coming.try_recv() {
                Ok((seen, healthy, steps)) => {
                    let Some((notebook, _)) = self.nb.stranded_coming.take() else {
                        return;
                    };
                    self.nb.stranded_seen = seen;
                    self.nb.stranded_healthy = healthy;
                    // The next frame works out when the next pass is.
                    ctx.request_repaint();
                    let unsettled = steps.unsettled().cloned();
                    let done = apply(&notebook, steps, self.session.path());
                    if !done.is_empty() {
                        self.nb.refresh_listing();
                        self.nb.stranded_due = true;
                        self.notice_tracker.announced();
                        self.notice = Some(stranded_notice(&done));
                    } else if let Some(new) = new_to_say(&mut self.nb.stranded_unsettled, unsettled)
                    {
                        self.notice_tracker.announced();
                        self.notice = Some(unsettled_notice(&new));
                    }
                }
                Err(TryRecvError::Disconnected) => self.nb.stranded_coming = None,
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
            }
            return;
        }
        let Some(notebook) = self.nb.notebook.clone() else {
            return;
        };
        let now = Instant::now();
        let seen = &self.nb.stranded_seen;
        if !pass_due(self.nb.stranded_due, seen, self.nb.stranded_checked, now) {
            if let Some(checked) = self.nb.stranded_checked {
                let since = now.saturating_duration_since(checked);
                ctx.request_repaint_after(pass_interval(seen, checked).saturating_sub(since));
            }
            return;
        }
        self.nb.stranded_due = false;
        self.nb.stranded_checked = Some(now);
        let seen = self.nb.stranded_seen.clone();
        let healthy = std::mem::take(&mut self.nb.stranded_healthy);
        let (send, coming) = channel();
        let reading = notebook.clone();
        self.nb.stranded_coming = Some((notebook, coming));
        std::thread::spawn(move || {
            let _ = send.send(read_pass(&reading, seen, healthy, Instant::now()));
        });
    }

    fn start_backlinks(&mut self) {
        if let Some((_, cancel)) = self.nb.backlinks_coming.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.nb.backlinks.clear();
        let (Some(notebook), Some(path)) = (
            self.nb.notebook.clone(),
            self.session.path().map(Path::to_path_buf),
        ) else {
            return;
        };
        self.nb.backlinks_for = Some(path.clone());
        let (send, coming) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.nb.backlinks_coming = Some((coming, cancel.clone()));
        std::thread::spawn(move || {
            let _ = send.send(linking_to(&notebook, &path, None, &cancel));
        });
    }

    /// Ctrl-L, Ctrl-R and Ctrl-Shift-O, which do something only in a notebook.
    pub(crate) fn handle_notebook_keys(&mut self, ctx: &egui::Context) {
        let pressed = |key: egui::Key, shift: bool| {
            ctx.input(|input| {
                input.events.iter().any(|event| {
                    matches!(
                        event,
                        egui::Event::Key {
                            key: pressed_key,
                            pressed: true,
                            repeat: false,
                            modifiers,
                            ..
                        } if *pressed_key == key && modifiers.command && modifiers.shift == shift
                    )
                })
            })
        };
        let (jump, home, find, browser) = (
            pressed(egui::Key::L, false),
            pressed(egui::Key::L, true),
            pressed(egui::Key::R, false),
            pressed(egui::Key::O, true),
        );
        if self.nb.notebook.is_none() || self.close_flow.prompting {
            return;
        }
        if jump {
            self.toggle_jump();
        }
        if home && !self.dialog_open() {
            self.go_home();
        }
        if find {
            self.toggle_search();
        }
        if browser {
            self.toggle_browser();
        }
    }

    /// File > New Notebook: makes a chosen folder a notebook, and opens its contents page.
    pub(crate) fn new_notebook(&mut self) {
        let Some(folder) = super::pick_notebook_folder(self.session.dialog_directory().as_deref())
        else {
            return;
        };
        match kame_notebook::init(&folder) {
            Ok(notebook) => {
                if let Some(index) = notebook.index() {
                    self.open_or_confirm(index);
                }
            }
            Err(err) => self.session.last_error = Some(err),
        }
    }

    /// Goes to the contents page, with Ctrl-[ coming back.
    pub(crate) fn go_home(&mut self) {
        match self.nb.notebook.as_ref().and_then(Notebook::index) {
            Some(index) => self.open_from_notebook(index, None),
            None => self.notice = Some(format!("this notebook has no {}", kame_notebook::INDEX)),
        }
    }

    pub(crate) fn toggle_jump(&mut self) {
        let open = self.nb.jump.is_some();
        self.nb.close_lists();
        if !open && !self.dialog_open() {
            self.nb.refresh_listing();
            self.nb.jump = Some(JumpList {
                query: String::new(),
                selected: 0,
                focus: true,
            });
        }
    }

    pub(crate) fn toggle_search(&mut self) {
        let open = self.nb.search.is_some();
        self.nb.close_lists();
        if !open && !self.dialog_open() {
            self.nb.search = Some(SearchList {
                query: String::new(),
                hits: Vec::new(),
                selected: 0,
                focus: true,
                running: None,
            });
        }
    }

    pub(crate) fn toggle_browser(&mut self) {
        self.nb.browser_open = !self.nb.browser_open;
        if self.nb.browser_open {
            self.nb.refresh_listing();
        }
        self.persist();
    }

    /// Opens a document from a list, at a place in it if there is one, with Ctrl-[ coming back.
    fn open_from_notebook(&mut self, path: PathBuf, place: Option<(BlockId, usize)>) {
        self.nb.close_lists();
        if self.nb.page_hidden {
            self.nb.browser_open = false;
            self.persist();
        }
        if same_document(self.session.path(), Some(&path)) {
            self.nb.pending_place = place;
            self.apply_pending_place();
            return;
        }
        if self.session.path().is_some() {
            self.back.push(BackStop {
                path: self.session.path().map(Path::to_path_buf),
                caret: self.session.caret,
                scroll: self.page_scroll,
            });
        }
        self.pending_heading = None;
        self.nb.pending_place = place;
        self.open_or_confirm(path);
    }

    /// Goes to the place a search asked for, once its document is open.
    pub(crate) fn apply_pending_place(&mut self) {
        if let Some((block_id, offset)) = self.nb.pending_place.take() {
            self.session
                .set_caret(kame_core::edit::Cursor { block_id, offset });
            self.session.restored_scroll = None;
            self.reveal_caret = true;
        }
    }

    /// Where a link leads in a notebook, doing what it takes to get there. `None` means it has been
    /// dealt with here, and otherwise it's a document for the usual path to open. A link this
    /// notebook won't follow falls back on opening anything that's already there.
    pub(crate) fn follow_in_notebook(
        &mut self,
        target: &str,
        heading: Option<&str>,
    ) -> Option<Option<PathBuf>> {
        let notebook = self.nb.notebook.clone()?;
        match notebook.follow(target, &self.session.link_base()) {
            Follow::Open(found) => {
                if let Err(err) = kame_sync::logs::build_from_logs(&found) {
                    self.session.last_error =
                        Some(format!("could not build {} — {err}", found.display()));
                    return Some(None);
                }
                Some(Some(found))
            }
            Follow::Create(found) => match create_document(&found, heading) {
                Ok(made) => {
                    self.nb.fresh = Some(Fresh {
                        path: found.clone(),
                        made,
                        metadata: Metadata::default(),
                    });
                    Some(Some(found))
                }
                Err(err) => {
                    self.session.last_error =
                        Some(format!("could not create {} — {err}", found.display()));
                    Some(None)
                }
            },
            Follow::View(found) => {
                if let Err(err) = open_in_system_viewer(&found) {
                    self.session.last_error =
                        Some(format!("could not open {} — {err}", found.display()));
                }
                Some(None)
            }
            Follow::Rotted => None,
        }
    }

    /// Whether what's on screen is a document made by following a link, and still untouched.
    pub(crate) fn on_untouched_fresh(&self) -> bool {
        self.nb.fresh.as_ref().is_some_and(|fresh| {
            same_document(self.session.path(), Some(&fresh.path))
                && self.session.history() == (0, 0)
                && self.session.metadata == fresh.metadata
        })
    }

    /// Notes what a fresh document's properties are once it's open, since opening one can add
    /// the default author, which isn't anybody's edit.
    pub(crate) fn note_fresh_opened(&mut self) {
        let metadata = self.session.metadata.clone();
        if let Some(fresh) = &mut self.nb.fresh {
            if same_document(self.session.path(), Some(&fresh.path)) {
                fresh.metadata = metadata;
            }
        }
    }

    /// Takes away a fresh document that was left untouched, now that nothing has it open.
    /// `untouched` was read before leaving it.
    pub(crate) fn clear_left_fresh(&mut self, untouched: bool) {
        let Some(fresh) = &self.nb.fresh else {
            return;
        };
        if same_document(self.session.path(), Some(&fresh.path)) {
            return;
        }
        let fresh = self.nb.fresh.take().expect("it was just read");
        if untouched {
            remove_untouched(&fresh.path, &fresh.made);
            self.back.forget(&fresh.path);
            self.nb.recent.retain(|recent| *recent != fresh.path);
            self.nb.refresh_listing();
            self.start_backlinks();
        }
    }

    /// The browser, as a panel to the left of the page.
    pub(crate) fn show_browser(&mut self, ui: &mut egui::Ui) {
        if !self.nb.browser_open || self.nb.notebook.is_none() || self.modes.hide_chrome {
            return;
        }
        let mut choose: Option<(PathBuf, Kind)> = None;
        let mut toggle: Option<String> = None;
        let mut file_op: Option<FileOp> = None;
        let current = self.session.path().map(Path::to_path_buf);
        let entries: Vec<_> = self
            .nb
            .listing
            .iter()
            .map(|item| item.entry.clone())
            .collect();
        let title = self
            .nb
            .notebook
            .as_ref()
            .map(Notebook::title)
            .unwrap_or_default();
        let show = |ui: &mut egui::Ui| {
            ui.add_space(6.0);
            ui.strong(&title);
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for row in rows(&entries, &self.nb.browser_folders) {
                        ui.horizontal(|ui| {
                            ui.add_space(row.depth as f32 * 14.0);
                            match &row.what {
                                RowKind::Folder(folder) => {
                                    let mark = if self.nb.browser_folders.contains(folder) {
                                        "▾"
                                    } else {
                                        "▸"
                                    };
                                    if ui
                                        .selectable_label(false, format!("{mark} {}", row.name))
                                        .clicked()
                                    {
                                        toggle = Some(folder.clone());
                                    }
                                }
                                RowKind::Entry(at) => {
                                    let entry = &entries[*at];
                                    let here = current.as_deref() == Some(entry.path.as_path());
                                    let label = ui.selectable_label(here, &row.name);
                                    if label.clicked() {
                                        choose = Some((entry.path.clone(), entry.kind));
                                    }
                                    if entry.kind == Kind::Document {
                                        let title = &self.nb.listing[*at].title;
                                        label.context_menu(|ui| {
                                            if let Some(op) = document_menu(ui, entry, title) {
                                                file_op = Some(op);
                                            }
                                        });
                                    }
                                }
                            }
                        });
                    }
                });
        };
        if self.nb.page_hidden {
            egui::CentralPanel::default().show(ui, show);
        } else {
            egui::Panel::left("kame-browser")
                .resizable(false)
                .exact_size(BROWSER_WIDTH)
                .show(ui, show);
        }
        if let Some(folder) = toggle {
            if !self.nb.browser_folders.remove(&folder) {
                self.nb.browser_folders.insert(folder);
            }
            self.persist();
        }
        if let Some(op) = file_op {
            self.start_file_op(op);
        }
        match choose {
            Some((path, Kind::Document)) => self.open_from_notebook(path, None),
            Some((path, Kind::File)) => {
                if let Err(err) = open_in_system_viewer(&path) {
                    self.session.last_error =
                        Some(format!("could not open {} — {err}", path.display()));
                }
            }
            None => {}
        }
    }

    /// The jump list and search, over the page.
    pub(crate) fn show_notebook_lists(&mut self, ctx: &egui::Context) {
        self.show_jump_list(ctx);
        self.show_search_list(ctx);
    }

    fn show_jump_list(&mut self, ctx: &egui::Context) {
        let Some(mut jump) = self.nb.jump.take() else {
            return;
        };
        let matching = narrow(&jump.query, &self.nb.listing, &self.nb.recent);
        let documents: Vec<usize> = matching
            .into_iter()
            .filter(|at| self.nb.listing[*at].entry.kind == Kind::Document)
            .collect();
        let creatable = match (&self.nb.notebook, documents.is_empty(), jump.query.trim()) {
            (Some(notebook), true, typed) if !typed.is_empty() => {
                match notebook.follow(typed, &notebook.root.join(kame_notebook::INDEX)) {
                    Follow::Create(path) => Some(path),
                    _ => None,
                }
            }
            _ => None,
        };
        let rows_count = documents.len() + usize::from(creatable.is_some());
        let (close, confirm) = list_keys(ctx, &mut jump.selected, rows_count);
        let mut clicked = None;
        let listing = &self.nb.listing;
        let root = self.nb.notebook.as_ref().map(|nb| nb.root.clone());
        list_window(ctx, "kame-jump", |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut jump.query)
                    .hint_text("Jump to a document")
                    .return_key(None)
                    .desired_width(f32::INFINITY),
            );
            focus_field(&edit, &mut jump.focus);
            if edit.changed() {
                jump.selected = 0;
            }
            ui.separator();
            list_rows(ui, rows_count, jump.selected, |ui, row, selected| {
                if let Some(at) = documents.get(row) {
                    let item = &listing[*at];
                    let response = list_row(ui, selected, &item.title, &item.entry.relative);
                    if response.clicked() {
                        clicked = Some(row);
                    }
                } else if let Some(path) = &creatable {
                    let shown = root
                        .as_ref()
                        .and_then(|root| path.strip_prefix(root).ok())
                        .unwrap_or(path)
                        .display()
                        .to_string();
                    let response = list_row(ui, selected, "Create", &shown);
                    if response.clicked() {
                        clicked = Some(row);
                    }
                }
            });
        });
        let chosen = clicked.or(confirm.then_some(jump.selected));
        if let Some(row) = chosen.filter(|row| *row < rows_count) {
            if let Some(at) = documents.get(row) {
                let path = self.nb.listing[*at].entry.path.clone();
                self.open_from_notebook(path, None);
            } else if let Some(path) = creatable {
                match create_document(&path, None) {
                    Ok(_) => self.open_from_notebook(path, None),
                    Err(err) => {
                        self.session.last_error =
                            Some(format!("could not create {} — {err}", path.display()));
                    }
                }
            }
            return;
        }
        if !close {
            self.nb.jump = Some(jump);
        }
    }

    fn start_search(&mut self, search_list: &mut SearchList) {
        if let Some(running) = search_list.running.take() {
            running.cancel.store(true, Ordering::Relaxed);
        }
        search_list.hits.clear();
        search_list.selected = 0;
        let Some(notebook) = self.nb.notebook.clone() else {
            return;
        };
        if search_list.query.is_empty() {
            return;
        }
        let (send, hits) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        search_list.running = Some(Running {
            hits,
            cancel: cancel.clone(),
        });
        let query = search_list.query.clone();
        let path = self.session.path().map(Path::to_path_buf);
        let doc = self.session.document().clone();
        std::thread::spawn(move || {
            let open = path.as_deref().map(|path| (path, &doc));
            search(&notebook, &query, open, &cancel, |hit| {
                let _ = send.send(hit);
            });
        });
    }

    fn show_search_list(&mut self, ctx: &egui::Context) {
        let Some(mut search_list) = self.nb.search.take() else {
            return;
        };
        let count = search_list.hits.len();
        let (close, confirm) = list_keys(ctx, &mut search_list.selected, count);
        let mut clicked = None;
        let mut changed = false;
        let running = search_list.running.is_some();
        list_window(ctx, "kame-search", |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut search_list.query)
                    .hint_text("Search this notebook")
                    .return_key(None)
                    .desired_width(f32::INFINITY),
            );
            focus_field(&edit, &mut search_list.focus);
            changed = edit.changed();
            let said = match (count, running, search_list.query.is_empty()) {
                (_, _, true) => String::new(),
                (0, true, _) => "searching…".to_string(),
                (0, false, _) => "no matches".to_string(),
                (1, false, _) => "1 match".to_string(),
                (count, false, _) => format!("{count} matches"),
                (count, true, _) => format!("{count} matches so far"),
            };
            ui.weak(said);
            ui.separator();
            let hits = &search_list.hits;
            list_rows(ui, count, search_list.selected, |ui, row, selected| {
                let hit = &hits[row];
                let response = list_row(ui, selected, &hit.line, &hit.title);
                if response.clicked() {
                    clicked = Some(row);
                }
            });
        });
        if changed {
            self.start_search(&mut search_list);
        }
        let chosen = clicked.or(confirm.then_some(search_list.selected));
        if let Some(hit) = chosen.and_then(|row| search_list.hits.get(row)) {
            let (path, block, offset) = (hit.path.clone(), hit.block, hit.offset);
            if let Some(running) = search_list.running.take() {
                running.cancel.store(true, Ordering::Relaxed);
            }
            self.open_from_notebook(path, Some((block, offset)));
            return;
        }
        if close {
            if let Some(running) = search_list.running.take() {
                running.cancel.store(true, Ordering::Relaxed);
            }
        } else {
            self.nb.search = Some(search_list);
        }
    }
}

/// A document row's right-click menu, and the dialog chosen from it.
fn document_menu(ui: &mut egui::Ui, entry: &Entry, title: &str) -> Option<FileOp> {
    ui.set_min_width(120.0);
    let path_dialog = |rename| FileOp::Path {
        from: entry.path.clone(),
        field: entry.relative.clone(),
        refusal: None,
        rename,
        focus: true,
    };
    let chosen = if ui.button("Rename…").clicked() {
        Some(path_dialog(true))
    } else if ui.button("Move…").clicked() {
        Some(path_dialog(false))
    } else if ui.button("Delete…").clicked() {
        Some(FileOp::Delete {
            path: entry.path.clone(),
            title: title.to_string(),
        })
    } else {
        None
    };
    if chosen.is_some() {
        ui.close();
    }
    chosen
}

/// Escape and Enter for a list, as (close, confirm), and the arrows moving through its rows.
fn list_keys(ctx: &egui::Context, selected: &mut usize, rows: usize) -> (bool, bool) {
    ctx.input_mut(|input| {
        let up = input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
        let down = input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
        if up {
            *selected = selected.saturating_sub(1);
        }
        if down && *selected + 1 < rows {
            *selected += 1;
        }
        (
            input.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
        )
    })
}

fn focus_field(field: &egui::Response, focus: &mut bool) {
    if *focus {
        field.request_focus();
        *focus = false;
    }
}

fn list_window(ctx: &egui::Context, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Window::new(id)
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_TOP, [0.0, 70.0])
        .fixed_size([LIST_WIDTH, LIST_HEIGHT])
        .show(ctx, |ui| {
            ui.set_max_width(LIST_WIDTH);
            add(ui);
        });
}

/// A list's rows, drawing only those in view, so that a list can be as long as it needs to be.
fn list_rows(
    ui: &mut egui::Ui,
    rows: usize,
    selected: usize,
    mut row: impl FnMut(&mut egui::Ui, usize, bool),
) {
    let height = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y + 4.0;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, height, rows, |ui, range| {
            for at in range {
                row(ui, at, at == selected);
            }
        });
}

fn list_row(ui: &mut egui::Ui, selected: bool, main: &str, side: &str) -> egui::Response {
    let response = ui
        .horizontal(|ui| {
            let main = ui.selectable_label(selected, main);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(side);
            });
            main
        })
        .inner;
    if selected {
        response.scroll_to_me(None);
    }
    response
}

/// How much room what links here takes below the footnotes, or none if nothing does.
pub(crate) fn backlinks_height(links: usize, size: f32) -> f32 {
    if links == 0 {
        0.0
    } else {
        size * 2.0 + (links + 1) as f32 * size * 1.5
    }
}

/// Draws what links here, returning the document clicked on. It's only drawn, so it never
/// reaches a document, an export or a site.
pub(crate) fn draw_backlinks(
    painter: &egui::Painter,
    response: &egui::Response,
    links: &[LinkFrom],
    at: egui::Pos2,
    size: f32,
    ink: egui::Color32,
) -> Option<PathBuf> {
    if links.is_empty() {
        return None;
    }
    let font = egui::FontId::proportional(size);
    let line = size * 1.5;
    let mut y = at.y + size * 2.0;
    painter.text(
        egui::Pos2::new(at.x, y),
        egui::Align2::LEFT_TOP,
        "Linked from",
        font.clone(),
        ink.gamma_multiply(0.6),
    );
    let mut clicked = None;
    for link in links {
        y += line;
        let rect = painter.text(
            egui::Pos2::new(at.x, y),
            egui::Align2::LEFT_TOP,
            &link.title,
            font.clone(),
            ink,
        );
        painter.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            egui::Stroke::new(1.0, ink.gamma_multiply(0.4)),
        );
        if response.clicked()
            && response
                .interact_pointer_pos()
                .is_some_and(|pos| rect.contains(pos))
        {
            clicked = Some(link.path.clone());
        }
    }
    clicked
}

impl KameApp {
    /// Opens the document clicked on in what links here, once the page has been drawn.
    pub(crate) fn open_pending_backlink(&mut self) {
        if let Some(path) = self.nb.pending_open.take() {
            self.open_from_notebook(path, None);
        }
    }

    /// Starts in a notebook named by its folder, with no contents page to open.
    pub(crate) fn start_in_notebook(&mut self, notebook: Notebook) {
        self.nb.notebook = Some(notebook);
        self.nb.browser_open = true;
        self.nb.refresh_listing();
    }

    /// What's open that a menu choice from the browser could run into.
    fn open_dialogs(&self) -> Vec<OpenDialog> {
        [
            (self.nb.file_op.is_some(), OpenDialog::FileOp),
            (self.replaced_prompt, OpenDialog::Replaced),
            (self.close_flow.prompting, OpenDialog::ClosePrompt),
            (self.metadata_prompt.is_some(), OpenDialog::Metadata),
            (self.snapshot_prompt.is_some(), OpenDialog::Snapshot),
            (self.open_prompt.is_some(), OpenDialog::OpenPath),
            (self.link_prompt.is_some(), OpenDialog::Link),
            (self.nb.jump.is_some(), OpenDialog::JumpList),
            (self.nb.search.is_some(), OpenDialog::Search),
            (self.nb.move_report.is_some(), OpenDialog::MoveReport),
            (self.publishing.report.is_some(), OpenDialog::SiteReport),
            (self.restore_picker.is_some(), OpenDialog::RestorePicker),
        ]
        .into_iter()
        .filter_map(|(open, dialog)| open.then_some(dialog))
        .collect()
    }

    /// Puts up a dialog chosen from a document's right-click menu. One that's open may hold something
    /// typed, or want an answer, and then this says to deal with it first. Something that can't be
    /// moved says so now, rather than after a path has been typed for it.
    fn start_file_op(&mut self, op: FileOp) {
        if self
            .open_dialogs()
            .into_iter()
            .any(OpenDialog::makes_menu_wait)
        {
            self.notice_tracker.announced();
            self.notice = Some("finish or close the open dialog first".to_string());
            return;
        }
        if let FileOp::Path { from, .. } = &op {
            if self.say_if_move_refused(from) {
                return;
            }
        }
        // By now, everything this closes has nothing to lose.
        self.dismiss_dialog();
        self.nb.file_op = Some(op);
    }

    /// Says so if the open document is `from` and can't be moved, and returns whether it did.
    fn say_if_move_refused(&mut self, from: &Path) -> bool {
        if !same_document(self.session.path(), Some(from)) {
            return false;
        }
        let Some(refusal) = why_not_move(self.session.save_failed, self.session.newer) else {
            return false;
        };
        self.notice_tracker.announced();
        self.notice = Some(refusal.to_string());
        true
    }

    /// The dialog a document's menu opened.
    pub(crate) fn show_file_op(&mut self, ctx: &egui::Context, enter: bool) {
        match self.nb.file_op.take() {
            Some(FileOp::Path {
                from,
                mut field,
                mut refusal,
                rename,
                focus,
            }) => {
                // A chord and an Enter arriving in the same frame would otherwise submit a path
                // nobody has looked at.
                let mut submit = enter && !focus;
                let mut cancel = false;
                let selection = focus.then(|| path_selection(&field, rename));
                let error = self.palette.error_ink;
                let root = self.nb.notebook.as_ref().map(|nb| nb.root.as_path());
                let hint = move_hint(rename, &from, root);
                egui::Window::new(if rename { "Rename" } else { "Move" })
                    .id(egui::Id::unique("kame-file-op-path-dialog"))
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .collapsible(false)
                    .resizable(false)
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        ui.label("Its path in this notebook:");
                        let edit = path_field(ui, &mut field, selection);
                        if edit.changed() {
                            refusal = None;
                        }
                        if let Some(hint) = hint {
                            ui.weak(hint);
                        }
                        if let Some(refusal) = &refusal {
                            ui.colored_label(error, refusal);
                        }
                        ui.horizontal(|ui| {
                            if ui.button(if rename { "Rename" } else { "Move" }).clicked() {
                                submit = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancel = true;
                            }
                        });
                    });
                if cancel {
                    return;
                }
                if submit {
                    match self.move_to_typed_path(&from, &field) {
                        Ok(()) => return,
                        Err(why) => refusal = Some(why),
                    }
                }
                self.nb.file_op = Some(FileOp::Path {
                    from,
                    field,
                    refusal,
                    rename,
                    focus: false,
                });
            }
            Some(FileOp::Delete { path, title }) => {
                let (mut delete, mut cancel) = (false, false);
                egui::Window::new("Delete")
                    .id(egui::Id::unique("kame-file-op-delete-dialog"))
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .collapsible(false)
                    .resizable(false)
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        ui.label(format!("Delete {title}? This can't be undone."));
                        ui.horizontal(|ui| {
                            delete = ui.button("Delete").clicked();
                            cancel = ui.button("Cancel").clicked();
                        });
                    });
                if delete {
                    self.delete_from_notebook(&path, &title);
                } else if !cancel {
                    self.nb.file_op = Some(FileOp::Delete { path, title });
                }
            }
            None => {}
        }
    }

    /// Moves a document to the path typed for it. `Err` is a refusal for the dialog, with nothing
    /// changed.
    fn move_to_typed_path(&mut self, from: &Path, typed: &str) -> Result<(), String> {
        let Some(notebook) = self.nb.notebook.clone() else {
            return Ok(());
        };
        let to = check_new_path(&notebook, from, typed)?;
        // Going nowhere is no reason to leave the document and come back.
        if to == from {
            return Ok(());
        }
        self.move_document(&notebook, from, &to)
    }

    fn move_document(&mut self, notebook: &Notebook, from: &Path, to: &Path) -> Result<(), String> {
        let moving_open = same_document(self.session.path(), Some(from));
        if self.say_if_move_refused(from) {
            return Ok(());
        }
        // This has to go before the document is left, or leaving would take away a new document
        // nobody has written in, which is about to be somewhere on purpose. It's also what lets the
        // sync below give it logs to move.
        self.forget_fresh(from);
        self.save_position_now();
        self.sync_logs();
        // That can have failed to save what it merged, and then the file isn't what's on the screen.
        if self.say_if_move_refused(from) {
            return Ok(());
        }
        self.ensure_device();
        let device = self.device.clone();
        let now = now_unix();
        // Before anything happens, since there's nothing to leave for if this fails.
        let blank = if moving_open {
            Some(
                Session::anonymous()
                    .map_err(|err| format!("could not leave this document — {err}"))?,
            )
        } else {
            None
        };
        // The plan reads the open document from the screen, so it comes before leaving it.
        let plan = plan_move(
            notebook,
            from,
            to,
            self.session
                .path()
                .map(|path| (path, self.session.document())),
            &device,
            now,
        );
        let (caret, scroll) = (self.session.caret, self.page_scroll);
        // Its session is closed before the files move: a connection held across the rename would go
        // on using its -wal under the old name, with the database under the new one.
        if let Some(blank) = blank {
            self.adopt_session(blank);
        }
        let open = (!moving_open).then_some(&mut self.session);
        let result = carry_out(notebook, plan, open, &device, now);
        if moving_open {
            // Only now, since carry_out has changed the file's links and title for it to read.
            let back_to = if result.is_ok() { to } else { from };
            if there_to_open(back_to) {
                self.pending_heading = None;
                self.open_document(back_to.display().to_string());
                if same_document(self.session.path(), Some(back_to)) {
                    self.session.set_caret(caret);
                    self.session.restored_scroll = None;
                    self.pending_scroll = Some(scroll);
                }
            } else {
                self.session.last_error = Some(format!("{} is no longer there", back_to.display()));
            }
        }
        let report = match result {
            Ok(report) => report,
            Err(why) => {
                self.notice = None;
                return Err(why);
            }
        };
        self.back.moved(from, to);
        for recent in &mut self.nb.recent {
            follow_move(recent, from, to);
        }
        // An import has no file of its own, and its links and images are read from where this is.
        if let Some(source) = &mut self.session.source_path {
            follow_move(source, from, to);
        }
        if self.last_document.moved(from, to) {
            self.persist();
        }
        if !moving_open {
            // carry_out saves its change to the open document, and leaves its logs to this.
            self.sync_logs();
        }
        self.notebook_files_changed();
        self.notice_tracker.announced();
        if report.left.is_empty() {
            let relative = to.strip_prefix(&notebook.root).unwrap_or(to).display();
            self.notice = Some(moved_notice(&relative.to_string(), report.changed.len()));
        } else {
            self.notice = None;
            self.nb.move_report = Some(report);
        }
        Ok(())
    }

    /// Stops treating `path` as a new document that may be taken away.
    fn forget_fresh(&mut self, path: &Path) {
        if self
            .nb
            .fresh
            .as_ref()
            .is_some_and(|fresh| same_place(&fresh.path, path))
        {
            self.nb.fresh = None;
        }
    }

    /// Looks again at what's in the notebook and what links here. Clearing `looked_from` covers it
    /// with a document open, and the rest is for an anonymous one, which that doesn't reach.
    fn notebook_files_changed(&mut self) {
        self.nb.looked_from = None;
        self.nb.refresh_listing();
        self.start_backlinks();
    }

    /// Deletes a document, for good. If it's open it's left first, since a session's connection
    /// would otherwise go on writing to a file with no name, and leave a -wal beside nothing.
    fn delete_from_notebook(&mut self, path: &Path, title: &str) {
        self.save_position_now();
        self.sync_logs();
        if same_document(self.session.path(), Some(path)) && !self.leave_to_delete(path) {
            return;
        }
        // Leaving a new document nobody has written in has already taken it away.
        let mut left_logs = false;
        if path.symlink_metadata().is_ok() || log_folder(path).exists() {
            match delete_document(path) {
                Ok(left) => left_logs = left,
                Err(err) => {
                    self.session.last_error =
                        Some(format!("could not delete {} — {err}", path.display()));
                    self.notebook_files_changed();
                    return;
                }
            }
        }
        self.back.forget(path);
        self.nb.recent.retain(|recent| !same_place(recent, path));
        if self.last_document.is(path) {
            self.last_document.forget();
            self.persist();
        }
        self.forget_fresh(path);
        self.notebook_files_changed();
        self.notice_tracker.announced();
        self.notice = Some(deleted_notice(title, left_logs));
    }

    /// Goes to the contents page, or a new document if that's what's being deleted or it won't open.
    /// Returns whether the document has been left.
    fn leave_to_delete(&mut self, path: &Path) -> bool {
        let index = self
            .nb
            .notebook
            .as_ref()
            .and_then(Notebook::index)
            .filter(|index| !same_document(Some(index), Some(path)));
        let mut why_not_index = None;
        if let Some(index) = index {
            self.pending_heading = None;
            self.open_document(index.display().to_string());
            if same_document(self.session.path(), Some(path)) {
                why_not_index = self.session.last_error.clone();
            }
        }
        if same_document(self.session.path(), Some(path)) {
            match Session::anonymous() {
                Ok(session) => {
                    self.adopt_session(session);
                    // It was said on the session just left, and would go with it.
                    self.session.last_error = why_not_index;
                }
                Err(err) => {
                    self.session.last_error = Some(format!("could not leave it — {err}"));
                    return false;
                }
            }
        }
        // A prompt about this file having been replaced was about what's now gone.
        self.replaced_prompt = false;
        true
    }

    /// What a move couldn't change, when there's something to name.
    pub(crate) fn show_move_report(&mut self, ctx: &egui::Context) {
        let Some(report) = &self.nb.move_report else {
            return;
        };
        let mut close = false;
        egui::Window::new("Moved")
            .id(egui::Id::unique("kame-move-report"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Updated links in {}. These weren't changed:",
                    documents(report.changed.len())
                ));
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for left in &report.left {
                            ui.label(format!("{}: {}", left.relative, left.why));
                        }
                    });
                close = ui.button("Close").clicked();
            });
        if close {
            self.nb.move_report = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the field for a frame in a window, as the dialog does, with `events` as what arrived.
    fn field_frame(
        ctx: &egui::Context,
        field: &mut String,
        selection: Option<Range<usize>>,
        events: Vec<egui::Event>,
    ) {
        let input = egui::RawInput {
            events,
            ..egui::RawInput::default()
        };
        let mut selection = selection;
        ctx.run_ui(input, |ui| {
            egui::Window::new("field").show(ui.ctx(), |ui| {
                path_field(ui, field, selection.take());
            });
        })
        .drop_without_applying_deltas();
    }

    /// What's selected in the field, as the start and end of a range.
    fn selected_in_field(ctx: &egui::Context) -> Range<usize> {
        let state = egui::TextEdit::load_state(ctx, path_field_id()).expect("it was drawn");
        let range = state.cursor.char_range().expect("something is selected");
        let [start, end] = range.sorted_cursors();
        start.index.into()..end.index.into()
    }

    #[test]
    fn the_field_opens_with_the_selection_it_was_given_and_keeps_it() {
        let ctx = egui::Context::default();
        let mut field = "a/b/doc.kame".to_string();
        field_frame(&ctx, &mut field, Some(4..7), Vec::new());
        assert_eq!(selected_in_field(&ctx), 4..7, "the frame it opens");
        assert!(ctx.memory(|memory| memory.has_focus(path_field_id())));
        field_frame(&ctx, &mut field, None, Vec::new());
        assert_eq!(selected_in_field(&ctx), 4..7, "the frame after");
    }

    /// A click on nothing in the field, as the one on a menu item is.
    fn click_elsewhere() -> Vec<egui::Event> {
        let at = egui::Pos2::new(700.0, 500.0);
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        vec![egui::Event::PointerMoved(at), button(true), button(false)]
    }

    #[test]
    fn the_click_that_closes_the_menu_leaves_the_field_selected_and_ready() {
        let ctx = egui::Context::default();
        let mut field = "a/b/doc.kame".to_string();
        field_frame(&ctx, &mut field, Some(4..7), click_elsewhere());
        assert!(ctx.memory(|memory| memory.has_focus(path_field_id())));
        field_frame(&ctx, &mut field, None, Vec::new());
        assert_eq!(selected_in_field(&ctx), 4..7);

        // Opening again, in a window that has been drawn before.
        ctx.memory_mut(|memory| memory.surrender_focus(path_field_id()));
        field_frame(&ctx, &mut field, None, Vec::new());
        field_frame(&ctx, &mut field, Some(0..3), click_elsewhere());
        assert!(ctx.memory(|memory| memory.has_focus(path_field_id())));
        field_frame(&ctx, &mut field, None, Vec::new());
        assert_eq!(selected_in_field(&ctx), 0..3);
        field_frame(
            &ctx,
            &mut field,
            None,
            vec![egui::Event::Text("x".to_string())],
        );
        assert_eq!(field, "x/doc.kame");
    }

    #[test]
    fn typing_over_a_rename_replaces_the_name() {
        let ctx = egui::Context::default();
        let mut field = "alpha.kame".to_string();
        let selection = path_selection(&field, true);
        field_frame(&ctx, &mut field, Some(selection), Vec::new());
        field_frame(&ctx, &mut field, None, Vec::new());
        field_frame(
            &ctx,
            &mut field,
            None,
            vec![egui::Event::Text("omega".to_string())],
        );
        assert_eq!(field, "omega.kame");
    }

    #[test]
    fn typing_over_a_move_replaces_the_folders() {
        let ctx = egui::Context::default();
        let mut field = "é/matière.kame".to_string();
        let selection = path_selection(&field, false);
        field_frame(&ctx, &mut field, Some(selection), Vec::new());
        field_frame(
            &ctx,
            &mut field,
            None,
            vec![egui::Event::Text("x/y".to_string())],
        );
        assert_eq!(field, "x/y/matière.kame");
    }

    #[test]
    fn reopening_the_field_selects_again_whatever_it_last_held() {
        let ctx = egui::Context::default();
        let mut field = "one/long/name.kame".to_string();
        field_frame(&ctx, &mut field, Some(9..13), Vec::new());
        ctx.memory_mut(|memory| memory.surrender_focus(path_field_id()));
        field_frame(&ctx, &mut field, None, Vec::new());
        let mut other = "b.kame".to_string();
        field_frame(&ctx, &mut other, Some(0..1), Vec::new());
        assert_eq!(selected_in_field(&ctx), 0..1);
    }

    #[test]
    fn a_dialog_that_may_hold_typing_or_wants_an_answer_makes_a_menu_choice_wait() {
        for dialog in [
            OpenDialog::FileOp,
            OpenDialog::Replaced,
            OpenDialog::ClosePrompt,
            OpenDialog::Metadata,
            OpenDialog::Snapshot,
            OpenDialog::OpenPath,
            OpenDialog::Link,
        ] {
            assert!(dialog.makes_menu_wait(), "{dialog:?}");
        }
    }

    #[test]
    fn a_dialog_with_nothing_typed_gives_way_to_a_menu_choice() {
        for dialog in [
            OpenDialog::JumpList,
            OpenDialog::Search,
            OpenDialog::MoveReport,
            OpenDialog::SiteReport,
            OpenDialog::RestorePicker,
        ] {
            assert!(!dialog.makes_menu_wait(), "{dialog:?}");
        }
    }

    #[test]
    fn rename_selects_the_name_and_move_selects_the_folders() {
        assert_eq!(path_selection("a/b/doc.kame", true), 4..7);
        assert_eq!(path_selection("a/b/doc.kame", false), 0..3);
        assert_eq!(path_selection("doc.kame", true), 0..3);
        assert_eq!(path_selection("doc.kame", false), 0..0);
    }

    #[test]
    fn the_selection_counts_characters_not_bytes() {
        assert_eq!(path_selection("é/matière.kame", true), 2..9);
        assert_eq!(path_selection("é/matière.kame", false), 0..1);
    }

    #[test]
    fn a_name_with_dots_in_it_keeps_all_but_its_extension_selected() {
        assert_eq!(path_selection("a/v1.2.kame", true), 2..6);
        assert_eq!(path_selection("a/plain", true), 2..7);
    }

    #[test]
    fn nothing_is_there_to_open_again_if_it_has_gone() {
        let root = kame_test_dir::folder("app-reopen-nothing");
        let gone = root.join("gone.kame");
        assert!(!there_to_open(&gone));
        assert!(!gone.exists(), "and asking made nothing");
    }

    #[test]
    fn empty_logs_are_nothing_to_open_again() {
        let root = kame_test_dir::folder("app-reopen-empty-logs");
        let path = root.join("a.kame");
        std::fs::create_dir_all(kame_notebook::log_folder(&path)).unwrap();
        assert!(!there_to_open(&path));
        assert!(!path.exists());
    }

    #[test]
    fn a_file_or_logs_that_can_build_one_are_there_to_open_again() {
        use kame_sync::logs::{Logged, MARKER};

        let root = kame_test_dir::folder("app-reopen-there");
        std::fs::write(root.join(MARKER), "").unwrap();
        let path = root.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("Words.");
        Logged::open(&session, "titan-bbbb")
            .unwrap()
            .sync(&mut session, 100)
            .unwrap();
        drop(session);
        assert!(there_to_open(&path));

        std::fs::remove_file(&path).unwrap();
        assert!(there_to_open(&path));
        assert!(path.exists(), "built from its logs");
    }

    #[test]
    fn a_notice_says_what_cannot_be_settled_and_why() {
        let one = Unsettled {
            waiting: 1,
            because: "essays/a.kame".to_string(),
        };
        assert_eq!(
            unsettled_notice(&one),
            "1 document looks left behind, and can't be settled because essays/a.kame's logs \
             can't all be read"
        );
        let two = Unsettled { waiting: 2, ..one };
        assert_eq!(
            unsettled_notice(&two),
            "2 documents look left behind, and can't be settled because essays/a.kame's logs \
             can't all be read"
        );
    }

    #[test]
    fn what_cannot_be_settled_is_said_only_when_there_is_something_new_to_say() {
        let one = Unsettled {
            waiting: 1,
            because: "a.kame".to_string(),
        };
        let two = Unsettled {
            waiting: 2,
            ..one.clone()
        };
        let mut said = None;
        assert_eq!(new_to_say(&mut said, Some(one.clone())), Some(one.clone()));
        assert_eq!(new_to_say(&mut said, Some(one.clone())), None);
        assert_eq!(new_to_say(&mut said, Some(two.clone())), Some(two));
        assert_eq!(new_to_say(&mut said, None), None);
        assert_eq!(new_to_say(&mut said, None), None);
        assert_eq!(new_to_say(&mut said, Some(one.clone())), Some(one));
    }

    #[test]
    fn a_moved_notice_says_where_and_how_many_changed() {
        assert_eq!(moved_notice("a/b.kame", 0), "moved to a/b.kame");
        assert_eq!(
            moved_notice("a/b.kame", 1),
            "moved to a/b.kame, and updated links in 1 document"
        );
        assert_eq!(
            moved_notice("a/b.kame", 3),
            "moved to a/b.kame, and updated links in 3 documents"
        );
    }

    #[test]
    fn a_document_with_a_failed_save_or_from_a_newer_version_is_not_moved() {
        assert!(why_not_move(false, false).is_none());
        assert!(why_not_move(true, false).unwrap().contains("failed"));
        // A newer document also counts as failing to save, and that isn't what to say.
        assert!(why_not_move(true, true).unwrap().contains("newer"));
        assert!(why_not_move(false, true).unwrap().contains("newer"));
    }

    #[test]
    fn a_stranded_database_waits_ten_minutes_from_when_it_was_first_seen() {
        let start = Instant::now();
        let path = PathBuf::from("/n/a.kame");
        let mut seen = HashMap::new();
        assert!(stranded_ready(&mut seen, std::slice::from_ref(&path), start).is_empty());
        assert!(stranded_ready(
            &mut seen,
            std::slice::from_ref(&path),
            start + Duration::from_secs(599)
        )
        .is_empty());
        assert_eq!(
            stranded_ready(
                &mut seen,
                std::slice::from_ref(&path),
                start + Duration::from_secs(600)
            ),
            std::slice::from_ref(&path)
        );
    }

    #[test]
    fn a_database_that_stops_being_stranded_starts_again() {
        let start = Instant::now();
        let path = PathBuf::from("/n/a.kame");
        let mut seen = HashMap::new();
        stranded_ready(&mut seen, std::slice::from_ref(&path), start);
        stranded_ready(&mut seen, &[], start + Duration::from_secs(300));
        assert!(seen.is_empty());
        assert!(stranded_ready(
            &mut seen,
            std::slice::from_ref(&path),
            start + Duration::from_secs(600)
        )
        .is_empty());
    }

    #[test]
    fn the_notice_for_a_retirement_says_where_it_is_and_what_it_holds() {
        let retired = |opened_there, unsynced| {
            stranded_notice(&[Done::Retired {
                from: PathBuf::from("/n/a.kame"),
                to: PathBuf::from("/n/.a.retired.kame"),
                opened_there,
                unsynced,
            }])
        };
        let words = retired(false, true);
        assert!(words.contains("/n/.a.retired.kame"), "{words}");
        assert!(words.contains("words that never synced"), "{words}");
        assert!(!words.contains("undo"), "{words}");
        let copy = retired(true, false);
        assert!(copy.contains("only a copy of what's in the logs"), "{copy}");
        assert!(
            copy.contains("undo history is in the retired file"),
            "{copy}"
        );
    }

    fn retired(name: &str, unsynced: bool) -> Done {
        Done::Retired {
            from: PathBuf::from(format!("/n/{name}.kame")),
            to: PathBuf::from(format!("/n/.{name}.retired.kame")),
            opened_there: false,
            unsynced,
        }
    }

    #[test]
    fn the_notice_for_several_says_each_with_retirements_first() {
        let text = stranded_notice(&[
            Done::Moved {
                from: PathBuf::from("/n/a.kame"),
                to: PathBuf::from("/n/b.kame"),
            },
            retired("c", true),
            retired("e", false),
        ]);
        let (c, e, a) = (
            text.find("/n/.c.retired.kame").expect(&text),
            text.find("/n/.e.retired.kame").expect(&text),
            text.find("/n/b.kame").expect(&text),
        );
        assert!(c < e && e < a, "{text}");
        assert!(text.contains("words that never synced"), "{text}");
        assert!(text.contains("only a copy"), "{text}");
        assert!(!text.contains("more"), "{text}");
    }

    #[test]
    fn the_notice_past_three_counts_the_rest() {
        let done: Vec<Done> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|name| retired(name, true))
            .collect();
        let text = stranded_notice(&done);
        assert!(text.contains("/n/.c.retired.kame"), "{text}");
        assert!(!text.contains("/n/.d.retired.kame"), "{text}");
        assert!(text.ends_with("and 2 more."), "{text}");
    }

    #[test]
    fn a_delete_that_left_logs_says_so() {
        assert_eq!(deleted_notice("Essay", false), "deleted Essay");
        assert_eq!(
            deleted_notice("Essay", true),
            "deleted Essay; the logs at that path may be another document's, \
             and were left"
        );
    }

    #[test]
    fn moving_a_document_at_the_root_says_how() {
        let root = Path::new("/n");
        let hint = Some("type a folder and / before the name");
        assert_eq!(move_hint(false, Path::new("/n/a.kame"), Some(root)), hint);
        assert_eq!(move_hint(true, Path::new("/n/a.kame"), Some(root)), None);
        assert_eq!(move_hint(false, Path::new("/n/x/a.kame"), Some(root)), None);
        assert_eq!(move_hint(false, Path::new("/n/a.kame"), None), None);
    }

    /// A map with one database in it, first seen stranded at `first`.
    fn seen_at(first: Instant) -> HashMap<PathBuf, Instant> {
        HashMap::from([(PathBuf::from("/n/a.kame"), first)])
    }

    #[test]
    fn a_pass_is_due_when_asked_or_when_there_has_never_been_one() {
        let now = Instant::now();
        assert!(pass_due(true, &HashMap::new(), Some(now), now));
        assert!(pass_due(true, &seen_at(now), Some(now), now));
        assert!(pass_due(false, &HashMap::new(), None, now));
    }

    #[test]
    fn with_nothing_stranded_a_pass_is_due_every_wait() {
        let looked = Instant::now();
        let nothing = HashMap::new();
        assert!(!pass_due(
            false,
            &nothing,
            Some(looked),
            looked + STRANDED_RECHECK
        ));
        assert!(pass_due(
            false,
            &nothing,
            Some(looked),
            looked + STRANDED_WAIT
        ));
    }

    #[test]
    fn while_something_is_waiting_out_its_time_a_pass_is_due_every_minute() {
        let first = Instant::now();
        let looked = first + STRANDED_RECHECK;
        let seen = seen_at(first);
        assert!(!pass_due(
            false,
            &seen,
            Some(looked),
            looked + STRANDED_RECHECK / 2
        ));
        assert!(pass_due(
            false,
            &seen,
            Some(looked),
            looked + STRANDED_RECHECK
        ));
    }

    #[test]
    fn the_time_running_out_since_the_last_look_still_brings_a_pass_in_a_minute() {
        let first = Instant::now();
        let looked = first + STRANDED_WAIT - STRANDED_RECHECK / 2;
        let now = looked + STRANDED_RECHECK;
        assert!(now > first + STRANDED_WAIT);
        assert!(pass_due(false, &seen_at(first), Some(looked), now));
    }

    #[test]
    fn once_everything_has_waited_and_been_looked_at_a_pass_is_due_every_wait() {
        let first = Instant::now();
        let looked = first + STRANDED_WAIT;
        let seen = seen_at(first);
        assert!(!pass_due(
            false,
            &seen,
            Some(looked),
            looked + STRANDED_RECHECK
        ));
        assert!(pass_due(false, &seen, Some(looked), looked + STRANDED_WAIT));
    }

    #[test]
    fn one_still_waiting_keeps_the_minutes_going_for_one_that_has_waited() {
        let first = Instant::now();
        let looked = first + STRANDED_WAIT;
        let mut seen = seen_at(first);
        seen.insert(PathBuf::from("/n/b.kame"), looked);
        assert!(pass_due(
            false,
            &seen,
            Some(looked),
            looked + STRANDED_RECHECK
        ));
    }

    #[test]
    fn a_retired_database_comes_back_on_a_pass_where_nothing_is_ready() {
        use kame_core::session::Session;
        use kame_sync::logs::{log_folder, Logged, MARKER};

        let root = kame_test_dir::folder("app-stranded-return")
            .canonicalize()
            .unwrap();
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        std::fs::write(root.join(MARKER), "").unwrap();
        let notebook = Notebook::open(&root).unwrap();
        let (a, b) = (root.join("a.kame"), root.join("b.kame"));
        let mut session = Session::open(&a).unwrap();
        session.insert("Words.");
        Logged::open(&session, "titan-bbbb")
            .unwrap()
            .sync(&mut session, 100)
            .unwrap();
        drop(session);
        let carried = root.join(".carried");
        std::fs::rename(log_folder(&a), &carried).unwrap();

        let start = Instant::now();
        let (seen, healthy, steps) =
            read_pass(&notebook, HashMap::new(), Healthy::default(), start);
        assert!(apply(&notebook, steps, None).is_empty(), "first sighting");
        let (seen, healthy, steps) = read_pass(&notebook, seen, healthy, start + STRANDED_WAIT);
        assert_eq!(apply(&notebook, steps, None).len(), 1);
        let retired = root.join(".a.retired.kame");
        assert!(retired.is_file());

        std::fs::rename(&carried, log_folder(&b)).unwrap();
        let (seen, _, steps) = read_pass(&notebook, seen, healthy, start + STRANDED_WAIT);
        assert!(seen.is_empty());
        assert_eq!(
            apply(&notebook, steps, None),
            [Done::Returned {
                from: retired,
                to: b
            }]
        );
    }

    #[test]
    fn the_text_stays_put_if_the_browser_fits_in_the_gutter() {
        assert_eq!(
            page_left_beside_browser(400.0, 800.0, 400.0, 1600.0),
            Some(400.0 - BROWSER_WIDTH)
        );
    }

    #[test]
    fn the_text_moves_only_as_far_as_it_has_to() {
        assert_eq!(
            page_left_beside_browser(200.0, 500.0, 300.0, 1100.0),
            Some(BROWSER_GAP)
        );
    }

    #[test]
    fn without_room_for_the_text_and_its_margin_the_page_hides() {
        assert_eq!(page_left_beside_browser(200.0, 600.0, 400.0, 1100.0), None);
        assert_eq!(page_left_beside_browser(20.0, 500.0, 0.0, 700.0), None);
        assert!(page_left_beside_browser(20.0, 500.0, 0.0, 800.0).is_some());
        assert!(
            page_left_beside_browser(186.0, 365.0, 146.0, 730.0).is_some(),
            "a window this wide has room"
        );
    }

    #[test]
    fn a_relative_link_is_one_without_a_scheme_or_a_heading() {
        assert!(is_relative_link("static/paper.pdf"));
        assert!(is_relative_link("../a.kame"));
        assert!(!is_relative_link("https://example.com/"));
        assert!(!is_relative_link("mailto:a@b.c"));
        assert!(!is_relative_link("#Heading"));
    }
}
