//! Decides, for each SDL event the game polls, whether the game gets it,
//! and turns it into egui input (menu open) or mod input (menu closed).
//! Pure logic, no SDL or GL calls, so it's tested on any platform.
//!
//! Rules:
//! - The menu key (F10) and pad chord (LS+RS) toggle the menu; the press
//!   that completes them is swallowed.
//! - While the menu is open every keyboard, mouse, text and gamepad event
//!   goes to the menu and is swallowed, except releases of keys and buttons
//!   whose press the game saw (so nothing stays held in the game).
//! - While closed, presses go to the mods (hotkeys) unless the game has a
//!   text field focused; a press a mod handles is swallowed, and so is its
//!   release. Releases go to the mods too, but only to observe: whether the
//!   game gets a release depends only on whether it got the press.

use std::collections::BTreeSet;

use egui::{Event as UiEvent, Key, Modifiers, PointerButton, Pos2, Vec2};

use super::sdl::{self, Input, sc};
use crate::hotkey::{Binding, PAD_LT, PAD_RT, pad_bit};
use crate::input::{InputEvent, KeyMods};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Deliver,
    Swallow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    None,
    Opened,
    Closed,
}

/// What the router needs to know about the game for one event.
#[derive(Debug, Clone, Copy, Default)]
pub struct Env {
    /// The game has SDL text input active (a text field is focused).
    pub game_text_input: bool,
}

/// A "press a key to bind" request from the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub mod_id: String,
    pub key: String,
    /// Pad buttons pressed since capture started. A pad capture finishes
    /// when one of them is released, so a chord can be pressed one button
    /// at a time. Buttons already held at the start (the A that pressed
    /// "Change") aren't part of it.
    pad_pressed: u64,
}

/// Trigger press threshold and release threshold (hysteresis), of 32767.
const TRIGGER_ON: i16 = 16000;
const TRIGGER_OFF: i16 = 8000;
/// Left-stick menu navigation thresholds.
const STICK_ON: i16 = 20000;
const STICK_OFF: i16 = 10000;
/// Seconds before a held d-pad direction repeats, and between repeats.
const REPEAT_DELAY: f64 = 0.4;
const REPEAT_EVERY: f64 = 0.08;

pub struct Router {
    pub open: bool,
    pub menu_key: Binding,
    pub menu_pad: Binding,
    /// Keys / mouse buttons / pad buttons whose press the game saw.
    game_keys: BTreeSet<u32>,
    game_mouse: u32,
    game_pad: u64,
    /// Pad buttons (and triggers) held right now.
    pad_held: u64,
    /// Pad buttons whose press was swallowed and that are still held: the
    /// game's polling of them (`SDL_GetGamepadButton`) reads "released".
    pub swallowed_pad: u64,
    triggers: [bool; 2],
    stick: [i8; 2],
    nav_held: Option<(Key, Option<f64>)>,
    pub capture: Option<Capture>,
    /// A finished capture: (mod, setting key, spec string).
    pub captured: Option<(String, String, String)>,
    /// Input for the menu, drained each frame.
    pub ui_events: Vec<UiEvent>,
    /// Window coordinates to egui points.
    pub to_points: f32,
    /// Set by the renderer: a menu text field has keyboard focus (Escape
    /// then leaves the field instead of closing the menu).
    pub ui_wants_keyboard: bool,
    /// The menu was opened from a gamepad: focus the first widget.
    pub focus_first: bool,
    mods: Modifiers,
}

impl Default for Router {
    fn default() -> Self {
        Router::new()
    }
}

fn key_mods(m: u16) -> KeyMods {
    KeyMods {
        shift: m & sdl::KMOD_SHIFT != 0,
        ctrl: m & sdl::KMOD_CTRL != 0,
        alt: m & sdl::KMOD_ALT != 0,
    }
}

fn ui_mods(m: u16) -> Modifiers {
    let k = key_mods(m);
    Modifiers {
        alt: k.alt,
        ctrl: k.ctrl,
        shift: k.shift,
        mac_cmd: false,
        command: k.ctrl,
    }
}

