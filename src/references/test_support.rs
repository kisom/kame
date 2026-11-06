//! What the tests of the references windows share: blocks, notebooks on disk, and pages with
//! keys pressed and windows opened.

use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::Duration;

use eframe::egui;
use egui::Pos2;
use kame_core::model::{Block, BlockKind};
use kame_core::references::{Reference, References};
use kame_core::session::Session;
use kame_notebook::references::Renamed;

use super::*;
use crate::{test_temp_dir, Headless};

pub(super) fn block(kind: BlockKind, text: &str) -> Block {
    Block {
        id: uuid::Uuid::new_v4(),
        kind,
        text: text.to_string(),
        spans: Vec::new(),
    }
}

pub(super) fn paragraph(text: &str) -> Block {
    block(BlockKind::Paragraph, text)
}

pub(super) fn known(keys: &[&str]) -> References {
    let mut references = References::new();
    for key in keys {
        references.insert(
            key.to_string(),
            Reference {
                title: "Breaking paragraphs into lines".to_string(),
                ..Reference::default()
            },
        );
    }
    references
}

/// A notebook folder with an empty `references/` in it.
pub(super) fn notebook_folder(name: &str) -> std::path::PathBuf {
    let root = test_temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("references")).unwrap();
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    root
}

pub(super) fn write_reference(root: &std::path::Path, key: &str, contents: &str) {
    std::fs::write(
        root.join("references").join(format!("{key}.yaml")),
        contents,
    )
    .unwrap();
}

pub(super) const GOOD: &str = "kind: article\ntitle: Breaking paragraphs into lines\n";

/// A page in a notebook holding `knuth1981`, with `text` as its only block.
pub(super) fn page_in_notebook(text: &str) -> Headless {
    let root = notebook_folder("page");
    write_reference(&root, "knuth1981", GOOD);
    let mut session = Session::open(&root.join("doc.kame")).unwrap();
    session.insert(text);
    let mut page = Headless::new(session);
    page.read_references();
    page
}

/// Somewhere on the first line of the text, a little way in.
pub(super) fn near_the_start(page: &Headless) -> Pos2 {
    Pos2::new(page.body_left() + 12.0, crate::PAGE_PADDING + 8.0)
}

/// Types `text` a character at a time, as the keyboard does.
pub(super) fn type_into(page: &mut Headless, text: &str) {
    for character in text.chars() {
        page.frame(vec![egui::Event::Text(character.to_string())]);
    }
}

/// `[lamport1994]` typed so that its note is up, and the form for it open with a title.
pub(super) fn noted_key_with_its_form() -> Headless {
    let mut page = page_in_notebook("");
    type_into(&mut page, "[lamport1994]");
    assert!(page.app.notice.is_some());
    cite_chord(&mut page);
    let window = page.app.refs.form.as_mut().expect("the form opens");
    window.form.reference.title = "How to write".to_string();
    page
}

/// A key pressed and let go with `modifiers`, as egui takes a key still down for a repeat.
pub(super) fn key(page: &mut Headless, key: egui::Key, modifiers: egui::Modifiers) {
    page.frame(
        [true, false]
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            })
            .to_vec(),
    );
}

pub(super) fn press_key(page: &mut Headless, pressed: egui::Key) {
    key(page, pressed, egui::Modifiers::NONE);
}

/// Ctrl-Shift-C, and the frame after it for a window to be drawn and take the keyboard.
pub(super) fn cite_chord(page: &mut Headless) {
    let modifiers = egui::Modifiers {
        command: true,
        shift: true,
        ..egui::Modifiers::NONE
    };
    key(page, egui::Key::C, modifiers);
    page.frame(Vec::new());
}

