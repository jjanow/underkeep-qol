//! The crash handler: a vectored exception handler that logs fatal
//! exceptions (code, address as module+offset, AV target, thread and the
//! active mod) and calls [`crate::note_crash`] so safe mode counts the
//! session as abnormal. It never handles an exception.

// Only the Windows handler uses the helpers; they are tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::fmt;
/// Exception codes worth logging: errors (severity 3) that aren't
/// customer-defined (C++ `0xE06D7363`, CLR, GCC unwinding `0x20474343`),
/// plus datatype misalignment. Informational and warning codes (thread
/// naming `0x406D1388`, `OutputDebugString`, breakpoints, single steps,
/// guard pages) are noise.
pub(super) fn is_fatal(code: u32) -> bool {
    code >> 28 == 0xC || code == 0x8000_0002
}

/// A fixed-capacity string, for formatting without the heap. Writes that
/// don't fit are truncated at a character boundary.
pub(super) struct StackStr<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackStr<N> {
    pub(super) const fn new() -> Self {
        StackStr {
            buf: [0; N],
            len: 0,
        }
    }

    pub(super) fn as_str(&self) -> &str {
        // Only whole UTF-8 sequences are ever copied in.
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> fmt::Write for StackStr<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = N - self.len;
        let mut n = s.len().min(room);
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

#[cfg(windows)]
pub(super) use win::install;

#[cfg(windows)]
mod win {
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::fmt::Write as _;
    use std::sync::Once;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::{StackStr, is_fatal};

    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
    const EXCEPTION_ACCESS_VIOLATION: u32 = 0xC000_0005;
    const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x4;
    const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x2;
    /// Exceptions logged per session; later ones pass silently.
    const MAX_LOGGED: u32 = 8;

    #[repr(C)]
    pub(in crate::guard) struct ExceptionRecord {
        pub code: u32,
        pub flags: u32,
        pub record: *mut ExceptionRecord,
        pub address: *mut c_void,
        pub nparams: u32,
        pub info: [usize; 15],
    }

    #[repr(C)]
    pub(in crate::guard) struct ExceptionPointers {
        pub record: *mut ExceptionRecord,
        pub context: *mut c_void,
    }

    type Handler = unsafe extern "system" fn(*mut ExceptionPointers) -> i32;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AddVectoredExceptionHandler(first: u32, handler: Handler) -> *mut c_void;
        fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut *mut c_void) -> i32;
        fn GetModuleFileNameW(module: *mut c_void, name: *mut u16, size: u32) -> u32;
        fn GetCurrentThreadId() -> u32;
    }

    static INSTALL: Once = Once::new();
    static LOGGED: AtomicU32 = AtomicU32::new(0);

    thread_local! {
        static IN_HANDLER: Cell<bool> = const { Cell::new(false) };
    }

    pub(in crate::guard) fn install() {
        INSTALL.call_once(|| {
            // SAFETY: registering a handler that never unwinds.
            let h = unsafe { AddVectoredExceptionHandler(1, handler) };
            crate::log(if h.is_null() {
                "crash handler: AddVectoredExceptionHandler failed"
            } else {
                "crash handler installed"
            });
        });
    }

