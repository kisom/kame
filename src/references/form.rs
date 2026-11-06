//! The new reference and edit windows, drawing a `ReferenceForm` and saving what it builds.

use crate::interface::room;
use eframe::egui;
use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Id, Order, Vec2};
use kame_core::references::form::{field_label, text_field, ReferenceForm, Tab};
use kame_core::references::{entry, Kind, References, RunStyle};
use kame_notebook::references::save;

use crate::KameApp;

use super::{enter_pressed, ReferencesUi};

/// The form in its window.
pub(super) struct FormWindow {
    pub(super) form: ReferenceForm,
    /// Why the last save didn't work.
    failed: Option<String>,
    /// Set for the frame a field should take the keyboard on.
    focus: bool,
    /// Whether a new reference is cited as it's added, or only added.
    cites: bool,
}

/// The entry `form` would save, as it would read, with the runs an export italicises in italics.
fn preview_job(form: &ReferenceForm, color: egui::Color32, font: egui::FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    for run in entry(&form.built()) {
        let format = TextFormat {
            font_id: font.clone(),
            color,
            italics: run.style == RunStyle::Italic,
            ..TextFormat::default()
        };
        job.append(&run.text, 0.0, format);
    }
    job
}

/// The freeform text box, which keeps Enter for itself.
fn freeform_text_id() -> Id {
    Id::unique("kame-reference-text")
}

impl ReferencesUi {
    pub(super) fn open_form(&mut self, form: ReferenceForm) {
        self.form = Some(FormWindow {
            form,
            failed: None,
            focus: true,
            cites: true,
        });
    }

    /// Opens the form for a new reference that's only added, with nothing put in the document.
    pub(super) fn open_form_to_add(&mut self) {
        self.open_form(ReferenceForm::new(None));
        if let Some(window) = &mut self.form {
            window.cites = false;
        }
    }
}

