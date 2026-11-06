//! How big the interface's text is: the menus, the title and status line, the notebook's browser
//! and lists, the palette, the help and every window and notice, all of which egui draws. The
//! page and its margin are set at the type size instead, which this leaves alone.
//!
//! It's one number, a scale of egui's own sizes, kept in the settings beside the type size. The
//! interface grows by egui's text styles and the sizes that go with them, and kame's own fixed
//! widths go through [`room`], so that nothing clips. It never touches `pixels_per_point`, which
//! would grow the page as well.

use eframe::egui;

/// The interface's text as egui sets it, which is how it looked before it could be changed.
pub(crate) const DEFAULT_SCALE: f32 = 1.0;
/// How far a step takes it: a tenth, about what a step of the type size does to the page.
pub(crate) const STEP: f32 = 1.0 / STEPS;
const STEPS: f32 = 10.0;
/// Its limits, between which the window stays usable at its smallest.
pub(crate) const MIN_SCALE: f32 = 0.8;
pub(crate) const MAX_SCALE: f32 = 2.0;

/// Which size a key changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sized {
    /// The page's type.
    Type,
    /// The interface's text.
    Interface,
}

/// What a key does to a size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Larger,
    Smaller,
    Reset,
}

/// `scale` as kame keeps it: within the limits and on a step, so that a step up and one back
/// come back to where they started. Anything unreadable is the default.
pub(crate) fn within(scale: f32) -> f32 {
    if !scale.is_finite() {
        return DEFAULT_SCALE;
    }
    ((scale * STEPS).round() / STEPS).clamp(MIN_SCALE, MAX_SCALE)
}

/// Sets egui's text styles, and the sizes that go with them, to `scale` times egui's own, in
/// the light style and the dark alike.
///
/// What a widget is sized by beside its text grows too: the least height of a row or a button,
/// a checkbox's box and a folder's arrow, the indent, and the widths egui starts a field, a menu
/// or a tooltip at.
pub(crate) fn apply(ctx: &egui::Context, scale: f32) {
    let own = egui::Style::default();
    ctx.all_styles_mut(|style| {
        for (name, font) in style.text_styles.iter_mut() {
            if let Some(usual) = own.text_styles.get(name) {
                font.size = usual.size * scale;
            }
        }
        let (usual, spacing) = (&own.spacing, &mut style.spacing);
        spacing.interact_size = usual.interact_size * scale;
        spacing.icon_width = usual.icon_width * scale;
        spacing.icon_width_inner = usual.icon_width_inner * scale;
        spacing.icon_spacing = usual.icon_spacing * scale;
        spacing.indent = usual.indent * scale;
        spacing.combo_width = usual.combo_width * scale;
        spacing.combo_height = usual.combo_height * scale;
        spacing.text_edit_width = usual.text_edit_width * scale;
        spacing.slider_width = usual.slider_width * scale;
        spacing.tooltip_width = usual.tooltip_width * scale;
        spacing.menu_width = usual.menu_width * scale;
    });
    ctx.data_mut(|data| data.insert_temp(scale_id(), scale));
}

fn scale_id() -> egui::Id {
    egui::Id::unique("kame-interface-scale")
}

/// The scale the interface is drawn at now.
pub(crate) fn scale_of(ctx: &egui::Context) -> f32 {
    ctx.data(|data| data.get_temp(scale_id()))
        .unwrap_or(DEFAULT_SCALE)
}

/// `points` of the interface's room, grown as its text is.
pub(crate) fn room(ctx: &egui::Context, points: f32) -> f32 {
    points * scale_of(ctx)
}

