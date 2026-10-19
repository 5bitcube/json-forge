//! Shared data model: edit descriptors, per-block buffers, cached document stats
//! and small formatting helpers used by every view.

use serde_json::Value;

/// How a block's text maps onto the JSON value it edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    /// Buffer is the raw string content of a `Value::String`.
    Text,
    /// Buffer is a JSON scalar literal (number, bool, null, or quoted string).
    Scalar,
    /// Buffer is a JSON document (object/array, or any value) that must parse.
    Json,
}

impl EditKind {
    pub fn natural(val: &Value) -> Self {
        match val {
            Value::String(_) => EditKind::Text,
            Value::Number(_) | Value::Bool(_) | Value::Null => EditKind::Scalar,
            _ => EditKind::Json,
        }
    }
}

/// Scratch text for one block, keyed by JSON pointer in the app.
pub struct BlockBuffer {
    pub text: String,
    pub error: Option<String>,
}

/// An edit produced by the block view, applied by the app after the UI pass.
pub struct PendingEdit {
    pub pointer: String,
    pub kind: EditKind,
    pub text: String,
}

/// Outcome of a successful edit application.
pub struct Applied {
    /// True when the value changed type (e.g. number -> string), which means
    /// the block's buffer should be re-seeded from the new value.
    pub kind_changed: bool,
}

/// Write `edit` into `root` at its pointer. Returns an error string for invalid input.
pub fn apply_edit(root: &mut Value, edit: &PendingEdit) -> Result<Applied, String> {
    let slot = root
        .pointer_mut(&edit.pointer)
        .ok_or_else(|| format!("path no longer exists: {}", edit.pointer))?;

    let new_val = match edit.kind {
        EditKind::Text => Value::String(edit.text.clone()),
        EditKind::Scalar => {
            let v: Value = serde_json::from_str(edit.text.trim())
                .map_err(|e| format!("not a JSON scalar: {}", e))?;
            if v.is_object() || v.is_array() {
                return Err("expected a scalar, got an object/array".to_string());
            }
            v
        }
        EditKind::Json => serde_json::from_str(&edit.text)
            .map_err(|e| format!("invalid JSON (line {}, col {}): {}", e.line(), e.column(), e))?,
    };

    let before = EditKind::natural(slot);
    let after = EditKind::natural(&new_val);
    *slot = new_val;
    Ok(Applied {
        kind_changed: before != after && edit.kind != EditKind::Json,
    })
}

/// Document-level statistics, computed once per parse/sync instead of per frame.
#[derive(Default, Clone)]
pub struct DocStats {
    pub total_bytes: usize,
    pub key_count: usize,
    pub token_estimate: usize,
    pub model: Option<String>,
    /// (chars, blocks)
    pub system: Option<(usize, usize)>,
    /// (count, chars)
    pub tools: Option<(usize, usize)>,
    /// (count, chars)
    pub messages: Option<(usize, usize)>,
    pub stream: Option<String>,
    pub stop_reason: Option<String>,
    pub usage: Option<String>,
}

impl DocStats {
    pub fn compute(val: &Value) -> Self {
        let total_bytes = serde_json::to_string(val).map(|s| s.len()).unwrap_or(0);

        let system = match val.get("system") {
            Some(Value::String(s)) => Some((s.len(), 1)),
            Some(Value::Array(arr)) => {
                let chars = arr
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .map(|s| s.len())
                    .sum();
                Some((chars, arr.len()))
            }
            _ => None,
        };

        let tools = val.get("tools").and_then(|v| v.as_array()).map(|arr| {
            (arr.len(), serde_json::to_string(arr).map(|s| s.len()).unwrap_or(0))
        });

        let messages = val.get("messages").and_then(|v| v.as_array()).map(|arr| {
            (arr.len(), serde_json::to_string(arr).map(|s| s.len()).unwrap_or(0))
        });

        let usage = val.get("usage").and_then(|u| u.as_object()).map(|u| {
            let n = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            let mut s = format!("in {} / out {}", n("input_tokens"), n("output_tokens"));
            let cr = n("cache_read_input_tokens");
            let cw = n("cache_creation_input_tokens");
            if cr > 0 {
                s.push_str(&format!(" / cache read {}", cr));
            }
            if cw > 0 {
                s.push_str(&format!(" / cache write {}", cw));
            }
            s
        });

        DocStats {
            total_bytes,
            key_count: count_keys(val),
            token_estimate: total_bytes / 4,
            model: val.get("model").and_then(|v| v.as_str()).map(String::from),
            system,
            tools,
            messages,
            stream: val.get("stream").map(value_text),
            stop_reason: val.get("stop_reason").and_then(|v| v.as_str()).map(String::from),
            usage,
        }
    }
}

pub fn count_keys(val: &Value) -> usize {
    match val {
        Value::Object(map) => map.len() + map.values().map(count_keys).sum::<usize>(),
        Value::Array(arr) => arr.iter().map(count_keys).sum(),
        _ => 0,
    }
}

/// Plain display text for a value: strings unquoted, containers pretty-printed.
pub fn value_text(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => serde_json::to_string_pretty(val).unwrap_or_default(),
    }
}

pub fn is_leaf(val: &Value) -> bool {
    matches!(val, Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null)
}

