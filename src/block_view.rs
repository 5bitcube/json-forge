use eframe::egui;
use serde_json::Value;
use std::collections::HashMap;

const COLOR_SYSTEM: egui::Color32 = egui::Color32::from_rgb(192, 57, 43);
const COLOR_USER: egui::Color32 = egui::Color32::from_rgb(39, 174, 101);
const COLOR_ASSISTANT: egui::Color32 = egui::Color32::from_rgb(123, 104, 238);
const COLOR_TOOL: egui::Color32 = egui::Color32::from_rgb(230, 126, 34);
const COLOR_THINKING: egui::Color32 = egui::Color32::from_rgb(160, 160, 160);
const COLOR_DEFAULT: egui::Color32 = egui::Color32::from_rgb(80, 80, 80);

const HEADER_BG: egui::Color32 = egui::Color32::from_rgb(235, 235, 235);
const BLOCK_BG: egui::Color32 = egui::Color32::from_rgb(250, 250, 250);
const SECTION_COLOR: egui::Color32 = egui::Color32::from_rgb(192, 57, 43);

fn char_size(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1000 {
        format!("{:.1}K", n as f64 / 1000.0)
    } else {
        format!("{}", n)
    }
}

fn value_text(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => serde_json::to_string_pretty(val).unwrap_or_default(),
    }
}

fn is_leaf(val: &Value) -> bool {
    matches!(val, Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null)
}

fn is_api_request(val: &Value) -> bool {
    val.get("system").is_some() && val.get("messages").is_some()
}

pub fn show(
    ui: &mut egui::Ui,
    parsed: &Option<Value>,
    _search: &str,
    buffers: &mut HashMap<String, String>,
    modified: &mut bool,
) {
    let Some(val) = parsed else {
        ui.centered_and_justified(|ui| {
            ui.label("No valid JSON to display");
        });
        return;
    };

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;

            render_summary(ui, val);
            ui.add_space(8.0);

            if is_api_request(val) {
                render_api_request(ui, val, buffers, modified);
            } else {
                render_generic(ui, val, "", buffers, modified);
            }
        });
}

fn render_summary(ui: &mut egui::Ui, val: &Value) {
    let total_bytes = serde_json::to_string(val).map(|s| s.len()).unwrap_or(0);

    egui::Frame::new()
        .fill(egui::Color32::from_rgb(245, 245, 245))
        .corner_radius(6.0)
        .inner_margin(12.0)
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 200, 200)))
        .show(ui, |ui| {
            egui::Grid::new("summary_grid")
                .num_columns(2)
                .spacing([20.0, 4.0])
                .show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, label: &str, value: String| {
                        ui.colored_label(egui::Color32::from_rgb(100, 100, 100), label);
                        ui.label(value);
                        ui.end_row();
                    };

                    if let Some(model) = val.get("model").and_then(|v| v.as_str()) {
                        row(ui, "Model", model.to_string());
                    }

                    row(ui, "Total size", format_bytes(total_bytes));

                    if let Some(sys) = val.get("system").and_then(|v| v.as_array()) {
                        let chars: usize = sys.iter()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .map(|s| s.len())
                            .sum();
                        row(ui, "System prompt", format!("{} chars in {} blocks", char_size(chars), sys.len()));
                    }

                    if let Some(tools) = val.get("tools").and_then(|v| v.as_array()) {
                        let json_len = serde_json::to_string(tools).map(|s| s.len()).unwrap_or(0);
                        row(ui, "Tools", format!("{} tools ({} chars)", tools.len(), char_size(json_len)));
                    }

                    if let Some(msgs) = val.get("messages").and_then(|v| v.as_array()) {
                        let json_len = serde_json::to_string(msgs).map(|s| s.len()).unwrap_or(0);
                        row(ui, "Messages", format!("{} messages ({} chars)", msgs.len(), char_size(json_len)));
                    }

                    if let Some(stream) = val.get("stream") {
                        row(ui, "Stream", value_text(stream));
                    }
                });
        });
}

