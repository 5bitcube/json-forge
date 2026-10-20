//! Center pane: the document rendered as editable, collapsible blocks.
//!
//! Every block is bound to a JSON pointer and an `EditKind`. Edits are not
//! applied here; they are collected into `BlockCtx::edits` and applied by the
//! app after the UI pass, which keeps the parsed value and raw text in sync.

use eframe::egui;
use egui::collapsing_header::CollapsingState;
use egui::{Align, Color32, CornerRadius, Layout, Rect, RichText, Sense, Stroke, UiBuilder};
use serde_json::Value;
use std::collections::HashMap;

use crate::autoscroll;
use crate::find::{self, BlockHit, FindState, Range as FindRange};
use crate::text_case::{self, Case};
use crate::model::{
    char_size, format_bytes, is_leaf, pointer_related, preview, value_text, BlockBuffer, DocStats, EditKind,
    PendingEdit,
};

/// Blocks larger than this start collapsed.
const AUTO_COLLAPSE_CHARS: usize = 2000;
const HEADER_H: f32 = 24.0;
const PREVIEW_CHARS: usize = 90;

#[derive(Clone, Copy)]
pub struct Palette {
    pub system: Color32,
    pub user: Color32,
    pub assistant: Color32,
    pub tool: Color32,
    pub thinking: Color32,
    pub neutral: Color32,
    pub header_bg: Color32,
    pub header_hover: Color32,
    pub block_bg: Color32,
    pub block_stroke: Color32,
    pub summary_bg: Color32,
    pub summary_stroke: Color32,
    pub meta: Color32,
    pub section: Color32,
    pub error: Color32,
    pub ok: Color32,
    pub banner_bg: Color32,
    pub banner_fg: Color32,
}

impl Palette {
    pub fn for_dark(dark: bool) -> Self {
        let c = Color32::from_rgb;
        if dark {
            Palette {
                system: c(235, 100, 90),
                user: c(90, 200, 140),
                assistant: c(160, 145, 255),
                tool: c(245, 160, 80),
                thinking: c(150, 150, 150),
                neutral: c(190, 190, 190),
                header_bg: c(45, 45, 48),
                header_hover: c(58, 58, 62),
                block_bg: c(32, 32, 34),
                block_stroke: c(70, 70, 74),
                summary_bg: c(40, 40, 43),
                summary_stroke: c(80, 80, 84),
                meta: c(150, 150, 150),
                section: c(235, 100, 90),
                error: c(255, 110, 110),
                ok: c(110, 220, 140),
                banner_bg: c(90, 70, 20),
                banner_fg: c(255, 220, 120),
            }
        } else {
            Palette {
                system: c(192, 57, 43),
                user: c(39, 174, 101),
                assistant: c(123, 104, 238),
                tool: c(230, 126, 34),
                thinking: c(150, 150, 150),
                neutral: c(90, 90, 90),
                header_bg: c(235, 235, 235),
                header_hover: c(224, 224, 224),
                block_bg: c(250, 250, 250),
                block_stroke: c(210, 210, 210),
                summary_bg: c(245, 245, 245),
                summary_stroke: c(200, 200, 200),
                meta: c(130, 130, 130),
                section: c(192, 57, 43),
                error: c(200, 50, 50),
                ok: c(30, 150, 70),
                banner_bg: c(255, 240, 200),
                banner_fg: c(120, 80, 0),
            }
        }
    }
}

/// Everything the block view needs from the app for one frame.
pub struct BlockCtx<'a> {
    /// Document id: scopes the editor widget ids (see `edit_id`).
    pub doc: u64,
    pub buffers: &'a mut HashMap<String, BlockBuffer>,
    pub edits: &'a mut Vec<PendingEdit>,
    /// Find / replace: match ranges per block, current match, filter flag.
    pub find: &'a mut FindState,
    /// JSON pointer to scroll to; cleared once a block claims it.
    pub scroll_to: &'a mut Option<String>,
    /// Force every block open (Some(true)) or closed (Some(false)) this frame.
    pub force_open: Option<bool>,
    pub stats: &'a DocStats,
    /// Parse error of the raw text while the blocks still show the last valid parse.
    pub stale_error: Option<&'a str>,
    pub pal: Palette,
    /// Output: number of blocks rendered (also the render-order index).
    pub total: usize,
    /// Output: case change picked from an editor's context menu.
    pub transform: &'a mut Option<(egui::Id, Case)>,
}

