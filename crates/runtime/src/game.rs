//! Guarded reads of the game's memory: plain values and the game's `String`.
//!
//! Every read first checks with `VirtualQuery` that the whole range is
//! committed, readable memory, so a wrong offset or a stale pointer gives
//! `None` instead of an access violation. That doesn't make a read *right*:
//! another thread may change the memory meanwhile. Read game state from the
//! game's own thread (a detour, `on_save`, `on_load_save`).
//!
//! # The game's `String` (1.0.5)
//! 0x18 bytes: `+0x00` pointer to a reference-counted buffer (null for
//! literals and the empty string), `+0x08` pointer to the bytes (never null:
//! an empty string points at a shared empty literal), `+0x10` length in bytes
//! (`i32`). The bytes are UTF-8 and not reliably NUL-terminated, so the length
//! is used. Seen in `Save_DeleteFile`, `SaveInfo` (`+0x08`), the hero name
//! (`Hero+0x10`) and item defs (`ItemDef+0x78`).

/// Offset of the data pointer in the game's `String`.
pub const STRING_DATA: usize = 0x08;
/// Offset of the `i32` length in the game's `String`.
pub const STRING_LEN: usize = 0x10;
/// Longest string [`read_string`] accepts; longer lengths are taken as a
/// sign of a wrong pointer.
pub const STRING_MAX: usize = 4096;

/// Whether `len` bytes from `addr` are committed and readable (not a guard
/// page). Addresses below 64 KB are never readable (Windows doesn't map
/// them; this catches null-plus-offset pointers). Off Windows (unit tests
/// only) any other range that doesn't wrap is taken as readable.
pub fn readable(addr: usize, len: usize) -> bool {
    if addr < 0x10000 || addr.checked_add(len).is_none() {
        return false;
    }
    sys::readable(addr, len)
}

/// Reads a `T` at `addr` if the memory is readable (unaligned is fine).
///
/// # Safety
/// `T` must be valid for any bit pattern (integers, raw pointers). Off
/// Windows the range must be valid memory (nothing is checked there).
pub unsafe fn read<T: Copy>(addr: usize) -> Option<T> {
    if !readable(addr, size_of::<T>()) {
        return None;
    }
    // SAFETY: the range is readable (checked); T accepts any bits (caller).
    Some(unsafe { std::ptr::read_unaligned(addr as *const T) })
}

/// Copies `len` bytes from `addr` if readable.
///
/// # Safety
/// Off Windows the range must be valid memory.
pub unsafe fn read_bytes(addr: usize, len: usize) -> Option<Vec<u8>> {
    if len == 0 {
        return Some(Vec::new());
    }
    if !readable(addr, len) {
        return None;
    }
    // SAFETY: the range is readable (checked).
    Some(unsafe { std::slice::from_raw_parts(addr as *const u8, len) }.to_vec())
}

/// Reads the game `String` object at `s` (see the module docs). `None` if
/// any part is unreadable or the length is negative or over [`STRING_MAX`].
/// Invalid UTF-8 is replaced.
///
/// # Safety
/// Off Windows `s` and the string's bytes must be valid memory.
pub unsafe fn read_string(s: usize) -> Option<String> {
    // SAFETY: forwarded; every read is checked.
    unsafe {
        let len = read::<i32>(s.checked_add(STRING_LEN)?)?;
        let len = usize::try_from(len).ok().filter(|&l| l <= STRING_MAX)?;
        if len == 0 {
            return Some(String::new());
        }
        let data = read::<usize>(s + STRING_DATA)?;
        let bytes = read_bytes(data, len)?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(windows)]
mod sys {
    use core::ffi::c_void;

    #[repr(C)]
    struct MemoryBasicInformation {
        base_address: *mut c_void,
        allocation_base: *mut c_void,
        allocation_protect: u32,
        partition_id: u16,
        region_size: usize,
        state: u32,
        protect: u32,
        kind: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualQuery(
            addr: *const c_void,
            info: *mut MemoryBasicInformation,
            len: usize,
        ) -> usize;
    }

    const MEM_COMMIT: u32 = 0x1000;
    const PAGE_GUARD: u32 = 0x100;
    /// PAGE_READONLY, READWRITE, WRITECOPY, EXECUTE_READ, EXECUTE_READWRITE,
    /// EXECUTE_WRITECOPY.
    const READABLE: u32 = 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80;

    pub fn readable(addr: usize, len: usize) -> bool {
        let end = addr + len;
        let mut at = addr;
        while at < end {
            let mut mbi = std::mem::MaybeUninit::<MemoryBasicInformation>::zeroed();
            // SAFETY: VirtualQuery only writes the out struct, of the given size.
            let n = unsafe {
                VirtualQuery(
                    at as *const c_void,
                    mbi.as_mut_ptr(),
                    size_of::<MemoryBasicInformation>(),
                )
            };
            if n == 0 {
                return false;
            }
            // SAFETY: filled by VirtualQuery (and zeroed before).
            let mbi = unsafe { mbi.assume_init() };
            if mbi.state != MEM_COMMIT
                || mbi.protect & PAGE_GUARD != 0
                || mbi.protect & READABLE == 0
            {
                return false;
            }
            let region_end = (mbi.base_address as usize).saturating_add(mbi.region_size);
            if region_end <= at {
                return false;
            }
            at = region_end;
        }
        true
    }
}

#[cfg(not(windows))]
mod sys {
    pub fn readable(_addr: usize, _len: usize) -> bool {
        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fake game `String` pointing at `bytes` (which must outlive it).
    pub fn fake_string(bytes: &[u8]) -> [u8; 0x18] {
        let mut s = [0u8; 0x18];
        s[STRING_DATA..STRING_DATA + 8].copy_from_slice(&(bytes.as_ptr() as usize).to_le_bytes());
        s[STRING_LEN..STRING_LEN + 4].copy_from_slice(&(bytes.len() as i32).to_le_bytes());
        s
    }

    #[test]
    fn strings() {
        let text = b"save_1 and more".to_vec();
        let s = fake_string(&text[..6]);
        assert_eq!(
            unsafe { read_string(s.as_ptr() as usize) }.as_deref(),
            Some("save_1")
        );
        let mut neg = fake_string(&text);
        neg[STRING_LEN..STRING_LEN + 4].copy_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(unsafe { read_string(neg.as_ptr() as usize) }, None);
        let mut long = fake_string(&text);
        long[STRING_LEN..STRING_LEN + 4].copy_from_slice(&(STRING_MAX as i32 + 1).to_le_bytes());
        assert_eq!(unsafe { read_string(long.as_ptr() as usize) }, None);
        let empty = fake_string(&[]);
        assert_eq!(
            unsafe { read_string(empty.as_ptr() as usize) }.as_deref(),
            Some("")
        );
        assert_eq!(unsafe { read_string(0) }, None);
        assert!(!readable(0x10, 4));
        assert!(!readable(usize::MAX - 2, 8));
    }
}
