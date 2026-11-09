//! Preferences ▸ Open presentations in tabs (off by default).
//!
//! With it on, the open presentations get a row of tabs under the title bar, as a file manager
//! has: a click switches, × (or Ctrl+W, or a middle click) closes and asks about unsaved changes
//! first, + makes a new presentation, a drag reorders, and Ctrl+Tab / Ctrl+Shift+Tab cycle. Each
//! presentation keeps its own zoom, view and scroll position while another one is shown (its
//! slide, selection and undo history live in its document already). Opening a file that is
//! already open switches to its tab, and a file the system opens while a window is running (a
//! double-click in the file manager) goes to that window's tabs: see [`Handoff`].
//!
//! With it off nothing here runs: the window shows one presentation at a time and every launch is
//! its own window, as before.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use egui::{Color32, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::json;

use crate::icons::{self, Icon};
use crate::theme::{self, Tokens};
use crate::{SlideApp, ViewMode};

/// The tab row's height.
pub const ROW_H: f32 = 36.0;
const TAB_H: f32 = 30.0;
/// Tabs share the row down to this width, then the row scrolls.
const MIN_W: f32 = 120.0;
const MAX_W: f32 = 232.0;
const PLUS_W: f32 = 34.0;
const LIST_W: f32 = 34.0;
const RADIUS: u8 = 8;
/// Seconds a tab takes to slide to its new place.
const SLIDE_SECS: f32 = 0.14;
/// Batches of files from other launches waiting for the window (a dialog or a show is open).
const MAX_WAITING: usize = 64;

/// What a presentation's tab remembers while another one is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocView {
    pub zoom: Option<f32>,
    pub view: ViewMode,
    pub sorter_zoom: f32,
}

/// Files another launch of the app handed to this window (see `apps/deckcraft`'s
/// `single_instance`). The host reads `enabled` to decide whether a launch hands its files over
/// (tabs on) or opens its own window (tabs off); the window keeps it equal to the preference.
pub struct Handoff {
    pub enabled: Arc<AtomicBool>,
    pub files: Receiver<Vec<String>>,
}

/// A tab being dragged: its presentation and where the pointer holds it.
#[derive(Clone, Copy, Debug)]
struct Drag {
    uid: u64,
    grab: f32,
}

/// The tab row's state.
#[derive(Default)]
pub struct Tabs {
    /// Each presentation's view, by uid, recorded at the end of every frame for the one shown.
    views: HashMap<u64, DocView>,
    /// The presentation whose view the window shows now.
    shown: Option<u64>,
    drag: Option<Drag>,
    /// How far the row is scrolled when the tabs don't fit.
    pub scroll: f32,
    /// The presentation last scrolled into view.
    revealed: Option<u64>,
    handoff: Option<Handoff>,
    waiting: Vec<Vec<String>>,
    /// A tab in the back being closed (it came forward to ask about unsaved changes) and the
    /// presentation to go back to once it has closed.
    closing: Option<(u64, u64)>,
}

enum Action {
    Select(usize),
    Close(usize),
    Move(usize, usize),
    New,
}

/// The same file, by path: the same string, or the same file once the file system resolves both.
fn same_file(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let (Ok(x), Ok(y)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        return x == y;
    }
    false
}

impl SlideApp {
    /// Take the files other launches hand to this window (desktop shell only).
    pub fn with_handoff(mut self, handoff: Handoff) -> Self {
        handoff.enabled.store(self.ui.tabs, Ordering::Relaxed);
        self.doc_tabs.handoff = Some(handoff);
        self
    }

    /// The index of the open presentation saved at `path`.
    pub(crate) fn open_index_of(&self, path: &str) -> Option<usize> {
        self.session.documents().iter().position(|d| d.path.as_deref().is_some_and(|p| same_file(p, path)))
    }

    /// With tabs on, a file that is already open isn't opened twice: its tab comes forward.
    /// Returns whether it did.
    pub(crate) fn focus_open_tab(&mut self, path: &str) -> bool {
        if !self.ui.tabs {
            return false;
        }
        let Some(i) = self.open_index_of(path) else { return false };
        let _ = self.run("window.next", json!({"index": i}));
        self.close_backstage();
        let name = self.session.active().map(|d| d.title()).unwrap_or_default();
        self.set_status(format!("{name} is already open"));
        sync(self);
        true
    }

