//! The system's file browser, asked for without stopping the frame.
//!
//! A browser that blocks runs a loop of its own inside winit's event handler, and on macOS an
//! event winit queued for after the frame (a drag over the panel brings one) then reaches that
//! handler while it's still in use, which aborts kame. So the browser is asked for here, its answer
//! is a future taken once a frame, and what the command was to do next waits in [`Then`] until it
//! comes. While it's up, nothing that happens in the window reaches kame ([`hold`]), so the answer
//! meets the state it was asked from. Nothing but [`native`] touches `rfd`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use eframe::egui;

/// What the status line says while the browser is up.
pub(crate) const OPEN_LINE: &str = "the file browser is open";

/// Whether the browser picks a file, picks a folder or names a file to save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    File,
    Folder,
    Save,
}

/// A kind of file the browser offers, by its extensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Filter {
    pub(crate) name: &'static str,
    pub(crate) extensions: &'static [&'static str],
}

const MARKDOWN: Filter = Filter {
    name: "Markdown",
    extensions: &["md", "markdown"],
};
const KAME: Filter = Filter {
    name: "kame document",
    extensions: &["kame"],
};
const KAME_OR_MARKDOWN: Filter = Filter {
    name: "kame or Markdown",
    extensions: &["kame", "md", "markdown"],
};
const MARKDOWN_EXPORT: Filter = Filter {
    name: "Markdown",
    extensions: &["md"],
};
const HTML_EXPORT: Filter = Filter {
    name: "HTML",
    extensions: &["html"],
};
const IMAGE: Filter = Filter {
    name: "Image",
    extensions: &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"],
};

/// One request for the browser: what it's titled, where it starts, what it offers and, for a
/// save, the name it suggests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ask {
    pub(crate) title: &'static str,
    pub(crate) directory: Option<PathBuf>,
    pub(crate) filters: Vec<Filter>,
    pub(crate) file_name: Option<String>,
    pub(crate) pick: Pick,
}

impl Ask {
    fn new(title: &'static str, directory: Option<&Path>, pick: Pick) -> Ask {
        Ask {
            title,
            directory: directory.map(Path::to_path_buf),
            filters: Vec::new(),
            file_name: None,
            pick,
        }
    }

    fn offering(mut self, filters: &[Filter]) -> Ask {
        self.filters.extend_from_slice(filters);
        self
    }

    /// A Markdown file to import.
    pub(crate) fn markdown(directory: Option<&Path>) -> Ask {
        Ask::new("Import Markdown", directory, Pick::File).offering(&[MARKDOWN])
    }

    /// A document to open.
    pub(crate) fn open(directory: Option<&Path>) -> Ask {
        Ask::new("Open", directory, Pick::File).offering(&[KAME_OR_MARKDOWN, KAME, MARKDOWN])
    }

    /// The folder of a new notebook.
    pub(crate) fn notebook_folder(directory: Option<&Path>) -> Ask {
        Ask::new("New Notebook", directory, Pick::Folder)
    }

    /// A document to link to.
    pub(crate) fn document_link(directory: Option<&Path>) -> Ask {
        Ask::new("Link to Document", directory, Pick::File).offering(&[
            KAME_OR_MARKDOWN,
            KAME,
            MARKDOWN,
        ])
    }

    /// An image to insert.
    pub(crate) fn image(directory: Option<&Path>) -> Ask {
        Ask::new("Insert Image", directory, Pick::File).offering(&[IMAGE])
    }

    /// Where to write a Markdown export, starting at `suggestion`.
    pub(crate) fn export_markdown(suggestion: &Path) -> Ask {
        Ask::export("Export Markdown", MARKDOWN_EXPORT, suggestion)
    }

    /// Where to write an HTML export, starting at `suggestion`.
    pub(crate) fn export_html(suggestion: &Path) -> Ask {
        Ask::export("Export HTML", HTML_EXPORT, suggestion)
    }

    /// Where to write an export, starting at `suggestion`: in its folder, under its name.
    fn export(title: &'static str, filter: Filter, suggestion: &Path) -> Ask {
        let directory = suggestion
            .parent()
            .filter(|directory| !directory.as_os_str().is_empty());
        let mut ask = Ask::new(title, directory, Pick::Save).offering(&[filter]);
        ask.file_name = suggestion
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string);
        ask
    }

    /// Where to save a document, suggesting `suggested_name`.
    pub(crate) fn save(suggested_name: &str, directory: Option<&Path>) -> Ask {
        let mut ask = Ask::new("Save kame document", directory, Pick::Save).offering(&[KAME]);
        ask.file_name = Some(suggested_name.to_string());
        ask
    }
}