struct Block<'v> {
    pointer: String,
    label: String,
    meta: Vec<String>,
    badge: Option<String>,
    accent: Color32,
    kind: EditKind,
    value: &'v Value,
}

impl<'v> Block<'v> {
    fn new(pointer: String, label: String, kind: EditKind, value: &'v Value, accent: Color32) -> Self {
        Block { pointer, label, meta: Vec::new(), badge: None, accent, kind, value }
    }
    fn meta(mut self, m: impl Into<String>) -> Self {
        self.meta.push(m.into());
        self
    }
    fn badge(mut self, b: impl Into<String>) -> Self {
        self.badge = Some(b.into());
        self
    }
}

#[derive(PartialEq)]
enum Shape {
    Request,
    Response,
    Generic,
}

fn shape(val: &Value) -> Shape {
    if val.get("messages").is_some_and(|m| m.is_array()) {
        return Shape::Request;
    }
    let has_content = val.get("content").is_some_and(|c| c.is_array());
    let looks_like_response = val.get("stop_reason").is_some()
        || val.get("usage").is_some()
        || val.get("type").and_then(|t| t.as_str()) == Some("message");
    if has_content && looks_like_response {
        return Shape::Response;
    }
    Shape::Generic
}

fn escape_key(k: &str) -> String {
    k.replace('~', "~0").replace('/', "~1")
}

/// `/messages/0/content/1/text` -> `messages[0].content[1].text`
/// Widget id of the editor for block `pointer` of document `doc`.
pub fn edit_id(doc: u64, pointer: &str) -> egui::Id {
    egui::Id::new(("edit", doc, pointer))
}

pub fn display_path(ptr: &str) -> String {
    if ptr.is_empty() {
        return "(root)".to_string();
    }
    let mut out = String::new();
    for seg in ptr.split('/').skip(1) {
        let seg = seg.replace("~1", "/").replace("~0", "~");
        if !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()) {
            out.push('[');
            out.push_str(&seg);
            out.push(']');
        } else {
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(&seg);
        }
    }
    out
}

pub fn show(ui: &mut egui::Ui, parsed: &Option<Value>, cx: &mut BlockCtx) {
    let Some(val) = parsed else {
        ui.centered_and_justified(|ui| {
            ui.label("No valid JSON to display. Switch to Raw to fix the text.");
        });
        return;
    };

    let out = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;

            if let Some(err) = cx.stale_error {
                let pal = cx.pal;
                egui::Frame::new()
                    .fill(pal.banner_bg)
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.colored_label(
                            pal.banner_fg,
                            format!(
                                "Raw text has a JSON error ({}). Blocks show the last valid parse; editing a block here will replace the raw text.",
                                err
                            ),
                        );
                    });
            }

            render_summary(ui, cx, val);
            ui.add_space(8.0);

            match shape(val) {
                Shape::Request => render_request(ui, val, cx),
                Shape::Response => render_response(ui, val, cx),
                Shape::Generic => render_generic(ui, val, "", cx),
            }
            ui.add_space(24.0);
        });
    autoscroll::record(ui, autoscroll::MAIN, &out);
}

