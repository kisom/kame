//! The browser's section for the references, below its folders, and what choosing from it does.

use eframe::egui;
use kame_core::references::form::ReferenceForm;
use kame_core::references::References;

use crate::notebook::BROWSER_INDENT;
use crate::KameApp;

use super::list::Confirming;

/// The browser's row for the references, kept open or closed along with its folders. A folder's
/// name is relative to the notebook, so none can be this.
pub(crate) const BROWSER_SECTION: &str = "/references";

/// What choosing something among the browser's references asks for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BrowserReference {
    New,
    Edit(String),
    Rename(String),
    Delete(String),
}

impl KameApp {
    /// What was chosen among the browser's references. The browser can be clicked past a dialog,
    /// so this leaves one alone and says so, as what's typed in a form would be lost. Over the
    /// References… window it does what that window's own buttons do.
    pub(crate) fn browser_reference(&mut self, chosen: BrowserReference) {
        if self.other_dialog_open()
            || self.close_flow.prompting
            || self.refs.form.is_some()
            || self.refs.cite_search.is_some()
            || self.refs.renaming.is_some()
        {
            self.notice_tracker.announced();
            self.notice = Some("Close what's open, then choose that again.".to_string());
            return;
        }
        self.refs.shown = None;
        match chosen {
            BrowserReference::New => self.refs.open_form_to_add(),
            BrowserReference::Edit(key) => {
                if let Some(reference) = self.refs.list.get(&key) {
                    let form = ReferenceForm::editing(&key, reference);
                    self.refs.open_form(form);
                }
            }
            BrowserReference::Rename(key) => self.ask_in_references(Confirming::Rename {
                key,
                new: String::new(),
                focus: true,
            }),
            BrowserReference::Delete(key) => self.ask_in_references(Confirming::Delete { key }),
        }
    }
}