    /// Ctrl+Tab and Ctrl+Shift+Tab: the next or previous tab, round the end.
    pub(crate) fn cycle_tab(&mut self, back: bool) {
        let n = self.session.documents().len();
        let Some(i) = self.session.active_index() else { return };
        if n < 2 {
            return;
        }
        let next = if back { (i + n - 1) % n } else { (i + 1) % n };
        let _ = self.run("window.next", json!({"index": next}));
        sync(self);
    }

    /// Closes the presentation in tab `i`. A tab in the back comes forward first, so the
    /// unsaved-changes question is about the presentation it names; once it has closed, the tab
    /// that was active is active again.
    pub(crate) fn close_tab(&mut self, i: usize) {
        let Some(closing) = self.session.documents().get(i).map(|d| d.uid) else { return };
        let back = self.session.active().map(|d| d.uid);
        if let Some(back) = back.filter(|b| *b != closing) {
            let _ = self.run("window.next", json!({"index": i}));
            sync(self);
            self.doc_tabs.closing = Some((closing, back));
        }
        self.close_presentation();
        return_after_close(self);
    }
}

/// Once a tab closed from the back has closed (at once, or after its unsaved-changes question),
/// the tab that was active before comes back. Kept, it stays forward (Cancel).
fn return_after_close(app: &mut SlideApp) {
    let Some((closing, back)) = app.doc_tabs.closing else { return };
    if app.dialog.is_some() {
        return;
    }
    app.doc_tabs.closing = None;
    let docs = app.session.documents();
    if docs.iter().all(|d| d.uid != closing)
        && let Some(i) = docs.iter().position(|d| d.uid == back)
    {
        let _ = app.run("window.next", json!({"index": i}));
    }
}

/// Files handed over by other launches: open them as tabs and bring the window forward. They
/// wait while a dialog or the slide show is up (both act on the presentation shown), or while a
/// file is still being opened.
pub(crate) fn drain_handoff(app: &mut SlideApp, ctx: &egui::Context) {
    let Some(h) = &app.doc_tabs.handoff else { return };
    h.enabled.store(app.ui.tabs, Ordering::Relaxed);
    let arrived: Vec<Vec<String>> = h.files.try_iter().take(MAX_WAITING).collect();
    if !arrived.is_empty() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        for files in arrived {
            if app.doc_tabs.waiting.len() < MAX_WAITING {
                app.doc_tabs.waiting.push(files);
            }
        }
    }
    if app.doc_tabs.waiting.is_empty() || app.dialog.is_some() || app.show.is_some() || app.opening.is_some() {
        return;
    }
    for files in std::mem::take(&mut app.doc_tabs.waiting) {
        for (file, e) in app.open_from_system(&files) {
            app.notify(format!("Couldn't open {file}: {e}"));
        }
    }
    sync(app);
    ctx.request_repaint();
}

/// Shows the view the active presentation had when it was last shown. Call after anything that
/// may change the active presentation; it does nothing when that is still the one shown.
pub(crate) fn sync(app: &mut SlideApp) {
    if !app.ui.tabs {
        app.doc_tabs.shown = None;
        app.doc_tabs.views.clear();
        app.doc_tabs.closing = None;
        return;
    }
    return_after_close(app);
    let cur = app.session.active().map(|d| d.uid);
    if cur == app.doc_tabs.shown {
        return;
    }
    let live: HashSet<u64> = app.session.documents().iter().map(|d| d.uid).collect();
    app.doc_tabs.views.retain(|uid, _| live.contains(uid));
    if let Some(uid) = cur {
        match app.doc_tabs.views.get(&uid).copied() {
            Some(v) => {
                app.ui.zoom = v.zoom;
                app.ui.view = v.view;
                app.ui.sorter_zoom = v.sorter_zoom;
            }
            // A presentation new to the row starts fitted to the window.
            None if app.doc_tabs.shown.is_some() => app.ui.zoom = None,
            None => {}
        }
    }
    app.doc_tabs.shown = cur;
}

