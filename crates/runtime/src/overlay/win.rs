//! Windows side of the overlay: SDL imports hooked through the exe's IAT,
//! SDL functions resolved from `SDL3.dll`, and the egui frame on the game's
//! GL context.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use egui_glow::glow;

use super::gl::{self, Saved};
use super::router::{Env, Router, Toggle, Verdict};
use super::sdl::{self, Event, Input};
use super::ui::{self, UiState};
use super::{OPEN, host, sink};
use crate::hotkey::{PAD_LT, PAD_RT, pad_bit};
use crate::iat::IatHook;
use crate::input::InputEvent;
use crate::{guard, i18n};

/// Panics in overlay code are contained under this id; once it has failed,
/// every hook is a plain pass-through.
const GUARD_ID: &str = "overlay";

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

type Window = *mut c_void;
type SwapFn = unsafe extern "C" fn(Window) -> bool;
type PollFn = unsafe extern "C" fn(*mut Event) -> bool;
type ButtonFn = unsafe extern "C" fn(*mut c_void, c_int) -> bool;
type AxisFn = unsafe extern "C" fn(*mut c_void, c_int) -> i16;

#[repr(C)]
struct Locale {
    language: *const c_char,
    country: *const c_char,
}

/// SDL functions the overlay calls itself (not hooked).
struct Api {
    gl_get_proc_address: unsafe extern "C" fn(*const c_char) -> *const c_void,
    gl_get_current_context: unsafe extern "C" fn() -> *mut c_void,
    get_window_size: unsafe extern "C" fn(Window, *mut c_int, *mut c_int) -> bool,
    get_window_size_in_pixels: unsafe extern "C" fn(Window, *mut c_int, *mut c_int) -> bool,
    get_keyboard_focus: unsafe extern "C" fn() -> Window,
    text_input_active: unsafe extern "C" fn(Window) -> bool,
    start_text_input: unsafe extern "C" fn(Window) -> bool,
    stop_text_input: unsafe extern "C" fn(Window) -> bool,
    cursor_visible: unsafe extern "C" fn() -> bool,
    show_cursor: unsafe extern "C" fn() -> bool,
    hide_cursor: unsafe extern "C" fn() -> bool,
    get_preferred_locales: unsafe extern "C" fn(*mut c_int) -> *mut *mut Locale,
    free: unsafe extern "C" fn(*mut c_void),
}

impl Api {
    #[allow(clippy::missing_transmute_annotations)] // each target type is the field type
    fn resolve(sdl: *mut c_void) -> Result<Api, String> {
        macro_rules! f {
            ($name:literal) => {{
                // SAFETY: `sdl` is the loaded SDL3.dll; the name is NUL-terminated.
                let p = unsafe { GetProcAddress(sdl, concat!($name, "\0").as_ptr()) };
                if p.is_null() {
                    return Err(format!("SDL3.dll has no {}", $name));
                }
                // SAFETY: signature per the SDL 3.4 headers.
                unsafe { std::mem::transmute::<*mut c_void, _>(p) }
            }};
        }
        Ok(Api {
            gl_get_proc_address: f!("SDL_GL_GetProcAddress"),
            gl_get_current_context: f!("SDL_GL_GetCurrentContext"),
            get_window_size: f!("SDL_GetWindowSize"),
            get_window_size_in_pixels: f!("SDL_GetWindowSizeInPixels"),
            get_keyboard_focus: f!("SDL_GetKeyboardFocus"),
            text_input_active: f!("SDL_TextInputActive"),
            start_text_input: f!("SDL_StartTextInput"),
            stop_text_input: f!("SDL_StopTextInput"),
            cursor_visible: f!("SDL_CursorVisible"),
            show_cursor: f!("SDL_ShowCursor"),
            hide_cursor: f!("SDL_HideCursor"),
            get_preferred_locales: f!("SDL_GetPreferredLocales"),
            free: f!("SDL_free"),
        })
    }
}

