//! A notebook's references, from the app: keeping up with its list, what a key looks like on the
//! page, and the notes that come of keys with no reference. The page and the caret keys shape
//! `with_citation_spans` of a block, a copy that is never edited, so a known key is coloured,
//! kept whole and clicked on as any link is. See
//! `docs/superpowers/specs/2026-11-04-references-design.md`.

use std::borrow::Cow;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use egui::{Id, Order, Pos2, Vec2};
use kame_core::model::{Block, Document};
use kame_core::references::{
    block_citation_ranges, key_of, plain, unresolved, with_citation_spans, References,
};
use kame_notebook::references::{load, stamp};
use kame_notebook::Notebook;

use super::KameApp;

/// How long to go without looking for a change to the reference files, since the page asks every
/// frame.
const CHECK_EVERY: Duration = Duration::from_secs(1);
/// How far below a click its reference's window starts, to leave the line clicked on showing.
const BELOW_CLICK: f32 = 16.0;

/// The notebook's references, and which one is showing.
#[derive(Default)]
pub(crate) struct ReferencesUi {
    /// What the notebook on screen holds. Nothing outside one.
    pub(crate) list: References,
    /// What was wrong with the files left out of the list.
    problems: Vec<String>,
    /// The files the list was read from, as they were.
    stamp: Vec<(String, SystemTime, u64)>,
    /// The notebook the list was read from.
    root: Option<PathBuf>,
    /// When the files were last looked at.
    checked: Option<Instant>,
    /// The key whose entry is in its window.
    pub(crate) shown: Option<String>,
    /// Where the click that opened it was.
    shown_at: Pos2,
}

impl ReferencesUi {
    /// Keeps the list up with `notebook` and its files. Says what's wrong with them when that
    /// changes, as there's no use saying it every time they are looked at.
    pub(crate) fn refresh(&mut self, notebook: Option<&Notebook>) -> Option<String> {
        let Some(notebook) = notebook else {
            *self = ReferencesUi::default();
            return None;
        };
        let moved = self.root.as_ref() != Some(&notebook.root);
        let due = self
            .checked
            .is_none_or(|checked| checked.elapsed() >= CHECK_EVERY);
        if !moved && !due {
            return None;
        }
        self.checked = Some(Instant::now());
        // Taken before reading, so that a file changing in between is read again next time.
        let files = stamp(notebook);
        if !moved && files == self.stamp {
            return None;
        }
        if moved {
            self.root = Some(notebook.root.clone());
            self.problems.clear();
            self.shown = None;
        }
        self.stamp = files;
        let loaded = load(notebook);
        self.list = loaded.references;
        let news = !loaded.problems.is_empty() && loaded.problems != self.problems;
        self.problems = loaded.problems;
        news.then(|| format!("this notebook's references: {}", self.problems.join("; ")))
    }

    /// Opens `key`'s entry, for a click at `at`.
    pub(crate) fn show(&mut self, key: &str, at: Pos2) {
        self.shown = Some(key.to_string());
        self.shown_at = at;
    }
}

/// `blocks` as the page draws them, with a link over each known key. They are only copied if there
/// is one, and the copy is for shaping and hit-testing, never editing.
pub(crate) fn page_blocks<'a>(blocks: &'a [Block], references: &References) -> Cow<'a, [Block]> {
    if references.is_empty() {
        return Cow::Borrowed(blocks);
    }
    let cited: Vec<Cow<Block>> = blocks
        .iter()
        .map(|block| with_citation_spans(block, references))
        .collect();
    if cited.iter().all(|block| matches!(block, Cow::Borrowed(_))) {
        return Cow::Borrowed(blocks);
    }
    Cow::Owned(cited.into_iter().map(Cow::into_owned).collect())
}

/// What to say when the caret has just closed a key with no reference.
pub(crate) fn just_closed_note(
    block: &Block,
    caret: usize,
    references: &References,
) -> Option<String> {
    let closed = block_citation_ranges(block)
        .into_iter()
        .find(|(_, end)| *end == caret)?;
    let key = key_of(&block.text, closed);
    (!references.contains(key)).then(|| format!("No reference called {key}. Ctrl-Shift-C adds it."))
}

/// What to add to an export's notice about keys that look lost. `marginals` is whether the format
/// carries them.
pub(crate) fn unresolved_note(
    doc: &Document,
    references: &References,
    marginals: bool,
) -> Option<String> {
    let lost = unresolved(doc, references, marginals);
    (!lost.is_empty()).then(|| format!("no reference for {}", lost.join(", ")))
}

