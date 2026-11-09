//! The Preferences (Options) dialog: controls apply as they change and Cancel, Esc and the
//! window's close button put the settings back the way they were (#752, reported as #878).

use deckcraft_engine::Session;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, Node};
use serde_json::json;

use crate::theme::Brightness;
use crate::{Services, SlideApp};

/// The slider itself, not the label drawn beside it.
fn interface_size<'a>(h: &'a Harness<'_, SlideApp>) -> Node<'a> {
    let mut sliders = h.query_all_by_role_and_label(egui::accesskit::Role::Slider, "Interface size");
    let slider = sliders.next().expect("the slider is there");
    assert!(sliders.next().is_none(), "exactly one Interface size slider");
    slider
}

fn harness() -> Harness<'static, SlideApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        SlideApp::new(Session::with_new(), Services::default()),
    );
    // The app keeps asking for frames, so step instead of run().
    h.run_steps(3);
    h
}

fn open_options(h: &mut Harness<'_, SlideApp>) {
    h.state_mut().run("app.preferences", json!({})).expect("open Options");
    h.run_steps(3);
}

fn dark(app: &SlideApp) -> bool {
    app.ui.brightness == Brightness::Dark
}

fn choose_interface_language(h: &mut Harness<'_, SlideApp>, label: &str, option: &str) {
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, label).click();
    h.run_steps(2);
    h.get_by_label(option).click();
    h.run_steps(3);
}

/// Switching changes the live UI, keeps document text intact and persists on Done.
#[test]
fn interface_language_switches_both_ways_and_done_keeps_it() {
    use crate::i18n::Language;
    let mut h = harness();
    let document = serde_json::to_value(&h.state().session.active().unwrap().doc).unwrap();
    open_options(&mut h);
    choose_interface_language(&mut h, "Interface language", "Русский");
    assert_eq!(h.state().ui.interface_language, Language::Russian);
    h.get_by_label("Тёмное оформление");
    h.get_by_label("Главная");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Создать слайд");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Параметры: Создать слайд");
    assert_eq!(serde_json::to_value(&h.state().session.active().unwrap().doc).unwrap(), document);
    if let Some(dir) = std::env::var_os("DECKCRAFT_LANGUAGE_SHOTS") {
        let _gpu = crate::gpu_lock::gpu_lock();
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.render().unwrap().save(dir.join("russian-preferences.png")).unwrap();
    }
    h.get_by_label("Готово").click();
    h.run_steps(3);
    assert!(h.state().dialog.is_none());
    assert_eq!(h.state().ui.interface_language, Language::Russian);
    open_options(&mut h);
    choose_interface_language(&mut h, "Язык интерфейса", "English");
    assert_eq!(h.state().ui.interface_language, Language::English);
    h.get_by_label("Dark appearance");
    if let Some(dir) = std::env::var_os("DECKCRAFT_LANGUAGE_SHOTS") {
        let _gpu = crate::gpu_lock::gpu_lock();
        h.render().unwrap().save(std::path::PathBuf::from(dir).join("english-preferences.png")).unwrap();
    }
    h.get_by_label("Done").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.interface_language, Language::English);
}

/// All three dismissal routes roll a previewed language back to the saved one.
#[test]
fn interface_language_cancel_escape_and_close_restore_it() {
    for dismiss in ["Cancel", "Escape", "Close"] {
        let mut h = harness();
        open_options(&mut h);
        choose_interface_language(&mut h, "Interface language", "Русский");
        match dismiss {
            "Cancel" => h.get_by_label("Отмена").click(),
            "Escape" => h.key_press(egui::Key::Escape),
            _ => h.get_all_by_label("Close window").next().unwrap().click(),
        }
        h.run_steps(3);
        assert!(h.state().dialog.is_none(), "{dismiss}");
        assert_eq!(h.state().ui.interface_language, crate::i18n::Language::English, "{dismiss}");
        h.get_by_label("Home");
    }
}

#[test]
fn interface_language_command_rejects_unknown_values_without_changing_language() {
    let mut app = SlideApp::new(Session::with_new(), Services::default());
    assert_eq!(app.run("app.language", json!({})).unwrap(), json!({"language": "en"}));
    assert_eq!(app.run("app.language", json!({"language": "ru"})).unwrap(), json!({"language": "ru"}));
    for invalid in [json!("xx"), json!(null), json!(23), json!(["en"])] {
        assert!(app.run("app.language", json!({"language": invalid})).is_err());
        assert_eq!(app.run("app.language", json!({})).unwrap(), json!({"language": "ru"}));
    }
}

