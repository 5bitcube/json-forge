//! Find / Replace panel, modelled on Atom's find-and-replace package.
//!
//! The panel docks at the bottom of the main view. Matching runs over the
//! text the user actually sees: the raw document in Raw mode, the block
//! buffers in Blocks mode. Matches are byte ranges into that text; the
//! "current" match is identified by its block and range so it survives
//! re-rendering, and a `Seek` resolves to a concrete match once the hit list
//! for the frame is known.

use eframe::egui;
use egui::text::{ByteIndex, CCursor, CCursorRange, LayoutJob, LayoutSection, TextFormat};
use egui::{Align, Align2, Color32, FontId, Key, Modifiers, RichText, Stroke};
use regex::Regex;
use std::collections::HashMap;

pub const FIND_ID: &str = "find_query";
pub const REPLACE_ID: &str = "find_replacement";
/// Upper bound on matches collected per text, to keep pathological regexes cheap.
const MAX_HITS: usize = 20_000;

pub type Range = (usize, usize);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FindAction {
    Next,
    Prev,
    Replace,
    ReplaceAll,
    FindAll,
    Close,
}

/// Compiled search pattern. Everything (including plain text) goes through
/// `regex` so case / whole-word options behave identically in every mode.
pub struct Matcher {
    re: Regex,
    /// Plain-text mode: the replacement is literal (no `$1` expansion).
    literal: bool,
}

impl Matcher {
    fn compile(query: &str, regex: bool, case_sensitive: bool, whole_word: bool) -> Result<Self, String> {
        let mut pat = if regex { query.to_string() } else { regex::escape(query) };
        if whole_word {
            pat = format!(r"\b(?:{})\b", pat);
        }
        if !case_sensitive {
            pat = format!("(?i){}", pat);
        }
        Regex::new(&pat)
            .map(|re| Matcher { re, literal: !regex })
            .map_err(|e| short_regex_error(&e))
    }

    /// Non-overlapping, non-empty matches in order.
    pub fn find_all(&self, text: &str) -> Vec<Range> {
        self.re
            .find_iter(text)
            .filter(|m| m.start() < m.end())
            .map(|m| (m.start(), m.end()))
            .take(MAX_HITS)
            .collect()
    }

    /// Replace the match at exactly `range`. Returns the end of the inserted
    /// text, or None when the text no longer matches there.
    pub fn replace_one(&self, text: &mut String, range: Range, replacement: &str) -> Option<usize> {
        let (start, end) = range;
        if end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            return None;
        }
        let caps = self.re.captures_at(text, start)?;
        let m = caps.get(0)?;
        if m.start() != start || m.end() != end {
            return None;
        }
        let mut out = String::new();
        if self.literal {
            out.push_str(replacement);
        } else {
            caps.expand(replacement, &mut out);
        }
        let new_end = start + out.len();
        text.replace_range(start..end, &out);
        Some(new_end)
    }

    /// Returns the rewritten text and the number of replacements.
    pub fn replace_all(&self, text: &str, replacement: &str) -> (String, usize) {
        let n = self.re.find_iter(text).filter(|m| m.start() < m.end()).count();
        if n == 0 {
            return (text.to_string(), 0);
        }
        let out = if self.literal {
            self.re.replace_all(text, regex::NoExpand(replacement))
        } else {
            self.re.replace_all(text, replacement)
        };
        (out.into_owned(), n)
    }
}

fn short_regex_error(e: &regex::Error) -> String {
    match e {
        regex::Error::Syntax(s) => {
            // The syntax error is multi-line; keep the final "error: ..." line.
            s.lines()
                .rev()
                .find(|l| l.trim_start().starts_with("error:"))
                .map(|l| l.trim().trim_start_matches("error:").trim().to_string())
                .unwrap_or_else(|| "invalid regex".to_string())
        }
        regex::Error::CompiledTooBig(_) => "regex too large".to_string(),
        _ => "invalid regex".to_string(),
    }
}