/// egui key for an SDL scancode (US layout positions).
pub fn ui_key(scancode: u32) -> Option<Key> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [Key; 10] = [
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
        Key::Num0,
    ];
    const FKEYS: [Key; 12] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ];
    Some(match scancode {
        s if (sc::A..sc::A + 26).contains(&s) => LETTERS[(s - sc::A) as usize],
        s if (sc::N1..=sc::N0).contains(&s) => DIGITS[(s - sc::N1) as usize],
        s if (sc::KP_1..=sc::KP_0).contains(&s) => DIGITS[(s - sc::KP_1) as usize],
        sc::RETURN | sc::KP_ENTER => Key::Enter,
        sc::ESCAPE => Key::Escape,
        sc::BACKSPACE => Key::Backspace,
        sc::TAB => Key::Tab,
        sc::SPACE => Key::Space,
        sc::MINUS | sc::KP_MINUS => Key::Minus,
        sc::EQUALS => Key::Equals,
        sc::KP_PLUS => Key::Plus,
        sc::PERIOD | sc::KP_PERIOD => Key::Period,
        sc::COMMA => Key::Comma,
        sc::INSERT => Key::Insert,
        sc::DELETE => Key::Delete,
        sc::HOME => Key::Home,
        sc::END => Key::End,
        sc::PAGEUP => Key::PageUp,
        sc::PAGEDOWN => Key::PageDown,
        sc::LEFT => Key::ArrowLeft,
        sc::RIGHT => Key::ArrowRight,
        sc::UP => Key::ArrowUp,
        sc::DOWN => Key::ArrowDown,
        s if (sc::F1..sc::F1 + 12).contains(&s) => FKEYS[(s - sc::F1) as usize],
        _ => return None,
    })
}

impl Router {
    pub fn new() -> Router {
        Router {
            open: false,
            menu_key: "F10".parse().expect("valid spec"),
            menu_pad: "Pad:LS+Pad:RS".parse().expect("valid spec"),
            game_keys: BTreeSet::new(),
            game_mouse: 0,
            game_pad: 0,
            pad_held: 0,
            swallowed_pad: 0,
            triggers: [false; 2],
            stick: [0; 2],
            nav_held: None,
            capture: None,
            captured: None,
            ui_events: Vec::new(),
            to_points: 1.0,
            ui_wants_keyboard: false,
            focus_first: false,
            mods: Modifiers::NONE,
        }
    }

    /// Opens or closes the menu (also from the menu's own Close button).
    pub fn set_open(&mut self, open: bool) -> Toggle {
        if open == self.open {
            return Toggle::None;
        }
        self.open = open;
        self.capture = None;
        self.nav_held = None;
        self.ui_events.clear();
        if open {
            Toggle::Opened
        } else {
            self.focus_first = false;
            self.ui_wants_keyboard = false;
            Toggle::Closed
        }
    }

    pub fn start_capture(&mut self, mod_id: &str, key: &str) {
        self.capture = Some(Capture {
            mod_id: mod_id.to_owned(),
            key: key.to_owned(),
            pad_pressed: 0,
        });
    }

    fn finish_capture(&mut self, spec: String) {
        if let Some(c) = self.capture.take() {
            self.captured = Some((c.mod_id, c.key, spec));
        }
    }

    fn pointer(&self, x: f32, y: f32) -> Pos2 {
        Pos2::new(x * self.to_points, y * self.to_points)
    }

