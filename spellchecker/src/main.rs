//! `spellchecker`: the spelling and grammar sidecar, powered by Harper.
//!
//! JSON Forge starts this executable with piped stdin and stdout and sends
//! one `Request` per line; every `Check` is answered with one `Reply` line.
//! The process exits when stdin closes, so it never outlives the app. A
//! reader thread keeps the pipe drained while a lint pass runs, and only the
//! newest revision of each block in the backlog gets linted.

mod protocol;

use harper_core::linting::{Lint, LintGroup, LintKind, Linter, Suggestion};
use harper_core::parsers::Markdown;
use harper_core::spell::{FstDictionary, MergedDictionary, MutableDictionary};
use harper_core::{remove_overlaps, Dialect, DictWordMetadata, Document};
use std::io::{BufRead, BufWriter, Write};
use std::sync::mpsc;
use std::sync::Arc;

use protocol::{Fix, Issue, Kind, Reply, Request};

const MAX_FIXES: usize = 5;
/// Harper rules that are noise on data files. `OrthographicConsistency`
/// wants `HashMap` spelled `hashmap`; `SentenceCapitalization` flags every
/// short lowercase value; `LongSentences` is a style opinion, not an error.
const DISABLED_RULES: &[&str] = &["OrthographicConsistency", "SentenceCapitalization", "LongSentences"];

fn main() {
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Request>(&line) {
                Ok(req) => {
                    if tx.send(req).is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("spellchecker: bad request: {e}"),
            }
        }
    });

    let curated = FstDictionary::curated();
    let mut engine = build_engine(&curated, &[]);
    let parser = Markdown::default();
    let mut out = BufWriter::new(std::io::stdout().lock());

    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while let Ok(r) = rx.try_recv() {
            batch.push(r);
        }
        let mut checks: Vec<(u64, String, u64, String)> = Vec::new();
        for req in batch {
            match req {
                Request::Words(words) => engine = build_engine(&curated, &words),
                Request::Check { doc, pointer, rev, text } => {
                    checks.retain(|c| !(c.0 == doc && c.1 == pointer));
                    checks.push((doc, pointer, rev, text));
                }
            }
        }
        for (doc, pointer, rev, text) in checks {
            let issues = lint_text(&mut engine, &parser, &text);
            let reply = Reply { doc, pointer, rev, issues };
            let ok = serde_json::to_writer(&mut out, &reply).is_ok()
                && out.write_all(b"\n").is_ok()
                && out.flush().is_ok();
            if !ok {
                return;
            }
        }
    }
}

struct Engine {
    dict: Arc<MergedDictionary>,
    linter: LintGroup,
}

fn build_engine(curated: &Arc<FstDictionary>, words: &[String]) -> Engine {
    let mut user = MutableDictionary::new();
    for w in words {
        user.append_word_str(w, DictWordMetadata::default());
    }
    let mut merged = MergedDictionary::new();
    merged.add_dictionary(curated.clone());
    merged.add_dictionary(Arc::new(user));
    let dict = Arc::new(merged);
    let mut linter = LintGroup::new_curated(dict.clone(), Dialect::American);
    for rule in DISABLED_RULES {
        linter.config.set_rule_enabled(*rule, false);
    }
    Engine { dict, linter }
}

/// A string value that is itself a JSON document (tool results, embedded
/// payloads) is data, not prose.
fn looks_like_json(text: &str) -> bool {
    let t = text.trim_start();
    (t.starts_with('{') || t.starts_with('[')) && serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok()
}

fn lint_text(engine: &mut Engine, parser: &Markdown, text: &str) -> Vec<Issue> {
    if text.trim().is_empty() || looks_like_json(text) {
        return Vec::new();
    }
    let doc = Document::new(text, parser, engine.dict.as_ref());
    let mut lints = engine.linter.lint(&doc);
    remove_overlaps(&mut lints);
    lints.sort_by_key(|l| (l.span.start, l.span.end));
    let chars: Vec<char> = text.chars().collect();
    lints.iter().filter_map(|l| to_issue(l, &chars)).collect()
}

fn to_issue(lint: &Lint, chars: &[char]) -> Option<Issue> {
    let (start, end) = (lint.span.start, lint.span.end);
    if start >= end || end > chars.len() {
        return None;
    }
    let text: String = chars[start..end].iter().collect();
    let kind = if matches!(lint.lint_kind, LintKind::Spelling) { Kind::Spelling } else { Kind::Grammar };
    let mut fixes: Vec<Fix> = Vec::new();
    for s in &lint.suggestions {
        let fix = match s {
            Suggestion::ReplaceWith(c) => {
                let r: String = c.iter().collect();
                Fix { label: format!("\u{201c}{}\u{201d}", r), replacement: r }
            }
            Suggestion::InsertAfter(c) => {
                let r: String = chars[start..end].iter().chain(c.iter()).collect();
                Fix { label: format!("\u{201c}{}\u{201d}", r), replacement: r }
            }
            Suggestion::Remove => Fix { label: "Remove".to_string(), replacement: String::new() },
        };
        if fix.replacement != text && !fixes.contains(&fix) {
            fixes.push(fix);
        }
        if fixes.len() == MAX_FIXES {
            break;
        }
    }
    Some(Issue { start, end, kind, message: lint.message.clone(), text, fixes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harper_flags_a_misspelling_with_a_fix() {
        let curated = FstDictionary::curated();
        let mut engine = build_engine(&curated, &["jsonforge".to_string()]);
        let issues = lint_text(&mut engine, &Markdown::default(), "The jsonforge app has a smple bug.");
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].text, "smple");
        assert!(issues[0].fixes.iter().any(|f| f.replacement == "simple"));
    }

    #[test]
    fn json_payloads_are_skipped() {
        let curated = FstDictionary::curated();
        let mut engine = build_engine(&curated, &[]);
        assert!(lint_text(&mut engine, &Markdown::default(), r#"{"nmae": "wrold"}"#).is_empty());
    }

    #[test]
    fn protocol_round_trips() {
        let line = r#"{"check":{"doc":1,"pointer":"/a","rev":7,"text":"hi"}}"#;
        let req: Request = serde_json::from_str(line).unwrap();
        assert!(matches!(req, Request::Check { doc: 1, rev: 7, .. }));
        let reply = Reply { doc: 1, pointer: "/a".into(), rev: 7, issues: Vec::new() };
        let json = serde_json::to_string(&reply).unwrap();
        assert!(!json.contains('\n'));
    }
}