/// What the browser's answer is for: the rest of the command that asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Then {
    ExportMarkdown,
    ExportHtml,
    ImportMarkdown,
    Open,
    InsertImage,
    LinkDocument,
    /// Saves the document there, and closes the window or starts a new document after, if that
    /// is what asked.
    Save {
        close_after: bool,
    },
    NewNotebook,
}

/// The path the browser gives back, or `None` if it's cancelled, once it has one.
pub(crate) type Answer = Pin<Box<dyn Future<Output = Option<PathBuf>>>>;

/// What puts the browser up for an [`Ask`].
pub(crate) type Browser = Box<dyn FnMut(&Ask) -> Answer>;

/// The system's own browser: a sheet on the window on macOS, the desktop portal's chooser on
/// Linux. Both return at once, and the answer comes later.
pub(crate) fn native(ask: &Ask) -> Answer {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(ask.title);
    if let Some(directory) = &ask.directory {
        dialog = dialog.set_directory(directory);
    }
    for filter in &ask.filters {
        dialog = dialog.add_filter(filter.name, filter.extensions);
    }
    if let Some(name) = &ask.file_name {
        dialog = dialog.set_file_name(name);
    }
    // Called here and not in the future, so that the browser goes up now.
    let picked: Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>> = match ask.pick {
        Pick::File => Box::pin(dialog.pick_file()),
        Pick::Folder => Box::pin(dialog.pick_folder()),
        Pick::Save => Box::pin(dialog.save_file()),
    };
    Box::pin(async move { picked.await.map(|file| file.path().to_path_buf()) })
}

/// The browser kame asks, and the answer it's waiting for, if it's up.
pub(crate) struct FileBrowser {
    browser: Browser,
    waiting: Option<(Then, Answer)>,
    /// What an answer wakes, for the frame that takes it.
    ctx: egui::Context,
}

/// Wakes a frame for an answer that has come.
struct Repaint(egui::Context);

impl Wake for Repaint {
    fn wake(self: Arc<Self>) {
        self.0.request_repaint();
    }
}

impl FileBrowser {
    /// The system's browser, or in a test one that fails it, as a test has no one to answer.
    pub(crate) fn new(ctx: egui::Context) -> FileBrowser {
        let browser: Browser = if cfg!(test) {
            Box::new(|ask: &Ask| panic!("a test asked for the file browser: {}", ask.title))
        } else {
            Box::new(native)
        };
        FileBrowser {
            browser,
            waiting: None,
            ctx,
        }
    }

    /// Asks with `browser` from now on.
    #[cfg(test)]
    pub(crate) fn set(&mut self, browser: impl FnMut(&Ask) -> Answer + 'static) {
        self.browser = Box::new(browser);
    }

    pub(crate) fn is_open(&self) -> bool {
        self.waiting.is_some()
    }

    /// Puts the browser up for `ask`, with `then` waiting for its answer. One already up is kept,
    /// and this is refused, returning false.
    pub(crate) fn ask(&mut self, ask: &Ask, then: Then) -> bool {
        if self.is_open() {
            return false;
        }
        let answer = (self.browser)(ask);
        self.waiting = Some((then, answer));
        // The next frame looks for the answer, and from then on the answer wakes one.
        self.ctx.request_repaint();
        true
    }

    /// What the answer is for and the answer, once it has come, which ends the wait. `None` while
    /// the browser is still up, or if it isn't.
    pub(crate) fn take_answer(&mut self) -> Option<(Then, Option<PathBuf>)> {
        let (_, answer) = self.waiting.as_mut()?;
        let waker = Waker::from(Arc::new(Repaint(self.ctx.clone())));
        match answer.as_mut().poll(&mut Context::from_waker(&waker)) {
            Poll::Pending => None,
            Poll::Ready(path) => self.waiting.take().map(|(then, _)| (then, path)),
        }
    }
}

