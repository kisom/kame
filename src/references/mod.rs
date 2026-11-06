//! A notebook's references, from the app: keeping up with its list, what a key looks like on the
//! page, the notes that come of keys with no reference, and the windows for citing and looking
//! after them. The page and the caret keys shape `with_citation_spans` of a block, a copy that is
//! never edited, so a known key is coloured, kept whole and clicked on as any link is. See
//! `docs/superpowers/specs/2026-11-04-references-design.md`.
//!
//! This holds what the windows share and what the rest of the app calls. `page` is what the page
//! draws of keys and the entry a click on one opens, `notice` the note about a key with no
//! reference, `cite` Cite and its search, `form` the new reference and edit windows, `list` the
//! References… window, which asks before a delete or a rename, `rename` a rename once it's asked
//! for, and `browser` the browser's section for references.

mod browser;
mod cite;
mod form;
mod list;
mod notice;
mod page;
mod rename;
#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use browser::{draw_browser_references, BrowserReference, BROWSER_SECTION};
pub(crate) use page::Page;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use egui::Pos2;
use kame_core::references::References;
use kame_notebook::references::{load, stamp, Loaded};
use kame_notebook::Notebook;

use super::KameApp;

use cite::CiteSearch;
use form::FormWindow;
use list::ListWindow;
use notice::key_in_note;
use rename::Renaming;

/// How long to go without looking for a change to the reference files, since the page asks every
/// frame.
const CHECK_EVERY: Duration = Duration::from_secs(1);
/// How often to look for what a background thread has found.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The notebook's references, and which one is showing.
#[derive(Default)]
pub(crate) struct ReferencesUi {
    /// What the notebook on screen holds. Nothing outside one.
    pub(crate) list: References,
    /// What is wrong with the files left out of the list, as of the last time they were read.
    problems: Vec<String>,
    /// The files the list was read from, as they were.
    stamp: Vec<(String, SystemTime, u64)>,
    /// The notebook the list was read from.
    root: Option<PathBuf>,
    /// When the files were last looked at.
    checked: Option<Instant>,
    /// A look at the files under way, on a background thread.
    reading: Option<Receiver<Read>>,
    /// The key whose entry is in its window.
    pub(crate) shown: Option<String>,
    /// Where the click that opened it was.
    shown_at: Pos2,
    /// How far the page was scrolled when the entry was first drawn.
    shown_scroll: Option<f32>,
    /// The search Ctrl-Shift-C opens.
    cite_search: Option<CiteSearch>,
    /// The new reference or edit window.
    form: Option<FormWindow>,
    /// The References… window.
    window: Option<ListWindow>,
    /// How many documents cite each key, once counted.
    counts: Option<BTreeMap<String, usize>>,
    /// The count under way, on a background thread.
    counting: Option<Receiver<BTreeMap<String, Vec<PathBuf>>>>,
    /// Whether the list has changed since the count was started.
    recount: bool,
    /// Whether the last count stopped before finding anything.
    count_stopped: bool,
    /// The rename under way, on a background thread.
    renaming: Option<Renaming>,
}

/// What a background look at the reference files found.
struct Read {
    /// The files as they were before anything was read.
    files: Vec<(String, SystemTime, u64)>,
    /// What they hold, unless they were as they had been.
    loaded: Option<Loaded>,
}

fn enter_pressed(ctx: &egui::Context) -> bool {
    ctx.input(|input| {
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
    })
}