fn render_summary(ui: &mut egui::Ui, cx: &BlockCtx, val: &Value) {
    let pal = cx.pal;
    let st = cx.stats;

    egui::Frame::new()
        .fill(pal.summary_bg)
        .corner_radius(6.0)
        .inner_margin(12.0)
        .stroke(Stroke::new(1.0, pal.summary_stroke))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new("summary_grid")
                .num_columns(2)
                .spacing([20.0, 4.0])
                .show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, label: &str, value: String| {
                        ui.colored_label(pal.meta, label);
                        ui.label(value);
                        ui.end_row();
                    };

                    if let Some(model) = &st.model {
                        row(ui, "Model", model.clone());
                    }
                    row(
                        ui,
                        "Total size",
                        format!(
                            "{}  (~{} tokens, rough estimate)",
                            format_bytes(st.total_bytes),
                            char_size(st.token_estimate)
                        ),
                    );
                    if let Some((chars, blocks)) = st.system {
                        row(ui, "System prompt", format!("{} chars in {} blocks", char_size(chars), blocks));
                    }
                    if let Some((count, chars)) = st.tools {
                        row(ui, "Tools", format!("{} tools ({} chars)", count, char_size(chars)));
                    }
                    if let Some((count, chars)) = st.messages {
                        row(ui, "Messages", format!("{} messages ({} chars)", count, char_size(chars)));
                    }
                    if let Some(stream) = &st.stream {
                        row(ui, "Stream", stream.clone());
                    }
                    if let Some(stop) = &st.stop_reason {
                        row(ui, "Stop reason", stop.clone());
                    }
                    if let Some(usage) = &st.usage {
                        row(ui, "Usage", usage.clone());
                    }
                    if shape(val) == Shape::Generic {
                        row(ui, "Keys", st.key_count.to_string());
                    }
                });
        });
}

fn render_request(ui: &mut egui::Ui, val: &Value, cx: &mut BlockCtx) {
    let Some(obj) = val.as_object() else { return };
    let pal = cx.pal;

    let skip = ["system", "messages", "tools"];
    let others: Vec<(&String, &Value)> =
        obj.iter().filter(|(k, _)| !skip.contains(&k.as_str())).collect();

    if !others.is_empty() {
        section_header(ui, cx, &format!("Config ({} fields)", others.len()));
        for (key, v) in &others {
            render_config_field(ui, cx, key, v);
        }
        ui.add_space(8.0);
    }

    match val.get("system") {
        Some(sys @ Value::String(_)) => {
            section_header(ui, cx, "System");
            render_block(ui, cx, Block::new("/system".into(), "system".into(), EditKind::Text, sys, pal.system));
            ui.add_space(8.0);
        }
        Some(Value::Array(arr)) => {
            section_header(ui, cx, &format!("System ({} blocks)", arr.len()));
            for (i, b) in arr.iter().enumerate() {
                let base = format!("/system/{}", i);
                let cache = b.get("cache_control");
                let ttl = cache.and_then(|c| c.get("ttl")).and_then(|t| t.as_str());
                match b.get("text") {
                    Some(text @ Value::String(_)) => {
                        let mut blk = Block::new(
                            format!("{}/text", base),
                            format!("system[{}]", i),
                            EditKind::Text,
                            text,
                            pal.system,
                        );
                        if cache.is_some() {
                            blk = blk.badge("cached");
                            if let Some(ttl) = ttl {
                                blk = blk.meta(format!("ttl {}", ttl));
                            }
                        }
                        render_block(ui, cx, blk);
                    }
                    _ => {
                        let ctype = b.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                        render_block(
                            ui,
                            cx,
                            Block::new(base, format!("system[{}] ({})", i, ctype), EditKind::Json, b, pal.system),
                        );
                    }
                }
            }
            ui.add_space(8.0);
        }
        _ => {}
    }

    if let Some(msgs) = val.get("messages").and_then(|v| v.as_array()) {
        section_header(ui, cx, &format!("Messages ({} turns)", msgs.len()));
        for (mi, msg) in msgs.iter().enumerate() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
            let role_color = role_color(&pal, role);
            message_header(ui, role_color, &format!("{} — message {}", role.to_uppercase(), mi));
            render_message_content(ui, cx, &format!("/messages/{}", mi), msg.get("content"), role_color);
        }
        ui.add_space(8.0);
    }

    if let Some(tools) = val.get("tools").and_then(|v| v.as_array()) {
        section_header(ui, cx, &format!("Tools ({})", tools.len()));
        for (i, tool) in tools.iter().enumerate() {
            let name = tool.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            render_block(
                ui,
                cx,
                Block::new(format!("/tools/{}", i), format!("tools[{}] · {}", i, name), EditKind::Json, tool, pal.tool),
            );
        }
    }
}

