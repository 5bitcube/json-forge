//! Spelling and grammar checking of string values, done by the optional
//! `spellchecker` executable next to the app (see `spellchecker/`).
//!
//! The sidecar is started the first time a block needs checking and talks
//! JSON lines over its stdin and stdout. The views ask `Checker::issues_for`
//! every frame and get whatever the sidecar has answered for that buffer
//! revision. Requests are debounced while the user types, and only the
//! string blocks that are open on screen are ever sent. Results are char
//! spans; `byte_ranges` maps them onto the current text and drops any whose
//! text no longer matches, so stale results never underline the wrong word.
//! Without the sidecar nothing is checked and nothing is said about it.

use eframe::egui;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::settings::data_dir;

#[path = "../spellchecker/src/protocol.rs"]
mod protocol;
pub use protocol::{Issue, Kind};
use protocol::{Reply, Request};

/// Wait this long after the last keystroke before checking a block.
const DEBOUNCE: Duration = Duration::from_millis(350);
/// Strings larger than this are not checked (a lint pass would take seconds).
const MAX_CHECK_BYTES: usize = 400_000;
/// Times the sidecar gets (re)started per run before we stop trying.
const MAX_SPAWNS: u8 = 3;
pub const SIDECAR: &str = "spellchecker";

/// Underline colors: (spelling, grammar).
pub fn underline_colors(dark: bool) -> (egui::Color32, egui::Color32) {
    let c = egui::Color32::from_rgb;
    if dark {
        (c(255, 95, 95), c(110, 160, 255))
    } else {
        (c(220, 40, 40), c(50, 110, 220))
    }
}

/// Where the sidecar lives: `custom` from the settings, else beside the app.
pub fn sidecar_path(custom: Option<&str>) -> PathBuf {
    if let Some(p) = custom.filter(|p| !p.trim().is_empty()) {
        return PathBuf::from(p);
    }
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
    dir.join(format!("{}{}", SIDECAR, std::env::consts::EXE_SUFFIX))
}

/// Byte ranges of the issues that still match `text`, with the issue index.
/// Sorted by position, non-overlapping.
pub fn byte_ranges(text: &str, issues: &[Issue]) -> Vec<((usize, usize), usize)> {
    if issues.is_empty() {
        return Vec::new();
    }
    let mut offsets: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    offsets.push(text.len());
    let mut out = Vec::with_capacity(issues.len());
    let mut last_end = 0;
    for (i, issue) in issues.iter().enumerate() {
        if issue.start >= issue.end || issue.end >= offsets.len() || issue.start < last_end {
            continue;
        }
        let (a, b) = (offsets[issue.start], offsets[issue.end]);
        if text[a..b] == issue.text {
            out.push(((a, b), i));
            last_end = issue.end;
        }
    }
    out
}

/// The issue under char index `at`, if its text still matches.
pub fn issue_at<'a>(text: &str, issues: &'a [Issue], at: usize) -> Option<&'a Issue> {
    let hit = issues.iter().position(|i| i.contains(at))?;
    byte_ranges(text, issues).iter().any(|(_, i)| *i == hit).then(|| &issues[hit])
}

// ----- the sidecar link ---------------------------------------------------------------

struct Link {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    rx: Receiver<Reply>,
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Entry {
    /// Revision the stored issues were computed for; `None` until the first reply.
    rev: Option<u64>,
    issues: Vec<Issue>,
    /// Revision sent to the sidecar and not answered yet.
    requested: Option<u64>,
    /// Revision waiting for the debounce, and since when.
    dirty: Option<(u64, Instant)>,
}

pub struct Checker {
    exe: PathBuf,
    ctx: egui::Context,
    link: Option<Link>,
    spawns_left: u8,
    cache: HashMap<(u64, String), Entry>,
    /// Words the user added, persisted.
    user_words: Vec<String>,
    /// Words ignored for this run only.
    session_words: Vec<String>,
    store: PathBuf,
}

impl Checker {
    /// Nothing is started here; the sidecar comes up on the first request.
    pub fn start(ctx: egui::Context, exe: PathBuf) -> Self {
        let store = data_dir().join("dictionary.txt");
        let user_words = load_words(&store);
        Self {
            exe,
            ctx,
            link: None,
            spawns_left: MAX_SPAWNS,
            cache: HashMap::new(),
            user_words,
            session_words: Vec::new(),
            store,
        }
    }

