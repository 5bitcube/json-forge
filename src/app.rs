//! Application state and frame logic.
//!
//! Two representations of the document exist: `json_text` (raw) and `parsed`.
//! Exactly one of them is authoritative at any moment:
//! - Raw mode edits set `raw_dirty_at`; after a debounce the text is reparsed.
//! - Block edits mutate `parsed` and set `text_dirty_at`; after a debounce the
//!   text is regenerated. `flush()` forces both to agree before save/format.

use eframe::egui;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::block_view::{self, BlockCtx, Palette};
use crate::editor_view::{self, RawFind, EDITOR_ID};
use crate::file_ops::{self, Confirm};
use crate::find::{self, BlockHit, FindAction, FindState, Seek};
use crate::model::{apply_edit, format_bytes, pointer_parent, BlockBuffer, DocStats, PendingEdit};
use crate::recent::RecentFiles;
use crate::tree_view;

const RAW_DEBOUNCE: Duration = Duration::from_millis(250);
const SYNC_DEBOUNCE: Duration = Duration::from_millis(300);
const STATUS_TTL: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Blocks,
    Raw,
}

struct StatusMsg {
    text: String,
    is_error: bool,
    at: Instant,
}

pub struct ForgeApp {
    json_text: String,
    parsed: Option<Value>,
    parse_error: Option<String>,
    file_path: Option<PathBuf>,
    modified: bool,
    view_mode: ViewMode,
    /// Serialize without whitespace (file was minified when loaded).
    compact: bool,
    /// Document is JSON Lines: `parsed` is an array of one value per line.
    jsonl: bool,
    block_buffers: HashMap<String, BlockBuffer>,
    stats: DocStats,
    raw_dirty_at: Option<Instant>,
    text_dirty_at: Option<Instant>,
    status: Option<StatusMsg>,
    scroll_to: Option<String>,
    force_open: Option<bool>,
    find: FindState,
    recent: RecentFiles,
    last_title: String,
    allow_close: bool,
    close_now: bool,
    autoscroll_origin: Option<egui::Pos2>,
}

const SAMPLE: &str = r#"{
  "example": "Open a JSON file, drop one here, or press Paste / Ctrl+V to load the clipboard",
  "array": [1, 2, 3],
  "nested": {
    "key": "value",
    "flag": true
  }
}"#;

fn serialize(val: &Value, compact: bool, jsonl: bool) -> String {
    if jsonl {
        if let Value::Array(items) = val {
            let mut out = String::new();
            for item in items {
                out.push_str(&serde_json::to_string(item).unwrap_or_default());
                out.push('\n');
            }
            return out;
        }
    }
    if compact {
        serde_json::to_string(val).unwrap_or_default()
    } else {
        serde_json::to_string_pretty(val).unwrap_or_default()
    }
}

