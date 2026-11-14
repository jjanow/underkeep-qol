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
pub mod hotkey;
pub mod i18n;
pub mod iat;
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
    overlay::install();
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

/// Like [`log`], but gives up instead of waiting if another thread holds the
/// log lock, and never opens the file. For the crash handler, which may run
/// while the crashing thread holds the lock.
pub fn try_log(msg: &str) {
    if let Ok(mut guard) = LOG.try_lock()
        && let Some(f) = guard.as_mut()
    {
        let _ = writeln!(f, "[{}] {msg}", unix_secs());
        let _ = f.flush();
    }
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

static CRASHED_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Starts the safe-mode session in `uk_qol/` (crash counter, running marker,
/// Shift check; [`safe::begin_session`]) and logs how the previous session
/// ended. Call once, from the runtime thread, before loading mods; load none
/// unless the result is `SafeMode::Off`. Without it [`stop`] does nothing.
pub fn begin_session() -> safe::SafeMode {
    let Some(dir) = data_dir() else {
        return safe::SafeMode::Off;
    };
    let s = safe::begin_session_report(&dir);
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
    if let Some(dir) = safe::armed_dir() {
        safe::note_active_mods(dir, ids);
    }
}

/// Records that a fatal-class exception was just seen (first-chance, so the
/// game may still survive it). If the process exits within
/// [`CRASH_EXIT_WINDOW_SECS`] of the last one, [`stop`] leaves the running
/// marker, so a crash that a crash reporter ends through `ExitProcess` still
/// counts as abnormal, while an exception the game survived long before a
/// normal quit doesn't.
pub fn note_crash() {
    CRASHED_AT.store(unix_secs().max(1), std::sync::atomic::Ordering::SeqCst);
}

/// See [`note_crash`]. Generous, since a crash reporter may show a dialog
/// before it exits.
pub const CRASH_EXIT_WINDOW_SECS: u64 = 600;

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Clean process exit, called by the proxy's `DllMain` on
/// `DLL_PROCESS_DETACH` during `ExitProcess`. Runs under the loader lock with
/// every other thread already gone: only file I/O, never waits on a lock
/// (another thread may have died holding it). Ends the safe-mode session
/// unless [`begin_session`] never ran or [`note_crash`] was called.
pub fn stop() {
    let Some(dir) = safe::armed_dir() else { return };
    let crashed_at = CRASHED_AT.load(std::sync::atomic::Ordering::SeqCst);
    if crashed_at != 0 && unix_secs().saturating_sub(crashed_at) <= CRASH_EXIT_WINDOW_SECS {
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