impl ReferencesUi {
    /// Keeps the list up with `notebook` and its files, and with what's wrong with them. The files
    /// are read on a background thread, and what it found is taken in on a later call.
    pub(crate) fn refresh(&mut self, notebook: Option<&Notebook>) {
        let Some(notebook) = notebook else {
            // A rename goes on, and its window stays until it has been read to say how it went.
            let renaming = self.renaming.take();
            let window = self
                .window
                .take()
                .filter(|window| renaming.is_some() || !window.said.is_empty());
            *self = ReferencesUi {
                renaming,
                window,
                ..ReferencesUi::default()
            };
            return;
        };
        let moved = self.root.as_ref() != Some(&notebook.root);
        if moved {
            self.root = Some(notebook.root.clone());
            // Another notebook's list mustn't show here while this one's is read.
            self.list = References::new();
            self.problems.clear();
            self.stamp.clear();
            self.close_windows();
            self.counts = None;
            self.counting = None;
            // Dropping a look at the other notebook leaves it nowhere to send what it finds.
            self.reading = None;
            self.checked = None;
        }
        self.take_read();
        let due = self
            .checked
            .is_none_or(|checked| checked.elapsed() >= CHECK_EVERY);
        if self.reading.is_some() || !due {
            return;
        }
        self.checked = Some(Instant::now());
        let (send, coming) = channel();
        self.reading = Some(coming);
        let notebook = notebook.clone();
        let known = (!moved).then(|| self.stamp.clone());
        std::thread::spawn(move || {
            // Taken before reading, so that a file changing in between is read again next time.
            let files = stamp(&notebook);
            let loaded = (known.as_ref() != Some(&files)).then(|| load(&notebook));
            let _ = send.send(Read { files, loaded });
        });
    }

    /// Whether the files are being looked at.
    pub(crate) fn is_reading(&self) -> bool {
        self.reading.is_some()
    }

