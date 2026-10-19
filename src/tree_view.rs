//! Left pane: read-only JSON tree. Clicking any node reports its JSON pointer
//! so the app can scroll the block view to the matching block.

use eframe::egui;
use egui_json_tree::render::DefaultRender;
use egui_json_tree::{DefaultExpand, JsonTree};

pub fn show(ui: &mut egui::Ui, parsed: &Option<serde_json::Value>, search: &str) -> Option<String> {
    let Some(val) = parsed else {
        ui.centered_and_justified(|ui| {
            ui.label("No valid JSON to display");
        });
        return None;
    };

    let mut clicked: Option<String> = None;

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // The tree auto-resets its expansion state whenever this setting
            // changes, so no manual reset is needed (doing it every frame
            // would prevent the user from collapsing nodes while searching).
            let expand = if search.is_empty() {
                DefaultExpand::ToLevel(1)
            } else {
                DefaultExpand::SearchResults(search)
            };

            JsonTree::new("json-tree", val)
                .default_expand(expand)
                .on_render(|ui, ctx| {
                    let resp = ctx
                        .render_default(ui)
                        .interact(egui::Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if resp.clicked() {
                        clicked = Some(ctx.pointer().to_json_pointer_string());
                    }
                })
                .show(ui);
        });

    clicked
}