/// Records the shown presentation's view, at the end of a frame. A presentation that became active
/// during the frame without [`sync`] gets its own view on the next frame instead.
pub(crate) fn remember(app: &mut SlideApp) {
    if !app.ui.tabs {
        return;
    }
    let cur = app.session.active().map(|d| d.uid);
    if cur != app.doc_tabs.shown {
        return;
    }
    if let Some(uid) = cur {
        app.doc_tabs.views.insert(uid, DocView { zoom: app.ui.zoom, view: app.ui.view, sorter_zoom: app.ui.sorter_zoom });
    }
}

/// The row behind the tabs, a step darker than the window, which the active tab joins.
fn row_fill(t: &Tokens) -> Color32 {
    if t.dark { t.canvas } else { t.hover }
}

/// One tab, as the row draws it.
struct TabInfo {
    uid: u64,
    title: String,
    tip: String,
    dirty: bool,
}

/// The tab row under the title bar (tabs on, a presentation open).
pub fn row(app: &mut SlideApp, ui: &mut Ui) {
    if !app.ui.tabs || app.session.documents().is_empty() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let fill = row_fill(&t);
    egui::Panel::top("doc_tabs").exact_size(ROW_H).frame(egui::Frame::NONE.fill(fill)).show(ui, |ui| {
        // A dialog acts on the presentation shown: no switching under it.
        if app.dialog.is_some() {
            ui.disable();
        }
        let action = tabs_ui(app, ui, &t, fill);
        match action {
            Some(Action::Select(i)) => {
                let _ = app.run("window.next", json!({"index": i}));
            }
            Some(Action::Close(i)) => app.close_tab(i),
            Some(Action::Move(from, to)) => {
                let _ = app.run("window.move", json!({"from": from, "to": to}));
            }
            Some(Action::New) => {
                let _ = app.run("file.new", json!({}));
            }
            None => {}
        }
        sync(app);
    });
}

/// Where the row puts things this frame.
struct Layout {
    /// The whole row.
    rect: Rect,
    /// Inside the row's margins.
    full: Rect,
    /// Where the tabs are; it scrolls when they don't fit.
    strip: Rect,
    tab_w: f32,
    /// The tabs don't fit: the row scrolls and lists them all.
    overflow: bool,
    /// The width of all the tabs side by side.
    content: f32,
}

/// Tabs share the row; past their least width the row scrolls.
fn layout(rect: Rect, n: usize) -> Layout {
    let full = Rect::from_min_max(pos2(rect.min.x + 8.0, rect.min.y), pos2(rect.max.x - 8.0, rect.max.y));
    let fit_w = (full.width() - PLUS_W) / n.max(1) as f32;
    let overflow = fit_w < MIN_W;
    let tab_w = fit_w.clamp(MIN_W, MAX_W);
    let strip = Rect::from_min_max(full.min, pos2(full.max.x - PLUS_W - if overflow { LIST_W } else { 0.0 }, full.max.y));
    Layout { rect, full, strip, tab_w, overflow, content: tab_w * n as f32 }
}

/// The empty part of the row moves the window, like the title bar.
fn window_drag(ui: &mut Ui, rect: Rect) {
    let back = ui.interact(rect, ui.scope_id().with("doc_tabs_drag"), Sense::click_and_drag());
    back.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Window, true, "Presentation tabs"));
    if back.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if back.double_clicked() {
        let max = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }
}

fn tab_infos(app: &SlideApp) -> Vec<TabInfo> {
    app.session
        .documents()
        .iter()
        .map(|d| {
            let title = d.title();
            let dirty = d.is_dirty();
            let mut tip = d.path.clone().unwrap_or_else(|| title.clone());
            if dirty {
                tip.push_str("\nUnsaved changes");
            }
            TabInfo { uid: d.uid, title, tip, dirty }
        })
        .collect()
}

