//! The editor context menu (spelling fixes, Cut, Copy, Paste, Select All,
//! Transform Text), the text operations behind it, and the pointer-press
//! rules that keep a selection alive through right clicks and multi-clicks.

use eframe::egui;
use egui::text::{CCursor, CCursorRange};
use egui::text_edit::{TextEditOutput, TextEditState};
use egui::{PointerButton, Response};

use crate::spell::{self, Issue, Kind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Case {
    Upper,
    Lower,
    Title,
    Sentence,
}

impl Case {
    pub const ALL: [Case; 4] = [Case::Upper, Case::Lower, Case::Title, Case::Sentence];

    pub fn label(self) -> &'static str {
        match self {
            Case::Upper => "UPPER CASE",
            Case::Lower => "lower case",
            Case::Title => "Title Case",
            Case::Sentence => "Sentence case",
        }
    }

    pub fn apply(self, s: &str) -> String {
        match self {
            Case::Upper => s.to_uppercase(),
            Case::Lower => s.to_lowercase(),
            Case::Title => title_case(s),
            Case::Sentence => sentence_case(s),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TextOp {
    Cut,
    Copy,
    Paste,
    SelectAll,
    Case(Case),
    /// Replace chars `start..end` with `text`: a spelling or grammar fix.
    Replace { start: usize, end: usize, text: String },
    /// Add the word to the personal dictionary.
    AddWord(String),
    /// Stop flagging the word until the app restarts.
    IgnoreWord(String),
}

/// What `apply` did: whether the text changed and what should go to the clipboard.
#[derive(Debug, PartialEq, Eq)]
pub struct Applied {
    pub changed: bool,
    pub copied: Option<String>,
}

/// First letter of every word up, the rest down. An apostrophe inside a word
/// does not start a new one ("don't"), a hyphen does ("well-known").
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut word_start = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if word_start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            word_start = false;
        } else {
            word_start = !matches!(c, '\'' | '\u{2019}');
            out.push(c);
        }
    }
    out
}

/// Everything down, then the first letter of the text, of each line, and of
/// each sentence (after `.`, `!` or `?` followed by whitespace) up.
fn sentence_case(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut capitalize = true;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_alphabetic() {
            if capitalize {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            capitalize = false;
        } else {
            if c.is_numeric() {
                capitalize = false;
            } else if c == '\n' {
                capitalize = true;
            } else if matches!(c, '.' | '!' | '?') {
                let next_is_space = chars.get(i + 1).map_or(true, |n| n.is_whitespace());
                if next_is_space {
                    capitalize = true;
                }
            }
            out.push(c);
        }
    }
    out
}

/// Context menu for a text field, headed by the fixes for `issue` when the
/// menu was opened over a flagged word. Returns the item clicked.
pub fn context_menu(ui: &mut egui::Ui, issue: Option<&Issue>) -> Option<TextOp> {
    let mut op = None;
    ui.set_min_width(190.0);
    if let Some(issue) = issue {
        ui.set_max_width(280.0);
        for fix in &issue.fixes {
            if ui.button(&fix.label).clicked() {
                op = Some(TextOp::Replace { start: issue.start, end: issue.end, text: fix.replacement.clone() });
            }
        }
        if issue.fixes.is_empty() {
            ui.add_enabled(false, egui::Button::new("No suggestions"));
        }
        match issue.kind {
            Kind::Spelling => {
                if ui.button("Add to Dictionary").clicked() {
                    op = Some(TextOp::AddWord(issue.text.clone()));
                }
                if ui.button("Ignore Word").clicked() {
                    op = Some(TextOp::IgnoreWord(issue.text.clone()));
                }
            }
            Kind::Grammar => {
                ui.add(egui::Label::new(egui::RichText::new(&issue.message).weak().small()).wrap());
            }
        }
        ui.separator();
    }
    if ui.add(egui::Button::new("Cut").shortcut_text("Ctrl+X")).clicked() {
        op = Some(TextOp::Cut);
    }
    if ui.add(egui::Button::new("Copy").shortcut_text("Ctrl+C")).clicked() {
        op = Some(TextOp::Copy);
    }
    if ui.add(egui::Button::new("Paste").shortcut_text("Ctrl+V")).clicked() {
        op = Some(TextOp::Paste);
    }
    ui.separator();
    if ui.add(egui::Button::new("Select All").shortcut_text("Ctrl+A")).clicked() {
        op = Some(TextOp::SelectAll);
    }
    ui.separator();
    ui.menu_button("Transform Text", |ui| {
        ui.set_min_width(170.0);
        for case in Case::ALL {
            if ui.button(case.label()).clicked() {
                op = Some(TextOp::Case(case));
            }
        }
    });
    op
}

