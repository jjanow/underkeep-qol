//! Hook layer: function detours and in-place byte patches, grouped per mod
//! so a mod's hooks are applied and removed as a unit (PLAN §2).
//!
//! Interface fixed; implementation pending.

use std::fmt;

#[derive(Debug)]
pub enum HookError {
    /// Not implemented on this platform (non-Windows builds).
    Unsupported,
    /// The target's first instructions can't be relocated into a trampoline.
    Relocate(String),
    /// VirtualProtect / VirtualAlloc failed.
    Memory(String),
    /// The address is already hooked or patched by another set.
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

/// The hooks and patches of one mod. Nothing is written to game memory until
/// [`HookSet::enable`]; [`HookSet::disable`] restores the original bytes.
/// Dropping an enabled set disables it.
pub struct HookSet {
    owner: String,
}

impl HookSet {
    pub fn new(owner: &str) -> HookSet {
        HookSet {
            owner: owner.to_owned(),
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Registers a detour of the function at `target` to `detour`. Returns the
    /// trampoline address: calling it runs the original function. The
    /// trampoline is valid as soon as this returns, before `enable`.
    ///
    /// # Safety
    /// `target` must be the start of a function in executable memory and
    /// `detour` a function with the same ABI and signature.
    pub unsafe fn detour(&mut self, target: usize, detour: usize) -> Result<usize, HookError> {
        let _ = (target, detour);
        Err(HookError::Unsupported)
    }

    /// Registers an in-place patch of `bytes` at `addr`.
    ///
    /// # Safety
    /// `addr..addr+bytes.len()` must be mapped game code or data that no
    /// other thread executes mid-instruction while the patch is written.
    pub unsafe fn patch(&mut self, addr: usize, bytes: &[u8]) -> Result<(), HookError> {
        let _ = (addr, bytes);
        Err(HookError::Unsupported)
    }

    /// Writes every registered detour and patch.
    pub fn enable(&mut self) -> Result<(), HookError> {
        Err(HookError::Unsupported)
    }

    /// Restores the original bytes of every detour and patch.
    pub fn disable(&mut self) -> Result<(), HookError> {
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        false
    }
}