/// Takes out of a frame's input everything that would act on kame while the browser is up: keys
/// pressed, text, clicks, the pointer's moves, scrolling, pastes and files dropped. Releases, the
/// modifiers and the window's focus are let through, so that egui doesn't take a key or a button
/// as still held once the browser goes.
pub(crate) fn hold(input: &mut egui::RawInput) {
    input.events.retain(|event| {
        matches!(
            event,
            egui::Event::Key { pressed: false, .. }
                | egui::Event::PointerButton { pressed: false, .. }
                | egui::Event::ModifiersChanged(_)
                | egui::Event::WindowFocused(_)
                | egui::Event::PointerGone
        )
    });
    input.dropped_files.clear();
    input.hovered_files.clear();
}

/// A browser for tests: it keeps what it was asked, and answers when the test says.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct StandIn(std::rc::Rc<std::cell::RefCell<Vec<(Ask, Reply)>>>);

/// Where a stand-in's answer is put, and what it wakes.
#[cfg(test)]
#[derive(Default)]
struct Slot {
    answer: Option<Option<PathBuf>>,
    waker: Option<Waker>,
}

#[cfg(test)]
type Reply = std::rc::Rc<std::cell::RefCell<Slot>>;

#[cfg(test)]
impl StandIn {
    /// Puts this in place of the browser `browser` asks.
    pub(crate) fn install(&self, browser: &mut FileBrowser) {
        let asked = self.0.clone();
        browser.set(move |ask: &Ask| {
            let reply = Reply::default();
            asked.borrow_mut().push((ask.clone(), reply.clone()));
            Box::pin(std::future::poll_fn(move |cx| {
                let mut slot = reply.borrow_mut();
                match slot.answer.take() {
                    Some(answer) => Poll::Ready(answer),
                    None => {
                        slot.waker = Some(cx.waker().clone());
                        Poll::Pending
                    }
                }
            }))
        });
    }

    /// What it has been asked, in order.
    pub(crate) fn asked(&self) -> Vec<Ask> {
        self.0.borrow().iter().map(|(ask, _)| ask.clone()).collect()
    }

