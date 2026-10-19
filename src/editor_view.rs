//! Raw text mode: a plain code editor over the whole document.

use eframe::egui;

/// Returns true when the text changed this frame.
pub fn show(ui: &mut egui::Ui, json_text: &mut String, error: Option<&str>) -> bool {
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

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let response = ui.add_sized(
                [available.x - 16.0, available.y - 16.0],
                egui::TextEdit::multiline(json_text)
                    .id_salt("raw_editor")
                    .code_editor()
                    .desired_width(f32::INFINITY),
            );
            response.changed()
        })
        .inner
}