    /// Make sure the sidecar runs. False when it cannot be started; the
    /// feature is then simply absent.
    fn connect(&mut self) -> bool {
        if self.link.is_some() {
            return true;
        }
        if self.spawns_left == 0 || !self.exe.is_file() {
            return false;
        }
        self.spawns_left -= 1;
        let mut cmd = Command::new(&self.exe);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let Ok(mut child) = cmd.spawn() else { return false };
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return false;
        };
        let (tx, rx) = mpsc::channel::<Reply>();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("spell-replies".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(reply) = serde_json::from_str::<Reply>(&line) {
                        if tx.send(reply).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                }
            })
            .expect("spawn spell-replies thread");
        self.link = Some(Link { child, stdin: BufWriter::new(stdin), rx });
        let words = self.all_words();
        self.send(&Request::Words(words));
        true
    }

    fn send(&mut self, req: &Request) {
        if !self.connect() {
            return;
        }
        let Some(link) = self.link.as_mut() else { return };
        let ok = serde_json::to_writer(&mut link.stdin, req).is_ok()
            && link.stdin.write_all(b"\n").is_ok()
            && link.stdin.flush().is_ok();
        if !ok {
            // The sidecar died; the next request starts a fresh one.
            self.link = None;
        }
    }

    /// Take the sidecar's replies. Call once per frame.
    pub fn poll(&mut self) {
        let Some(link) = self.link.as_mut() else { return };
        loop {
            match link.rx.try_recv() {
                Ok(r) => {
                    let Some(e) = self.cache.get_mut(&(r.doc, r.pointer)) else { continue };
                    if e.requested == Some(r.rev) {
                        e.requested = None;
                    }
                    e.rev = Some(r.rev);
                    e.issues = r.issues;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.link = None;
                    // Unanswered requests go out again once a new sidecar is up.
                    for e in self.cache.values_mut() {
                        e.requested = None;
                    }
                    break;
                }
            }
        }
    }

    /// Issues known for block `pointer` of document `doc`. Asks the sidecar
    /// when `rev` is new; until it answers, the previous issues are returned
    /// (callers validate them against the text).
    pub fn issues_for(&mut self, ctx: &egui::Context, doc: u64, pointer: &str, rev: u64, text: &str) -> &[Issue] {
        let key = (doc, pointer.to_string());
        let e = self.cache.entry(key.clone()).or_insert_with(|| Entry {
            rev: None,
            issues: Vec::new(),
            requested: None,
            dirty: None,
        });
        if e.rev != Some(rev) && e.requested != Some(rev) {
            if text.len() > MAX_CHECK_BYTES {
                e.rev = Some(rev);
                e.issues.clear();
                return &self.cache[&key].issues;
            }
            let now = Instant::now();
            let send = if e.rev.is_none() && e.requested.is_none() {
                true
            } else {
                match e.dirty {
                    Some((r, at)) if r == rev => {
                        let waited = now.duration_since(at);
                        if waited >= DEBOUNCE {
                            true
                        } else {
                            ctx.request_repaint_after(DEBOUNCE - waited);
                            false
                        }
                    }
                    _ => {
                        e.dirty = Some((rev, now));
                        ctx.request_repaint_after(DEBOUNCE);
                        false
                    }
                }
            };
            if send {
                e.dirty = None;
                e.requested = Some(rev);
                let req = Request::Check { doc: key.0, pointer: key.1.clone(), rev, text: text.to_string() };
                self.send(&req);
            }
        }
        &self.cache[&key].issues
    }

    /// Issues known across every checked block of `doc`.
    pub fn issue_count(&self, doc: u64) -> usize {
        self.cache.iter().filter(|(k, _)| k.0 == doc).map(|(_, e)| e.issues.len()).sum()
    }

    /// Drop everything cached for a closed document.
    pub fn forget(&mut self, doc: u64) {
        self.cache.retain(|k, _| k.0 != doc);
    }

    pub fn add_word(&mut self, word: &str) {
        if !self.user_words.iter().any(|w| w == word) {
            self.user_words.push(word.to_string());
            save_words(&self.store, &self.user_words);
        }
        self.push_words();
    }

    pub fn ignore_word(&mut self, word: &str) {
        if !self.session_words.iter().any(|w| w == word) {
            self.session_words.push(word.to_string());
        }
        self.push_words();
    }

    fn all_words(&self) -> Vec<String> {
        let mut all = self.user_words.clone();
        all.extend(self.session_words.iter().cloned());
        all
    }

    fn push_words(&mut self) {
        let words = self.all_words();
        self.send(&Request::Words(words));
        // Every block must be checked again with the new dictionary.
        self.cache.clear();
    }
}

fn load_words(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

fn save_words(path: &PathBuf, words: &[String]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut body = words.join("\n");
    body.push('\n');
    let _ = std::fs::write(path, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(start: usize, end: usize, text: &str) -> Issue {
        Issue { start, end, kind: Kind::Spelling, message: String::new(), text: text.into(), fixes: Vec::new() }
    }

    #[test]
    fn byte_ranges_follow_chars_and_drop_stale_spans() {
        let text = "héllo wrold";
        let issues = vec![issue(0, 5, "nope"), issue(6, 11, "wrold")];
        let ranges = byte_ranges(text, &issues);
        assert_eq!(ranges, vec![((7, 12), 1)]);
        assert_eq!(&text[7..12], "wrold");
    }

    #[test]
    fn issue_at_respects_validation() {
        let text = "a wrold";
        let issues = vec![issue(2, 7, "wrold")];
        assert!(issue_at(text, &issues, 3).is_some());
        assert!(issue_at(text, &issues, 1).is_none());
        assert!(issue_at("a world", &issues, 3).is_none());
    }

    #[test]
    fn missing_sidecar_is_silent() {
        let ctx = egui::Context::default();
        let mut ch = Checker::start(ctx.clone(), PathBuf::from("Z:/nowhere/spellchecker.exe"));
        assert!(ch.issues_for(&ctx, 1, "/a", 1, "a smple test").is_empty());
        ch.poll();
        assert!(ch.link.is_none());
        assert_eq!(ch.issue_count(1), 0);
    }
}
