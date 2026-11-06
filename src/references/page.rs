//! What the page draws of a notebook's references: the copy of the blocks with a link over
//! each known key, and the entry a click on one opens.

use std::borrow::Cow;

use eframe::egui;
use egui::{Id, Order, Pos2, Vec2};
use kame_core::model::Block;
use kame_core::references::form::ReferenceForm;
use kame_core::references::{plain, with_citation_spans, References};

use crate::KameApp;

/// How far below a click its reference's window starts, to leave the line clicked on showing.
const BELOW_CLICK: f32 = 16.0;

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

/// The page's blocks, [`page_blocks`] of the document, kept from frame to frame for as long as the
/// document and the keys known are the same.
#[derive(Default)]
pub(crate) struct Page {
    /// The document's revision and the keys known when the page was worked out.
    from: Option<(u64, Vec<String>)>,
    /// A number that's another whenever the page may have changed.
    revision: u64,
    /// The page, if it is a copy, and otherwise it's the document's own blocks.
    copy: Option<Vec<Block>>,
}

impl Page {
    /// Works the page out again, if `blocks`, at `document` revision, or the keys known are
    /// other than they were. Returns the page's revision.
    pub(crate) fn update(
        &mut self,
        document: u64,
        blocks: &[Block],
        references: &References,
    ) -> u64 {
        let same = self.from.as_ref().is_some_and(|(revision, keys)| {
            *revision == document && keys.iter().map(String::as_str).eq(references.keys())
        });
        if !same {
            self.copy = match page_blocks(blocks, references) {
                Cow::Borrowed(_) => None,
                Cow::Owned(copy) => Some(copy),
            };
            self.from = Some((document, references.keys().map(str::to_string).collect()));
            self.revision += 1;
        }
        self.revision
    }

    /// The page, for `blocks`, the document it was last updated with.
    pub(crate) fn blocks<'a>(&'a self, blocks: &'a [Block]) -> &'a [Block] {
        self.copy.as_deref().unwrap_or(blocks)
    }
}