/// A text field drawn last frame, so the input hook can tell what a press hit.
#[derive(Clone, Copy)]
struct EditorInfo {
    id: egui::Id,
    rect: egui::Rect,
    layer: egui::LayerId,
    chars: usize,
}

#[derive(Clone, Copy, Default)]
struct Clicks {
    time: f64,
    pos: egui::Pos2,
    count: u32,
}

/// Presses the hook swallowed whose release must be swallowed too.
#[derive(Clone, Copy, Default)]
struct Pending {
    menu: Option<egui::Id>,
    primary: bool,
}

fn registry_key() -> egui::Id {
    egui::Id::new("text_ops_editors")
}

fn open_menu_key(id: egui::Id) -> egui::Id {
    id.with("open_menu")
}

fn word_select_key(id: egui::Id) -> egui::Id {
    id.with("select_word_at")
}

fn menu_issue_key(id: egui::Id) -> egui::Id {
    id.with("menu_issue")
}

/// Char index under screen position `pos` in a just-shown field.
pub fn char_at_pos(out: &TextEditOutput, pos: egui::Pos2) -> usize {
    let local = pos - out.galley_pos + egui::vec2(out.galley.rect.left(), 0.0);
    out.galley.cursor_from_pos(local).index.0
}

/// The issue under `pos`: at the char there, else the one before it (the
/// right half of a glyph resolves to the cursor after it).
pub fn issue_at_pos<'a>(out: &TextEditOutput, text: &str, issues: &'a [Issue], pos: egui::Pos2) -> Option<&'a Issue> {
    if issues.is_empty() {
        return None;
    }
    let ci = char_at_pos(out, pos);
    spell::issue_at(text, issues, ci).or_else(|| ci.checked_sub(1).and_then(|c| spell::issue_at(text, issues, c)))
}

/// Draw the context menu of field `id` after its `TextEdit::show`. It opens
/// when `filter_input` saw a right click released over the field, and a
/// primary click on the field closes it. `issues` are the field's spelling
/// results; the one under the right click heads the menu. Returns the item
/// picked.
pub fn show_menu(ctx: &egui::Context, id: egui::Id, out: &TextEditOutput, text: &str, issues: &[Issue]) -> Option<TextOp> {
    let response = &out.response.response;
    let open_at = ctx.data_mut(|d| d.remove_temp::<egui::Pos2>(open_menu_key(id)));
    let set = if let Some(pos) = open_at {
        let issue = issue_at_pos(out, text, issues, pos).cloned();
        ctx.data_mut(|d| d.insert_temp(menu_issue_key(id), issue));
        Some(egui::SetOpenCommand::Bool(true))
    } else if response.clicked() {
        Some(egui::SetOpenCommand::Bool(false))
    } else {
        None
    };
    let issue: Option<Issue> = ctx.data(|d| d.get_temp::<Option<Issue>>(menu_issue_key(id))).flatten();
    let mut picked = None;
    egui::Popup::menu(response).at_pointer_fixed().open_memory(set).show(|ui| picked = context_menu(ui, issue.as_ref()));
    picked
}

/// Call once per frame before any editor is drawn.
pub fn clear(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Vec<EditorInfo>>(registry_key()));
}