fn render_response(ui: &mut egui::Ui, val: &Value, cx: &mut BlockCtx) {
    let Some(obj) = val.as_object() else { return };
    let pal = cx.pal;

    let others: Vec<(&String, &Value)> = obj.iter().filter(|(k, _)| k.as_str() != "content").collect();
    if !others.is_empty() {
        section_header(ui, cx, &format!("Response ({} fields)", others.len()));
        for (key, v) in &others {
            render_config_field(ui, cx, key, v);
        }
        ui.add_space(8.0);
    }

    let role = val.get("role").and_then(|r| r.as_str()).unwrap_or("assistant");
    let color = role_color(&pal, role);
    section_header(ui, cx, "Content");
    message_header(ui, color, &format!("{} — response", role.to_uppercase()));
    render_message_content(ui, cx, "", val.get("content"), color);
}

fn render_config_field(ui: &mut egui::Ui, cx: &mut BlockCtx, key: &str, v: &Value) {
    let pal = cx.pal;
    let kind = if is_leaf(v) { EditKind::natural(v) } else { EditKind::Json };
    render_block(ui, cx, Block::new(format!("/{}", escape_key(key)), key.to_string(), kind, v, pal.neutral));
}

fn render_message_content(
    ui: &mut egui::Ui,
    cx: &mut BlockCtx,
    base: &str,
    content: Option<&Value>,
    role_color: Color32,
) {
    let ptr = format!("{}/content", base);
    match content {
        Some(s @ Value::String(_)) => {
            render_block(ui, cx, Block::new(ptr.clone(), format!("{} (text)", display_path(&ptr)), EditKind::Text, s, role_color));
        }
        Some(Value::Array(parts)) => {
            for (pi, part) in parts.iter().enumerate() {
                render_part(ui, cx, &format!("{}/{}", ptr, pi), part, role_color);
            }
        }
        Some(other) => {
            render_block(ui, cx, Block::new(ptr.clone(), display_path(&ptr), EditKind::Json, other, role_color));
        }
        None => {}
    }
}

