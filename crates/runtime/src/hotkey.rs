//! Hotkeys (PLAN §4): key specs, matching against [`InputEvent`]s, and the
//! per-mod bindings.
//!
//! A hotkey is a mod setting of type `key`; its value is a spec string:
//!
//! - keyboard: optional modifiers and one key, joined by `+`: `S`, `Shift+1`,
//!   `Ctrl+Alt+F5`, `F10`, `Num5`. Modifiers are `Ctrl`, `Shift`, `Alt` and
//!   must match exactly (`S` does not fire while Shift is held). Keys are
//!   physical positions (SDL scancodes) named after the US layout.
//! - gamepad: one or more buttons, each prefixed `Pad:`, joined by `+`:
//!   `Pad:Y`, `Pad:LB+Pad:X`. A chord fires when its last button goes down
//!   while the others are held. Buttons use Xbox names: `A B X Y LB RB LT RT
//!   LS RS Back Start Guide Up Down Left Right`, plus `Paddle1`..`Paddle4`
//!   (Steam Deck R4 L4 R5 L5), `Misc1`, `Touchpad`. `LT`/`RT` are the
//!   triggers, which SDL reports as axes: they count as pressed past half way.
//! - empty or `None`: unbound.
//!
//! Mods check their hotkeys in `Mod::on_input` with [`pressed`] and return
//! its result, so the event is swallowed when the hotkey fires. The host
//! calls [`set_binding`] for every `key` setting when settings load or change.
//! Hotkeys never fire while the game has a text field focused or the mod
//! menu is open (the overlay doesn't dispatch input to mods then).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::input::{InputEvent, KeyMods};
use crate::overlay::sdl::{self, sc};

/// Virtual button numbers for the triggers (SDL reports them as axes).
pub const PAD_LT: u32 = 0x100;
pub const PAD_RT: u32 = 0x101;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Binding {
    #[default]
    None,
    Key {
        scancode: u32,
        mods: KeyMods,
    },
    /// Gamepad buttons, sorted, no duplicates. The chord fires on the press
    /// that completes it.
    Pad(Vec<u32>),
}

const PAD_NAMES: &[(&str, u32)] = &[
    ("A", sdl::PAD_SOUTH),
    ("B", sdl::PAD_EAST),
    ("X", sdl::PAD_WEST),
    ("Y", sdl::PAD_NORTH),
    ("Back", sdl::PAD_BACK),
    ("Guide", sdl::PAD_GUIDE),
    ("Start", sdl::PAD_START),
    ("LS", sdl::PAD_LEFT_STICK),
    ("RS", sdl::PAD_RIGHT_STICK),
    ("LB", sdl::PAD_LEFT_SHOULDER),
    ("RB", sdl::PAD_RIGHT_SHOULDER),
    ("Up", sdl::PAD_DPAD_UP),
    ("Down", sdl::PAD_DPAD_DOWN),
    ("Left", sdl::PAD_DPAD_LEFT),
    ("Right", sdl::PAD_DPAD_RIGHT),
    ("Misc1", sdl::PAD_MISC1),
    ("Paddle1", sdl::PAD_RIGHT_PADDLE1),
    ("Paddle2", sdl::PAD_LEFT_PADDLE1),
    ("Paddle3", sdl::PAD_RIGHT_PADDLE2),
    ("Paddle4", sdl::PAD_LEFT_PADDLE2),
    ("Touchpad", sdl::PAD_TOUCHPAD),
    ("LT", PAD_LT),
    ("RT", PAD_RT),
    // Aliases (parsed, never printed).
    ("Select", sdl::PAD_BACK),
    ("View", sdl::PAD_BACK),
    ("Menu", sdl::PAD_START),
    ("L1", sdl::PAD_LEFT_SHOULDER),
    ("R1", sdl::PAD_RIGHT_SHOULDER),
    ("L2", PAD_LT),
    ("R2", PAD_RT),
    ("L3", sdl::PAD_LEFT_STICK),
    ("R3", sdl::PAD_RIGHT_STICK),
    ("DpadUp", sdl::PAD_DPAD_UP),
    ("DpadDown", sdl::PAD_DPAD_DOWN),
    ("DpadLeft", sdl::PAD_DPAD_LEFT),
    ("DpadRight", sdl::PAD_DPAD_RIGHT),
];

