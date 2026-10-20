//! Application state and frame logic: the window, its tabs, menus, status bar
//! and the per-frame plumbing. Per-document logic lives in `document.rs`.

use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crate::autoscroll::{self, AutoScroll};
use crate::block_view::{self, BlockCtx, Palette};
use crate::document::{self, Document, ViewMode, SAMPLE};
use crate::editor_view::{self, RawFind};
use crate::file_ops::{self, Confirm};
use crate::find::{self, BlockHit, FindAction, FindState, Seek};
use crate::model::{format_bytes, pointer_parent, PendingEdit};
use crate::recent::RecentFiles;
use crate::single_instance;
use crate::tab_bar::{self, TabEvent, TabLabel};
use crate::text_ops::{self, TextOp};
use crate::tree_view;

const RAW_DEBOUNCE: Duration = Duration::from_millis(250);
const SYNC_DEBOUNCE: Duration = Duration::from_millis(300);
const STATUS_TTL: Duration = Duration::from_secs(6);

struct StatusMsg {
    text: String,
    is_error: bool,
    at: Instant,
}

pub struct ForgeApp {
    /// Open tabs in strip order. Never empty once constructed.
    docs: Vec<Document>,
    active: usize,
    next_id: u64,
    status: Option<StatusMsg>,
    recent: RecentFiles,
    last_title: String,
    allow_close: bool,
    close_now: bool,
    autoscroll: Option<AutoScroll>,
    /// Files handed over by later launches (see `single_instance`).
    incoming: Receiver<Vec<PathBuf>>,
}