/// Matches found in one block (render order `index`) during the last frame.
pub struct BlockHit {
    pub index: usize,
    pub pointer: String,
    pub ranges: Vec<Range>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target {
    pub block: usize,
    pub pointer: String,
    pub range: Range,
}

#[derive(Clone, Copy, Debug)]
pub enum Seek {
    /// First match at or after `pos` in block `block`, else wrap to the first.
    Forward { block: usize, pos: usize },
    Last,
}

pub struct FindState {
    pub open: bool,
    pub query: String,
    pub replacement: String,
    pub use_regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Blocks mode "Find All": show only blocks that contain a match.
    pub filter: bool,
    pub matcher: Option<Matcher>,
    pub error: Option<String>,
    compiled_for: Option<(String, bool, bool, bool)>,
    pub current: Option<Target>,
    pub seek: Option<Seek>,
    /// Scroll the main view to `current` once.
    pub scroll_pending: bool,
    /// Put the text cursor of the editor that holds `current` on the match once.
    pub select_pending: bool,
    pub focus_find: bool,
    pub focus_replace: bool,
    pub pending: Option<FindAction>,
    /// Hits collected during the last render, in document order.
    pub hits: Vec<BlockHit>,
    /// Blocks: per-pointer cache of (buffer revision, ranges).
    pub cache: HashMap<String, (u64, Vec<Range>)>,
    /// Raw: (text hash, ranges).
    pub raw_cache: Option<(u64, Vec<Range>)>,
}

impl Default for FindState {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            replacement: String::new(),
            use_regex: false,
            case_sensitive: false,
            whole_word: false,
            filter: false,
            matcher: None,
            error: None,
            compiled_for: None,
            current: None,
            seek: None,
            scroll_pending: false,
            select_pending: false,
            focus_find: false,
            focus_replace: false,
            pending: None,
            hits: Vec::new(),
            cache: HashMap::new(),
            raw_cache: None,
        }
    }
}

impl FindState {
    /// Recompile the matcher when the query or options changed.
    pub fn refresh(&mut self) {
        let key = (self.query.clone(), self.use_regex, self.case_sensitive, self.whole_word);
        if self.compiled_for.as_ref() == Some(&key) {
            return;
        }
        self.compiled_for = Some(key);
        self.cache.clear();
        self.raw_cache = None;
        self.current = None;
        if self.query.is_empty() {
            self.matcher = None;
            self.error = None;
            return;
        }
        match Matcher::compile(&self.query, self.use_regex, self.case_sensitive, self.whole_word) {
            Ok(m) => {
                self.matcher = Some(m);
                self.error = None;
            }
            Err(e) => {
                self.matcher = None;
                self.error = Some(e);
            }
        }
    }

    pub fn active(&self) -> bool {
        self.matcher.is_some()
    }

    /// Forget everything tied to the current document.
    pub fn reset_document(&mut self) {
        self.cache.clear();
        self.raw_cache = None;
        self.hits.clear();
        self.current = None;
        self.seek = None;
        self.scroll_pending = false;
        self.select_pending = false;
    }

    pub fn total(&self) -> usize {
        self.hits.iter().map(|h| h.ranges.len()).sum()
    }

    /// Ranges of block `pointer`, for highlighting. Cached by buffer revision.
    pub fn ranges_for(&mut self, pointer: &str, rev: u64, text: &str) -> &[Range] {
        let Some(m) = &self.matcher else { return &[] };
        let entry = self.cache.entry(pointer.to_string()).or_insert_with(|| (u64::MAX, Vec::new()));
        if entry.0 != rev {
            *entry = (rev, m.find_all(text));
        }
        &entry.1
    }

    pub fn raw_ranges(&mut self, text: &str) -> &[Range] {
        let Some(m) = &self.matcher else { return &[] };
        let hash = hash_str(text);
        if self.raw_cache.as_ref().map(|c| c.0) != Some(hash) {
            self.raw_cache = Some((hash, m.find_all(text)));
        }
        &self.raw_cache.as_ref().unwrap().1
    }