/// Named keys other than letters, digits, F-keys and numpad digits.
const KEY_NAMES: &[(&str, u32)] = &[
    ("Enter", sc::RETURN),
    ("Escape", sc::ESCAPE),
    ("Backspace", sc::BACKSPACE),
    ("Tab", sc::TAB),
    ("Space", sc::SPACE),
    ("Minus", sc::MINUS),
    ("Equals", sc::EQUALS),
    ("LeftBracket", sc::LEFTBRACKET),
    ("RightBracket", sc::RIGHTBRACKET),
    ("Backslash", sc::BACKSLASH),
    ("Semicolon", sc::SEMICOLON),
    ("Apostrophe", sc::APOSTROPHE),
    ("Grave", sc::GRAVE),
    ("Comma", sc::COMMA),
    ("Period", sc::PERIOD),
    ("Slash", sc::SLASH),
    ("CapsLock", sc::CAPSLOCK),
    ("PrintScreen", sc::PRINTSCREEN),
    ("ScrollLock", sc::SCROLLLOCK),
    ("Pause", sc::PAUSE),
    ("Insert", sc::INSERT),
    ("Home", sc::HOME),
    ("PageUp", sc::PAGEUP),
    ("Delete", sc::DELETE),
    ("End", sc::END),
    ("PageDown", sc::PAGEDOWN),
    ("Right", sc::RIGHT),
    ("Left", sc::LEFT),
    ("Down", sc::DOWN),
    ("Up", sc::UP),
    ("NumDivide", sc::KP_DIVIDE),
    ("NumMultiply", sc::KP_MULTIPLY),
    ("NumMinus", sc::KP_MINUS),
    ("NumPlus", sc::KP_PLUS),
    ("NumEnter", sc::KP_ENTER),
    ("NumPeriod", sc::KP_PERIOD),
    // Aliases.
    ("Return", sc::RETURN),
    ("Esc", sc::ESCAPE),
    ("Del", sc::DELETE),
    ("Ins", sc::INSERT),
    ("PgUp", sc::PAGEUP),
    ("PgDn", sc::PAGEDOWN),
    ("Backquote", sc::GRAVE),
    ("Tilde", sc::GRAVE),
];

/// Canonical name of a scancode, if it has one.
pub fn key_name(scancode: u32) -> Option<String> {
    match scancode {
        s if (sc::A..sc::A + 26).contains(&s) => Some(((b'A' + (s - sc::A) as u8) as char).into()),
        s if (sc::N1..sc::N1 + 9).contains(&s) => Some((s - sc::N1 + 1).to_string()),
        sc::N0 => Some("0".into()),
        s if (sc::F1..sc::F1 + 12).contains(&s) => Some(format!("F{}", s - sc::F1 + 1)),
        s if (sc::F13..sc::F13 + 12).contains(&s) => Some(format!("F{}", s - sc::F13 + 13)),
        s if (sc::KP_1..sc::KP_1 + 9).contains(&s) => Some(format!("Num{}", s - sc::KP_1 + 1)),
        sc::KP_0 => Some("Num0".into()),
        s => KEY_NAMES
            .iter()
            .find(|(_, c)| *c == s)
            .map(|(n, _)| (*n).into()),
    }
}

fn parse_key(name: &str) -> Option<u32> {
    let up = name.to_ascii_uppercase();
    let b = up.as_bytes();
    if b.len() == 1 {
        return match b[0] {
            c @ b'A'..=b'Z' => Some(sc::A + (c - b'A') as u32),
            b'0' => Some(sc::N0),
            c @ b'1'..=b'9' => Some(sc::N1 + (c - b'1') as u32),
            _ => None,
        };
    }
    if let Some(n) = up.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        return match n {
            1..=12 => Some(sc::F1 + n - 1),
            13..=24 => Some(sc::F13 + n - 13),
            _ => None,
        };
    }
    if let Some(n) = up.strip_prefix("NUM").and_then(|n| n.parse::<u32>().ok()) {
        return match n {
            0 => Some(sc::KP_0),
            1..=9 => Some(sc::KP_1 + n - 1),
            _ => None,
        };
    }
    KEY_NAMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, c)| *c)
}

pub fn pad_name(button: u32) -> String {
    PAD_NAMES
        .iter()
        .find(|(_, b)| *b == button)
        .map_or_else(|| format!("Button{button}"), |(n, _)| (*n).into())
}

fn parse_pad(name: &str) -> Option<u32> {
    if let Some(n) = name
        .strip_prefix("Button")
        .and_then(|n| n.parse::<u32>().ok())
    {
        return (n < sdl::PAD_BUTTON_COUNT).then_some(n);
    }
    PAD_NAMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, b)| *b)
}

impl FromStr for Binding {
    type Err = String;

