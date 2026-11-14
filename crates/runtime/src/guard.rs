//! Failure containment (PLAN §2): every hook body and mod callback runs
//! inside [`call`], which catches Rust panics and turns the failing mod off
//! for the session.
//!
//! Interface fixed; implementation pending.

/// Runs `f` on behalf of mod `mod_id`. Returns `None` (and marks the mod
/// failed, logging the panic message) if it panics or the mod has already
/// failed this session. Detours must call this inside the `extern` function:
/// a panic unwinding out of an `extern "system"` fn aborts the process.
pub fn call<R>(mod_id: &str, f: impl FnOnce() -> R) -> Option<R> {
    if is_failed(mod_id) {
        return None;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok()
}

/// Whether `mod_id` failed earlier this session.
pub fn is_failed(mod_id: &str) -> bool {
    let _ = mod_id;
    false
}

/// The reason `mod_id` was turned off, if it failed.
pub fn failure(mod_id: &str) -> Option<String> {
    let _ = mod_id;
    None
}

/// Marks `mod_id` failed with `reason`.
pub fn fail(mod_id: &str, reason: String) {
    let _ = (mod_id, reason);
}

/// The mod whose code is running on this thread, if any (for crash logs).
pub fn current_mod() -> Option<String> {
    None
}

/// Installs the process-wide handler that logs the active mod when the game
/// crashes. No-op off Windows.
pub fn install_crash_handler() {}