static API: OnceLock<Api> = OnceLock::new();
static ORIG_SWAP: AtomicUsize = AtomicUsize::new(0);
static ORIG_POLL: AtomicUsize = AtomicUsize::new(0);
static ORIG_BUTTON: AtomicUsize = AtomicUsize::new(0);
static ORIG_AXIS: AtomicUsize = AtomicUsize::new(0);
static HOOKS: Mutex<Vec<IatHook>> = Mutex::new(Vec::new());
/// The window last presented (for text-input checks).
static WINDOW: AtomicUsize = AtomicUsize::new(0);
/// Mirror of `Router::swallowed_pad` for the lock-free pad-state hooks.
static SWALLOWED: AtomicU64 = AtomicU64::new(0);
/// Set once the overlay has been shut down after a failure.
static FORCED_CLOSED: AtomicBool = AtomicBool::new(false);
/// The HUD was drawn inside this frame (see [`hud_in_frame`]).
static HUD_IN_FRAME: AtomicBool = AtomicBool::new(false);
/// The in-frame target has been logged.
static LOGGED_TARGET: AtomicBool = AtomicBool::new(false);
/// Last logged game text-input state (0 unknown, 1 off, 2 on).
static GAME_TEXT: AtomicU8 = AtomicU8::new(0);
/// Last logged draw geometry: window size in points and pixels, target
/// size, in-frame or not (see [`draw`]).
static LOGGED_GEOMETRY: Mutex<Option<[c_int; 7]>> = Mutex::new(None);
/// Mouse presses logged while HUD widgets are up (capped at
/// [`MAX_PRESS_LOGS`] per session).
static PRESS_LOGS: AtomicUsize = AtomicUsize::new(0);
const MAX_PRESS_LOGS: usize = 60;

struct State {
    router: Router,
    /// The cursor was hidden by the game when the menu opened.
    cursor_was_hidden: bool,
    /// We started SDL text input for a menu text field.
    text_started: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn state() -> MutexGuard<'static, Option<State>> {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(State {
            router: Router::new(),
            cursor_was_hidden: false,
            text_started: false,
        });
    }
    g
}

pub fn install() {
    let name: Vec<u16> = "SDL3.dll\0".encode_utf16().collect();
    // SAFETY: plain module lookups.
    let (exe, sdl) = unsafe {
        (
            GetModuleHandleW(std::ptr::null()),
            GetModuleHandleW(name.as_ptr()),
        )
    };
    if exe.is_null() || sdl.is_null() {
        crate::log("overlay: SDL3.dll is not loaded; overlay off");
        return;
    }
    match Api::resolve(sdl) {
        Ok(api) => {
            let _ = API.set(api);
        }
        Err(e) => {
            crate::log(&format!("overlay: {e}; overlay off"));
            return;
        }
    }
    // (function, detour, original, required). The pad-state hooks only
    // keep a polling game from seeing the pad while the menu is open.
    let hooks: [(&str, usize, &AtomicUsize, bool); 4] = [
        (
            "SDL_PollEvent",
            poll_hook as PollFn as usize,
            &ORIG_POLL,
            true,
        ),
        (
            "SDL_GL_SwapWindow",
            swap_hook as SwapFn as usize,
            &ORIG_SWAP,
            true,
        ),
        (
            "SDL_GetGamepadButton",
            button_hook as ButtonFn as usize,
            &ORIG_BUTTON,
            false,
        ),
        (
            "SDL_GetGamepadAxis",
            axis_hook as AxisFn as usize,
            &ORIG_AXIS,
            false,
        ),
    ];
    let mut installed = HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    let mut hooked = Vec::new();
    for (func, detour, orig, required) in hooks {
        // SAFETY: the exe is loaded and its imports are bound (the runtime
        // thread starts after process initialization); the detours have the
        // SDL signatures.
        match unsafe { IatHook::install(exe as usize, "SDL3.dll", func, detour, orig) } {
            Ok(h) => {
                // SAFETY: as above.
                let real = unsafe { GetProcAddress(sdl, format!("{func}\0").as_ptr()) };
                if h.original() != real as usize {
                    crate::log(&format!(
                        "overlay: {func} was already redirected ({:#x}, SDL's is {:#x}); chaining",
                        h.original(),
                        real as usize
                    ));
                }
                installed.push(h);
                hooked.push(func);
            }
            Err(e) if !required => {
                crate::log(&format!("overlay: not hooking {func}: {e}"));
            }
            Err(e) => {
                crate::log(&format!("overlay: can't hook {func}: {e}; overlay off"));
                for h in installed.drain(..).rev() {
                    let _ = h.remove();
                }
                return;
            }
        }
    }
    crate::log(&format!(
        "overlay: hooked {} in the exe's import table; F10 or LS+RS opens the menu",
        hooked.join(", ")
    ));
}