/// What `event` does to a size, if anything: Ctrl + / Ctrl - / Ctrl-0 change the type size, and
/// with Alt as well, the interface's text. `+` is Shift-= on most layouts, so the type takes
/// either, and the interface takes = with or without Shift. It goes by the key, as egui-winit
/// has it with Ctrl or Cmd down, and not by the text, which Option changes on a Mac.
pub(crate) fn size_key(event: &egui::Event) -> Option<(Sized, Change)> {
    let egui::Event::Key {
        key,
        pressed: true,
        repeat: false,
        modifiers,
        ..
    } = event
    else {
        return None;
    };
    if !modifiers.command {
        return None;
    }
    let change = match key {
        egui::Key::Plus => Change::Larger,
        egui::Key::Equals if modifiers.shift || modifiers.alt => Change::Larger,
        egui::Key::Minus => Change::Smaller,
        egui::Key::Num0 if !modifiers.shift => Change::Reset,
        _ => return None,
    };
    let sized = if modifiers.alt {
        Sized::Interface
    } else {
        Sized::Type
    };
    Some((sized, change))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_temp_dir, Headless, KameApp, KameConfig, MenuAction, DEFAULT_BODY_SIZE};
    use kame_core::session::Session;

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// A key pressed and let go with `modifiers`, then a frame to show what it did.
    fn chord(page: &mut Headless, pressed: egui::Key, modifiers: egui::Modifiers) {
        page.frame(vec![key(pressed, modifiers)]);
        page.frame(vec![egui::Event::Key {
            key: pressed,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers,
        }]);
        page.frame(Vec::new());
    }

    fn page() -> Headless {
        let mut page = Headless::new(Session::anonymous().unwrap());
        page.frame(Vec::new());
        page
    }

    /// The size of each of egui's text styles, by name.
    fn sizes(ctx: &egui::Context) -> Vec<(egui::TextStyle, f32)> {
        ctx.global_style()
            .text_styles
            .iter()
            .map(|(style, font)| (style.clone(), font.size))
            .collect()
    }

    fn egui_sizes(times: f32) -> Vec<(egui::TextStyle, f32)> {
        egui::Style::default()
            .text_styles
            .iter()
            .map(|(style, font)| (style.clone(), font.size * times))
            .collect()
    }

    const CTRL_ALT: egui::Modifiers = egui::Modifiers {
        alt: true,
        ctrl: false,
        shift: false,
        mac_cmd: false,
        command: true,
    };

    #[test]
    fn the_interface_text_starts_as_egui_sets_it() {
        assert_eq!(within(DEFAULT_SCALE), 1.0);
        assert_eq!(KameConfig::default().interface_scale, DEFAULT_SCALE);
        let page = page();
        assert_eq!(sizes(&page.ctx), egui_sizes(1.0));
        assert_eq!(scale_of(&page.ctx), 1.0);
        assert_eq!(room(&page.ctx, 200.0), 200.0);
    }

    #[test]
    fn a_step_is_a_tenth_and_the_steps_stop_at_the_limits() {
        assert_eq!(within(DEFAULT_SCALE + STEP), 1.1);
        assert_eq!(
            within(within(1.1) - STEP),
            1.0,
            "a step back is where it was"
        );
        let mut up = DEFAULT_SCALE;
        let mut down = DEFAULT_SCALE;
        for _ in 0..40 {
            up = within(up + STEP);
            down = within(down - STEP);
        }
        assert_eq!(up, MAX_SCALE);
        assert_eq!(down, MIN_SCALE);
        assert_eq!(within(5.0), MAX_SCALE);
        assert_eq!(within(0.1), MIN_SCALE);
        assert_eq!(within(1.04), 1.0, "on a step");
        assert_eq!(within(f32::NAN), DEFAULT_SCALE);
        assert_eq!(within(f32::INFINITY), DEFAULT_SCALE);
    }

    #[test]
    fn an_old_settings_file_reads_with_the_interface_text_as_it_was() {
        let old: KameConfig = serde_json::from_str(r#"{"body_size":18.0}"#).unwrap();
        assert_eq!(old.interface_scale, DEFAULT_SCALE);
        assert_eq!(old.body_size, 18.0);

        let kept = KameConfig {
            interface_scale: 1.3,
            ..KameConfig::default()
        };
        let read: KameConfig =
            serde_json::from_str(&serde_json::to_string(&kept).unwrap()).unwrap();
        assert_eq!(read.interface_scale, 1.3);
    }

    #[test]
    fn a_settings_file_from_a_newer_kame_still_reads() {
        // An older kame meets settings it doesn't know as this one met `interface_scale`.
        let newer: KameConfig =
            serde_json::from_str(r#"{"body_size":18.0,"a_later_setting":[1,2]}"#).unwrap();
        assert_eq!(newer.body_size, 18.0);
    }

    #[test]
    fn the_text_styles_and_their_sizes_follow_the_setting() {
        let mut page = page();
        let own = egui::Style::default();
        page.app.set_interface_scale(&page.ctx, 1.5);
        page.frame(Vec::new());

        assert_eq!(page.app.interface_scale, 1.5);
        assert_eq!(sizes(&page.ctx), egui_sizes(1.5));
        for style in [egui::Theme::Light, egui::Theme::Dark] {
            let style = page.ctx.style_of(style);
            let body = &style.text_styles[&egui::TextStyle::Body];
            assert_eq!(
                body.size,
                own.text_styles[&egui::TextStyle::Body].size * 1.5
            );
            assert_eq!(style.spacing.interact_size, own.spacing.interact_size * 1.5);
            assert_eq!(style.spacing.icon_width, own.spacing.icon_width * 1.5);
        }
        assert_eq!(scale_of(&page.ctx), 1.5);
        assert_eq!(room(&page.ctx, 200.0), 300.0);
        assert_eq!(
            page.app.body_size, DEFAULT_BODY_SIZE,
            "the page's type is left"
        );
        assert_eq!(page.ctx.zoom_factor(), 1.0);

        page.app.set_interface_scale(&page.ctx, DEFAULT_SCALE);
        assert_eq!(sizes(&page.ctx), egui_sizes(1.0), "back exactly as it was");
        assert_eq!(
            page.ctx.global_style().spacing.interact_size,
            own.spacing.interact_size
        );
    }

    #[test]
    fn the_setting_is_kept_with_the_others() {
        let mut page = page();
        page.app.set_interface_scale(&page.ctx, 1.2);
        assert_eq!(page.app.settings().interface_scale, 1.2);
        assert_eq!(page.app.settings().body_size, DEFAULT_BODY_SIZE);
    }

    #[test]
    fn the_window_opens_at_the_interface_text_last_set() {
        let ctx = egui::Context::default();
        let creation = eframe::CreationContext::_new_kittest(ctx.clone());
        let app = KameApp::new(
            &creation,
            Session::anonymous().unwrap(),
            None,
            KameConfig {
                interface_scale: 1.4,
                ..KameConfig::default()
            },
        );
        assert_eq!(app.interface_scale, 1.4);
        assert_eq!(sizes(&ctx), egui_sizes(1.4));
    }

    #[test]
    fn the_keys_are_told_from_the_type_size_s_by_alt() {
        let command = egui::Modifiers::COMMAND;
        let shift = command | egui::Modifiers::SHIFT;
        let alt_shift = CTRL_ALT | egui::Modifiers::SHIFT;
        let cases = [
            (egui::Key::Plus, shift, Some((Sized::Type, Change::Larger))),
            (
                egui::Key::Equals,
                shift,
                Some((Sized::Type, Change::Larger)),
            ),
            (
                egui::Key::Minus,
                command,
                Some((Sized::Type, Change::Smaller)),
            ),
            (egui::Key::Num0, command, Some((Sized::Type, Change::Reset))),
            (
                egui::Key::Equals,
                CTRL_ALT,
                Some((Sized::Interface, Change::Larger)),
            ),
            (
                egui::Key::Plus,
                alt_shift,
                Some((Sized::Interface, Change::Larger)),
            ),
            (
                egui::Key::Equals,
                alt_shift,
                Some((Sized::Interface, Change::Larger)),
            ),
            (
                egui::Key::Minus,
                CTRL_ALT,
                Some((Sized::Interface, Change::Smaller)),
            ),
            (
                egui::Key::Num0,
                CTRL_ALT,
                Some((Sized::Interface, Change::Reset)),
            ),
            (egui::Key::Num0, alt_shift, None),
            (egui::Key::Num0, shift, None),
            (egui::Key::Minus, egui::Modifiers::ALT, None),
            (egui::Key::Minus, egui::Modifiers::NONE, None),
        ];
        for (pressed, modifiers, does) in cases {
            assert_eq!(
                size_key(&key(pressed, modifiers)),
                does,
                "{pressed:?} with {modifiers:?}"
            );
        }
        let held = egui::Event::Key {
            key: egui::Key::Minus,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: CTRL_ALT,
        };
        assert_eq!(size_key(&held), None, "a key held down steps once");
    }

    #[test]
    fn the_keys_change_the_interface_text_and_leave_the_type() {
        let mut page = page();
        chord(&mut page, egui::Key::Equals, CTRL_ALT);
        assert_eq!(page.app.interface_scale, 1.1);
        chord(
            &mut page,
            egui::Key::Plus,
            CTRL_ALT | egui::Modifiers::SHIFT,
        );
        assert_eq!(page.app.interface_scale, 1.2);
        assert_eq!(sizes(&page.ctx), egui_sizes(1.2));
        chord(&mut page, egui::Key::Minus, CTRL_ALT);
        assert_eq!(page.app.interface_scale, 1.1);
        assert_eq!(page.app.body_size, DEFAULT_BODY_SIZE);
        assert_eq!(page.ctx.zoom_factor(), 1.0);
        chord(&mut page, egui::Key::Num0, CTRL_ALT);
        assert_eq!(page.app.interface_scale, DEFAULT_SCALE);
        assert_eq!(sizes(&page.ctx), egui_sizes(1.0));

        chord(
            &mut page,
            egui::Key::Plus,
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        );
        assert_eq!(
            page.app.interface_scale, DEFAULT_SCALE,
            "Ctrl + is the type's"
        );
        assert!(page.app.body_size > DEFAULT_BODY_SIZE);
    }

    #[test]
    fn the_menu_commands_change_it() {
        let mut page = page();
        let commands = page.app.commands();
        let find = |change| {
            commands
                .iter()
                .find(|command| command.action == MenuAction::InterfaceText(change))
                .unwrap_or_else(|| panic!("no command for {change:?}"))
        };
        assert_eq!(find(Change::Larger).label, "Larger interface text");
        assert_eq!(find(Change::Larger).key, Some("Ctrl-Alt-+"));
        assert_eq!(find(Change::Smaller).label, "Smaller interface text");
        assert_eq!(find(Change::Smaller).key, Some("Ctrl-Alt--"));
        assert_eq!(find(Change::Reset).label, "Reset interface text");
        assert_eq!(find(Change::Reset).key, Some("Ctrl-Alt-0"));

        page.app
            .run_menu_action(&page.ctx, MenuAction::InterfaceText(Change::Larger));
        page.app
            .run_menu_action(&page.ctx, MenuAction::InterfaceText(Change::Larger));
        assert_eq!(page.app.interface_scale, 1.2);
        page.app
            .run_menu_action(&page.ctx, MenuAction::InterfaceText(Change::Smaller));
        assert_eq!(page.app.interface_scale, 1.1);
        page.app
            .run_menu_action(&page.ctx, MenuAction::InterfaceText(Change::Reset));
        assert_eq!(page.app.interface_scale, DEFAULT_SCALE);
        assert_eq!(sizes(&page.ctx), egui_sizes(1.0));
    }

    #[test]
    fn the_help_lists_the_keys() {
        for keys in ["Ctrl-Alt +  Ctrl-Alt -", "Ctrl-Alt-0"] {
            let row = crate::HELP_ROWS.iter().find(|(row, _)| *row == keys);
            assert!(
                row.is_some_and(|(_, what)| what.contains("interface text")),
                "{keys}"
            );
        }
    }

    /// A page in a notebook, with the browser up.
    fn page_with_browser() -> Headless {
        let root = test_temp_dir().join(format!("interface-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        let mut session = Session::open(&root.join("doc.kame")).unwrap();
        session.insert("Some words.");
        let mut page = Headless::new(session);
        page.app.nb.browser_open = true;
        page.frame(Vec::new());
        page.frame(Vec::new());
        page
    }

    /// Where the browser's title, cut to fit the browser, ends.
    fn title_end(page: &Headless) -> f32 {
        page.drawn
            .iter()
            .flat_map(|tree| &tree.nodes)
            .find_map(|(_, node)| {
                let title = node
                    .value()
                    .is_some_and(|text| text.starts_with("interface-"));
                node.bounds().filter(|_| title)
            })
            .expect("the browser's title is drawn")
            .x1 as f32
    }

    #[test]
    fn the_browser_grows_with_the_interface_text() {
        let mut page = page_with_browser();
        // The notebook's long name is cut where the browser ends.
        let usual = title_end(&page);
        page.app.set_interface_scale(&page.ctx, 1.5);
        page.frame(Vec::new());
        page.frame(Vec::new());
        let grown = title_end(&page);
        assert!(grown > usual * 1.4, "{grown} from {usual}");
    }

    #[test]
    fn a_list_grows_with_the_interface_text_and_stays_in_the_window() {
        let mut page = page_with_browser();
        chord(&mut page, egui::Key::L, egui::Modifiers::COMMAND);
        let list = |page: &Headless| {
            page.ctx
                .memory(|memory| memory.area_rect(egui::Id::unique("kame-jump")))
                .expect("the jump list is up")
        };
        let usual = list(&page).width();
        page.app.set_interface_scale(&page.ctx, 1.4);
        for _ in 0..4 {
            page.frame(Vec::new());
        }
        assert!(
            list(&page).width() > usual * 1.3,
            "{} from {usual}",
            list(&page).width()
        );

        page.app.set_interface_scale(&page.ctx, MAX_SCALE);
        for _ in 0..4 {
            page.frame(Vec::new());
        }
        let screen =
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(page.width, page.height));
        assert!(screen.contains_rect(list(&page)), "{:?}", list(&page));
    }
}