impl KameApp {
    /// The new reference and edit window.
    pub(super) fn show_reference_form(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.refs.form.take() else {
            return;
        };
        let focus = std::mem::take(&mut window.focus);
        // A chord and an Enter arriving in the same frame would otherwise save a form nobody has
        // read, and in the text box Enter is a new line.
        let in_text_box = ctx.memory(|memory| memory.has_focus(freeform_text_id()));
        let mut submit =
            enter_pressed(ctx) && !focus && !in_text_box && !self.keys_are_elsewhere(ctx);
        let mut cancel = false;
        let is_new = window.form.is_new();
        let cites = window.cites;
        let title = if is_new {
            "New reference"
        } else {
            "Edit reference"
        };
        // Under another dialog, a click here would bring the form over it, and so it takes none.
        let covered = self.other_dialog_open();
        let references = &self.refs.list;
        let error_ink = self.palette.error_ink;
        let shown = egui::Window::new(title)
            .id(Id::unique("kame-reference-form"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(room(ctx, 480.0))
            .order(Order::Foreground)
            .interactable(!covered)
            .show(ctx, |ui| {
                ui.add_enabled_ui(!covered, |ui| {
                    draw_form(ui, &mut window, focus, references);
                    let problem = window.form.problem(references);
                    if let Some(problem) = &problem {
                        ui.weak(problem);
                    }
                    if let Some(failed) = &window.failed {
                        ui.colored_label(error_ink, failed);
                    }
                    ui.horizontal(|ui| {
                        let save_button = match (is_new, cites) {
                            (true, true) => "Add and cite",
                            (true, false) => "Add",
                            (false, _) => "Save",
                        };
                        let button = egui::Button::new(save_button);
                        if ui.add_enabled(problem.is_none(), button).clicked() {
                            submit = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            });
        // A window that takes no clicks lets them through to the page, unless this catches them
        // there, under what's covering the form.
        if let Some(shown) = shown.filter(|_| covered) {
            let rect = shown.response.rect;
            egui::Area::new(Id::unique("kame-reference-form-covered"))
                .order(Order::Middle)
                .fixed_pos(rect.min)
                .constrain(false)
                .show(ctx, |ui| {
                    ui.allocate_response(rect.size(), egui::Sense::click_and_drag());
                });
        }
        if cancel {
            return;
        }
        if !submit || !window.form.can_save(&self.refs.list) {
            self.refs.form = Some(window);
            return;
        }
        let Some(notebook) = self.nb.notebook.clone() else {
            return;
        };
        let key = window.form.key.clone();
        if let Err(why) = save(&notebook, &key, &window.form.built()) {
            window.failed = Some(why);
            self.refs.form = Some(window);
            return;
        }
        self.reload_references();
        if is_new && cites {
            self.cite(&key);
        }
    }
}

/// The fields of the form, following the tab and the kind.
fn draw_form(ui: &mut egui::Ui, window: &mut FormWindow, focus: bool, references: &References) {
    let form = &mut window.form;
    ui.horizontal(|ui| {
        for (tab, label) in [(Tab::Structured, "Structured"), (Tab::Freeform, "Freeform")] {
            if ui.selectable_value(&mut form.tab, tab, label).changed() {
                window.focus = true;
            }
        }
    });
    if form.tab == Tab::Structured {
        ui.horizontal(|ui| {
            for (kind, label) in [
                (Kind::Article, "Article"),
                (Kind::Conference, "Conference"),
                (Kind::Book, "Book"),
                (Kind::Web, "Web"),
            ] {
                ui.selectable_value(&mut form.reference.kind, kind, label);
            }
        });
    }
    ui.separator();
    let mut changed = false;
    egui::Grid::new("kame-reference-fields")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Key");
            if form.is_new() {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut form.key)
                        .id_salt("kame-reference-key")
                        .return_key(None)
                        .desired_width(room(ui.ctx(), 360.0))
                        .hint_text("suggested from the author and year"),
                );
                if field.changed() {
                    form.key_edited = true;
                }
                if focus && form.tab == Tab::Freeform && form.key.is_empty() {
                    field.request_focus();
                }
            } else {
                ui.label(&form.key)
                    .on_hover_text("Rename it from Annotate > References…");
            }
            ui.end_row();
            if form.tab == Tab::Structured {
                let kind = form.reference.kind;
                for (index, name) in kind.fields().iter().enumerate() {
                    ui.label(field_label(name));
                    let value = match *name {
                        "authors" => Some(&mut form.authors_text),
                        "editors" => Some(&mut form.editors_text),
                        name => text_field(&mut form.reference, name),
                    };
                    let Some(value) = value else {
                        ui.end_row();
                        continue;
                    };
                    let mut edit = egui::TextEdit::singleline(value)
                        .id_salt(*name)
                        .return_key(None)
                        .desired_width(room(ui.ctx(), 360.0));
                    if matches!(*name, "authors" | "editors") {
                        edit = edit.hint_text("separated by commas, or ; if a name has a comma");
                    }
                    let field = ui.add(edit);
                    if focus && index == 0 {
                        field.request_focus();
                    }
                    changed |= field.changed();
                    ui.end_row();
                }
            }
        });
    if changed {
        form.suggest(references);
    }
    match form.tab {
        Tab::Structured => {
            ui.separator();
            let style = ui.style();
            let job = preview_job(
                form,
                style.visuals.weak_text_color(),
                egui::TextStyle::Body.resolve(style),
            );
            ui.label(job);
        }
        Tab::Freeform => {
            let field = ui.add(
                egui::TextEdit::multiline(&mut form.reference.text)
                    .id(freeform_text_id())
                    .desired_rows(6)
                    .desired_width(room(ui.ctx(), 440.0))
                    .hint_text("Goes into the References section exactly as typed"),
            );
            if focus && !(form.is_new() && form.key.is_empty()) {
                field.request_focus();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::test_support::*;
    use kame_core::model::AnnotationKind;
    use kame_core::references::plain;

    #[test]
    fn the_preview_sets_a_book_title_and_a_journal_name_in_italics() {
        for kind in [Kind::Book, Kind::Article] {
            let mut form = ReferenceForm::new(Some("k".to_string()));
            form.reference.kind = kind;
            form.reference.title = "A Title".to_string();
            form.reference.published_in = "A Journal".to_string();
            form.authors_text = "D. E. Knuth".to_string();

            let job = preview_job(&form, egui::Color32::GRAY, egui::FontId::default());

            let italic: Vec<&str> = job
                .sections
                .iter()
                .filter(|section| section.format.italics)
                .map(|section| &job.text[section.byte_range.start.0..section.byte_range.end.0])
                .collect();
            assert!(!italic.is_empty(), "{kind:?} has an italic section");
            assert!(job.sections.iter().any(|section| !section.format.italics));
            assert_eq!(job.text, plain(&form.built()));
        }
    }

    #[test]
    fn add_and_cite_saves_inserts_and_shows_at_once() {
        let mut page = page_in_notebook("hello ");
        cite_chord(&mut page);
        type_into(&mut page, "nothing like it");
        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());

        // The first field has the keyboard, and the key follows it.
        type_into(&mut page, "L. Lamport");
        {
            let form = &mut page.app.refs.form.as_mut().unwrap().form;
            form.reference.year = "1994".to_string();
            form.reference.title = "How to write".to_string();
        }
        type_into(&mut page, ", J. Doe");
        page.frame(Vec::new());
        assert_eq!(page.app.refs.form.as_ref().unwrap().form.key, "lamport1994");
        assert!(page.sees("How to write"), "the preview");
        assert_eq!(only_text(&page), "hello ", "typing stays in the form");

        page.press("Add and cite");

        assert!(page.app.refs.form.is_none());
        assert_eq!(only_text(&page), "hello [lamport1994]");
        let saved = page
            .app
            .refs
            .list
            .get("lamport1994")
            .expect("listed at once");
        assert_eq!(saved.authors, ["L. Lamport", "J. Doe"]);
        assert!(reference_file(&page, "lamport1994").exists());

        assert!(page.app.session.undo());
        assert_eq!(only_text(&page), "hello ");
        assert!(page.app.refs.list.contains("lamport1994"), "only the text");
    }

    #[test]
    fn a_form_with_a_problem_cant_be_saved() {
        let mut page = page_in_notebook("");
        cite_chord(&mut page);
        type_into(&mut page, "nothing like it");
        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());
        page.frame(Vec::new());
        assert!(!page.can_press("Add and cite"));
        assert!(page.sees("It needs a key."));

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.refs.form.is_some(), "it stays up");
    }

    #[test]
    fn a_failed_save_stays_in_the_window() {
        let mut page = page_in_notebook("[broken2020]");
        write_reference(&root_of(&page), "broken2020", "kind: nonsense\n");
        page.app.reload_references();
        cite_chord(&mut page);

        let window = page.app.refs.form.as_mut().expect("the form opens");
        window.form.reference.title = "T".to_string();
        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());

        let window = page.app.refs.form.as_ref().expect("still up");
        let failed = window.failed.as_deref().expect("it says why");
        assert!(failed.contains("damaged"), "{failed}");
        assert!(page.sees("damaged"));
    }

    #[test]
    fn enter_in_the_freeform_box_is_a_new_line() {
        let mut page = page_in_notebook("[letter1984]");
        cite_chord(&mut page);
        page.press("Freeform");
        page.frame(Vec::new());

        type_into(&mut page, "A letter");
        press_key(&mut page, egui::Key::Enter);
        type_into(&mut page, "and more");
        page.frame(Vec::new());

        let window = page.app.refs.form.as_ref().expect("still up");
        assert_eq!(window.form.reference.text, "A letter\nand more");

        page.press("Add and cite");
        let saved = page.app.refs.list.get("letter1984").unwrap();
        assert_eq!(saved.kind, Kind::Freeform);
        assert_eq!(saved.text, "A letter\nand more");
    }

    #[test]
    fn typing_in_the_form_never_reaches_the_document_and_escape_cancels_it() {
        let mut page = page_in_notebook("[lamport1994]");
        cite_chord(&mut page);
        assert!(page.app.refs.form.is_some());

        type_into(&mut page, "typed in the form");
        assert_eq!(only_text(&page), "[lamport1994]");

        press_key(&mut page, egui::Key::Escape);

        assert!(page.app.refs.form.is_none());
        assert!(!page.app.dialog_open());
        assert!(!reference_file(&page, "lamport1994").exists());
        assert_eq!(only_text(&page), "[lamport1994]");
    }

    #[test]
    fn help_being_up_leaves_the_form_its_enter() {
        let mut page = noted_key_with_its_form();
        page.app.show_help = true;
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.refs.form.is_none());
        assert!(reference_file(&page, "lamport1994").exists());
    }

    #[test]
    fn escape_closes_a_note_opened_over_a_form_before_the_form() {
        let mut page = page_in_notebook("hello");
        page.app.refs.open_form_to_add();
        fill_form(&mut page, "lamport1994");
        let id = page
            .app
            .session
            .add_annotation(AnnotationKind::Footnote)
            .unwrap();
        page.app.set_open_note(Some(id));
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::Escape);

        assert_eq!(page.app.open_note, None);
        assert!(page.app.refs.form.is_some(), "the form is under it");
        press_key(&mut page, egui::Key::Escape);
        assert!(page.app.refs.form.is_none());
    }

