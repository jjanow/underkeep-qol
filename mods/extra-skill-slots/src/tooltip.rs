//! The game's own skill tooltip for the extra slots' HUD buttons.
//!
//! The game shows one tooltip at a time: a "show" call arms it for the
//! current frame with a text and a position (`tooltip_show`), and the
//! tooltip pass near the end of the frame draws it after the hover delay;
//! a frame without a show call hides it. The HUD skill slot of each hero
//! (`hud_skill_slot`, called once per hero per frame from the portrait HUD,
//! before the tooltip pass) arms it while the slot is hovered, with the text
//! of the skill tooltip builder (`skill_tooltip`) at a fixed place left of
//! the dungeon view.
//!
//! The extra slots do the same from a detour of that function: the HUD pass
//! (egui, later in the frame) says which button is hovered with [`want`];
//! on the next frame the detour, on the call for that hero, builds the text
//! for the slot's skill with the game's builder, appends the slot's own
//! line (which slot, the clicks, "On cooldown") and arms the tooltip just as
//! the game's slot does. The text is built, shown and released every frame,
//! as the game does it (the show call keeps its own reference).
//!
//! weapon-swap detours the tooltip pass itself; this hooks the skill slot
//! instead, so the two mods don't need the same address.
//!
//! Everything here runs on the game thread; the mutex only satisfies Rust.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use uk_qol_runtime::{guard, hud};

use crate::game::{self, Game, Skill};
use crate::{ID, log};

/// The game's `String`: a reference-counted buffer (null for literals), the
/// bytes and their length (the padding covers what callees may assume).
#[repr(C)]
struct GString {
    buf: *mut i32,
    data: *const u8,
    len: i32,
    _pad: [u32; 3],
}

impl GString {
    fn empty() -> GString {
        GString {
            buf: std::ptr::null_mut(),
            data: c"".as_ptr().cast(),
            len: 0,
            _pad: [0; 3],
        }
    }

    /// A literal over `s`, which must end in a NUL and outlive the String.
    fn literal(s: &str) -> GString {
        let text = s.strip_suffix('\0').unwrap_or(s);
        GString {
            buf: std::ptr::null_mut(),
            data: s.as_ptr(),
            len: text.len() as i32,
            _pad: [0; 3],
        }
    }
}

type SlotFn = unsafe extern "system" fn(usize, usize, i32);
type ShowFn = unsafe extern "system" fn(*const GString, *const f32, u8);
type SkillTooltipFn =
    unsafe extern "system" fn(*mut GString, usize, usize, i32, u8) -> *mut GString;
type ConcatFn =
    unsafe extern "system" fn(*mut GString, *const GString, *const GString) -> *mut GString;
type FreeFn = unsafe extern "system" fn(*mut GString);

static ORIG_SLOT: AtomicUsize = AtomicUsize::new(0);
static SHOW: AtomicUsize = AtomicUsize::new(0);
static SKILL_TOOLTIP: AtomicUsize = AtomicUsize::new(0);
static WIDTH: AtomicUsize = AtomicUsize::new(0);
static CONCAT: AtomicUsize = AtomicUsize::new(0);
static FREE: AtomicUsize = AtomicUsize::new(0);
/// The detour is in and every address resolved.
static READY: AtomicBool = AtomicBool::new(false);

/// The patterns this needs (all optional: without them the buttons keep
/// their own hover text).
const PATTERNS: [&str; 6] = [
    "hud_skill_slot",
    "tooltip_show",
    "skill_tooltip",
    "g_skill_tooltip_width",
    "string_concat",
    "string_free",
];

/// Gap the game leaves between its skill tooltip and the dungeon view.
const GAP: f32 = 6.0;

/// A hovered button: whose, which slot, which skill.
#[derive(Clone)]
struct Want {
    hero: usize,
    slot: usize,
    skill: Skill,
}

struct Shared {
    game: Option<Game>,
    want: Option<Want>,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    game: None,
    want: None,
});

fn shared() -> std::sync::MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the game's tooltip is used for the buttons (otherwise they show
/// their own hover text).
pub fn active() -> bool {
    READY.load(Ordering::Relaxed)
}

/// From the HUD pass, every frame a button with a skill is hovered (the
/// next frame's call for `hero` takes it).
pub fn want(hero: usize, slot: usize, skill: &Skill) {
    shared().want = Some(Want {
        hero,
        slot,
        skill: skill.clone(),
    });
}

/// The game handle for the detour (`None` stops it acting).
pub fn set_game(game: Option<Game>) {
    let mut s = shared();
    s.game = game;
    s.want = None;
}

/// Forgets a hover (a save was loaded, the HUD went away).
pub fn clear() {
    shared().want = None;
}

