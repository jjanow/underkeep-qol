//! Underkeep QoL runtime: runs inside the game process, loaded by the
//! `winmm.dll` proxy.
//!
//! It writes `uk_qol/uk_qol.log` in the game folder, logs which build of the
//! game it loaded into, starts the mod host (`host`) and installs its own
//! hooks (`corehooks`): save sidecars (`savehook`) and in-memory data
//! patches (`datapatch`), and connects the host to the overlay (`bridge`).

/// The overlay's egui, for mods that draw in the menu (`Mod::menu`).
pub use egui;

pub mod bridge;
pub mod build;
pub mod config;
pub mod corehooks;
pub mod datapatch;
pub mod game;
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
pub mod savehook;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, Once};
use std::time::{SystemTime, UNIX_EPOCH};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

static START: Once = Once::new();
static LOG: Mutex<Option<File>> = Mutex::new(None);

/// Starts the runtime on its own thread. Safe to call from `DllMain`: it only
/// spawns the thread, and does so once. `builtin` are the built-in mods.
pub fn start(builtin: &'static [host::ModCtor]) {
    START.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("uk-qol".into())
            .spawn(move || {
                let _ = std::panic::catch_unwind(|| run(builtin));
            });
    });
}

/// Startup: build detection, safe-mode session, config, then the mod host
/// (mods are loaded only outside safe mode), installed as the global.
fn run(builtin: &'static [host::ModCtor]) {
    // Releases the startup gate however run() ends, panics included.
    let _ready = ReadyOnDrop;
    let exe = std::env::current_exe();
    let shown = exe
        .as_ref()
        .map_or_else(|_| "?".into(), |p| p.display().to_string());
    log(&format!(
        "uk-qol {VERSION} loaded into {shown} (pid {})",
        std::process::id()
    ));
    guard::install_crash_handler();
    overlay::install();
    let build = match exe
        .map_err(build::DetectError::Io)
        .and_then(|p| build::detect_from_file(&p))
    {
        Ok(b) => {
            log(&b.to_string());
            Some(b.sha256_hex())
        }
        Err(e) => {
            log(&format!("build detection failed: {e}"));
            None
        }
    };

    let safe = begin_session();
    let dir = data_dir();
    let config = match dir
        .as_deref()
        .map(|d| config::Config::load(&config::config_path(d)))
    {
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            log(&format!("{e}; using defaults (the file is left as it is)"));
            config::Config::default()
        }
        None => config::Config::default(),
    };
    let (image, base) = match host::own_image() {
        Some((img, base)) => (Some(img), base),
        None => {
            if !safe.is_on() {
                log("can't read the game's image in memory; no mod can load");
            }
            (None, 0)
        }
    };
    let patterns_dir = dir.as_ref().map(|d| d.join("patterns"));
    // The runtime's own patterns, resolved before any mod's hooks go in.
    let core = match &image {
        Some(img) if !safe.is_on() => Some(corehooks::resolve(
            img,
            build.as_deref(),
            patterns_dir.as_deref(),
        )),
        _ => None,
    };
    let env = host::HostEnv {
        image,
        base,
        build,
        patterns_dir,
        log,
        note_active: log_active_mods,
    };
    let h = host::Host::start(builtin, env, &config, safe);
    for line in h.summary() {
        log(&line);
    }
    let mod_patches = h.active_patches();
    host::install(h);
    // Save and data-patch hooks, before the startup gate opens.
    if let Some(core) = core {
        let patches = datapatch::PatchSet::collect(
            &mod_patches,
            dir.as_ref().map(|d| d.join("patches")).as_deref(),
            log,
        );
        let log_defs = config
            .extra
            .get("runtime")
            .and_then(|r| r.get("log_defs"))
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);
        corehooks::install(&core, base, patches, log_defs);
    }
    bridge::install();
}

// Startup gate. The game's code must not run ahead of the mod hooks: defs
// are parsed and the startup logo starts during game init, which races this
// thread. The proxy calls `wait_ready` on the game's first winmm call (SDL
// initializing its timer), so the game waits there until `run` is done.

static READY: Mutex<bool> = Mutex::new(false);
static READY_CV: std::sync::Condvar = std::sync::Condvar::new();

struct ReadyOnDrop;

impl Drop for ReadyOnDrop {
    fn drop(&mut self) {
        signal_ready();
    }
}

/// Marks startup finished (hooks installed) and wakes [`wait_ready`].
pub fn signal_ready() {
    if let Ok(mut r) = READY.lock() {
        *r = true;
        READY_CV.notify_all();
    }
}

/// Blocks until [`signal_ready`] or `timeout`. Returns whether startup
/// finished. Called by the proxy on the game's first winmm call; must not be
/// called from the runtime thread or under the loader lock.
pub fn wait_ready(timeout: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    let Ok(guard) = READY.lock() else {
        return false;
    };
    let ok = match READY_CV.wait_timeout_while(guard, timeout, |r| !*r) {
        Ok((r, _)) => *r,
        Err(_) => false,
    };
    let waited = started.elapsed().as_millis();
    if ok {
        log(&format!(
            "game resumed after waiting {waited} ms for startup"
        ));
    } else {
        log(&format!(
            "startup took longer than {} ms; the game continues without waiting",
            timeout.as_millis()
        ));
    }
    ok
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
        let _ = writeln!(f, "[{}] {msg}", stamp());
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
        let _ = writeln!(f, "[{}] {msg}", stamp());
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

/// Log timestamp: Unix seconds with milliseconds (startup timing matters).
fn stamp() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", d.as_secs(), d.subsec_millis())
}

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