pub fn uninstall() {
    let mut hooks = HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    for h in hooks.drain(..).rev() {
        if let Err(e) = h.remove() {
            crate::log(&format!("overlay: unhook {}: {e}", h.name));
        }
    }
}

// ---- hooks ----

unsafe extern "C" fn poll_hook(ev: *mut Event) -> bool {
    // SAFETY: set before the slot was swapped.
    let orig: PollFn = unsafe { std::mem::transmute(ORIG_POLL.load(Ordering::Relaxed)) };
    if ev.is_null() {
        // A peek: nothing to inspect.
        return unsafe { orig(ev) };
    }
    loop {
        // SAFETY: the game's event buffer.
        if !unsafe { orig(ev) } {
            return false;
        }
        // The original is called outside the guard, exactly once per event;
        // a panic in routing delivers the event.
        // SAFETY: SDL just filled it.
        let swallow = guard::call_or(
            GUARD_ID,
            || route(unsafe { &*ev }),
            || {
                force_close();
                false
            },
        );
        if !swallow {
            return true;
        }
    }
}

/// Returns true to swallow.
fn route(ev: &Event) -> bool {
    // SAFETY: filled by SDL.
    let input = unsafe { ev.decode() };
    if matches!(input, Input::Other(_)) {
        return false;
    }
    let Some(api) = API.get() else { return false };
    let mut g = state();
    let st = g.as_mut().expect("initialized by state()");
    let window = current_window(api);
    // SAFETY: SDL calls on the thread that polls events.
    let active = !window.is_null() && unsafe { (api.text_input_active)(window) };
    let env = Env {
        game_text_input: active && !st.text_started,
    };
    // Hotkeys are off while this is on; log changes so a game that leaves
    // text input on shows up in the log.
    let now = if env.game_text_input { 2 } else { 1 };
    if GAME_TEXT.swap(now, Ordering::Relaxed) != now {
        crate::log(&format!(
            "overlay: game text input {} (hotkeys {})",
            if env.game_text_input { "on" } else { "off" },
            if env.game_text_input {
                "paused"
            } else {
                "active"
            }
        ));
    }
    let sink = sink();
    let mut to_mods = |e: &InputEvent, held: u64| match sink {
        Some(s) => {
            crate::hotkey::set_input_state(held, false);
            s(e)
        }
        None => false,
    };
    // Where mouse presses land while HUD widgets are up (for triage of
    // clicks that don't reach a widget on some setups).
    if let Input::MouseButton {
        button,
        down: true,
        x,
        y,
    } = input
        && !st.router.open
        && !st.router.hud_hits.is_empty()
        && PRESS_LOGS.fetch_add(1, Ordering::Relaxed) < MAX_PRESS_LOGS
    {
        let hit = st.router.hud_hit(x, y);
        crate::log(&format!(
            "overlay: mouse {button} down at {x:.1},{y:.1} (window), {:.1},{:.1} (points): {}",
            x * st.router.to_points,
            y * st.router.to_points,
            if hit { "HUD widget" } else { "game" }
        ));
    }
    let (v, t) = st.router.route(&input, &env, &mut to_mods);
    SWALLOWED.store(st.router.swallowed_pad, Ordering::Relaxed);
    apply_toggle(api, st, t);
    v == Verdict::Swallow
}

