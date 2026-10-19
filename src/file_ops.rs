use std::path::{Path, PathBuf};

pub fn open_file() -> Option<(String, PathBuf)> {
    let path = rfd::FileDialog::new()
        .add_filter("JSON", &["json", "jsonl"])
        .add_filter("All files", &["*"])
        .pick_file()?;

    let text = std::fs::read_to_string(&path).ok()?;
    Some((text, path))
}

pub fn save_file(path: &Path, content: &str) {
    let _ = std::fs::write(path, content);
}

pub fn save_file_as(content: &str) -> Option<PathBuf> {
    let path = rfd::FileDialog::new()
        .add_filter("JSON", &["json"])
        .set_file_name("untitled.json")
        .save_file()?;

    let _ = std::fs::write(&path, content);
    Some(path)
}