fn render_part(ui: &mut egui::Ui, cx: &mut BlockCtx, base: &str, part: &Value, role_color: Color32) {
    let pal = cx.pal;
    let ctype = part.get("type").and_then(|t| t.as_str()).unwrap_or("?");
    let path = display_path(base);
    let cached = part.get("cache_control").is_some();
    let sig_meta = part
        .get("signature")
        .and_then(|s| s.as_str())
        .map(|s| format!("sig {}…", s.chars().take(10).collect::<String>()));

    fn with_cached<'v>(mut b: Block<'v>, cached: bool) -> Block<'v> {
        if cached {
            b = b.meta("cached");
        }
        b
    }

    match ctype {
        "text" => {
            let mut b = match part.get("text") {
                Some(t @ Value::String(_)) => {
                    Block::new(format!("{}/text", base), format!("{} (text)", path), EditKind::Text, t, role_color)
                }
                _ => Block::new(base.to_string(), format!("{} (text)", path), EditKind::Json, part, role_color),
            };
            if let Some(m) = sig_meta {
                b = b.badge("signed").meta(m);
            }
            render_block(ui, cx, with_cached(b, cached));
        }
        "thinking" => {
            let mut b = match part.get("thinking") {
                Some(t @ Value::String(_)) => Block::new(
                    format!("{}/thinking", base),
                    format!("{} (thinking)", path),
                    EditKind::Text,
                    t,
                    pal.thinking,
                ),
                _ => Block::new(base.to_string(), format!("{} (thinking)", path), EditKind::Json, part, pal.thinking),
            }
            .badge("thinking");
            if let Some(m) = sig_meta {
                b = b.meta(m);
            }
            render_block(ui, cx, with_cached(b, cached));
        }
        "redacted_thinking" => {
            let b = Block::new(base.to_string(), format!("{} (redacted_thinking)", path), EditKind::Json, part, pal.thinking)
                .badge("redacted");
            render_block(ui, cx, with_cached(b, cached));
        }
        "tool_use" | "server_tool_use" => {
            let name = part.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let id = part.get("id").and_then(|i| i.as_str()).unwrap_or("");
            let mut b = match part.get("input") {
                Some(input) => Block::new(
                    format!("{}/input", base),
                    format!("{} · {}", path, name),
                    EditKind::Json,
                    input,
                    pal.tool,
                ),
                None => Block::new(base.to_string(), format!("{} · {}", path, name), EditKind::Json, part, pal.tool),
            }
            .badge(ctype);
            if !id.is_empty() {
                b = b.meta(format!("id {}", id));
            }
            render_block(ui, cx, with_cached(b, cached));
        }
        "tool_result" => {
            let tool_id = part.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("");
            let is_err = part.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
            fn decorate_result<'v>(mut b: Block<'v>, is_err: bool, tool_id: &str, cached: bool) -> Block<'v> {
                b = b.badge(if is_err { "error" } else { "tool_result" });
                if !tool_id.is_empty() {
                    b = b.meta(format!("for {}", tool_id));
                }
                with_cached(b, cached)
            }
            match part.get("content") {
                Some(s @ Value::String(_)) => {
                    let b = Block::new(format!("{}/content", base), format!("{} (tool_result)", path), EditKind::Text, s, pal.tool);
                    render_block(ui, cx, decorate_result(b, is_err, tool_id, cached));
                }
                Some(Value::Array(sub)) => {
                    for (k, s) in sub.iter().enumerate() {
                        let sub_ptr = format!("{}/content/{}", base, k);
                        let b = match s.get("text") {
                            Some(t @ Value::String(_)) => Block::new(
                                format!("{}/text", sub_ptr),
                                format!("{} (tool_result)", display_path(&sub_ptr)),
                                EditKind::Text,
                                t,
                                pal.tool,
                            ),
                            _ => {
                                let st = s.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                                Block::new(
                                    sub_ptr.clone(),
                                    format!("{} ({})", display_path(&sub_ptr), st),
                                    EditKind::Json,
                                    s,
                                    pal.tool,
                                )
                            }
                        };
                        render_block(ui, cx, decorate_result(b, is_err, tool_id, cached));
                    }
                }
                Some(other) => {
                    let b = Block::new(format!("{}/content", base), format!("{} (tool_result)", path), EditKind::Json, other, pal.tool);
                    render_block(ui, cx, decorate_result(b, is_err, tool_id, cached));
                }
                None => {
                    let b = Block::new(base.to_string(), format!("{} (tool_result)", path), EditKind::Json, part, pal.tool);
                    render_block(ui, cx, decorate_result(b, is_err, tool_id, cached));
                }
            }
        }
        _ => {
            let b = Block::new(base.to_string(), format!("{} ({})", path, ctype), EditKind::Json, part, role_color)
                .badge(ctype);
            render_block(ui, cx, with_cached(b, cached));
        }
    }
}

fn render_generic(ui: &mut egui::Ui, val: &Value, prefix: &str, cx: &mut BlockCtx) {
    let pal = cx.pal;
    match val {
        Value::Object(map) => {
            for (key, v) in map {
                let ptr = format!("{}/{}", prefix, escape_key(key));
                render_generic_child(ui, v, &ptr, cx);
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let ptr = format!("{}/{}", prefix, i);
                render_generic_child(ui, v, &ptr, cx);
            }
        }
        _ => {
            render_block(
                ui,
                cx,
                Block::new(prefix.to_string(), display_path(prefix), EditKind::natural(val), val, pal.neutral),
            );
        }
    }
}

