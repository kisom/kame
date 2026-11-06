//! The References… window: every reference with how many documents cite it, and the rows
//! that ask before a delete or a rename.

use crate::interface::room;
use std::sync::mpsc::channel;

use eframe::egui;
use egui::{Align2, Id, Vec2};
use kame_core::references::form::{ReferenceForm, NOT_A_KEY};
use kame_core::references::{plain, valid_key, Reference};
use kame_notebook::references::{citing, remove};

use crate::notebook::focus_field;
use crate::KameApp;

use super::enter_pressed;

/// What the References… window is asking and has to say.
#[derive(Default)]
pub(super) struct ListWindow {
    pub(super) confirming: Option<Confirming>,
    /// Whether what's being asked has yet to be scrolled to, having been asked from elsewhere.
    reveal: bool,
    /// How the last delete or rename went.
    pub(super) said: Vec<String>,
}

/// A confirmation row, under the key it's about.
pub(super) enum Confirming {
    Delete {
        key: String,
    },
    Rename {
        key: String,
        new: String,
        focus: bool,
    },
}

impl Confirming {
    /// The key it's about.
    fn key(&self) -> &str {
        match self {
            Confirming::Delete { key } | Confirming::Rename { key, .. } => key,
        }
    }
}

/// What pressing something in the References… window asks for.
enum ListAction {
    Close,
    New,
    Edit(String),
    AskToDelete(String),
    AskToRename(String),
    Cancel,
    Delete(String),
    Rename(String, String),
}

/// What's known of how many documents cite a key.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Citing {
    Counting,
    /// The count stopped, so the windows go on without it.
    NotCounted,
    Documents(usize),
}

/// A count of documents, in words.
pub(super) fn documents(count: usize) -> String {
    match count {
        1 => "1 document".to_string(),
        _ => format!("{count} documents"),
    }
}

/// What a confirmation row says of how many documents cite a key.
fn cited_by(count: usize) -> String {
    match count {
        0 => "No document cites it.".to_string(),
        1 => "1 document cites it.".to_string(),
        _ => format!("{count} documents cite it."),
    }
}

/// What a confirmation row says before deleting `key`.
fn delete_warning(key: &str, citing: Citing) -> String {
    match citing {
        Citing::Counting => "Counting the documents that cite it…".to_string(),
        Citing::NotCounted => {
            format!("Its documents couldn't be counted. Any would then hold plain [{key}] text.")
        }
        Citing::Documents(0) => cited_by(0),
        Citing::Documents(1) => {
            format!("{} It would then hold plain [{key}] text.", cited_by(1))
        }
        Citing::Documents(count) => {
            format!(
                "{} They would then hold plain [{key}] text.",
                cited_by(count)
            )
        }
    }
}

/// What a confirmation row says before renaming a key.
fn rename_warning(citing: Citing) -> String {
    match citing {
        Citing::Counting => "Counting the documents that cite it…".to_string(),
        Citing::NotCounted => {
            "Its documents couldn't be counted. Any found are rewritten as an ordinary edit."
                .to_string()
        }
        Citing::Documents(0) => cited_by(0),
        Citing::Documents(1) => format!("{} It's rewritten as an ordinary edit.", cited_by(1)),
        Citing::Documents(count) => {
            format!("{} Each is rewritten as an ordinary edit.", cited_by(count))
        }
    }
}

impl KameApp {
    /// Annotate > References… Over another dialog it says so, as a menu row that did nothing would
    /// look broken.
    pub(crate) fn open_references(&mut self) {
        if self.nb.notebook.is_none() || self.refs.window.is_some() {
            return;
        }
        if self.dialog_open() || self.close_flow.prompting {
            self.notice_tracker.announced();
            self.notice = Some("Close what's open, then choose References… again.".to_string());
            return;
        }
        self.refs.window = Some(ListWindow::default());
        self.refs.recount = true;
    }

    /// Asks in the References… window, opening it if it isn't up.
    pub(super) fn ask_in_references(&mut self, confirming: Confirming) {
        if self.refs.window.is_none() {
            self.refs.recount = true;
        }
        let window = self.refs.window.get_or_insert_with(ListWindow::default);
        window.said.clear();
        window.confirming = Some(confirming);
        window.reveal = true;
    }