/// Call after each `TextEdit::show`: registers the field for `filter_input`
/// and applies the word selection of a double click it swallowed.
pub fn after_show(ctx: &egui::Context, id: egui::Id, out: &egui::text_edit::TextEditOutput, text: &str) {
    register(ctx, id, &out.response.response, text);
    let Some(pos) = ctx.data_mut(|d| d.remove_temp::<egui::Pos2>(word_select_key(id))) else { return };
    let at = char_at_pos(out, pos);
    let chars: Vec<char> = text.chars().collect();
    let (a, b) = word_at(&chars, at);
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
        state.store(ctx, id);
    }
    ctx.memory_mut(|m| m.request_focus(id));
    ctx.request_repaint();
}

/// The word around char index `at`; a lone non-word char selects itself.
fn word_at(chars: &[char], at: usize) -> (usize, usize) {
    let n = chars.len();
    let at = at.min(n);
    let anchor = if at < n && is_word_char(chars[at]) {
        at
    } else if at > 0 && is_word_char(chars[at - 1]) {
        at - 1
    } else {
        return (at, (at + 1).min(n));
    };
    let (mut a, mut b) = (anchor, anchor + 1);
    while a > 0 && is_word_char(chars[a - 1]) {
        a -= 1;
    }
    while b < n && is_word_char(chars[b]) {
        b += 1;
    }
    (a, b)
}

fn register(ctx: &egui::Context, id: egui::Id, response: &Response, text: &str) {
    let info = EditorInfo { id, rect: response.rect, layer: response.layer_id, chars: text.chars().count() };
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<EditorInfo>>(registry_key()).push(info));
}

fn editor_at(ctx: &egui::Context, pos: egui::Pos2) -> Option<EditorInfo> {
    let layer = ctx.layer_id_at(pos)?;
    ctx.data(|d| {
        d.get_temp::<Vec<EditorInfo>>(registry_key())
            .and_then(|v| v.iter().copied().find(|e| e.layer == layer && e.rect.contains(pos)))
    })
}

fn select_all(ctx: &egui::Context, editor: EditorInfo) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, editor.id) {
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(editor.chars))));
        state.store(ctx, editor.id);
    }
}

/// The time, in seconds, within which two clicks count as a double click.
/// egui defaults to 0.3 s; Windows users set theirs in the mouse settings
/// (0.5 s by default), so that is what their fingers are trained on. Set it
/// on the egui options at startup; `filter_input` reads it from there.
pub fn system_double_click_delay() -> f64 {
    #[cfg(windows)]
    {
        // SAFETY: plain FFI call with no arguments; returns milliseconds.
        let ms = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
        if ms > 0 {
            return f64::from(ms) / 1000.0;
        }
    }
    0.3
}

