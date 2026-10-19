//! Recently opened files, persisted as a small JSON array in the user's
//! roaming app-data folder (`%APPDATA%\json-forge\recent.json`).

use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 10;

pub struct RecentFiles {
    paths: Vec<PathBuf>,
    store: PathBuf,
}

fn store_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("json-forge").join("recent.json")
}

fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

impl RecentFiles {
    pub fn load() -> Self {
        let store = store_path();
        let paths = std::fs::read_to_string(&store)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .map(|v| v.into_iter().map(PathBuf::from).collect())
            .unwrap_or_default();
        Self { paths, store }
    }

    pub fn iter(&self) -> impl Iterator<Item = &PathBuf> {
        self.paths.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Move `path` to the front of the list.
    pub fn push(&mut self, path: &Path) {
        let path = std::fs::canonicalize(path)
            .map(strip_verbatim)
            .unwrap_or_else(|_| path.to_path_buf());
        self.paths.retain(|p| !same_path(p, &path));
        self.paths.insert(0, path);
        self.paths.truncate(MAX_RECENT);
        self.save();
    }

    pub fn remove(&mut self, path: &Path) {
        self.paths.retain(|p| !same_path(p, path));
        self.save();
    }

    pub fn clear(&mut self) {
        self.paths.clear();
        self.save();
    }

    fn save(&self) {
        if let Some(dir) = self.store.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let list: Vec<String> = self.paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
        if let Ok(json) = serde_json::to_string_pretty(&list) {
            let _ = std::fs::write(&self.store, json);
        }
    }
}

/// `canonicalize` on Windows yields `\\?\C:\...`; strip that prefix for display.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}
