//! Hook layer: function detours and in-place byte patches, grouped per mod
//! so a mod's hooks are applied and removed as a unit (PLAN §2).
//!
//! # Detours
//! [`HookSet::detour`] copies the target's first whole instructions (at
//! least 5 bytes) into a trampoline, relocating RIP-relative operands and
//! relative branches ([`x64::relocate`]), and ends it with a jump back. On
//! [`HookSet::enable`] the target's first 5 bytes become `jmp rel32` to a
//! 16-byte relay (`jmp [rip+2]` and the detour's address) in a slot
//! allocated within ±2 GB of the target; the trampoline sits in the same
//! slot. If no memory is free near the target, the target gets a 14-byte
//! `jmp [rip+0]` straight to the detour instead, and the trampoline (placed
//! anywhere) can only relocate RIP-relative operands it can still reach.
//!
//! # Byte patches
//! [`HookSet::patch`] saves the original bytes when registered; enable writes
//! the new ones and disable puts the originals back.
//!
//! # Writing and other threads
//! Every write goes through `VirtualProtect` (restored afterwards) and
//! `FlushInstructionCache`. Other threads are **not** suspended. A write
//! that fits in one aligned 8-byte word (most 5-byte jumps when the target
//! is suitably placed, every relay retarget) is one atomic store, so a thread
//! running there sees either the old or the new bytes. Otherwise a thread
//! executing exactly those bytes at that instant could see a torn
//! instruction; a thread *inside* the replaced prologue (past its first
//! instruction) when the jump lands continues into the jump's tail. Hooks
//! are applied at load, mostly before the game reaches the hooked code, which
//! keeps this window small.
//!
//! Before enabling, the current bytes must still equal the saved originals
//! (else `HookError::Busy`). On disable, if a detour's jump was overwritten
//! by someone else (the Steam overlay, MangoHud and similar tools hook too),
//! the bytes are left alone and the relay is pointed at the trampoline
//! instead, so their hook keeps working and ours stops calling the detour.
//! Slots are never freed (a thread may still be inside a trampoline).
//!
//! Detour bodies must contain panics: see [`crate::guard::call_or`].

pub mod x64;

mod sys;

use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub enum HookError {
    /// Not implemented on this platform (non-Windows builds).
    Unsupported,
    /// The target's first instructions can't be relocated into a trampoline.
    Relocate(String),
    /// VirtualProtect / VirtualAlloc failed.
    Memory(String),
    /// The address is already hooked or patched by another set, or its bytes
    /// changed since the hook was registered.
    Busy(usize),
}

impl fmt::Display for HookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HookError::Unsupported => write!(f, "hooks are not supported on this platform"),
            HookError::Relocate(m) => write!(f, "cannot relocate target code: {m}"),
            HookError::Memory(m) => write!(f, "memory protection failed: {m}"),
            HookError::Busy(a) => write!(f, "address {a:#x} is already hooked"),
        }
    }
}

impl std::error::Error for HookError {}

/// Relay: `jmp [rip+2]`, two pad bytes, then the 8-aligned destination.
const RELAY_LEN: usize = 16;
const RELAY_TARGET_OFF: usize = 8;

/// A byte range one set has claimed, so no two sets touch the same code.
struct Claim {
    start: usize,
    end: usize,
    set: u64,
}

/// All claims; also serializes every hook write.
static CLAIMS: Mutex<Vec<Claim>> = Mutex::new(Vec::new());
static NEXT_SET: AtomicU64 = AtomicU64::new(1);

fn claims() -> std::sync::MutexGuard<'static, Vec<Claim>> {
    CLAIMS.lock().unwrap_or_else(|e| e.into_inner())
}

fn overlaps(claims: &[Claim], start: usize, end: usize) -> bool {
    claims.iter().any(|c| c.start < end && start < c.end)
}

/// One registered change: `new` replaces `old` at `addr` while enabled.
struct Write {
    addr: usize,
    new: Vec<u8>,
    old: Vec<u8>,
    /// For detours through a relay: (relay address, detour, trampoline).
    relay: Option<(usize, usize, usize)>,
}

/// The hooks and patches of one mod. Nothing is written to game memory until
/// [`HookSet::enable`]; [`HookSet::disable`] restores the original bytes.
/// Dropping an enabled set disables it.
pub struct HookSet {
    owner: String,
    id: u64,
    writes: Vec<Write>,
    enabled: bool,
}