/// After the overlay failed (panic, renderer error): closes the menu so the
/// pad-state hooks (which keep running) and the cursor go back to normal.
/// Runs once; never blocks or panics (it's also the hooks' fallback).
fn force_close() {
    if FORCED_CLOSED.swap(true, Ordering::SeqCst) {
        return;
    }
    OPEN.store(false, Ordering::Relaxed);
    SWALLOWED.store(0, Ordering::Relaxed);
    let lock = match STATE.try_lock() {
        Ok(g) => Some(g),
        // A panic while drawing poisons the lock; the state is still usable.
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    };
    if let (Some(api), Some(mut g)) = (API.get(), lock)
        && let Some(st) = g.as_mut()
    {
        let t = st.router.set_open(false);
        apply_toggle(api, st, t);
        st.router.swallowed_pad = 0;
    }
    SWALLOWED.store(0, Ordering::Relaxed);
    crate::try_log("overlay: turned off after an error; the game keeps running without it");
}

fn current_window(api: &Api) -> Window {
    let w = WINDOW.load(Ordering::Relaxed) as Window;
    if w.is_null() {
        // SAFETY: plain SDL query.
        unsafe { (api.get_keyboard_focus)() }
    } else {
        w
    }
}

fn apply_toggle(api: &Api, st: &mut State, t: Toggle) {
    // SAFETY: SDL video calls on the game's event/render thread.
    unsafe {
        match t {
            Toggle::None => {}
            Toggle::Opened => {
                OPEN.store(true, Ordering::Relaxed);
                st.cursor_was_hidden = !(api.cursor_visible)();
                if st.cursor_was_hidden {
                    (api.show_cursor)();
                }
            }
            Toggle::Closed => {
                OPEN.store(false, Ordering::Relaxed);
                if std::mem::take(&mut st.cursor_was_hidden) {
                    (api.hide_cursor)();
                }
                if std::mem::take(&mut st.text_started) {
                    (api.stop_text_input)(current_window(api));
                }
                SWALLOWED.store(st.router.swallowed_pad, Ordering::Relaxed);
            }
        }
    }
}

unsafe extern "C" fn button_hook(pad: *mut c_void, button: c_int) -> bool {
    // SAFETY: set before the slot was swapped.
    let orig: ButtonFn = unsafe { std::mem::transmute(ORIG_BUTTON.load(Ordering::Relaxed)) };
    let down = unsafe { orig(pad, button) };
    if down
        && (OPEN.load(Ordering::Relaxed)
            || (0..32).contains(&button)
                && SWALLOWED.load(Ordering::Relaxed) & pad_bit(button as u32) != 0)
    {
        return false;
    }
    down
}

unsafe extern "C" fn axis_hook(pad: *mut c_void, axis: c_int) -> i16 {
    // SAFETY: set before the slot was swapped.
    let orig: AxisFn = unsafe { std::mem::transmute(ORIG_AXIS.load(Ordering::Relaxed)) };
    let v = unsafe { orig(pad, axis) };
    if OPEN.load(Ordering::Relaxed) {
        return 0;
    }
    let trigger = match axis as u8 {
        sdl::AXIS_LEFT_TRIGGER => PAD_LT,
        sdl::AXIS_RIGHT_TRIGGER => PAD_RT,
        _ => return v,
    };
    if SWALLOWED.load(Ordering::Relaxed) & pad_bit(trigger) != 0 {
        0
    } else {
        v
    }
}

unsafe extern "C" fn swap_hook(window: Window) -> bool {
    // SAFETY: set before the slot was swapped.
    let orig: SwapFn = unsafe { std::mem::transmute(ORIG_SWAP.load(Ordering::Relaxed)) };
    if let Some(f) = super::frame_sink() {
        guard::call_or(GUARD_ID, f, || {});
    }
    // Draw under the guard; present exactly once, outside it.
    guard::call_or(GUARD_ID, || frame(window), force_close);
    unsafe { orig(window) }
}

// ---- rendering ----

struct Gfx {
    /// The SDL GL context these objects belong to.
    context: usize,
    /// Mod HUD widgets were drawn last frame.
    hud_was_on: bool,
    gl: Arc<glow::Context>,
    caps: gl::Caps,
    painter: Option<egui_glow::Painter>,
    egui: egui::Context,
    ui: UiState,
}

thread_local! {
    static GFX: RefCell<Option<Gfx>> = const { RefCell::new(None) };
}

static START: OnceLock<Instant> = OnceLock::new();

