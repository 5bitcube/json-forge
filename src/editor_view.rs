//! Raw text mode: a plain code editor over the whole document.

use eframe::egui;

use crate::autoscroll;
use crate::find::{self, Range};
use crate::text_ops::{self, TextOp};

pub const EDITOR_ID: &str = "raw_editor";

/// Widget id of the raw editor of document `doc`.
pub fn editor_id(doc: u64) -> egui::Id {
    egui::Id::new((EDITOR_ID, doc))
}

/// Find state the raw editor needs for one frame.
pub struct RawFind<'a> {
    pub ranges: &'a [Range],
    pub current: Option<Range>,
    pub scroll: &'a mut bool,
    pub select: &'a mut bool,
    /// Paint match backgrounds (the find panel is open).
    pub highlight: bool,
}

/// Returns true when the text changed this frame. `op` receives the item
/// picked from the context menu; the app applies it.
pub fn show(
    ui: &mut egui::Ui,
    id: egui::Id,
    json_text: &mut String,
    error: Option<&str>,
    find: Option<RawFind<'_>>,
    op: &mut Option<TextOp>,
) -> bool {
    if let Some(err) = error {
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(140, 45, 45))
            .inner_margin(egui::Margin::symmetric(8, 4))
            .corner_radius(4.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.colored_label(egui::Color32::WHITE, format!("Parse error: {}", err));
            });
        ui.add_space(4.0);
    }

    let available = ui.available_size();

    let (ranges, current, highlight, mut scroll) = match &find {
        Some(f) => (f.ranges, f.current, f.highlight, *f.scroll),
        None => (&[][..], None, false, false),
    };
    if let Some(f) = &find {
        if *f.select {
            if let Some(r) = current {
                find::select_range(ui.ctx(), id, json_text, r);
            }
        }
    }

    let out = egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let size = egui::vec2(available.x - 16.0, available.y - 16.0);
            ui.allocate_ui_with_layout(size, egui::Layout::centered_and_justified(egui::Direction::TopDown), |ui| {
                let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                    let job = find::highlight_job(ui, buf.as_str(), wrap_width, ranges, current);
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                let mut edit = egui::TextEdit::multiline(json_text)
                    .id(id)
                    .code_editor()
                    .desired_width(f32::INFINITY);
                if highlight && !ranges.is_empty() {
                    edit = edit.layouter(&mut layouter);
                }
                let before = text_ops::selection_before(ui.ctx(), id);
                let out = edit.show(ui);
                text_ops::keep_selection_on_right_click(ui.ctx(), id, before, &out.response.response);
                out.response.response.context_menu(|ui| {
                    if let Some(picked) = text_ops::context_menu(ui) {
                        *op = Some(picked);
                    }
                });
                if scroll {
                    if let Some(r) = current {
                        find::scroll_to_range(ui, &out, json_text, r);
                    }
                    scroll = false;
                }
                out.response.response.changed()
            })
            .inner
        });
    autoscroll::record(ui, autoscroll::MAIN, &out);
    let changed = out.inner;

    if let Some(f) = find {
        *f.scroll = scroll;
        *f.select = false;
    }
    changed
}
