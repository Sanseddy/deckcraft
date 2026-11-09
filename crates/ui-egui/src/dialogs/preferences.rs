use super::{Dialog, SlideApp, Value, json, theme};

/// What the Preferences dialog can change, as it was when the dialog opened.
pub(super) fn preferences_snapshot(app: &SlideApp) -> Value {
    json!({
        "dark": app.ui.brightness == theme::Brightness::Dark,
        "uiScale": app.ui.ui_scale,
        "interfaceLanguage": app.ui.interface_language,
        "tabs": app.ui.tabs,
        "prefs": serde_json::to_value(&app.session.prefs).unwrap_or(Value::Null),
    })
}

/// Choose an interface language while retaining the original settings for Cancel.
pub(super) fn interface_language_row(app: &mut SlideApp, ui: &mut egui::Ui, d: &mut Dialog) {
    if d.params.get("before").is_none() {
        d.params = json!({ "before": preferences_snapshot(app) });
    }
    let mut language = app.ui.interface_language;
    ui.horizontal(|ui| {
        let label = ui.label(language.text("Interface language"));
        let combo = craft_ui::ComboBox::from_id_salt("interface-language").selected_text(language.display_name()).show_ui(ui, |ui| {
            for option in [crate::i18n::Language::English, crate::i18n::Language::Russian] {
                ui.selectable_value(&mut language, option, option.display_name());
            }
        });
        let _ = combo.response.labelled_by(label.id);
    });
    if language != app.ui.interface_language {
        let _ = app.run("app.language", json!({ "language": language }));
        crate::i18n::set_context(ui.ctx(), language);
        ui.ctx().request_repaint();
    }
    ui.separator();
}

/// Puts back the settings the Preferences dialog opened with (Cancel, Esc, the close button).
pub(super) fn restore_preferences(app: &mut SlideApp, d: &Dialog) {
    let Some(before) = d.params.get("before") else { return };
    if let Some(language) = before.get("interfaceLanguage").and_then(|v| serde_json::from_value::<crate::i18n::Language>(v.clone()).ok()) {
        let _ = app.run("app.language", json!({ "language": language }));
    }
    if let Some(dark) = before.get("dark").and_then(Value::as_bool)
        && dark != (app.ui.brightness == theme::Brightness::Dark)
    {
        let _ = app.run("view.dark", json!({ "on": dark }));
    }
    if let Some(scale) = before.get("uiScale").and_then(Value::as_f64) {
        app.ui.ui_scale = scale as f32;
    }
    if let Some(tabs) = before.get("tabs").and_then(Value::as_bool)
        && tabs != app.ui.tabs
    {
        let _ = app.run("app.tabs", json!({ "on": tabs }));
    }
    if let Some(prefs) = before.get("prefs").and_then(|p| serde_json::from_value(p.clone()).ok()) {
        app.session.prefs = prefs;
    }
}
