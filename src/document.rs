//! One open document: the content of a tab.
//!
//! Two representations exist: `json_text` (raw) and `parsed`. Exactly one of
//! them is authoritative at any moment:
//! - Raw mode edits set `raw_dirty_at`; after a debounce the text is reparsed.
//! - Block edits mutate `parsed` and set `text_dirty_at`; after a debounce the
//!   text is regenerated. `flush()` forces both to agree before save/format.

use eframe::egui;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::find::FindState;
use crate::model::{apply_edit, BlockBuffer, DocStats, PendingEdit};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Blocks,
    Raw,
}

pub const SAMPLE: &str = r#"{
  "example": "Open a JSON file, drop one here, or press Paste / Ctrl+V to load the clipboard",
  "array": [1, 2, 3],
  "nested": {
    "key": "value",
    "flag": true
  }
}"#;

pub struct Document {
    /// Unique per tab. Scopes the egui state of every widget that shows this
    /// document, so two tabs never share a cursor, undo stack or scroll offset.
    pub id: u64,
    pub json_text: String,
    pub parsed: Option<Value>,
    pub parse_error: Option<String>,
    pub file_path: Option<PathBuf>,
    pub modified: bool,
    pub view_mode: ViewMode,
    /// Serialize without whitespace (file was minified when loaded).
    pub compact: bool,
    /// Document is JSON Lines: `parsed` is an array of one value per line.
    pub jsonl: bool,
    pub block_buffers: HashMap<String, BlockBuffer>,
    pub stats: DocStats,
    pub raw_dirty_at: Option<Instant>,
    pub text_dirty_at: Option<Instant>,
    pub scroll_to: Option<String>,
    pub force_open: Option<bool>,
    pub find: FindState,
    /// Last text field that had keyboard focus; Edit > Transform Text acts on it.
    pub last_editor: Option<egui::Id>,
    /// Created as an untitled starter (sample or "New"). While it stays
    /// unmodified, opening a file reuses its tab instead of adding one.
    placeholder: bool,
}

pub fn serialize(val: &Value, compact: bool, jsonl: bool) -> String {
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

fn format_error(e: &serde_json::Error) -> String {
    format!("Line {}, col {}: {}", e.line(), e.column(), e)
}

impl Document {
    pub fn new(id: u64) -> Self {
        Self {
            id,
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
            scroll_to: None,
            force_open: None,
            find: FindState::default(),
            last_editor: None,
            placeholder: false,
        }
    }

    /// Untitled starter document holding `text`.
    pub fn placeholder(id: u64, text: &str) -> Self {
        let mut doc = Self::new(id);
        doc.load_text(text.to_string(), None);
        doc.compact = false;
        doc.view_mode = ViewMode::Blocks;
        doc.placeholder = true;
        doc
    }

    /// Still the untouched starter: safe to replace with an opened file.
    pub fn is_placeholder(&self) -> bool {
        self.placeholder && !self.modified && self.file_path.is_none()
    }

    pub fn name(&self) -> String {
        self.file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".to_string())
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

    /// Replace the whole document with `text`. Check `parse_error` afterwards.
    pub fn load_text(&mut self, text: String, path: Option<PathBuf>) {
        self.json_text = text;
        self.file_path = path;
        self.modified = false;
        self.placeholder = false;
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
                self.parse_error = Some(e);
                self.view_mode = ViewMode::Raw;
            }
        }
    }

    pub fn reparse_from_raw(&mut self) {
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

    pub fn sync_text_from_parsed(&mut self) {
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
    pub fn flush(&mut self) {
        if self.raw_dirty_at.is_some() {
            self.reparse_from_raw();
        }
        if self.text_dirty_at.is_some() {
            self.sync_text_from_parsed();
        }
    }

    pub fn apply_edits(&mut self, edits: Vec<PendingEdit>) {
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

    pub fn set_mode(&mut self, mode: ViewMode) {
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
}