/// Short folder hint for a recent-file entry: the last directory component.
fn parent_hint(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn format_error(e: &serde_json::Error) -> String {
    format!("Line {}, col {}: {}", e.line(), e.column(), e)
}

impl ForgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        // Maximize after creation: setting it in the ViewportBuilder together
        // with an inner size leaves the window flagged maximized at the small size.
        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));

        let mut app = Self {
            json_text: String::new(),
            parsed: None,
            parse_error: None,
            file_path: None,
            modified: false,
            view_mode: ViewMode::Blocks,
            compact: false,
            jsonl: false,
            block_buffers: HashMap::new(),
            stats: DocStats::default(),
            raw_dirty_at: None,
            text_dirty_at: None,
            status: None,
            scroll_to: None,
            force_open: None,
            find: FindState::default(),
            recent: RecentFiles::load(),
            last_title: String::new(),
            allow_close: false,
            close_now: false,
            autoscroll_origin: None,
        };

        match initial {
            Some(path) => app.open_path(path),
            None => app.load_text(SAMPLE.to_string(), None),
        }
        app
    }

    // ----- status -----------------------------------------------------------

    fn set_status(&mut self, text: impl Into<String>, is_error: bool) {
        self.status = Some(StatusMsg { text: text.into(), is_error, at: Instant::now() });
    }

    fn doc_name(&self) -> String {
        self.file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".to_string())
    }

    fn window_title(&self) -> String {
        if self.modified {
            format!("JSON Forge - {} *", self.doc_name())
        } else {
            format!("JSON Forge - {}", self.doc_name())
        }
    }

    // ----- parsing / syncing ------------------------------------------------

    /// Parse the raw text. Falls back to JSON Lines when the whole text is not
    /// a single document but every non-empty line is.
    fn parse_text(&self) -> Result<(Value, bool), String> {
        let err = match serde_json::from_str::<Value>(&self.json_text) {
            Ok(v) => return Ok((v, false)),
            Err(e) => e,
        };

        let is_jsonl_ext = self
            .file_path
            .as_ref()
            .and_then(|p| p.extension())
            .is_some_and(|x| x.eq_ignore_ascii_case("jsonl"));
        let lines: Vec<&str> = self.json_text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();

        if is_jsonl_ext || lines.len() > 1 {
            let mut items = Vec::with_capacity(lines.len());
            let mut ok = true;
            for line in &lines {
                match serde_json::from_str::<Value>(line) {
                    Ok(v) => items.push(v),
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && (items.len() > 1 || is_jsonl_ext) {
                return Ok((Value::Array(items), true));
            }
        }

        Err(format_error(&err))
    }

    /// Replace the whole document with `text`.
    fn load_text(&mut self, text: String, path: Option<PathBuf>) {
        self.json_text = text;
        self.file_path = path;
        self.modified = false;
        self.block_buffers.clear();
        self.find.reset_document();
        self.raw_dirty_at = None;
        self.text_dirty_at = None;
        self.scroll_to = None;
        self.compact = !self.json_text.trim().contains('\n');

        match self.parse_text() {
            Ok((v, jsonl)) => {
                self.jsonl = jsonl;
                self.stats = DocStats::compute(&v);
                self.parsed = Some(v);
                self.parse_error = None;
            }
            Err(e) => {
                self.jsonl = false;
                self.parsed = None;
                self.stats = DocStats::default();
                self.set_status(format!("Invalid JSON: {}", e), true);
                self.parse_error = Some(e);
                self.view_mode = ViewMode::Raw;
            }
        }
    }

    fn reparse_from_raw(&mut self) {
        self.raw_dirty_at = None;
        match self.parse_text() {
            Ok((v, jsonl)) => {
                self.jsonl = jsonl;
                self.stats = DocStats::compute(&v);
                self.parsed = Some(v);
                self.parse_error = None;
                self.block_buffers.clear();
                self.find.cache.clear();
            }
            Err(e) => self.parse_error = Some(e),
        }
    }

    fn sync_text_from_parsed(&mut self) {
        self.text_dirty_at = None;
        if let Some(v) = &self.parsed {
            let text = serialize(v, self.compact, self.jsonl);
            let stats = DocStats::compute(v);
            self.json_text = text;
            self.stats = stats;
            self.parse_error = None;
            self.raw_dirty_at = None;
        }
    }

    /// Make raw text and parsed value agree, whichever is newer.
    fn flush(&mut self) {
        if self.raw_dirty_at.is_some() {
            self.reparse_from_raw();
        }
        if self.text_dirty_at.is_some() {
            self.sync_text_from_parsed();
        }
    }

    fn apply_edits(&mut self, edits: Vec<PendingEdit>) {
        let Some(root) = self.parsed.as_mut() else { return };
        for edit in edits {
            match apply_edit(root, &edit) {
                Ok(applied) => {
                    self.modified = true;
                    self.text_dirty_at = Some(Instant::now());
                    if applied.kind_changed {
                        self.block_buffers.remove(&edit.pointer);
                        self.find.cache.remove(&edit.pointer);
                    } else if let Some(buf) = self.block_buffers.get_mut(&edit.pointer) {
                        buf.error = None;
                    }
                }
                Err(msg) => {
                    if let Some(buf) = self.block_buffers.get_mut(&edit.pointer) {
                        buf.error = Some(msg);
                    }
                }
            }
        }
    }

    fn set_mode(&mut self, mode: ViewMode) {
        if mode == self.view_mode {
            return;
        }
        match mode {
            ViewMode::Raw => {
                if self.text_dirty_at.is_some() {
                    self.sync_text_from_parsed();
                }
            }
            ViewMode::Blocks => {
                if self.raw_dirty_at.is_some() {
                    self.reparse_from_raw();
                }
            }
        }
        self.view_mode = mode;
        self.find.reset_document();
    }

    // ----- file operations --------------------------------------------------

    fn write_to(&mut self, path: &Path) -> bool {
        match file_ops::write(path, &self.json_text) {
            Ok(()) => {
                self.modified = false;
                self.recent.push(path);
                self.set_status(format!("Saved {}", path.display()), false);
                true
            }
            Err(e) => {
                self.set_status(e, true);
                false
            }
        }
    }

    fn save(&mut self) -> bool {
        self.flush();
        match self.file_path.clone() {
            Some(path) => self.write_to(&path),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) -> bool {
        self.flush();
        let Some(path) = file_ops::pick_save(self.file_path.as_deref()) else { return false };
        if self.write_to(&path) {
            self.file_path = Some(path);
            true
        } else {
            false
        }
    }

    /// Returns true when it is OK to discard the current document.
    fn confirm_discard(&mut self) -> bool {
        if !self.modified {
            return true;
        }
        match file_ops::confirm_unsaved(&self.doc_name()) {
            Confirm::Save => self.save(),
            Confirm::Discard => true,
            Confirm::Cancel => false,
        }
    }

    fn open_dialog(&mut self) {
        if !self.confirm_discard() {
            return;
        }
        if let Some(path) = file_ops::pick_open() {
            self.open_path(path);
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        match file_ops::read(&path) {
            Ok(text) => {
                let name = path.display().to_string();
                self.recent.push(&path);
                self.load_text(text, Some(path));
                if self.parse_error.is_none() {
                    self.set_status(format!("Opened {}", name), false);
                }
            }
            Err(e) => {
                if !path.exists() {
                    self.recent.remove(&path);
                }
                self.set_status(e, true);
            }
        }
    }

    fn paste_document(&mut self, text: String) {
        if text.trim().is_empty() {
            self.set_status("Clipboard has no text", true);
            return;
        }
        if !self.confirm_discard() {
            return;
        }
        self.load_text(text, None);
        if self.parse_error.is_none() {
            self.view_mode = ViewMode::Blocks;
            self.set_status("Loaded JSON from clipboard", false);
        }
    }

    fn new_document(&mut self) {
        if !self.confirm_discard() {
            return;
        }
        self.load_text("{}".to_string(), None);
        self.compact = false;
        self.view_mode = ViewMode::Blocks;
    }

    fn copy_document(&mut self, ctx: &egui::Context) {
        self.flush();
        ctx.copy_text(self.json_text.clone());
        self.set_status("Document copied to clipboard", false);
    }

    fn reformat(&mut self, compact: bool) {
        self.flush();
        if self.parse_error.is_some() || self.parsed.is_none() {
            self.set_status("Fix the JSON error before formatting", true);
            return;
        }
        if self.jsonl {
            self.set_status("JSON Lines: normalized to one record per line", false);
        } else {
            self.compact = compact;
        }
        let Some(v) = &self.parsed else { return };
        let text = serialize(v, self.compact, self.jsonl);
        if text != self.json_text {
            self.json_text = text;
            self.modified = true;
        }
    }

    // ----- find / replace ---------------------------------------------------

    /// Single-line selection of whichever editor has keyboard focus.
    fn selected_text(&self, ctx: &egui::Context) -> Option<String> {
        let focused = ctx.memory(|m| m.focused())?;
        let text: &str = if focused == egui::Id::new(EDITOR_ID) {
            &self.json_text
        } else {
            let (_, buf) = self
                .block_buffers
                .iter()
                .find(|(p, _)| egui::Id::new(("edit", p.as_str())) == focused)?;
            &buf.text
        };
        let range = egui::TextEdit::load_state(ctx, focused)?.cursor.char_range()?;
        let (a, b) = (range.primary.index.0.min(range.secondary.index.0), range.primary.index.0.max(range.secondary.index.0));
        if a == b {
            return None;
        }
        let sel: String = text.chars().skip(a).take(b - a).collect();
        if sel.contains('\n') || sel.chars().count() > 200 {
            return None;
        }
        Some(sel)
    }

    fn open_find(&mut self, ctx: &egui::Context, replace: bool) {
        if let Some(sel) = self.selected_text(ctx) {
            self.find.query = sel;
        }
        self.find.open = true;
        if replace {
            self.find.focus_replace = true;
        } else {
            self.find.focus_find = true;
        }
    }

    /// Where "Find" starts when there is no current match.
    fn find_start(&self, ctx: &egui::Context) -> Seek {
        if self.view_mode != ViewMode::Raw {
            return Seek::Forward { block: 0, pos: 0 };
        }
        let pos = egui::TextEdit::load_state(ctx, egui::Id::new(EDITOR_ID))
            .and_then(|s| s.cursor.char_range())
            .map(|r| r.primary.index.0.min(r.secondary.index.0))
            .map(|ci| self.json_text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(self.json_text.len()))
            .unwrap_or(0);
        Seek::Forward { block: 0, pos }
    }

    fn run_find_action(&mut self, ctx: &egui::Context, action: FindAction) {
        let raw = self.view_mode == ViewMode::Raw;
        match action {
            FindAction::Close => {
                self.find.open = false;
                self.find.filter = false;
                // Hand focus back to the editor holding the current match.
                let id = match &self.find.current {
                    Some(t) if !raw => Some(egui::Id::new(("edit", t.pointer.as_str()))),
                    _ if raw => Some(egui::Id::new(EDITOR_ID)),
                    _ => None,
                };
                if let Some(id) = id {
                    ctx.memory_mut(|m| m.request_focus(id));
                    self.find.select_pending = self.find.current.is_some();
                }
            }
            FindAction::FindAll => {
                if raw {
                    self.set_status("Find All filters blocks: switch to Blocks mode to use it", true);
                } else {
                    self.find.filter = !self.find.filter;
                }
            }
            FindAction::Next | FindAction::Prev => {
                if self.find.query.is_empty() {
                    self.find.open = true;
                    self.find.focus_find = true;
                    return;
                }
                let start = self.find_start(ctx);
                self.find.step(action == FindAction::Next, start);
            }
            FindAction::Replace => {
                if !self.find.active() {
                    return;
                }
                let Some(t) = self.find.current.clone() else {
                    let start = self.find_start(ctx);
                    self.find.step(true, start);
                    return;
                };
                let Some(m) = &self.find.matcher else { return };
                let repl = self.find.replacement.clone();
                let result = if raw {
                    m.replace_one(&mut self.json_text, t.range, &repl).map(|end| (end, None))
                } else {
                    match self.block_buffers.get_mut(&t.pointer) {
                        Some(buf) => m.replace_one(&mut buf.text, t.range, &repl).map(|end| {
                            buf.rev += 1;
                            (end, Some(PendingEdit { pointer: t.pointer.clone(), kind: buf.kind, text: buf.text.clone() }))
                        }),
                        None => None,
                    }
                };
                self.find.current = None;
                match result {
                    Some((end, edit)) => {
                        if let Some(edit) = edit {
                            self.apply_edits(vec![edit]);
                        } else {
                            self.modified = true;
                            self.raw_dirty_at = Some(Instant::now());
                        }
                        self.find.seek = Some(Seek::Forward { block: t.block, pos: end });
                    }
                    None => self.set_status("The text under the match changed; find it again", true),
                }
            }
            FindAction::ReplaceAll => {
                let Some(m) = &self.find.matcher else { return };
                let repl = self.find.replacement.clone();
                let (count, blocks) = if raw {
                    let (new_text, n) = m.replace_all(&self.json_text, &repl);
                    if n > 0 {
                        self.json_text = new_text;
                        self.modified = true;
                        self.raw_dirty_at = Some(Instant::now());
                    }
                    (n, 0)
                } else {
                    let pointers: Vec<String> = self.find.hits.iter().map(|h| h.pointer.clone()).collect();
                    let mut edits = Vec::new();
                    let mut n = 0;
                    for p in pointers {
                        let Some(buf) = self.block_buffers.get_mut(&p) else { continue };
                        let (new_text, c) = m.replace_all(&buf.text, &repl);
                        if c > 0 {
                            buf.text = new_text;
                            buf.rev += 1;
                            n += c;
                            edits.push(PendingEdit { pointer: p, kind: buf.kind, text: buf.text.clone() });
                        }
                    }
                    let blocks = edits.len();
                    self.apply_edits(edits);
                    (n, blocks)
                };
                self.find.current = None;
                if count == 0 {
                    self.set_status(format!("No matches for \"{}\"", self.find.query), true);
                } else if raw {
                    self.set_status(format!("Replaced {} occurrence(s)", count), false);
                } else {
                    self.set_status(format!("Replaced {} occurrence(s) in {} block(s)", count, blocks), false);
                }
            }
        }
    }

    // ----- frame pieces -----------------------------------------------------

    fn handle_autoscroll(&mut self, ctx: &egui::Context) {
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

        let Some(origin) = self.autoscroll_origin else { return };
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

        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("autoscroll")));
        let r = 14.0;
        let col = egui::Color32::from_rgb(100, 100, 100);
        painter.circle_filled(origin, r, egui::Color32::from_rgba_premultiplied(245, 245, 245, 230));
        painter.circle_stroke(origin, r, egui::Stroke::new(1.0, col));
        painter.circle_filled(origin, 2.0, col);
        let tip_up = origin + egui::vec2(0.0, -8.0);
        painter.add(egui::Shape::convex_polygon(
            vec![tip_up, tip_up + egui::vec2(-4.0, 5.0), tip_up + egui::vec2(4.0, 5.0)],
            col,
            egui::Stroke::NONE,
        ));
        let tip_down = origin + egui::vec2(0.0, 8.0);
        painter.add(egui::Shape::convex_polygon(
            vec![tip_down, tip_down + egui::vec2(-4.0, -5.0), tip_down + egui::vec2(4.0, -5.0)],
            col,
            egui::Stroke::NONE,
        ));
        ctx.request_repaint();
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let cmd_shift = Modifiers::COMMAND.plus(Modifiers::SHIFT);

        if ctx.input_mut(|i| i.consume_key(cmd_shift, Key::S)) {
            self.save_as();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)) {
            self.save();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.open_dialog();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            self.new_document();
        }
        if ctx.input_mut(|i| i.consume_key(cmd_shift, Key::C)) {
            self.copy_document(ctx);
        }
        // Find / replace (Atom keymap).
        let cmd_alt = Modifiers::COMMAND.plus(Modifiers::ALT);
        if ctx.input_mut(|i| i.consume_key(cmd_alt, Key::F)) {
            self.open_find(ctx, true);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            self.open_find(ctx, false);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::E)) {
            if let Some(sel) = self.selected_text(ctx) {
                self.find.query = sel;
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F3)) {
            self.find.pending = Some(FindAction::Next);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::F3)) {
            self.find.pending = Some(FindAction::Prev);
        }
        if self.find.open {
            if ctx.input_mut(|i| i.consume_key(cmd_alt, Key::Slash)) {
                self.find.use_regex = !self.find.use_regex;
            }
            if ctx.input_mut(|i| i.consume_key(cmd_alt, Key::C)) {
                self.find.case_sensitive = !self.find.case_sensitive;
            }
            if ctx.input_mut(|i| i.consume_key(cmd_alt, Key::W)) {
                self.find.whole_word = !self.find.whole_word;
            }
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.find.pending = Some(FindAction::Close);
            }
        }

        // Ctrl+V with no text field focused loads the clipboard as a document.
        let nothing_focused = ctx.memory(|m| m.focused().is_none());
        if nothing_focused {
            let pasted = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Paste(s) => Some(s.clone()),
                    _ => None,
                })
            });
            if let Some(text) = pasted {
                self.paste_document(text);
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Option<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().find_map(|f| {
                let p = f.path();
                if p.as_os_str().is_empty() {
                    None
                } else {
                    Some(p.to_path_buf())
                }
            })
        });
        if let Some(path) = dropped {
            if self.confirm_discard() {
                self.open_path(path);
            }
        }
    }

    fn handle_debounces(&mut self, ctx: &egui::Context) {
        if let Some(at) = self.raw_dirty_at {
            let elapsed = at.elapsed();
            if elapsed >= RAW_DEBOUNCE {
                self.reparse_from_raw();
            } else {
                ctx.request_repaint_after(RAW_DEBOUNCE - elapsed);
            }
        }
        if let Some(at) = self.text_dirty_at {
            let elapsed = at.elapsed();
            if elapsed >= SYNC_DEBOUNCE {
                self.sync_text_from_parsed();
            } else {
                ctx.request_repaint_after(SYNC_DEBOUNCE - elapsed);
            }
        }
        if let Some(st) = &self.status {
            let elapsed = st.at.elapsed();
            if elapsed >= STATUS_TTL {
                self.status = None;
            } else {
                ctx.request_repaint_after(STATUS_TTL - elapsed);
            }
        }
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if self.close_now {
            self.close_now = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.modified && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.confirm_discard() {
                self.allow_close = true;
                self.close_now = true;
                ctx.request_repaint();
            }
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            self.file_menu(ui);
            self.edit_menu(ui);
            self.view_menu(ui);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                let mut mode = self.view_mode;
                ui.selectable_value(&mut mode, ViewMode::Raw, "Raw");
                ui.selectable_value(&mut mode, ViewMode::Blocks, "Blocks");
                if mode != self.view_mode {
                    self.set_mode(mode);
                }
            });
        });
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("File", |ui| {
            ui.set_min_width(220.0);
            if ui.add(egui::Button::new("New").shortcut_text("Ctrl+N")).clicked() {
                self.new_document();
            }
            if ui.add(egui::Button::new("Open...").shortcut_text("Ctrl+O")).clicked() {
                self.open_dialog();
            }
            ui.menu_button("Open Recent", |ui| {
                ui.set_min_width(280.0);
                if self.recent.is_empty() {
                    ui.add_enabled(false, egui::Button::new("(no recent files)"));
                    return;
                }
                let entries: Vec<PathBuf> = self.recent.iter().cloned().collect();
                let mut chosen: Option<PathBuf> = None;
                for (i, path) in entries.iter().enumerate() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    let label = format!("{}  {}", i + 1, name);
                    let resp = ui.add(
                        egui::Button::new(label)
                            .right_text(egui::RichText::new(parent_hint(path)).weak().small()),
                    );
                    if resp.on_hover_text(path.display().to_string()).clicked() {
                        chosen = Some(path.clone());
                    }
                }
                ui.separator();
                if ui.button("Clear Recent").clicked() {
                    self.recent.clear();
                }
                if let Some(path) = chosen {
                    if self.confirm_discard() {
                        self.open_path(path);
                    }
                }
            });
            ui.separator();
            if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                self.save();
            }
            if ui.add(egui::Button::new("Save As...").shortcut_text("Ctrl+Shift+S")).clicked() {
                self.save_as();
            }
            ui.separator();
            if ui.add(egui::Button::new("Exit").shortcut_text("Alt+F4")).clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Edit", |ui| {
            ui.set_min_width(240.0);
            let can_format = self.parsed.is_some();
            if ui.add_enabled(can_format, egui::Button::new("Format Document")).clicked() {
                self.reformat(false);
            }
            if ui.add_enabled(can_format, egui::Button::new("Minify Document")).clicked() {
                self.reformat(true);
            }
            ui.separator();
            if ui.add(egui::Button::new("Paste as Document").shortcut_text("Ctrl+V")).clicked() {
                match file_ops::clipboard_text() {
                    Ok(text) => self.paste_document(text),
                    Err(e) => self.set_status(e, true),
                }
            }
            if ui.add(egui::Button::new("Copy Document").shortcut_text("Ctrl+Shift+C")).clicked() {
                let ctx = ui.ctx().clone();
                self.copy_document(&ctx);
            }
            ui.separator();
            if ui.add(egui::Button::new("Find...").shortcut_text("Ctrl+F")).clicked() {
                let ctx = ui.ctx().clone();
                self.open_find(&ctx, false);
            }
            if ui.add(egui::Button::new("Replace...").shortcut_text("Ctrl+Alt+F")).clicked() {
                let ctx = ui.ctx().clone();
                self.open_find(&ctx, true);
            }
            if ui.add(egui::Button::new("Find Next").shortcut_text("F3")).clicked() {
                self.find.pending = Some(FindAction::Next);
            }
            if ui.add(egui::Button::new("Find Previous").shortcut_text("Shift+F3")).clicked() {
                self.find.pending = Some(FindAction::Prev);
            }
        });
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("View", |ui| {
            ui.set_min_width(200.0);
            let mut mode = self.view_mode;
            ui.radio_value(&mut mode, ViewMode::Blocks, "Blocks");
            ui.radio_value(&mut mode, ViewMode::Raw, "Raw Text");
            if mode != self.view_mode {
                self.set_mode(mode);
            }
            ui.separator();
            let blocks = self.view_mode == ViewMode::Blocks;
            if ui.add_enabled(blocks, egui::Button::new("Expand All Blocks")).clicked() {
                self.force_open = Some(true);
            }
            if ui.add_enabled(blocks, egui::Button::new("Collapse All Blocks")).clicked() {
                self.force_open = Some(false);
            }
            ui.separator();
            let mut dark = ui.ctx().theme() == egui::Theme::Dark;
            if ui.checkbox(&mut dark, "Dark Mode").changed() {
                let pref = if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light };
                ui.ctx().set_theme(pref);
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let pal = Palette::for_dark(ui.visuals().dark_mode);
        ui.horizontal(|ui| {
            match &self.parse_error {
                Some(err) => {
                    let short: String = err.chars().take(90).collect();
                    let short = if short.len() < err.len() { format!("{}…", short) } else { short };
                    ui.colored_label(pal.error, format!("Error: {}", short)).on_hover_text(err);
                }
                None if self.parsed.is_some() => {
                    let label = if self.jsonl { "Valid JSON Lines" } else { "Valid JSON" };
                    ui.colored_label(pal.ok, label);
                }
                None => {
                    ui.colored_label(pal.meta, "Empty");
                }
            }

            ui.separator();
            ui.label(format!("{} keys", self.stats.key_count));
            ui.separator();
            ui.label(format_bytes(self.json_text.len()));

            ui.separator();
            match &self.file_path {
                Some(path) => {
                    let full = path.display().to_string();
                    ui.add(egui::Label::new(&full).truncate()).on_hover_text(full.clone());
                }
                None => {
                    ui.label("No file");
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(st) = &self.status {
                    let col = if st.is_error { pal.error } else { pal.ok };
                    ui.colored_label(col, &st.text);
                }
            });
        });
    }
}