fn detect_language(api: &Api) {
    let mut n: c_int = 0;
    // SAFETY: SDL returns a NULL-terminated array freed with SDL_free.
    unsafe {
        let list = (api.get_preferred_locales)(&mut n);
        if list.is_null() {
            return;
        }
        for i in 0..n.max(0) as usize {
            let l = *list.add(i);
            if l.is_null() || (*l).language.is_null() {
                continue;
            }
            let code = CStr::from_ptr((*l).language).to_string_lossy();
            if let Some(lang) = i18n::Lang::from_locale(&code) {
                crate::log(&format!(
                    "overlay: language {} (system locale {code})",
                    lang.code()
                ));
                i18n::set_default_lang(lang);
                break;
            }
        }
        (api.free)(list.cast());
    }
}

fn frame(window: Window) {
    let Some(api) = API.get() else { return };
    WINDOW.store(window as usize, Ordering::Relaxed);
    // SAFETY: called by the game right before presenting, with its context current.
    let context = unsafe { (api.gl_get_current_context)() } as usize;
    if context == 0 {
        return;
    }
    GFX.with_borrow_mut(|slot| {
        if slot.as_ref().is_none_or(|g| g.context != context) {
            if slot.is_some() {
                // The old context is gone; its objects went with it.
                crate::log("overlay: GL context changed; recreating renderer");
            } else {
                detect_language(api);
            }
            // SAFETY: loader for the current context.
            let gl = unsafe {
                glow::Context::from_loader_function_cstr(|name| {
                    (api.gl_get_proc_address)(name.as_ptr())
                })
            };
            let caps = gl::caps(&gl);
            if let Some(old) = slot.take() {
                // Don't free GL names on the new context.
                std::mem::forget(old.painter);
            }
            *slot = Some(Gfx {
                context,
                hud_was_on: false,
                gl: Arc::new(gl),
                caps,
                painter: None,
                egui: egui::Context::default(),
                ui: UiState::default(),
            });
        }
        let g = slot.as_mut().expect("just set");
        let open = OPEN.load(Ordering::Relaxed);
        let (mut pw, mut ph) = (0, 0);
        // SAFETY: the game's window.
        unsafe { (api.get_window_size_in_pixels)(window, &mut pw, &mut ph) };
        crate::hud::set_window(pw, ph, pixels_per_point(ph));
        let hud = !open && hud_wanted();
        if g.hud_was_on && !hud {
            // Clicks go back to the game.
            if let Some(st) = state().as_mut() {
                st.router.clear_hud();
            }
        }
        g.hud_was_on = hud;
        // The HUD and toasts were drawn inside the frame already (under the
        // game's cursor); this frame's egui pass is done.
        let in_frame = HUD_IN_FRAME.swap(false, Ordering::Relaxed);
        if open || (!in_frame && (hud || crate::toast::any())) {
            draw(api, window, g, open, hud, false);
        }
        super::capture::maybe(&g.gl, pw, ph);
    });
}

/// Called right before the game submits its top UI layer (see `layer`):
/// draws the mods' HUD widgets and toasts into the game's frame, so the
/// item on the cursor and the cursor itself are drawn over them. Only with
/// the menu closed and the HUD up; otherwise everything waits for swap time.
pub(super) fn hud_in_frame() {
    guard::call_or(GUARD_ID, in_frame, force_close);
}

fn in_frame() {
    if OPEN.load(Ordering::Relaxed) || HUD_IN_FRAME.load(Ordering::Relaxed) {
        return;
    }
    let Some(api) = API.get() else { return };
    let window = WINDOW.load(Ordering::Relaxed) as Window;
    if window.is_null() {
        return;
    }
    // SAFETY: called by the game while it draws, with its context current.
    let context = unsafe { (api.gl_get_current_context)() } as usize;
    GFX.with_borrow_mut(|slot| {
        // Set up at swap time (first frame, new context); until then the
        // HUD draws there.
        let Some(g) = slot.as_mut().filter(|g| g.context == context) else {
            return;
        };
        // Same rule as at swap time (where `hud_was_on` is kept).
        if !g.hud_was_on || !hud_wanted() {
            return;
        }
        if draw(api, window, g, false, true, true) {
            HUD_IN_FRAME.store(true, Ordering::Relaxed);
        }
    });
}