/// Installs the skill slot detour if every pattern resolved (from
/// `on_load`).
///
/// # Safety
/// `at` must give the resolved address of each pattern in [`PATTERNS`].
pub unsafe fn install(
    hooks: &mut uk_qol_runtime::hook::HookSet,
    at: impl Fn(&str) -> Option<usize>,
) {
    let mut addrs = [0usize; PATTERNS.len()];
    for (i, name) in PATTERNS.iter().enumerate() {
        match at(name) {
            Some(a) => addrs[i] = a,
            None => {
                log(&format!(
                    "pattern {name} not found; the slots keep their own hover text"
                ));
                return;
            }
        }
    }
    let [slot, show, skill_tooltip, width, concat, free] = addrs;
    SHOW.store(show, Ordering::Relaxed);
    SKILL_TOOLTIP.store(skill_tooltip, Ordering::Relaxed);
    WIDTH.store(width, Ordering::Relaxed);
    CONCAT.store(concat, Ordering::Relaxed);
    FREE.store(free, Ordering::Relaxed);
    // SAFETY: the resolved start of the HUD skill slot function; the detour
    // keeps its arguments and passes them on unchanged.
    match unsafe { hooks.detour(slot, on_slot as SlotFn as usize) } {
        Ok(t) => {
            ORIG_SLOT.store(t, Ordering::Relaxed);
            READY.store(true, Ordering::Relaxed);
        }
        Err(e) => log(&format!(
            "couldn't hook the HUD skill slot ({e}); the slots keep their own hover text"
        )),
    }
}

/// The lines under the game's text, NUL-terminated. The game's text ends
/// without a line break, so they start with a blank line.
fn hint(slot: usize, on_cooldown: bool) -> String {
    let mut s = format!(
        "\n\n{{a:center}}{{color_text_darker}}Extra skill slot {} (mod). Left-click: cast. \
         Right-click: assign another skill.\n",
        slot + 1
    );
    if on_cooldown {
        s.push_str("{a:center}{color_text_darker}On cooldown.\n");
    }
    s.push('\0');
    s
}

/// Arms the game's tooltip for hero `hero`'s hovered extra slot, if any.
///
/// # Safety
/// Game thread, inside the frame (from the slot detour).
unsafe fn arm(hero: usize) {
    let (game, want) = {
        let mut s = shared();
        let Some(game) = s.game else { return };
        if s.want.as_ref().is_none_or(|w| w.hero != hero) {
            return;
        }
        (game, s.want.take())
    };
    let Some(w) = want else { return };
    let h = game.hero(hero);
    // SAFETY: inside the static hero array; game thread.
    let Some((state, kind, cooldown)) = (unsafe {
        game::hero_valid(h)
            .then(|| game::resolve(h, &w.skill))
            .flatten()
            .and_then(|p| game::tooltip_args(h, p).map(|(s, k)| (s, k, game::cooldown(h, p))))
    }) else {
        return;
    };
    let Some(view) = hud::gui_rect("dung_view_rect") else {
        return;
    };
    // SAFETY: the resolved functions and global with their signatures; the
    // hero and skill state are live (read this frame).
    unsafe {
        let width = game::rd::<f32>(WIDTH.load(Ordering::Relaxed));
        if !(0.0..=4000.0).contains(&width) {
            return;
        }
        let build: SkillTooltipFn = std::mem::transmute(SKILL_TOOLTIP.load(Ordering::Relaxed));
        let concat: ConcatFn = std::mem::transmute(CONCAT.load(Ordering::Relaxed));
        let free: FreeFn = std::mem::transmute(FREE.load(Ordering::Relaxed));
        let show: ShowFn = std::mem::transmute(SHOW.load(Ordering::Relaxed));
        let mut text = GString::empty();
        build(&mut text, h, state, kind, 1);
        let line = hint(w.slot, cooldown > 0);
        let line_s = GString::literal(&line);
        let mut joined = GString::empty();
        concat(&mut joined, &text, &line_s);
        free(&mut text);
        let pos = [view.min.x - width - GAP, view.min.y];
        show(&joined, pos.as_ptr(), 1);
        free(&mut joined);
    }
}

/// The game's HUD skill slot of hero `hero` (see `patterns.toml`).
unsafe extern "system" fn on_slot(a: usize, b: usize, hero: i32) {
    // SAFETY: set before the hook was enabled.
    let orig: SlotFn = unsafe { std::mem::transmute(ORIG_SLOT.load(Ordering::Relaxed)) };
    // SAFETY: the original with the game's arguments.
    unsafe { orig(a, b, hero) };
    if let Ok(hero) = usize::try_from(hero)
        && hero < game::HEROES
    {
        // SAFETY: game thread, inside the frame.
        guard::call(ID, || unsafe { arm(hero) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_lines() {
        let h = hint(0, false);
        assert!(h.starts_with("\n\n"), "{h}");
        assert!(h.contains("Extra skill slot 1 (mod)"), "{h}");
        assert!(!h.contains("On cooldown"));
        assert!(h.ends_with("\n\0"));
        assert!(hint(2, true).contains("On cooldown."));
    }
}
