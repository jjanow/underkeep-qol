//! Safe mode (PLAN §2): if the game exited abnormally twice in a row after
//! mods loaded, or Shift is held at start, the session runs with all mods off.
//!
//! Interface fixed; implementation pending.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeMode {
    Off,
    /// Previous sessions ended abnormally this many times in a row.
    Crashes(u32),
    /// Shift was held while the game started.
    ShiftHeld,
}

/// Called at runtime start: reads and updates the crash counter in `dir`
/// (`uk_qol/`), marks this session as running, and decides safe mode.
pub fn begin_session(dir: &Path) -> SafeMode {
    let _ = dir;
    SafeMode::Off
}

/// Called on a clean exit: clears the running marker.
pub fn end_session(dir: &Path) {
    let _ = dir;
}