    /// 1-based position of `current` in the hit list.
    fn current_index(&self) -> Option<usize> {
        let cur = self.current.as_ref()?;
        let mut idx = 0;
        for h in &self.hits {
            if h.index == cur.block && h.pointer == cur.pointer {
                return h.ranges.iter().position(|r| *r == cur.range).map(|p| idx + p + 1);
            }
            idx += h.ranges.len();
        }
        None
    }

    fn nth(&self, n: usize) -> Option<Target> {
        let mut idx = 0;
        for h in &self.hits {
            if n < idx + h.ranges.len() {
                return Some(Target { block: h.index, pointer: h.pointer.clone(), range: h.ranges[n - idx] });
            }
            idx += h.ranges.len();
        }
        None
    }

    fn set_current(&mut self, t: Option<Target>) {
        self.current = t;
        self.scroll_pending = self.current.is_some();
        self.select_pending = self.current.is_some();
    }

    /// Drop `current` when the text under it changed. Call after the hit
    /// list for the frame is complete.
    pub fn validate_current(&mut self) {
        if self.current.is_some() && self.current_index().is_none() {
            self.current = None;
            self.scroll_pending = false;
            self.select_pending = false;
        }
    }

    pub fn resolve_seek(&mut self) {
        let Some(seek) = self.seek.take() else { return };
        let total = self.total();
        if total == 0 {
            self.set_current(None);
            return;
        }
        let target = match seek {
            Seek::Forward { block, pos } => {
                let mut found = None;
                'outer: for h in &self.hits {
                    if h.index < block {
                        continue;
                    }
                    for r in &h.ranges {
                        if h.index > block || r.0 >= pos {
                            found = Some(Target { block: h.index, pointer: h.pointer.clone(), range: *r });
                            break 'outer;
                        }
                    }
                }
                found.or_else(|| self.nth(0))
            }
            Seek::Last => self.nth(total - 1),
        };
        self.set_current(target);
    }

    /// Move to the next / previous match relative to `current`; `start` is
    /// used when there is no current match (e.g. the editor's cursor).
    pub fn step(&mut self, forward: bool, start: Seek) {
        let total = self.total();
        if total == 0 {
            self.set_current(None);
            return;
        }
        match self.current_index() {
            Some(i) => {
                let i = i - 1;
                let n = if forward { (i + 1) % total } else { (i + total - 1) % total };
                let t = self.nth(n);
                self.set_current(t);
            }
            None => {
                self.seek = Some(if forward { start } else { Seek::Last });
                self.resolve_seek();
            }
        }
    }

    pub fn options_label(&self) -> String {
        let mut opts = Vec::new();
        if !self.case_sensitive {
            opts.push("Case Insensitive");
        }
        if self.use_regex {
            opts.push("Regex");
        }
        if self.whole_word {
            opts.push("Whole Word");
        }
        if opts.is_empty() {
            "Finding with Options: None".to_string()
        } else {
            format!("Finding with Options: {}", opts.join(", "))
        }
    }

    fn counter(&self) -> (String, Counter) {
        if self.error.is_some() {
            return (String::new(), Counter::Neutral);
        }
        if self.query.is_empty() {
            return (String::new(), Counter::Neutral);
        }
        let total = self.total();
        if total == 0 {
            return ("No results".to_string(), Counter::None);
        }
        let capped = if total >= MAX_HITS { "+" } else { "" };
        match self.current_index() {
            Some(i) => (format!("{} of {}{}", i, total, capped), Counter::Some),
            None => (format!("{}{} found", total, capped), Counter::Some),
        }
    }
}

#[derive(Clone, Copy)]
enum Counter {
    Neutral,
    None,
    Some,
}

fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

// ----- highlighting helpers shared by both views --------------------------------

pub fn highlight_colors(dark: bool) -> (Color32, Color32) {
    if dark {
        (Color32::from_rgb(110, 90, 20), Color32::from_rgb(190, 110, 20))
    } else {
        (Color32::from_rgb(255, 235, 130), Color32::from_rgb(255, 170, 70))
    }
}

