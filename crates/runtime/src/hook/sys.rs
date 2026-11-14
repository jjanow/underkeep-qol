//! Live-memory access for the hook layer: reading and writing code, and the
//! executable slots that hold relays and trampolines. Windows only; elsewhere
//! every call returns `HookError::Unsupported`.

#[cfg(windows)]
pub(super) use win::*;

#[cfg(not(windows))]
pub(super) use other::*;

/// Size of one relay + trampoline slot.
pub(super) const SLOT: usize = 128;

#[cfg(not(windows))]
mod other {
    use crate::hook::HookError;

    pub(in crate::hook) fn read(_addr: usize, _len: usize) -> Result<Vec<u8>, HookError> {
        Err(HookError::Unsupported)
    }

    pub(in crate::hook) fn read_code(_addr: usize, _max: usize) -> Result<Vec<u8>, HookError> {
        Err(HookError::Unsupported)
    }

    pub(in crate::hook) unsafe fn write(_addr: usize, _bytes: &[u8]) -> Result<(), HookError> {
        Err(HookError::Unsupported)
    }

    pub(in crate::hook) fn alloc_slot(_near: Option<usize>) -> Result<Option<usize>, HookError> {
        Err(HookError::Unsupported)
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::SLOT;
    use crate::hook::HookError;

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_FREE: u32 = 0x10000;
    const PAGE_NOACCESS: u32 = 0x01;
    const PAGE_GUARD: u32 = 0x100;
    const PAGE_EXECUTE_ANY: u32 = 0x10 | 0x20 | 0x40 | 0x80;
    const PAGE_EXECUTE_READ: u32 = 0x20;
    const PAGE_EXECUTE_READWRITE: u32 = 0x40;

    /// Allocation granularity on every Windows version and Wine.
    const GRANULARITY: usize = 0x10000;
    /// One slab of slots.
    const SLAB: usize = GRANULARITY;
    /// How far a slab may be from the target, leaving margin under ±2 GB so
    /// both rel32 jumps into it and RIP-relative data seen from it reach.
    const MAX_DIST: usize = 0x7FF0_0000;
    const MIN_ADDR: usize = 0x10000;

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
        fn VirtualAlloc(addr: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
        fn VirtualProtect(addr: *mut c_void, size: usize, new: u32, old: *mut u32) -> i32;
        fn VirtualQuery(
            addr: *const c_void,
            info: *mut MemoryBasicInformation,
            len: usize,
        ) -> usize;
        fn FlushInstructionCache(process: *mut c_void, addr: *const c_void, size: usize) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn GetLastError() -> u32;
    }

    fn query(addr: usize) -> Option<MemoryBasicInformation> {
        // SAFETY: VirtualQuery only inspects the address space.
        unsafe {
            let mut mbi: MemoryBasicInformation = std::mem::zeroed();
            let n = VirtualQuery(
                addr as *const c_void,
                &mut mbi,
                size_of::<MemoryBasicInformation>(),
            );
            (n != 0).then_some(mbi)
        }
    }

    /// Bytes readable at `addr` without faulting, up to `max`.
    fn readable(addr: usize, max: usize) -> Result<(usize, u32), HookError> {
        let mbi =
            query(addr).ok_or_else(|| HookError::Memory(format!("{addr:#x} is not mapped")))?;
        if mbi.state != MEM_COMMIT || mbi.protect & (PAGE_NOACCESS | PAGE_GUARD) != 0 {
            return Err(HookError::Memory(format!("{addr:#x} is not readable")));
        }
        let end = mbi.base_address as usize + mbi.region_size;
        Ok(((end - addr).min(max), mbi.protect))
    }

    /// Exactly `len` bytes at `addr`.
    pub(in crate::hook) fn read(addr: usize, len: usize) -> Result<Vec<u8>, HookError> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let at = addr + out.len();
            let (n, _) = readable(at, len - out.len())?;
            // SAFETY: `n` bytes at `at` are committed and readable.
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(at as *const u8, n) });
        }
        Ok(out)
    }

    /// Up to `max` bytes of executable code at `addr` (fewer at the end of
    /// the region).
    pub(in crate::hook) fn read_code(addr: usize, max: usize) -> Result<Vec<u8>, HookError> {
        let (n, protect) = readable(addr, max)?;
        if protect & PAGE_EXECUTE_ANY == 0 {
            return Err(HookError::Memory(format!("{addr:#x} is not executable")));
        }
        // SAFETY: as in `read`.
        Ok(unsafe { std::slice::from_raw_parts(addr as *const u8, n) }.to_vec())
    }

    /// Writes `bytes` at `addr` (code or data), restoring the page protection
    /// and flushing the instruction cache. A write that fits inside one
    /// aligned qword is a single atomic store, so another thread executing
    /// there sees the old or the new bytes, never a mix.
    ///
    /// # Safety
    /// `addr..addr+bytes.len()` must be mapped and safe to change.
    pub(in crate::hook) unsafe fn write(addr: usize, bytes: &[u8]) -> Result<(), HookError> {
        let len = bytes.len();
        if len == 0 {
            return Ok(());
        }
        let mut old = 0u32;
        // SAFETY: the caller guarantees the range is mapped.
        unsafe {
            if VirtualProtect(addr as *mut c_void, len, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
                return Err(HookError::Memory(format!(
                    "VirtualProtect({addr:#x}, {len}) failed: error {}",
                    GetLastError()
                )));
            }
            let q = addr & !7;
            if addr + len <= q + 8 {
                let cell = &*(q as *const AtomicU64);
                let off = addr - q;
                let mut cur = cell.load(Ordering::SeqCst);
                loop {
                    let mut b = cur.to_le_bytes();
                    b[off..off + len].copy_from_slice(bytes);
                    match cell.compare_exchange(
                        cur,
                        u64::from_le_bytes(b),
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(_) => break,
                        Err(now) => cur = now,
                    }
                }
            } else {
                ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, len);
            }
            let mut ignored = 0u32;
            VirtualProtect(addr as *mut c_void, len, old, &mut ignored);
            FlushInstructionCache(GetCurrentProcess(), addr as *const c_void, len);
        }
        Ok(())
    }

    struct Slab {
        base: usize,
        used: usize,
    }

    static SLABS: Mutex<Vec<Slab>> = Mutex::new(Vec::new());

    fn near(a: usize, target: usize) -> bool {
        a.abs_diff(target) < MAX_DIST && (a + SLOT).abs_diff(target) < MAX_DIST
    }

    fn try_alloc(at: usize) -> Option<usize> {
        // SAFETY: allocating fresh pages; failure is a null return.
        let p = unsafe {
            VirtualAlloc(
                at as *mut c_void,
                SLAB,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_EXECUTE_READ,
            )
        };
        (!p.is_null()).then_some(p as usize)
    }

    /// A free 64 KB block within `MAX_DIST` of `target`: nearest below
    /// first, then above.
    fn alloc_near(target: usize) -> Option<usize> {
        let lo = target.saturating_sub(MAX_DIST).max(MIN_ADDR);
        let hi = target.saturating_add(MAX_DIST);
        let mut a = (target & !(GRANULARITY - 1)).checked_sub(GRANULARITY)?;
        while a >= lo {
            let mbi = query(a)?;
            if mbi.state == MEM_FREE {
                if let Some(p) = try_alloc(a) {
                    return Some(p);
                }
                a = a.checked_sub(GRANULARITY)?;
            } else {
                a = (mbi.allocation_base as usize & !(GRANULARITY - 1)).checked_sub(GRANULARITY)?;
            }
        }
        let mut a = (target & !(GRANULARITY - 1)) + GRANULARITY;
        while a + SLAB <= hi {
            let Some(mbi) = query(a) else { break };
            if mbi.state == MEM_FREE {
                if let Some(p) = try_alloc(a) {
                    return Some(p);
                }
                a += GRANULARITY;
            } else {
                let end = mbi.base_address as usize + mbi.region_size;
                a = (end + GRANULARITY - 1) & !(GRANULARITY - 1);
            }
        }
        None
    }

    /// A `SLOT`-byte executable slot (read-execute; write it with `write`).
    /// With `near`, the slot is within rel32 reach of that address, or
    /// `Ok(None)` if no memory is free there. Slots are never freed: a thread
    /// may still be running in a relay or trampoline after its hook is gone.
    pub(in crate::hook) fn alloc_slot(near_to: Option<usize>) -> Result<Option<usize>, HookError> {
        let mut slabs = SLABS.lock().unwrap_or_else(|e| e.into_inner());
        for s in slabs.iter_mut() {
            let a = s.base + s.used;
            if s.used + SLOT <= SLAB && near_to.is_none_or(|t| near(a, t)) {
                s.used += SLOT;
                return Ok(Some(a));
            }
        }
        let base = match near_to {
            Some(t) => match alloc_near(t) {
                Some(b) if near(b, t) => b,
                Some(b) => {
                    // Usable for far slots later.
                    slabs.push(Slab { base: b, used: 0 });
                    return Ok(None);
                }
                None => return Ok(None),
            },
            None => try_alloc(0).ok_or_else(|| {
                // SAFETY: plain FFI call.
                HookError::Memory(format!("VirtualAlloc failed: error {}", unsafe {
                    GetLastError()
                }))
            })?,
        };
        slabs.push(Slab { base, used: SLOT });
        Ok(Some(base))
    }
}
