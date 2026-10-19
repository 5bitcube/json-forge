use eframe::egui;

pub fn show(ui: &mut egui::Ui, json_text: &mut String) -> bool {
    let available = ui.available_size();

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let response = ui.add_sized(
                [available.x - 16.0, available.y - 16.0],
                egui::TextEdit::multiline(json_text)
                    .code_editor()
                    .desired_width(f32::INFINITY),
            );
            response.changed()
        })
        .inner
}