/// The wheel scrolls the row, and a tab that becomes active is scrolled into view. Returns the
/// scroll offset.
fn scroll_row(state: &mut Tabs, ui: &Ui, l: &Layout, active: Option<(usize, u64)>) -> f32 {
    if ui.rect_contains_pointer(l.strip) {
        let d = ui.input(|i| i.smooth_scroll_delta);
        state.scroll -= d.x + d.y;
    }
    if let Some((i, uid)) = active
        && state.revealed != Some(uid)
    {
        let left = i as f32 * l.tab_w;
        if left < state.scroll {
            state.scroll = left;
        } else if left + l.tab_w > state.scroll + l.strip.width() {
            state.scroll = left + l.tab_w - l.strip.width();
        }
        state.revealed = Some(uid);
    }
    state.scroll = state.scroll.clamp(0.0, (l.content - l.strip.width()).max(0.0));
    state.scroll
}

/// Where each tab is drawn this frame: it slides to its place, and the dragged one follows the
/// pointer. The active and dragged tabs come last, to be drawn over their neighbours.
fn place(ui: &Ui, docs: &[TabInfo], l: &Layout, scroll: f32, drag: Option<Drag>, active: Option<usize>) -> Vec<(usize, Rect)> {
    let ctx = ui.ctx();
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let top = l.rect.max.y - TAB_H;
    let mut placed: Vec<(usize, Rect)> = docs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let anim = ui.scope_id().with(("doc_tab_x", d.uid));
            let x = match (drag, pointer) {
                (Some(g), Some(p)) if g.uid == d.uid => {
                    let x = (p.x - g.grab).clamp(l.strip.min.x, (l.strip.max.x - l.tab_w).max(l.strip.min.x));
                    ctx.animate_value_with_time(anim, x, 0.0)
                }
                _ => ctx.animate_value_with_time(anim, l.strip.min.x + i as f32 * l.tab_w - scroll, SLIDE_SECS),
            };
            (i, Rect::from_min_size(pos2(x, top), vec2(l.tab_w, TAB_H)))
        })
        .collect();
    let front = |i: usize| Some(i) == active || drag.is_some_and(|g| docs.get(i).is_some_and(|d| d.uid == g.uid));
    placed.sort_by_key(|(i, _)| front(*i));
    placed
}

/// How one tab looks this frame.
struct TabLook {
    active: bool,
    hovered: bool,
    /// 0 to 1: the hover highlight fading in or out.
    hover: f32,
    /// The next tab is the active one (no divider before it).
    next_active: bool,
    last: bool,
}

/// Draws a tab's background, icon and title; returns where its close button goes.
fn paint_tab(painter: &egui::Painter, d: &TabInfo, r: Rect, look: &TabLook, t: &Tokens, fill: Color32) -> Rect {
    if look.active {
        paint_active(painter, r, t.chrome, fill);
    } else if look.hover > 0.0 {
        let c = fill.lerp_to_gamma(t.chrome, 0.6 * look.hover);
        painter.rect_filled(r.shrink2(vec2(2.0, 3.0)).translate(vec2(0.0, -1.0)), CornerRadius::same(6), c);
    }
    // A divider between two tabs that are neither active nor hovered.
    if !look.active && !look.next_active && !look.hovered && !look.last {
        let x = r.max.x - 0.5;
        painter.line_segment([pos2(x, r.center().y - 8.0), pos2(x, r.center().y + 7.0)], Stroke::new(1.0, t.border));
    }
    // A small slide as the tab's icon.
    let glyph = Rect::from_center_size(pos2(r.min.x + 19.0, r.center().y), vec2(14.0, 10.0));
    painter.rect_stroke(glyph, CornerRadius::same(2), Stroke::new(1.3, t.accent), egui::StrokeKind::Inside);
    painter.rect_filled(Rect::from_min_size(glyph.min + vec2(3.0, 3.0), vec2(8.0, 1.6)), CornerRadius::ZERO, t.accent);
    let close = Rect::from_center_size(pos2(r.max.x - 17.0, r.center().y), vec2(20.0, 20.0));
    let text_w = (close.min.x - 4.0 - (glyph.max.x + 8.0)).max(8.0);
    let ink = if look.active { t.text } else { t.text_dim };
    let mut job = egui::text::LayoutJob::simple_singleline(d.title.clone(), theme::font(12.5), ink);
    job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
    let galley = painter.layout_job(job);
    painter.galley(pos2(glyph.max.x + 8.0, r.center().y - galley.size().y / 2.0), galley, ink);
    close
}

