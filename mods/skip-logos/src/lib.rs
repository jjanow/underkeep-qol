//! `skip-logos`: starts the game on the main menu without the startup logos
//! (README, "Mod designs").
//!
//! The game's per-frame update keeps drawing the logo animation while it is
//! active and, once it ends, takes its own hand-over to the main menu (or to
//! the intro cinematic on the very first run). The mod removes the "still
//! active" branch (a 2-byte `jne` becomes two `nop`s), so the first frame
//! takes the hand-over. Nothing else changes: the first-run intro check is
//! the game's own. Turning the mod off restores the two bytes.

use uk_qol_runtime::host::{LoadCtx, Mod};

pub struct SkipLogos;

/// `jne rel8` opcode expected at the patch site.
const JNE_SHORT: u8 = 0x75;

impl Mod for SkipLogos {
    fn id(&self) -> &'static str {
        "skip-logos"
    }

    fn manifest(&self) -> &'static str {
        include_str!("../mod.toml")
    }

    fn patterns(&self) -> &'static str {
        include_str!("../patterns.toml")
    }

    fn on_load(&mut self, cx: &mut LoadCtx) -> Result<(), String> {
        let rva = *cx
            .patterns
            .get("logo_wait")
            .ok_or("pattern logo_wait not resolved")?;
        let at = cx.image.bytes.get(rva as usize).copied();
        if at != Some(JNE_SHORT) {
            return Err(format!(
                "unexpected code at the logo check (rva {rva:#x}: {at:02x?}, expected jne)"
            ));
        }
        // SAFETY: `rva` is a resolved, verified `jne rel8` in the exe's code.
        // Mods load while the game waits at its first winmm call, before the
        // first frame, so no thread is executing these bytes.
        unsafe { cx.hooks.patch(cx.base + rva as usize, &[0x90, 0x90]) }.map_err(|e| e.to_string())
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(SkipLogos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uk_qol_runtime::host::{Manifest, PatternFile};

    #[test]
    fn manifest_and_patterns_parse() {
        let m = new();
        let manifest = Manifest::parse(m.manifest()).unwrap();
        assert_eq!(manifest.id, m.id());
        assert!(manifest.settings.is_empty());
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 1);
    }
}
