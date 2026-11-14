//! `borderless-compat`: borderless fullscreen always uses the game's
//! Compatibility mode (README, "Mod designs").
//!
//! The game has two borderless modes (Video settings → Borderless Mode).
//! Performance, the default, uses SDL's fullscreen-desktop window; with
//! OpenGL on Windows the driver then takes the screen over like exclusive
//! fullscreen (slow Alt+Tab, broken HDR). Compatibility uses the game's own
//! "emulated" borderless window, which stays a normal desktop window.
//!
//! The display-mode setter picks between them with `cmp [mode], 1; je
//! <emulated>`. The mod turns that `je rel32` into `nop; jmp rel32` with the
//! same target, so the emulated path is taken whatever the setting says.
//! Leaving the emulated mode is driven by the game's own state flag, so
//! switching to windowed or exclusive fullscreen still works. The patch
//! applies the next time the game sets the display mode (at start, or when
//! a video setting changes). Turning the mod off restores the six bytes.

use uk_qol_runtime::host::{LoadCtx, Mod};

pub struct BorderlessCompat;

/// `je rel32` opcode bytes expected at the patch site.
const JE_NEAR: [u8; 2] = [0x0F, 0x84];

/// The replacement for `0F 84 <rel32>`: `nop` then `jmp rel32` (E9). Both
/// end at the same address, so the same rel32 reaches the same target.
fn always_jump(original: &[u8; 6]) -> [u8; 6] {
    let mut out = *original;
    out[0] = 0x90;
    out[1] = 0xE9;
    out
}

impl Mod for BorderlessCompat {
    fn id(&self) -> &'static str {
        "borderless-compat"
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
            .get("borderless_choice")
            .ok_or("pattern borderless_choice not resolved")?;
        let start = rva as usize;
        let at: [u8; 6] = cx
            .image
            .bytes
            .get(start..start + 6)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| format!("borderless check out of range (rva {rva:#x})"))?;
        if at[..2] != JE_NEAR {
            return Err(format!(
                "unexpected code at the borderless check (rva {rva:#x}: {:02x?}, expected je)",
                &at[..2]
            ));
        }
        // SAFETY: `rva` is a resolved, verified `je rel32` in the exe's code.
        // The display-mode setter runs on the game's main thread only when the
        // mode is (re)applied, and the patch keeps the instruction boundary
        // after it (both versions are 6 bytes ending at the same address).
        unsafe { cx.hooks.patch(cx.base + start, &always_jump(&at)) }.map_err(|e| e.to_string())
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(BorderlessCompat)
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

    #[test]
    fn je_becomes_nop_jmp_with_same_target() {
        // je +0x44e (from the end of the 6-byte instruction).
        let je = [0x0F, 0x84, 0x4E, 0x04, 0x00, 0x00];
        assert_eq!(always_jump(&je), [0x90, 0xE9, 0x4E, 0x04, 0x00, 0x00]);
    }
}