/// What the row draws with.
struct Paint<'a> {
    painter: &'a egui::Painter,
    t: &'a Tokens,
    fill: Color32,
}

/// One tab: drawn, with its close button, and what a click on it asks for.
fn tab(app: &mut SlideApp, ui: &mut Ui, p: &Paint<'_>, docs: &[TabInfo], (i, r): (usize, Rect), l: &Layout) -> Option<Action> {
    let d = docs.get(i)?;
    let t = p.t;
    let active = app.session.active_index();
    let is_active = Some(i) == active;
    let id = ui.scope_id().with(("doc_tab", d.uid));
    let resp = ui.interact(r.intersect(l.strip), id, Sense::click_and_drag());
    let enabled = ui.is_enabled();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, is_active, &d.title));
    let hovered = resp.hovered() || resp.dragged();
    let hover = ui.ctx().animate_bool_with_time(id.with("hover"), hovered && !is_active, 0.1);
    let look = TabLook { active: is_active, hovered, hover, next_active: active == Some(i + 1), last: i + 1 == docs.len() };
    let close = paint_tab(p.painter, d, r, &look, t, p.fill);
    let x_resp = ui.interact(close, id.with("close"), Sense::click());
    x_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, format!("Close {}", d.title)));
    let ink = if is_active { t.text } else { t.text_dim };
    if hovered || x_resp.hovered() || (is_active && !d.dirty) {
        if x_resp.hovered() {
            let c = if x_resp.is_pointer_button_down_on() { t.pressed } else { t.hover };
            p.painter.rect_filled(close, CornerRadius::same(4), if is_active { c } else { c.lerp_to_gamma(t.chrome, 0.3) });
        }
        icons::paint(p.painter, close.shrink(4.0), Icon::Close, ink, false);
    } else if d.dirty {
        // Unsaved changes: a dot where the close button is, until the pointer comes.
        p.painter.circle_filled(close.center(), 3.5, ink);
    }
    let action = if x_resp.clicked() || resp.middle_clicked() {
        Some(Action::Close(i))
    } else if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            app.doc_tabs.drag = Some(Drag { uid: d.uid, grab: pos.x - r.min.x });
        }
        (!is_active).then_some(Action::Select(i))
    } else {
        (resp.clicked() && !is_active).then_some(Action::Select(i))
    };
    let _ = resp.on_hover_text(&d.tip);
    action
}

/// Dragging a tab past the middle of a neighbour swaps the two.
fn drag_target(drag: Option<Drag>, pointer: Option<egui::Pos2>, docs: &[TabInfo], l: &Layout, scroll: f32) -> Option<Action> {
    let (g, p) = (drag?, pointer?);
    let from = docs.iter().position(|d| d.uid == g.uid)?;
    let centre = p.x - g.grab + l.tab_w / 2.0 - l.strip.min.x + scroll;
    let to = ((centre / l.tab_w).floor().max(0.0) as usize).min(docs.len().saturating_sub(1));
    (to != from).then_some(Action::Move(from, to))
}

/// The New Tab button after the last tab (it stays at the right end while the row scrolls), and
/// the list of every open presentation when the tabs don't fit.
fn row_buttons(ui: &mut Ui, docs: &[TabInfo], l: &Layout, scroll: f32, active: Option<usize>, t: &Tokens) -> Option<Action> {
    let mut action = None;
    let plus_x = (l.strip.min.x + l.content - scroll).min(l.strip.max.x) + 2.0;
    let plus = Rect::from_min_size(pos2(plus_x, l.rect.center().y - 14.0), vec2(28.0, 28.0));
    if small_button(ui, plus, Icon::Plus, "New Tab", t).clicked() {
        action = Some(Action::New);
    }
    if l.overflow {
        let list = Rect::from_min_size(pos2(l.full.max.x - LIST_W + 3.0, l.rect.center().y - 14.0), vec2(28.0, 28.0));
        let r = small_button(ui, list, Icon::ChevronDown, "All Open Presentations", t);
        egui::Popup::menu(&r).width(260.0).show(|ui| {
            for (i, d) in docs.iter().enumerate() {
                let label = if d.dirty { format!("{} •", d.title) } else { d.title.clone() };
                if ui.selectable_label(Some(i) == active, label).clicked() {
                    action = Some(Action::Select(i));
                    ui.close();
                }
            }
        });
    }
    action
}