/// Short folder hint for a recent-file entry: the last directory component.
fn parent_hint(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Same file on disk, tolerating different spellings of the path.
fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// The find panel is one per window (as in Atom): its query and options
/// follow the user from tab to tab, while the matches belong to each document.
fn carry_find(from: &FindState, to: &mut FindState) {
    to.open = from.open;
    to.query = from.query.clone();
    to.replacement = from.replacement.clone();
    to.use_regex = from.use_regex;
    to.case_sensitive = from.case_sensitive;
    to.whole_word = from.whole_word;
}

impl ForgeApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial: Vec<PathBuf>,
        listener: Option<interprocess::local_socket::Listener>,
    ) -> Self {
        // Maximize after creation: setting it in the ViewportBuilder together
        // with an inner size leaves the window flagged maximized at the small size.
        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        // Light theme by default regardless of the OS setting; View > Dark Mode toggles it.
        cc.egui_ctx.set_theme(egui::ThemePreference::Light);

        let (tx, incoming) = mpsc::channel();
        if let Some(listener) = listener {
            single_instance::serve(listener, cc.egui_ctx.clone(), tx);
        }

        let mut app = Self {
            docs: Vec::new(),
            active: 0,
            next_id: 0,
            status: None,
            recent: RecentFiles::load(),
            last_title: String::new(),
            allow_close: false,
            close_now: false,
            autoscroll: None,
            incoming,
        };

        for path in initial {
            app.open_path(path);
        }
        if app.docs.is_empty() {
            let doc = app.placeholder(SAMPLE);
            app.docs.push(doc);
        }
        app
    }

    // ----- tabs -------------------------------------------------------------

    fn doc(&self) -> &Document {
        &self.docs[self.active]
    }

    fn doc_mut(&mut self) -> &mut Document {
        &mut self.docs[self.active]
    }

    fn fresh(&mut self) -> Document {
        self.next_id += 1;
        Document::new(self.next_id)
    }

    fn placeholder(&mut self, text: &str) -> Document {
        self.next_id += 1;
        Document::placeholder(self.next_id, text)
    }

    fn activate(&mut self, i: usize) {
        if i >= self.docs.len() || i == self.active {
            return;
        }
        let (lo, hi) = self.docs.split_at_mut(self.active.max(i));
        let (from, to) = if i > self.active { (&lo[self.active], &mut hi[0]) } else { (&hi[0], &mut lo[i]) };
        carry_find(&from.find, &mut to.find);
        self.active = i;
    }

    fn cycle_tab(&mut self, delta: isize) {
        let n = self.docs.len() as isize;
        let i = (self.active as isize + delta).rem_euclid(n) as usize;
        self.activate(i);
    }

    /// Tab that receives a document about to be loaded: the active tab when it
    /// is an untouched starter, otherwise a new one at the end of the strip.
    fn open_slot(&mut self) -> usize {
        let mut doc = self.fresh();
        if let Some(cur) = self.docs.get(self.active) {
            carry_find(&cur.find, &mut doc.find);
        }
        let reuse = self.docs.get(self.active).is_some_and(|d| d.is_placeholder());
        if reuse {
            self.docs[self.active] = doc;
        } else {
            self.docs.push(doc);
            self.active = self.docs.len() - 1;
        }
        self.active
    }

    fn new_document(&mut self) {
        let mut doc = self.placeholder("{}");
        carry_find(&self.doc().find, &mut doc.find);
        self.docs.push(doc);
        self.active = self.docs.len() - 1;
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.docs.len() || !self.confirm_discard(i) {
            return;
        }
        let closed = self.docs.remove(i);
        if self.docs.is_empty() {
            let mut doc = self.placeholder(SAMPLE);
            carry_find(&closed.find, &mut doc.find);
            self.docs.push(doc);
            self.active = 0;
        } else if i < self.active {
            self.active -= 1;
        } else if i == self.active {
            self.active = i.min(self.docs.len() - 1);
            carry_find(&closed.find, &mut self.docs[self.active].find);
        }
    }

    fn close_other_tabs(&mut self) {
        let mut i = self.docs.len();
        while i > 0 {
            i -= 1;
            if i != self.active {
                self.close_tab(i);
            }
        }
    }

    // ----- status -----------------------------------------------------------

    fn set_status(&mut self, text: impl Into<String>, is_error: bool) {
        self.status = Some(StatusMsg { text: text.into(), is_error, at: Instant::now() });
    }

    fn window_title(&self) -> String {
        let doc = self.doc();
        if doc.modified {
            format!("JSON Forge - {} *", doc.name())
        } else {
            format!("JSON Forge - {}", doc.name())
        }
    }

    // ----- file operations --------------------------------------------------

    fn write_to(&mut self, i: usize, path: &Path) -> bool {
        match file_ops::write(path, &self.docs[i].json_text) {
            Ok(()) => {
                self.docs[i].modified = false;
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

    fn save_doc(&mut self, i: usize) -> bool {
        self.docs[i].flush();
        match self.docs[i].file_path.clone() {
            Some(path) => self.write_to(i, &path),
            None => self.save_as_doc(i),
        }
    }

    fn save_as_doc(&mut self, i: usize) -> bool {
        self.docs[i].flush();
        let Some(path) = file_ops::pick_save(self.docs[i].file_path.as_deref()) else { return false };
        if self.write_to(i, &path) {
            self.docs[i].file_path = Some(path);
            true
        } else {
            false
        }
    }

    fn save(&mut self) -> bool {
        self.save_doc(self.active)
    }

    fn save_as(&mut self) -> bool {
        self.save_as_doc(self.active)
    }

    /// Returns true when it is OK to discard tab `i`.
    fn confirm_discard(&mut self, i: usize) -> bool {
        if !self.docs[i].modified {
            return true;
        }
        let name = self.docs[i].name();
        match file_ops::confirm_unsaved(&name) {
            Confirm::Save => self.save_doc(i),
            Confirm::Discard => true,
            Confirm::Cancel => false,
        }
    }

    fn open_dialog(&mut self) {
        for path in file_ops::pick_open() {
            self.open_path(path);
        }
    }

    /// Open `path` in its own tab, or switch to it when it is already open.
    fn open_path(&mut self, path: PathBuf) {
        if let Some(i) = self.docs.iter().position(|d| d.file_path.as_deref().is_some_and(|p| same_path(p, &path))) {
            self.activate(i);
            return;
        }
        match file_ops::read(&path) {
            Ok(text) => {
                let name = path.display().to_string();
                self.recent.push(&path);
                let i = self.open_slot();
                self.docs[i].load_text(text, Some(path));
                match self.docs[i].parse_error.clone() {
                    None => self.set_status(format!("Opened {}", name), false),
                    Some(e) => self.set_status(format!("Invalid JSON: {}", e), true),
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
        let i = self.open_slot();
        self.docs[i].load_text(text, None);
        match self.docs[i].parse_error.clone() {
            None => {
                self.docs[i].view_mode = ViewMode::Blocks;
                self.set_status("Loaded JSON from clipboard", false);
            }
            Some(e) => self.set_status(format!("Invalid JSON: {}", e), true),
        }
    }

    fn copy_document(&mut self, ctx: &egui::Context) {
        self.doc_mut().flush();
        ctx.copy_text(self.doc().json_text.clone());
        self.set_status("Document copied to clipboard", false);
    }

    fn reformat(&mut self, compact: bool) {
        self.doc_mut().flush();
        let doc = self.doc();
        if doc.parse_error.is_some() || doc.parsed.is_none() {
            self.set_status("Fix the JSON error before formatting", true);
            return;
        }
        if self.doc().jsonl {
            self.set_status("JSON Lines: normalized to one record per line", false);
        } else {
            self.doc_mut().compact = compact;
        }
        let doc = self.doc_mut();
        let Some(v) = &doc.parsed else { return };
        let text = document::serialize(v, doc.compact, doc.jsonl);
        if text != doc.json_text {
            doc.json_text = text;
            doc.modified = true;
        }
    }

    // ----- editor context menu ----------------------------------------------

    /// Run a context-menu item on the text field `id` of the active document.
    fn text_op(&mut self, ctx: &egui::Context, id: egui::Id, op: TextOp) {
        let clipboard = if op == TextOp::Paste {
            match file_ops::clipboard_text() {
                Ok(text) => Some(text),
                Err(e) => {
                    self.set_status(e, true);
                    return;
                }
            }
        } else {
            None
        };
        let doc = &mut self.docs[self.active];
        let did = doc.id;
        let raw = id == editor_view::editor_id(did);
        let pointer = if raw { None } else { doc.block_buffers.keys().find(|p| block_view::edit_id(did, p) == id).cloned() };
        let (Some(mut state), true) = (egui::TextEdit::load_state(ctx, id), raw || pointer.is_some()) else { return };
        let text = match &pointer {
            None => &mut doc.json_text,
            Some(p) => &mut doc.block_buffers.get_mut(p).expect("pointer came from the map").text,
        };
        let result = text_ops::apply(op, text, &mut state, clipboard.as_deref());
        state.store(ctx, id);
        ctx.memory_mut(|m| m.request_focus(id));
        let applied = match result {
            Ok(applied) => applied,
            Err(msg) => {
                self.set_status(msg, true);
                return;
            }
        };
        if let Some(copied) = applied.copied {
            ctx.copy_text(copied);
        }
        if !applied.changed {
            return;
        }
        match pointer {
            None => {
                doc.modified = true;
                doc.raw_dirty_at = Some(Instant::now());
            }
            Some(p) => {
                let buf = doc.block_buffers.get_mut(&p).expect("pointer came from the map");
                buf.rev += 1;
                let edit = PendingEdit { pointer: p, kind: buf.kind, text: buf.text.clone() };
                doc.apply_edits(vec![edit]);
            }
        }
    }

    // ----- find / replace ---------------------------------------------------

    /// Single-line selection of whichever editor has keyboard focus.
    fn selected_text(&self, ctx: &egui::Context) -> Option<String> {
        let doc = self.doc();
        let focused = ctx.memory(|m| m.focused())?;
        let text: &str = if focused == editor_view::editor_id(doc.id) {
            &doc.json_text
        } else {
            let (_, buf) = doc.block_buffers.iter().find(|(p, _)| block_view::edit_id(doc.id, p) == focused)?;
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
        let sel = self.selected_text(ctx);
        let find = &mut self.doc_mut().find;
        if let Some(sel) = sel {
            find.query = sel;
        }
        find.open = true;
        if replace {
            find.focus_replace = true;
        } else {
            find.focus_find = true;
        }
    }

    /// Where "Find" starts when there is no current match.
    fn find_start(&self, ctx: &egui::Context) -> Seek {
        let doc = self.doc();
        if doc.view_mode != ViewMode::Raw {
            return Seek::Forward { block: 0, pos: 0 };
        }
        let pos = egui::TextEdit::load_state(ctx, editor_view::editor_id(doc.id))
            .and_then(|s| s.cursor.char_range())
            .map(|r| r.primary.index.0.min(r.secondary.index.0))
            .map(|ci| doc.json_text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(doc.json_text.len()))
            .unwrap_or(0);
        Seek::Forward { block: 0, pos }
    }

    fn run_find_action(&mut self, ctx: &egui::Context, action: FindAction) {
        let start = self.find_start(ctx);
        let mut status: Option<(String, bool)> = None;
        let doc = &mut self.docs[self.active];
        let raw = doc.view_mode == ViewMode::Raw;
        match action {
            FindAction::Close => {
                doc.find.open = false;
                doc.find.filter = false;
                // Hand focus back to the editor holding the current match.
                let id = match &doc.find.current {
                    Some(t) if !raw => Some(block_view::edit_id(doc.id, &t.pointer)),
                    _ if raw => Some(editor_view::editor_id(doc.id)),
                    _ => None,
                };
                if let Some(id) = id {
                    ctx.memory_mut(|m| m.request_focus(id));
                    doc.find.select_pending = doc.find.current.is_some();
                }
            }
            FindAction::FindAll => {
                if raw {
                    status = Some(("Find All filters blocks: switch to Blocks mode to use it".into(), true));
                } else {
                    doc.find.filter = !doc.find.filter;
                }
            }
            FindAction::Next | FindAction::Prev => {
                if doc.find.query.is_empty() {
                    doc.find.open = true;
                    doc.find.focus_find = true;
                } else {
                    doc.find.step(action == FindAction::Next, start);
                }
            }
            FindAction::Replace => {
                if !doc.find.active() {
                    return;
                }
                let Some(t) = doc.find.current.clone() else {
                    doc.find.step(true, start);
                    return;
                };
                let Some(m) = &doc.find.matcher else { return };
                let repl = doc.find.replacement.clone();
                let result = if raw {
                    m.replace_one(&mut doc.json_text, t.range, &repl).map(|end| (end, None))
                } else {
                    match doc.block_buffers.get_mut(&t.pointer) {
                        Some(buf) => m.replace_one(&mut buf.text, t.range, &repl).map(|end| {
                            buf.rev += 1;
                            (end, Some(PendingEdit { pointer: t.pointer.clone(), kind: buf.kind, text: buf.text.clone() }))
                        }),
                        None => None,
                    }
                };
                doc.find.current = None;
                match result {
                    Some((end, edit)) => {
                        if let Some(edit) = edit {
                            doc.apply_edits(vec![edit]);
                        } else {
                            doc.modified = true;
                            doc.raw_dirty_at = Some(Instant::now());
                        }
                        doc.find.seek = Some(Seek::Forward { block: t.block, pos: end });
                    }
                    None => status = Some(("The text under the match changed; find it again".into(), true)),
                }
            }
            FindAction::ReplaceAll => {
                let Some(m) = &doc.find.matcher else { return };
                let repl = doc.find.replacement.clone();
                let (count, blocks) = if raw {
                    let (new_text, n) = m.replace_all(&doc.json_text, &repl);
                    if n > 0 {
                        doc.json_text = new_text;
                        doc.modified = true;
                        doc.raw_dirty_at = Some(Instant::now());
                    }
                    (n, 0)
                } else {
                    let pointers: Vec<String> = doc.find.hits.iter().map(|h| h.pointer.clone()).collect();
                    let mut edits = Vec::new();
                    let mut n = 0;
                    for p in pointers {
                        let Some(buf) = doc.block_buffers.get_mut(&p) else { continue };
                        let (new_text, c) = m.replace_all(&buf.text, &repl);
                        if c > 0 {
                            buf.text = new_text;
                            buf.rev += 1;
                            n += c;
                            edits.push(PendingEdit { pointer: p, kind: buf.kind, text: buf.text.clone() });
                        }
                    }
                    let blocks = edits.len();
                    doc.apply_edits(edits);
                    (n, blocks)
                };
                doc.find.current = None;
                status = Some(if count == 0 {
                    (format!("No matches for \"{}\"", doc.find.query), true)
                } else if raw {
                    (format!("Replaced {} occurrence(s)", count), false)
                } else {
                    (format!("Replaced {} occurrence(s) in {} block(s)", count, blocks), false)
                });
            }
        }
        if let Some((text, is_error)) = status {
            self.set_status(text, is_error);
        }
    }

    // ----- frame pieces -----------------------------------------------------

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let cmd_shift = Modifiers::COMMAND.plus(Modifiers::SHIFT);
        let cmd_alt = Modifiers::COMMAND.plus(Modifiers::ALT);

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

        // Tabs.
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::W)) {
            self.close_tab(self.active);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Tab) || i.consume_key(Modifiers::COMMAND, Key::PageDown)) {
            self.cycle_tab(1);
        }
        if ctx.input_mut(|i| i.consume_key(cmd_shift, Key::Tab) || i.consume_key(Modifiers::COMMAND, Key::PageUp)) {
            self.cycle_tab(-1);
        }
        const NUMS: [Key; 9] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        for (n, key) in NUMS.iter().enumerate() {
            if ctx.input_mut(|i| i.consume_key(Modifiers::ALT, *key)) {
                // Alt+9 is always the last tab, as in Atom and the browsers.
                let i = if n == 8 { self.docs.len() - 1 } else { n };
                self.activate(i);
            }
        }

        // Find / replace (Atom keymap).
        if ctx.input_mut(|i| i.consume_key(cmd_alt, Key::F)) {
            self.open_find(ctx, true);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            self.open_find(ctx, false);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::E)) {
            if let Some(sel) = self.selected_text(ctx) {
                self.doc_mut().find.query = sel;
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F3)) {
            self.doc_mut().find.pending = Some(FindAction::Next);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::F3)) {
            self.doc_mut().find.pending = Some(FindAction::Prev);
        }
        if self.doc().find.open {
            let find = &mut self.docs[self.active].find;
            if ctx.input_mut(|i| i.consume_key(cmd_alt, Key::Slash)) {
                find.use_regex = !find.use_regex;
            }
            if ctx.input_mut(|i| i.consume_key(cmd_alt, Key::C)) {
                find.case_sensitive = !find.case_sensitive;
            }
            if ctx.input_mut(|i| i.consume_key(cmd_alt, Key::W)) {
                find.whole_word = !find.whole_word;
            }
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                find.pending = Some(FindAction::Close);
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

    /// Files from another launch: open them and bring the window forward.
    fn handle_incoming(&mut self, ctx: &egui::Context) {
        let mut any = false;
        while let Ok(paths) = self.incoming.try_recv() {
            any = true;
            for path in paths {
                self.open_path(path);
            }
        }
        if any {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| {
                    let p = f.path();
                    if p.as_os_str().is_empty() {
                        None
                    } else {
                        Some(p.to_path_buf())
                    }
                })
                .collect()
        });
        for path in dropped {
            self.open_path(path);
        }
    }

    fn handle_debounces(&mut self, ctx: &egui::Context) {
        for doc in &mut self.docs {
            if let Some(at) = doc.raw_dirty_at {
                let elapsed = at.elapsed();
                if elapsed >= RAW_DEBOUNCE {
                    doc.reparse_from_raw();
                } else {
                    ctx.request_repaint_after(RAW_DEBOUNCE - elapsed);
                }
            }
            if let Some(at) = doc.text_dirty_at {
                let elapsed = at.elapsed();
                if elapsed >= SYNC_DEBOUNCE {
                    doc.sync_text_from_parsed();
                } else {
                    ctx.request_repaint_after(SYNC_DEBOUNCE - elapsed);
                }
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
        let unsaved = self.docs.iter().any(|d| d.modified);
        if ctx.input(|i| i.viewport().close_requested()) && unsaved && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            for i in 0..self.docs.len() {
                if self.docs[i].modified {
                    self.active = i;
                    if !self.confirm_discard(i) {
                        return;
                    }
                }
            }
            self.allow_close = true;
            self.close_now = true;
            ctx.request_repaint();
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            self.file_menu(ui);
            self.edit_menu(ui);
            self.view_menu(ui);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                let mut mode = self.doc().view_mode;
                ui.selectable_value(&mut mode, ViewMode::Raw, "Raw");
                ui.selectable_value(&mut mode, ViewMode::Blocks, "Blocks");
                self.doc_mut().set_mode(mode);
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
                    self.open_path(path);
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
            if ui.add(egui::Button::new("Close Tab").shortcut_text("Ctrl+W")).clicked() {
                self.close_tab(self.active);
            }
            let others = self.docs.len() > 1;
            if ui.add_enabled(others, egui::Button::new("Close Other Tabs")).clicked() {
                self.close_other_tabs();
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
            let can_format = self.doc().parsed.is_some();
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
                self.doc_mut().find.pending = Some(FindAction::Next);
            }
            if ui.add(egui::Button::new("Find Previous").shortcut_text("Shift+F3")).clicked() {
                self.doc_mut().find.pending = Some(FindAction::Prev);
            }
        });
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("View", |ui| {
            ui.set_min_width(220.0);
            let mut mode = self.doc().view_mode;
            ui.radio_value(&mut mode, ViewMode::Blocks, "Blocks");
            ui.radio_value(&mut mode, ViewMode::Raw, "Raw Text");
            self.doc_mut().set_mode(mode);
            ui.separator();
            let blocks = self.doc().view_mode == ViewMode::Blocks;
            if ui.add_enabled(blocks, egui::Button::new("Expand All Blocks")).clicked() {
                self.doc_mut().force_open = Some(true);
            }
            if ui.add_enabled(blocks, egui::Button::new("Collapse All Blocks")).clicked() {
                self.doc_mut().force_open = Some(false);
            }
            ui.separator();
            let tabs = self.docs.len() > 1;
            if ui.add_enabled(tabs, egui::Button::new("Next Tab").shortcut_text("Ctrl+Tab")).clicked() {
                self.cycle_tab(1);
            }
            if ui.add_enabled(tabs, egui::Button::new("Previous Tab").shortcut_text("Ctrl+Shift+Tab")).clicked() {
                self.cycle_tab(-1);
            }
            ui.separator();
            let mut dark = ui.ctx().theme() == egui::Theme::Dark;
            if ui.checkbox(&mut dark, "Dark Mode").changed() {
                let pref = if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light };
                ui.ctx().set_theme(pref);
            }
        });
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let labels: Vec<TabLabel> = self
            .docs
            .iter()
            .map(|d| TabLabel {
                title: d.name(),
                modified: d.modified,
                tooltip: d
                    .file_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "Not saved yet".to_string()),
            })
            .collect();
        match tab_bar::show(ui, &labels, self.active) {
            Some(TabEvent::Select(i)) => self.activate(i),
            Some(TabEvent::Close(i)) => self.close_tab(i),
            Some(TabEvent::New) => self.new_document(),
            None => {}
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let pal = Palette::for_dark(ui.visuals().dark_mode);
        let doc = self.doc();
        ui.horizontal(|ui| {
            match &doc.parse_error {
                Some(err) => {
                    let short: String = err.chars().take(90).collect();
                    let short = if short.len() < err.len() { format!("{}…", short) } else { short };
                    ui.colored_label(pal.error, format!("Error: {}", short)).on_hover_text(err);
                }
                None if doc.parsed.is_some() => {
                    let label = if doc.jsonl { "Valid JSON Lines" } else { "Valid JSON" };
                    ui.colored_label(pal.ok, label);
                }
                None => {
                    ui.colored_label(pal.meta, "Empty");
                }
            }

            ui.separator();
            ui.label(format!("{} keys", doc.stats.key_count));
            ui.separator();
            ui.label(format_bytes(doc.json_text.len()));

            ui.separator();
            match &doc.file_path {
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

    fn main_view(&mut self, ui: &mut egui::Ui) {
        let mut op: Option<(egui::Id, TextOp)> = None;
        let doc = &mut self.docs[self.active];
        doc.find.hits.clear();
        match doc.view_mode {
            ViewMode::Raw => {
                let ranges: Vec<find::Range> =
                    if doc.find.active() { doc.find.raw_ranges(&doc.json_text).to_vec() } else { Vec::new() };
                if !ranges.is_empty() {
                    doc.find.hits.push(BlockHit { index: 0, pointer: String::new(), ranges: ranges.clone() });
                }
                let current = doc.find.current.as_ref().filter(|t| t.pointer.is_empty()).map(|t| t.range);
                let rf = RawFind {
                    ranges: &ranges,
                    current,
                    scroll: &mut doc.find.scroll_pending,
                    select: &mut doc.find.select_pending,
                    highlight: doc.find.open,
                };
                let id = editor_view::editor_id(doc.id);
                let mut picked = None;
                if editor_view::show(ui, id, &mut doc.json_text, doc.parse_error.as_deref(), Some(rf), &mut picked) {
                    doc.modified = true;
                    doc.raw_dirty_at = Some(Instant::now());
                }
                op = picked.map(|p| (id, p));
            }
            ViewMode::Blocks => {
                let mut edits: Vec<PendingEdit> = Vec::new();
                let pal = Palette::for_dark(ui.visuals().dark_mode);
                let mut cx = BlockCtx {
                    doc: doc.id,
                    buffers: &mut doc.block_buffers,
                    edits: &mut edits,
                    find: &mut doc.find,
                    scroll_to: &mut doc.scroll_to,
                    force_open: doc.force_open,
                    stats: &doc.stats,
                    stale_error: doc.parse_error.as_deref(),
                    pal,
                    total: 0,
                    op: &mut op,
                };
                block_view::show(ui, &doc.parsed, &mut cx);

                doc.force_open = None;
                if !edits.is_empty() {
                    doc.apply_edits(edits);
                }

                // A tree click that no block claimed: retry with the parent
                // pointer next frame, until the root gives up.
                if let Some(unresolved) = doc.scroll_to.take() {
                    doc.scroll_to = pointer_parent(&unresolved);
                    if doc.scroll_to.is_some() {
                        ui.ctx().request_repaint();
                    }
                }
            }
        }

        // Hits for this frame are complete: settle the current match and
        // run whatever the panel or a shortcut asked for.
        doc.find.validate_current();
        doc.find.resolve_seek();
        let pending = doc.find.pending.take();
        if let Some((id, op)) = op {
            let ctx = ui.ctx().clone();
            self.text_op(&ctx, id, op);
        }
        if let Some(action) = pending {
            let ctx = ui.ctx().clone();
            self.run_find_action(&ctx, action);
        }
        let find = &self.doc().find;
        if find.scroll_pending || find.select_pending || find.seek.is_some() {
            ui.ctx().request_repaint();
        }
    }
}

impl eframe::App for ForgeApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        autoscroll::update(&mut self.autoscroll, ctx);
        self.handle_shortcuts(ctx);
        self.handle_incoming(ctx);
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
        autoscroll::clear(ui.ctx());
        egui::Panel::top("menu_bar").show(ui, |ui| self.menu_bar(ui));
        let bar_frame = egui::Frame::new().fill(tab_bar::fill(ui.visuals()));
        egui::Panel::top("tab_bar").resizable(false).frame(bar_frame).show(ui, |ui| self.tab_bar(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));

        // Everything that shows the document is scoped by its id so tabs keep
        // separate scroll offsets, collapsed blocks and text cursors.
        egui::Panel::left("tree_panel")
            .default_size(450.0)
            .min_size(200.0)
            .show(ui, |ui| {
                let doc = &mut self.docs[self.active];
                ui.push_id(doc.id, |ui| {
                    if let Some(ptr) = tree_view::show(ui, doc.id, &doc.parsed) {
                        if doc.view_mode == ViewMode::Blocks {
                            doc.scroll_to = Some(ptr);
                        }
                    }
                });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            self.doc_mut().find.refresh();
            if self.doc().find.open {
                let raw = self.doc().view_mode == ViewMode::Raw;
                let fill = ui.visuals().panel_fill;
                let frame = egui::Frame::new()
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
                    .inner_margin(egui::Margin::symmetric(10, 8));
                egui::Panel::bottom("find_panel").resizable(false).frame(frame).show(ui, |ui| {
                    let doc = self.doc_mut();
                    if let Some(action) = find::panel(ui, &mut doc.find, raw) {
                        doc.find.pending = Some(action);
                    }
                });
                self.doc_mut().find.refresh();
            }
            let id = self.doc().id;
            egui::CentralPanel::default().show(ui, |ui| {
                ui.push_id(id, |ui| self.main_view(ui));
            });
        });

        // Last, so its overlay sits above everything and its cursor wins.
        autoscroll::overlay(&self.autoscroll, ui.ctx());
    }
}