fn render_generic_child(ui: &mut egui::Ui, v: &Value, ptr: &str, cx: &mut BlockCtx) {
    let pal = cx.pal;
    let empty_container = matches!(v, Value::Object(m) if m.is_empty()) || matches!(v, Value::Array(a) if a.is_empty());
    if is_leaf(v) {
        render_block(ui, cx, Block::new(ptr.to_string(), display_path(ptr), EditKind::natural(v), v, pal.neutral));
    } else if empty_container {
        render_block(ui, cx, Block::new(ptr.to_string(), display_path(ptr), EditKind::Json, v, pal.neutral));
    } else {
        section_header(ui, cx, &display_path(ptr));
        render_generic(ui, v, ptr, cx);
    }
}

fn role_color(pal: &Palette, role: &str) -> Color32 {
    match role {
        "user" => pal.user,
        "assistant" => pal.assistant,
        "system" => pal.system,
        _ => pal.neutral,
    }
}

fn section_header(ui: &mut egui::Ui, cx: &BlockCtx, text: &str) {
    let col = cx.pal.section;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.colored_label(col, "——");
        ui.colored_label(col, RichText::new(text).strong());
        ui.colored_label(col, "——");
    });
    ui.add_space(2.0);
}

fn message_header(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.add_space(4.0);
    ui.colored_label(color, RichText::new(text).strong());
}

fn badge_color(pal: &Palette, badge: &str) -> Color32 {
    match badge {
        "cached" => pal.user,
        "thinking" | "redacted" => pal.thinking,
        "tool_use" | "server_tool_use" | "tool_result" => pal.tool,
        "signed" => pal.system,
        "error" => pal.error,
        _ => pal.neutral,
    }
}