/// A byte range to underline, and the color to do it in.
pub type Underline = (Range, Color32);

/// Monospace layout of `text` with match backgrounds and spelling underlines.
/// Each list is sorted by position; ranges that overlap an earlier one or no
/// longer fit the text (it can change mid-frame while typing) are skipped.
pub fn layout_job(
    ui: &egui::Ui,
    text: &str,
    wrap_width: f32,
    ranges: &[Range],
    current: Option<Range>,
    underlines: &[Underline],
) -> LayoutJob {
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let color = ui
        .style()
        .visuals
        .override_text_color
        .unwrap_or_else(|| ui.visuals().widgets.inactive.text_color());
    let (bg_all, bg_cur) = highlight_colors(ui.visuals().dark_mode);

    let fits = |(s, e): Range| s < e && e <= text.len() && text.is_char_boundary(s) && text.is_char_boundary(e);
    let mut last = 0;
    let bgs: Vec<Range> = ranges.iter().copied().filter(|&r| fits(r) && r.0 >= std::mem::replace(&mut last, r.1)).collect();
    last = 0;
    let uls: Vec<Underline> =
        underlines.iter().copied().filter(|&(r, _)| fits(r) && r.0 >= std::mem::replace(&mut last, r.1)).collect();

    let mut cuts: Vec<usize> = vec![0, text.len()];
    cuts.extend(bgs.iter().flat_map(|&(s, e)| [s, e]));
    cuts.extend(uls.iter().flat_map(|&((s, e), _)| [s, e]));
    cuts.sort_unstable();
    cuts.dedup();

    let mut job = LayoutJob { text: text.to_owned(), ..Default::default() };
    job.wrap.max_width = wrap_width;
    let (mut bi, mut ui_) = (0, 0);
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        while bi < bgs.len() && bgs[bi].1 <= a {
            bi += 1;
        }
        let background = match bgs.get(bi) {
            Some(&(s, e)) if s <= a && b <= e => {
                if current == Some((s, e)) {
                    bg_cur
                } else {
                    bg_all
                }
            }
            _ => Color32::TRANSPARENT,
        };
        while ui_ < uls.len() && uls[ui_].0 .1 <= a {
            ui_ += 1;
        }
        let underline = match uls.get(ui_) {
            Some(&((s, e), c)) if s <= a && b <= e => Stroke::new(1.5, c),
            _ => Stroke::NONE,
        };
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(a)..ByteIndex(b),
            format: TextFormat { font_id: font_id.clone(), color, background, underline, ..Default::default() },
        });
    }
    job
}

