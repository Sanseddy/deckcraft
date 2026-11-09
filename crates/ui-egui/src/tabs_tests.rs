//! Preferences ▸ Open presentations in tabs: the tab row, switching, closing, reordering, each
//! presentation's own view, and the default (off) staying as it was.

use deckcraft_engine::Session;
use egui::{Key, Modifiers, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use crate::{Services, SlideApp, ViewMode};

/// A window with `n` new presentations (`Presentation1`…), the last one active.
fn harness(n: usize, tabs: bool) -> Harness<'static, SlideApp> {
    let mut s = Session::new();
    for _ in 0..n {
        s.execute("file.new", &json!({})).unwrap();
    }
    let mut app = SlideApp::new(s, Services::default());
    app.ui.tabs = tabs;
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(3);
    h
}

fn active_title(h: &Harness<'_, SlideApp>) -> String {
    h.state().session.active().map(|d| d.title()).unwrap_or_default()
}

fn titles(h: &Harness<'_, SlideApp>) -> Vec<String> {
    h.state().session.documents().iter().map(|d| d.title()).collect()
}

fn saved_deck(name: &str) -> String {
    let mut s = Session::with_new();
    let dir = std::env::temp_dir().join(format!("deckcraft-tabs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name).to_string_lossy().to_string();
    s.execute("file.saveAs", &json!({"path": path})).unwrap();
    path
}

/// Off by default, and off there is no tab row: the window works as it always has, one
/// presentation shown at a time, and the same file opened twice is open twice.
#[test]
fn tabs_are_off_by_default_and_change_nothing() {
    assert!(!crate::UiState::default().tabs);
    let saved: crate::UiState = serde_json::from_str("{}").unwrap();
    assert!(!saved.tabs, "a ui.json from before the setting keeps it off");
    let mut h = harness(2, false);
    assert!(h.query_by_label("New Tab").is_none());
    assert!(h.query_by_label("Close Presentation1").is_none());
    // Ctrl+Tab is not taken from the slide.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Tab);
    h.run_steps(2);
    assert_eq!(active_title(&h), "Presentation2");
    let path = saved_deck("twice-off.deckcraft");
    h.state_mut().open_path(&path).unwrap();
    h.state_mut().open_path(&path).unwrap();
    assert_eq!(h.state().session.documents().len(), 4, "off: opened twice, as before");
}

/// On: a tab per presentation, the active one switching on a click, + making a new one.
#[test]
fn clicking_a_tab_switches_and_plus_adds_one() {
    let mut h = harness(3, true);
    assert_eq!(active_title(&h), "Presentation3");
    h.get_by_label("Presentation1").click();
    h.run_steps(3);
    assert_eq!(active_title(&h), "Presentation1");
    h.get_by_label("New Tab").click();
    h.run_steps(3);
    assert_eq!(h.state().session.documents().len(), 4);
    assert_eq!(active_title(&h), "Presentation4");
}

/// Ctrl+Tab and Ctrl+Shift+Tab go round the tabs.
#[test]
fn ctrl_tab_cycles_the_tabs() {
    let mut h = harness(3, true);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Tab);
    h.run_steps(2);
    assert_eq!(active_title(&h), "Presentation1", "past the last tab: the first");
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Tab);
    h.run_steps(2);
    assert_eq!(active_title(&h), "Presentation3");
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Tab);
    h.run_steps(2);
    assert_eq!(active_title(&h), "Presentation2");
}

/// Each presentation keeps its zoom and view while another one is shown.
#[test]
fn each_tab_keeps_its_zoom_and_view() {
    let mut h = harness(2, true);
    h.state_mut().run("view.zoom", json!({"percent": 150})).unwrap();
    h.state_mut().run("view.sorter", json!({})).unwrap();
    h.run_steps(2);
    h.get_by_label("Presentation1").click();
    h.run_steps(3);
    assert_eq!(h.state().ui.zoom, None, "a presentation new to the row starts fitted");
    assert_eq!(h.state().ui.view, ViewMode::Sorter);
    h.state_mut().run("view.normal", json!({})).unwrap();
    h.state_mut().run("view.zoom", json!({"percent": 50})).unwrap();
    h.run_steps(2);
    h.get_by_label("Presentation2").click();
    h.run_steps(3);
    assert_eq!(h.state().ui.view, ViewMode::Sorter);
    assert!(h.state().ui.zoom.is_some_and(|z| (z - 1.5).abs() < 1e-3), "{:?}", h.state().ui.zoom);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Tab);
    h.run_steps(3);
    assert_eq!(active_title(&h), "Presentation1");
    assert_eq!(h.state().ui.view, ViewMode::Normal);
    assert!(h.state().ui.zoom.is_some_and(|z| (z - 0.5).abs() < 1e-3), "{:?}", h.state().ui.zoom);
}

/// × closes a tab with nothing unsaved at once; the other presentations stay as they were.
#[test]
fn closing_a_saved_tab_leaves_the_others() {
    let mut h = harness(3, true);
    h.get_by_label("Presentation1").hover();
    h.run_steps(2);
    h.get_by_label("Close Presentation1").click();
    h.run_steps(3);
    assert_eq!(titles(&h), ["Presentation2", "Presentation3"]);
    assert_eq!(active_title(&h), "Presentation3", "closing a tab in the back leaves the active one");
}

