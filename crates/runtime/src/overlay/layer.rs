//! Draws the mods' HUD widgets inside the game's frame, under its mouse
//! cursor and the item carried on it.
//!
//! The game draws its UI into an offscreen frame in three layers (the
//! panels, popups, and a top layer with the item being dragged), copies
//! the frame to the window, and only then draws its own mouse cursor (a
//! sprite, `cursor1`..`cursor3`). Drawn at `SDL_GL_SwapWindow`, the HUD
//! widgets would cover both. So the HUD is drawn when the game submits its
//! top UI layer: above the game's panels, below the carried item and the
//! cursor. The menu and toasts still draw at swap time, over everything.
//!
//! Two detours, both from the `ui_layers_draw` pattern (core.toml):
//! `ui_layers_draw(ui)` itself, which records the UI context for the length
//! of the call, and the render device's list submission (the function's
//! tail jump), which draws the HUD right before the context's top layer.
//! Without the pattern the HUD stays at swap time and the overlay redraws
//! the game's cursor over its widgets instead (see `win.rs`), the sprite
//! picked by the game's `cursor` setting ([`cursor_sprite`]).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::hook::HookSet;

/// `ui_layers_draw`: offsets in the pattern of the top layer's
/// `mov rdx, [rbx + disp32]` displacement and of the tail `jmp rel32`.
const TOP_LAYER_DISP: usize = 0x60;
const TAIL_JMP: usize = 0x70;

static ORIG_LAYERS: AtomicUsize = AtomicUsize::new(0);
static ORIG_SUBMIT: AtomicUsize = AtomicUsize::new(0);
/// Offset of the top layer's list pointer in the UI context.
static TOP_LAYER: AtomicUsize = AtomicUsize::new(0);
/// The UI context while `ui_layers_draw` runs, else 0.
static CURRENT: AtomicUsize = AtomicUsize::new(0);
/// The `cursor` setting's cvar index and the cvar table pointer (globals).
static CURSOR_CVAR: AtomicUsize = AtomicUsize::new(0);
static CVARS: AtomicUsize = AtomicUsize::new(0);
/// A cvar's size in the table and its value's offset.
#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows glue
const CVAR_SIZE: usize = 0x80;
#[cfg_attr(not(windows), allow(dead_code))]
const CVAR_VALUE: usize = 0x70;

type LayersFn = unsafe extern "system" fn(usize);
type SubmitFn = unsafe extern "system" fn(usize, usize);

/// What the `ui_layers_draw` pattern gives: the top layer's offset in the
/// UI context and the submit function's address, from the function's code
/// at `func` (`code` is its bytes, at least `TAIL_JMP + 5` long).
pub fn parse(func: usize, code: &[u8]) -> Option<(usize, usize)> {
    let d = |at: usize| -> Option<i32> {
        Some(i32::from_le_bytes(code.get(at..at + 4)?.try_into().ok()?))
    };
    if code.get(TOP_LAYER_DISP - 3..TOP_LAYER_DISP) != Some(&[0x48, 0x8B, 0x93])
        || code.get(TAIL_JMP) != Some(&0xE9)
    {
        return None;
    }
    let top = usize::try_from(d(TOP_LAYER_DISP)?).ok()?;
    let rel = d(TAIL_JMP + 1)? as isize;
    let submit = (func + TAIL_JMP + 5).checked_add_signed(rel)?;
    Some((top, submit))
}

/// The game's cursor sprite for its `cursor` setting (0..2: `cursor1`..
/// `cursor3`; 3 is the system cursor: `None`). `cursor1` if the setting
/// can't be read.
#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows glue
pub fn cursor_sprite() -> Option<&'static str> {
    let (cvar, cvars) = (
        CURSOR_CVAR.load(Ordering::Relaxed),
        CVARS.load(Ordering::Relaxed),
    );
    let value = (cvar != 0 && cvars != 0)
        .then(|| {
            // SAFETY: globals of the exe and the table they point at; every
            // read is checked.
            unsafe {
                let i = crate::game::read::<i32>(cvar)?;
                let table = crate::game::read::<usize>(cvars)?;
                let i = usize::try_from(i).ok().filter(|&i| i < 4096)?;
                crate::game::read::<i32>(table + i * CVAR_SIZE + CVAR_VALUE)
            }
        })
        .flatten();
    match value {
        Some(1) => Some("cursor2"),
        Some(2) => Some("cursor3"),
        Some(3) => None,
        _ => Some("cursor1"),
    }
}