/// Runs frames until `done`, for what a background thread is doing.
pub(super) fn run_until(page: &mut Headless, done: impl Fn(&KameApp) -> bool) {
    for _ in 0..2000 {
        page.frame(Vec::new());
        if done(&page.app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("it never finished");
}

/// Opens the References… window and waits for its counts.
pub(super) fn open_list(page: &mut Headless) {
    page.app.open_references();
    run_until(page, |app| app.refs.counts.is_some());
    page.frame(Vec::new());
}

pub(super) fn only_text(page: &Headless) -> &str {
    &page.app.session.document().blocks[0].text
}

pub(super) fn root_of(page: &Headless) -> PathBuf {
    page.app.nb.notebook.as_ref().unwrap().root.clone()
}

pub(super) fn reference_file(page: &Headless, key: &str) -> PathBuf {
    root_of(page).join("references").join(format!("{key}.yaml"))
}

/// A document beside the open one, saved with `text` in it.
pub(super) fn other_document(page: &Headless, name: &str, text: &str) -> PathBuf {
    let path = root_of(page).join(name);
    let mut session = Session::open(&path).unwrap();
    session.insert(text);
    path
}

/// Has the files looked at again, as a second's wait would, and waits for that.
pub(super) fn look_again(page: &mut Headless) {
    page.app.refs.checked = None;
    page.read_references();
}

/// Asks to rename the only key, typing `new` as what it's to be.
pub(super) fn ask_to_rename(page: &mut Headless, new: &str) {
    page.press("Rename…");
    page.frame(Vec::new());
    page.frame(Vec::new());
    type_into(page, new);
    page.frame(Vec::new());
}

/// Waits for a rename, and for the list to be counted again after it.
pub(super) fn wait_for_rename(page: &mut Headless) {
    run_until(page, |app| {
        app.refs.renaming.is_none() && app.refs.counts.is_some()
    });
    page.frame(Vec::new());
}

/// Selects `start..end` of the first block, with the caret at `caret`.
pub(super) fn select(page: &mut Headless, start: usize, end: usize, caret: usize) {
    let block_id = page.app.session.document().blocks[0].id;
    let at = |offset| crate::edit::Cursor { block_id, offset };
    page.app.session.set_caret(at(caret));
    let (from, to) = if caret == start {
        (end, start)
    } else {
        (start, end)
    };
    page.app.session.selection = Some(crate::edit::Selection {
        start: at(from),
        end: at(to),
    });
}

pub(super) fn ctrl_k(page: &mut Headless) {
    let modifiers = egui::Modifiers {
        command: true,
        ..egui::Modifiers::NONE
    };
    key(page, egui::Key::K, modifiers);
    page.frame(Vec::new());
}

/// A rename that never finishes until `send` says so, of `knuth1981`.
pub(super) fn rename_held_open(
    page: &mut Headless,
) -> std::sync::mpsc::Sender<Result<Renamed, String>> {
    let (send, coming) = channel();
    page.app.refs.renaming = Some(Renaming {
        coming,
        notebook: page.app.nb.notebook.clone().unwrap(),
        open: page.app.session.path().map(Path::to_path_buf),
        old: "knuth1981".to_string(),
        new: "plass1981".to_string(),
    });
    send
}

/// The form for a new reference, over a page that has `[knuth1981]` near its start, with
/// something typed in it.
pub(super) fn typed_form_over_a_citation() -> Headless {
    let mut page = page_in_notebook("[knuth1981] says so ");
    cite_chord(&mut page);
    type_into(&mut page, "zzz");
    press_key(&mut page, egui::Key::Enter);
    page.frame(Vec::new());
    page.frame(Vec::new());
    type_into(&mut page, "Typed Author");
    page.frame(Vec::new());
    assert_eq!(typed_authors(&page), "Typed Author");
    page
}

pub(super) fn typed_authors(page: &Headless) -> String {
    let window = page.app.refs.form.as_ref().expect("the form is up");
    window.form.authors_text.clone()
}

/// A page with the browser up and its references showing.
pub(super) fn page_with_browser(text: &str) -> Headless {
    let mut page = page_in_notebook(text);
    page.app.nb.browser_open = true;
    page.frame(Vec::new());
    page.press("▸ References");
    page.frame(Vec::new());
    page
}

/// A reference typed into the form that's up.
pub(super) fn fill_form(page: &mut Headless, key: &str) {
    page.frame(Vec::new());
    let form = &mut page.app.refs.form.as_mut().unwrap().form;
    form.key = key.to_string();
    form.key_edited = true;
    form.reference.title = "How to write".to_string();
    page.frame(Vec::new());
}

/// A form with something typed in it, up over the document in a notebook.
pub(super) fn page_with_form() -> (Headless, Option<PathBuf>) {
    let mut page = page_with_browser("");
    page.press("New reference…");
    fill_form(&mut page, "lamport1994");
    let opened = page.app.session.path().map(Path::to_path_buf);
    (page, opened)
}