/// A checkbox applies as it is toggled and Cancel puts the old value back.
#[test]
fn options_cancel_undoes_the_changes() {
    let mut h = harness();
    let was = h.state().session.prefs.smart_guides;
    open_options(&mut h);

    h.query_by_label("Show smart guides").expect("the checkbox is there").click();
    h.run_steps(2);
    assert_eq!(h.state().session.prefs.smart_guides, !was, "the change applies as it is made");

    h.query_by_label("Cancel").expect("the Cancel button is there").click();
    h.run_steps(2);
    assert_eq!(h.state().session.prefs.smart_guides, was, "Cancel restores the setting");
    assert!(h.state().dialog.is_none(), "and the dialog closes");
}

/// Every Editing setting and the name field go back on Cancel, not only the first checkbox: a
/// reporter who changed a different control and saw Cancel do nothing has the same answer.
#[test]
fn options_cancel_undoes_every_editing_setting() {
    let mut h = harness();
    let before = h.state().session.prefs.clone();
    open_options(&mut h);

    for label in ["Snap objects to grid", "Replace straight quotes with smart quotes", "AutoCorrect as you type"] {
        h.query_by_label(label).unwrap_or_else(|| panic!("the {label} checkbox is there")).click();
        h.run_steps(1);
    }
    // The name field: it writes into the settings as it is typed.
    // The name field is the last text input of the dialog (the label is a separate widget).
    let name = h.get_all_by_role(egui::accesskit::Role::TextInput).last().expect("the name field is there");
    name.click();
    h.run_steps(1);
    h.event(egui::Event::Text("Z".into()));
    h.run_steps(1);
    let changed = h.state().session.prefs.clone();
    assert_ne!(
        (changed.snap_to_grid, changed.smart_quotes, changed.autocorrect),
        (before.snap_to_grid, before.smart_quotes, before.autocorrect),
        "the settings apply as they are changed"
    );
    assert_ne!(changed.author, before.author, "the name applies as it is typed: {:?}", changed.author);

    h.query_by_label("Cancel").expect("the Cancel button is there").click();
    h.run_steps(2);
    let after = h.state().session.prefs.clone();
    assert_eq!(after.snap_to_grid, before.snap_to_grid, "snap to grid is restored");
    assert_eq!(after.smart_quotes, before.smart_quotes, "smart quotes are restored");
    assert_eq!(after.autocorrect, before.autocorrect, "autocorrect is restored");
    assert_eq!(after.author, before.author, "the name is restored");
}

/// The appearance toggle and the interface size go back on Cancel, too; Esc acts as Cancel.
#[test]
fn options_cancel_puts_the_appearance_and_scale_back() {
    let mut h = harness();
    let (dark_before, scale_before) = (dark(h.state()), h.state().ui.ui_scale);
    open_options(&mut h);

    h.query_by_label("Dark appearance").expect("the checkbox is there").click();
    h.run_steps(2);
    assert!(dark(h.state()), "the appearance applies as it is toggled");

    // Clicking the slider's middle sets it near the middle of its range, away from the default.
    interface_size(&h).click();
    h.run_steps(2);
    let scale_in_dialog = h.state().ui.ui_scale;
    assert!((scale_in_dialog - scale_before).abs() > 1e-3, "the slider applied a new size");

    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(dark(h.state()), dark_before, "Esc restores the appearance");
    assert!((h.state().ui.ui_scale - scale_before).abs() < 1e-3, "Esc restores the size");
    assert!(h.state().dialog.is_none(), "and the dialog closes");
}

/// The window's close button is the same as Cancel: the settings go back.
#[test]
fn options_close_button_restores_the_settings() {
    let mut h = harness();
    let was = h.state().session.prefs.smart_guides;
    open_options(&mut h);

    h.query_by_label("Show smart guides").expect("the checkbox is there").click();
    h.run_steps(2);
    assert_eq!(h.state().session.prefs.smart_guides, !was, "the change applies as it is made");

    h.get_all_by_label("Close window").next().expect("the window's close button is there").click();
    h.run_steps(2);
    assert_eq!(h.state().session.prefs.smart_guides, was, "the close button restores the setting");
    assert!(h.state().dialog.is_none(), "and the dialog closes");
}
