//! User preferences, persisted as `%APPDATA%\json-forge\settings.json`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Folder holding every file the app writes for the user: recent files,
/// settings, the personal dictionary.
pub fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("json-forge")
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// Underline misspellings and grammar slips in string values.
    pub spell_check: bool,
    /// Path of the `spellchecker` executable; empty means next to the app.
    pub spell_checker: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { spell_check: true, spell_checker: String::new() }
    }
}

impl Settings {
    fn store() -> PathBuf {
        data_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::store())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::store();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}
