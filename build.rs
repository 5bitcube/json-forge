//! Embeds the application icon into the Windows executable so Explorer and
//! the taskbar show it, plus the document icon for .json files. The window
//! icon at runtime is set in `main.rs`.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/json-file.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        // Resource 1 is the application icon (lowest id = what Explorer shows
        // for the exe); resource 2 is the .json document icon that the file
        // association points at as `json-forge.exe,-2` (see register.ps1).
        res.set_icon_with_id("assets/icon.ico", "1");
        res.set_icon_with_id("assets/json-file.ico", "2");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon resource not embedded: {e}");
        }
    }
}