    /// Module holding `addr`: its file name (written to `name`) and base.
    pub(in crate::guard) fn module_of(addr: usize, name: &mut StackStr<128>) -> Option<usize> {
        let mut h: *mut c_void = std::ptr::null_mut();
        // SAFETY: FROM_ADDRESS takes `addr` as an address, not a name;
        // UNCHANGED_REFCOUNT means nothing to release.
        let ok = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                addr as *const u16,
                &mut h,
            )
        };
        if ok == 0 || h.is_null() {
            return None;
        }
        let mut path = [0u16; 260];
        // SAFETY: the buffer is 260 u16 long.
        let n = unsafe { GetModuleFileNameW(h, path.as_mut_ptr(), path.len() as u32) } as usize;
        let path = &path[..n.min(path.len())];
        let start = path
            .iter()
            .rposition(|&c| c == u16::from(b'\\') || c == u16::from(b'/'))
            .map_or(0, |i| i + 1);
        for c in char::decode_utf16(path[start..].iter().copied()) {
            let _ = name.write_char(c.unwrap_or('?'));
        }
        Some(h as usize)
    }

    /// The vectored handler. Runs on the faulting thread, possibly with a
    /// corrupt heap or a lock held, so it doesn't allocate: it formats into
    /// a stack buffer and calls `crate::try_log`, which only writes to its
    /// already-open file (it would allocate only if nothing was logged yet).
    pub(in crate::guard) unsafe extern "system" fn handler(ep: *mut ExceptionPointers) -> i32 {
        // SAFETY: the OS passes valid pointers; null-checked anyway.
        let Some(rec) = (unsafe { ep.as_ref().and_then(|ep| ep.record.as_ref()) }) else {
            return EXCEPTION_CONTINUE_SEARCH;
        };
        if !is_fatal(rec.code) {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        crate::note_crash();
        let reentered = IN_HANDLER.try_with(|f| f.replace(true)).unwrap_or(true);
        if reentered {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        if LOGGED.fetch_add(1, Ordering::Relaxed) < MAX_LOGGED {
            report(rec);
        }
        let _ = IN_HANDLER.try_with(|f| f.set(false));
        EXCEPTION_CONTINUE_SEARCH
    }

    fn report(rec: &ExceptionRecord) {
        let addr = rec.address as usize;
        let mut module = StackStr::<128>::new();
        let base = module_of(addr, &mut module);
        let module = base.map(|b| (module.as_str(), (addr - b) as u64));

        super::super::with_current_mod(|active| {
            let mut line = StackStr::<512>::new();
            let _ = write!(line, "exception {:#010x} at {addr:#x}", rec.code);
            if let Some((m, off)) = module {
                let _ = write!(line, " ({m}+{off:#x})");
            }
            if rec.code == EXCEPTION_ACCESS_VIOLATION && rec.nparams >= 2 {
                let what = match rec.info[0] {
                    0 => "reading",
                    1 => "writing",
                    8 => "executing",
                    _ => "accessing",
                };
                let _ = write!(line, " {what} {:#x}", rec.info[1]);
            }
            // SAFETY: plain FFI call.
            let tid = unsafe { GetCurrentThreadId() };
            let _ = write!(
                line,
                ", thread {tid}, mod {}; first chance, the game may still handle it",
                active.unwrap_or("none")
            );
            crate::try_log(line.as_str());
        });
    }
}

#[cfg(all(test, windows))]
pub(super) use win::{ExceptionPointers, ExceptionRecord, handler, module_of};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    #[test]
    fn fatal_codes() {
        for c in [
            0xC000_0005u32,
            0xC000_001D,
            0xC000_0094,
            0xC000_00FD,
            0xC000_0374,
            0x8000_0002,
        ] {
            assert!(is_fatal(c), "{c:#x}");
        }
        for c in [
            0xE06D_7363u32, // C++
            0xE043_4352,    // CLR
            0x2047_4343,    // GCC unwind (Rust panics on windows-gnu)
            0x406D_1388,    // thread naming
            0x4001_0006,
            0x4001_000A, // OutputDebugString
            0x8000_0003,
            0x8000_0004,
            0x8000_0001, // breakpoint, single step, guard page
        ] {
            assert!(!is_fatal(c), "{c:#x}");
        }
    }

    #[test]
    fn stack_str_truncates_at_char_boundary() {
        let mut s = StackStr::<5>::new();
        write!(s, "ab").unwrap();
        write!(s, "cdé").unwrap(); // é is 2 bytes; only "cd" fits... plus 1 byte
        assert_eq!(s.as_str(), "abcd");
        write!(s, "xyz").unwrap();
        assert_eq!(s.as_str(), "abcdx");
    }

    #[cfg(windows)]
    #[test]
    fn handler_logs_and_continues_search() {
        let here = fatal_codes as fn() as usize;
        let mut name = StackStr::<128>::new();
        let base = module_of(here, &mut name).expect("own module");
        assert!(base <= here);
        assert!(name.as_str().ends_with(".exe"), "{}", name.as_str());
        assert!(module_of(8, &mut StackStr::<128>::new()).is_none());

        // A synthetic access violation: logged, never handled.
        let mut rec = ExceptionRecord {
            code: 0xC000_0005,
            flags: 0,
            record: std::ptr::null_mut(),
            address: here as *mut _,
            nparams: 2,
            info: [0; 15],
        };
        rec.info[1] = 0x10;
        let mut ep = ExceptionPointers {
            record: &mut rec,
            context: std::ptr::null_mut(),
        };
        let r = crate::guard::call("crash-test", || unsafe { handler(&mut ep) });
        assert_eq!(r, Some(0));
        unsafe { (*ep.record).code = 0xE06D_7363 };
        assert_eq!(unsafe { handler(&mut ep) }, 0);
    }
}
