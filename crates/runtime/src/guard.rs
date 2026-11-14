//! Failure containment (PLAN §2): every hook body and mod callback runs
//! inside [`call`], which catches Rust panics and turns the failing mod off
//! for the session.
//!
//! - [`call`] runs a mod's code, records it as this thread's active mod
//!   ([`current_mod`], nested calls stack) and catches panics. A panic marks
//!   the mod failed ([`fail`]) with the panic message and logs it.
//! - Detours wrap their body in [`call_or`], which falls back to the
//!   original function (the trampoline) when the mod panics or has failed,
//!   so the game carries on. A panic must never unwind out of an
//!   `extern "system"`/`extern "C"` detour: since Rust 1.81 that aborts the
//!   process.
//! - [`install_crash_handler`] logs hardware exceptions (access violations
//!   and the like) with the active mod and marks the session crashed for
//!   safe mode ([`crate::note_crash`]). It never handles them.
//!
//! Only Rust panics are contained. An access violation or other hardware
//! exception inside a detour can't be recovered from safely in Rust (no
//! unwinding through it, state unknown); the game crashes as it would
//! without mods, and the crash handler records which mod was running. A C++
//! exception thrown by game code through Rust frames isn't caught either.

mod crash;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static FAILED: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
/// Fast path for `is_failed` on hot detours: false until any mod fails.
static ANY_FAILED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Mods whose code is running on this thread, innermost last. The
    /// pointers borrow the `mod_id` of each live [`call`] frame and are
    /// popped before it returns.
    static ACTIVE: RefCell<Vec<*const str>> = const { RefCell::new(Vec::new()) };
}

/// Pops this thread's innermost active mod when dropped (also on unwind).
struct Active;

impl Active {
    fn push(mod_id: &str) -> Active {
        let _ = ACTIVE.try_with(|a| {
            if let Ok(mut a) = a.try_borrow_mut() {
                a.push(mod_id as *const str);
            }
        });
        Active
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        let _ = ACTIVE.try_with(|a| {
            if let Ok(mut a) = a.try_borrow_mut() {
                a.pop();
            }
        });
    }
}

/// Runs `f` on behalf of mod `mod_id`. Returns `None` (and marks the mod
/// failed, logging the panic message) if it panics or the mod has already
/// failed this session. Detours must call this inside the `extern` function:
/// a panic unwinding out of an `extern "system"` fn aborts the process.
pub fn call<R>(mod_id: &str, f: impl FnOnce() -> R) -> Option<R> {
    if is_failed(mod_id) {
        return None;
    }
    let active = Active::push(mod_id);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    drop(active);
    match r {
        Ok(v) => Some(v),
        Err(payload) => {
            let msg = panic_message(payload.as_ref());
            // A payload whose Drop panics would escape; leak it instead.
            std::mem::forget(payload);
            fail(mod_id, format!("panicked: {msg}"));
            None
        }
    }
}

/// [`call`] for detour bodies: runs `f` for `mod_id`, or `fallback` if the
/// mod panics or has failed this session. The fallback should call the
/// original function through the trampoline, so the game goes on as if
/// the hook weren't there. If `f` panicked partway, its side effects stay.
///
/// ```ignore
/// static ORIG: AtomicUsize = AtomicUsize::new(0); // trampoline from HookSet::detour
/// extern "system" fn on_swap(window: *mut c_void) -> bool {
///     let orig: extern "system" fn(*mut c_void) -> bool =
///         unsafe { std::mem::transmute(ORIG.load(Ordering::Relaxed)) };
///     guard::call_or("overlay", || { draw(); orig(window) }, || orig(window))
/// }
/// ```
///
/// `fallback` runs outside the guard: it must not panic.
pub fn call_or<R>(mod_id: &str, f: impl FnOnce() -> R, fallback: impl FnOnce() -> R) -> R {
    match call(mod_id, f) {
        Some(v) => v,
        None => fallback(),
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "(non-string panic payload)".to_owned()
    }
}

fn failed() -> std::sync::MutexGuard<'static, BTreeMap<String, String>> {
    FAILED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether `mod_id` failed earlier this session.
pub fn is_failed(mod_id: &str) -> bool {
    ANY_FAILED.load(Ordering::Acquire) && failed().contains_key(mod_id)
}