    fn from_str(spec: &str) -> Result<Binding, String> {
        let spec = spec.trim();
        if spec.is_empty() || spec.eq_ignore_ascii_case("none") {
            return Ok(Binding::None);
        }
        let parts: Vec<&str> = spec.split('+').map(str::trim).collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(format!("empty key name in {spec:?}"));
        }
        let is_pad = |p: &&str| p.len() > 4 && p[..4].eq_ignore_ascii_case("pad:");
        if parts.iter().any(is_pad) {
            if !parts.iter().all(is_pad) {
                return Err(format!(
                    "{spec:?} mixes keyboard and gamepad; prefix every button with Pad:"
                ));
            }
            let mut buttons = Vec::new();
            for p in &parts {
                let b = parse_pad(&p[4..]).ok_or_else(|| format!("unknown gamepad button {p:?}"))?;
                if !buttons.contains(&b) {
                    buttons.push(b);
                }
            }
            buttons.sort_unstable();
            return Ok(Binding::Pad(buttons));
        }
        let (key, mod_names) = parts.split_last().expect("split is never empty");
        let mut mods = KeyMods::default();
        for m in mod_names {
            let flag = match m.to_ascii_lowercase().as_str() {
                "shift" => &mut mods.shift,
                "ctrl" | "control" => &mut mods.ctrl,
                "alt" => &mut mods.alt,
                _ => return Err(format!("unknown modifier {m:?} (use Ctrl, Shift or Alt)")),
            };
            *flag = true;
        }
        let scancode = parse_key(key).ok_or_else(|| format!("unknown key {key:?}"))?;
        Ok(Binding::Key { scancode, mods })
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Binding::None => Ok(()),
            Binding::Key { scancode, mods } => {
                for (on, name) in [(mods.ctrl, "Ctrl+"), (mods.shift, "Shift+"), (mods.alt, "Alt+")] {
                    if on {
                        f.write_str(name)?;
                    }
                }
                match key_name(*scancode) {
                    Some(n) => f.write_str(&n),
                    None => write!(f, "Key{scancode}"),
                }
            }
            Binding::Pad(buttons) => {
                for (i, b) in buttons.iter().enumerate() {
                    if i > 0 {
                        f.write_str("+")?;
                    }
                    write!(f, "Pad:{}", pad_name(*b))?;
                }
                Ok(())
            }
        }
    }
}

/// Bit for a pad button in a held/swallowed mask (triggers at 32 and 33).
pub fn pad_bit(button: u32) -> u64 {
    match button {
        PAD_LT => 1 << 32,
        PAD_RT => 1 << 33,
        b if b < 32 => 1 << b,
        _ => 0,
    }
}

impl Binding {
    pub fn is_none(&self) -> bool {
        matches!(self, Binding::None)
    }

    /// Whether `ev` fires this binding. `pad_held` is the mask of pad buttons
    /// held *before* `ev` (see [`pad_bit`]). Key repeats never fire.
    pub fn matches(&self, ev: &InputEvent, pad_held: u64) -> bool {
        match (self, ev) {
            (
                Binding::Key { scancode, mods },
                InputEvent::KeyDown {
                    scancode: s,
                    mods: m,
                    repeat: false,
                },
            ) => scancode == s && mods == m,
            (Binding::Pad(chord), InputEvent::PadDown { button }) => {
                chord.contains(button)
                    && chord
                        .iter()
                        .filter(|b| *b != button)
                        .all(|b| pad_held & pad_bit(*b) != 0)
            }
            _ => false,
        }
    }
}

static BINDINGS: Mutex<BTreeMap<(String, String), Binding>> = Mutex::new(BTreeMap::new());
static PAD_HELD: AtomicU64 = AtomicU64::new(0);
static SUPPRESSED: AtomicBool = AtomicBool::new(false);

/// Sets the binding of hotkey `name` of mod `mod_id` from a spec string.
/// On a bad spec the hotkey is unbound and the error returned (log it).
pub fn set_binding(mod_id: &str, name: &str, spec: &str) -> Result<(), String> {
    let parsed = spec.parse::<Binding>();
    let mut map = BINDINGS.lock().unwrap_or_else(|e| e.into_inner());
    let key = (mod_id.to_owned(), name.to_owned());
    match parsed {
        Ok(b) => {
            map.insert(key, b);
            Ok(())
        }
        Err(e) => {
            map.insert(key, Binding::None);
            Err(e)
        }
    }
}

pub fn binding(mod_id: &str, name: &str) -> Binding {
    BINDINGS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(mod_id.to_owned(), name.to_owned()))
        .cloned()
        .unwrap_or_default()
}

/// Removes all of a mod's bindings (on unload).
pub fn clear(mod_id: &str) {
    BINDINGS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(m, _), _| m != mod_id);
}

/// Whether `ev` fires mod `mod_id`'s hotkey `name`. Call from
/// `Mod::on_input` and return true (swallow) when it does.
pub fn pressed(mod_id: &str, name: &str, ev: &InputEvent) -> bool {
    if SUPPRESSED.load(Ordering::Relaxed) {
        return false;
    }
    binding(mod_id, name).matches(ev, PAD_HELD.load(Ordering::Relaxed))
}