    #[test]
    fn escape_closes_a_form_opened_over_a_note_before_the_note() {
        let mut page = page_in_notebook("hello");
        let id = page
            .app
            .session
            .add_annotation(AnnotationKind::Footnote)
            .unwrap();
        page.app.set_open_note(Some(id));
        page.frame(Vec::new());
        page.app.refs.open_form_to_add();
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::Escape);

        assert!(page.app.refs.form.is_none());
        assert_eq!(page.app.open_note, Some(id));
    }

    #[test]
    fn enter_in_a_link_prompt_doesnt_save_the_form_behind_it() {
        let mut page = page_in_notebook("hello [lamport1994]");
        select(&mut page, 0, 5, 5);
        // The caret's on nothing, so reach the form on the key by hand.
        page.app
            .refs
            .open_form(ReferenceForm::new(Some("lamport1994".to_string())));
        page.app.refs.form.as_mut().unwrap().form.reference.title = "T".to_string();
        page.frame(Vec::new());
        ctrl_k(&mut page);
        assert!(page.app.link_prompt.is_some());

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.link_prompt.is_none(), "the prompt had it");
        assert!(page.app.refs.form.is_some());
        assert!(!reference_file(&page, "lamport1994").exists());
    }

    #[test]
    fn enter_in_the_properties_prompt_doesnt_save_the_form_behind_it() {
        let mut page = page_in_notebook("[lamport1994]");
        cite_chord(&mut page);
        page.app.refs.form.as_mut().unwrap().form.reference.title = "T".to_string();
        page.app.begin_properties();
        page.frame(Vec::new());
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.metadata_prompt.is_none(), "the prompt had it");
        assert!(page.app.refs.form.is_some());
        assert!(!reference_file(&page, "lamport1994").exists());
    }

    #[test]
    fn enter_in_the_find_bar_doesnt_save_the_form_behind_it() {
        let mut page = page_in_notebook("[lamport1994]");
        cite_chord(&mut page);
        page.app.refs.form.as_mut().unwrap().form.reference.title = "T".to_string();
        let ctrl = egui::Modifiers {
            command: true,
            ..egui::Modifiers::NONE
        };
        key(&mut page, egui::Key::F, ctrl);
        assert!(page.app.find.is_some());
        // The find bar is in the chrome, which isn't drawn here. It takes the keyboard as it opens.
        page.ctx
            .memory_mut(|memory| memory.request_focus(crate::find_query_id()));

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.refs.form.is_some());
        assert!(!reference_file(&page, "lamport1994").exists());
    }

    #[test]
    fn a_find_bar_left_up_doesnt_take_enter_from_a_form_opened_after_it() {
        let mut page = page_in_notebook("[lamport1994]");
        let ctrl = egui::Modifiers {
            command: true,
            ..egui::Modifiers::NONE
        };
        key(&mut page, egui::Key::F, ctrl);
        page.frame(Vec::new());
        page.click(near_the_start(&page));
        page.frame(Vec::new());
        let end = only_text(&page).len();
        let block_id = page.app.session.document().blocks[0].id;
        page.app.session.set_caret(crate::edit::Cursor {
            block_id,
            offset: end,
        });

        cite_chord(&mut page);
        page.app
            .refs
            .form
            .as_mut()
            .expect("the form opens")
            .form
            .reference
            .title = "T".to_string();
        page.frame(Vec::new());
        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.find.is_some());
        assert!(reference_file(&page, "lamport1994").exists());
    }

    #[test]
    fn clicking_a_form_under_a_prompt_leaves_the_prompt_on_top() {
        let mut page = typed_form_over_a_citation();
        select(&mut page, 0, 3, 3);
        ctrl_k(&mut page);
        page.frame(Vec::new());
        assert!(page.app.link_prompt.is_some());
        let prompt = page.top_layer();
        let centre = page.ctx.content_rect().center();

        // Left of the prompt, where only the form is.
        page.click(centre - Vec2::new(235.0, 0.0));
        page.frame(Vec::new());

        assert_eq!(page.top_layer(), prompt);
        assert!(page.app.link_prompt.is_some());
    }

    #[test]
    fn a_click_on_a_form_under_a_prompt_doesnt_reach_the_page() {
        let mut page = typed_form_over_a_citation();
        select(&mut page, 0, 3, 3);
        ctrl_k(&mut page);
        page.frame(Vec::new());
        assert!(page.app.link_prompt.is_some());
        let (caret, selection) = (page.app.session.caret, page.app.session.selection);
        assert!(selection.is_some());
        let centre = page.ctx.content_rect().center();

        // Left of the prompt, where only the form is.
        page.click(centre - Vec2::new(235.0, 0.0));
        page.frame(Vec::new());

        assert_eq!(page.app.session.caret, caret);
        assert_eq!(page.app.session.selection, selection);
        assert_eq!(typed_authors(&page), "Typed Author");

        // What covers it still takes clicks, and then the form does again.
        page.press("Set link");
        assert!(page.app.link_prompt.is_none());
        page.press("Cancel");
        assert!(page.app.refs.form.is_none());
    }

    #[test]
    fn escape_closes_a_prompt_over_a_form_and_leaves_the_form() {
        let mut page = typed_form_over_a_citation();
        select(&mut page, 0, 3, 3);
        ctrl_k(&mut page);
        assert!(page.app.link_prompt.is_some());

        press_key(&mut page, egui::Key::Escape);

        assert!(page.app.link_prompt.is_none());
        assert_eq!(typed_authors(&page), "Typed Author");
    }
}
