//! One window per machine. A later launch hands its command line to the
//! instance already running, over a named pipe (a Unix socket elsewhere), and
//! exits; the running window opens the files in new tabs and comes to the
//! front. Same behaviour as MDHero's "open in existing window" mode.

use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use eframe::egui;
use interprocess::local_socket::{prelude::*, GenericNamespaced, Listener, ListenerOptions, Name, Stream};

pub enum Launch {
    /// This process owns the window. `None` when the pipe could not be
    /// created: the app still runs, just without forwarding.
    Primary(Option<Listener>),
    /// The files went to the running instance; exit now.
    Forwarded,
}

/// Pipe name tied to this executable, so a dev build and the installed copy
/// run side by side without stealing each other's files.
fn socket_name() -> io::Result<Name<'static>> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let exe = std::env::current_exe().and_then(std::fs::canonicalize).unwrap_or_default();
    let mut h = DefaultHasher::new();
    exe.to_string_lossy().to_lowercase().hash(&mut h);
    format!("json-forge-{:016x}.sock", h.finish()).to_ns_name::<GenericNamespaced>()
}

/// Join the running instance if there is one, else become it.
pub fn claim(paths: &[PathBuf]) -> Launch {
    let Ok(name) = socket_name() else { return Launch::Primary(None) };
    if let Ok(conn) = Stream::connect(name.clone()) {
        if forward(conn, paths).is_ok() {
            return Launch::Forwarded;
        }
    }
    Launch::Primary(ListenerOptions::new().name(name).create_sync().ok())
}

/// Send every path, made absolute against this launch's working directory,
/// one per line.
fn forward(mut conn: Stream, paths: &[PathBuf]) -> io::Result<()> {
    allow_foreground();
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut msg = String::new();
    for p in paths {
        let abs = if p.is_absolute() { p.clone() } else { cwd.join(p) };
        msg.push_str(&abs.to_string_lossy());
        msg.push('\n');
    }
    conn.write_all(msg.as_bytes())?;
    conn.flush()
}

/// Let the window we are about to poke take the foreground: Windows only
/// grants that to the process the user just launched, which is us.
#[cfg(windows)]
fn allow_foreground() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
    }
}

#[cfg(not(windows))]
fn allow_foreground() {}

/// Accept hand-overs on a background thread. Each one arrives on `tx` as the
/// list of paths (possibly empty: "just come to the front").
pub fn serve(listener: Listener, ctx: egui::Context, tx: Sender<Vec<PathBuf>>) {
    let _ = std::thread::Builder::new().name("single-instance".into()).spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let paths: Vec<PathBuf> = BufReader::new(conn)
                .lines()
                .map_while(Result::ok)
                .filter(|l| !l.trim().is_empty())
                .map(PathBuf::from)
                .collect();
            if tx.send(paths).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
}