impl KameApp {
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
        // The entry stays where the click was, so it goes when the page moves from under it.
        let scrolled = *self.refs.shown_scroll.get_or_insert(self.page_scroll);
        if (scrolled - self.page_scroll).abs() > 0.5 {
            self.refs.shown = None;
            return;
        }
        let mut edit = None;
        let drawn = egui::Window::new(key)
            .id(Id::unique("kame-citation"))
            .fixed_pos(at + Vec2::new(0.0, BELOW_CLICK))
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.label(plain(reference));
                if ui.button("Edit").clicked() {
                    edit = Some(ReferenceForm::editing(key, reference));
                }
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
        if let Some(form) = edit {
            self.refs.shown = None;
            self.refs.open_form(form);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::test_support::*;
    use crate::Headless;
    use kame_core::session::Session;

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

    #[test]
    fn the_page_is_worked_out_again_only_for_another_document_or_other_keys() {
        let blocks = vec![paragraph("as [knuth1981] says")];
        let mut page = Page::default();

        let first = page.update(1, &blocks, &known(&["knuth1981"]));
        assert_eq!(page.blocks(&blocks)[0].spans.len(), 1);
        assert_eq!(page.update(1, &blocks, &known(&["knuth1981"])), first);

        // The same revision is the same document, so other blocks are never looked at.
        let plain = vec![paragraph("as knuth1981 says")];
        assert_eq!(page.update(1, &plain, &known(&["knuth1981"])), first);
        assert_eq!(page.blocks(&plain)[0].spans.len(), 1);

        let another = page.update(2, &plain, &known(&["knuth1981"]));
        assert_ne!(another, first);
        assert!(page.blocks(&plain)[0].spans.is_empty());

        let second = page.update(3, &blocks, &known(&["knuth1981"]));
        assert_ne!(second, another);
        let none = page.update(3, &blocks, &References::new());
        assert_ne!(none, second, "a key going is another page");
        assert!(page.blocks(&blocks)[0].spans.is_empty());
        let other = page.update(3, &blocks, &known(&["other"]));
        assert_ne!(other, none, "so is another key");
        assert_eq!(page.update(3, &blocks, &known(&["other"])), other);
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
    fn scrolling_the_page_closes_the_entry() {
        let mut page = page_in_notebook("[knuth1981] says so.");
        for _ in 0..60 {
            page.app.session.split();
            page.app
                .session
                .insert("Another paragraph of words to fill the page.");
        }
        page.frame(Vec::new());
        page.click(near_the_start(&page));
        page.frame(Vec::new());
        page.frame(Vec::new());
        assert_eq!(page.app.refs.shown.as_deref(), Some("knuth1981"));

        page.app.pending_scroll = Some(300.0);
        page.frame(Vec::new());

        assert!(page.app.page_scroll > 0.0, "the page moved");
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
    fn another_document_in_the_notebook_closes_the_entry() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        assert!(page.app.refs.shown.is_some());
        let root = page.app.nb.notebook.clone().unwrap().root;

        let other = Session::open(&root.join("other.kame")).unwrap();
        page.app.adopt_session(other);
        page.frame(Vec::new());

        assert_eq!(page.app.refs.shown, None);
        assert!(!page.app.dialog_open());
        assert_eq!(
            page.app.nb.notebook.as_ref().map(|nb| &nb.root),
            Some(&root)
        );
        assert!(page.app.refs.list.contains("knuth1981"), "the same list");
        // The keyboard is the new document's.
        type_into(&mut page, "x");
        assert_eq!(page.app.session.document().blocks[0].text, "x");
    }

    #[test]
    fn an_entry_whose_reference_is_gone_closes() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        page.app.refs.list = References::new();

        page.frame(Vec::new());

        assert_eq!(page.app.refs.shown, None);
    }

    #[test]
    fn the_entrys_edit_button_opens_the_form() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        page.frame(Vec::new());
        assert!(page.app.refs.shown.is_some());

        page.press("Edit");

        assert_eq!(page.app.refs.shown, None);
        let window = page.app.refs.form.as_ref().expect("the form opens");
        assert_eq!(window.form.key, "knuth1981");
        assert!(!window.form.is_new());
    }

    #[test]
    fn a_click_on_a_citation_behind_a_form_leaves_the_form_alone() {
        let mut page = typed_form_over_a_citation();

        page.click(near_the_start(&page));
        page.frame(Vec::new());

        assert_eq!(page.app.refs.shown, None);
        assert_eq!(typed_authors(&page), "Typed Author");
    }

    #[test]
    fn a_click_on_a_citation_during_a_rename_opens_nothing() {
        let mut page = page_in_notebook("[knuth1981] says so");
        open_list(&mut page);
        let _send = rename_held_open(&mut page);

        page.click(near_the_start(&page));
        page.frame(Vec::new());

        assert_eq!(page.app.refs.shown, None);
        assert!(page.app.refs.form.is_none());
    }

