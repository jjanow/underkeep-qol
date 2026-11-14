//! Safe mode (PLAN §2): if the game exited abnormally twice in a row after
//! mods loaded, or Shift is held at start, the session runs with all mods off.
//!
//! State is one file, `uk_qol/session.toml`:
//!
//! ```toml
//! running = true          # set at start, false after a clean exit
//! abnormal_exits = 1      # consecutive sessions that ended without a clean exit
//! pid = 4242              # of the session that wrote the file
//! started = 1763140000    # Unix seconds
//! mods = ["skip-logos"]   # mods active in that session, for crash triage
//! ```
//!
//! - [`begin_session`] (runtime start): if the file says `running = true`,
//!   the previous session never reached a clean exit (crash, kill, power
//!   loss): `abnormal_exits` goes up by one, otherwise it's reset to 0. At
//!   [`CRASH_THRESHOLD`] the session is in safe mode. Then the file is
//!   rewritten with `running = true` and an empty mods list.
//! - [`note_active_mods`] records the mods this session loads.
//! - [`end_session`] (clean exit, from the proxy's `DllMain` on process
//!   detach) rewrites it with `running = false` and `abnormal_exits = 0`. The
//!   mods list stays, for triage.
//! - [`reset`] zeroes the counter (the manager or overlay leaving safe mode)
//!   and leaves `running` as it is.
//!
//! A file that doesn't parse counts as "no previous session". Nothing here
//! panics or returns errors to the game; failed writes only lose the counter.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use toml::{Table, Value};

use crate::config::write_atomic;

/// Consecutive abnormal exits that turn safe mode on.
pub const CRASH_THRESHOLD: u32 = 2;

/// File name of the session state in the data folder.
pub const FILE_NAME: &str = "session.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeMode {
    Off,
    /// Previous sessions ended abnormally this many times in a row.
    Crashes(u32),
    /// Shift was held while the game started.
    ShiftHeld,
}

impl SafeMode {
    pub fn is_on(&self) -> bool {
        *self != SafeMode::Off
    }
}

/// The contents of `session.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    pub running: bool,
    pub abnormal_exits: u32,
    pub pid: u32,
    /// Unix seconds.
    pub started: u64,
    pub mods: Vec<String>,
}

