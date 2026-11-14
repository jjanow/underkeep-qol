//! Underkeep QoL runtime: runs inside the game process, loaded by the
//! `winmm.dll` proxy.
//!
//! So far it writes `uk_qol/uk_qol.log` in the game folder and logs which
//! build of the game it loaded into. The pattern scanner (`pattern`) is ready
//! for hooks; hooks and mods come next (PLAN.md).

pub mod build;
pub mod config;
pub mod guard;
pub mod hook;
pub mod host;
pub mod input;
pub mod overlay;
pub mod pattern;
pub mod pe;
pub mod safe;
pub mod save;

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
pub fn start(builtin: &'static [host::ModCtor]) {
    let _ = builtin; // loaded by the mod host (pending)
    START.call_once(|| {
        let _ = std::thread::Builder::new().name("uk-qol".into()).spawn(|| {
            let _ = std::panic::catch_unwind(run);
        });
    });
}

fn run() {
    let exe = std::env::current_exe();
    let shown = exe
        .as_ref()
        .map_or_else(|_| "?".into(), |p| p.display().to_string());
    log(&format!(
        "uk-qol {VERSION} loaded into {shown} (pid {})",
        std::process::id()
    ));
    match exe
        .map_err(build::DetectError::Io)
        .and_then(|p| build::detect_from_file(&p))
    {
        Ok(b) => log(&b.to_string()),
        Err(e) => log(&format!("build detection failed: {e}")),
    }
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

// Session state for safe mode and crash triage (`safe`). The mod host calls
// `begin_session` before loading mods and `log_active_mods` before enabling
// their hooks; the proxy calls `stop` on process exit.

static SESSION_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
static CRASHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Starts the safe-mode session in `uk_qol/` (crash counter, running marker,
/// Shift check) and logs how the previous session ended. Call once, from the
/// runtime thread, before loading mods; load none unless the result is
/// `SafeMode::Off`. Without this call [`stop`] does nothing.
pub fn begin_session() -> safe::SafeMode {
    let Some(dir) = data_dir() else {
        return safe::SafeMode::Off;
    };
    let s = safe::begin_session_with(&dir, safe::shift_held());
    if let Some(prev) = &s.previous_abnormal {
        let mods = if prev.mods.is_empty() {
            "none".to_owned()
        } else {
            prev.mods.join(", ")
        };
        log(&format!(
            "previous session (pid {}) ended abnormally with mods: {mods} \
             ({} abnormal exits in a row)",
            prev.pid, s.abnormal_exits
        ));
    }
    match &s.mode {
        safe::SafeMode::Off => {}
        safe::SafeMode::Crashes(n) => log(&format!(
            "safe mode: the game exited abnormally {n} times in a row; all mods off"
        )),
        safe::SafeMode::ShiftHeld => log("safe mode: Shift held at start; all mods off"),
    }
    let _ = SESSION_DIR.set(dir);
    s.mode
}

/// Logs `active mods: a, b` and records them in the session file, so that a
/// crash names them on the next start. Call before enabling their hooks.
pub fn log_active_mods(ids: &[&str]) {
    let list = if ids.is_empty() {
        "none".to_owned()
    } else {
        ids.join(", ")
    };
    log(&format!("active mods: {list}"));
    if let Some(dir) = SESSION_DIR.get() {
        safe::note_active_mods(dir, ids);
    }
}

/// Marks this session as crashed: [`stop`] then leaves the running marker, so
/// the next start counts an abnormal exit even if the process goes on to exit
/// through `ExitProcess` (e.g. a crash handler that exits "normally").
pub fn note_crash() {
    CRASHED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Clean process exit, called by the proxy's `DllMain` on
/// `DLL_PROCESS_DETACH` during `ExitProcess`. Runs under the loader lock with
/// every other thread already gone: only file I/O, never waits on a lock
/// (another thread may have died holding it). Ends the safe-mode session
/// unless [`begin_session`] never ran or [`note_crash`] was called.
pub fn stop() {
    let Some(dir) = SESSION_DIR.get() else { return };
    if CRASHED.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    safe::end_session(dir);
    if let Ok(mut guard) = LOG.try_lock()
        && let Some(f) = guard.as_mut()
    {
        let _ = writeln!(f, "clean exit");
        let _ = f.flush();
    }
}
