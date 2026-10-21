//! Native dialogs and file IO. Every IO function returns an error string the
//! app can surface in the status bar instead of swallowing failures.

use std::path::{Path, PathBuf};

/// Multi-select open dialog; empty when cancelled.
pub fn pick_open() -> Vec<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("JSON", &["json", "jsonl"])
        .add_filter("All files", &["*"])
        .pick_files()
        .unwrap_or_default()
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

/// Ask whether to save `name` before it goes away.
///
/// On Windows this is a plain `MessageBoxW` without an icon: every dialog
/// that shows one (which rfd always does) also plays the system alert sound.
#[cfg(windows)]
pub fn confirm_unsaved(name: &str) -> Confirm {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, MessageBoxW, IDNO, IDYES, MB_DEFBUTTON1, MB_TASKMODAL, MB_YESNOCANCEL,
    };
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let title = wide("Unsaved changes");
    let text = wide(&format!("Save changes to {}?", name));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let result = unsafe {
        MessageBoxW(GetForegroundWindow(), text.as_ptr(), title.as_ptr(), MB_YESNOCANCEL | MB_DEFBUTTON1 | MB_TASKMODAL)
    };
    match result {
        IDYES => Confirm::Save,
        IDNO => Confirm::Discard,
        _ => Confirm::Cancel,
    }
}

#[cfg(not(windows))]
pub fn confirm_unsaved(name: &str) -> Confirm {
    let result = rfd::MessageDialog::new()
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
