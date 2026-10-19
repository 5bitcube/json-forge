//! Native dialogs and file IO. Every IO function returns an error string the
//! app can surface in the status bar instead of swallowing failures.

use std::path::{Path, PathBuf};

pub fn pick_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("JSON", &["json", "jsonl"])
        .add_filter("All files", &["*"])
        .pick_file()
}

pub fn pick_save(current: Option<&Path>) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new()
        .add_filter("JSON", &["json"])
        .add_filter("JSON Lines", &["jsonl"])
        .add_filter("All files", &["*"]);
    match current {
        Some(p) => {
            if let Some(dir) = p.parent() {
                dlg = dlg.set_directory(dir);
            }
            if let Some(name) = p.file_name() {
                dlg = dlg.set_file_name(name.to_string_lossy());
            }
        }
        None => dlg = dlg.set_file_name("untitled.json"),
    }
    dlg.save_file()
}

/// Read a file as UTF-8, stripping a leading BOM (serde_json rejects it).
pub fn read(path: &Path) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{} is not valid UTF-8", path.display()))?;
    Ok(match text.strip_prefix('\u{feff}') {
        Some(stripped) => stripped.to_string(),
        None => text,
    })
}

pub fn write(path: &Path, content: &str) -> Result<(), String> {
    std::fs::write(path, content)
        .map_err(|e| format!("cannot write {}: {}", path.display(), e))
}

pub enum Confirm {
    Save,
    Discard,
    Cancel,
}

pub fn confirm_unsaved(name: &str) -> Confirm {
    let result = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title("Unsaved changes")
        .set_description(format!("Save changes to {}?", name))
        .set_buttons(rfd::MessageButtons::YesNoCancel)
        .show();
    match result {
        rfd::MessageDialogResult::Yes => Confirm::Save,
        rfd::MessageDialogResult::No => Confirm::Discard,
        _ => Confirm::Cancel,
    }
}

pub fn clipboard_text() -> Result<String, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard: {}", e))?;
    cb.get_text().map_err(|e| format!("clipboard: {}", e))
}