    #[test]
    fn a_click_on_a_link_behind_a_form_stays_where_it_is() {
        let mut page = page_in_notebook("other says so ");
        other_document(&page, "other.kame", "words");
        let block_id = page.app.session.document().blocks[0].id;
        assert!(page
            .app
            .session
            .edit_link(block_id, 0, 5, "other", "other.kame"));
        let here = page.app.session.path().unwrap().to_path_buf();
        page.frame(Vec::new());
        cite_chord(&mut page);
        press_key(&mut page, egui::Key::Escape);
        page.app.refs.open_form(ReferenceForm::new(None));
        page.frame(Vec::new());

        page.click(near_the_start(&page));
        page.frame(Vec::new());

        assert_eq!(page.app.session.path(), Some(here.as_path()));
        assert!(page.app.refs.form.is_some());

        // Nor does a right-click offer to follow it.
        let pos = near_the_start(&page);
        page.frame(vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            page.frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
        assert_eq!(page.app.link_menu, None);

        // With nothing up, the same click does follow it.
        press_key(&mut page, egui::Key::Escape);
        press_key(&mut page, egui::Key::Escape);
        page.click(near_the_start(&page));
        page.frame(Vec::new());
        assert_ne!(page.app.session.path(), Some(here.as_path()));
    }

    /// A page in a notebook whose path is the real one, for which a link that leads to nothing
    /// there yet can create a document.
    fn page_in_notebook_at_its_real_path(text: &str) -> Headless {
        let root = notebook_folder("page").canonicalize().unwrap();
        let mut session = Session::open(&root.join("doc.kame")).unwrap();
        session.insert(text);
        let mut page = Headless::new(session);
        page.frame(Vec::new());
        page
    }

    #[test]
    fn a_click_on_a_web_address_with_no_scheme_goes_to_the_browser() {
        let mut page = page_in_notebook_at_its_real_path("www.example.com/page and more");
        let here = page.app.session.path().unwrap().to_path_buf();

        page.click(near_the_start(&page));
        page.frame(Vec::new());

        assert_eq!(page.app.session.path(), Some(here.as_path()));
        assert!(!root_of(&page).join("www.example.com").exists());
        assert_eq!(crate::opened_in_browser(), ["https://www.example.com/page"]);
    }

    #[test]
    fn a_click_on_a_link_to_a_web_address_with_no_scheme_goes_to_the_browser() {
        let mut page = page_in_notebook_at_its_real_path("the words");
        let block_id = page.app.session.document().blocks[0].id;
        assert!(page
            .app
            .session
            .edit_link(block_id, 0, 9, "the words", "www.example.com/x"));
        let here = page.app.session.path().unwrap().to_path_buf();
        page.frame(Vec::new());

        page.click(near_the_start(&page));
        page.frame(Vec::new());

        assert_eq!(page.app.session.path(), Some(here.as_path()));
        assert!(!root_of(&page).join("www.example.com").exists());
        assert_eq!(crate::opened_in_browser(), ["https://www.example.com/x"]);
    }

    #[test]
    fn a_click_on_what_links_here_behind_a_form_stays_where_it_is() {
        let root = notebook_folder("linked-to");
        let mut linking = Session::open(&root.join("linking.kame")).unwrap();
        linking.insert("to it");
        let block_id = linking.document().blocks[0].id;
        assert!(linking.edit_link(block_id, 0, 2, "to", "doc.kame"));
        drop(linking);
        let mut session = Session::open(&root.join("doc.kame")).unwrap();
        session.insert("words");
        let mut page = Headless::new(session);
        run_until(&mut page, |app| !app.nb.backlinks.is_empty());
        let x = page.body_left() + 8.0;
        let click_down_the_page = |page: &mut Headless| {
            for y in (20..780).step_by(8) {
                page.click(Pos2::new(x, y as f32));
            }
        };

        page.app.refs.open_form(ReferenceForm::new(None));
        page.frame(Vec::new());
        click_down_the_page(&mut page);
        assert_eq!(page.app.nb.pending_open, None);
        assert!(page.app.refs.form.is_some());

        // With nothing up, one of those clicks is on it.
        press_key(&mut page, egui::Key::Escape);
        click_down_the_page(&mut page);
        assert_ne!(page.app.nb.pending_open, None);
    }

    #[test]
    fn escape_closes_an_entry_over_the_list_and_leaves_the_list() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);
        page.press("Delete…");
        page.frame(Vec::new());
        page.app.refs.show("knuth1981", Pos2::new(10.0, 10.0));
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::Escape);

        assert_eq!(page.app.refs.shown, None);
        let window = page.app.refs.window.as_ref().expect("the list stays");
        assert!(window.confirming.is_some(), "and so does what it's asking");
    }
}