/// A tab with unsaved changes asks first, about that presentation; Cancel keeps it and Don't Save
/// closes it alone.
#[test]
fn closing_an_unsaved_tab_asks_first() {
    let mut h = harness(2, true);
    h.state_mut().session.set_active(0);
    h.state_mut().run("slide.new", json!({})).unwrap();
    h.state_mut().session.set_active(1);
    h.run_steps(3);
    h.state_mut().run("app.closeTab", json!({"index": 0})).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().dialog.as_ref().map(|d| d.id.as_str()), Some("close"));
    assert_eq!(active_title(&h), "Presentation1", "the question is about the tab being closed");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert_eq!(h.state().session.documents().len(), 2, "Cancel keeps it");
    h.key_press_modifiers(Modifiers::COMMAND, Key::W);
    h.run_steps(3);
    h.get_by_label("Don't Save").click();
    h.run_steps(3);
    assert_eq!(titles(&h), ["Presentation2"]);
    assert!(h.state().session.active().is_some_and(|d| !d.is_dirty()));
}

/// Don't Save on a tab closed from the back goes back to the tab that was active.
#[test]
fn closing_an_unsaved_tab_in_the_back_returns_to_the_active_one() {
    let mut h = harness(3, true);
    h.state_mut().session.set_active(0);
    h.state_mut().run("slide.new", json!({})).unwrap();
    h.state_mut().session.set_active(2);
    h.run_steps(3);
    h.state_mut().run("app.closeTab", json!({"index": 0})).unwrap();
    h.run_steps(3);
    assert_eq!(active_title(&h), "Presentation1");
    h.get_by_label("Don't Save").click();
    h.run_steps(3);
    assert_eq!(titles(&h), ["Presentation2", "Presentation3"]);
    assert_eq!(active_title(&h), "Presentation3");
}

/// With tabs on, opening a file that is already open brings its tab forward instead of a copy.
#[test]
fn opening_an_open_file_switches_to_its_tab() {
    let path = saved_deck("twice-on.deckcraft");
    let mut h = harness(1, true);
    h.state_mut().open_path(&path).unwrap();
    h.state_mut().run("window.next", json!({"index": 0})).unwrap();
    h.run_steps(2);
    assert!(h.state_mut().open_from_system(std::slice::from_ref(&path)).is_empty());
    h.state_mut().open_path_in_background(&path);
    h.run_steps(2);
    assert!(h.state().opening.is_none(), "nothing to read");
    assert_eq!(h.state().session.documents().len(), 2);
    assert_eq!(h.state().session.active().and_then(|d| d.path.clone()), Some(path));
}

/// Dragging a tab over its neighbour reorders them, and the dragged one stays active.
#[test]
fn dragging_a_tab_reorders() {
    let mut h = harness(3, true);
    h.get_by_label("Presentation1").click();
    h.run_steps(3);
    let from = h.get_by_label("Presentation1").rect().center();
    let to = h.get_by_label("Presentation3").rect().center() + egui::vec2(20.0, 0.0);
    drag(&mut h, from, to);
    assert_eq!(titles(&h), ["Presentation2", "Presentation3", "Presentation1"]);
    assert_eq!(active_title(&h), "Presentation1");
}

fn drag(h: &mut Harness<'_, SlideApp>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.run_steps(1);
    h.drag_at(from);
    h.run_steps(1);
    for i in 1..=10 {
        h.hover_at(from + (to - from) * (i as f32 / 10.0));
        h.run_steps(1);
    }
    h.drop_at(to);
    h.run_steps(4);
}

/// Many presentations don't squeeze the tabs past reading: the row scrolls, keeps the active tab in
/// view and lists every presentation.
#[test]
fn many_tabs_scroll_and_are_listed() {
    let mut h = harness(14, true);
    assert!(h.query_by_label("All Open Presentations").is_some());
    assert!(h.state().doc_tabs.scroll > 0.0, "the active (last) tab is scrolled into view");
    h.get_by_label("All Open Presentations").click();
    h.run_steps(3);
    // The list names it too, over the tab row.
    let named: Vec<_> = h.get_all_by_label("Presentation2").collect();
    assert_eq!(named.len(), 2, "the tab and its entry in the open list");
    named.last().unwrap().click();
    h.run_steps(3);
    assert_eq!(active_title(&h), "Presentation2");
}

/// The setting is a command, saved with the window's settings, and Cancel in Preferences puts it
/// back.
#[test]
fn the_setting_round_trips_and_cancel_restores_it() {
    let mut h = harness(1, false);
    let r = h.state_mut().run("app.tabs", json!({"on": true})).unwrap();
    assert_eq!(r["on"], json!(true));
    assert_eq!(r["tabs"].as_array().map(Vec::len), Some(1));
    let saved: crate::UiState = serde_json::from_value(serde_json::to_value(&h.state().ui).unwrap()).unwrap();
    assert!(saved.tabs);
    h.state_mut().run("app.preferences", json!({})).unwrap();
    h.run_steps(3);
    h.get_by_label("Open presentations in tabs").click();
    h.run_steps(2);
    assert!(!h.state().ui.tabs);
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().ui.tabs, "Cancel restores it");
}

/// Indices from the control channel are checked.
#[test]
fn close_tab_rejects_a_missing_tab() {
    let mut h = harness(2, true);
    for p in [json!({"index": 2}), json!({"index": -1}), json!({"index": "x"})] {
        assert!(h.state_mut().run("app.closeTab", p.clone()).is_err(), "{p}");
    }
    assert_eq!(h.state().session.documents().len(), 2);
}