    /// Takes in what the look at the files under way found, if it's done.
    fn take_read(&mut self) {
        let Some(coming) = &self.reading else {
            return;
        };
        match coming.try_recv() {
            Ok(read) => {
                self.reading = None;
                self.stamp = read.files;
                if let Some(loaded) = read.loaded {
                    self.take_loaded(loaded);
                }
            }
            Err(TryRecvError::Disconnected) => self.reading = None,
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Reads the list now, for a change made here that should show at once. The files may look no
    /// different from before, if it was quick and kept their length. A look begun before the
    /// change could find the files as they were, so it's dropped.
    fn reload(&mut self, notebook: &Notebook) {
        self.checked = Some(Instant::now());
        self.reading = None;
        self.stamp = stamp(notebook);
        self.take_loaded(load(notebook));
    }

    fn take_loaded(&mut self, loaded: Loaded) {
        self.recount = true;
        self.list = loaded.references;
        self.problems = loaded.problems;
    }

    /// What the status row says is wrong with the files now, if anything is.
    pub(crate) fn problem_line(&self) -> Option<String> {
        (!self.problems.is_empty())
            .then(|| format!("this notebook's references: {}", self.problems.join("; ")))
    }

    /// Whether any of the windows for references is up.
    pub(crate) fn window_open(&self) -> bool {
        self.shown.is_some()
            || self.cite_search.is_some()
            || self.form.is_some()
            || self.window.is_some()
    }

    /// Whether a form or the Cite search is up, either of which may hold something typed.
    pub(crate) fn typing_open(&self) -> bool {
        self.form.is_some() || self.cite_search.is_some()
    }

    /// Closes everything, for another document or notebook, but for the window of a rename under
    /// way, which has yet to say how it went.
    pub(crate) fn close_windows(&mut self) {
        self.shown = None;
        self.cite_search = None;
        self.form = None;
        if self.renaming.is_none() {
            self.window = None;
        }
    }

    /// Closes the window on top, as Escape does. The form goes back to the window it came from,
    /// and a rename under way keeps its window up to say how it went.
    pub(crate) fn dismiss(&mut self) {
        if self.shown.take().is_some()
            || self.cite_search.take().is_some()
            || self.form.take().is_some()
            || self.renaming.is_some()
        {
            return;
        }
        let asking = self
            .window
            .as_mut()
            .is_some_and(|window| window.confirming.take().is_some());
        if !asking {
            self.window = None;
        }
    }

    /// Opens `key`'s entry, for a click at `at`.
    pub(crate) fn show(&mut self, key: &str, at: Pos2) {
        self.shown = Some(key.to_string());
        self.shown_at = at;
        self.shown_scroll = None;
    }
}

impl KameApp {
    /// Reads the list after a change made here, so that it shows at once, or for an export, which
    /// can't go by a list still being read, as it is just after a notebook opens.
    pub(crate) fn reload_references(&mut self) {
        let Some(notebook) = self.nb.notebook.clone() else {
            return;
        };
        self.refs.reload(&notebook);
    }

    /// Whether the keyboard is with something over the windows for references, so that an Enter
    /// is not for them: another dialog, the close prompt, or the find bar or a note's box if it
    /// has the keys. Those two can stay up beside a window without having them, and help takes
    /// no keys but F1 and Escape.
    fn keys_are_elsewhere(&self, ctx: &egui::Context) -> bool {
        self.other_dialog_open()
            || self.close_flow.prompting
            || ctx.memory(|memory| {
                memory.has_focus(super::find_query_id())
                    || memory.has_focus(super::note_body_edit_id())
            })
    }

    /// Takes in what the background threads have finished, and drops the note about a key once
    /// it has a reference, however it got one.
    pub(crate) fn references_tick(&mut self, ctx: &egui::Context) {
        if self.refs.is_reading() {
            ctx.request_repaint_after(LOOK_AGAIN);
        }
        let noted = self.notice.as_deref().and_then(key_in_note);
        if noted.is_some_and(|key| self.refs.list.contains(key)) {
            self.notice = None;
        }
        if let Some(coming) = &self.refs.counting {
            match coming.try_recv() {
                Ok(found) => {
                    self.refs.counts = Some(
                        found
                            .into_iter()
                            .map(|(key, documents)| (key, documents.len()))
                            .collect(),
                    );
                    self.refs.counting = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.refs.counting = None;
                    self.refs.count_stopped = true;
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(LOOK_AGAIN),
            }
        }
        if let Some(renaming) = &self.refs.renaming {
            match renaming.coming.try_recv() {
                Ok(result) => self.finish_rename(result),
                Err(TryRecvError::Disconnected) => {
                    self.refs.renaming = None;
                    self.say_in_references(vec!["The rename stopped unexpectedly.".to_string()]);
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(LOOK_AGAIN),
            }
        }
    }

    fn say_in_references(&mut self, said: Vec<String>) {
        if let Some(window) = &mut self.refs.window {
            window.said = said;
        }
    }

    /// Draws whichever of the windows for citing and looking after references are up.
    pub(crate) fn show_reference_windows(&mut self, ctx: &egui::Context) {
        self.show_references_list(ctx);
        self.show_cite_search(ctx);
        self.show_reference_form(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::test_support::*;
    use crate::{test_temp_dir, Headless};
    use kame_core::session::Session;
    use std::path::Path;

    /// Refreshes, and waits for the files to have been read.
    fn settle(refs: &mut ReferencesUi, notebook: &Notebook) {
        refs.refresh(Some(notebook));
        for _ in 0..2000 {
            if refs.reading.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
            refs.refresh(Some(notebook));
        }
        panic!("the files were never read");
    }

    #[test]
    fn the_files_are_read_away_from_the_page_and_the_list_follows() {
        let root = notebook_folder("list-background");
        write_reference(&root, "knuth1981", GOOD);
        let notebook = Notebook::open(&root).unwrap();
        let mut refs = ReferencesUi::default();

        refs.refresh(Some(&notebook));
        assert!(
            refs.reading.is_some(),
            "the files are read on another thread"
        );
        settle(&mut refs, &notebook);
        assert!(refs.list.contains("knuth1981"));

        write_reference(&root, "smith2020", GOOD);
        refs.checked = None;
        refs.refresh(Some(&notebook));
        assert!(refs.reading.is_some());
        settle(&mut refs, &notebook);
        assert!(refs.list.contains("smith2020"));
    }

    #[test]
    fn the_list_follows_the_files_and_the_notebook() {
        let first = notebook_folder("list-first");
        let second = notebook_folder("list-second");
        write_reference(&first, "knuth1981", GOOD);
        let first = Notebook::open(&first).unwrap();
        let second = Notebook::open(&second).unwrap();

        let mut refs = ReferencesUi::default();
        settle(&mut refs, &first);
        assert!(refs.list.contains("knuth1981"));

        write_reference(&first.root, "smith2020", GOOD);
        refs.checked = None;
        settle(&mut refs, &first);
        assert!(refs.list.contains("smith2020"), "a new file is seen");

        std::fs::remove_file(first.root.join("references/smith2020.yaml")).unwrap();
        // Not looked for yet, since it hasn't been long.
        settle(&mut refs, &first);
        assert!(refs.list.contains("smith2020"));
        refs.checked = None;
        settle(&mut refs, &first);
        assert!(!refs.list.contains("smith2020"), "a removed file is seen");

        refs.shown = Some("knuth1981".to_string());
        refs.refresh(Some(&second));
        assert!(refs.list.is_empty(), "another notebook, at once");
        assert_eq!(refs.shown, None);

        settle(&mut refs, &first);
        assert!(refs.list.contains("knuth1981"));
        refs.refresh(None);
        assert!(refs.list.is_empty(), "outside a notebook there are none");
    }

    #[test]
    fn a_damaged_file_is_reported_while_it_lasts_and_the_others_still_load() {
        let root = notebook_folder("list-damaged");
        write_reference(&root, "knuth1981", GOOD);
        write_reference(&root, "broken2020", "title: [not closed\n");
        let notebook = Notebook::open(&root).unwrap();

        let mut refs = ReferencesUi::default();
        settle(&mut refs, &notebook);
        let problem = refs.problem_line().expect("the damage is reported");
        assert!(problem.contains("broken2020"), "{problem}");
        assert!(refs.list.contains("knuth1981"));

        write_reference(&root, "smith2020", GOOD);
        refs.checked = None;
        settle(&mut refs, &notebook);
        assert_eq!(refs.problem_line(), Some(problem), "still so");
        assert!(refs.list.contains("smith2020"));

        std::fs::remove_file(root.join("references/broken2020.yaml")).unwrap();
        refs.checked = None;
        settle(&mut refs, &notebook);
        assert_eq!(refs.problem_line(), None);

        refs.refresh(None);
        assert_eq!(
            refs.problem_line(),
            None,
            "outside a notebook there's nothing to say"
        );
    }

    #[test]
    fn a_document_in_a_notebook_has_its_references() {
        let page = page_in_notebook("hello");
        assert!(page.app.refs.list.contains("knuth1981"));
    }

    #[test]
    fn a_document_outside_a_notebook_has_none() {
        let folder = test_temp_dir().join(format!("outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let session = Session::open(&folder.join("doc.kame")).unwrap();
        let mut page = Headless::new(session);
        page.frame(Vec::new());
        assert!(page.app.nb.notebook.is_none());
        assert!(page.app.refs.list.is_empty());
    }

    #[test]
    fn the_status_says_whats_wrong_with_a_file_for_as_long_as_it_is() {
        let mut page = page_in_notebook("words");
        let root = root_of(&page);
        write_reference(&root, "broken2020", "kind: nonsense\n");
        look_again(&mut page);
        let (said, wrong) = page.app.status_message().expect("something is wrong");
        assert!(said.contains("references/broken2020.yaml"), "{said}");
        assert!(wrong);

        // Another error goes first while it's there, and what's wrong is still said after.
        page.app.session.last_error = Some("could not export".to_string());
        page.frame(Vec::new());
        assert_eq!(
            page.app.status_message(),
            Some(("could not export".to_string(), true))
        );
        page.app.session.last_error = None;
        page.frame(Vec::new());
        assert_eq!(page.app.status_message(), Some((said, true)));

        write_reference(&root, "broken2020", GOOD);
        look_again(&mut page);
        assert_eq!(
            page.app.status_message(),
            None,
            "fixed, it's no longer said"
        );
        assert!(page.app.refs.list.contains("broken2020"));
    }

    #[test]
    fn escape_takes_the_windows_down_one_at_a_time() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);
        page.press("Edit");
        page.frame(Vec::new());
        assert!(page.app.refs.form.is_some());

        press_key(&mut page, egui::Key::Escape);
        assert!(page.app.refs.form.is_none());
        assert!(page.app.refs.window.is_some());

        page.press("Delete…");
        page.frame(Vec::new());
        press_key(&mut page, egui::Key::Escape);
        assert!(page.app.refs.window.as_ref().unwrap().confirming.is_none());
        assert!(page.app.refs.window.is_some());

        press_key(&mut page, egui::Key::Escape);
        assert!(page.app.refs.window.is_none());
        assert!(!page.app.dialog_open());
    }

    /// That the form is still there, the document hasn't changed, and it was said why.
    fn assert_form_kept(page: &Headless, opened: &Option<PathBuf>) {
        assert_eq!(&page.app.session.path().map(Path::to_path_buf), opened);
        let form = &page.app.refs.form.as_ref().expect("still up").form;
        assert_eq!(form.key, "lamport1994");
        assert_eq!(
            page.app.notice.as_deref(),
            Some("finish or close the open dialog first")
        );
    }

    #[test]
    fn opening_a_document_by_any_way_doesnt_take_away_a_form() {
        let (mut page, opened) = page_with_form();
        let other = other_document(&page, "other.kame", "elsewhere");
        page.app.open_picked(other.clone());
        assert_form_kept(&page, &opened);
        page.app.pending_heading = Some("Part".to_string());
        page.app.open_or_confirm(other);
        assert_form_kept(&page, &opened);
        assert_eq!(page.app.pending_heading, None);
    }

    #[test]
    fn the_replaced_file_prompt_doesnt_take_away_a_form() {
        let (mut page, opened) = page_with_form();
        let path = opened.clone().unwrap();
        let other = other_document(&page, "other.kame", "from another machine");
        std::fs::rename(&other, &path).unwrap();
        page.app.session.check_file();
        page.app.replaced_prompt = true;
        page.app.merge_with_replacement();
        assert_form_kept(&page, &opened);
        assert!(page.app.session.replaced);
        page.app.notice = None;
        page.app.open_replacement(&path);
        assert_form_kept(&page, &opened);
        assert!(page.app.session.replaced);
        assert!(page.app.replaced_prompt);
    }

    #[test]
    fn a_new_document_doesnt_take_away_a_form() {
        let (mut page, opened) = page_with_form();
        page.app.request_new();
        assert_form_kept(&page, &opened);
        assert!(!page.app.close_flow.prompting);
    }

    #[test]
    fn going_back_to_another_document_doesnt_take_away_a_form_or_its_stop() {
        let (mut page, opened) = page_with_form();
        let other = other_document(&page, "other.kame", "elsewhere");
        let caret = page.app.session.caret;
        page.app.back.push(crate::BackStop {
            path: Some(other),
            caret,
            scroll: 0.0,
        });
        page.app.go_back();
        assert_form_kept(&page, &opened);
        assert_eq!(page.app.back.stops.len(), 1);
    }

    #[test]
    fn going_back_within_the_document_leaves_a_form_up() {
        let (mut page, opened) = page_with_form();
        let caret = page.app.session.caret;
        page.app.back.push(crate::BackStop {
            path: opened.clone(),
            caret,
            scroll: 0.0,
        });
        page.app.go_back();
        assert!(page.app.refs.form.is_some());
        assert!(page.app.back.stops.is_empty());
    }

    #[test]
    fn the_notebooks_home_doesnt_take_away_a_form_or_leave_a_stop() {
        let (mut page, opened) = page_with_form();
        let index = root_of(&page).join(kame_notebook::INDEX);
        drop(Session::open(&index).unwrap());
        let stops = page.app.back.stops.len();
        page.app.go_home();
        assert_form_kept(&page, &opened);
        assert_eq!(page.app.back.stops.len(), stops);
    }

    #[test]
    fn asking_to_import_or_start_a_notebook_doesnt_take_away_a_form() {
        // Each would put up the system's file browser, which a test isn't allowed to.
        let (mut page, opened) = page_with_form();
        page.app.import_markdown();
        assert_form_kept(&page, &opened);
        page.app.new_notebook();
        assert_form_kept(&page, &opened);
    }

    #[test]
    fn a_document_the_os_asks_for_opens_once_the_form_closes() {
        let (mut page, opened) = page_with_form();
        let other = other_document(&page, "other.kame", "elsewhere");

        page.app.request_open(other.clone());
        page.frame(Vec::new());

        assert_eq!(page.app.session.path().map(Path::to_path_buf), opened);
        assert_eq!(page.app.refs.form.as_ref().unwrap().form.key, "lamport1994");
        assert_eq!(
            page.app.notice.as_deref(),
            Some("other.kame opens once this window is closed")
        );

        page.app.refs.form = None;
        page.frame(Vec::new());
        assert_eq!(page.app.session.path(), Some(other.as_path()));
    }

    #[test]
    fn the_latest_document_the_os_asks_for_is_the_one_that_waits() {
        let (mut page, _) = page_with_form();
        let first = other_document(&page, "first.kame", "one");
        let second = other_document(&page, "second.kame", "two");
        page.app.request_open(first);
        page.app.request_open(second.clone());
        page.app.refs.form = None;
        page.frame(Vec::new());
        assert_eq!(page.app.session.path(), Some(second.as_path()));
    }

    #[test]
    fn a_document_the_os_asks_for_waits_for_a_dialog_and_leaves_it_alone() {
        let mut page = page_in_notebook("");
        let opened = page.app.session.path().map(Path::to_path_buf);
        let other = other_document(&page, "other.kame", "elsewhere");
        page.app.begin_properties();
        page.frame(Vec::new());

        page.app.request_open(other.clone());
        page.frame(Vec::new());

        assert_eq!(page.app.session.path().map(Path::to_path_buf), opened);
        assert!(page.app.metadata_prompt.is_some());
        assert_eq!(
            page.app.notice.as_deref(),
            Some("other.kame opens once this window is closed")
        );

        press_key(&mut page, egui::Key::Escape);
        page.frame(Vec::new());
        assert_eq!(page.app.session.path(), Some(other.as_path()));
        assert!(page.app.metadata_prompt.is_none());
    }

    #[test]
    fn properties_saved_after_the_os_asks_for_a_document_stay_on_the_first() {
        let mut page = page_in_notebook("");
        let other = other_document(&page, "other.kame", "elsewhere");
        {
            let mut session = Session::open(&other).unwrap();
            let mut metadata = session.metadata.clone();
            metadata.author = "Other author".to_string();
            session.set_metadata(metadata);
        }
        page.app.begin_properties();
        page.frame(Vec::new());
        let prompt = page.app.metadata_prompt.as_mut().unwrap();
        prompt.title = "Title of the first".to_string();

        page.app.request_open(other.clone());
        page.frame(Vec::new());
        page.press("Save");
        page.frame(Vec::new());
        page.frame(Vec::new());

        assert_eq!(page.app.session.path(), Some(other.as_path()));
        assert_eq!(page.app.session.metadata.author, "Other author");
        assert_ne!(page.app.session.metadata.title, "Title of the first");
    }

    #[test]
    fn a_document_the_os_asks_for_on_a_window_just_started_opens_at_once() {
        let folder = test_temp_dir().join(format!("start-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let other = folder.join("other.kame");
        drop(Session::open(&other).unwrap());
        let mut page = Headless::new(Session::anonymous().unwrap());

        page.app.request_open(other.clone());

        assert_eq!(page.app.session.path(), Some(other.as_path()));
    }

    #[test]
    fn a_later_request_from_the_os_beats_one_held_on_the_frame_the_form_closes() {
        let (mut page, _) = page_with_form();
        let held = other_document(&page, "held.kame", "one");
        let later = other_document(&page, "later.kame", "two");
        page.app.request_open(held);
        page.app.refs.form = None;

        page.app.request_open(later.clone());
        page.app.open_held();

        assert_eq!(page.app.session.path(), Some(later.as_path()));
    }

    #[test]
    fn a_document_the_os_asks_for_waits_for_the_close_prompt() {
        let mut page = page_in_notebook("");
        let opened = page.app.session.path().map(Path::to_path_buf);
        let other = other_document(&page, "other.kame", "elsewhere");
        page.app.close_flow.prompting = true;

        page.app.request_open(other.clone());
        page.frame(Vec::new());
        assert_eq!(page.app.session.path().map(Path::to_path_buf), opened);

        page.app.close_flow.prompting = false;
        page.frame(Vec::new());
        assert_eq!(page.app.session.path(), Some(other.as_path()));
    }

    #[test]
    fn what_the_person_then_picks_beats_a_document_the_os_asked_for() {
        let mut page = page_in_notebook("");
        let held = other_document(&page, "held.kame", "one");
        let picked = other_document(&page, "picked.kame", "two");
        page.app.begin_properties();
        page.app.request_open(held);

        page.app.open_document(picked.display().to_string());
        page.app.metadata_prompt = None;
        page.frame(Vec::new());
        page.frame(Vec::new());

        assert_eq!(page.app.session.path(), Some(picked.as_path()));
    }

    /// An unsaved document with something in it, which asks before it's left, and a file to open.
    fn page_that_would_lose_work() -> (Headless, PathBuf) {
        let folder = test_temp_dir().join(format!("lose-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let other = folder.join("other.kame");
        drop(Session::open(&other).unwrap());
        let mut session = Session::anonymous().unwrap();
        session.insert("words");
        let mut page = Headless::new(session);
        page.frame(Vec::new());
        (page, other)
    }

    #[test]
    fn a_document_the_os_asks_for_takes_over_the_prompt_about_losing_work() {
        let (mut page, other) = page_that_would_lose_work();
        page.app.open_or_confirm(PathBuf::from("first.kame"));
        assert!(page.app.open_prompt.is_some());

        page.app.request_open(other.clone());

        let prompt = page.app.open_prompt.as_ref().unwrap();
        assert_eq!(prompt.path, other.display().to_string());
        assert!(prompt.confirming);
        assert_eq!(page.app.held_open, None);
    }

    #[test]
    fn the_promise_to_open_goes_once_the_held_document_comes_up_for_opening() {
        let (mut page, other) = page_that_would_lose_work();
        page.app.begin_properties();
        page.app.request_open(other);
        assert!(page.app.notice.is_some());

        page.app.metadata_prompt = None;
        page.frame(Vec::new());

        assert!(page.app.open_prompt.is_some(), "it asks about the words");
        assert_eq!(page.app.notice, None);
    }

    #[test]
    fn a_held_document_that_has_gone_isnt_made_again() {
        let mut page = page_in_notebook("");
        let opened = page.app.session.path().map(Path::to_path_buf);
        let other = other_document(&page, "other.kame", "elsewhere");
        page.app.begin_properties();
        page.app.request_open(other.clone());
        std::fs::remove_file(&other).unwrap();

        page.app.metadata_prompt = None;
        page.frame(Vec::new());

        assert_eq!(page.app.session.path().map(Path::to_path_buf), opened);
        assert!(!other.exists());
        assert_eq!(
            page.app.notice.as_deref(),
            Some("other.kame is no longer there")
        );
    }

    #[test]
    fn of_several_documents_the_os_asks_for_at_once_the_first_opens() {
        let mut page = page_in_notebook("");
        let first = other_document(&page, "first.kame", "one");
        let second = other_document(&page, "second.kame", "two");
        let ctx = page.ctx.clone();

        page.app.handle_os_requests(
            &ctx,
            crate::external::Requests {
                open: vec![first.clone(), second],
                quit: false,
            },
        );

        assert_eq!(page.app.session.path(), Some(first.as_path()));
    }

    #[test]
    fn the_os_asking_to_quit_closes_the_window_as_its_own_button_does() {
        let mut page = page_in_notebook("");
        let ctx = page.ctx.clone();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            page.app.handle_os_requests(
                ui.ctx(),
                crate::external::Requests {
                    open: Vec::new(),
                    quit: true,
                },
            );
        });
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(commands.contains(&egui::ViewportCommand::Close));
    }

    #[test]
    fn a_document_the_os_asks_for_opens_at_once_with_no_form() {
        let mut page = page_in_notebook("");
        let other = other_document(&page, "other.kame", "elsewhere");
        page.app.request_open(other.clone());
        assert_eq!(page.app.session.path(), Some(other.as_path()));
    }

    #[test]
    fn opening_a_path_doesnt_take_away_a_search_being_typed_in() {
        let mut page = page_with_browser("");
        cite_chord(&mut page);
        type_into(&mut page, "knu");

        page.app.begin_open();

        assert!(page.app.open_prompt.is_none());
        assert_eq!(page.app.refs.cite_search.as_ref().unwrap().query, "knu");
        assert_eq!(
            page.app.notice.as_deref(),
            Some("finish or close the open dialog first")
        );
    }
}