fn tabs_ui(app: &mut SlideApp, ui: &mut Ui, t: &Tokens, fill: Color32) -> Option<Action> {
    let rect = ui.max_rect();
    window_drag(ui, rect);
    let docs = tab_infos(app);
    let active = app.session.active_index();
    let l = layout(rect, docs.len());
    let scroll = scroll_row(&mut app.doc_tabs, ui, &l, active.zip(app.session.active().map(|d| d.uid)));
    if !ui.input(|i| i.pointer.any_down()) {
        app.doc_tabs.drag = None;
    }
    let drag = app.doc_tabs.drag;
    let painter = ui.painter_at(l.strip.expand2(vec2(RADIUS as f32, 0.0)).intersect(rect));
    let paint = Paint { painter: &painter, t, fill };
    let mut action = None;
    for placed in place(ui, &docs, &l, scroll, drag, active) {
        action = tab(app, ui, &paint, &docs, placed, &l).or(action);
    }
    let pointer = ui.input(|i| i.pointer.interact_pos());
    action = action.or_else(|| drag_target(app.doc_tabs.drag, pointer, &docs, &l, scroll));
    action = row_buttons(ui, &docs, &l, scroll, active, t).or(action);
    // Keep painting while a tab is held or still sliding into place.
    let settling = ui.ctx().animate_bool_with_time(ui.scope_id().with("doc_tabs_settle"), drag.is_some(), SLIDE_SECS);
    if drag.is_some() || settling > 0.0 {
        ui.ctx().request_repaint();
    }
    action
}

/// The active tab: rounded at the top and joined to the window below it by two small flares, as
/// tabs that sit on a toolbar are.
fn paint_active(painter: &egui::Painter, r: Rect, tab: Color32, row: Color32) {
    let rad = RADIUS as f32;
    painter.rect_filled(r, CornerRadius { nw: RADIUS, ne: RADIUS, sw: 0, se: 0 }, tab);
    let left = Rect::from_min_max(pos2(r.min.x - rad, r.max.y - rad), pos2(r.min.x, r.max.y));
    let right = Rect::from_min_max(pos2(r.max.x, r.max.y - rad), pos2(r.max.x + rad, r.max.y));
    for (foot, corner) in [(left, CornerRadius { se: RADIUS, ..CornerRadius::ZERO }), (right, CornerRadius { sw: RADIUS, ..CornerRadius::ZERO })] {
        painter.rect_filled(foot, CornerRadius::ZERO, tab);
        painter.rect_filled(foot, corner, row);
    }
}

/// A square icon button in the tab row.
fn small_button(ui: &mut Ui, rect: Rect, icon: Icon, label: &str, t: &Tokens) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(("doc_tabs_button", label)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));
    let painter = ui.painter();
    if resp.hovered() {
        let fill = row_fill(t).lerp_to_gamma(t.chrome, if resp.is_pointer_button_down_on() { 1.0 } else { 0.6 });
        painter.rect_filled(rect, CornerRadius::same(6), fill);
    }
    icons::paint(painter, rect.shrink(7.0), icon, t.text, !ui.is_enabled());
    resp.on_hover_text(label)
}

/// The setting and the tabs in order, for `app.tabs`: `{on, tabs: [{title, path, dirty, active}]}`.
pub fn describe(app: &SlideApp) -> serde_json::Value {
    let active = app.session.active_index();
    json!({
        "on": app.ui.tabs,
        "tabs": app.session.documents().iter().enumerate().map(|(i, d)| json!({
            "title": d.title(), "path": d.path, "dirty": d.is_dirty(), "active": Some(i) == active,
        })).collect::<Vec<_>>(),
    })
}
