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
            ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                let gutter = Gutter::new(ui, json_text);
                let (gutter_rect, _) = ui.allocate_exact_size(egui::vec2(gutter.width, size.y), egui::Sense::hover());
                let text_width = (size.x - gutter.width - ui.spacing().item_spacing.x).max(0.0);

                let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                    let job = find::layout_job(ui, buf.as_str(), wrap_width, ranges, current, &[]);
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                let mut edit = egui::TextEdit::multiline(json_text)
                    .id(id)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .min_size(egui::vec2(text_width, size.y));
                if highlight && !ranges.is_empty() {
                    edit = edit.layouter(&mut layouter);
                }
                let out = edit.show(ui);
                gutter.paint(ui, gutter_rect, &out);
                text_ops::after_show(ui.ctx(), id, &out, json_text);
                if let Some(picked) = text_ops::show_menu(ui.ctx(), id, &out, json_text, &[]) {
                    *op = Some(picked);
                }
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

/// Line numbers down the left of the editor. Parse errors and the find
/// panel report positions by line, so the gutter makes them findable.
struct Gutter {
    font: egui::FontId,
    width: f32,
    pad: f32,
}

impl Gutter {
    fn new(ui: &egui::Ui, text: &str) -> Self {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let lines = text.bytes().filter(|&b| b == b'\n').count() + 1;
        let digits = lines.to_string().len().max(2);
        let glyph = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let pad = 6.0;
        Self { font, width: digits as f32 * glyph + 2.0 * pad, pad }
    }

    /// Number every row of the editor's galley that starts a line, aligned
    /// with the text. Only rows inside the clip rect are painted.
    fn paint(&self, ui: &egui::Ui, gutter_rect: egui::Rect, out: &egui::text_edit::TextEditOutput) {
        let text_rect = out.response.response.rect;
        let column = egui::Rect::from_x_y_ranges(gutter_rect.x_range(), gutter_rect.top()..=text_rect.bottom().max(gutter_rect.bottom()));
        let painter = ui.painter();
        let visuals = ui.visuals();
        painter.rect_filled(column, 0.0, visuals.faint_bg_color);
        painter.vline(column.right(), column.y_range(), visuals.widgets.noninteractive.bg_stroke);

        let clip = ui.clip_rect();
        let color = visuals.weak_text_color();
        let x = column.right() - self.pad;
        let mut line = 1usize;
        let mut starts_line = true;
        for row in &out.galley.rows {
            let top = out.galley_pos.y + row.pos.y;
            if starts_line && top + row.row.height() >= clip.top() && top <= clip.bottom() {
                painter.text(egui::pos2(x, top), egui::Align2::RIGHT_TOP, line.to_string(), self.font.clone(), color);
            }
            if starts_line {
                line += 1;
            }
            starts_line = row.ends_with_newline;
        }
    }
}