fn char_index(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// Put the editor's cursor on `range` (shown as a selection once it has focus).
pub fn select_range(ctx: &egui::Context, id: egui::Id, text: &str, range: Range) {
    if range.1 > text.len() || !text.is_char_boundary(range.0) || !text.is_char_boundary(range.1) {
        return;
    }
    let a = char_index(text, range.0);
    let b = char_index(text, range.1);
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
    state.store(ctx, id);
}

/// Scroll the enclosing scroll areas so `range` of a just-shown TextEdit is visible.
pub fn scroll_to_range(ui: &egui::Ui, out: &egui::text_edit::TextEditOutput, text: &str, range: Range) {
    if range.0 > text.len() || !text.is_char_boundary(range.0) {
        return;
    }
    let c = char_index(text, range.0);
    let rect = out.galley.pos_from_cursor(CCursor::new(c)).translate(out.galley_pos.to_vec2());
    ui.scroll_to_rect(rect.expand2(egui::vec2(0.0, 60.0)), Some(Align::Center));
}

// ----- the panel ------------------------------------------------------------------

const BTN_H: f32 = 22.0;
const BTN_W: f32 = 78.0;
const BTN_ALL_W: f32 = 100.0;
const COUNTER_W: i8 = 96;

/// Draw the panel. Returns the action the user asked for this frame.
pub fn panel(ui: &mut egui::Ui, st: &mut FindState, raw_mode: bool) -> Option<FindAction> {
    let ctx = ui.ctx().clone();
    let find_id = egui::Id::new(FIND_ID);
    let replace_id = egui::Id::new(REPLACE_ID);
    let mut action: Option<FindAction> = None;

    // Enter / Shift+Enter in the fields, consumed before the TextEdits see them
    // so the fields keep focus.
    if ctx.memory(|m| m.has_focus(find_id)) {
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Enter)) {
            action = Some(FindAction::Prev);
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            action = Some(FindAction::Next);
        }
    } else if ctx.memory(|m| m.has_focus(replace_id)) {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
            action = Some(FindAction::ReplaceAll);
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            action = Some(FindAction::Replace);
        }
    }

    let weak = ui.visuals().weak_text_color();
    let (counter_text, counter_kind) = st.counter();
    let counter_color = match counter_kind {
        Counter::Neutral => weak,
        Counter::None => Color32::from_rgb(230, 80, 80),
        Counter::Some => Color32::from_rgb(60, 190, 100),
    };

    ui.spacing_mut().item_spacing = egui::vec2(6.0, 5.0);

    // Header row.
    ui.horizontal(|ui| {
        let title = if raw_mode { "Find in Document (Raw)" } else { "Find in Document (Blocks)" };
        ui.label(RichText::new(title).strong());
        if let Some(err) = &st.error {
            ui.colored_label(Color32::from_rgb(230, 80, 80), format!("Invalid regex: {}", err));
        }
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if ui.add(egui::Button::new("×").small()).on_hover_text("Close (Esc)").clicked() {
                action = Some(FindAction::Close);
            }
            ui.add_space(4.0);
            if ui
                .add(egui::Button::new(RichText::new("ab").underline()).small().selected(st.whole_word))
                .on_hover_text("Only Whole Words (Ctrl+Alt+W)")
                .clicked()
            {
                st.whole_word = !st.whole_word;
            }
            if ui
                .add(egui::Button::new("Aa").small().selected(st.case_sensitive))
                .on_hover_text("Match Case (Ctrl+Alt+C)")
                .clicked()
            {
                st.case_sensitive = !st.case_sensitive;
            }
            if ui
                .add(egui::Button::new(RichText::new(".*").monospace()).small().selected(st.use_regex))
                .on_hover_text("Use Regex (Ctrl+Alt+/)")
                .clicked()
            {
                st.use_regex = !st.use_regex;
            }
            ui.add_space(4.0);
            ui.label(RichText::new(st.options_label()).color(weak).small());
        });
    });

    // Inputs in a left column, buttons in a right column: egui's Tab order
    // follows registration order, so Tab moves find -> replace like Atom.
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let w = (ui.available_width() - BTN_W - BTN_ALL_W - 2.0 * gap).max(80.0);
        ui.vertical(|ui| {
            let resp = ui.add_sized(
                [w, BTN_H],
                egui::TextEdit::singleline(&mut st.query)
                    .id(find_id)
                    .hint_text("Find in document")
                    .margin(egui::Margin { left: 6, right: COUNTER_W, top: 2, bottom: 2 }),
            );
            if !counter_text.is_empty() {
                ui.painter().text(
                    resp.rect.right_center() - egui::vec2(8.0, 0.0),
                    Align2::RIGHT_CENTER,
                    counter_text,
                    FontId::proportional(12.0),
                    counter_color,
                );
            }
            ui.add_sized(
                [w, BTN_H],
                egui::TextEdit::singleline(&mut st.replacement)
                    .id(replace_id)
                    .hint_text(if st.use_regex { "Replace in document ($1 for groups)" } else { "Replace in document" })
                    .margin(egui::Margin { left: 6, right: 6, top: 2, bottom: 2 }),
            );
        });
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if ui.add_sized([BTN_W, BTN_H], egui::Button::new("Find")).on_hover_text("Find next (Enter)").clicked() {
                    action = Some(FindAction::Next);
                }
                let find_all = ui
                    .add_sized([BTN_ALL_W, BTN_H], egui::Button::new("Find All").selected(st.filter && !raw_mode))
                    .on_hover_text(if raw_mode {
                        "In Blocks mode: show only the blocks that contain a match"
                    } else {
                        "Show only the blocks that contain a match (click again to show all)"
                    });
                if find_all.clicked() {
                    action = Some(FindAction::FindAll);
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add_sized([BTN_W, BTN_H], egui::Button::new("Replace"))
                    .on_hover_text("Replace the current match and move to the next (Enter)")
                    .clicked()
                {
                    action = Some(FindAction::Replace);
                }
                if ui
                    .add_sized([BTN_ALL_W, BTN_H], egui::Button::new("Replace All"))
                    .on_hover_text("Replace every match (Ctrl+Enter)")
                    .clicked()
                {
                    action = Some(FindAction::ReplaceAll);
                }
            });
        });
    });

    if std::mem::take(&mut st.focus_find) {
        ctx.memory_mut(|m| m.request_focus(find_id));
        let n = st.query.chars().count();
        let mut state = egui::TextEdit::load_state(&ctx, find_id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
        state.store(&ctx, find_id);
    }
    if std::mem::take(&mut st.focus_replace) {
        ctx.memory_mut(|m| m.request_focus(replace_id));
        let n = st.replacement.chars().count();
        let mut state = egui::TextEdit::load_state(&ctx, replace_id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
        state.store(&ctx, replace_id);
    }

    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_search_is_literal_and_case_insensitive() {
        let m = Matcher::compile("a.b", false, false, false).unwrap();
        assert_eq!(m.find_all("A.B axb a.b"), vec![(0, 3), (8, 11)]);
    }

    #[test]
    fn whole_word_and_case() {
        let m = Matcher::compile("cat", false, true, true).unwrap();
        assert_eq!(m.find_all("cat concat Cat cat."), vec![(0, 3), (15, 18)]);
    }

    #[test]
    fn replace_one_literal_and_regex() {
        let m = Matcher::compile("x", false, false, false).unwrap();
        let mut t = "axbxc".to_string();
        assert_eq!(m.replace_one(&mut t, (1, 2), "$0$0"), Some(5));
        assert_eq!(t, "a$0$0bxc");

        let m = Matcher::compile("(\\d+)", true, false, false).unwrap();
        let mut t = "v=12;".to_string();
        assert_eq!(m.replace_one(&mut t, (2, 4), "[$1]"), Some(6));
        assert_eq!(t, "v=[12];");
        // Stale range: text no longer matches there.
        assert_eq!(m.replace_one(&mut t, (2, 4), "z"), None);
    }

    #[test]
    fn replace_all_counts() {
        let m = Matcher::compile("o", false, false, false).unwrap();
        assert_eq!(m.replace_all("foo boo", "0"), ("f00 b00".to_string(), 4));
    }

    #[test]
    fn stepping_wraps_and_seeks() {
        let mut st = FindState::default();
        st.hits = vec![
            BlockHit { index: 0, pointer: "/a".into(), ranges: vec![(0, 1), (5, 6)] },
            BlockHit { index: 3, pointer: "/b".into(), ranges: vec![(2, 3)] },
        ];
        st.step(true, Seek::Forward { block: 0, pos: 0 });
        assert_eq!(st.current.as_ref().unwrap().range, (0, 1));
        st.step(false, Seek::Forward { block: 0, pos: 0 });
        assert_eq!(st.current.as_ref().unwrap().pointer, "/b");
        st.step(true, Seek::Forward { block: 0, pos: 0 });
        assert_eq!(st.current.as_ref().unwrap().range, (0, 1));

        st.current = None;
        st.seek = Some(Seek::Forward { block: 0, pos: 3 });
        st.resolve_seek();
        assert_eq!(st.current.as_ref().unwrap().range, (5, 6));
        assert_eq!(st.current_index(), Some(2));
    }

    #[test]
    fn invalid_regex_reports_error() {
        let mut st = FindState::default();
        st.query = "(".into();
        st.use_regex = true;
        st.refresh();
        assert!(st.matcher.is_none());
        assert!(st.error.is_some());
    }
}