/// The browser's section for the references, below its folders: a row that opens as a folder does,
/// with a row for each key and one for adding another. Returns whether that row was clicked, and
/// what was chosen from the rest.
pub(crate) fn draw_browser_references(
    ui: &mut egui::Ui,
    references: &References,
    open: bool,
) -> (bool, Option<BrowserReference>) {
    let mark = if open { "▾" } else { "▸" };
    let toggled = ui
        .selectable_label(false, format!("{mark} References"))
        .clicked();
    if !open {
        return (toggled, None);
    }
    let mut chosen = None;
    let indented = |ui: &mut egui::Ui, label: &str| {
        ui.horizontal(|ui| {
            ui.add_space(BROWSER_INDENT);
            ui.selectable_label(false, label)
        })
        .inner
    };
    for (key, _) in references.sorted() {
        let row = indented(ui, key);
        if row.clicked() {
            chosen = Some(BrowserReference::Edit(key.to_string()));
        }
        row.context_menu(|ui| {
            ui.set_min_width(120.0);
            if ui.button("Rename…").clicked() {
                chosen = Some(BrowserReference::Rename(key.to_string()));
                ui.close();
            } else if ui.button("Delete…").clicked() {
                chosen = Some(BrowserReference::Delete(key.to_string()));
                ui.close();
            }
        });
    }
    if indented(ui, "New reference…").clicked() {
        chosen = Some(BrowserReference::New);
    }
    (toggled, chosen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::rename::Renaming;
    use crate::references::test_support::*;
    use crate::Headless;
    use kame_core::session::Session;
    use std::path::Path;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[test]
    fn the_browser_keeps_its_references_under_a_row_that_opens() {
        let mut page = page_in_notebook("");
        page.app.nb.browser_open = true;
        page.frame(Vec::new());
        assert!(page.can_press("▸ References"));
        assert!(!page.sees("knuth1981"), "closed as a folder starts");

        page.press("▸ References");
        page.frame(Vec::new());

        assert!(page.can_press("▾ References"));
        assert!(page.can_press("knuth1981"));
        assert!(page.can_press("New reference…"));

        page.press("▾ References");
        page.frame(Vec::new());
        assert!(!page.sees("knuth1981"));
    }

    #[test]
    fn a_browser_with_no_references_can_still_add_one() {
        let root = notebook_folder("no-references");
        let session = Session::open(&root.join("doc.kame")).unwrap();
        let mut page = Headless::new(session);
        page.app.nb.browser_open = true;
        page.frame(Vec::new());

        page.press("▸ References");
        page.frame(Vec::new());

        assert!(page.can_press("New reference…"));
    }

    #[test]
    fn the_browser_remembers_its_references_as_it_does_a_folder() {
        let page = page_with_browser("");
        assert!(page.app.nb.browser_folders.contains(BROWSER_SECTION));
        // A folder's name is relative to the notebook, so none can be this.
        assert!(BROWSER_SECTION.starts_with('/'));
    }

    #[test]
    fn choosing_a_reference_in_the_browser_opens_it_to_edit() {
        let mut page = page_with_browser("hello");

        page.press("knuth1981");
        page.frame(Vec::new());

        assert!(page.sees("Edit reference"));
        assert!(page.sees("Breaking paragraphs into lines"));
        page.app.refs.form.as_mut().unwrap().form.reference.title = "Changed".to_string();
        page.press("Save");

        assert_eq!(
            page.app.refs.list.get("knuth1981").unwrap().title,
            "Changed"
        );
        assert_eq!(only_text(&page), "hello");
    }

    #[test]
    fn adding_from_the_browser_leaves_the_document_alone() {
        let mut page = page_with_browser("hello ");
        let history = page.app.session.history();

        page.press("New reference…");
        fill_form(&mut page, "lamport1994");
        assert!(page.can_press("Add"));
        assert!(!page.can_press("Add and cite"));
        page.press("Add");

        assert!(page.app.refs.form.is_none());
        assert!(page.app.refs.list.contains("lamport1994"), "listed at once");
        assert!(reference_file(&page, "lamport1994").exists());
        assert_eq!(only_text(&page), "hello ");
        assert_eq!(page.app.session.history(), history, "no edit was made");
        page.frame(Vec::new());
        assert!(page.can_press("lamport1994"), "and in the browser");
    }

    #[test]
    fn enter_adds_from_the_browser_without_citing_too() {
        let mut page = page_with_browser("hello ");

        page.press("New reference…");
        fill_form(&mut page, "lamport1994");
        press_key(&mut page, egui::Key::Enter);

        assert!(reference_file(&page, "lamport1994").exists());
        assert_eq!(only_text(&page), "hello ");
    }

    #[test]
    fn a_failed_add_from_the_browser_can_still_be_added_without_citing() {
        let mut page = page_with_browser("hello ");
        let folder = root_of(&page).join("references");
        page.press("New reference…");
        fill_form(&mut page, "lamport1994");

        std::fs::remove_dir_all(&folder).unwrap();
        std::fs::write(&folder, "a file in the way").unwrap();
        page.press("Add");
        assert!(page.app.refs.form.is_some(), "stays up to say why");

        std::fs::remove_file(&folder).unwrap();
        std::fs::create_dir(&folder).unwrap();
        page.frame(Vec::new());
        assert!(page.can_press("Add"));
        page.press("Add");

        assert!(reference_file(&page, "lamport1994").exists());
        assert_eq!(only_text(&page), "hello ");
    }

    #[test]
    fn citing_still_adds_and_cites_once_the_browser_has_added() {
        let mut page = page_with_browser("hello ");
        page.press("New reference…");
        fill_form(&mut page, "lamport1994");
        page.press("Add");

        cite_chord(&mut page);
        type_into(&mut page, "plass");
        page.frame(Vec::new());
        page.press("New reference “plass”…");
        fill_form(&mut page, "plass1981");
        assert!(page.can_press("Add and cite"));
        page.press("Add and cite");

        assert_eq!(only_text(&page), "hello [plass1981]");
    }

    #[test]
    fn deleting_from_the_browser_asks_first() {
        let mut page = page_with_browser("[knuth1981]");

        page.right_click("knuth1981");
        page.press("Delete…");
        run_until(&mut page, |app| app.refs.counts.is_some());
        page.frame(Vec::new());

        assert!(page.sees("1 document cites it."));
        assert!(reference_file(&page, "knuth1981").exists(), "only asked");

        page.press("Delete knuth1981");

        assert!(!reference_file(&page, "knuth1981").exists());
        assert_eq!(only_text(&page), "[knuth1981]");
    }

    #[test]
    fn renaming_from_the_browser_asks_for_the_new_key() {
        let mut page = page_with_browser("see [knuth1981]");

        page.right_click("knuth1981");
        page.press("Rename…");
        run_until(&mut page, |app| app.refs.counts.is_some());
        page.frame(Vec::new());
        page.frame(Vec::new());
        type_into(&mut page, "plass1981");
        page.frame(Vec::new());

        page.press("Rename knuth1981");
        wait_for_rename(&mut page);

        assert_eq!(only_text(&page), "see [plass1981]");
        assert!(reference_file(&page, "plass1981").exists());
    }

    #[test]
    fn a_confirmation_asked_from_the_browser_scrolls_into_view() {
        let mut page = page_in_notebook("");
        let root = root_of(&page);
        for at in 0..40 {
            write_reference(&root, &format!("zed{at:02}"), GOOD);
        }
        page.app.reload_references();
        page.app.nb.browser_open = true;
        page.frame(Vec::new());
        page.app
            .browser_reference(BrowserReference::Delete("zed39".to_string()));
        run_until(&mut page, |app| app.refs.counts.is_some());
        for _ in 0..4 {
            page.frame(Vec::new());
        }

        let (_, bottom) = page.drawn_at("Delete zed39").expect("the button is drawn");
        let (close_top, _) = page.drawn_at("Close").expect("the window's button");
        assert!(
            bottom <= close_top,
            "{bottom} is below the list, at {close_top}"
        );
    }

    #[test]
    fn the_browser_opens_the_form_over_the_references_window() {
        let mut page = page_with_browser("");
        page.app.open_references();
        run_until(&mut page, |app| app.refs.counts.is_some());

        page.app
            .browser_reference(BrowserReference::Edit("knuth1981".to_string()));
        page.frame(Vec::new());
        page.frame(Vec::new());

        assert!(page.sees("Edit reference"));
        press_key(&mut page, egui::Key::Escape);
        assert!(page.app.refs.form.is_none());
        assert!(page.app.refs.window.is_some(), "back to the list");
    }

    #[test]
    fn the_browser_doesnt_take_away_a_form_being_typed_in() {
        let mut page = page_with_browser("");
        page.press("New reference…");
        fill_form(&mut page, "lamport1994");

        page.press("knuth1981");

        let form = &page.app.refs.form.as_ref().expect("still up").form;
        assert_eq!(form.key, "lamport1994");
        assert!(form.is_new());
        assert_eq!(
            page.app.notice.as_deref(),
            Some("Close what's open, then choose that again.")
        );
    }

    #[test]
    fn the_browser_waits_for_a_rename_under_way() {
        let mut page = page_with_browser("");
        page.app.open_references();
        let (_send, coming) = channel();
        let notebook = page.app.nb.notebook.clone().unwrap();
        page.app.refs.renaming = Some(Renaming {
            coming,
            notebook,
            open: None,
            old: "knuth1981".to_string(),
            new: "plass1981".to_string(),
        });

        page.app.browser_reference(BrowserReference::New);

        assert!(page.app.refs.form.is_none());
        assert_eq!(
            page.app.notice.as_deref(),
            Some("Close what's open, then choose that again.")
        );
    }

    #[test]
    fn the_browser_waits_behind_another_dialog() {
        let mut page = page_with_browser("");
        page.app.nb.move_report = Some(Default::default());

        page.app
            .browser_reference(BrowserReference::Delete("knuth1981".to_string()));

        assert!(page.app.refs.window.is_none());
    }

    #[test]
    fn a_document_row_in_the_browser_doesnt_take_away_a_form_being_typed_in() {
        let mut page = page_with_browser("");
        other_document(&page, "other.kame", "elsewhere");
        page.app.nb.refresh_listing();
        wait_for_row(&mut page, "other.kame");
        page.press("New reference…");
        fill_form(&mut page, "lamport1994");
        let opened = page.app.session.path().map(Path::to_path_buf);

        page.press("other.kame");

        assert_eq!(page.app.session.path().map(Path::to_path_buf), opened);
        let form = &page.app.refs.form.as_ref().expect("still up").form;
        assert_eq!(form.key, "lamport1994");
        assert_eq!(
            page.app.notice.as_deref(),
            Some("finish or close the open dialog first")
        );
    }

    #[test]
    fn a_paper_opens_in_the_system_viewer_with_a_form_up() {
        let (mut page, opened) = page_with_form();
        let paper = root_of(&page).join("paper.pdf");
        std::fs::write(&paper, "").unwrap();
        page.app.nb.refresh_listing();
        wait_for_row(&mut page, "paper.pdf");
        page.press("paper.pdf");

        assert_eq!(
            crate::notebook::opened_in_system_viewer(),
            vec![paper.clone()]
        );
        assert_eq!(page.app.session.path().map(Path::to_path_buf), opened);
        assert_eq!(page.app.refs.form.as_ref().unwrap().form.key, "lamport1994");
        assert_eq!(page.app.notice, None);
    }

    /// Runs frames until the browser has a row saying `label`, as its listing comes from a
    /// background thread.
    fn wait_for_row(page: &mut Headless, label: &str) {
        for _ in 0..2000 {
            page.frame(Vec::new());
            if page.can_press(label) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the browser never listed {label:?}");
    }

    #[test]
    fn the_browser_doesnt_take_away_a_search_being_typed_in() {
        let mut page = page_with_browser("");
        cite_chord(&mut page);
        type_into(&mut page, "knu");

        page.app.browser_reference(BrowserReference::New);

        assert!(page.app.refs.form.is_none());
        assert_eq!(page.app.refs.cite_search.as_ref().unwrap().query, "knu");
    }

    #[test]
    fn the_browser_waits_for_the_close_prompt() {
        let mut page = page_with_browser("");
        page.app.close_flow.prompting = true;

        page.app.browser_reference(BrowserReference::New);

        assert!(page.app.refs.form.is_none());
    }

    #[test]
    fn asking_from_the_browser_counts_again() {
        let mut page = page_with_browser("");
        open_list(&mut page);
        page.press("Close");
        other_document(&page, "other.kame", "see [knuth1981]");

        page.app
            .browser_reference(BrowserReference::Delete("knuth1981".to_string()));
        page.frame(Vec::new());
        run_until(&mut page, |app| app.refs.counts.is_some());
        page.frame(Vec::new());

        assert!(page.sees("1 document cites it."));
    }
}
