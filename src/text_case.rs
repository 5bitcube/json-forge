//! Case transforms offered by Edit > Transform Text and the editor context
//! menus. They act on the selection, or on the word under the cursor.

use eframe::egui;
use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;

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

/// The four items, for a menu or a context menu. Returns the one clicked.
pub fn menu(ui: &mut egui::Ui) -> Option<Case> {
    let mut chosen = None;
    for case in Case::ALL {
        if ui.button(case.label()).clicked() {
            chosen = Some(case);
        }
    }
    chosen
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Apply `case` to the selection in `text`, or to the word under the cursor
/// when nothing is selected, and move the selection onto the result.
/// Ok(false) means the text already had that case.
pub fn transform_in(text: &mut String, state: &mut TextEditState, case: Case) -> Result<bool, &'static str> {
    let chars: Vec<char> = text.chars().collect();
    let (mut a, mut b) = state
        .cursor
        .char_range()
        .map(|r| (r.primary.index.0.min(r.secondary.index.0), r.primary.index.0.max(r.secondary.index.0)))
        .unwrap_or((0, 0));
    let (a0, b0) = (a.min(chars.len()), b.min(chars.len()));
    (a, b) = (a0, b0);
    if a == b {
        while a > 0 && is_word_char(chars[a - 1]) {
            a -= 1;
        }
        while b < chars.len() && is_word_char(chars[b]) {
            b += 1;
        }
    }
    if a == b {
        return Err("Select some text first");
    }

    let selected: String = chars[a..b].iter().collect();
    let replaced = case.apply(&selected);
    let new_b = a + replaced.chars().count();
    let changed = replaced != selected;
    if changed {
        let byte_at = |ci: usize| text.char_indices().nth(ci).map(|(i, _)| i).unwrap_or(text.len());
        let (ba, bb) = (byte_at(a), byte_at(b));
        text.replace_range(ba..bb, &replaced);
    }
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(new_b))));
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn transform_uses_selection_or_word() {
        let mut text = String::from("alpha beta gamma");
        let mut st = TextEditState::default();
        st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(6), CCursor::new(10))));
        assert_eq!(transform_in(&mut text, &mut st, Case::Upper), Ok(true));
        assert_eq!(text, "alpha BETA gamma");

        // Caret inside "gamma", nothing selected: the word is used.
        st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(13))));
        assert_eq!(transform_in(&mut text, &mut st, Case::Title), Ok(true));
        assert_eq!(text, "alpha BETA Gamma");
        let r = st.cursor.char_range().unwrap();
        let (lo, hi) = (r.primary.index.0.min(r.secondary.index.0), r.primary.index.0.max(r.secondary.index.0));
        assert_eq!((lo, hi), (11, 16));

        // Caret between two spaces: nothing to do.
        let mut spaced = String::from("a  b");
        st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(2))));
        assert_eq!(transform_in(&mut spaced, &mut st, Case::Upper), Err("Select some text first"));
    }
}