impl Session {
    pub fn parse(text: &str) -> Option<Session> {
        let t: Table = text.parse().ok()?;
        let int = |k: &str| t.get(k).and_then(Value::as_integer).unwrap_or(0).max(0);
        Some(Session {
            running: t.get("running").and_then(Value::as_bool).unwrap_or(false),
            abnormal_exits: int("abnormal_exits").min(u32::MAX as i64) as u32,
            pid: int("pid").min(u32::MAX as i64) as u32,
            started: int("started") as u64,
            mods: t
                .get("mods")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    pub fn to_toml(&self) -> String {
        format!(
            "# uk-qol session state (safe mode). Written by the mod runtime.\n\
             running = {}\nabnormal_exits = {}\npid = {}\nstarted = {}\nmods = {}\n",
            self.running,
            self.abnormal_exits,
            self.pid,
            self.started,
            Value::Array(self.mods.iter().map(|m| Value::from(m.as_str())).collect()),
        )
    }
}

/// `<dir>/session.toml`.
pub fn session_path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// Reads the session file; `None` if it's missing or doesn't parse.
pub fn read(dir: &Path) -> Option<Session> {
    Session::parse(&fs::read_to_string(session_path(dir)).ok()?)
}

fn write(dir: &Path, s: &Session) {
    let _ = write_atomic(&session_path(dir), s.to_toml().as_bytes());
}

/// What [`begin_session_with`] found and decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStart {
    pub mode: SafeMode,
    /// Consecutive abnormal exits, this session's predecessor included.
    pub abnormal_exits: u32,
    /// The previous session, if it ended abnormally (its `mods` are the ones
    /// that were active when it went down).
    pub previous_abnormal: Option<Session>,
}

/// The folder of this process's session, set by [`begin_session`].
static ARMED: OnceLock<PathBuf> = OnceLock::new();

/// Called at runtime start: reads and updates the crash counter in `dir`
/// (`uk_qol/`), marks this session as running, and decides safe mode.
/// Also arms the clean-exit path: [`crate::stop`] ends this session on
/// process exit, and [`crate::log_active_mods`] records into it.
pub fn begin_session(dir: &Path) -> SafeMode {
    begin_session_report(dir).mode
}

/// [`begin_session`], returning the details (for the log and the overlay).
pub fn begin_session_report(dir: &Path) -> SessionStart {
    let s = begin_session_with(dir, shift_held());
    let _ = ARMED.set(dir.to_owned());
    s
}

/// The session folder if [`begin_session`] ran in this process.
pub fn armed_dir() -> Option<&'static Path> {
    ARMED.get().map(PathBuf::as_path)
}

/// The counter logic of [`begin_session`], with the Shift state given and
/// without arming the clean-exit path (for tests and tools).
/// Crashes take precedence over Shift in the returned mode (both mean mods off).
pub fn begin_session_with(dir: &Path, shift: bool) -> SessionStart {
    let prev = read(dir);
    let (abnormal_exits, previous_abnormal) = match prev {
        Some(p) if p.running => (p.abnormal_exits.saturating_add(1), Some(p)),
        _ => (0, None),
    };
    write(
        dir,
        &Session {
            running: true,
            abnormal_exits,
            pid: std::process::id(),
            started: now(),
            mods: Vec::new(),
        },
    );
    let mode = if abnormal_exits >= CRASH_THRESHOLD {
        SafeMode::Crashes(abnormal_exits)
    } else if shift {
        SafeMode::ShiftHeld
    } else {
        SafeMode::Off
    };
    SessionStart {
        mode,
        abnormal_exits,
        previous_abnormal,
    }
}

/// Records the mods this session is loading, so a crash can be traced to
/// them on the next start. Call it before their hooks are enabled.
pub fn note_active_mods(dir: &Path, ids: &[&str]) {
    let mut s = read(dir).unwrap_or_else(|| Session {
        running: true,
        pid: std::process::id(),
        started: now(),
        ..Session::default()
    });
    s.mods = ids.iter().map(|&m| m.to_owned()).collect();
    write(dir, &s);
}

/// Called on a clean exit: clears the running marker and the crash counter.
/// Only file I/O, so it can run under the loader lock.
pub fn end_session(dir: &Path) {
    let mut s = read(dir).unwrap_or_default();
    s.running = false;
    s.abnormal_exits = 0;
    write(dir, &s);
}

/// Leaves crash safe mode: zeroes the counter. `running` is left as it is, so
/// resetting from the overlay mid-session still lets a later crash count.
/// Does nothing if there's no session file.
pub fn reset(dir: &Path) {
    if let Some(mut s) = read(dir) {
        s.abnormal_exits = 0;
        write(dir, &s);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether a Shift key is down right now. On Windows `GetAsyncKeyState` is
/// looked up at run time (user32 is already loaded in the game; the proxy
/// must not import it). Always false elsewhere.
///
/// Runs at runtime start, before the game has a window. Under Wine the key
/// state may not be known to Wine yet at that point (untested under Proton).
pub fn shift_held() -> bool {
    #[cfg(windows)]
    {
        win::shift_held()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    const VK_SHIFT: i32 = 0x10;

    pub fn shift_held() -> bool {
        let name: Vec<u16> = "user32.dll\0".encode_utf16().collect();
        // SAFETY: NUL-terminated name. Not called under the loader lock (runs
        // on the runtime thread).
        let user32 = unsafe { LoadLibraryW(name.as_ptr()) };
        if user32.is_null() {
            return false;
        }
        // SAFETY: valid module, NUL-terminated name.
        let f = unsafe { GetProcAddress(user32, c"GetAsyncKeyState".as_ptr().cast()) };
        if f.is_null() {
            return false;
        }
        // SAFETY: GetAsyncKeyState's signature is `SHORT (int vKey)`.
        let get: extern "system" fn(i32) -> i16 = unsafe { std::mem::transmute(f) };
        get(VK_SHIFT) < 0 // high bit: down now
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::temp_dir;

    #[test]
    fn crash_counter() {
        let dir = temp_dir("safe");
        // First launch ever.
        let s = begin_session_with(&dir, false);
        assert_eq!(s.mode, SafeMode::Off);
        assert_eq!(s.abnormal_exits, 0);
        assert!(read(&dir).unwrap().running);
        note_active_mods(&dir, &["a", "b"]);

        // That session crashed (no end_session).
        let s = begin_session_with(&dir, false);
        assert_eq!(s.mode, SafeMode::Off);
        assert_eq!(s.abnormal_exits, 1);
        assert_eq!(s.previous_abnormal.unwrap().mods, ["a", "b"]);
        assert!(read(&dir).unwrap().mods.is_empty());

        // Crashed again: safe mode.
        let s = begin_session_with(&dir, false);
        assert_eq!(s.mode, SafeMode::Crashes(2));
        assert!(s.mode.is_on());

        // And again, with Shift: crashes win.
        assert_eq!(begin_session_with(&dir, true).mode, SafeMode::Crashes(3));

        // Clean exit resets.
        note_active_mods(&dir, &["c"]);
        end_session(&dir);
        let after = read(&dir).unwrap();
        assert!(!after.running);
        assert_eq!(after.abnormal_exits, 0);
        assert_eq!(after.mods, ["c"], "mods kept for triage");
        let s = begin_session_with(&dir, false);
        assert_eq!(s.mode, SafeMode::Off);
        assert_eq!(s.previous_abnormal, None);

        // Shift alone.
        end_session(&dir);
        assert_eq!(begin_session_with(&dir, true).mode, SafeMode::ShiftHeld);
    }

    #[test]
    fn reset_keeps_running() {
        let dir = temp_dir("safe-reset");
        reset(&dir); // no file: nothing happens
        assert!(read(&dir).is_none());
        begin_session_with(&dir, false);
        begin_session_with(&dir, false);
        assert_eq!(begin_session_with(&dir, false).mode, SafeMode::Crashes(2));
        reset(&dir);
        let s = read(&dir).unwrap();
        assert!(s.running);
        assert_eq!(s.abnormal_exits, 0);
        // A crash after the reset counts from zero.
        assert_eq!(begin_session_with(&dir, false).abnormal_exits, 1);
    }

    #[test]
    fn bad_file_is_no_session() {
        let dir = temp_dir("safe-bad");
        fs::write(session_path(&dir), "running = [").unwrap();
        let s = begin_session_with(&dir, false);
        assert_eq!(s.abnormal_exits, 0);
        assert!(read(&dir).unwrap().running);
        // end_session without begin still writes a valid file.
        let dir = temp_dir("safe-end");
        end_session(&dir);
        assert!(!read(&dir).unwrap().running);
    }

    #[test]
    fn shift_lookup_runs() {
        // No key is held during tests; this checks the dynamic lookup works.
        assert!(!shift_held());
    }

    #[test]
    fn session_round_trip() {
        let s = Session {
            running: true,
            abnormal_exits: 7,
            pid: 99,
            started: 1_700_000_000,
            mods: vec!["x".into(), "we\"ird".into()],
        };
        assert_eq!(Session::parse(&s.to_toml()).unwrap(), s);
    }
}