/// The reason `mod_id` was turned off, if it failed.
pub fn failure(mod_id: &str) -> Option<String> {
    if !ANY_FAILED.load(Ordering::Acquire) {
        return None;
    }
    failed().get(mod_id).cloned()
}

/// Marks `mod_id` failed with `reason`. The first reason is kept; it is
/// logged once.
pub fn fail(mod_id: &str, reason: String) {
    let mut f = failed();
    if f.contains_key(mod_id) {
        return;
    }
    crate::log(&format!(
        "mod {mod_id} turned off for this session: {reason}"
    ));
    f.insert(mod_id.to_owned(), reason);
    ANY_FAILED.store(true, Ordering::Release);
}

/// The mod whose code is running on this thread, if any (for crash logs).
pub fn current_mod() -> Option<String> {
    with_current_mod(|m| m.map(str::to_owned))
}

/// Calls `f` with this thread's innermost active mod, without allocating
/// (usable from the crash handler).
fn with_current_mod<R>(f: impl FnOnce(Option<&str>) -> R) -> R {
    let mut f = Some(f);
    let r = ACTIVE.try_with(|a| {
        let top = a.try_borrow().ok().and_then(|a| a.last().copied());
        // SAFETY: each pointer borrows the `mod_id` of a `call` frame that is
        // still running on this thread (it's popped before that frame ends).
        let top = top.map(|p| unsafe { &*p });
        (f.take().expect("called once"))(top)
    });
    match r {
        Ok(v) => v,
        Err(_) => (f.take().expect("not called"))(None),
    }
}

/// Installs the process-wide handler that logs the active mod when the game
/// crashes. No-op off Windows. Safe to call more than once.
///
/// It is a vectored exception handler, so it sees exceptions *first-chance*:
/// before the game's own handlers, including ones the game or a driver
/// catches and survives. A logged exception is therefore not proof of a
/// crash; the first 8 per session are logged. Every one also calls
/// [`crate::note_crash`]: an exit soon after one counts as abnormal for safe
/// mode, one survived long before a normal quit doesn't. It always returns `EXCEPTION_CONTINUE_SEARCH`:
/// nothing is ever swallowed.
pub fn install_crash_handler() {
    #[cfg(windows)]
    crash::install();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_marks_failed_and_is_contained() {
        assert_eq!(call("guard-ok", || 5), Some(5));
        assert!(!is_failed("guard-ok"));

        let r = call("guard-bad", || -> u32 { panic!("boom {}", 1) });
        assert_eq!(r, None);
        assert!(is_failed("guard-bad"));
        assert_eq!(failure("guard-bad").as_deref(), Some("panicked: boom 1"));
        // Failed mods don't run again.
        let mut ran = false;
        assert_eq!(call("guard-bad", || ran = true), None);
        assert!(!ran);
        assert!(!is_failed("guard-ok"));
        assert_eq!(failure("guard-ok"), None);
    }

    #[test]
    fn call_or_falls_back() {
        assert_eq!(call_or("guard-or", || 1, || 2), 1);
        assert_eq!(call_or("guard-or", || -> i32 { panic!("x") }, || 2), 2);
        assert_eq!(call_or("guard-or", || 1, || 2), 2);
    }

    #[test]
    fn first_reason_is_kept() {
        fail("guard-twice", "first".into());
        fail("guard-twice", "second".into());
        assert_eq!(failure("guard-twice").as_deref(), Some("first"));
    }

    #[test]
    fn current_mod_tracks_nesting_and_unwinds() {
        assert_eq!(current_mod(), None);
        call("guard-outer", || {
            assert_eq!(current_mod().as_deref(), Some("guard-outer"));
            call("guard-inner", || {
                assert_eq!(current_mod().as_deref(), Some("guard-inner"));
            });
            let _ = call("guard-inner-panics", || {
                assert_eq!(current_mod().as_deref(), Some("guard-inner-panics"));
                panic!("inner");
            });
            assert_eq!(current_mod().as_deref(), Some("guard-outer"));
        });
        assert_eq!(current_mod(), None);
        assert!(!is_failed("guard-outer"));
        assert!(is_failed("guard-inner-panics"));
    }

    #[test]
    fn current_mod_is_per_thread() {
        call("guard-main-thread", || {
            let other = std::thread::spawn(current_mod).join().unwrap();
            assert_eq!(other, None);
        });
    }
}