    fn ui_key_event(&mut self, key: Key, pressed: bool, repeat: bool) {
        self.ui_events.push(UiEvent::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat,
            modifiers: self.mods,
        });
    }

    fn ui_tap(&mut self, key: Key) {
        self.ui_key_event(key, true, false);
        self.ui_key_event(key, false, false);
    }

    /// Generates d-pad / stick auto-repeat. Call once per frame.
    pub fn tick(&mut self, now: f64) {
        if !self.open {
            return;
        }
        if let Some((key, next)) = self.nav_held {
            match next {
                None => self.nav_held = Some((key, Some(now + REPEAT_DELAY))),
                Some(t) if now >= t => {
                    self.ui_key_event(key, true, true);
                    self.ui_key_event(key, false, false);
                    self.nav_held = Some((key, Some(now + REPEAT_EVERY)));
                }
                Some(_) => {}
            }
        }
    }

    fn nav_press(&mut self, key: Key) {
        self.ui_tap(key);
        self.nav_held = Some((key, None));
    }

    fn nav_release(&mut self, key: Key) {
        if matches!(self.nav_held, Some((k, _)) if k == key) {
            self.nav_held = None;
        }
    }

    /// Routes one event. `sink` gets mod input (menu closed, no text field)
    /// with the pad buttons held before it, and returns true to swallow it.
    pub fn route(
        &mut self,
        input: &Input,
        env: &Env,
        sink: &mut dyn FnMut(&InputEvent, u64) -> bool,
    ) -> (Verdict, Toggle) {
        use Verdict::*;
        match *input {
            Input::Key {
                scancode,
                mods,
                down,
                repeat,
            } => {
                self.mods = ui_mods(mods);
                let km = key_mods(mods);
                if !down {
                    if self.open
                        && let Some(k) = ui_key(scancode)
                    {
                        self.ui_key_event(k, false, false);
                    }
                    if !self.open && !env.game_text_input {
                        // Releases are observed by mods, never swallowed by them.
                        sink(&InputEvent::KeyUp { scancode, mods: km }, self.pad_held);
                    }
                    let game_had = self.game_keys.remove(&scancode);
                    return (if game_had { Deliver } else { Swallow }, Toggle::None);
                }
                if self.capture.is_some() {
                    if !repeat && scancode == sc::ESCAPE {
                        self.capture = None;
                    } else if !repeat && !sc::is_modifier(scancode) {
                        let b = Binding::Key { scancode, mods: km };
                        self.finish_capture(b.to_string());
                    }
                    return (Swallow, Toggle::None);
                }
                let ev = InputEvent::KeyDown {
                    scancode,
                    mods: km,
                    repeat,
                };
                if self.menu_key.matches(&ev, self.pad_held) {
                    let open = !self.open;
                    return (Swallow, self.set_open(open));
                }
                if self.open {
                    if scancode == sc::ESCAPE && !repeat && !self.ui_wants_keyboard {
                        return (Swallow, self.set_open(false));
                    }
                    if let Some(k) = ui_key(scancode) {
                        self.ui_key_event(k, true, repeat);
                    }
                    return (Swallow, Toggle::None);
                }
                if repeat {
                    // Repeats follow the first press: delivered only if the
                    // game got it.
                    let v = if self.game_keys.contains(&scancode) {
                        Deliver
                    } else {
                        Swallow
                    };
                    return (v, Toggle::None);
                }
                if !env.game_text_input && sink(&ev, self.pad_held) {
                    return (Swallow, Toggle::None);
                }
                self.game_keys.insert(scancode);
                (Deliver, Toggle::None)
            }
            Input::Text(ref t) => {
                if self.open {
                    if self.ui_wants_keyboard {
                        self.ui_events.push(UiEvent::Text(t.clone()));
                    }
                    (Swallow, Toggle::None)
                } else {
                    (Deliver, Toggle::None)
                }
            }
            Input::MouseMotion { x, y } => {
                if self.open {
                    let p = self.pointer(x, y);
                    self.ui_events.push(UiEvent::PointerMoved(p));
                    (Swallow, Toggle::None)
                } else {
                    (Deliver, Toggle::None)
                }
            }
            Input::MouseButton { button, down, x, y } => {
                let bit = 1u32 << (button & 31);
                if self.open {
                    let b = match button {
                        sdl::BUTTON_LEFT => Some(PointerButton::Primary),
                        sdl::BUTTON_RIGHT => Some(PointerButton::Secondary),
                        sdl::BUTTON_MIDDLE => Some(PointerButton::Middle),
                        _ => None,
                    };
                    if let Some(b) = b {
                        let pos = self.pointer(x, y);
                        self.ui_events.push(UiEvent::PointerMoved(pos));
                        self.ui_events.push(UiEvent::PointerButton {
                            pos,
                            button: b,
                            pressed: down,
                            modifiers: self.mods,
                        });
                    }
                }
                if down {
                    if self.open {
                        return (Swallow, Toggle::None);
                    }
                    self.game_mouse |= bit;
                    (Deliver, Toggle::None)
                } else {
                    let game_had = self.game_mouse & bit != 0;
                    self.game_mouse &= !bit;
                    (if game_had { Deliver } else { Swallow }, Toggle::None)
                }
            }
            Input::Wheel { x, y } => {
                if self.open {
                    self.ui_events.push(UiEvent::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: Vec2::new(x, y),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers: self.mods,
                    });
                    (Swallow, Toggle::None)
                } else {
                    (Deliver, Toggle::None)
                }
            }
            Input::PadButton { button, down } => self.pad_button(button, down, env, sink),
            Input::PadAxis { axis, value } => self.pad_axis(axis, value, env, sink),
            Input::FocusLost | Input::Other(_) => (Deliver, Toggle::None),
        }
    }

    fn pad_button(
        &mut self,
        button: u32,
        down: bool,
        env: &Env,
        sink: &mut dyn FnMut(&InputEvent, u64) -> bool,
    ) -> (Verdict, Toggle) {
        use Verdict::*;
        let bit = pad_bit(button);
        let before = self.pad_held;
        if !down {
            if let Some(c) = &self.capture
                && c.pad_pressed & bit != 0
            {
                let pressed = c.pad_pressed;
                let buttons = (0..34u32)
                    .map(|i| match i {
                        32 => PAD_LT,
                        33 => PAD_RT,
                        i => i,
                    })
                    .filter(|b| pressed & pad_bit(*b) != 0)
                    .collect();
                self.finish_capture(Binding::Pad(buttons).to_string());
            }
            self.pad_held &= !bit;
            self.swallowed_pad &= !bit;
            if let Some(k) = nav_key(button) {
                self.nav_release(k);
            }
            if !self.open && !env.game_text_input {
                sink(&InputEvent::PadUp { button }, self.pad_held);
            }
            let game_had = self.game_pad & bit != 0;
            self.game_pad &= !bit;
            return (if game_had { Deliver } else { Swallow }, Toggle::None);
        }
        self.pad_held |= bit;
        if let Some(c) = &mut self.capture {
            c.pad_pressed |= bit;
            self.swallowed_pad |= bit;
            return (Swallow, Toggle::None);
        }
        let ev = InputEvent::PadDown { button };
        if self.menu_pad.matches(&ev, before) {
            self.swallowed_pad |= bit;
            let open = !self.open;
            let t = self.set_open(open);
            if t == Toggle::Opened {
                self.focus_first = true;
            }
            return (Swallow, t);
        }
        if self.open {
            self.swallowed_pad |= bit;
            if let Some(k) = nav_key(button) {
                self.nav_press(k);
            } else {
                match button {
                    sdl::PAD_SOUTH => self.ui_tap(Key::Enter),
                    sdl::PAD_EAST => {
                        if self.ui_wants_keyboard {
                            self.ui_tap(Key::Escape);
                        } else {
                            return (Swallow, self.set_open(false));
                        }
                    }
                    sdl::PAD_LEFT_SHOULDER => self.ui_tap(Key::PageUp),
                    sdl::PAD_RIGHT_SHOULDER => self.ui_tap(Key::PageDown),
                    _ => {}
                }
            }
            return (Swallow, Toggle::None);
        }
        if !env.game_text_input && sink(&ev, before) {
            self.swallowed_pad |= bit;
            return (Swallow, Toggle::None);
        }
        self.game_pad |= bit;
        (Deliver, Toggle::None)
    }

    fn pad_axis(
        &mut self,
        axis: u8,
        value: i16,
        env: &Env,
        sink: &mut dyn FnMut(&InputEvent, u64) -> bool,
    ) -> (Verdict, Toggle) {
        use Verdict::*;
        match axis {
            sdl::AXIS_LEFT_TRIGGER | sdl::AXIS_RIGHT_TRIGGER => {
                let i = (axis - sdl::AXIS_LEFT_TRIGGER) as usize;
                let button = if i == 0 { PAD_LT } else { PAD_RT };
                let mut toggle = Toggle::None;
                if !self.triggers[i] && value > TRIGGER_ON {
                    self.triggers[i] = true;
                    toggle = self.pad_button(button, true, env, sink).1;
                } else if self.triggers[i] && value < TRIGGER_OFF {
                    self.triggers[i] = false;
                    self.pad_button(button, false, env, sink);
                    // Always let the game see the trigger come back up.
                    return (if self.open { Swallow } else { Deliver }, Toggle::None);
                }
                let swallowed = self.swallowed_pad & pad_bit(button) != 0;
                (
                    if self.open || swallowed {
                        Swallow
                    } else {
                        Deliver
                    },
                    toggle,
                )
            }
            sdl::AXIS_LEFTX | sdl::AXIS_LEFTY if self.open => {
                let i = axis as usize;
                let (neg, pos) = if i == 0 {
                    (Key::ArrowLeft, Key::ArrowRight)
                } else {
                    (Key::ArrowUp, Key::ArrowDown)
                };
                let dir = if value <= -STICK_ON {
                    -1
                } else if value >= STICK_ON {
                    1
                } else if value.unsigned_abs() < STICK_OFF as u16 {
                    0
                } else {
                    self.stick[i]
                };
                if dir != self.stick[i] {
                    match self.stick[i] {
                        -1 => self.nav_release(neg),
                        1 => self.nav_release(pos),
                        _ => {}
                    }
                    match dir {
                        -1 => self.nav_press(neg),
                        1 => self.nav_press(pos),
                        _ => {}
                    }
                    self.stick[i] = dir;
                }
                (Swallow, Toggle::None)
            }
            _ => (if self.open { Swallow } else { Deliver }, Toggle::None),
        }
    }
}

