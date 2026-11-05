//! The commands on the menu bar, in the one list the menus and the command palette both read, and
//! the palette itself: Ctrl-P, or Cmd-P on a Mac, lists every command that can be run with its
//! key, narrowed by typing as the jump list is, and Enter runs the one chosen as the menu would.

use eframe::egui;
use kame_notebook::list::rank;

use super::notebook::{focus_field, list_keys, list_row, list_rows, list_window};
use super::{
    menu_entry, shortcut_label, AnnotationKind, BarMenu, FileCommand, KameApp, MenuAction,
    ModeCommand, FONT_SIZE_STEP,
};

/// One command on the menu bar, as its menu shows it and the palette lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MenuCommand {
    pub(crate) menu: BarMenu,
    pub(crate) label: &'static str,
    /// Its key, written for Linux, as [`shortcut_label`] takes it.
    pub(crate) key: Option<&'static str>,
    pub(crate) action: MenuAction,
    /// Whether it can be chosen now. The menu shows one that can't greyed out, and the palette
    /// leaves it out.
    pub(crate) enabled: bool,
    /// What the menu says on hovering over it while it can't be chosen.
    pub(crate) why_not: Option<&'static str>,
    /// Whether it's a setting that's on, which the menu shows.
    pub(crate) checked: bool,
    /// Whether a rule comes before it in its menu.
    pub(crate) rule: bool,
}

impl MenuCommand {
    fn new(
        menu: BarMenu,
        label: &'static str,
        key: Option<&'static str>,
        action: MenuAction,
    ) -> Self {
        MenuCommand {
            menu,
            label,
            key,
            action,
            enabled: true,
            why_not: None,
            checked: false,
            rule: false,
        }
    }

    fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }

    fn after_rule(mut self) -> Self {
        self.rule = true;
        self
    }

    fn enabled(mut self, enabled: bool, why_not: Option<&'static str>) -> Self {
        self.enabled = enabled;
        self.why_not = why_not;
        self
    }

    /// How the palette names it: its menu, then itself.
    pub(crate) fn full_name(&self) -> String {
        format!("{}: {}", menu_name(self.menu), self.label)
    }
}

pub(crate) fn menu_name(menu: BarMenu) -> &'static str {
    match menu {
        BarMenu::File => "File",
        BarMenu::Annotate => "Annotate",
        BarMenu::View => "View",
        BarMenu::Site => "Site",
        BarMenu::Help => "Help",
    }
}

/// The palette, open.
pub(crate) struct CommandList {
    query: String,
    selected: usize,
    focus: bool,
}

/// Which of `commands` the palette lists for `query`, as indices, best first, with equals in the
/// menus' order. With nothing typed, that's all of them in the menus' order. A command that can't
/// be chosen now isn't listed, and nor is the palette itself.
pub(crate) fn narrow_commands(query: &str, commands: &[MenuCommand]) -> Vec<usize> {
    let mut found: Vec<(u32, usize)> = commands
        .iter()
        .enumerate()
        .filter(|(_, command)| command.enabled && command.action != MenuAction::Commands)
        .filter_map(|(at, command)| {
            let score = rank(query, command.label, &command.full_name())?;
            Some((score, at))
        })
        .collect();
    if !query.is_empty() {
        found.sort_by_key(|(score, at)| (score / 8, *at));
    }
    found.into_iter().map(|(_, at)| at).collect()
}

/// Draws `commands` as rows of their menu, setting `action` to the one clicked.
pub(crate) fn command_rows<'a>(
    ui: &mut egui::Ui,
    commands: impl IntoIterator<Item = &'a MenuCommand>,
    action: &mut Option<MenuAction>,
) {
    for command in commands {
        if command.rule {
            ui.separator();
        }
        let row = ui.add_enabled_ui(command.enabled, |ui| {
            let entry = menu_entry(ui, command.label, command.key, command.checked);
            match command.why_not {
                Some(why) => entry.on_disabled_hover_text(why),
                None => entry,
            }
        });
        if row.inner.clicked() {
            *action = Some(command.action);
        }
    }
}