    fn start_counting(&mut self) {
        let Some(notebook) = self.nb.notebook.clone() else {
            return;
        };
        let (send, coming) = channel();
        self.refs.counts = None;
        self.refs.count_stopped = false;
        // Dropping an older one leaves it nowhere to send what it finds.
        self.refs.counting = Some(coming);
        std::thread::spawn(move || {
            let _ = send.send(citing(&notebook));
        });
    }

    /// The References… window: every reference, with what can be done to it.
    pub(super) fn show_references_list(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.refs.window.take() else {
            return;
        };
        if std::mem::take(&mut self.refs.recount) {
            self.start_counting();
        }
        let renaming = self.refs.renaming.is_some();
        let busy = renaming || self.refs.form.is_some();
        // The form on top of this has the keyboard.
        let enter = enter_pressed(ctx) && !busy && !self.keys_are_elsewhere(ctx);
        let in_notebook = self.refs.root.is_some();
        let count_stopped = self.refs.count_stopped;
        let error_ink = self.palette.error_ink;
        let (references, counts, problems) =
            (&self.refs.list, &self.refs.counts, &self.refs.problems);
        let mut action = None;
        egui::Window::new("References")
            .id(Id::unique("kame-references"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(room(ctx, 560.0))
            .show(ctx, |ui| {
                for problem in problems {
                    ui.colored_label(error_ink, problem);
                }
                if references.is_empty() && in_notebook {
                    ui.weak("No references yet.");
                }
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(room(ui.ctx(), 360.0))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (key, reference) in references.sorted() {
                                let citing = match counts {
                                    Some(counts) => Citing::Documents(
                                        counts.get(key).copied().unwrap_or_default(),
                                    ),
                                    None if count_stopped => Citing::NotCounted,
                                    None => Citing::Counting,
                                };
                                draw_list_row(ui, key, reference, citing, &mut action);
                                if let Some(confirming) = &mut window.confirming {
                                    draw_confirming(
                                        ui,
                                        key,
                                        confirming,
                                        citing,
                                        enter,
                                        &mut action,
                                    );
                                    // A window's first frame only measures it, and scrolls nothing.
                                    if window.reveal
                                        && confirming.key() == key
                                        && !ui.is_sizing_pass()
                                    {
                                        ui.scroll_to_cursor_animation(
                                            Some(egui::Align::BOTTOM),
                                            egui::style::ScrollAnimation::none(),
                                        );
                                        window.reveal = false;
                                    }
                                }
                                ui.separator();
                            }
                        });
                });
                if renaming {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Renaming…");
                    });
                }
                for line in &window.said {
                    ui.label(line);
                }
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!renaming, |ui| {
                        if ui.button("Close").clicked() {
                            action = Some(ListAction::Close);
                        }
                    });
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui.button("New reference…").clicked() {
                            action = Some(ListAction::New);
                        }
                    });
                });
            });
        match action {
            Some(ListAction::Close) => return,
            Some(ListAction::New) => {
                window.confirming = None;
                self.refs.open_form_to_add();
            }
            Some(ListAction::Edit(key)) => {
                window.confirming = None;
                if let Some(reference) = self.refs.list.get(&key) {
                    let form = ReferenceForm::editing(&key, reference);
                    self.refs.open_form(form);
                }
            }
            Some(ListAction::AskToDelete(key)) => {
                window.said.clear();
                window.confirming = Some(Confirming::Delete { key });
            }
            Some(ListAction::AskToRename(key)) => {
                window.said.clear();
                window.confirming = Some(Confirming::Rename {
                    new: String::new(),
                    key,
                    focus: true,
                });
            }
            Some(ListAction::Cancel) => window.confirming = None,
            Some(ListAction::Delete(key)) => {
                window.confirming = None;
                if let Some(notebook) = &self.nb.notebook {
                    window.said = vec![match remove(notebook, &key) {
                        Ok(()) => format!(
                            "Deleted {key}. Documents citing it now hold plain [{key}] text."
                        ),
                        Err(why) => why,
                    }];
                }
                self.reload_references();
            }
            Some(ListAction::Rename(old, new)) => {
                window.confirming = None;
                self.start_rename(old, new);
            }
            None => {}
        }
        self.refs.window = Some(window);
    }
}