fn seed_text(kind: EditKind, value: &Value) -> String {
    match kind {
        EditKind::Text => match value {
            Value::String(s) => s.clone(),
            other => value_text(other),
        },
        EditKind::Scalar => serde_json::to_string(value).unwrap_or_default(),
        EditKind::Json => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}

fn render_block(ui: &mut egui::Ui, cx: &mut BlockCtx, b: Block<'_>) {
    let BlockCtx { doc, buffers, edits, find, scroll_to, force_open, pal, total, transform, .. } = cx;
    let doc = *doc;
    let pal = *pal;
    let index = *total;
    *total += 1;

    let entry = buffers.entry(b.pointer.clone()).or_insert_with(|| BlockBuffer {
        text: seed_text(b.kind, b.value),
        error: None,
        kind: b.kind,
        rev: 0,
    });

    let is_target = scroll_to.as_deref().is_some_and(|t| pointer_related(&b.pointer, t));

    // Find / replace: matches in this block's text.
    let ranges: Vec<FindRange> = if find.active() {
        find.ranges_for(&b.pointer, entry.rev, &entry.text).to_vec()
    } else {
        Vec::new()
    };
    let is_find_target = find.current.as_ref().is_some_and(|t| t.pointer == b.pointer);
    let current_range = if is_find_target { find.current.as_ref().map(|t| t.range) } else { None };
    if find.active() {
        if find.filter && ranges.is_empty() && !is_target {
            return;
        }
        if !ranges.is_empty() {
            find.hits.push(BlockHit { index, pointer: b.pointer.clone(), ranges: ranges.clone() });
        }
    }
    let highlight = find.open && !ranges.is_empty();

    let id = ui.make_persistent_id(("block", &b.pointer));
    let default_open = entry.text.len() <= AUTO_COLLAPSE_CHARS;
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
    if let Some(open) = *force_open {
        state.set_open(open);
    }
    if is_target || (is_find_target && find.scroll_pending) {
        state.set_open(true);
    }

    let stroke_color = if is_target { b.accent } else { pal.block_stroke };
    let frame = egui::Frame::new()
        .fill(pal.block_bg)
        .corner_radius(4.0)
        .stroke(Stroke::new(1.0, stroke_color))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;

            // Header: register the click target first so the Copy button,
            // registered later, sits on top of it.
            let avail = ui.available_rect_before_wrap();
            let header_rect = Rect::from_min_size(avail.min, egui::vec2(avail.width(), HEADER_H));
            let header_resp = ui.interact(header_rect, id.with("hdr"), Sense::click());
            if header_resp.clicked() {
                state.toggle(ui);
            }
            let fill = if header_resp.hovered() { pal.header_hover } else { pal.header_bg };
            let is_open = state.is_open();
            let radius = if is_open {
                CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }
            } else {
                CornerRadius::same(4)
            };
            ui.painter().rect_filled(header_rect, radius, fill);
            ui.painter().rect_filled(
                Rect::from_min_size(header_rect.min, egui::vec2(3.0, HEADER_H)),
                CornerRadius { nw: 4, ne: 0, sw: if is_open { 0 } else { 4 }, se: 0 },
                b.accent,
            );

            let text_len = entry.text.len();
            let preview_line = if is_open { None } else { Some(preview(&entry.text, PREVIEW_CHARS)) };

            ui.scope_builder(
                UiBuilder::new().max_rect(header_rect).layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.add_space(8.0);
                    ui.label(RichText::new(if is_open { "⏷" } else { "⏵" }).small().color(pal.meta));
                    ui.strong(&b.label);
                    if let Some(badge) = &b.badge {
                        egui::Frame::new()
                            .fill(badge_color(&pal, badge))
                            .corner_radius(3.0)
                            .inner_margin(egui::Margin::symmetric(4, 1))
                            .show(ui, |ui| {
                                ui.label(RichText::new(badge).small().color(Color32::WHITE));
                            });
                    }
                    for m in &b.meta {
                        ui.label(RichText::new(m).small().color(pal.meta));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(6.0);
                        if ui.small_button("Copy").on_hover_text("Copy block content").clicked() {
                            ui.ctx().copy_text(entry.text.clone());
                        }
                        ui.label(RichText::new(format!("{} chars", char_size(text_len))).small().color(pal.meta));
                        if let Some(p) = preview_line {
                            ui.add(
                                egui::Label::new(RichText::new(p).small().color(pal.meta).italics())
                                    .truncate(),
                            );
                        }
                    });
                },
            );
            ui.allocate_rect(header_rect, Sense::hover());

            state.show_body_unindented(ui, |ui| {
                egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if let Some(err) = &entry.error {
                        ui.colored_label(pal.error, err);
                        ui.add_space(2.0);
                    }
                    let edit_id = edit_id(doc, &b.pointer);
                    if is_find_target && find.select_pending {
                        if let Some(r) = current_range {
                            find::select_range(ui.ctx(), edit_id, &entry.text, r);
                        }
                        find.select_pending = false;
                    }
                    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                        let job = find::highlight_job(ui, buf.as_str(), wrap_width, &ranges, current_range);
                        ui.fonts_mut(|f| f.layout_job(job))
                    };
                    let mut edit = match b.kind {
                        EditKind::Scalar => egui::TextEdit::singleline(&mut entry.text),
                        _ => egui::TextEdit::multiline(&mut entry.text).desired_rows(1),
                    }
                    .id(edit_id)
                    .code_editor()
                    .desired_width(f32::INFINITY);
                    if highlight {
                        edit = edit.layouter(&mut layouter);
                    }
                    let out = edit.show(ui);
                    out.response.response.context_menu(|ui| {
                        if let Some(case) = text_case::menu(ui) {
                            **transform = Some((edit_id, case));
                        }
                    });
                    if is_find_target && find.scroll_pending {
                        if let Some(r) = current_range {
                            find::scroll_to_range(ui, &out, &entry.text, r);
                        }
                        find.scroll_pending = false;
                    }
                    if out.response.response.changed() {
                        entry.rev += 1;
                        edits.push(PendingEdit { pointer: b.pointer.clone(), kind: b.kind, text: entry.text.clone() });
                    }
                });
            });
        });

    state.store(ui.ctx());

    if is_target {
        frame.response.scroll_to_me(Some(Align::TOP));
        **scroll_to = None;
    }
}
