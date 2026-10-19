#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod block_view;
mod editor_view;
mod file_ops;
mod model;
mod tree_view;

use app::ForgeApp;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    // `json-forge.exe file.json` (also used by Explorer "Open with").
    let initial = std::env::args_os().nth(1).map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([600.0, 400.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "JSON Forge",
        options,
        Box::new(move |cc| Ok(Box::new(ForgeApp::new(cc, initial)))),
    )
}