    /// Answers the last ask with `path`, or cancels it with `None`.
    pub(crate) fn answer(&self, path: Option<&Path>) {
        let asked = self.0.borrow();
        let (_, reply) = asked.last().expect("the browser was asked");
        let mut slot = reply.borrow_mut();
        slot.answer = Some(path.map(Path::to_path_buf));
        if let Some(waker) = slot.waker.take() {
            waker.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn browser_with_stand_in() -> (FileBrowser, StandIn) {
        let mut browser = FileBrowser::new(egui::Context::default());
        let stand_in = StandIn::default();
        stand_in.install(&mut browser);
        (browser, stand_in)
    }

    #[test]
    fn an_answer_that_hasnt_come_leaves_it_waiting() {
        let (mut browser, stand_in) = browser_with_stand_in();
        assert!(browser.ask(&Ask::open(None), Then::Open));
        for _ in 0..3 {
            assert_eq!(browser.take_answer(), None);
            assert!(browser.is_open());
        }
        stand_in.answer(Some(Path::new("/a.kame")));
        assert_eq!(
            browser.take_answer(),
            Some((Then::Open, Some(PathBuf::from("/a.kame"))))
        );
        assert!(!browser.is_open());
        assert_eq!(browser.take_answer(), None);
    }

    #[test]
    fn a_cancelled_browser_is_answered_with_nothing() {
        let (mut browser, stand_in) = browser_with_stand_in();
        browser.ask(&Ask::markdown(None), Then::ImportMarkdown);
        stand_in.answer(None);
        assert_eq!(browser.take_answer(), Some((Then::ImportMarkdown, None)));
        assert!(!browser.is_open());
    }

    #[test]
    fn a_second_ask_while_one_waits_is_refused_and_the_first_kept() {
        let (mut browser, stand_in) = browser_with_stand_in();
        assert!(browser.ask(&Ask::open(None), Then::Open));
        assert!(!browser.ask(
            &Ask::save("Untitled.kame", None),
            Then::Save { close_after: false }
        ));
        assert_eq!(stand_in.asked(), vec![Ask::open(None)]);
        stand_in.answer(Some(Path::new("/a.kame")));
        assert_eq!(
            browser.take_answer(),
            Some((Then::Open, Some(PathBuf::from("/a.kame"))))
        );
    }

    #[test]
    fn an_answer_wakes_a_frame() {
        let ctx = egui::Context::default();
        let woken = Arc::new(AtomicBool::new(false));
        let seen = woken.clone();
        ctx.set_repaint_observer(move |_| seen.store(true, Ordering::SeqCst));
        let mut browser = FileBrowser::new(ctx);
        let stand_in = StandIn::default();
        stand_in.install(&mut browser);
        browser.ask(&Ask::open(None), Then::Open);
        // The frame that asking woke looks for the answer, and finds none yet.
        assert_eq!(browser.take_answer(), None);
        woken.store(false, Ordering::SeqCst);

        stand_in.answer(Some(Path::new("/a.kame")));

        assert!(woken.load(Ordering::SeqCst));
    }

    #[test]
    fn asking_wakes_the_frame_that_looks_for_the_answer() {
        let ctx = egui::Context::default();
        let woken = Arc::new(AtomicBool::new(false));
        let seen = woken.clone();
        ctx.set_repaint_observer(move |_| seen.store(true, Ordering::SeqCst));
        let mut browser = FileBrowser::new(ctx);
        StandIn::default().install(&mut browser);

        browser.ask(&Ask::open(None), Then::Open);

        assert!(woken.load(Ordering::SeqCst));
    }

    #[test]
    #[should_panic(expected = "a test asked for the file browser: Open")]
    fn a_test_that_doesnt_stand_in_for_the_browser_fails_if_it_asks() {
        FileBrowser::new(egui::Context::default()).ask(&Ask::open(None), Then::Open);
    }

    #[test]
    fn the_asks_say_what_the_dialogs_did() {
        let folder = Path::new("/notes");
        assert_eq!(
            Ask::save("Essay.kame", Some(folder)),
            Ask {
                title: "Save kame document",
                directory: Some(folder.to_path_buf()),
                filters: vec![KAME],
                file_name: Some("Essay.kame".to_string()),
                pick: Pick::Save,
            }
        );
        assert_eq!(
            Ask::export_html(&folder.join("Essay.html")),
            Ask {
                title: "Export HTML",
                directory: Some(folder.to_path_buf()),
                filters: vec![Filter {
                    name: "HTML",
                    extensions: &["html"],
                }],
                file_name: Some("Essay.html".to_string()),
                pick: Pick::Save,
            }
        );
        // A suggestion with no folder leaves where to start to the system.
        assert_eq!(
            Ask::export_markdown(Path::new("Essay.md")),
            Ask {
                title: "Export Markdown",
                directory: None,
                filters: vec![Filter {
                    name: "Markdown",
                    extensions: &["md"],
                }],
                file_name: Some("Essay.md".to_string()),
                pick: Pick::Save,
            }
        );
        assert_eq!(Ask::open(Some(folder)).title, "Open");
        assert_eq!(
            Ask::open(None).filters,
            vec![KAME_OR_MARKDOWN, KAME, MARKDOWN]
        );
        assert_eq!(Ask::notebook_folder(None).pick, Pick::Folder);
        assert_eq!(Ask::image(None).filters, vec![IMAGE]);
        assert_eq!(Ask::markdown(None).filters, vec![MARKDOWN]);
        assert_eq!(Ask::document_link(None).title, "Link to Document");
    }

    /// A file dropped on the window.
    #[derive(Debug)]
    struct Dropped;

    impl egui::DroppedFile for Dropped {
        fn path(&self) -> &Path {
            Path::new("/dropped.kame")
        }

        fn bytes(&self) -> Result<Vec<u8>, String> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn while_its_up_only_releases_the_modifiers_and_focus_reach_kame() {
        let modifiers = egui::Modifiers::COMMAND;
        let key = |pressed| egui::Event::Key {
            key: egui::Key::O,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        let button = |pressed| egui::Event::PointerButton {
            pos: egui::Pos2::ZERO,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        };
        let mut input = egui::RawInput {
            events: vec![
                key(true),
                key(false),
                egui::Event::Text("x".to_string()),
                egui::Event::Paste("y".to_string()),
                button(true),
                button(false),
                egui::Event::PointerMoved(egui::Pos2::ZERO),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::Vec2::Y,
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    modifiers,
                },
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::WindowFocused(false),
            ],
            dropped_files: vec![Arc::new(Dropped)],
            hovered_files: vec![egui::HoveredFile::default()],
            ..Default::default()
        };

        hold(&mut input);

        assert_eq!(
            input.events,
            vec![
                key(false),
                button(false),
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::WindowFocused(false),
            ]
        );
        assert!(input.dropped_files.is_empty());
        assert!(input.hovered_files.is_empty());
    }

    // --- in the app ----------------------------------------------------------

    use crate::{Headless, MenuAction};
    use kame_core::model::{BlockKind, SpanStyle};
    use kame_core::session::Session;

    /// A fresh folder, as the browser would give it: by its own name, links and all resolved.
    fn folder(name: &str) -> PathBuf {
        kame_test_dir::folder(name).canonicalize().unwrap()
    }

    /// A page with `session`, whose file browser is a stand-in.
    fn page_with(session: Session) -> (Headless, StandIn) {
        let mut page = Headless::new(session);
        let stand_in = StandIn::default();
        stand_in.install(&mut page.app.browser);
        page.frame(Vec::new());
        (page, stand_in)
    }

    /// A page on `doc.kame` in a folder of its own, holding `text` with the caret after it.
    fn page_on_file(text: &str) -> (Headless, StandIn, PathBuf) {
        let path = folder("browse").join("doc.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert(text);
        let (page, stand_in) = page_with(session);
        (page, stand_in, path)
    }

    /// A page on a document with no file, holding `text`.
    fn page_unsaved(text: &str) -> (Headless, StandIn) {
        let mut session = Session::anonymous().unwrap();
        session.insert(text);
        page_with(session)
    }

    fn text_of(page: &Headless) -> Vec<String> {
        let blocks = &page.app.session.document().blocks;
        blocks.iter().map(|block| block.text.clone()).collect()
    }

    fn command(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }
    }

    fn chord(page: &mut Headless, key: egui::Key) {
        for pressed in [true, false] {
            page.frame(vec![command(key, pressed)]);
        }
    }

    /// What the close prompt's Save button does.
    fn save_before_closing(page: &mut Headless) {
        page.app.save_action(true);
    }

    fn menu(page: &mut Headless, action: MenuAction) {
        let ctx = page.ctx.clone();
        page.app.run_menu_action(&ctx, action);
    }

    #[test]
    fn file_open_asks_in_the_documents_folder_and_the_frame_goes_on() {
        let (mut page, stand_in, path) = page_on_file("words");

        chord(&mut page, egui::Key::O);

        assert_eq!(stand_in.asked(), vec![Ask::open(path.parent())]);
        assert_eq!(stand_in.asked()[0].title, "Open");
        // The frames go on, and nothing opens until the browser answers.
        for _ in 0..5 {
            page.frame(Vec::new());
            assert!(page.app.browser.is_open());
            assert_eq!(page.app.session.path(), Some(path.as_path()));
        }
    }

    #[test]
    fn while_the_browser_is_up_typing_clicks_and_chords_reach_nothing() {
        let (mut page, stand_in, _) = page_on_file("words");
        let caret = page.app.session.caret;
        let near_the_start = egui::Pos2::new(page.body_left() + 2.0, crate::PAGE_PADDING + 8.0);
        menu(&mut page, MenuAction::Open);

        page.frame(vec![egui::Event::Text("x".to_string())]);
        page.frame(vec![egui::Event::Paste("pasted".to_string())]);
        chord(&mut page, egui::Key::J);
        // The page takes a click under a dialog as far as the caret, and this one not even that.
        page.click(near_the_start);

        assert_eq!(text_of(&page), vec!["words"]);
        assert!(!page.app.nb.list_open(), "the jump list didn't open");
        assert_eq!(page.app.session.caret, caret);
        assert!(page.app.browser.is_open());

        stand_in.answer(None);
        page.frame(Vec::new());
        page.frame(vec![egui::Event::Text("x".to_string())]);
        assert_eq!(text_of(&page), vec!["wordsx"], "typing works again");
        page.click(near_the_start);
        assert_eq!(page.app.session.caret.offset, 0, "and clicking");
    }

    #[test]
    fn escape_while_the_browser_is_up_closes_nothing() {
        let (mut page, _stand_in, _) = page_on_file("words");
        page.app.toggle_headings();
        page.frame(Vec::new());
        page.app.link_document_with_dialog();

        for pressed in [true, false] {
            page.frame(vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
        assert!(page.app.dismiss_dialog(), "something is up");

        assert!(page.app.nb.headings.is_some());
        assert!(page.app.browser.is_open());
    }

    #[test]
    fn an_answer_to_open_opens_the_document() {
        let (mut page, stand_in, path) = page_on_file("words");
        let other = path.with_file_name("other.kame");
        let mut session = Session::open(&other).unwrap();
        session.insert("elsewhere");
        drop(session);
        menu(&mut page, MenuAction::Open);

        stand_in.answer(Some(&other));
        page.frame(Vec::new());

        assert!(!page.app.browser.is_open());
        assert_eq!(page.app.session.path(), Some(other.as_path()));
        assert_eq!(text_of(&page), vec!["elsewhere"]);
    }

    #[test]
    fn an_answer_to_import_opens_the_markdown() {
        let (mut page, stand_in, path) = page_on_file("words");
        let markdown = path.with_file_name("notes.md");
        std::fs::write(&markdown, "Imported words.\n").unwrap();
        menu(&mut page, MenuAction::ImportMarkdown);
        assert_eq!(stand_in.asked(), vec![Ask::markdown(path.parent())]);

        stand_in.answer(Some(&markdown));
        page.frame(Vec::new());

        assert!(page.app.session.is_anonymous());
        assert_eq!(text_of(&page), vec!["Imported words."]);
    }

    #[test]
    fn an_answer_to_save_writes_the_unnamed_document_there() {
        let (mut page, stand_in) = page_unsaved("words");
        menu(&mut page, MenuAction::Save);
        assert_eq!(stand_in.asked(), vec![Ask::save("Untitled.kame", None)]);
        assert_eq!(
            stand_in.asked()[0].file_name.as_deref(),
            Some("Untitled.kame")
        );
        let path = folder("browse-save").join("essay.kame");

        stand_in.answer(Some(&path));
        page.frame(Vec::new());

        assert_eq!(page.app.session.path(), Some(path.as_path()));
        assert_eq!(
            text_of(&Headless::new(Session::open(&path).unwrap())),
            vec!["words"]
        );
        assert!(!page.app.close_flow.confirmed, "and nothing closes");
    }

    #[test]
    fn a_save_the_close_prompt_asked_for_goes_on_to_close() {
        let (mut page, stand_in) = page_unsaved("words");
        assert!(page.close_request(), "the close waits for the prompt");
        assert!(page.app.close_flow.prompting);
        save_before_closing(&mut page);
        let path = folder("browse-save-close").join("essay.kame");

        stand_in.answer(Some(&path));
        page.frame(Vec::new());

        assert_eq!(page.app.session.path(), Some(path.as_path()));
        assert!(page.app.close_flow.confirmed);
        assert!(page.closing, "the window was asked to close");
    }

    #[test]
    fn a_cancelled_save_changes_nothing_and_leaves_the_close_prompt() {
        let (mut page, stand_in) = page_unsaved("words");
        page.close_request();
        save_before_closing(&mut page);
        let before = page.app.session.document().clone();

        stand_in.answer(None);
        page.frame(Vec::new());

        assert!(!page.app.browser.is_open());
        assert!(page.app.session.is_anonymous());
        assert_eq!(page.app.session.document(), &before);
        assert!(page.app.close_flow.prompting);
        assert!(!page.app.close_flow.confirmed);
        assert!(!page.closing);
    }

    #[test]
    fn an_answer_to_either_export_writes_its_file() {
        for (action, extension) in [
            (crate::FileCommand::ExportMarkdown, "md"),
            (crate::FileCommand::ExportHtml, "html"),
        ] {
            let (mut page, stand_in, path) = page_on_file("words");
            let suggestion = page.app.session.export_suggestion(extension);
            menu(&mut page, MenuAction::File(action));
            let expected = match action {
                crate::FileCommand::ExportMarkdown => Ask::export_markdown(&suggestion),
                _ => Ask::export_html(&suggestion),
            };
            assert_eq!(stand_in.asked(), vec![expected]);
            let name = format!("doc.{extension}");
            assert_eq!(
                stand_in.asked()[0].file_name.as_deref(),
                Some(name.as_str())
            );
            let out = path.with_file_name(format!("out.{extension}"));

            stand_in.answer(Some(&out));
            page.frame(Vec::new());

            assert!(std::fs::read_to_string(&out).unwrap().contains("words"));
        }
    }

    #[test]
    fn an_answer_to_insert_image_inserts_it() {
        let (mut page, stand_in, path) = page_on_file("words");
        let picture = path.with_file_name("picture.png");
        std::fs::write(&picture, b"not really a picture").unwrap();
        page.app.insert_image_with_dialog();
        assert_eq!(stand_in.asked(), vec![Ask::image(path.parent())]);

        stand_in.answer(Some(&picture));
        page.frame(Vec::new());

        assert!(page
            .app
            .session
            .document()
            .blocks
            .iter()
            .any(|block| matches!(
                &block.kind,
                BlockKind::Image { path, .. } if path.ends_with("picture.png")
            )));
    }

    #[test]
    fn an_answer_to_link_to_document_makes_the_link() {
        let (mut page, stand_in, path) = page_on_file("word");
        let other = path.with_file_name("other.kame");
        drop(Session::open(&other).unwrap());
        menu(&mut page, MenuAction::LinkDocument);
        assert_eq!(stand_in.asked(), vec![Ask::document_link(path.parent())]);

        stand_in.answer(Some(&other));
        page.frame(Vec::new());

        let spans = &page.app.session.document().blocks[0].spans;
        assert!(
            spans.iter().any(|span| matches!(
                &span.style,
                SpanStyle::DocumentLink { target } if target.ends_with("other.kame")
            )),
            "{spans:?}"
        );
    }

    #[test]
    fn an_answer_to_new_notebook_makes_it_and_opens_its_contents_page() {
        let (mut page, stand_in, path) = page_on_file("words");
        menu(&mut page, MenuAction::NewNotebook);
        assert_eq!(stand_in.asked(), vec![Ask::notebook_folder(path.parent())]);
        let root = folder("browse-notebook");

        stand_in.answer(Some(&root));
        page.frame(Vec::new());

        assert!(root.join("notebook.yaml").is_file());
        let index = root.join(kame_notebook::INDEX);
        assert_eq!(page.app.session.path(), Some(index.as_path()));
    }

    #[test]
    fn a_close_request_while_the_browser_is_up_doesnt_close() {
        let (mut page, stand_in, _) = page_on_file("words");
        assert!(!page.close_request(), "a document on file closes at once");
        menu(&mut page, MenuAction::Open);

        assert!(page.close_request());

        stand_in.answer(None);
        page.frame(Vec::new());
        assert!(!page.close_request());
    }

    #[test]
    fn a_quit_the_os_asks_for_waits_for_the_browser() {
        let (mut page, stand_in, _) = page_on_file("words");
        menu(&mut page, MenuAction::Open);

        page.requests.quit = true;
        page.frame(Vec::new());
        page.frame(Vec::new());
        assert!(!page.closing);

        stand_in.answer(None);
        page.frame(Vec::new());
        assert!(page.closing);
    }

    #[test]
    fn a_document_the_os_asks_for_waits_for_the_browser() {
        let (mut page, stand_in, path) = page_on_file("words");
        let other = path.with_file_name("other.kame");
        drop(Session::open(&other).unwrap());
        menu(&mut page, MenuAction::Open);

        page.requests.open = vec![other.clone()];
        page.frame(Vec::new());
        assert_eq!(page.app.session.path(), Some(path.as_path()));

        stand_in.answer(None);
        page.frame(Vec::new());
        assert_eq!(page.app.session.path(), Some(other.as_path()));
    }

    #[test]
    fn a_second_ask_while_the_browser_is_up_is_refused() {
        let (mut page, stand_in) = page_unsaved("words");
        menu(&mut page, MenuAction::ImportMarkdown);
        menu(&mut page, MenuAction::Save);
        assert_eq!(stand_in.asked(), vec![Ask::markdown(None)]);
        assert!(page.app.browser.is_open());
    }

    #[test]
    fn the_status_line_says_the_browser_is_open_while_it_is() {
        let (mut page, stand_in, _) = page_on_file("words");
        page.chrome = true;
        menu(&mut page, MenuAction::Open);
        page.frame(Vec::new());
        assert!(page.sees(OPEN_LINE));
        assert_eq!(page.app.notice, None, "it isn't a notice");

        stand_in.answer(None);
        page.frame(Vec::new());
        page.frame(Vec::new());
        assert!(!page.sees(OPEN_LINE));
    }
}