impl KameApp {
    /// Says so if typing `]` has just closed a key with no reference. A document outside a
    /// notebook has no references to add one to.
    pub(crate) fn note_closed_citation(&mut self) {
        if self.nb.notebook.is_none() {
            return;
        }
        let caret = self.session.valid_caret();
        let note = self
            .session
            .document()
            .blocks
            .iter()
            .find(|block| block.id == caret.block_id)
            .and_then(|block| just_closed_note(block, caret.offset, &self.refs.list));
        if let Some(note) = note {
            self.notice_tracker.announced();
            self.notice = Some(note);
        }
    }

    /// `unresolved_note` for the open document, if it's in a notebook.
    fn lost_keys_note(&self, marginals: bool) -> Option<String> {
        self.nb.notebook.as_ref()?;
        unresolved_note(self.session.document(), &self.refs.list, marginals)
    }

    /// What to say about an export, given what it did.
    pub(crate) fn export_notice(&self, done: String, marginals: bool) -> String {
        match self.lost_keys_note(marginals) {
            Some(note) => format!("{done}; {note}"),
            None => done,
        }
    }

    /// Draws the entry of the citation clicked on, until Escape or a press anywhere else.
    pub(crate) fn show_citation(&mut self, ctx: &egui::Context) {
        let Some(key) = self.refs.shown.as_deref() else {
            return;
        };
        let Some(reference) = self.refs.list.get(key) else {
            self.refs.shown = None;
            return;
        };
        let at = self.refs.shown_at;
        let drawn = egui::Window::new(key)
            .id(Id::unique("kame-citation"))
            .fixed_pos(at + Vec2::new(0.0, BELOW_CLICK))
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.label(plain(reference));
                ui.add_enabled(false, egui::Button::new("Edit"));
            });
        // A press at the same place is the click that opened this, in one frame on a quick tap.
        let pressed = ctx.input(|input| {
            input
                .pointer
                .interact_pos()
                .filter(|pos| input.pointer.any_pressed() && *pos != at)
        });
        let inside = |pos: Pos2| {
            drawn
                .as_ref()
                .is_some_and(|drawn| drawn.response.rect.contains(pos))
        };
        if pressed.is_some_and(|pos| !inside(pos)) {
            self.refs.shown = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_temp_dir, Headless};
    use kame_core::model::{Anchor, Annotation, AnnotationKind, BlockKind};
    use kame_core::references::Reference;
    use kame_core::session::Session;

    fn block(kind: BlockKind, text: &str) -> Block {
        Block {
            id: uuid::Uuid::new_v4(),
            kind,
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    fn paragraph(text: &str) -> Block {
        block(BlockKind::Paragraph, text)
    }

    fn known(keys: &[&str]) -> References {
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

    #[test]
    fn closing_an_unknown_key_says_how_to_add_it() {
        let references = known(&["knuth1981"]);
        let note =
            |text: &str, caret: usize| just_closed_note(&paragraph(text), caret, &references);
        let closed = "as in [kleppmann2019]";

        assert_eq!(
            note(closed, closed.len()).as_deref(),
            Some("No reference called kleppmann2019. Ctrl-Shift-C adds it.")
        );
        // Having the shape of a key is enough, since the note doesn't interrupt.
        assert_eq!(
            note("[sic]", 5).as_deref(),
            Some("No reference called sic. Ctrl-Shift-C adds it.")
        );
        assert_eq!(note("as in [knuth1981]", 17), None, "a known key");
        assert_eq!(note("as in [a b]", 11), None, "not the shape of a key");
        assert_eq!(note(closed, 3), None, "a caret elsewhere");
        assert_eq!(note(closed, closed.len() - 1), None, "a caret inside it");

        let code = block(BlockKind::Code { language: None }, "[kleppmann2019]");
        assert_eq!(just_closed_note(&code, 15, &references), None);
    }

    #[test]
    fn an_export_with_a_lost_key_says_so() {
        let references = known(&["knuth1981"]);
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("as [smith2020], [sic] and [knuth1981] say")];

        let note = unresolved_note(&doc, &references, true).expect("a lost key is named");
        assert!(note.contains("smith2020"), "{note}");
        assert!(!note.contains("sic"), "{note}");
        assert!(!note.contains("knuth1981"), "{note}");

        doc.blocks = vec![paragraph("as [sic] and [knuth1981] say")];
        assert_eq!(unresolved_note(&doc, &references, true), None);

        // A marginal counts only where the format carries it.
        doc.annotations.push(Annotation {
            id: uuid::Uuid::new_v4(),
            kind: AnnotationKind::Marginal,
            anchor: Anchor::block(doc.blocks[0].id),
            body: "[jones2019]".to_string(),
        });
        assert_eq!(unresolved_note(&doc, &references, false), None);
        let note = unresolved_note(&doc, &references, true).expect("a marginal's key is named");
        assert!(note.contains("jones2019"), "{note}");
    }

    #[test]
    fn the_page_gets_a_copy_with_links_over_known_keys_only() {
        let blocks = vec![
            paragraph("as [knuth1981] says"),
            paragraph("no [smith2020]"),
        ];

        let none = page_blocks(&blocks, &References::new());
        assert!(matches!(none, Cow::Borrowed(_)), "nothing to copy");

        let unrelated = page_blocks(&blocks, &known(&["other"]));
        assert!(
            matches!(unrelated, Cow::Borrowed(_)),
            "nothing cited is known"
        );

        let page = page_blocks(&blocks, &known(&["knuth1981"]));
        assert_eq!(page[0].spans.len(), 1);
        assert_eq!((page[0].spans[0].start, page[0].spans[0].end), (3, 14));
        assert_eq!(page[1], blocks[1]);
        assert_eq!(page[0].text, blocks[0].text);
        assert!(blocks[0].spans.is_empty(), "the document is left alone");
    }

    /// A notebook folder with an empty `references/` in it.
    fn notebook_folder(name: &str) -> std::path::PathBuf {
        let root = test_temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("references")).unwrap();
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        root
    }

    fn write_reference(root: &std::path::Path, key: &str, contents: &str) {
        std::fs::write(
            root.join("references").join(format!("{key}.yaml")),
            contents,
        )
        .unwrap();
    }

    const GOOD: &str = "kind: article\ntitle: Breaking paragraphs into lines\n";

    #[test]
    fn the_list_follows_the_files_and_the_notebook() {
        let first = notebook_folder("list-first");
        let second = notebook_folder("list-second");
        write_reference(&first, "knuth1981", GOOD);
        let first = Notebook::open(&first).unwrap();
        let second = Notebook::open(&second).unwrap();

        let mut refs = ReferencesUi::default();
        assert_eq!(refs.refresh(Some(&first)), None);
        assert!(refs.list.contains("knuth1981"));

        write_reference(&first.root, "smith2020", GOOD);
        refs.checked = None;
        assert_eq!(refs.refresh(Some(&first)), None);
        assert!(refs.list.contains("smith2020"), "a new file is seen");

        std::fs::remove_file(first.root.join("references/smith2020.yaml")).unwrap();
        // Not looked for yet, since it hasn't been long.
        refs.refresh(Some(&first));
        assert!(refs.list.contains("smith2020"));
        refs.checked = None;
        refs.refresh(Some(&first));
        assert!(!refs.list.contains("smith2020"), "a removed file is seen");

        refs.shown = Some("knuth1981".to_string());
        refs.refresh(Some(&second));
        assert!(refs.list.is_empty(), "another notebook, at once");
        assert_eq!(refs.shown, None);

        refs.refresh(Some(&first));
        assert!(refs.list.contains("knuth1981"));
        refs.refresh(None);
        assert!(refs.list.is_empty(), "outside a notebook there are none");
    }

    #[test]
    fn a_damaged_file_is_reported_once_and_the_others_still_load() {
        let root = notebook_folder("list-damaged");
        write_reference(&root, "knuth1981", GOOD);
        write_reference(&root, "broken2020", "title: [not closed\n");
        let notebook = Notebook::open(&root).unwrap();

        let mut refs = ReferencesUi::default();
        let problem = refs
            .refresh(Some(&notebook))
            .expect("the damage is reported");
        assert!(problem.contains("broken2020"), "{problem}");
        assert!(refs.list.contains("knuth1981"));

        write_reference(&root, "smith2020", GOOD);
        refs.checked = None;
        assert_eq!(refs.refresh(Some(&notebook)), None, "already said");
        assert!(refs.list.contains("smith2020"));
    }

    /// A page in a notebook holding `knuth1981`, with `text` as its only block.
    fn page_in_notebook(text: &str) -> Headless {
        let root = notebook_folder("page");
        write_reference(&root, "knuth1981", GOOD);
        let mut session = Session::open(&root.join("doc.kame")).unwrap();
        session.insert(text);
        let mut page = Headless::new(session);
        page.frame(Vec::new());
        page
    }

    /// Somewhere on the first line of the text, a little way in.
    fn near_the_start(page: &Headless) -> Pos2 {
        Pos2::new(page.body_left() + 12.0, crate::PAGE_PADDING + 8.0)
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
    fn clicking_a_known_key_shows_it_and_leaves_the_caret() {
        let mut page = page_in_notebook("[knuth1981] says so");
        let caret = page.app.session.caret;

        page.click(near_the_start(&page));

        assert_eq!(page.app.refs.shown.as_deref(), Some("knuth1981"));
        assert_eq!(page.app.session.caret, caret);
        assert!(page.app.dialog_open());
        // The click that opened it isn't a click elsewhere, on this frame or the next.
        page.frame(Vec::new());
        assert_eq!(page.app.refs.shown.as_deref(), Some("knuth1981"));
    }

    #[test]
    fn clicking_an_unknown_key_places_the_caret() {
        let mut page = page_in_notebook("[smith2020] says so");
        let caret = page.app.session.caret;

        page.click(near_the_start(&page));

        assert_eq!(page.app.refs.shown, None);
        assert_ne!(page.app.session.caret, caret);
    }

    #[test]
    fn escape_closes_the_entry() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        assert!(page.app.refs.shown.is_some());

        page.frame(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);

        assert_eq!(page.app.refs.shown, None);
        assert!(!page.app.dialog_open());
    }

    #[test]
    fn clicking_elsewhere_closes_the_entry() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        assert!(page.app.refs.shown.is_some());

        // Far enough right to land past the key, since a click below the text lands on its last
        // line.
        page.click(Pos2::new(page.body_left() + 300.0, 600.0));

        assert_eq!(page.app.refs.shown, None);
    }

    #[test]
    fn clicking_in_the_entry_keeps_it_open() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        let caret = page.app.session.caret;
        page.frame(Vec::new());
        let inside = page.app.refs.shown_at + egui::Vec2::new(30.0, BELOW_CLICK + 30.0);

        page.click(inside);

        assert_eq!(page.app.refs.shown.as_deref(), Some("knuth1981"));
        assert_eq!(
            page.app.session.caret, caret,
            "the page didn't get the click"
        );
    }

    #[test]
    fn a_quick_tap_in_one_frame_opens_the_entry_and_leaves_it_open() {
        let mut page = page_in_notebook("[knuth1981] says so");
        let pos = near_the_start(&page);
        page.frame(vec![egui::Event::PointerMoved(pos)]);

        page.frame(
            [true, false]
                .map(|pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                })
                .to_vec(),
        );

        assert_eq!(page.app.refs.shown.as_deref(), Some("knuth1981"));
    }

    #[test]
    fn a_double_click_selects_the_key_and_closes_the_entry() {
        let mut page = page_in_notebook("[knuth1981] says so");

        page.double_click(near_the_start(&page));

        assert_eq!(page.app.refs.shown, None);
        assert!(page.app.session.selection.is_some());
    }

    #[test]
    fn an_entry_whose_reference_is_gone_closes() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        page.app.refs.list = References::new();

        page.frame(Vec::new());

        assert_eq!(page.app.refs.shown, None);
    }

    /// Types `text` a character at a time, as the keyboard does.
    fn type_into(page: &mut Headless, text: &str) {
        for character in text.chars() {
            page.frame(vec![egui::Event::Text(character.to_string())]);
        }
    }

    #[test]
    fn typing_the_bracket_of_an_unknown_key_leaves_a_note_that_passes() {
        let mut page = page_in_notebook("");
        type_into(&mut page, "as in [kleppmann2019");
        assert_eq!(page.app.notice, None);

        type_into(&mut page, "]");
        assert_eq!(
            page.app.notice.as_deref(),
            Some("No reference called kleppmann2019. Ctrl-Shift-C adds it.")
        );

        // Typing joins up into words, so it's the next word that's the next edit.
        type_into(&mut page, " next");
        page.frame(Vec::new());
        assert_eq!(page.app.notice, None);
    }

    #[test]
    fn typing_the_bracket_of_a_known_key_says_nothing() {
        let mut page = page_in_notebook("");
        type_into(&mut page, "as in [knuth1981]");
        assert_eq!(page.app.notice, None);
    }

    #[test]
    fn the_same_note_twice_running_still_shows() {
        let mut page = page_in_notebook("[a2000 and [a2000");
        type_into(&mut page, "]");
        assert!(page.app.notice.is_some());

        // Moving the caret isn't an edit, so the note stays up until the next one, which says it
        // again.
        let block_id = page.app.session.document().blocks[0].id;
        page.app.session.set_caret(crate::edit::Cursor {
            block_id,
            offset: "[a2000".len(),
        });
        type_into(&mut page, "]");
        page.frame(Vec::new());

        assert_eq!(
            page.app.session.document().blocks[0].text,
            "[a2000] and [a2000]"
        );
        assert!(page.app.notice.is_some());
    }

    #[test]
    fn outside_a_notebook_no_note_is_left_for_a_key_or_an_export() {
        let folder = test_temp_dir().join(format!("outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let session = Session::open(&folder.join("doc.kame")).unwrap();
        let mut page = Headless::new(session);
        page.frame(Vec::new());

        type_into(&mut page, "[smith2020]");

        assert_eq!(page.app.notice, None);
        assert_eq!(
            page.app.export_notice("exported".to_string(), true),
            "exported"
        );
    }

    #[test]
    fn in_a_notebook_an_export_names_a_lost_key() {
        let mut page = page_in_notebook("");
        type_into(&mut page, "[smith2020] and [knuth1981]");

        let notice = page.app.export_notice("exported".to_string(), true);

        assert_eq!(notice, "exported; no reference for smith2020");
    }
}
