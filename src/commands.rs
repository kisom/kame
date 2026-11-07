//! The commands on the menu bar, in the one list the menus and the command palette both read, and
//! the palette itself: Ctrl-P, or Cmd-P on a Mac, lists every command that can be run with its
//! key, narrowed by typing as the jump list is, and Enter runs the one chosen as the menu would.

use eframe::egui;
use kame_core::model::BlockKind;
use kame_notebook::list::rank;

use super::interface::Change;
use super::notebook::{focus_field, list_keys, list_row, list_rows, list_window};
use super::{
    menu_entry, shortcut_label, AnnotationKind, BarMenu, Command, FileCommand, KameApp, MenuAction,
    ModeCommand, FONT_SIZE_STEP,
};

/// What the Edit menu does, each the same as its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditCommand {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Find,
    /// Makes the caret's block a heading of this level, or with none, body text.
    Kind(Option<u8>),
    Link,
    MoveItem {
        up: bool,
    },
    IndentItem {
        indent: bool,
    },
}

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
        BarMenu::Edit => "Edit",
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

        commands.extend(self.edit_commands());

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
                "Smaller interface text",
                Some("Ctrl-Alt--"),
                MenuAction::InterfaceText(Change::Smaller),
            ),
            item(
                View,
                "Larger interface text",
                Some("Ctrl-Alt-+"),
                MenuAction::InterfaceText(Change::Larger),
            ),
            item(
                View,
                "Reset interface text",
                Some("Ctrl-Alt-0"),
                MenuAction::InterfaceText(Change::Reset),
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

    /// The Edit menu. What would change the document is greyed out where its key is refused, in
    /// a document that can't be written or while a dialog has the keyboard, and the rest where it
    /// would do nothing: an undo with
    /// nothing to undo, a cut or copy with nothing selected, a link with nothing to link, and the
    /// list items' commands outside a list. Whether there's anything to paste isn't known without
    /// asking the system, so Paste always can be chosen.
    fn edit_commands(&self) -> Vec<MenuCommand> {
        use EditCommand::*;
        let item = |label, key, command| {
            MenuCommand::new(BarMenu::Edit, label, Some(key), MenuAction::Edit(command))
        };
        let session = &self.session;
        // Why the keys that write are refused now, if they are: the document can't be changed,
        // or a dialog has the keyboard.
        let refused = if !session.save_failed {
            self.dialog_open().then_some("Close the dialog first.")
        } else if session.newer {
            Some("This document is read-only until kame is updated.")
        } else if session.held_elsewhere {
            Some("Another kame has this file open, so save it under another name first.")
        } else if session.replaced {
            Some("This file changed on disk, so save it under another name first.")
        } else {
            Some("The last save failed, so retry it first.")
        };
        // Those that write: refused as their keys are, or else greyed out unless `can`.
        let writes = |command: MenuCommand, can: bool, why_not: Option<&'static str>| match refused
        {
            Some(why) => command.enabled(false, Some(why)),
            None => command.enabled(can, why_not),
        };
        let (undo, redo) = session.history();
        let selected = session
            .ordered_selection()
            .is_some_and(|(start, end)| start != end);
        let nothing_selected = Some("Select some words first.");
        let in_list = session.tabbed_blocks().iter().any(|id| {
            session.block_index(*id).is_some_and(|at| {
                matches!(
                    session.document().blocks[at].kind,
                    BlockKind::ListItem { .. }
                )
            })
        });
        let not_in_list = Some("The caret isn't in a list.");
        let heading = |level, label, key| writes(item(label, key, Kind(Some(level))), true, None);
        vec![
            writes(
                item("Undo", "Ctrl-Z", Undo),
                undo > 0,
                Some("Nothing to undo."),
            ),
            writes(
                item("Redo", "Ctrl-Shift-Z", Redo),
                redo > 0,
                Some("Nothing to redo."),
            ),
            writes(item("Cut", "Ctrl-X", Cut), selected, nothing_selected).after_rule(),
            item("Copy", "Ctrl-C", Copy).enabled(selected, nothing_selected),
            writes(item("Paste", "Ctrl-V", Paste), true, None),
            item("Select All", "Ctrl-A", SelectAll),
            item("Find", "Ctrl-F", Find).after_rule(),
            heading(1, "Heading 1", "Ctrl-Shift-1").after_rule(),
            heading(2, "Heading 2", "Ctrl-Shift-2"),
            heading(3, "Heading 3", "Ctrl-Shift-3"),
            writes(item("Body Text", "Ctrl-Shift-0", Kind(None)), true, None),
            writes(
                item("Link…", "Ctrl-K", Link),
                session.link_target().is_some(),
                Some("Select words to link, or put the caret in a link."),
            )
            .after_rule(),
            writes(
                item("Move Item Up", "Ctrl-Shift-Up", MoveItem { up: true }),
                in_list,
                not_in_list,
            )
            .after_rule(),
            writes(
                item("Move Item Down", "Ctrl-Shift-Down", MoveItem { up: false }),
                in_list,
                not_in_list,
            ),
            writes(
                item("Indent Item", "Tab", IndentItem { indent: true }),
                in_list,
                not_in_list,
            ),
            writes(
                item("Outdent Item", "Shift-Tab", IndentItem { indent: false }),
                in_list,
                not_in_list,
            ),
        ]
    }

    /// Runs an Edit menu command through the code its key runs. A cut, a copy or a paste is asked
    /// of the window, which brings it back next frame as the event its key makes, so what reaches
    /// the page, kame's own clipboard with it, is just what the key's would.
    pub(crate) fn run_edit_command(&mut self, ctx: &egui::Context, command: EditCommand) {
        let ask = |request| {
            ctx.send_viewport_cmd(request);
            ctx.request_repaint();
        };
        match command {
            EditCommand::Undo => self.run_command(ctx, Command::Undo),
            EditCommand::Redo => self.run_command(ctx, Command::Redo),
            EditCommand::Cut => ask(egui::ViewportCommand::RequestCut),
            EditCommand::Copy => ask(egui::ViewportCommand::RequestCopy),
            EditCommand::Paste => ask(egui::ViewportCommand::RequestPaste),
            EditCommand::SelectAll => self.run_command(ctx, Command::SelectAll),
            EditCommand::Find => self.toggle_find(),
            EditCommand::Kind(level) => {
                let kind = match level {
                    Some(level) => BlockKind::Heading {
                        level,
                        centered: false,
                    },
                    None => BlockKind::Paragraph,
                };
                self.run_command(ctx, Command::SetBlockKind(kind));
            }
            EditCommand::Link => self.begin_link(),
            EditCommand::MoveItem { up } => self.run_command(ctx, Command::MoveListItem { up }),
            EditCommand::IndentItem { indent } => {
                self.follow_caret = true;
                self.session.indent_list_item(indent);
            }
        }
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
    use kame_core::edit::Cursor;
    use kame_core::model::{BlockId, BlockKind};
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

    /// One frame of the menu bar alone, with `events` having just happened. What it asks of the
    /// window comes back in the page's next frame, as eframe brings it.
    fn bar_frame(page: &mut Headless, events: Vec<egui::Event>) {
        page.time += 0.02;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(page.width, page.height),
            )),
            time: Some(page.time),
            events,
            ..Default::default()
        };
        let app = &mut page.app;
        let mut output = page.ctx.run_ui(input, |ui| app.show_status(ui));
        page.drawn = output.platform_output.accesskit_update.take();
        page.answer(&output);
        output.drop_without_applying_deltas();
    }

    /// Draws the menu bar with `menu` open, for a few frames, as the window does.
    fn open_menu(page: &mut Headless, menu: BarMenu) {
        page.app.pending_menu = Some(menu);
        for _ in 0..3 {
            bar_frame(page, Vec::new());
        }
    }

    /// Where the last frame drew something saying exactly `label`, at its middle.
    fn middle_of(page: &Headless, label: &str) -> Option<egui::Pos2> {
        page.drawn
            .iter()
            .flat_map(|tree| &tree.nodes)
            .find_map(|(_, node)| node.bounds().filter(|_| node.label() == Some(label)))
            .map(|bounds| {
                egui::Pos2::new(
                    ((bounds.x0 + bounds.x1) / 2.0) as f32,
                    ((bounds.y0 + bounds.y1) / 2.0) as f32,
                )
            })
    }

    /// What `menu`'s row for the command called `label` says: its words, then its key.
    fn row(page: &Headless, menu: BarMenu, label: &str) -> String {
        let commands = page.app.commands();
        let command = commands
            .iter()
            .find(|command| command.menu == menu && command.label == label)
            .unwrap_or_else(|| panic!("{menu:?} has no {label:?}"));
        match command.key {
            Some(key) => format!("{label} {}", shortcut_label(key, cfg!(target_os = "macos"))),
            None => label.to_string(),
        }
    }

    /// Opens `menu` and clicks its row for `label`, as a hand would, then gives the page the
    /// frame after, which is when what the click asked of the window comes back.
    fn choose(page: &mut Headless, menu: BarMenu, label: &str) {
        let row = row(page, menu, label);
        open_menu(page, menu);
        let at = middle_of(page, &row).unwrap_or_else(|| panic!("no row saying {row:?}"));
        assert!(page.can_press(&row), "{row:?} can be chosen");
        bar_frame(page, vec![egui::Event::PointerMoved(at)]);
        for pressed in [true, false] {
            bar_frame(
                page,
                vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        page.frame(Vec::new());
    }

    /// "Some words.", then a list of two items, with the caret at the end of the second.
    fn list_page() -> (Headless, BlockId, BlockId) {
        let mut page = page();
        let item = BlockKind::ListItem {
            ordered: false,
            depth: 0,
        };
        let session = &mut page.app.session;
        let mut ids = Vec::new();
        for words in ["First", "Second"] {
            session.split();
            session.insert(words);
            let id = session.valid_caret().block_id;
            session.set_block_kind(id, item.clone());
            ids.push(id);
        }
        page.frame(Vec::new());
        (page, ids[0], ids[1])
    }

    fn select(page: &mut Headless, block: usize, start: usize, end: usize) {
        let block_id = page.app.session.document().blocks[block].id;
        page.app.session.set_caret(Cursor {
            block_id,
            offset: start,
        });
        page.app.session.select_to(Cursor {
            block_id,
            offset: end,
        });
    }

    fn texts(page: &Headless) -> Vec<String> {
        let document = page.app.session.document();
        document
            .blocks
            .iter()
            .map(|block| block.text.clone())
            .collect()
    }

    /// The Edit menu's command called `label`.
    fn edit_command(page: &Headless, label: &str) -> MenuCommand {
        page.app
            .commands()
            .into_iter()
            .find(|command| command.menu == BarMenu::Edit && command.label == label)
            .unwrap_or_else(|| panic!("the Edit menu has no {label:?}"))
    }

    const EDIT_ROWS: [&str; 16] = [
        "Undo",
        "Redo",
        "Cut",
        "Copy",
        "Paste",
        "Select All",
        "Find",
        "Heading 1",
        "Heading 2",
        "Heading 3",
        "Body Text",
        "Link…",
        "Move Item Up",
        "Move Item Down",
        "Indent Item",
        "Outdent Item",
    ];

    #[test]
    fn the_edit_menu_comes_after_file() {
        let mut page = page();
        bar_frame(&mut page, Vec::new());
        let x = |name: &str| middle_of(&page, name).expect(name).x;
        assert!(x("File") < x("Edit"));
        assert!(x("Edit") < x("Annotate"));
    }

    #[test]
    fn the_edit_menu_holds_its_commands_in_their_groups() {
        let page = page();
        let commands = page.app.commands();
        let edit: Vec<&MenuCommand> = commands
            .iter()
            .filter(|command| command.menu == BarMenu::Edit)
            .collect();
        let labels: Vec<&str> = edit.iter().map(|command| command.label).collect();
        assert_eq!(labels, EDIT_ROWS);
        let rules: Vec<&str> = edit
            .iter()
            .filter(|command| command.rule)
            .map(|command| command.label)
            .collect();
        assert_eq!(rules, ["Cut", "Find", "Heading 1", "Link…", "Move Item Up"]);
        let keys: Vec<Option<&str>> = edit.iter().map(|command| command.key).collect();
        assert_eq!(
            keys,
            [
                Some("Ctrl-Z"),
                Some("Ctrl-Shift-Z"),
                Some("Ctrl-X"),
                Some("Ctrl-C"),
                Some("Ctrl-V"),
                Some("Ctrl-A"),
                Some("Ctrl-F"),
                Some("Ctrl-Shift-1"),
                Some("Ctrl-Shift-2"),
                Some("Ctrl-Shift-3"),
                Some("Ctrl-Shift-0"),
                Some("Ctrl-K"),
                Some("Ctrl-Shift-Up"),
                Some("Ctrl-Shift-Down"),
                Some("Tab"),
                Some("Shift-Tab"),
            ]
        );
        // Nothing in it is in another menu as well.
        for command in &edit {
            let elsewhere = commands
                .iter()
                .filter(|other| other.menu != BarMenu::Edit)
                .any(|other| other.action == command.action || other.label == command.label);
            assert!(!elsewhere, "{} is only in Edit", command.label);
        }
    }

    #[test]
    fn undo_and_redo_from_the_menu_step_the_history() {
        let mut page = page();
        page.app.session.split();
        page.app.session.insert("More.");
        let before = texts(&page);
        let (undo, _) = page.app.session.history();

        choose(&mut page, BarMenu::Edit, "Undo");
        assert_eq!(page.app.session.history(), (undo - 1, 1));
        assert_ne!(texts(&page), before);

        choose(&mut page, BarMenu::Edit, "Redo");
        assert_eq!(page.app.session.history(), (undo, 0));
        assert_eq!(texts(&page), before);
    }

    /// The menu asks the window for a copy, a cut or a paste, which comes back as the keys' do,
    /// so what kame copied, its notes and formatting, goes with the words as it does with them.
    #[test]
    fn cut_copy_and_paste_from_the_menu_keep_what_kame_copied() {
        let mut page = page();
        select(&mut page, 0, 0, "Some".len());
        assert!(page
            .app
            .session
            .add_annotation(AnnotationKind::Footnote)
            .is_some());
        select(&mut page, 0, 0, "Some".len());

        choose(&mut page, BarMenu::Edit, "Copy");
        assert_eq!(page.clipboard, "Some");
        assert_eq!(texts(&page), ["Some words."]);

        let end = Cursor {
            block_id: page.app.session.document().blocks[0].id,
            offset: "Some words.".len(),
        };
        page.app.session.set_caret(end);
        choose(&mut page, BarMenu::Edit, "Paste");
        assert_eq!(texts(&page), ["Some words.Some"]);
        assert_eq!(
            page.app.session.document().annotations.len(),
            2,
            "the footnote came with the words"
        );

        select(&mut page, 0, "Some words.".len(), "Some words.Some".len());
        page.clipboard.clear();
        choose(&mut page, BarMenu::Edit, "Cut");
        assert_eq!(page.clipboard, "Some");
        assert_eq!(texts(&page), ["Some words."]);
        assert_eq!(page.app.session.document().annotations.len(), 1);
    }

    #[test]
    fn select_all_from_the_menu_takes_the_block_then_the_document() {
        let (mut page, _, second) = list_page();
        let first = page.app.session.document().blocks[0].id;

        choose(&mut page, BarMenu::Edit, "Select All");
        let (start, end) = page.app.session.ordered_selection().unwrap();
        assert_eq!((start.block_id, start.offset), (second, 0));
        assert_eq!((end.block_id, end.offset), (second, "Second".len()));

        choose(&mut page, BarMenu::Edit, "Select All");
        let (start, end) = page.app.session.ordered_selection().unwrap();
        assert_eq!((start.block_id, start.offset), (first, 0));
        assert_eq!((end.block_id, end.offset), (second, "Second".len()));
    }

    #[test]
    fn find_from_the_menu_opens_the_find_bar() {
        let mut page = page();
        choose(&mut page, BarMenu::Edit, "Find");
        assert!(page.app.find.is_some());
    }

    #[test]
    fn the_block_kinds_from_the_menu_change_the_carets_block() {
        let mut page = page();
        let heading = |level| BlockKind::Heading {
            level,
            centered: false,
        };
        for (label, kind) in [
            ("Heading 1", heading(1)),
            ("Heading 2", heading(2)),
            ("Heading 3", heading(3)),
            ("Body Text", BlockKind::Paragraph),
        ] {
            choose(&mut page, BarMenu::Edit, label);
            assert_eq!(page.app.session.document().blocks[0].kind, kind, "{label}");
        }
    }

    #[test]
    fn link_from_the_menu_asks_for_the_selections_address() {
        let mut page = page();
        select(&mut page, 0, 0, "Some".len());
        choose(&mut page, BarMenu::Edit, "Link…");
        let prompt = page.app.link_prompt.as_ref().expect("the link prompt");
        assert_eq!(prompt.words, "Some");
    }

    #[test]
    fn the_list_items_from_the_menu_move_the_carets_item() {
        let (mut page, first, second) = list_page();
        let depth = |page: &Headless, id: BlockId| {
            let document = page.app.session.document();
            match document
                .blocks
                .iter()
                .find(|block| block.id == id)
                .unwrap()
                .kind
            {
                BlockKind::ListItem { depth, .. } => depth,
                _ => panic!("not a list item"),
            }
        };

        choose(&mut page, BarMenu::Edit, "Move Item Up");
        assert_eq!(texts(&page), ["Some words.", "Second", "First"]);
        choose(&mut page, BarMenu::Edit, "Move Item Down");
        assert_eq!(texts(&page), ["Some words.", "First", "Second"]);

        choose(&mut page, BarMenu::Edit, "Indent Item");
        assert_eq!((depth(&page, first), depth(&page, second)), (0, 1));
        choose(&mut page, BarMenu::Edit, "Outdent Item");
        assert_eq!((depth(&page, first), depth(&page, second)), (0, 0));
    }

    #[test]
    fn what_the_edit_menu_cant_do_here_is_greyed_out() {
        let folder = test_temp_dir().join(format!("commands-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut page = Headless::new(Session::open(&folder.join("doc.kame")).unwrap());
        page.frame(Vec::new());
        assert_eq!(page.app.session.history(), (0, 0));

        let can = |page: &Headless, label| edit_command(page, label).enabled;
        for label in [
            "Undo",
            "Redo",
            "Cut",
            "Copy",
            "Link…",
            "Move Item Up",
            "Move Item Down",
            "Indent Item",
            "Outdent Item",
        ] {
            assert!(!can(&page, label), "{label} can't act here");
        }
        for label in ["Paste", "Select All", "Find", "Heading 1", "Body Text"] {
            assert!(can(&page, label), "{label} can");
        }
        // As the menu draws them.
        open_menu(&mut page, BarMenu::Edit);
        let undo = row(&page, BarMenu::Edit, "Undo");
        let paste = row(&page, BarMenu::Edit, "Paste");
        assert!(middle_of(&page, &undo).is_some() && !page.can_press(&undo));
        assert!(page.can_press(&paste));

        // Something to undo, words chosen and the caret in a list, and they can.
        let (mut page, _, _) = list_page();
        select(&mut page, 2, 0, "Se".len());
        for label in EDIT_ROWS.iter().filter(|label| **label != "Redo") {
            assert!(can(&page, label), "{label} can now");
        }
    }

    #[test]
    fn a_read_only_document_greys_out_what_would_change_it() {
        let (mut page, _, _) = list_page();
        select(&mut page, 2, 0, "Se".len());
        page.app.session.mark_newer();

        for label in ["Copy", "Select All", "Find"] {
            assert!(
                edit_command(&page, label).enabled,
                "{label} changes nothing"
            );
        }
        for label in EDIT_ROWS
            .iter()
            .filter(|label| !["Copy", "Select All", "Find"].contains(label))
        {
            let command = edit_command(&page, label);
            assert!(!command.enabled, "{label} is refused");
            assert!(
                command.why_not.is_some_and(|why| why.contains("updated")),
                "{label} says why"
            );
        }
    }

    /// The rows that change the document, those whose keys are refused while a dialog has the
    /// keyboard.
    const EDIT_WRITES: [&str; 13] = [
        "Undo",
        "Redo",
        "Cut",
        "Paste",
        "Heading 1",
        "Heading 2",
        "Heading 3",
        "Body Text",
        "Link…",
        "Move Item Up",
        "Move Item Down",
        "Indent Item",
        "Outdent Item",
    ];

    /// Under the link prompt, which keeps the words it opened on, an undo from the menu would
    /// shift them, and setting the link would then write over others. The keys are refused
    /// there, and so is the menu.
    #[test]
    fn while_a_dialog_is_up_what_would_change_the_document_is_greyed_out() {
        let (mut page, _, _) = list_page();
        page.app.session.split();
        page.app.session.insert("More.");
        select(&mut page, 2, 0, "Se".len());
        let can = |page: &Headless, label| edit_command(page, label).enabled;
        // Redo has nothing to redo yet.
        for label in EDIT_WRITES.iter().filter(|label| **label != "Redo") {
            assert!(can(&page, label), "{label} can before the dialog");
        }

        page.app.begin_link();
        page.frame(Vec::new());
        assert!(page.app.link_prompt.is_some());

        for label in EDIT_WRITES {
            let command = edit_command(&page, label);
            assert!(!command.enabled, "{label} waits for the dialog");
            assert_eq!(command.why_not, Some("Close the dialog first."), "{label}");
        }
        for label in ["Copy", "Select All", "Find"] {
            assert!(can(&page, label), "{label} changes nothing");
        }
        open_menu(&mut page, BarMenu::Edit);
        let undo = row(&page, BarMenu::Edit, "Undo");
        assert!(middle_of(&page, &undo).is_some() && !page.can_press(&undo));
    }

    /// Each greyed-out row says what's actually in the way, and so what to do about it.
    #[test]
    fn the_greyed_out_rows_say_why() {
        let why = |page: &Headless| edit_command(page, "Undo").why_not;

        let mut page = page();
        page.app.session.mark_save_failed("disk full");
        assert_eq!(why(&page), Some("The last save failed, so retry it first."));

        let mut page = self::page();
        page.app.session.mark_newer();
        assert_eq!(
            why(&page),
            Some("This document is read-only until kame is updated.")
        );

        let mut page = self::page();
        page.app
            .session
            .mark_held_elsewhere("doc.kame is open in another kame".to_string());
        assert_eq!(
            why(&page),
            Some("Another kame has this file open, so save it under another name first.")
        );

        let mut page = self::page();
        let path = page.app.session.path().unwrap().to_path_buf();
        std::fs::rename(&path, path.with_extension("moved")).unwrap();
        std::fs::write(&path, b"someone else's").unwrap();
        assert!(page.app.session.check_file());
        assert!(page.app.session.replaced && !page.app.session.held_elsewhere);
        assert_eq!(
            why(&page),
            Some("This file changed on disk, so save it under another name first.")
        );
    }

    /// After a failed save the keys for a cut and a link do what the greyed-out rows say:
    /// nothing. A cut that only copied would seem to have moved the words.
    #[test]
    fn after_a_failed_save_the_cut_and_link_keys_are_refused_as_the_menu_is() {
        let mut page = page();
        select(&mut page, 0, 0, "Some".len());
        page.app.session.mark_save_failed("disk full");
        assert!(!edit_command(&page, "Cut").enabled);
        assert!(!edit_command(&page, "Link…").enabled);

        page.frame(vec![egui::Event::Cut]);
        assert_eq!(page.clipboard, "");
        assert!(page.app.clipped.is_none());
        assert_eq!(texts(&page), ["Some words."]);

        press(&mut page, egui::Key::K, egui::Modifiers::COMMAND);
        assert!(page.app.link_prompt.is_none());

        // Copy changes nothing, so it still works.
        page.frame(vec![egui::Event::Copy]);
        assert_eq!(page.clipboard, "Some");
    }

    #[test]
    fn the_palette_finds_the_edit_commands_and_runs_them() {
        let (mut page, _, second) = list_page();
        let commands = page.app.commands();
        let first = narrow_commands("move item up", &commands)[0];
        assert_eq!(commands[first].full_name(), "Edit: Move Item Up");

        palette_chord(&mut page);
        type_text(&mut page, "select all");
        page.frame(Vec::new());
        assert!(page.can_press("Edit: Select All"));
        press(&mut page, egui::Key::Enter, egui::Modifiers::NONE);

        let (start, end) = page.app.session.ordered_selection().unwrap();
        assert_eq!((start.block_id, start.offset), (second, 0));
        assert_eq!((end.block_id, end.offset), (second, "Second".len()));

        // What can't act here isn't listed.
        let page = self::page();
        let commands = page.app.commands();
        assert!(!narrow_commands("move item", &commands)
            .iter()
            .any(|at| commands[*at].menu == BarMenu::Edit));
    }

    #[test]
    fn each_menu_shows_its_commands_from_the_list() {
        let mut page = page();
        for menu in [
            BarMenu::File,
            BarMenu::Edit,
            BarMenu::Annotate,
            BarMenu::View,
            BarMenu::Help,
        ] {
            open_menu(&mut page, menu);
            for command in page.app.commands().iter().filter(|c| c.menu == menu) {
                // Those are the buttons of a row of their own.
                if matches!(
                    command.action,
                    MenuAction::TypeSize(_) | MenuAction::InterfaceText(_)
                ) {
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
    fn the_view_menu_shows_the_interface_text_and_puts_it_back() {
        let mut page = page();
        page.app.set_interface_scale(&page.ctx, 1.3);
        open_menu(&mut page, BarMenu::View);
        assert!(page.sees("Interface text 130%"));
        assert!(page.sees("Type size 16"), "beside the type size");
        let reset = page
            .drawn
            .iter()
            .flat_map(|tree| &tree.nodes)
            .find_map(|(_, node)| node.bounds().filter(|_| node.label() == Some("Reset")))
            .expect("a Reset button");
        let at = egui::Pos2::new(
            ((reset.x0 + reset.x1) / 2.0) as f32,
            ((reset.y0 + reset.y1) / 2.0) as f32,
        );
        for pressed in [true, false] {
            page.time += 0.02;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(page.width, page.height),
                )),
                time: Some(page.time),
                events: vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            let app = &mut page.app;
            let output = page.ctx.run_ui(input, |ui| app.show_status(ui));
            output.drop_without_applying_deltas();
        }
        assert_eq!(page.app.interface_scale, crate::interface::DEFAULT_SCALE);
    }

    #[test]
    fn the_help_names_the_key() {
        let row = crate::HELP_ROWS.iter().find(|(key, _)| *key == "Ctrl-P");
        assert!(row.is_some_and(|(_, what)| what.contains("command")));
    }
}
