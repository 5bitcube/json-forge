use eframe::egui;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::block_view;
use crate::file_ops;
use crate::tree_view;

pub struct ForgeApp {
    pub json_text: String,
    pub parsed: Option<serde_json::Value>,
    pub parse_error: Option<String>,
    pub file_path: Option<PathBuf>,
    pub search_query: String,
    pub modified: bool,
    pub block_buffers: HashMap<String, String>,
    last_parse_time: Instant,
    needs_reparse: bool,
    autoscroll_origin: Option<egui::Pos2>,
}

impl ForgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::light());

        let sample = r#"{
  "example": "Open a JSON file or paste JSON here",
  "array": [1, 2, 3],
  "nested": {
    "key": "value",
    "flag": true
  }
}"#;
        let parsed = serde_json::from_str(sample).ok();

        Self {
            json_text: sample.to_string(),
            parsed,
            parse_error: None,
            file_path: None,
            search_query: String::new(),
            modified: false,
            block_buffers: HashMap::new(),
            last_parse_time: Instant::now(),
            needs_reparse: false,
            autoscroll_origin: None,
        }
    }

    fn reparse(&mut self) {
        match serde_json::from_str::<serde_json::Value>(&self.json_text) {
            Ok(val) => {
                self.parsed = Some(val);
                self.parse_error = None;
                self.block_buffers.clear();
            }
            Err(e) => {
                self.parse_error = Some(format!("Line {}, col {}: {}", e.line(), e.column(), e));
            }
        }
    }

    fn window_title(&self) -> String {
        let name = self
            .file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".to_string());

        if self.modified {
            format!("JSON Forge - {} *", name)
        } else {
            format!("JSON Forge - {}", name)
        }
    }

    fn count_keys(val: &serde_json::Value) -> usize {
        match val {
            serde_json::Value::Object(map) => {
                map.len() + map.values().map(Self::count_keys).sum::<usize>()
            }
            serde_json::Value::Array(arr) => arr.iter().map(Self::count_keys).sum(),
            _ => 0,
        }
    }
}