fn render_api_request(
    ui: &mut egui::Ui,
    val: &Value,
    buffers: &mut HashMap<String, String>,
    modified: &mut bool,
) {
    let obj = match val.as_object() {
        Some(o) => o,
        None => return,
    };

    let skip = ["system", "messages", "tools"];
    let others: Vec<_> = obj.iter()
        .filter(|(k, _)| !skip.contains(&k.as_str()))
        .collect();

    if !others.is_empty() {
        section_header(ui, &format!("Config ({} fields)", others.len()));
        for (key, val) in &others {
            render_block(ui, key, &value_text(val), COLOR_DEFAULT, None, buffers, modified);
        }
        ui.add_space(8.0);
    }

    if let Some(sys) = val.get("system").and_then(|v| v.as_array()) {
        section_header(ui, &format!("System ({} blocks)", sys.len()));
        for (i, block) in sys.iter().enumerate() {
            let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let cache = block.get("cache_control")
                .and_then(|c| c.get("ttl"))
                .and_then(|t| t.as_str());
            let label = if let Some(ttl) = cache {
                format!("system[{}] — cached {}", i, ttl)
            } else {
                format!("system[{}]", i)
            };
            let badge = cache.map(|_| "cached");
            render_block(ui, &label, text, COLOR_SYSTEM, badge, buffers, modified);
        }
        ui.add_space(8.0);
    }

    if let Some(msgs) = val.get("messages").and_then(|v| v.as_array()) {
        section_header(ui, &format!("Messages ({} turns)", msgs.len()));
        for (mi, msg) in msgs.iter().enumerate() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
            let role_color = match role {
                "user" => COLOR_USER,
                "assistant" => COLOR_ASSISTANT,
                "system" => COLOR_SYSTEM,
                _ => COLOR_DEFAULT,
            };

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.colored_label(role_color, format!("{} — message {}", role.to_uppercase(), mi));
            });

            let contents = match msg.get("content") {
                Some(Value::Array(arr)) => arr.clone(),
                Some(Value::String(s)) => vec![serde_json::json!({"type": "text", "text": s})],
                _ => vec![],
            };

            for (pi, part) in contents.iter().enumerate() {
                let ctype = part.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                let (text, color, badge) = match ctype {
                    "thinking" => {
                        let t = part.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                        (t.to_string(), COLOR_THINKING, Some("thinking"))
                    }
                    "tool_use" => {
                        let name = part.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                        let input = part.get("input")
                            .map(|i| serde_json::to_string_pretty(i).unwrap_or_default())
                            .unwrap_or_default();
                        let id = part.get("id").and_then(|i| i.as_str()).unwrap_or("");
                        (format!("Tool: {}\nID: {}\n{}", name, id, input), COLOR_TOOL, Some("tool_use"))
                    }
                    "tool_result" => {
                        let c = part.get("content");
                        let text = match c {
                            Some(Value::String(s)) => s.clone(),
                            Some(Value::Array(arr)) => arr.iter()
                                .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Some(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
                            None => String::new(),
                        };
                        let is_err = part.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                        let tool_id = part.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("");
                        let prefix = if is_err { "ERROR — " } else { "" };
                        (format!("{}ID: {}\n{}", prefix, tool_id, text), COLOR_TOOL, Some("tool_result"))
                    }
                    "text" => {
                        let t = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if let Some(sig) = part.get("signature").and_then(|s| s.as_str()) {
                            (format!("[signature] {}\n\n{}", sig, t), role_color, Some("signed"))
                        } else {
                            (t.to_string(), role_color, None)
                        }
                    }
                    _ => {
                        (serde_json::to_string_pretty(part).unwrap_or_default(), role_color, None)
                    }
                };

                let label = format!("msg[{}].content[{}] ({})", mi, pi, ctype);
                render_block(ui, &label, &text, color, badge, buffers, modified);
            }
        }
        ui.add_space(8.0);
    }

    if let Some(tools) = val.get("tools").and_then(|v| v.as_array()) {
        section_header(ui, &format!("Tools ({})", tools.len()));
        for (i, tool) in tools.iter().enumerate() {
            let name = tool.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let text = serde_json::to_string_pretty(tool).unwrap_or_default();
            let label = format!("tools[{}] — {}", i, name);
            render_block(ui, &label, &text, COLOR_TOOL, None, buffers, modified);
        }
    }
}

fn render_generic(
    ui: &mut egui::Ui,
    val: &Value,
    prefix: &str,
    buffers: &mut HashMap<String, String>,
    modified: &mut bool,
) {
    match val {
        Value::Object(map) => {
            for (key, v) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", prefix, key)
                };
                if is_leaf(v) {
                    render_block(ui, &path, &value_text(v), COLOR_DEFAULT, None, buffers, modified);
                } else {
                    section_header(ui, &path);
                    render_generic(ui, v, &path, buffers, modified);
                }
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let path = format!("{}[{}]", prefix, i);
                if is_leaf(v) {
                    render_block(ui, &path, &value_text(v), COLOR_DEFAULT, None, buffers, modified);
                } else {
                    section_header(ui, &path);
                    render_generic(ui, v, &path, buffers, modified);
                }
            }
        }
        _ => {
            render_block(ui, prefix, &value_text(val), COLOR_DEFAULT, None, buffers, modified);
        }
    }
}

fn section_header(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.colored_label(SECTION_COLOR, "——");
        ui.colored_label(SECTION_COLOR, text);
        ui.colored_label(SECTION_COLOR, "——");
    });
    ui.add_space(2.0);
}

fn render_block(
    ui: &mut egui::Ui,
    label: &str,
    content: &str,
    accent: egui::Color32,
    badge: Option<&str>,
    buffers: &mut HashMap<String, String>,
    modified: &mut bool,
) {
    let buf = buffers.entry(label.to_string()).or_insert_with(|| content.to_string());
    let content_len = buf.len();

    egui::Frame::new()
        .fill(BLOCK_BG)
        .corner_radius(4.0)
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(210, 210, 210)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(3.0, 0.0),
                    egui::Sense::hover(),
                );
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(rect.min, egui::vec2(3.0, ui.available_height().max(20.0))),
                    0.0,
                    accent,
                );

                ui.vertical(|ui| {
                    egui::Frame::new()
                        .fill(HEADER_BG)
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.strong(label);
                                if let Some(b) = badge {
                                    let badge_color = match b {
                                        "cached" => COLOR_USER,
                                        "thinking" => COLOR_THINKING,
                                        "tool_use" | "tool_result" => COLOR_TOOL,
                                        "signed" | "crypto" => COLOR_SYSTEM,
                                        _ => COLOR_DEFAULT,
                                    };
                                    egui::Frame::new()
                                        .fill(badge_color)
                                        .corner_radius(3.0)
                                        .inner_margin(egui::Margin::symmetric(4, 1))
                                        .show(ui, |ui| {
                                            ui.label(egui::RichText::new(b).small().color(egui::Color32::WHITE));
                                        });
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(140, 140, 140),
                                        format!("{} chars", char_size(content_len)),
                                    );
                                });
                            });
                        });

                    let response = ui.add(
                        egui::TextEdit::multiline(buf)
                            .code_editor()
                            .desired_width(f32::INFINITY),
                    );
                    if response.changed() {
                        *modified = true;
                    }
                });
            });
        });
}

fn format_bytes(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{} B", n)
    }
}