/// eframe's `raw_input_hook`: runs before egui sees the frame's input.
///
/// egui moves the caret on every pointer press and offers no way to opt out,
/// so the presses that must leave a selection alone never reach it:
/// - a right click over a text field (the context menu opens on the release
///   instead, where egui would have opened it anyway);
/// - the second and later clicks of a multi-click: two selects the word,
///   three the whole field, more do nothing. Only the first click, the one
///   that places the caret, reaches egui; its own double/triple detection
///   never fires for these fields, which keeps its line-select quirk out.
pub fn filter_input(ctx: &egui::Context, raw: &mut egui::RawInput) {
    let time = raw.time.unwrap_or_else(|| ctx.input(|i| i.time));
    let (delay, dist) = ctx.options(|o| (o.input_options.max_double_click_delay, o.input_options.max_click_dist));
    let pending_key = egui::Id::new("text_ops_pending");
    let clicks_key = egui::Id::new("text_ops_clicks");
    let mut pending: Pending = ctx.data(|d| d.get_temp(pending_key)).unwrap_or_default();

    raw.events.retain(|event| {
        let egui::Event::PointerButton { pos, button, pressed, .. } = event else { return true };
        match (button, pressed) {
            (PointerButton::Secondary, true) => match editor_at(ctx, *pos) {
                Some(e) => {
                    pending.menu = Some(e.id);
                    false
                }
                None => true,
            },
            (PointerButton::Secondary, false) => match pending.menu.take() {
                Some(id) => {
                    if editor_at(ctx, *pos).is_some_and(|e| e.id == id) {
                        ctx.data_mut(|d| d.insert_temp(open_menu_key(id), *pos));
                    }
                    false
                }
                None => true,
            },
            (PointerButton::Primary, true) => {
                let Some(e) = editor_at(ctx, *pos) else { return true };
                let last: Clicks = ctx.data(|d| d.get_temp(clicks_key)).unwrap_or_default();
                let count = if time - last.time < delay && last.pos.distance(*pos) < dist { last.count + 1 } else { 1 };
                ctx.data_mut(|d| d.insert_temp(clicks_key, Clicks { time, pos: *pos, count }));
                match count {
                    1 => return true,
                    2 => {
                        ctx.data_mut(|d| d.insert_temp(word_select_key(e.id), *pos));
                    }
                    3 => select_all(ctx, e),
                    _ => {}
                }
                pending.primary = true;
                false
            }
            (PointerButton::Primary, false) => {
                if pending.primary {
                    pending.primary = false;
                    false
                } else {
                    true
                }
            }
            _ => true,
        }
    });
    ctx.data_mut(|d| d.insert_temp(pending_key, pending));
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Run `op` on `text`. The selection comes from `state`; Cut, Copy and the
/// case changes fall back to the word under the caret when it is empty.
/// `clipboard` is the clipboard text, needed for Paste only.
pub fn apply(op: TextOp, text: &mut String, state: &mut TextEditState, clipboard: Option<&str>) -> Result<Applied, &'static str> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let (mut a, mut b) = state
        .cursor
        .char_range()
        .map(|r| (r.primary.index.0.min(r.secondary.index.0), r.primary.index.0.max(r.secondary.index.0)))
        .unwrap_or((0, 0));
    (a, b) = (a.min(n), b.min(n));
    let byte_at = |ci: usize| text.char_indices().nth(ci).map(|(i, _)| i).unwrap_or(text.len());
    let unchanged = Applied { changed: false, copied: None };

    if op == TextOp::SelectAll {
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
        return Ok(unchanged);
    }
    if op == TextOp::Paste {
        let Some(clip) = clipboard.filter(|c| !c.is_empty()) else { return Err("Clipboard has no text") };
        let (ba, bb) = (byte_at(a), byte_at(b));
        text.replace_range(ba..bb, clip);
        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(a + clip.chars().count()))));
        return Ok(Applied { changed: true, copied: None });
    }
    if let TextOp::Replace { start, end, text: with } = &op {
        if start > end || *end > n {
            return Err("The text changed; check it again");
        }
        let (ba, bb) = (byte_at(*start), byte_at(*end));
        text.replace_range(ba..bb, with);
        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(start + with.chars().count()))));
        return Ok(Applied { changed: true, copied: None });
    }
    if matches!(op, TextOp::AddWord(_) | TextOp::IgnoreWord(_)) {
        // Dictionary changes are the app's business; the text stays.
        return Ok(unchanged);
    }

    if a == b {
        while a > 0 && is_word_char(chars[a - 1]) {
            a -= 1;
        }
        while b < n && is_word_char(chars[b]) {
            b += 1;
        }
    }
    if a == b {
        return Err("Select some text first");
    }
    let selected: String = chars[a..b].iter().collect();
    let (ba, bb) = (byte_at(a), byte_at(b));

    match op {
        TextOp::Copy => {
            state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
            Ok(Applied { changed: false, copied: Some(selected) })
        }
        TextOp::Cut => {
            text.replace_range(ba..bb, "");
            state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(a))));
            Ok(Applied { changed: true, copied: Some(selected) })
        }
        TextOp::Case(case) => {
            let replaced = case.apply(&selected);
            let changed = replaced != selected;
            if changed {
                text.replace_range(ba..bb, &replaced);
            }
            // Done with it: leave the caret after the result, nothing highlighted.
            state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(a + replaced.chars().count()))));
            Ok(Applied { changed, copied: None })
        }
        TextOp::SelectAll | TextOp::Paste | TextOp::Replace { .. } | TextOp::AddWord(_) | TextOp::IgnoreWord(_) => {
            unreachable!("handled above")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(a: usize, b: usize) -> TextEditState {
        let mut st = TextEditState::default();
        st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
        st
    }

    fn caret(st: &TextEditState) -> (usize, usize) {
        let r = st.cursor.char_range().unwrap();
        (r.primary.index.0.min(r.secondary.index.0), r.primary.index.0.max(r.secondary.index.0))
    }

    #[test]
    fn title_case_words() {
        assert_eq!(title_case("hello wORLD, don't stop the well-known 3d show"), "Hello World, Don't Stop The Well-Known 3d Show");
    }

    #[test]
    fn sentence_case_sentences() {
        assert_eq!(sentence_case("hello WORLD. this is IT! ok? yes"), "Hello world. This is it! Ok? Yes");
        // A period followed by whitespace ends a sentence; abbreviations are not special-cased.
        assert_eq!(sentence_case("3.14 is pi. e.g. this"), "3.14 is pi. E.g. This");
        assert_eq!(sentence_case("one\ntwo"), "One\nTwo");
    }

    #[test]
    fn case_change_uses_selection_or_word_and_leaves_a_caret() {
        let mut text = String::from("alpha beta gamma");
        let mut st = state_with(6, 10);
        assert_eq!(apply(TextOp::Case(Case::Upper), &mut text, &mut st, None), Ok(Applied { changed: true, copied: None }));
        assert_eq!(text, "alpha BETA gamma");
        assert_eq!(caret(&st), (10, 10));

        // Caret inside "gamma", nothing selected: the word is used.
        st = state_with(13, 13);
        assert_eq!(apply(TextOp::Case(Case::Title), &mut text, &mut st, None), Ok(Applied { changed: true, copied: None }));
        assert_eq!(text, "alpha BETA Gamma");
        assert_eq!(caret(&st), (16, 16));

        // Caret between two spaces: nothing to do.
        let mut spaced = String::from("a  b");
        st = state_with(2, 2);
        assert_eq!(apply(TextOp::Case(Case::Upper), &mut spaced, &mut st, None), Err("Select some text first"));
    }

    #[test]
    fn word_at_picks_the_word_under_or_before_the_index() {
        let c: Vec<char> = "foo_bar, baz".chars().collect();
        assert_eq!(word_at(&c, 0), (0, 7));
        assert_eq!(word_at(&c, 7), (0, 7)); // just past the word
        assert_eq!(word_at(&c, 8), (8, 9)); // the space
        assert_eq!(word_at(&c, 10), (9, 12));
        assert_eq!(word_at(&c, 99), (9, 12));
        assert_eq!(word_at(&[], 0), (0, 0));
    }

    #[test]
    fn cut_copy_paste_select_all() {
        let mut text = String::from("alpha beta gamma");
        let mut st = state_with(6, 10);
        assert_eq!(apply(TextOp::Copy, &mut text, &mut st, None), Ok(Applied { changed: false, copied: Some("beta".into()) }));
        assert_eq!(text, "alpha beta gamma");
        assert_eq!(caret(&st), (6, 10));

        assert_eq!(apply(TextOp::Cut, &mut text, &mut st, None), Ok(Applied { changed: true, copied: Some("beta".into()) }));
        assert_eq!(text, "alpha  gamma");
        assert_eq!(caret(&st), (6, 6));

        assert_eq!(apply(TextOp::Paste, &mut text, &mut st, Some("BETA")), Ok(Applied { changed: true, copied: None }));
        assert_eq!(text, "alpha BETA gamma");
        assert_eq!(caret(&st), (10, 10));
        assert_eq!(apply(TextOp::Paste, &mut text, &mut st, None), Err("Clipboard has no text"));

        // Paste over a selection replaces it; non-ASCII counts in chars.
        st = state_with(0, 5);
        assert_eq!(apply(TextOp::Paste, &mut text, &mut st, Some("ñ")).map(|a| a.changed), Ok(true));
        assert_eq!(text, "ñ BETA gamma");
        assert_eq!(caret(&st), (1, 1));

        assert_eq!(apply(TextOp::SelectAll, &mut text, &mut st, None), Ok(Applied { changed: false, copied: None }));
        assert_eq!(caret(&st), (0, 12));
    }
}