impl eframe::App for ForgeApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Auto-scroll: middle click toggles, mouse distance controls speed
        let middle_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Middle));
        let left_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
        let right_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
        let pointer_pos = ctx.input(|i| i.pointer.hover_pos());

        if middle_clicked {
            if self.autoscroll_origin.is_some() {
                self.autoscroll_origin = None;
            } else if let Some(pos) = pointer_pos {
                self.autoscroll_origin = Some(pos);
            }
        }
        if left_clicked || right_clicked {
            self.autoscroll_origin = None;
        }

        if let Some(origin) = self.autoscroll_origin {
            let deadzone = 15.0;

            if let Some(pos) = pointer_pos {
                let dy = pos.y - origin.y;

                if dy.abs() > deadzone {
                    let speed = (dy - dy.signum() * deadzone) * 0.15;
                    ctx.input_mut(|i| {
                        i.smooth_scroll_delta.y -= speed;
                    });
                }

                if dy < -deadzone {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeNorth);
                } else if dy > deadzone {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeSouth);
                } else {
                    ctx.set_cursor_icon(egui::CursorIcon::None);
                }
            }

            // Draw Chrome-style origin indicator
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("autoscroll"),
            ));
            let r = 14.0;
            let col = egui::Color32::from_rgb(100, 100, 100);
            painter.circle_filled(origin, r, egui::Color32::from_rgba_premultiplied(245, 245, 245, 230));
            painter.circle_stroke(origin, r, egui::Stroke::new(1.0, col));
            painter.circle_filled(origin, 2.0, col);

            // Up arrow triangle
            let tip_up = origin + egui::vec2(0.0, -8.0);
            painter.add(egui::Shape::convex_polygon(
                vec![tip_up, tip_up + egui::vec2(-4.0, 5.0), tip_up + egui::vec2(4.0, 5.0)],
                col,
                egui::Stroke::NONE,
            ));
            // Down arrow triangle
            let tip_down = origin + egui::vec2(0.0, 8.0);
            painter.add(egui::Shape::convex_polygon(
                vec![tip_down, tip_down + egui::vec2(-4.0, -5.0), tip_down + egui::vec2(4.0, -5.0)],
                col,
                egui::Stroke::NONE,
            ));

            ctx.request_repaint();
        }

        let ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.mac_cmd);
        let shift = ctx.input(|i| i.modifiers.shift);

        if ctrl && !shift && ctx.input(|i| i.key_pressed(egui::Key::O)) {
            if let Some((text, path)) = file_ops::open_file() {
                self.json_text = text;
                self.file_path = Some(path);
                self.modified = false;
                self.reparse();
            }
        }
        if ctrl && !shift && ctx.input(|i| i.key_pressed(egui::Key::S)) {
            if let Some(path) = &self.file_path {
                file_ops::save_file(path, &self.json_text);
                self.modified = false;
            } else if let Some(path) = file_ops::save_file_as(&self.json_text) {
                self.file_path = Some(path);
                self.modified = false;
            }
        }
        if ctrl && shift && ctx.input(|i| i.key_pressed(egui::Key::S)) {
            if let Some(path) = file_ops::save_file_as(&self.json_text) {
                self.file_path = Some(path);
                self.modified = false;
            }
        }

        // Handle drag-and-drop
        ctx.input(|i| {
            for file in &i.raw.dropped_files {
                let path = file.path();
                if path.as_os_str().len() > 0 {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        self.json_text = text;
                        self.file_path = Some(path.to_path_buf());
                        self.modified = false;
                        self.needs_reparse = true;
                    }
                }
            }
        });

        // Debounced reparse
        if self.needs_reparse && self.last_parse_time.elapsed().as_millis() > 150 {
            self.reparse();
            self.needs_reparse = false;
        }

        // Update window title
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.window_title()));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Toolbar
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("  Open  ").clicked() {
                    if let Some((text, path)) = file_ops::open_file() {
                        self.json_text = text;
                        self.file_path = Some(path);
                        self.modified = false;
                        self.reparse();
                    }
                }
                if ui.button("  Save  ").clicked() {
                    if let Some(path) = &self.file_path {
                        file_ops::save_file(path, &self.json_text);
                        self.modified = false;
                    } else if let Some(path) = file_ops::save_file_as(&self.json_text) {
                        self.file_path = Some(path);
                        self.modified = false;
                    }
                }
                if ui.button(" Save As ").clicked() {
                    if let Some(path) = file_ops::save_file_as(&self.json_text) {
                        self.file_path = Some(path);
                        self.modified = false;
                    }
                }

                ui.separator();

                if ui.button(" Format ").clicked() {
                    if let Some(val) = &self.parsed {
                        self.json_text = serde_json::to_string_pretty(val).unwrap_or(self.json_text.clone());
                        self.modified = true;
                    }
                }
                if ui.button(" Minify ").clicked() {
                    if let Some(val) = &self.parsed {
                        self.json_text = serde_json::to_string(val).unwrap_or(self.json_text.clone());
                        self.modified = true;
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let search_width = 200.0;
                    ui.add_sized(
                        [search_width, 20.0],
                        egui::TextEdit::singleline(&mut self.search_query)
                            .hint_text("Search..."),
                    );
                    ui.label("Search:");
                });
            });
        });

        // Status bar
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(err) = &self.parse_error {
                    ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("Error: {}", err));
                } else {
                    ui.colored_label(egui::Color32::from_rgb(100, 255, 100), "Valid JSON");
                }

                ui.separator();

                if let Some(val) = &self.parsed {
                    let keys = Self::count_keys(val);
                    ui.label(format!("{} keys", keys));
                }

                ui.separator();

                if let Some(path) = &self.file_path {
                    ui.label(path.display().to_string());
                } else {
                    ui.label("No file");
                }

                ui.separator();

                let bytes = self.json_text.len();
                if bytes < 1024 {
                    ui.label(format!("{} B", bytes));
                } else {
                    ui.label(format!("{:.1} KB", bytes as f64 / 1024.0));
                }
            });
        });

        // Tree panel (left)
        egui::Panel::left("tree_panel")
            .default_size(450.0)
            .min_size(200.0)
            .show(ui, |ui| {
                tree_view::show(ui, &self.parsed, &self.search_query);
            });

        // Block view (center — must be last, fills remaining space)
        egui::CentralPanel::default().show(ui, |ui| {
            block_view::show(ui, &self.parsed, &self.search_query, &mut self.block_buffers, &mut self.modified);
        });
    }
}