fn nav_key(button: u32) -> Option<Key> {
    Some(match button {
        sdl::PAD_DPAD_UP => Key::ArrowUp,
        sdl::PAD_DPAD_DOWN => Key::ArrowDown,
        sdl::PAD_DPAD_LEFT => Key::ArrowLeft,
        sdl::PAD_DPAD_RIGHT => Key::ArrowRight,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use Verdict::*;

    fn key(scancode: u32, down: bool) -> Input {
        Input::Key {
            scancode,
            mods: 0,
            down,
            repeat: false,
        }
    }

    fn pad(button: u32, down: bool) -> Input {
        Input::PadButton { button, down }
    }

    const W: u32 = sc::A + 22;
    const S: u32 = sc::A + 18;
    const F10: u32 = sc::F1 + 9;

    fn none(_: &InputEvent, _: u64) -> bool {
        false
    }

    #[test]
    fn menu_key_toggles_and_is_swallowed() {
        let mut r = Router::new();
        let env = Env::default();
        assert_eq!(
            r.route(&key(F10, true), &env, &mut none),
            (Swallow, Toggle::Opened)
        );
        assert_eq!(
            r.route(&key(F10, false), &env, &mut none),
            (Swallow, Toggle::None)
        );
        assert!(r.open);
        // Typing goes to the menu, not the game.
        assert_eq!(r.route(&key(W, true), &env, &mut none).0, Swallow);
        assert_eq!(r.route(&key(W, false), &env, &mut none).0, Swallow);
        assert_eq!(
            r.route(&Input::Text("w".into()), &env, &mut none).0,
            Swallow
        );
        assert_eq!(
            r.route(&key(F10, true), &env, &mut none),
            (Swallow, Toggle::Closed)
        );
        assert_eq!(r.route(&key(W, true), &env, &mut none).0, Deliver);
        // Other events always pass.
        assert_eq!(r.route(&Input::Other(0x100), &env, &mut none).0, Deliver);
    }

    #[test]
    fn keys_held_before_opening_are_released_in_game() {
        let mut r = Router::new();
        let env = Env::default();
        assert_eq!(r.route(&key(W, true), &env, &mut none).0, Deliver);
        r.route(&key(F10, true), &env, &mut none);
        assert_eq!(r.route(&key(W, false), &env, &mut none).0, Deliver);
        // Mouse too.
        r.set_open(false);
        let click = |down| Input::MouseButton {
            button: sdl::BUTTON_LEFT,
            down,
            x: 1.0,
            y: 2.0,
        };
        assert_eq!(r.route(&click(true), &env, &mut none).0, Deliver);
        r.set_open(true);
        assert_eq!(r.route(&click(false), &env, &mut none).0, Deliver);
        assert_eq!(r.route(&click(true), &env, &mut none).0, Swallow);
        r.set_open(false);
        assert_eq!(r.route(&click(false), &env, &mut none).0, Swallow);
    }

    #[test]
    fn hotkeys_swallow_press_and_release() {
        let mut r = Router::new();
        let env = Env::default();
        let mut sort = |ev: &InputEvent, _: u64| matches!(ev, InputEvent::KeyDown { scancode, .. } if *scancode == S);
        assert_eq!(r.route(&key(S, true), &env, &mut sort).0, Swallow);
        let rep = Input::Key {
            scancode: S,
            mods: 0,
            down: true,
            repeat: true,
        };
        assert_eq!(r.route(&rep, &env, &mut sort).0, Swallow);
        assert_eq!(r.route(&key(S, false), &env, &mut sort).0, Swallow);
        // Not while the game has a text field focused.
        let typing = Env {
            game_text_input: true,
        };
        assert_eq!(r.route(&key(S, true), &typing, &mut sort).0, Deliver);
        assert_eq!(r.route(&key(S, false), &typing, &mut sort).0, Deliver);
        // Not while the menu is open (the menu gets it).
        r.set_open(true);
        let mut called = false;
        let mut spy = |_: &InputEvent, _: u64| {
            called = true;
            true
        };
        r.route(&key(S, true), &env, &mut spy);
        assert!(!called);
    }

    #[test]
    fn releases_reach_mods_but_cannot_be_swallowed_by_them() {
        let mut r = Router::new();
        let env = Env::default();
        let mut seen = Vec::new();
        let mut all = |ev: &InputEvent, _: u64| {
            seen.push(ev.clone());
            true
        };
        // The mod swallows W's press; its release is shown to the mod and
        // swallowed (the game never saw the press).
        assert_eq!(r.route(&key(W, true), &env, &mut all).0, Swallow);
        assert_eq!(r.route(&key(W, false), &env, &mut all).0, Swallow);
        assert_eq!(
            r.route(&pad(sdl::PAD_NORTH, false), &env, &mut all).0,
            Swallow
        );
        assert_eq!(seen.len(), 3);
        assert!(matches!(seen[1], InputEvent::KeyUp { scancode: W, .. }));
        assert_eq!(
            seen[2],
            InputEvent::PadUp {
                button: sdl::PAD_NORTH
            }
        );
        // A release whose press the game got is delivered even if the mod
        // returns true for it.
        let mut ups = |ev: &InputEvent, _: u64| matches!(ev, InputEvent::KeyUp { .. });
        assert_eq!(r.route(&key(S, true), &env, &mut ups).0, Deliver);
        assert_eq!(r.route(&key(S, false), &env, &mut ups).0, Deliver);
    }

    #[test]
    fn pad_chord_opens_menu_and_navigates() {
        let mut r = Router::new();
        let env = Env::default();
        // LS goes to the game (nothing to know yet), RS completes the chord.
        assert_eq!(
            r.route(&pad(sdl::PAD_LEFT_STICK, true), &env, &mut none).0,
            Deliver
        );
        assert_eq!(
            r.route(&pad(sdl::PAD_RIGHT_STICK, true), &env, &mut none),
            (Swallow, Toggle::Opened)
        );
        assert!(r.focus_first);
        assert_eq!(r.swallowed_pad, pad_bit(sdl::PAD_RIGHT_STICK));
        assert_eq!(
            r.route(&pad(sdl::PAD_RIGHT_STICK, false), &env, &mut none)
                .0,
            Swallow
        );
        assert_eq!(
            r.route(&pad(sdl::PAD_LEFT_STICK, false), &env, &mut none).0,
            Deliver
        );
        assert_eq!(r.swallowed_pad, 0);
        r.ui_events.clear();
        assert_eq!(
            r.route(&pad(sdl::PAD_DPAD_DOWN, true), &env, &mut none).0,
            Swallow
        );
        assert!(matches!(
            r.ui_events[0],
            UiEvent::Key {
                key: Key::ArrowDown,
                pressed: true,
                ..
            }
        ));
        // Held d-pad repeats.
        r.ui_events.clear();
        r.tick(10.0);
        r.tick(10.1);
        assert!(r.ui_events.is_empty());
        r.tick(10.5);
        assert_eq!(r.ui_events.len(), 2);
        r.route(&pad(sdl::PAD_DPAD_DOWN, false), &env, &mut none);
        r.ui_events.clear();
        r.tick(11.0);
        assert!(r.ui_events.is_empty());
        // B closes.
        assert_eq!(
            r.route(&pad(sdl::PAD_EAST, true), &env, &mut none),
            (Swallow, Toggle::Closed)
        );
    }

    #[test]
    fn capture_binds_keys_and_chords() {
        let mut r = Router::new();
        let env = Env::default();
        r.set_open(true);
        r.start_capture("m", "k");
        // Modifiers alone don't finish it.
        let shift = Input::Key {
            scancode: sc::LSHIFT,
            mods: sdl::KMOD_LSHIFT,
            down: true,
            repeat: false,
        };
        r.route(&shift, &env, &mut none);
        assert!(r.captured.is_none());
        let shift1 = Input::Key {
            scancode: sc::N1,
            mods: sdl::KMOD_LSHIFT,
            down: true,
            repeat: false,
        };
        assert_eq!(r.route(&shift1, &env, &mut none).0, Swallow);
        assert_eq!(
            r.captured.take(),
            Some(("m".into(), "k".into(), "Shift+1".into()))
        );
        // Pad: A was held when capture started (it pressed "Change"); its
        // release doesn't finish the capture.
        r.route(&pad(sdl::PAD_SOUTH, true), &env, &mut none);
        r.start_capture("m", "k");
        r.route(&pad(sdl::PAD_SOUTH, false), &env, &mut none);
        assert!(r.captured.is_none());
        r.route(&pad(sdl::PAD_NORTH, true), &env, &mut none);
        assert!(r.captured.is_none());
        r.route(&pad(sdl::PAD_NORTH, false), &env, &mut none);
        assert_eq!(r.captured.take().unwrap().2, "Pad:Y");
        // Escape cancels.
        r.start_capture("m", "k");
        r.route(&key(sc::ESCAPE, true), &env, &mut none);
        assert!(r.capture.is_none() && r.captured.is_none());
        assert!(
            r.open,
            "Escape that cancels a capture doesn't close the menu"
        );
    }

    #[test]
    fn capture_pad_chord() {
        let mut r = Router::new();
        let env = Env::default();
        r.set_open(true);
        r.start_capture("m", "k");
        // LB, then X while LB is held; the first release finishes it.
        r.route(&pad(sdl::PAD_LEFT_SHOULDER, true), &env, &mut none);
        r.route(&pad(sdl::PAD_WEST, true), &env, &mut none);
        assert!(r.captured.is_none());
        assert_eq!(
            r.route(&pad(sdl::PAD_WEST, false), &env, &mut none).0,
            Swallow
        );
        assert_eq!(r.captured.take().unwrap().2, "Pad:X+Pad:LB");
        assert_eq!(
            r.route(&pad(sdl::PAD_LEFT_SHOULDER, false), &env, &mut none)
                .0,
            Swallow
        );
        assert!(r.captured.is_none());
    }

    #[test]
    fn triggers_act_as_buttons() {
        let mut r = Router::new();
        let env = Env::default();
        let mut rt = |ev: &InputEvent, _: u64| *ev == InputEvent::PadDown { button: PAD_RT };
        let axis = |value| Input::PadAxis {
            axis: sdl::AXIS_RIGHT_TRIGGER,
            value,
        };
        assert_eq!(r.route(&axis(5000), &env, &mut rt).0, Deliver);
        assert_eq!(r.route(&axis(30000), &env, &mut rt).0, Swallow);
        assert_eq!(r.swallowed_pad, pad_bit(PAD_RT));
        assert_eq!(r.route(&axis(20000), &env, &mut rt).0, Swallow);
        assert_eq!(r.route(&axis(0), &env, &mut rt).0, Deliver);
        assert_eq!(r.swallowed_pad, 0);
    }

    #[test]
    fn escape_closes_unless_typing() {
        let mut r = Router::new();
        let env = Env::default();
        r.set_open(true);
        r.ui_wants_keyboard = true;
        assert_eq!(
            r.route(&key(sc::ESCAPE, true), &env, &mut none),
            (Swallow, Toggle::None)
        );
        r.ui_wants_keyboard = false;
        assert_eq!(
            r.route(&key(sc::ESCAPE, true), &env, &mut none),
            (Swallow, Toggle::Closed)
        );
    }

    #[test]
    fn pointer_scaled_to_points() {
        let mut r = Router::new();
        r.set_open(true);
        r.to_points = 0.5;
        r.route(
            &Input::MouseMotion { x: 100.0, y: 40.0 },
            &Env::default(),
            &mut none,
        );
        assert_eq!(
            r.ui_events,
            vec![UiEvent::PointerMoved(Pos2::new(50.0, 20.0))]
        );
    }
}
