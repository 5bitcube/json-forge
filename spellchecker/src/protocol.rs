//! Messages between JSON Forge and its spelling sidecar: one JSON object per
//! line on the child's stdin (requests) and stdout (replies). This file is
//! compiled into both executables, so the two can never disagree.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    /// Check the text of block `pointer` of document `doc` at revision `rev`.
    Check { doc: u64, pointer: String, rev: u64, text: String },
    /// Replace the user's word list (added and ignored words).
    Words(Vec<String>),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Reply {
    pub doc: u64,
    pub pointer: String,
    pub rev: u64,
    pub issues: Vec<Issue>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Spelling,
    Grammar,
}

/// One replacement offered for an issue.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub label: String,
    pub replacement: String,
}

/// A problem found in one block. `start..end` are char indices.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Issue {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
    pub message: String,
    /// The text the span covered when the issue was found.
    pub text: String,
    pub fixes: Vec<Fix>,
}

#[allow(dead_code)]
impl Issue {
    pub fn contains(&self, char_index: usize) -> bool {
        self.start <= char_index && char_index < self.end
    }

    /// Short explanation for a tooltip.
    pub fn tooltip(&self) -> String {
        match self.kind {
            Kind::Spelling => format!("Unknown word \u{201c}{}\u{201d}. Right-click for suggestions.", self.text),
            Kind::Grammar => self.message.clone(),
        }
    }
}