/// The overlay's UI scale: 1 at 1080 lines and below (Steam Deck 800),
/// larger above.
fn pixels_per_point(ph: c_int) -> f32 {
    (ph as f32 / 1080.0).max(1.0)
}

/// Whether to run the HUD pass: some mod draws on the HUD (or the layout
/// outlines are on) and the game's plain HUD is up.
fn hud_wanted() -> bool {
    (crate::hud::debug() || host().is_some_and(|h| h.wants_hud())) && crate::hud::shown()
}

/// The egui layer of the mods' HUD widgets.
fn hud_layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Middle, egui::Id::unique("uk_qol_hud"))
}

/// Draws the menu (if `open`), the mods' HUD widgets (if `hud`) and any
/// toasts. Only the menu changes text-input or cursor state; the HUD gets
/// mouse input through the router's hit test.
///
/// `in_frame`: called inside the game's frame (see [`hud_in_frame`]), so
/// it draws into the framebuffer the game has bound, at its viewport's
/// size; else into the window. Returns whether it drew.
fn draw(api: &Api, window: Window, g: &mut Gfx, open: bool, hud: bool, in_frame: bool) -> bool {
    let (mut w, mut h, mut pw, mut ph) = (0, 0, 0, 0);
    // SAFETY: the game's window.
    unsafe {
        (api.get_window_size)(window, &mut w, &mut h);
        (api.get_window_size_in_pixels)(window, &mut pw, &mut ph);
    }
    if w <= 0 || h <= 0 || pw <= 0 || ph <= 0 {
        return false;
    }
    let ppp = pixels_per_point(ph);
    let to_points = (pw as f32 / w as f32) / ppp;
    let now = START.get_or_init(Instant::now).elapsed().as_secs_f64();

    // SAFETY: the context is current; the scope restores everything it changes.
    let saved = unsafe { Saved::save(&g.gl, g.caps, in_frame) };
    // The target's size in pixels, and egui's scale for it: the window's
    // points (where input and the HUD layout live) mapped onto it.
    let (tw, th) = if in_frame {
        let [x, y, vw, vh] = saved.viewport();
        if !LOGGED_TARGET.swap(true, Ordering::Relaxed) {
            crate::log(&format!(
                "overlay: drawing the HUD inside the game's frame (framebuffer {}, viewport {x},{y} {vw}x{vh}, window {pw}x{ph})",
                saved.draw_fb()
            ));
        }
        if x != 0 || y != 0 || vw <= 0 || vh <= 0 {
            // Not a whole-target viewport: leave it to swap time.
            return false;
        }
        (vw, vh)
    } else {
        (pw, ph)
    };
    let ppp_target = ppp * tw as f32 / pw as f32;
    {
        let g = [w, h, pw, ph, tw, th, in_frame as c_int];
        let mut last = LOGGED_GEOMETRY.lock().unwrap_or_else(|e| e.into_inner());
        if *last != Some(g) {
            *last = Some(g);
            crate::log(&format!(
                "overlay: window {w}x{h} (points) {pw}x{ph} (pixels), drawing {} at {tw}x{th}, \
                 {ppp:.3} pixels per point, window to points x{to_points:.4}",
                if in_frame {
                    "in the game's frame"
                } else {
                    "at swap"
                }
            ));
        }
    }
    if g.painter.is_none() {
        match egui_glow::Painter::new(g.gl.clone(), "", None, false) {
            Ok(p) => g.painter = Some(p),
            Err(e) => {
                drop(saved);
                crate::log(&format!("overlay: can't create the renderer: {e}"));
                guard::fail(GUARD_ID, format!("renderer: {e}"));
                force_close();
                return false;
            }
        }
    }
    let painter = g.painter.as_mut().expect("just set");

    let mut sg = state();
    let st = sg.as_mut().expect("initialized by state()");
    st.router.to_points = to_points;
    let events = if open {
        st.router.tick(now);
        std::mem::take(&mut st.router.ui_events)
    } else if hud {
        std::mem::take(&mut st.router.hud_events)
    } else {
        Vec::new()
    };
    let mut raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(tw as f32 / ppp_target, th as f32 / ppp_target),
        )),
        max_texture_side: Some(painter.max_texture_side()),
        time: Some(now),
        events,
        focused: open || hud,
        ..Default::default()
    };
    raw.viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .native_pixels_per_point = Some(ppp_target);
    let host = host();
    let mut close = false;
    // Without the in-frame hook the HUD covers the game's cursor; draw a
    // copy of it on top while it's over a HUD widget.
    let redraw_cursor = hud && !in_frame && {
        // SAFETY: plain SDL query on the render thread.
        let system = unsafe { (api.cursor_visible)() };
        !system
    };
    let hits = st.router.hud_hits.clone();
    let out = g.egui.run_ui(raw, |ui| {
        if open {
            close = ui::menu(ui.ctx(), host.as_deref(), &mut st.router, &mut g.ui);
        } else if hud {
            let layer = hud_layer();
            let screen = ui.ctx().content_rect();
            egui::Area::new(layer.id)
                .order(layer.order)
                .fixed_pos(egui::Pos2::ZERO)
                .movable(false)
                .constrain(false)
                .sense(egui::Sense::hover())
                .show(ui.ctx(), |ui| {
                    ui.set_clip_rect(screen);
                    if crate::hud::debug() {
                        crate::hud::draw_debug(ui);
                    }
                    if let Some(h) = host.as_deref() {
                        h.hud(ui);
                    }
                });
        }
        crate::toast::draw(ui.ctx());
        if redraw_cursor {
            paint_game_cursor(ui.ctx(), &hits);
        }
    });
    if hud {
        // What the router lets the HUD have next: the widgets that take
        // clicks or drags.
        let layer = hud_layer();
        st.router.hud_hits = g.egui.viewport(|v| {
            v.prev_pass
                .widgets
                .get_layer(layer)
                .filter(|w| w.sense.senses_click() || w.sense.senses_drag())
                .map(|w| w.interact_rect)
                .collect()
        });
    }
    // With the menu closed (HUD widgets and toasts), text-input and cursor
    // state are left alone.
    if open {
        let wants_keyboard = g.egui.egui_wants_keyboard_input();
        st.router.ui_wants_keyboard = wants_keyboard;
        if close {
            let t = st.router.set_open(false);
            apply_toggle(api, st, t);
        } else {
            // SAFETY: SDL keyboard calls on the render thread.
            unsafe {
                if wants_keyboard && !st.text_started && !(api.text_input_active)(window) {
                    st.text_started = (api.start_text_input)(window);
                } else if !wants_keyboard && st.text_started {
                    (api.stop_text_input)(window);
                    st.text_started = false;
                }
            }
        }
    }
    drop(sg);

    let mut out = out;
    let prims = g
        .egui
        .tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
    painter.paint_and_update_textures(
        [tw as u32, th as u32],
        out.pixels_per_point,
        &prims,
        &mut out.textures_delta,
    );
    drop(saved);
    true
}

/// Fallback when the HUD draws at swap time, over the game's software
/// cursor: draws the game's cursor sprite (read from the install) at the
/// pointer, at the HUD's GUI scale and with its pivot, while the pointer
/// is over one of `hits` (the HUD widgets).
fn paint_game_cursor(ctx: &egui::Context, hits: &[egui::Rect]) {
    let Some(p) = ctx.input(|i| i.pointer.latest_pos()) else {
        return;
    };
    if !hits.iter().any(|r| r.contains(p)) {
        return;
    }
    let Some(name) = super::layer::cursor_sprite() else {
        return;
    };
    let (Some((tex, size)), Some(sprite)) =
        (crate::sprite::texture(ctx, name), crate::sprite::get(name))
    else {
        return;
    };
    let ppu = crate::hud::points_per_unit();
    let min = p - egui::vec2(sprite.pivot[0], sprite.pivot[1]) * ppu;
    let rect = egui::Rect::from_min_size(min, size * ppu);
    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Debug,
        egui::Id::unique("uk_qol_game_cursor"),
    ))
    .image(tex.id(), rect, uv, egui::Color32::WHITE);
}