pub fn char_size(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1000 {
        format!("{:.1}K", n as f64 / 1000.0)
    } else {
        format!("{}", n)
    }
}

pub fn format_bytes(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{} B", n)
    }
}

/// ASCII case-insensitive substring test without allocating.
pub fn contains_ci(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let n = needle.as_bytes();
    let h = hay.as_bytes();
    if n.len() > h.len() {
        return false;
    }
    h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// Single-line preview of `text`, at most `max` chars.
pub fn preview(text: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= max {
            out.push_str("...");
            break;
        }
        out.push(if ch == '\n' || ch == '\r' || ch == '\t' { ' ' } else { ch });
    }
    out
}

/// Pointer-prefix relation used to resolve tree clicks to blocks:
/// true when one pointer is the other, or a strict ancestor of it.
pub fn pointer_related(block: &str, target: &str) -> bool {
    if block == target {
        return true;
    }
    let under = |a: &str, b: &str| {
        a.len() > b.len() && a.starts_with(b) && a.as_bytes()[b.len()] == b'/'
    };
    under(block, target) || under(target, block)
}

/// Strip the last segment of a JSON pointer. Returns None for the root.
pub fn pointer_parent(ptr: &str) -> Option<String> {
    ptr.rfind('/').map(|i| ptr[..i].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit(ptr: &str, kind: EditKind, text: &str) -> PendingEdit {
        PendingEdit { pointer: ptr.to_string(), kind, text: text.to_string() }
    }

    #[test]
    fn text_edit_writes_string() {
        let mut v = json!({"messages":[{"content":[{"type":"text","text":"old"}]}]});
        let a = apply_edit(&mut v, &edit("/messages/0/content/0/text", EditKind::Text, "new")).unwrap();
        assert!(!a.kind_changed);
        assert_eq!(v["messages"][0]["content"][0]["text"], "new");
    }

    #[test]
    fn scalar_edit_parses_literal_and_rejects_containers() {
        let mut v = json!({"max_tokens": 10, "stream": false});
        apply_edit(&mut v, &edit("/max_tokens", EditKind::Scalar, "4096")).unwrap();
        assert_eq!(v["max_tokens"], 4096);
        apply_edit(&mut v, &edit("/stream", EditKind::Scalar, "true")).unwrap();
        assert_eq!(v["stream"], true);
        assert!(apply_edit(&mut v, &edit("/stream", EditKind::Scalar, "[1]")).is_err());
        assert!(apply_edit(&mut v, &edit("/stream", EditKind::Scalar, "nope")).is_err());
        let a = apply_edit(&mut v, &edit("/max_tokens", EditKind::Scalar, "\"abc\"")).unwrap();
        assert!(a.kind_changed);
        assert_eq!(v["max_tokens"], "abc");
    }

    #[test]
    fn json_edit_replaces_subtree_or_errors() {
        let mut v = json!({"tools":[{"name":"a"}]});
        apply_edit(&mut v, &edit("/tools/0", EditKind::Json, r#"{"name":"b","x":1}"#)).unwrap();
        assert_eq!(v["tools"][0]["x"], 1);
        assert!(apply_edit(&mut v, &edit("/tools/0", EditKind::Json, "{bad")).is_err());
        assert_eq!(v["tools"][0]["name"], "b");
        assert!(apply_edit(&mut v, &edit("/missing/9", EditKind::Text, "x")).is_err());
    }

    #[test]
    fn escaped_keys_round_trip() {
        let mut v = json!({"a/b": {"m~n": 1}});
        apply_edit(&mut v, &edit("/a~1b/m~0n", EditKind::Scalar, "2")).unwrap();
        assert_eq!(v["a/b"]["m~n"], 2);
    }

    #[test]
    fn pointer_helpers() {
        assert!(pointer_related("/messages/0/content/0/text", "/messages/0"));
        assert!(pointer_related("/messages/0", "/messages/0/content/0/text"));
        assert!(pointer_related("/a", "/a"));
        assert!(!pointer_related("/messages/1", "/messages/10"));
        assert!(!pointer_related("/model", "/messages/0/role"));
        assert_eq!(pointer_parent("/messages/0/role").as_deref(), Some("/messages/0"));
        assert_eq!(pointer_parent("/model").as_deref(), Some(""));
        assert_eq!(pointer_parent(""), None);
    }

    #[test]
    fn stats_and_helpers() {
        let v = json!({"model":"m","system":[{"text":"abcd"}],"messages":[{"role":"user","content":"hi"}],
                       "usage":{"input_tokens":5,"output_tokens":7,"cache_read_input_tokens":3}});
        let s = DocStats::compute(&v);
        assert_eq!(s.model.as_deref(), Some("m"));
        assert_eq!(s.system, Some((4, 1)));
        assert_eq!(s.messages.map(|m| m.0), Some(1));
        assert_eq!(s.usage.as_deref(), Some("in 5 / out 7 / cache read 3"));
        assert!(contains_ci("Hello World", "WORLD"));
        assert!(!contains_ci("Hello", "World"));
        assert_eq!(preview("a\nb\tc", 10), "a b c");
        assert_eq!(preview("abcdef", 3), "abc...");
        assert_eq!(char_size(1500), "1.5K");
        assert_eq!(format_bytes(2048), "2.0 KB");
    }
}