impl ForgeApp {
    fn main_view(&mut self, ui: &mut egui::Ui) {
        self.find.hits.clear();
        match self.view_mode {
            ViewMode::Raw => {
                let ranges: Vec<find::Range> =
                    if self.find.active() { self.find.raw_ranges(&self.json_text).to_vec() } else { Vec::new() };
                if !ranges.is_empty() {
                    self.find.hits.push(BlockHit { index: 0, pointer: String::new(), ranges: ranges.clone() });
                }
                let current = self.find.current.as_ref().filter(|t| t.pointer.is_empty()).map(|t| t.range);
                let rf = RawFind {
                    ranges: &ranges,
                    current,
                    scroll: &mut self.find.scroll_pending,
                    select: &mut self.find.select_pending,
                    highlight: self.find.open,
                };
                if editor_view::show(ui, &mut self.json_text, self.parse_error.as_deref(), Some(rf)) {
                    self.modified = true;
                    self.raw_dirty_at = Some(Instant::now());
                }
            }
            ViewMode::Blocks => {
                let mut edits: Vec<PendingEdit> = Vec::new();
                let pal = Palette::for_dark(ui.visuals().dark_mode);
                let mut cx = BlockCtx {
                    buffers: &mut self.block_buffers,
                    edits: &mut edits,
                    find: &mut self.find,
                    scroll_to: &mut self.scroll_to,
                    force_open: self.force_open,
                    stats: &self.stats,
                    stale_error: self.parse_error.as_deref(),
                    pal,
                    total: 0,
                };
                block_view::show(ui, &self.parsed, &mut cx);

                self.force_open = None;
                if !edits.is_empty() {
                    self.apply_edits(edits);
                }

                // A tree click that no block claimed: retry with the parent
                // pointer next frame, until the root gives up.
                if let Some(unresolved) = self.scroll_to.take() {
                    self.scroll_to = pointer_parent(&unresolved);
                    if self.scroll_to.is_some() {
                        ui.ctx().request_repaint();
                    }
                }
            }
        }

        // Hits for this frame are complete: settle the current match and
        // run whatever the panel or a shortcut asked for.
        self.find.validate_current();
        self.find.resolve_seek();
        if let Some(action) = self.find.pending.take() {
            let ctx = ui.ctx().clone();
            self.run_find_action(&ctx, action);
        }
        if self.find.scroll_pending || self.find.select_pending || self.find.seek.is_some() {
            ui.ctx().request_repaint();
        }
    }
}

impl eframe::App for ForgeApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_autoscroll(ctx);
        self.handle_shortcuts(ctx);
        self.handle_dropped_files(ctx);
        self.handle_debounces(ctx);
        self.handle_close(ctx);

        let title = self.window_title();
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu_bar").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));

        egui::Panel::left("tree_panel")
            .default_size(450.0)
            .min_size(200.0)
            .show(ui, |ui| {
                if let Some(ptr) = tree_view::show(ui, &self.parsed) {
                    if self.view_mode == ViewMode::Blocks {
                        self.scroll_to = Some(ptr);
                    }
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            self.find.refresh();
            if self.find.open {
                let raw = self.view_mode == ViewMode::Raw;
                let fill = ui.visuals().panel_fill;
                let frame = egui::Frame::new()
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
                    .inner_margin(egui::Margin::symmetric(10, 8));
                egui::Panel::bottom("find_panel").resizable(false).frame(frame).show(ui, |ui| {
                    if let Some(action) = find::panel(ui, &mut self.find, raw) {
                        self.find.pending = Some(action);
                    }
                });
                self.find.refresh();
            }
            egui::CentralPanel::default().show(ui, |ui| self.main_view(ui));
        });
    }
}