impl HookSet {
    pub fn new(owner: &str) -> HookSet {
        HookSet {
            owner: owner.to_owned(),
            id: NEXT_SET.fetch_add(1, Ordering::Relaxed),
            writes: Vec::new(),
            enabled: false,
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Registers a detour of the function at `target` to `detour`. Returns the
    /// trampoline address: calling it runs the original function. The
    /// trampoline is valid as soon as this returns, before `enable`. If the
    /// set is already enabled, the detour is written immediately.
    ///
    /// # Safety
    /// `target` must be the start of a function in executable memory and
    /// `detour` a function with the same ABI and signature.
    pub unsafe fn detour(&mut self, target: usize, detour: usize) -> Result<usize, HookError> {
        // SAFETY: forwarded from the caller.
        unsafe { self.detour_with(target, detour, false) }
    }

    /// `detour`, optionally forcing the 14-byte far jump (for tests).
    unsafe fn detour_with(
        &mut self,
        target: usize,
        detour: usize,
        force_far: bool,
    ) -> Result<usize, HookError> {
        if target == 0 || detour == 0 {
            return Err(HookError::Memory("null hook address".into()));
        }
        let mut claims = claims();
        if overlaps(&claims, target, target + 1) {
            return Err(HookError::Busy(target));
        }
        // Enough for the longest prologue a 14-byte jump needs.
        let code = sys::read_code(target, 64)?;
        let near = if force_far {
            None
        } else {
            sys::alloc_slot(Some(target))?
        };
        let (jmp, slot_bytes, tramp, consumed, relay) = match near {
            Some(slot) => {
                let tramp = slot + RELAY_LEN;
                let r = x64::relocate(&code, target as u64, tramp as u64, x64::JMP_REL32_LEN)?;
                let jmp = x64::jmp_rel32(target as u64, slot as u64)
                    .ok_or_else(|| HookError::Memory("relay slot out of rel32 range".into()))?;
                let mut bytes = relay_bytes(detour).to_vec();
                bytes.extend_from_slice(&r.code);
                (
                    jmp.to_vec(),
                    bytes,
                    tramp,
                    r.consumed,
                    Some((slot, detour, tramp)),
                )
            }
            None => {
                let slot = sys::alloc_slot(None)?
                    .ok_or_else(|| HookError::Memory("no memory for a trampoline".into()))?;
                let r = x64::relocate(&code, target as u64, slot as u64, x64::JMP_ABS_LEN)?;
                (
                    x64::jmp_abs(detour as u64).to_vec(),
                    r.code,
                    slot,
                    r.consumed,
                    None,
                )
            }
        };
        if slot_bytes.len() > sys::SLOT {
            return Err(HookError::Relocate(format!(
                "trampoline needs {} bytes, slots hold {}",
                slot_bytes.len(),
                sys::SLOT
            )));
        }
        if overlaps(&claims, target, target + consumed) {
            return Err(HookError::Busy(target));
        }
        let slot = tramp - if relay.is_some() { RELAY_LEN } else { 0 };
        // SAFETY: the slot is ours, freshly allocated and SLOT bytes long.
        unsafe { sys::write(slot, &slot_bytes)? };
        let w = Write {
            addr: target,
            old: code[..jmp.len()].to_vec(),
            new: jmp,
            relay,
        };
        if self.enabled {
            // SAFETY: the claim check above makes the target ours.
            unsafe { apply(&w)? };
        }
        claims.push(Claim {
            start: target,
            end: target + consumed,
            set: self.id,
        });
        self.writes.push(w);
        Ok(tramp)
    }

    /// Registers an in-place patch of `bytes` at `addr`. If the set is already
    /// enabled, the patch is written immediately.
    ///
    /// # Safety
    /// `addr..addr+bytes.len()` must be mapped game code or data that no
    /// other thread executes mid-instruction while the patch is written.
    pub unsafe fn patch(&mut self, addr: usize, bytes: &[u8]) -> Result<(), HookError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut claims = claims();
        let end = addr + bytes.len();
        if overlaps(&claims, addr, end) {
            return Err(HookError::Busy(addr));
        }
        let w = Write {
            addr,
            old: sys::read(addr, bytes.len())?,
            new: bytes.to_vec(),
            relay: None,
        };
        if self.enabled {
            // SAFETY: as guaranteed by the caller; the range is unclaimed.
            unsafe { apply(&w)? };
        }
        claims.push(Claim {
            start: addr,
            end,
            set: self.id,
        });
        self.writes.push(w);
        Ok(())
    }

    /// Writes every registered detour and patch. If one fails, those already
    /// written are restored and the set stays disabled.
    pub fn enable(&mut self) -> Result<(), HookError> {
        if self.enabled {
            return Ok(());
        }
        let _claims = claims();
        for (i, w) in self.writes.iter().enumerate() {
            // SAFETY: every write's range is claimed by this set and was
            // checked at registration.
            if let Err(e) = unsafe { apply(w) } {
                for done in self.writes[..i].iter().rev() {
                    // SAFETY: as above; restoring our own bytes.
                    let _ = unsafe { restore(done, &self.owner) };
                }
                return Err(e);
            }
        }
        self.enabled = true;
        Ok(())
    }

    /// Restores the original bytes of every detour and patch. Returns the
    /// first error, after attempting every one; the set counts as disabled
    /// either way.
    pub fn disable(&mut self) -> Result<(), HookError> {
        if !self.enabled {
            return Ok(());
        }
        let _claims = claims();
        let mut first = None;
        for w in self.writes.iter().rev() {
            // SAFETY: as in `enable`.
            if let Err(e) = unsafe { restore(w, &self.owner) } {
                first.get_or_insert(e);
            }
        }
        self.enabled = false;
        first.map_or(Ok(()), Err)
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl Drop for HookSet {
    fn drop(&mut self) {
        let _ = self.disable();
        let id = self.id;
        claims().retain(|c| c.set != id);
    }
}

fn relay_bytes(to: usize) -> [u8; RELAY_LEN] {
    let mut b = [0xCC; RELAY_LEN];
    b[..6].copy_from_slice(&[0xFF, 0x25, 0x02, 0, 0, 0]);
    b[RELAY_TARGET_OFF..].copy_from_slice(&(to as u64).to_le_bytes());
    b
}

/// Writes `w.new` if the bytes are still the originals.
unsafe fn apply(w: &Write) -> Result<(), HookError> {
    let cur = sys::read(w.addr, w.old.len())?;
    if cur != w.old {
        return Err(HookError::Busy(w.addr));
    }
    // SAFETY: the relay is ours; the target range is claimed (callers).
    unsafe {
        if let Some((relay, detour, _)) = w.relay {
            sys::write(relay + RELAY_TARGET_OFF, &(detour as u64).to_le_bytes())?;
        }
        sys::write(w.addr, &w.new)
    }
}

/// Puts `w.old` back if our bytes are still there. If someone else wrote
/// over them, leaves theirs and (for relayed detours) bypasses the detour.
unsafe fn restore(w: &Write, owner: &str) -> Result<(), HookError> {
    let cur = sys::read(w.addr, w.new.len())?;
    if cur == w.old {
        return Ok(());
    }
    if cur == w.new {
        // SAFETY: our own bytes at a claimed range.
        return unsafe { sys::write(w.addr, &w.old) };
    }
    match w.relay {
        Some((relay, _, tramp)) => {
            crate::log(&format!(
                "hooks of {owner}: {:#x} was re-hooked by other software; \
                 leaving its jump and bypassing ours",
                w.addr
            ));
            // SAFETY: the relay is ours; an aligned 8-byte store.
            unsafe { sys::write(relay + RELAY_TARGET_OFF, &(tramp as u64).to_le_bytes()) }
        }
        None => {
            crate::log(&format!(
                "hooks of {owner}: bytes at {:#x} were changed by other software; left as they are",
                w.addr
            ));
            Err(HookError::Busy(w.addr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_jumps_to_its_qword() {
        let b = relay_bytes(0x1122_3344_5566_7788);
        // jmp [rip+2] at offset 0 reads offset 6 + 2.
        assert_eq!(&b[..6], &[0xFF, 0x25, 2, 0, 0, 0]);
        assert_eq!(
            u64::from_le_bytes(b[RELAY_TARGET_OFF..].try_into().unwrap()),
            0x1122_3344_5566_7788
        );
    }

    #[test]
    fn empty_set_enables_everywhere() {
        let mut s = HookSet::new("empty");
        assert_eq!(s.owner(), "empty");
        s.enable().unwrap();
        assert!(s.is_enabled());
        s.disable().unwrap();
        assert!(!s.is_enabled());
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_off_windows() {
        let mut s = HookSet::new("x");
        let f = relay_jumps_to_its_qword as fn() as usize;
        assert!(matches!(
            unsafe { s.detour(f, f) },
            Err(HookError::Unsupported)
        ));
        assert!(matches!(
            unsafe { s.patch(f, &[0x90]) },
            Err(HookError::Unsupported)
        ));
    }
}

#[cfg(all(test, windows))]
mod win_tests;