/// Registers the two detours in `hooks` (the core set). Logs and leaves
/// the HUD at swap time if the pattern is missing.
pub fn install(hooks: &mut HookSet, patterns: &BTreeMap<String, u32>, base: usize) {
    if let (Some(&c), Some(&t)) = (patterns.get("g_cursor_cvar"), patterns.get("g_cvars")) {
        CURSOR_CVAR.store(base + c as usize, Ordering::Relaxed);
        CVARS.store(base + t as usize, Ordering::Relaxed);
    }
    let Some(&rva) = patterns.get("ui_layers_draw") else {
        crate::log("overlay: ui_layers_draw not found; HUD widgets draw over the game's cursor");
        return;
    };
    let func = base + rva as usize;
    // SAFETY: the pattern matched code of the running exe.
    let parsed =
        unsafe { crate::game::read_bytes(func, TAIL_JMP + 5) }.and_then(|code| parse(func, &code));
    let Some((top, submit)) = parsed else {
        crate::log(
            "overlay: ui_layers_draw doesn't look as expected; HUD widgets draw over the game's cursor",
        );
        return;
    };
    TOP_LAYER.store(top, Ordering::Relaxed);
    // SAFETY: both are function entries in the exe (the pattern and its
    // tail jump); the detours have their signatures.
    let r = unsafe {
        hooks
            .detour(func, layers as LayersFn as usize)
            .and_then(|t| {
                ORIG_LAYERS.store(t, Ordering::Release);
                hooks.detour(submit, submit_list as SubmitFn as usize)
            })
    };
    match r {
        Ok(t) => {
            ORIG_SUBMIT.store(t, Ordering::Release);
            crate::log(&format!(
                "overlay: HUD widgets draw under the game's cursor (UI layer +{top:#x}, submit at rva {:#x})",
                submit - base
            ));
        }
        Err(e) => crate::log(&format!(
            "overlay: can't hook the UI layers ({e}); HUD widgets draw over the game's cursor"
        )),
    }
}

/// `ui_layers_draw(ui)`: remembers `ui` while the layers are submitted.
extern "system" fn layers(ui: usize) {
    // SAFETY: the trampoline, stored before the hooks were enabled.
    let orig: LayersFn = unsafe { std::mem::transmute(ORIG_LAYERS.load(Ordering::Acquire)) };
    CURRENT.store(ui, Ordering::Relaxed);
    // SAFETY: the game's own call, forwarded.
    unsafe { orig(ui) };
    CURRENT.store(0, Ordering::Relaxed);
}

/// The render device's `submit(device, list)`: draws the HUD right before
/// the UI context's top layer.
extern "system" fn submit_list(device: usize, list: usize) {
    // SAFETY: the trampoline, stored before the hooks were enabled.
    let orig: SubmitFn = unsafe { std::mem::transmute(ORIG_SUBMIT.load(Ordering::Acquire)) };
    let ui = CURRENT.load(Ordering::Relaxed);
    if ui != 0 && list != 0 {
        let top = TOP_LAYER.load(Ordering::Relaxed);
        // SAFETY: a field of the game's live UI context; the read is checked.
        if unsafe { crate::game::read::<usize>(ui + top) } == Some(list) {
            #[cfg(windows)]
            super::win::hud_in_frame();
        }
    }
    // SAFETY: the game's own call, forwarded.
    unsafe { orig(device, list) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ui_layers_draw` of game 1.0.5 (at 0x1401615a0; submit at 0x1400752e0).
    const CODE: &str = "40 53 48 83 EC 20 80 B9 C1 00 00 00 00 48 8B D9 74 25 48 8B 81 78 07 00 00 \
        C6 00 00 48 8B 81 D8 06 00 00 C6 00 00 48 8B 81 38 06 00 00 C6 00 00 C6 81 C1 00 00 00 00 \
        48 8B 91 38 06 00 00 48 8B 0D B3 0F 2E 00 E8 F6 3C F1 FF 48 8B 93 D8 06 00 00 48 8B 0D A0 \
        0F 2E 00 E8 E3 3C F1 FF 48 8B 93 78 07 00 00 48 8B 0D 8D 0F 2E 00 48 83 C4 20 5B E9 CB 3C \
        F1 FF";

    fn bytes() -> Vec<u8> {
        CODE.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    #[test]
    fn parses_top_layer_and_submit() {
        assert_eq!(parse(0x1401615a0, &bytes()), Some((0x778, 0x1400752e0)));
    }

    #[test]
    fn rejects_other_code() {
        let mut b = bytes();
        b[TAIL_JMP] = 0xE8;
        assert_eq!(parse(0x1401615a0, &b), None);
        assert_eq!(parse(0x1401615a0, &b[..0x40]), None);
    }

    #[test]
    fn pattern_matches_the_sample() {
        let f = crate::host::PatternFile::parse(crate::corehooks::PATTERNS).unwrap();
        let def = f
            .patterns
            .iter()
            .find(|p| p.name == "ui_layers_draw")
            .unwrap();
        assert!(def.candidates[0].pattern.matches_at(&bytes(), 0));
    }
}
