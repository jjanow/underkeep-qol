//! Underkeep QoL runtime: runs inside the game process, loaded by the
//! `winmm.dll` proxy.
//!
//! For now this only proves the loader: it writes `uk_qol/uk_qol.log` in the
//! game folder. Build detection, patterns, hooks and mods come next (PLAN.md).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, Once};
use std::time::{SystemTime, UNIX_EPOCH};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

static START: Once = Once::new();
static LOG: Mutex<Option<File>> = Mutex::new(None);

/// Starts the runtime on its own thread. Safe to call from `DllMain`: it only
/// spawns the thread, and does so once.
pub fn start() {
    START.call_once(|| {
        let _ = std::thread::Builder::new().name("uk-qol".into()).spawn(|| {
            let _ = std::panic::catch_unwind(run);
        });
    });
}

fn run() {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    log(&format!(
        "uk-qol {VERSION} loaded into {exe} (pid {})",
        std::process::id()
    ));
}

/// `<game folder>/uk_qol`, next to the exe.
pub fn data_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("uk_qol"))
}

/// Appends a line to `uk_qol/uk_qol.log`, truncating the log at the first
/// write of each session. Never panics; logging failures are ignored.
pub fn log(msg: &str) {
    let Ok(mut guard) = LOG.lock() else { return };
    if guard.is_none() {
        let Some(dir) = data_dir() else { return };
        let _ = fs::create_dir_all(&dir);
        let Ok(f) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(dir.join("uk_qol.log"))
        else {
            return;
        };
        *guard = Some(f);
    }
    if let Some(f) = guard.as_mut() {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{t}] {msg}");
        let _ = f.flush();
    }
}
