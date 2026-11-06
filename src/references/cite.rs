//! Cite: what it opens for where the caret is, the search over references, and putting a key
//! in the document.

use eframe::egui;
use kame_core::references::form::ReferenceForm;
use kame_core::references::{
    block_citation_ranges, citation_at, key_of, plain, valid_key, Reference, References,
};

use crate::notebook::{focus_field, list_keys, list_row, list_rows, list_window};
use crate::KameApp;

/// How much of an entry a row of the search shows.
const SHOWN_OF_ENTRY: usize = 60;

/// The search over references: what's typed, and the row chosen.
pub(super) struct CiteSearch {
    pub(super) query: String,
    selected: usize,
    focus: bool,
}

/// What Cite opens, going by the caret.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CiteStart {
    NeedsNotebook,
    New { key: String },
    Edit { key: String },
    Search,
}

/// What Cite opens for a caret at `offset` in `text`.
pub(crate) fn cite_start(
    text: &str,
    offset: usize,
    references: &References,
    in_notebook: bool,
) -> CiteStart {
    if !in_notebook {
        return CiteStart::NeedsNotebook;
    }
    let Some(range) = citation_at(text, offset) else {
        return CiteStart::Search;
    };
    let key = key_of(text, range).to_string();
    if references.contains(&key) {
        CiteStart::Edit { key }
    } else {
        CiteStart::New { key }
    }
}

/// The keys whose key, authors or title hold `query`, ignoring case, in `sorted()` order.
pub(crate) fn search<'a>(references: &'a References, query: &str) -> Vec<&'a str> {
    let query = query.trim().to_lowercase();
    let holds = |text: &str| text.to_lowercase().contains(&query);
    references
        .sorted()
        .into_iter()
        .filter(|(key, reference)| {
            holds(key)
                || holds(&reference.title)
                || reference.authors.iter().any(|name| holds(name))
        })
        .map(|(key, _)| key)
        .collect()
}

/// Where citing `key` puts it, for a caret at `offset`: nowhere if the caret is on it already, and
/// after a different key the caret is on rather than in the middle of it.
fn where_to_cite(text: &str, offset: usize, key: &str) -> Option<usize> {
    match citation_at(text, offset) {
        Some(range) if key_of(text, range) == key => None,
        Some((_, end)) => Some(end),
        None => Some(offset),
    }
}

/// The start of an entry, for a row with little room.
fn entry_start(reference: &Reference) -> String {
    let entry = plain(reference);
    match entry.char_indices().nth(SHOWN_OF_ENTRY) {
        Some((at, _)) => format!("{}…", &entry[..at]),
        None => entry,
    }
}

impl KameApp {
    /// The text of the block `caret` is in, to look for a key there. Nothing if the caret is on
    /// text that only looks like a key, in code or a link's words, as that is no key to open or
    /// to take for one already cited.
    fn text_for_citing(&self, caret: crate::edit::Cursor) -> &str {
        self.session
            .document()
            .blocks
            .iter()
            .find(|block| block.id == caret.block_id)
            .filter(|block| {
                citation_at(&block.text, caret.offset)
                    .is_none_or(|range| block_citation_ranges(block).contains(&range))
            })
            .map_or("", |block| block.text.as_str())
    }

    /// Cite: the chord, the menus and the right-click row. Does nothing over another dialog.
    pub(crate) fn begin_cite(&mut self) {
        if self.dialog_open() || self.close_flow.prompting {
            return;
        }
        let caret = self.session.valid_caret();
        let text = self.text_for_citing(caret);
        let in_notebook = self.nb.notebook.is_some();
        match cite_start(text, caret.offset, &self.refs.list, in_notebook) {
            CiteStart::NeedsNotebook => {
                self.notice_tracker.announced();
                self.notice = Some("Citing needs a notebook.".to_string());
            }
            CiteStart::New { key } => self.refs.open_form(ReferenceForm::new(Some(key))),
            CiteStart::Edit { key } => {
                if let Some(reference) = self.refs.list.get(&key) {
                    let form = ReferenceForm::editing(&key, reference);
                    self.refs.open_form(form);
                }
            }
            CiteStart::Search => {
                self.refs.cite_search = Some(CiteSearch {
                    query: String::new(),
                    selected: 0,
                    focus: true,
                });
            }
        }
    }

