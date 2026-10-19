use eframe::egui;
use egui_json_tree::{DefaultExpand, JsonTree};

pub fn show(ui: &mut egui::Ui, parsed: &Option<serde_json::Value>, search: &str) {
    match parsed {
        Some(val) => {
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let expand = if search.is_empty() {
                        DefaultExpand::ToLevel(1)
                    } else {
                        DefaultExpand::SearchResults(search)
                    };

                    let response = JsonTree::new("json-tree", val)
                        .default_expand(expand)
                        .show(ui);

                    if !search.is_empty() {
                        response.reset_expanded(ui);
                    }
                });
        }
        None => {
            ui.centered_and_justified(|ui| {
                ui.label("No valid JSON to display");
            });
        }
    }
}