/// A reference in the References… window.
fn draw_list_row(
    ui: &mut egui::Ui,
    key: &str,
    reference: &Reference,
    citing: Citing,
    action: &mut Option<ListAction>,
) {
    ui.horizontal(|ui| {
        ui.strong(key);
        ui.weak(match citing {
            Citing::Counting => "counting…".to_string(),
            Citing::NotCounted => "not counted".to_string(),
            Citing::Documents(count) => documents(count),
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Delete…").clicked() {
                *action = Some(ListAction::AskToDelete(key.to_string()));
            }
            if ui.small_button("Rename…").clicked() {
                *action = Some(ListAction::AskToRename(key.to_string()));
            }
            if ui.small_button("Edit").clicked() {
                *action = Some(ListAction::Edit(key.to_string()));
            }
        });
    });
    ui.weak(plain(reference));
}

/// The confirmation row, if it's about `key`. Nothing can be confirmed while the count it has to
/// say is coming.
fn draw_confirming(
    ui: &mut egui::Ui,
    key: &str,
    confirming: &mut Confirming,
    citing: Citing,
    enter: bool,
    action: &mut Option<ListAction>,
) {
    let counted = citing != Citing::Counting;
    match confirming {
        Confirming::Delete { key: asked } if asked == key => {
            ui.label(delete_warning(key, citing));
            ui.horizontal(|ui| {
                let delete = egui::Button::new(format!("Delete {key}"));
                if ui.add_enabled(counted, delete).clicked() {
                    *action = Some(ListAction::Delete(key.to_string()));
                }
                if ui.button("Cancel").clicked() {
                    *action = Some(ListAction::Cancel);
                }
            });
        }
        Confirming::Rename {
            key: asked,
            new,
            focus,
        } if asked == key => {
            ui.label(rename_warning(citing));
            let ready = counted && valid_key(new) && new != key;
            let opening = *focus;
            ui.horizontal(|ui| {
                ui.label("New key");
                let field = ui.add(
                    egui::TextEdit::singleline(new)
                        .hint_text(key)
                        .return_key(None)
                        .desired_width(room(ui.ctx(), 200.0)),
                );
                focus_field(&field, focus);
                let button = egui::Button::new(format!("Rename {key}"));
                let pressed = ui.add_enabled(ready, button).clicked();
                if pressed || (enter && ready && !opening) {
                    *action = Some(ListAction::Rename(key.to_string(), new.clone()));
                }
                if ui.button("Cancel").clicked() {
                    *action = Some(ListAction::Cancel);
                }
            });
            if new.is_empty() || new == key {
                ui.weak("Type the new key.");
            } else if !valid_key(new) {
                ui.weak(NOT_A_KEY);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::test_support::*;
    use crate::Headless;
    use kame_core::session::Session;

    #[test]
    fn how_many_documents_cite_a_key_is_said_in_words() {
        assert_eq!(cited_by(0), "No document cites it.");
        assert_eq!(cited_by(1), "1 document cites it.");
        assert_eq!(cited_by(3), "3 documents cite it.");
    }

    #[test]
    fn help_being_up_leaves_a_rename_its_enter() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);
        ask_to_rename(&mut page, "plass1981");
        page.app.show_help = true;
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::Enter);
        wait_for_rename(&mut page);

        assert_eq!(only_text(&page), "see [plass1981]");
    }

    #[test]
    fn the_references_window_says_why_it_wont_open_over_a_dialog() {
        let mut page = page_in_notebook("hello");
        page.app.toggle_jump();
        page.frame(Vec::new());

        page.app.open_references();

        assert!(page.app.refs.window.is_none());
        let notice = page.app.notice.clone().expect("it says so");
        assert!(notice.contains("References"), "{notice}");

        page.app.nb.close_lists();
        page.frame(Vec::new());
        page.app.notice = None;
        page.app.open_references();
        assert!(page.app.refs.window.is_some());
        assert_eq!(page.app.notice, None);

        // Asking again with it up is no complaint.
        page.app.open_references();
        assert_eq!(page.app.notice, None);
    }

    #[test]
    fn the_list_shows_counts_and_load_problems() {
        let mut page = page_in_notebook("[knuth1981] and [knuth1981]");
        let root = root_of(&page);
        other_document(&page, "other.kame", "also [knuth1981]");
        write_reference(&root, "smith2020", GOOD);
        write_reference(&root, "broken2020", "kind: nonsense\n");
        page.app.reload_references();

        open_list(&mut page);

        assert_eq!(page.app.refs.counts.as_ref().unwrap()["knuth1981"], 2);
        assert!(page.sees("2 documents"));
        assert!(page.sees("0 documents"), "smith2020");
        assert!(page.sees("broken2020"), "the window says what's wrong too");
    }

    #[test]
    fn a_new_file_while_the_list_is_up_is_counted_again() {
        let mut page = page_in_notebook("[smith2020]");
        open_list(&mut page);
        assert!(!page
            .app
            .refs
            .counts
            .as_ref()
            .unwrap()
            .contains_key("smith2020"));

        write_reference(&root_of(&page), "smith2020", GOOD);
        page.app.refs.checked = None;
        run_until(&mut page, |app| {
            app.refs
                .counts
                .as_ref()
                .is_some_and(|counts| counts.contains_key("smith2020"))
        });
    }

    #[test]
    fn deleting_says_how_many_cite_it_and_shows_at_once() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);

        page.press("Delete…");
        page.frame(Vec::new());
        assert!(page.sees("1 document cites it."));
        assert!(reference_file(&page, "knuth1981").exists(), "only asked");

        page.press("Delete knuth1981");

        assert!(!page.app.refs.list.contains("knuth1981"), "at once");
        assert!(!reference_file(&page, "knuth1981").exists());
        assert_eq!(only_text(&page), "[knuth1981]");
        page.frame(Vec::new());
        assert!(page.sees("Deleted knuth1981"));
    }

    #[test]
    fn cancelling_a_confirmation_changes_nothing() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);

        page.press("Delete…");
        page.frame(Vec::new());
        page.press("Cancel");
        page.frame(Vec::new());

        assert!(!page.can_press("Delete knuth1981"));
        assert!(page.app.refs.list.contains("knuth1981"));
    }

    #[test]
    fn nothing_can_be_confirmed_before_the_count_is_known() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);
        page.press("Delete…");
        page.frame(Vec::new());
        let (_keep_it_waiting, coming) = channel();
        page.app.refs.counts = None;
        page.app.refs.counting = Some(coming);
        page.frame(Vec::new());
        page.frame(Vec::new());

        assert!(page.sees("Counting the documents that cite it…"));
        assert!(!page.can_press("Delete knuth1981"));
        assert!(page.sees("Delete knuth1981"));
    }

    #[test]
    fn the_rename_field_starts_empty_and_holds_only_whats_typed() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);
        let field = |page: &Headless| match &page.app.refs.window.as_ref().unwrap().confirming {
            Some(Confirming::Rename { new, .. }) => new.clone(),
            _ => panic!("not asking to rename"),
        };

        page.press("Rename…");
        page.frame(Vec::new());
        page.frame(Vec::new());
        assert_eq!(field(&page), "");
        assert!(!page.can_press("Rename knuth1981"));
        assert!(page.sees("Type the new key."));
        assert!(!page.sees("A key is letters"), "nothing wrong's been typed");

        type_into(&mut page, "plass1981");
        page.frame(Vec::new());
        assert_eq!(field(&page), "plass1981");
        assert!(page.can_press("Rename knuth1981"));
    }

    #[test]
    fn enter_in_the_form_doesnt_confirm_a_rename_behind_it() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);
        ask_to_rename(&mut page, "plass1981");

        let reference = page.app.refs.list.get("knuth1981").unwrap().clone();
        page.app
            .refs
            .open_form(ReferenceForm::editing("knuth1981", &reference));
        page.frame(Vec::new());
        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.refs.renaming.is_none());
        assert!(reference_file(&page, "knuth1981").exists());
    }

    #[test]
    fn the_rename_button_renames_as_enter_does() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);
        ask_to_rename(&mut page, "plass1981");

        page.press("Rename knuth1981");
        wait_for_rename(&mut page);

        assert_eq!(only_text(&page), "see [plass1981]");
    }

    #[test]
    fn a_rename_to_nothing_new_cant_be_confirmed() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);

        ask_to_rename(&mut page, "knuth1981");
        assert!(!page.can_press("Rename knuth1981"));

        ask_to_rename(&mut page, "not a key");
        assert!(!page.can_press("Rename knuth1981"));
        press_key(&mut page, egui::Key::Enter);
        assert!(page.app.refs.renaming.is_none());
    }

    #[test]
    fn enter_in_a_link_prompt_doesnt_start_the_rename_behind_it() {
        let mut page = page_in_notebook("hello [knuth1981]");
        select(&mut page, 0, 5, 5);
        open_list(&mut page);
        ask_to_rename(&mut page, "plass1981");
        ctrl_k(&mut page);
        assert!(page.app.link_prompt.is_some());

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.link_prompt.is_none(), "the prompt had it");
        assert!(page.app.refs.renaming.is_none());
        assert!(reference_file(&page, "knuth1981").exists());
        assert!(!reference_file(&page, "plass1981").exists());
    }

    #[test]
    fn a_rename_says_why_its_button_is_off() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);

        ask_to_rename(&mut page, "not a key");
        page.frame(Vec::new());
        assert!(!page.can_press("Rename knuth1981"));
        assert!(page.sees("A key is letters"));

        ask_to_rename(&mut page, "knuth1981");
        page.frame(Vec::new());
        assert!(!page.can_press("Rename knuth1981"));
        assert!(page.sees("Type the new key."));
    }

    #[test]
    fn a_confirmation_reads_right_with_nothing_citing_the_key() {
        let mut page = page_in_notebook("no citations here");
        open_list(&mut page);

        page.press("Delete…");
        page.frame(Vec::new());
        assert!(page.sees("No document cites it."));
        assert!(!page.sees("hold plain"));

        page.press("Cancel");
        ask_to_rename(&mut page, "plass1981");
        assert!(page.sees("No document cites it."));
        assert!(!page.sees("rewritten"));
    }

    #[test]
    fn a_confirmation_reads_right_with_one_document_citing_the_key() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);

        page.press("Delete…");
        page.frame(Vec::new());
        assert!(page.sees("1 document cites it. It would then hold plain [knuth1981] text."));
    }

    #[test]
    fn a_count_that_stopped_can_still_be_gone_ahead_with() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);
        page.press("Delete…");
        page.frame(Vec::new());

        let (send, coming) = channel();
        drop(send);
        page.app.refs.counts = None;
        page.app.refs.counting = Some(coming);
        page.frame(Vec::new());
        page.frame(Vec::new());

        assert!(page.sees("couldn't be counted"));
        assert!(page.can_press("Delete knuth1981"));
        page.press("Delete knuth1981");
        assert!(!reference_file(&page, "knuth1981").exists());
    }

    #[test]
    fn the_references_window_can_add_without_citing() {
        let mut page = page_in_notebook("hello ");
        open_list(&mut page);

        page.press("New reference…");
        fill_form(&mut page, "lamport1994");
        page.press("Add");

        assert!(page.app.refs.window.is_some(), "the list stays up");
        assert_eq!(only_text(&page), "hello ");
        page.frame(Vec::new());
        assert!(page.sees("lamport1994"), "and shows it");
    }

    #[test]
    fn an_empty_references_window_says_how_to_add_one() {
        let root = notebook_folder("empty-window");
        let session = Session::open(&root.join("doc.kame")).unwrap();
        let mut page = Headless::new(session);
        page.frame(Vec::new());
        open_list(&mut page);

        assert!(page.sees("No references yet."));
        assert!(!page.sees("Ctrl-Shift-C"), "the button is right there");
        assert!(page.can_press("New reference…"));
    }
}