    /// Puts `[key]` in the document, as one ordinary edit, unless the caret is on it already. A
    /// selection is left in the document, with the key just after it, as citing is no way to
    /// replace words.
    pub(super) fn cite(&mut self, key: &str) {
        if let Some((_, end)) = self.session.ordered_selection() {
            self.session.set_caret(end);
        }
        let caret = self.session.valid_caret();
        let Some(at) = where_to_cite(self.text_for_citing(caret), caret.offset, key) else {
            return;
        };
        if at != caret.offset {
            self.session.set_caret(crate::edit::Cursor {
                block_id: caret.block_id,
                offset: at,
            });
        }
        self.follow_caret = true;
        if !self.session.insert_as_step(&format!("[{key}]")) {
            let why = self.session.last_error.take();
            let why = why.as_deref().unwrap_or("this document can't be changed");
            // The caret goes past the key once it's in the document, whether or not the write
            // that follows lands. The error strip's retry saves it, as for anything typed.
            self.session.last_error = Some(if self.session.caret.offset > at {
                format!("{key} is in the document but couldn't be saved — {why}")
            } else {
                format!("could not cite {key} here — {why}")
            });
        }
    }

    pub(super) fn show_cite_search(&mut self, ctx: &egui::Context) {
        let Some(mut search_list) = self.refs.cite_search.take() else {
            return;
        };
        let hits: Vec<String> = search(&self.refs.list, &search_list.query)
            .into_iter()
            .map(str::to_string)
            .collect();
        let rows_count = hits.len() + 1;
        // Keys taken here wouldn't reach what's on top of this. Escape needs nothing from here, as
        // `dismiss_dialog` has seen to it, knowing whether this is the one on top.
        let confirm = !self.keys_are_elsewhere(ctx)
            && list_keys(ctx, &mut search_list.selected, rows_count).1
            && !search_list.focus;
        let mut clicked = None;
        let references = &self.refs.list;
        let typed = search_list.query.trim().to_string();
        let new_row = if typed.is_empty() {
            "New reference…".to_string()
        } else {
            format!("New reference “{typed}”…")
        };
        list_window(ctx, "kame-cite", |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut search_list.query)
                    .hint_text("Cite a reference")
                    .return_key(None)
                    .desired_width(f32::INFINITY),
            );
            focus_field(&edit, &mut search_list.focus);
            if edit.changed() {
                search_list.selected = 0;
            }
            ui.separator();
            list_rows(ui, rows_count, search_list.selected, |ui, row, selected| {
                let (main, side) = match hits.get(row) {
                    Some(key) => (
                        key.as_str(),
                        references.get(key).map(entry_start).unwrap_or_default(),
                    ),
                    None => (new_row.as_str(), String::new()),
                };
                if list_row(ui, selected, main, &side).clicked() {
                    clicked = Some(row);
                }
            });
        });
        let chosen = clicked.or(confirm.then_some(search_list.selected));
        match chosen {
            Some(row) => match hits.get(row) {
                Some(key) => self.cite(key),
                None => {
                    let key = valid_key(&typed).then_some(typed);
                    self.refs.open_form(ReferenceForm::new(key));
                }
            },
            None => self.refs.cite_search = Some(search_list),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::test_support::*;
    use crate::{test_temp_dir, Headless};
    use egui::Pos2;
    use kame_core::model::{AnnotationKind, BlockKind};
    use kame_core::session::Session;

    #[test]
    fn cite_opens_what_the_caret_is_on() {
        let references = known(&["knuth1981"]);
        let start = |text: &str, offset: usize| cite_start(text, offset, &references, true);
        let new = CiteStart::New {
            key: "new1".to_string(),
        };
        let edit = CiteStart::Edit {
            key: "knuth1981".to_string(),
        };

        assert_eq!(
            cite_start("[knuth1981]", 3, &references, false),
            CiteStart::NeedsNotebook
        );
        assert_eq!(start("see [new1] here", 6), new, "in an unknown key");
        assert_eq!(start("see [new1] here", 10), new, "just after it");
        assert_eq!(start("see [knuth1981] here", 6), edit, "in a known key");
        assert_eq!(start("see [knuth1981] here", 15), edit, "just after it");
        assert_eq!(
            start("see [knuth1981] here", 4),
            CiteStart::Search,
            "before it"
        );
        assert_eq!(
            start("see [knuth1981] here", 16),
            CiteStart::Search,
            "past it"
        );
        assert_eq!(start("plain words", 3), CiteStart::Search);
        assert_eq!(
            start("in [a b] here", 6),
            CiteStart::Search,
            "not a key's shape"
        );
        assert_eq!(start("", 0), CiteStart::Search);
        // Offsets are bytes, so a multibyte letter before the key has to be counted as such.
        assert_eq!(start("é [new1]", "é [new1]".len()), new);
    }

    #[test]
    fn search_looks_through_keys_authors_and_titles() {
        let mut references = References::new();
        references.insert(
            "zeta".to_string(),
            Reference {
                authors: vec!["D. E. Knuth".to_string()],
                title: "Breaking paragraphs".to_string(),
                ..Reference::default()
            },
        );
        references.insert(
            "Alpha".to_string(),
            Reference {
                title: "Knuth and his lines".to_string(),
                ..Reference::default()
            },
        );
        references.insert(
            "knuth1981".to_string(),
            Reference {
                title: "Élan".to_string(),
                ..Reference::default()
            },
        );

        assert_eq!(search(&references, "KNUTH"), ["Alpha", "knuth1981", "zeta"]);
        assert_eq!(search(&references, "breaking"), ["zeta"]);
        assert_eq!(search(&references, "alpha"), ["Alpha"]);
        assert_eq!(search(&references, "élan"), ["knuth1981"]);
        assert_eq!(search(&references, "  zeta "), ["zeta"]);
        assert_eq!(search(&references, "nothing"), Vec::<&str>::new());
        assert_eq!(
            search(&references, ""),
            ["Alpha", "knuth1981", "zeta"],
            "everything, with nothing typed"
        );
    }

    #[test]
    fn a_citation_goes_at_the_caret_unless_the_caret_is_on_it() {
        assert_eq!(where_to_cite("plain words", 5, "k"), Some(5));
        assert_eq!(where_to_cite("see [k] here", 6, "k"), None, "in it");
        assert_eq!(where_to_cite("see [k] here", 7, "k"), None, "just after it");
        assert_eq!(where_to_cite("see [k] here", 4, "k"), Some(4), "before it");
        // A key other than the one being cited isn't broken in two.
        assert_eq!(where_to_cite("see [other] here", 6, "k"), Some(11));
        assert_eq!(where_to_cite("see [other] here", 11, "k"), Some(11));
    }

    #[test]
    fn citing_outside_a_notebook_says_it_needs_one() {
        let folder = test_temp_dir().join(format!("outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut page = Headless::new(Session::open(&folder.join("doc.kame")).unwrap());
        page.frame(Vec::new());

        cite_chord(&mut page);

        assert_eq!(page.app.notice.as_deref(), Some("Citing needs a notebook."));
        assert!(!page.app.dialog_open());
    }

    #[test]
    fn choosing_from_the_search_inserts_the_key_as_one_undo() {
        let mut page = page_in_notebook("as said by ");
        cite_chord(&mut page);
        assert!(page.app.dialog_open());

        // What's typed goes to the search, not the document.
        type_into(&mut page, "KNUTH");
        assert_eq!(only_text(&page), "as said by ");

        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "as said by [knuth1981]");
        assert!(!page.app.dialog_open());
        assert_eq!(page.app.session.caret.offset, only_text(&page).len());
        assert!(page.app.session.undo());
        assert_eq!(only_text(&page), "as said by ", "only the key goes");
    }

    #[test]
    fn up_and_down_choose_a_row_of_the_search() {
        let mut page = page_in_notebook("");
        write_reference(&root_of(&page), "smith2020", GOOD);
        page.app.reload_references();
        cite_chord(&mut page);

        press_key(&mut page, egui::Key::ArrowDown);
        press_key(&mut page, egui::Key::ArrowDown);
        press_key(&mut page, egui::Key::ArrowUp);
        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "[smith2020]");
    }

    #[test]
    fn clicking_a_row_of_the_search_chooses_it() {
        let mut page = page_in_notebook("");
        cite_chord(&mut page);

        page.press("knuth1981");

        assert_eq!(only_text(&page), "[knuth1981]");
    }

    #[test]
    fn the_searchs_last_row_is_a_new_reference_with_the_query_as_its_key() {
        let mut page = page_in_notebook("");
        cite_chord(&mut page);
        type_into(&mut page, "lamport1994");
        page.frame(Vec::new());
        assert!(page.sees("New reference “lamport1994”…"));

        press_key(&mut page, egui::Key::Enter);

        let window = page.app.refs.form.as_ref().expect("the form opens");
        assert_eq!(window.form.key, "lamport1994");
        assert!(page.app.refs.cite_search.is_none());
        assert_eq!(only_text(&page), "");
    }

    #[test]
    fn a_query_that_cant_be_a_key_leaves_the_key_to_be_suggested() {
        let mut page = page_in_notebook("");
        cite_chord(&mut page);
        type_into(&mut page, "two words");

        press_key(&mut page, egui::Key::Enter);

        let form = &page.app.refs.form.as_ref().expect("the form opens").form;
        assert_eq!(form.key, "");
        assert!(!form.key_edited);
    }

    #[test]
    fn saving_a_new_key_the_caret_is_on_leaves_the_text_alone() {
        let mut page = page_in_notebook("as in [lamport1994]");
        let history = page.app.session.history();
        cite_chord(&mut page);

        let window = page.app.refs.form.as_mut().expect("the form opens");
        assert_eq!(window.form.key, "lamport1994");
        window.form.reference.title = "How to write".to_string();
        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.refs.form.is_none());
        assert_eq!(only_text(&page), "as in [lamport1994]");
        assert_eq!(page.app.session.history(), history);
        assert!(page.app.refs.list.contains("lamport1994"));
    }

    #[test]
    fn a_key_the_form_was_changed_to_isnt_put_inside_the_one_the_caret_is_on() {
        let mut page = page_in_notebook("as in [lamport1994] so");
        let block_id = page.app.session.document().blocks[0].id;
        page.app.session.set_caret(crate::edit::Cursor {
            block_id,
            offset: "as in [lam".len(),
        });
        cite_chord(&mut page);

        let window = page.app.refs.form.as_mut().expect("the form opens");
        window.form.key = "other".to_string();
        window.form.reference.title = "T".to_string();
        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "as in [lamport1994][other] so");
    }

    #[test]
    fn a_known_key_under_the_caret_opens_for_editing() {
        let mut page = page_in_notebook("as in [knuth1981]");
        let history = page.app.session.history();
        cite_chord(&mut page);

        let window = page.app.refs.form.as_mut().expect("the form opens");
        assert_eq!(
            window.form.reference.title,
            "Breaking paragraphs into lines"
        );
        window.form.reference.title = "Another title".to_string();
        page.frame(Vec::new());
        page.press("Save");

        assert!(page.app.refs.form.is_none());
        assert_eq!(
            page.app.refs.list.get("knuth1981").unwrap().title,
            "Another title"
        );
        let file = std::fs::read_to_string(reference_file(&page, "knuth1981")).unwrap();
        assert!(file.contains("Another title"), "{file}");
        assert_eq!(page.app.session.history(), history, "no edit to the text");
    }

    #[test]
    fn text_that_only_looks_like_a_key_opens_the_search() {
        let mut page = page_in_notebook("[knuth1981]");
        let block_id = page.app.session.document().blocks[0].id;
        page.app
            .session
            .set_block_kind(block_id, BlockKind::Code { language: None });
        cite_chord(&mut page);

        assert!(page.app.refs.cite_search.is_some());
        assert!(page.app.refs.form.is_none());
    }

    #[test]
    fn help_being_up_leaves_the_search_its_keys() {
        let mut page = page_in_notebook("");
        write_reference(&root_of(&page), "smith2020", GOOD);
        page.app.reload_references();
        cite_chord(&mut page);
        page.app.show_help = true;
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::ArrowDown);
        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "[smith2020]");
    }

    #[test]
    fn the_chord_is_ignored_over_a_dialog() {
        let mut page = page_in_notebook("hello");
        page.app.toggle_jump();
        cite_chord(&mut page);
        assert!(page.app.refs.cite_search.is_none());
        page.app.nb.close_lists();

        cite_chord(&mut page);
        assert!(page.app.refs.cite_search.is_some());
        cite_chord(&mut page);
        assert!(page.app.refs.cite_search.is_some(), "it doesn't toggle");
        assert!(page.app.refs.form.is_none());
    }

    #[test]
    fn the_chord_is_ignored_in_a_notes_box() {
        let mut page = page_in_notebook("hello");
        let id = page
            .app
            .session
            .add_annotation(AnnotationKind::Footnote)
            .unwrap();
        page.app.set_open_note(Some(id));
        page.frame(Vec::new());
        page.frame(Vec::new());

        cite_chord(&mut page);

        assert!(!page.app.refs.window_open());
    }

    #[test]
    fn a_right_click_offers_citing_in_a_notebook_only() {
        let secondary = |page: &mut Headless, pos: Pos2| {
            page.frame(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                page.frame(vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }
            page.frame(Vec::new());
        };
        let mut page = page_in_notebook("hello ");
        let pos = near_the_start(&page);
        secondary(&mut page, pos);
        page.press("Cite…");
        page.frame(Vec::new());
        assert!(page.app.refs.cite_search.is_some());

        let folder = test_temp_dir().join(format!("outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut session = Session::open(&folder.join("doc.kame")).unwrap();
        session.insert("hello");
        let mut outside = Headless::new(session);
        outside.frame(Vec::new());
        secondary(&mut outside, pos);
        assert!(outside.sees("Footnote"), "the menu is up");
        assert!(!outside.sees("Cite…"));
    }

    #[test]
    fn citing_over_a_selection_keeps_the_words_and_one_undo_takes_only_the_key() {
        for caret in [10, 5] {
            let mut page = page_in_notebook("keep these words here");
            select(&mut page, 5, 10, caret);
            cite_chord(&mut page);
            assert!(page.app.refs.cite_search.is_some());

            press_key(&mut page, egui::Key::Enter);

            assert_eq!(only_text(&page), "keep these[knuth1981] words here");
            assert_eq!(page.app.session.selection, None);
            assert!(page.app.session.undo());
            assert_eq!(
                only_text(&page),
                "keep these words here",
                "caret at {caret}"
            );
        }
    }

    #[test]
    fn add_and_cite_over_a_selection_keeps_the_words_too() {
        let mut page = page_in_notebook("keep these words here");
        select(&mut page, 5, 10, 10);
        cite_chord(&mut page);
        type_into(&mut page, "lamport1994");
        page.frame(Vec::new());
        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());
        page.app.refs.form.as_mut().unwrap().form.reference.title = "T".to_string();
        page.frame(Vec::new());

        page.press("Add and cite");

        assert_eq!(only_text(&page), "keep these[lamport1994] words here");
        assert!(page.app.session.undo());
        assert_eq!(only_text(&page), "keep these words here");
    }

    #[test]
    fn enter_in_a_link_prompt_doesnt_choose_from_the_search_behind_it() {
        let mut page = page_in_notebook("hello ");
        select(&mut page, 0, 5, 5);
        cite_chord(&mut page);
        ctrl_k(&mut page);
        assert!(page.app.link_prompt.is_some());

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.link_prompt.is_none());
        assert!(page.app.refs.cite_search.is_some());
        assert_eq!(only_text(&page), "hello ");
    }

    #[test]
    fn a_key_that_cant_be_put_in_the_document_says_so() {
        let mut page = page_in_notebook("hello ");
        cite_chord(&mut page);
        page.app.session.save_failed = true;
        page.app.session.last_error = Some("the disk is full".to_string());

        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "hello ");
        let said = page.app.session.last_error.as_deref().unwrap();
        assert!(said.contains("knuth1981"), "{said}");
        assert!(said.contains("the disk is full"), "{said}");
    }

    #[test]
    fn typing_after_a_cited_key_is_an_undo_step_of_its_own() {
        let command = egui::Modifiers {
            command: true,
            ..egui::Modifiers::NONE
        };
        let command_shift = egui::Modifiers {
            shift: true,
            ..command
        };
        let mut page = page_in_notebook("hello ");
        cite_chord(&mut page);
        press_key(&mut page, egui::Key::Enter);
        assert_eq!(only_text(&page), "hello [knuth1981]");

        type_into(&mut page, ", xy");
        assert_eq!(only_text(&page), "hello [knuth1981], xy");

        // The typing is a word and the spaces after it, and then the next word.
        key(&mut page, egui::Key::Z, command);
        assert_eq!(only_text(&page), "hello [knuth1981], ");
        key(&mut page, egui::Key::Z, command);
        assert_eq!(only_text(&page), "hello [knuth1981]");
        assert_eq!(page.app.session.caret.offset, "hello [knuth1981]".len());
        key(&mut page, egui::Key::Z, command);
        assert_eq!(only_text(&page), "hello ");
        assert_eq!(page.app.session.caret.offset, "hello ".len());

        key(&mut page, egui::Key::Z, command_shift);
        assert_eq!(only_text(&page), "hello [knuth1981]");
        assert_eq!(page.app.session.caret.offset, "hello [knuth1981]".len());
        key(&mut page, egui::Key::Z, command_shift);
        assert_eq!(only_text(&page), "hello [knuth1981], ");
        key(&mut page, egui::Key::Z, command_shift);
        assert_eq!(only_text(&page), "hello [knuth1981], xy");
    }

    #[test]
    fn a_key_that_went_in_but_couldnt_be_saved_says_that() {
        let mut page = page_in_notebook("hello ");
        // Something else takes the file's place, which is noticed when the key is written.
        let path = root_of(&page).join("doc.kame");
        let beside = root_of(&page).join("replacement");
        std::fs::write(&beside, b"another file").unwrap();
        std::fs::rename(&beside, &path).unwrap();

        page.app.cite("knuth1981");

        assert_eq!(only_text(&page), "hello [knuth1981]");
        assert!(page.app.session.save_failed);
        let said = page.app.session.last_error.as_deref().unwrap();
        assert!(!said.contains("could not cite"), "{said}");
        assert!(
            said.contains("knuth1981 is in the document but couldn't be saved"),
            "{said}"
        );
        assert!(said.contains("doc.kame changed on disk"), "{said}");
    }

    #[test]
    fn a_key_chosen_on_the_same_text_in_code_is_still_put_in() {
        let mut page = page_in_notebook("[knuth1981]");
        let block_id = page.app.session.document().blocks[0].id;
        page.app
            .session
            .set_block_kind(block_id, BlockKind::Code { language: None });
        cite_chord(&mut page);
        assert!(page.app.refs.cite_search.is_some());

        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "[knuth1981][knuth1981]");
    }

    #[test]
    fn escape_closes_a_prompt_over_the_search_and_leaves_the_search() {
        let mut page = page_in_notebook("hello ");
        select(&mut page, 0, 5, 5);
        cite_chord(&mut page);
        ctrl_k(&mut page);

        press_key(&mut page, egui::Key::Escape);

        assert!(page.app.link_prompt.is_none());
        assert!(page.app.refs.cite_search.is_some());
    }
}