impl KameApp {
    /// Every command on the menu bar, menu by menu, in the order its menu shows them. Something
    /// only a notebook has, or a merge with no conflict copies to merge, isn't there at all.
    pub(crate) fn commands(&self) -> Vec<MenuCommand> {
        use BarMenu::{Annotate, File, Help, Site, View};
        let item = MenuCommand::new;
        let in_notebook = self.nb.notebook.is_some();
        let mut commands = vec![
            item(File, "New", Some("Ctrl-N"), MenuAction::New),
            item(File, "New Notebook…", None, MenuAction::NewNotebook),
            item(File, "Open…", Some("Ctrl-O"), MenuAction::Open),
            item(File, "Open Path…", None, MenuAction::OpenPath),
            item(File, "Save", Some("Ctrl-S"), MenuAction::Save),
            item(File, "Save As…", None, MenuAction::SaveAs),
        ];
        let mut history = vec![
            item(File, "Compact History", None, MenuAction::CompactHistory),
            item(File, "Back", Some("Ctrl-["), MenuAction::Back),
            item(File, "Link to Document…", None, MenuAction::LinkDocument),
            item(File, "Import Markdown…", None, MenuAction::ImportMarkdown),
            item(
                File,
                "Export Markdown…",
                Some("Ctrl-Shift-E"),
                MenuAction::File(FileCommand::ExportMarkdown),
            ),
            item(
                File,
                "Export HTML…",
                Some("Ctrl-Shift-B"),
                MenuAction::File(FileCommand::ExportHtml),
            ),
        ];
        if !self.conflict_copies.is_empty() {
            history.insert(
                0,
                item(
                    File,
                    "Merge Conflict Copies",
                    None,
                    MenuAction::MergeConflictCopies,
                ),
            );
        }
        history[0].rule = true;
        commands.extend(history);
        commands.extend([
            item(
                File,
                "Snapshot Now…",
                Some("Ctrl-Shift-S"),
                MenuAction::File(FileCommand::SnapshotNow),
            )
            .after_rule(),
            item(
                File,
                "Restore Snapshot…",
                Some("Ctrl-Shift-R"),
                MenuAction::File(FileCommand::RestoreSnapshot),
            ),
            item(File, "Document Properties…", None, MenuAction::Properties).after_rule(),
        ]);

        commands.extend([
            item(
                Annotate,
                "Note",
                Some("Ctrl-Shift-N"),
                MenuAction::Annotate(AnnotationKind::Note),
            ),
            item(
                Annotate,
                "Marginal",
                Some("Ctrl-Shift-M"),
                MenuAction::Annotate(AnnotationKind::Marginal),
            ),
            item(
                Annotate,
                "Footnote",
                Some("Ctrl-Shift-F"),
                MenuAction::Annotate(AnnotationKind::Footnote),
            ),
            item(Annotate, "Cite…", Some("Ctrl-Shift-C"), MenuAction::Cite)
                .enabled(in_notebook, Some("Citing needs a notebook."))
                .after_rule(),
        ]);
        if in_notebook {
            commands.push(item(Annotate, "References…", None, MenuAction::References));
        }

        commands.extend([
            item(
                View,
                "Smaller type",
                Some("Ctrl -"),
                MenuAction::TypeSize(-FONT_SIZE_STEP),
            ),
            item(
                View,
                "Larger type",
                Some("Ctrl +"),
                MenuAction::TypeSize(FONT_SIZE_STEP),
            ),
            item(
                View,
                "Dark mode",
                Some("Ctrl-Shift-D"),
                MenuAction::ToggleDark,
            )
            .checked(self.dark),
            item(View, "Hyphenate", None, MenuAction::ToggleHyphenate).checked(self.hyphenate),
        ]);
        if in_notebook {
            commands.extend([
                item(
                    View,
                    "Notebook browser",
                    Some("Ctrl-Shift-O"),
                    MenuAction::ToggleBrowser,
                )
                .checked(self.nb.browser_open),
                item(
                    View,
                    "Notebook contents",
                    Some("Ctrl-Shift-L"),
                    MenuAction::NotebookHome,
                ),
                item(View, "Jump to document", Some("Ctrl-L"), MenuAction::Jump),
                item(
                    View,
                    "Search this notebook",
                    Some("Ctrl-R"),
                    MenuAction::SearchNotebook,
                ),
            ]);
        }
        let jump_to_heading = item(
            View,
            "Jump to heading",
            Some("Ctrl-J"),
            MenuAction::JumpToHeading,
        );
        commands.push(if in_notebook {
            jump_to_heading.after_rule()
        } else {
            jump_to_heading
        });
        commands.push(
            item(View, "Check spelling", None, MenuAction::ToggleCheck)
                .checked(self.check_spelling)
                .after_rule(),
        );
        for (at, (label, mode, key)) in [
            ("Fullscreen", ModeCommand::Fullscreen, "F11"),
            ("Hide chrome", ModeCommand::HideChrome, "Ctrl-Shift-H"),
            ("Hide margin", ModeCommand::HideMargin, "Ctrl-Shift-G"),
            (
                "Paragraph focus",
                ModeCommand::ParagraphFocus,
                "Ctrl-Shift-P",
            ),
            ("Typewriter", ModeCommand::Typewriter, "Ctrl-Shift-T"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut command = item(View, label, Some(key), MenuAction::Mode(mode))
                .checked(self.modes.is_on(mode));
            command.rule = at == 0;
            commands.push(command);
        }

        if let Some(notebook) = &self.nb.notebook {
            let working = self.publishing.working();
            let has_remote = notebook
                .settings
                .remote
                .as_deref()
                .is_some_and(|remote| !remote.trim().is_empty());
            commands.extend([
                item(Site, "Build", None, MenuAction::SiteBuild).enabled(!working, None),
                item(Site, "Sync", None, MenuAction::SiteSync).enabled(
                    !working && has_remote,
                    Some("Set remote in notebook.yaml to send the site"),
                ),
            ]);
        }

        commands.extend([
            item(Help, "Help", Some("F1"), MenuAction::Help),
            item(Help, "Commands…", Some("Ctrl-P"), MenuAction::Commands),
        ]);
        commands
    }

    /// Ctrl-P: opens the palette, or closes it, waiting behind another dialog as the other lists
    /// do.
    pub(crate) fn toggle_commands(&mut self) {
        let open = self.nb.commands.is_some();
        self.nb.close_lists();
        if !open && !self.dialog_open() {
            self.nb.commands = Some(CommandList {
                query: String::new(),
                selected: 0,
                focus: true,
            });
        }
    }

    pub(crate) fn show_command_list(&mut self, ctx: &egui::Context) {
        let Some(mut list) = self.nb.commands.take() else {
            return;
        };
        let commands = self.commands();
        let shown = narrow_commands(&list.query, &commands);
        let (close, confirm) = list_keys(ctx, &mut list.selected, shown.len());
        let mut clicked = None;
        let mac = cfg!(target_os = "macos");
        list_window(ctx, "kame-commands", |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut list.query)
                    .hint_text("Run a command")
                    .return_key(None)
                    .desired_width(f32::INFINITY),
            );
            focus_field(&edit, &mut list.focus);
            if edit.changed() {
                list.selected = 0;
            }
            ui.separator();
            list_rows(ui, shown.len(), list.selected, |ui, row, selected| {
                let command = &commands[shown[row]];
                let key = command
                    .key
                    .map_or(String::new(), |key| shortcut_label(key, mac));
                if list_row(ui, selected, &command.full_name(), &key).clicked() {
                    clicked = Some(row);
                }
            });
        });
        let chosen = clicked.or(confirm.then_some(list.selected));
        if let Some(at) = chosen.and_then(|row| shown.get(row)) {
            self.run_menu_action(ctx, commands[*at].action);
            return;
        }
        if !close {
            self.nb.commands = Some(list);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_temp_dir, Headless};
    use kame_core::session::Session;

    fn page() -> Headless {
        let folder = test_temp_dir().join(format!("commands-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut session = Session::open(&folder.join("doc.kame")).unwrap();
        session.insert("Some words.");
        let mut page = Headless::new(session);
        page.frame(Vec::new());
        page
    }

    fn press(page: &mut Headless, key: egui::Key, modifiers: egui::Modifiers) {
        for pressed in [true, false] {
            page.frame(vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            }]);
        }
    }

    fn palette_chord(page: &mut Headless) {
        press(page, egui::Key::P, egui::Modifiers::COMMAND);
        page.frame(Vec::new());
    }

    fn type_text(page: &mut Headless, text: &str) {
        for character in text.chars() {
            page.frame(vec![egui::Event::Text(character.to_string())]);
        }
    }

    #[test]
    fn the_key_lists_the_menus_commands_with_their_keys() {
        let mut page = page();

        palette_chord(&mut page);

        assert!(page.app.dialog_open());
        assert!(page.can_press("File: New"));
        assert!(page.sees(&shortcut_label("Ctrl-N", cfg!(target_os = "macos"))));
        assert!(page.can_press("File: Open…"));
    }

    #[test]
    fn typing_narrows_the_list_and_enter_runs_the_first_as_the_menu_would() {
        let mut page = page();
        let dark = page.app.dark;
        palette_chord(&mut page);

        type_text(&mut page, "dark mode");
        page.frame(Vec::new());
        assert!(page.can_press("View: Dark mode"));
        assert!(!page.can_press("File: New"));
        press(&mut page, egui::Key::Enter, egui::Modifiers::NONE);

        assert_eq!(page.app.dark, !dark);
        assert!(!page.app.dialog_open());
        assert_eq!(page.app.session.document().blocks[0].text, "Some words.");
    }

    #[test]
    fn a_click_runs_its_command() {
        let mut page = page();
        palette_chord(&mut page);
        type_text(&mut page, "typewriter");

        page.press("View: Typewriter");

        assert!(page.app.modes.typewriter);
        assert!(!page.app.dialog_open());
    }

    #[test]
    fn the_arrows_choose_which_runs() {
        let mut page = page();
        palette_chord(&mut page);
        type_text(&mut page, "hide");
        page.frame(Vec::new());
        assert!(page.can_press("View: Hide chrome"));
        assert!(page.can_press("View: Hide margin"));

        press(&mut page, egui::Key::ArrowDown, egui::Modifiers::NONE);
        press(&mut page, egui::Key::Enter, egui::Modifiers::NONE);

        assert!(!page.app.modes.hide_chrome);
        assert!(page.app.modes.hide_margin);
    }

    #[test]
    fn escape_runs_nothing() {
        let mut page = page();
        palette_chord(&mut page);
        type_text(&mut page, "typewriter");

        press(&mut page, egui::Key::Escape, egui::Modifiers::NONE);

        assert!(!page.app.modes.typewriter);
        assert!(!page.app.dialog_open());
    }

    #[test]
    fn the_key_again_closes_the_list() {
        let mut page = page();
        palette_chord(&mut page);
        palette_chord(&mut page);
        assert!(!page.app.dialog_open());
    }

    #[test]
    fn typing_goes_to_the_list_and_not_the_document() {
        let mut page = page();
        palette_chord(&mut page);

        type_text(&mut page, "xyz");

        assert_eq!(page.app.session.document().blocks[0].text, "Some words.");
        assert!(!page.can_press("File: New"), "nothing fits");
    }

    #[test]
    fn what_cant_be_run_here_is_left_out() {
        let mut page = page();
        assert!(page.app.nb.notebook.is_none());
        palette_chord(&mut page);

        type_text(&mut page, "cite");
        page.frame(Vec::new());

        assert!(!page.sees("Annotate: Cite…"), "citing needs a notebook");
        let commands = page.app.commands();
        let cite = commands
            .iter()
            .position(|command| command.action == MenuAction::Cite)
            .expect("the menu has it, greyed out");
        assert!(!commands[cite].enabled);
        assert!(!narrow_commands("cite", &commands).contains(&cite));
        let palette = commands
            .iter()
            .position(|command| command.action == MenuAction::Commands)
            .expect("the Help menu has the palette");
        assert!(!narrow_commands("commands", &commands).contains(&palette));
    }

    #[test]
    fn the_key_waits_behind_another_dialog() {
        let mut page = page();
        page.app.begin_properties();
        page.frame(Vec::new());

        palette_chord(&mut page);

        assert!(page.app.nb.commands.is_none());
        assert!(page.app.metadata_prompt.is_some());
    }

    #[test]
    fn the_key_waits_for_the_close_prompt() {
        let mut page = page();
        page.app.close_flow.prompting = true;

        palette_chord(&mut page);

        assert!(page.app.nb.commands.is_none());
    }

    #[test]
    fn the_key_waits_in_a_notes_box() {
        let mut page = page();
        press(
            &mut page,
            egui::Key::M,
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        );
        page.frame(Vec::new());
        assert!(page.app.open_note.is_some());

        palette_chord(&mut page);

        assert!(page.app.nb.commands.is_none());
    }

    #[test]
    fn the_key_takes_over_from_another_list() {
        let mut page = page();
        press(&mut page, egui::Key::J, egui::Modifiers::COMMAND);
        page.frame(Vec::new());
        assert!(page.app.nb.headings.is_some());

        palette_chord(&mut page);

        assert!(page.app.nb.headings.is_none());
        assert!(page.app.nb.commands.is_some());
    }

    #[test]
    fn with_nothing_typed_the_palette_lists_every_command_the_menus_can_run() {
        let page = page();
        let commands = page.app.commands();
        let listed: Vec<MenuAction> = narrow_commands("", &commands)
            .into_iter()
            .map(|at| commands[at].action)
            .collect();
        let runnable: Vec<MenuAction> = commands
            .iter()
            .filter(|command| command.enabled && command.action != MenuAction::Commands)
            .map(|command| command.action)
            .collect();
        assert_eq!(listed, runnable);
        assert!(listed.contains(&MenuAction::Save));
        assert!(listed.contains(&MenuAction::TypeSize(FONT_SIZE_STEP)));
    }

    #[test]
    fn a_few_letters_of_each_word_find_a_command() {
        let page = page();
        let commands = page.app.commands();
        let first = narrow_commands("exp ht", &commands)[0];
        assert_eq!(
            commands[first].action,
            MenuAction::File(FileCommand::ExportHtml)
        );
    }

    /// Draws the menu bar with `menu` open, for a few frames, as the window does.
    fn open_menu(page: &mut Headless, menu: BarMenu) {
        page.app.pending_menu = Some(menu);
        for _ in 0..3 {
            page.time += 0.02;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(page.width, page.height),
                )),
                time: Some(page.time),
                ..Default::default()
            };
            let app = &mut page.app;
            let mut output = page.ctx.run_ui(input, |ui| app.show_status(ui));
            page.drawn = output.platform_output.accesskit_update.take();
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn each_menu_shows_its_commands_from_the_list() {
        let mut page = page();
        for menu in [
            BarMenu::File,
            BarMenu::Annotate,
            BarMenu::View,
            BarMenu::Help,
        ] {
            open_menu(&mut page, menu);
            for command in page.app.commands().iter().filter(|c| c.menu == menu) {
                if matches!(command.action, MenuAction::TypeSize(_)) {
                    continue;
                }
                assert!(page.sees(command.label), "{menu:?} shows {}", command.label);
            }
            // The chord again closes it.
            open_menu(&mut page, menu);
            assert!(!page.sees("Typewriter") && !page.sees("Save As…") && !page.sees("Footnote"));
        }
        open_menu(&mut page, BarMenu::Annotate);
        assert!(page.sees("Cite…"));
        assert!(!page.can_press("Cite…"), "greyed out outside a notebook");
    }

    #[test]
    fn the_help_names_the_key() {
        let row = crate::HELP_ROWS.iter().find(|(key, _)| *key == "Ctrl-P");
        assert!(row.is_some_and(|(_, what)| what.contains("command")));
    }
}
