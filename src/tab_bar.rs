//! Tab strip under the menu bar: one tab per open document, Atom-style.
//! Click selects, the × or a middle click closes, + opens a new document.

use eframe::egui;
use egui::text::{LayoutJob, TextWrapping};
use egui::{Align, Color32, CornerRadius, Id, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};

pub const HEIGHT: f32 = 30.0;
const MAX_TAB_W: f32 = 220.0;
const PAD_LEFT: f32 = 12.0;
const CLOSE_BOX: f32 = 18.0;
const PAD_RIGHT: f32 = 6.0;

pub struct TabLabel {
    pub title: String,
    pub modified: bool,
    pub tooltip: String,
}

pub enum TabEvent {
    Select(usize),
    Close(usize),
    New,
}

/// Background of the strip: a shade darker than the panels so tabs read as tabs.
pub fn fill(visuals: &egui::Visuals) -> Color32 {
    visuals.widgets.inactive.weak_bg_fill
}

pub fn show(ui: &mut egui::Ui, tabs: &[TabLabel], active: usize) -> Option<TabEvent> {
    let mut event = None;
    ui.spacing_mut().item_spacing = Vec2::ZERO;
    ui.horizontal(|ui| {
        let strip_w = (ui.available_width() - HEIGHT).max(0.0);
        ui.allocate_ui_with_layout(Vec2::new(strip_w, HEIGHT), egui::Layout::left_to_right(Align::Center), |ui| {
            egui::ScrollArea::horizontal()
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    ui.horizontal(|ui| {
                        for (i, tab) in tabs.iter().enumerate() {
                            if let Some(e) = tab_item(ui, i, tab, i == active) {
                                event = Some(e);
                            }
                        }
                    });
                });
        });
        if new_tab_button(ui) {
            event = Some(TabEvent::New);
        }
    });
    event
}

fn tab_item(ui: &mut egui::Ui, i: usize, tab: &TabLabel, active: bool) -> Option<TabEvent> {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let (text_color, strong, line, accent, panel) = {
        let v = ui.visuals();
        (v.text_color(), v.strong_text_color(), v.widgets.noninteractive.bg_stroke.color, v.selection.bg_fill, v.panel_fill)
    };
    let color = if active { strong } else { text_color };

    let mut job = LayoutJob::simple_singleline(tab.title.clone(), font, color);
    job.wrap = TextWrapping::truncate_at_width(MAX_TAB_W - PAD_LEFT - CLOSE_BOX - PAD_RIGHT);
    let galley = ui.painter().layout_job(job);
    let w = galley.size().x + PAD_LEFT + CLOSE_BOX + PAD_RIGHT + 4.0;

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, HEIGHT), Sense::click());
    let close_rect = Rect::from_center_size(
        Pos2::new(rect.right() - PAD_RIGHT - CLOSE_BOX / 2.0, rect.center().y),
        Vec2::splat(CLOSE_BOX),
    );
    // Registered after the tab so it wins the hit test where they overlap.
    let close = ui.interact(close_rect, resp.id.with("close"), Sense::click());
    let hovered = resp.hovered() || close.hovered();

    let p = ui.painter();
    if active {
        p.rect_filled(rect, CornerRadius::ZERO, panel);
        p.hline(rect.x_range(), rect.top() + 1.0, Stroke::new(2.0, accent));
    } else if hovered {
        p.rect_filled(rect, CornerRadius::ZERO, text_color.gamma_multiply(0.08));
    }
    p.vline(rect.right() - 0.5, rect.y_range(), Stroke::new(1.0, line));

    let text_pos = Pos2::new(rect.left() + PAD_LEFT, rect.center().y - galley.size().y / 2.0);
    p.galley(text_pos, galley, color);

    // Unsaved tabs show a dot where the × is, until the pointer comes near.
    let c = close_rect.center();
    if close.hovered() {
        p.rect_filled(close_rect, 3.0, text_color.gamma_multiply(0.15));
    }
    if tab.modified && !hovered {
        p.circle_filled(c, 3.5, color);
    } else {
        let s = 3.5;
        let st = Stroke::new(1.3, color);
        p.line_segment([c + Vec2::new(-s, -s), c + Vec2::new(s, s)], st);
        p.line_segment([c + Vec2::new(-s, s), c + Vec2::new(s, -s)], st);
    }

    if active {
        // Bring a newly activated tab into view when the strip overflows.
        let key = Id::new("tab_bar_active_seen");
        if ui.data(|d| d.get_temp::<usize>(key)) != Some(i) {
            resp.scroll_to_me(Some(Align::Center));
            ui.data_mut(|d| d.insert_temp(key, i));
        }
    }

    let resp = resp.on_hover_text(&tab.tooltip);
    if close.clicked() || close.clicked_by(PointerButton::Middle) || resp.clicked_by(PointerButton::Middle) {
        return Some(TabEvent::Close(i));
    }
    if resp.clicked() {
        return Some(TabEvent::Select(i));
    }
    None
}

fn new_tab_button(ui: &mut egui::Ui) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(HEIGHT), Sense::click());
    let (text_color, strong) = {
        let v = ui.visuals();
        (v.text_color(), v.strong_text_color())
    };
    let color = if resp.hovered() { strong } else { text_color };
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect.shrink(5.0), 4.0, text_color.gamma_multiply(0.12));
    }
    let c = rect.center();
    let s = 5.0;
    let st = Stroke::new(1.5, color);
    p.line_segment([c - Vec2::new(s, 0.0), c + Vec2::new(s, 0.0)], st);
    p.line_segment([c - Vec2::new(0.0, s), c + Vec2::new(0.0, s)], st);
    resp.on_hover_text("New Tab (Ctrl+N)").clicked()
}