/// Set by the overlay: the pad buttons held before the event being
/// dispatched, and whether hotkeys are suppressed (text input or menu).
pub(crate) fn set_input_state(pad_held: u64, suppressed: bool) {
    PAD_HELD.store(pad_held, Ordering::Relaxed);
    SUPPRESSED.store(suppressed, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Binding {
        s.parse().unwrap()
    }

    fn down(scancode: u32, shift: bool) -> InputEvent {
        InputEvent::KeyDown {
            scancode,
            mods: KeyMods {
                shift,
                ..KeyMods::default()
            },
            repeat: false,
        }
    }

    #[test]
    fn parses_and_prints() {
        for (spec, canon) in [
            ("S", "S"),
            ("shift+1", "Shift+1"),
            ("F10", "F10"),
            ("alt + ctrl + f5", "Ctrl+Alt+F5"),
            ("Num0", "Num0"),
            ("esc", "Escape"),
            ("F24", "F24"),
            ("Pad:Y", "Pad:Y"),
            ("Pad:X+Pad:LB", "Pad:X+Pad:LB"),
            ("pad:l3+pad:r3", "Pad:LS+Pad:RS"),
            ("Pad:RT", "Pad:RT"),
            ("", ""),
            ("None", ""),
        ] {
            let parsed = b(spec);
            assert_eq!(parsed.to_string(), canon, "{spec}");
            assert_eq!(b(canon), parsed, "{canon} round trip");
        }
        assert_eq!(
            b("Pad:LB+Pad:X"),
            Binding::Pad(vec![sdl::PAD_WEST, sdl::PAD_LEFT_SHOULDER])
        );
        assert_eq!(
            b("Shift+1"),
            Binding::Key {
                scancode: sc::N1,
                mods: KeyMods {
                    shift: true,
                    ..KeyMods::default()
                }
            }
        );
    }

    #[test]
    fn rejects_bad_specs() {
        for spec in ["Hyper+S", "Shift+", "F25", "Pad:Q", "Shift+Pad:A", "Pad:A+B", "Num10", "SS"] {
            assert!(spec.parse::<Binding>().is_err(), "{spec}");
        }
    }

    #[test]
    fn every_scancode_name_round_trips() {
        for s in 0..300 {
            if let Some(n) = key_name(s) {
                assert_eq!(parse_key(&n), Some(s), "{n}");
            }
        }
    }

    #[test]
    fn key_matching_is_exact() {
        let s = b("S");
        assert!(s.matches(&down(sc::A + 18, false), 0));
        assert!(!s.matches(&down(sc::A + 18, true), 0));
        assert!(b("Shift+S").matches(&down(sc::A + 18, true), 0));
        let repeat = InputEvent::KeyDown {
            scancode: sc::A + 18,
            mods: KeyMods::default(),
            repeat: true,
        };
        assert!(!s.matches(&repeat, 0));
        assert!(!s.matches(
            &InputEvent::KeyUp {
                scancode: sc::A + 18,
                mods: KeyMods::default()
            },
            0
        ));
        assert!(!Binding::None.matches(&down(sc::A, false), 0));
    }

    #[test]
    fn pad_chords() {
        let chord = b("Pad:LB+Pad:X");
        let x = InputEvent::PadDown {
            button: sdl::PAD_WEST,
        };
        let lb = InputEvent::PadDown {
            button: sdl::PAD_LEFT_SHOULDER,
        };
        assert!(!chord.matches(&x, 0));
        assert!(chord.matches(&x, pad_bit(sdl::PAD_LEFT_SHOULDER)));
        // Either order completes it.
        assert!(chord.matches(&lb, pad_bit(sdl::PAD_WEST)));
        assert!(b("Pad:Y").matches(
            &InputEvent::PadDown {
                button: sdl::PAD_NORTH
            },
            0
        ));
        assert!(b("Pad:LT").matches(&InputEvent::PadDown { button: PAD_LT }, 0));
        assert_eq!(pad_bit(PAD_RT), 1 << 33);
    }

    #[test]
    fn registry() {
        set_binding("test-mod", "sort", "S").unwrap();
        let ev = down(sc::A + 18, false);
        set_input_state(0, false);
        assert!(pressed("test-mod", "sort", &ev));
        assert!(!pressed("test-mod", "other", &ev));
        assert!(set_binding("test-mod", "sort", "Bogus+S").is_err());
        assert!(!pressed("test-mod", "sort", &ev));
        clear("test-mod");
        assert_eq!(binding("test-mod", "sort"), Binding::None);
    }
}
