//! A notebook on the screen: the jump list, search, the browser, and what links here. What these
//! know about a notebook comes from `kame-notebook`, on background threads, since it reads every
//! document. See `docs/superpowers/specs/2026-11-03-notebook-design.md`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use eframe::egui;
use kame_core::model::{BlockId, Metadata};
use kame_notebook::links::{create_document, linking_to, remove_untouched, Follow, LinkFrom, Made};
use kame_notebook::list::{listed, listed_if_changed, narrow, rows, Kind, Listed, RowKind};
use kame_notebook::search::{search, Hit};
use kame_notebook::Notebook;

use super::{same_document, BackStop, KameApp};

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

#[derive(Default)]
pub(crate) struct NotebookUi {
    pub(crate) notebook: Option<Notebook>,
    /// The file the notebook was last looked for from.
    looked_from: Option<PathBuf>,
    listing: Vec<Listed>,
    listing_coming: Option<Receiver<Vec<Listed>>>,
    listing_checked: Option<std::time::Instant>,
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
                self.start_backlinks();
            }
        }
        self.nb.check_listing();
        if let Some(coming) = &self.nb.listing_coming {
            match coming.try_recv() {
                Ok(listing) => {
                    self.nb.listing = listing;
                    self.nb.listing_coming = None;
                }
                Err(TryRecvError::Disconnected) => self.nb.listing_coming = None,
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
            }
        }
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
                                    if ui.selectable_label(here, &row.name).clicked() {
                                        choose = Some((entry.path.clone(), entry.kind));
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
