//! The parts of the SDL3 ABI the overlay reads. Layouts and constants are
//! from SDL 3.4.18's headers (`SDL_events.h`, `SDL_gamepad.h`,
//! `SDL_scancode.h`, `SDL_keycode.h`), the version the game ships; they are
//! unchanged since 3.2.0 except where noted.

#![allow(dead_code)] // the full constant set documents the ABI

use std::ffi::{CStr, c_char};

// SDL_EventType
pub const EVENT_QUIT: u32 = 0x100;
pub const EVENT_WINDOW_FOCUS_LOST: u32 = 0x20f;
pub const EVENT_KEY_DOWN: u32 = 0x300;
pub const EVENT_KEY_UP: u32 = 0x301;
pub const EVENT_TEXT_EDITING: u32 = 0x302;
pub const EVENT_TEXT_INPUT: u32 = 0x303;
pub const EVENT_MOUSE_MOTION: u32 = 0x400;
pub const EVENT_MOUSE_BUTTON_DOWN: u32 = 0x401;
pub const EVENT_MOUSE_BUTTON_UP: u32 = 0x402;
pub const EVENT_MOUSE_WHEEL: u32 = 0x403;
pub const EVENT_GAMEPAD_AXIS_MOTION: u32 = 0x650;
pub const EVENT_GAMEPAD_BUTTON_DOWN: u32 = 0x651;
pub const EVENT_GAMEPAD_BUTTON_UP: u32 = 0x652;

// SDL_Keymod
pub const KMOD_LSHIFT: u16 = 0x0001;
pub const KMOD_RSHIFT: u16 = 0x0002;
pub const KMOD_LCTRL: u16 = 0x0040;
pub const KMOD_RCTRL: u16 = 0x0080;
pub const KMOD_LALT: u16 = 0x0100;
pub const KMOD_RALT: u16 = 0x0200;
pub const KMOD_SHIFT: u16 = KMOD_LSHIFT | KMOD_RSHIFT;
pub const KMOD_CTRL: u16 = KMOD_LCTRL | KMOD_RCTRL;
pub const KMOD_ALT: u16 = KMOD_LALT | KMOD_RALT;

// SDL_GamepadButton
pub const PAD_SOUTH: u32 = 0;
pub const PAD_EAST: u32 = 1;
pub const PAD_WEST: u32 = 2;
pub const PAD_NORTH: u32 = 3;
pub const PAD_BACK: u32 = 4;
pub const PAD_GUIDE: u32 = 5;
pub const PAD_START: u32 = 6;
pub const PAD_LEFT_STICK: u32 = 7;
pub const PAD_RIGHT_STICK: u32 = 8;
pub const PAD_LEFT_SHOULDER: u32 = 9;
pub const PAD_RIGHT_SHOULDER: u32 = 10;
pub const PAD_DPAD_UP: u32 = 11;
pub const PAD_DPAD_DOWN: u32 = 12;
pub const PAD_DPAD_LEFT: u32 = 13;
pub const PAD_DPAD_RIGHT: u32 = 14;
pub const PAD_MISC1: u32 = 15;
pub const PAD_RIGHT_PADDLE1: u32 = 16;
pub const PAD_LEFT_PADDLE1: u32 = 17;
pub const PAD_RIGHT_PADDLE2: u32 = 18;
pub const PAD_LEFT_PADDLE2: u32 = 19;
pub const PAD_TOUCHPAD: u32 = 20;
/// `SDL_GAMEPAD_BUTTON_COUNT` in 3.4 (MISC2..MISC6 follow TOUCHPAD).
pub const PAD_BUTTON_COUNT: u32 = 26;

// SDL_GamepadAxis
pub const AXIS_LEFTX: u8 = 0;
pub const AXIS_LEFTY: u8 = 1;
pub const AXIS_RIGHTX: u8 = 2;
pub const AXIS_RIGHTY: u8 = 3;
pub const AXIS_LEFT_TRIGGER: u8 = 4;
pub const AXIS_RIGHT_TRIGGER: u8 = 5;

// SDL mouse buttons
pub const BUTTON_LEFT: u8 = 1;
pub const BUTTON_MIDDLE: u8 = 2;
pub const BUTTON_RIGHT: u8 = 3;
pub const BUTTON_X1: u8 = 4;
pub const BUTTON_X2: u8 = 5;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct KeyboardEvent {
    pub r#type: u32,
    pub reserved: u32,
    pub timestamp: u64,
    pub window_id: u32,
    pub which: u32,
    pub scancode: u32,
    pub key: u32,
    pub r#mod: u16,
    pub raw: u16,
    pub down: bool,
    pub repeat: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TextInputEvent {
    pub r#type: u32,
    pub reserved: u32,
    pub timestamp: u64,
    pub window_id: u32,
    pub text: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MouseMotionEvent {
    pub r#type: u32,
    pub reserved: u32,
    pub timestamp: u64,
    pub window_id: u32,
    pub which: u32,
    pub state: u32,
    pub x: f32,
    pub y: f32,
    pub xrel: f32,
    pub yrel: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MouseButtonEvent {
    pub r#type: u32,
    pub reserved: u32,
    pub timestamp: u64,
    pub window_id: u32,
    pub which: u32,
    pub button: u8,
    pub down: bool,
    pub clicks: u8,
    pub padding: u8,
    pub x: f32,
    pub y: f32,
}

/// `integer_x`/`integer_y` were added in 3.2.12.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MouseWheelEvent {
    pub r#type: u32,
    pub reserved: u32,
    pub timestamp: u64,
    pub window_id: u32,
    pub which: u32,
    pub x: f32,
    pub y: f32,
    pub direction: u32,
    pub mouse_x: f32,
    pub mouse_y: f32,
    pub integer_x: i32,
    pub integer_y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GamepadAxisEvent {
    pub r#type: u32,
    pub reserved: u32,
    pub timestamp: u64,
    pub which: u32,
    pub axis: u8,
    pub padding1: u8,
    pub padding2: u8,
    pub padding3: u8,
    pub value: i16,
    pub padding4: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GamepadButtonEvent {
    pub r#type: u32,
    pub reserved: u32,
    pub timestamp: u64,
    pub which: u32,
    pub button: u8,
    pub down: bool,
    pub padding1: u8,
    pub padding2: u8,
}

/// `SDL_Event`: a 128-byte union whose first field is the event type.
#[repr(C)]
#[derive(Clone, Copy)]
pub union Event {
    pub r#type: u32,
    pub key: KeyboardEvent,
    pub text: TextInputEvent,
    pub motion: MouseMotionEvent,
    pub button: MouseButtonEvent,
    pub wheel: MouseWheelEvent,
    pub gaxis: GamepadAxisEvent,
    pub gbutton: GamepadButtonEvent,
    pub padding: [u8; 128],
}

/// An SDL event decoded into what the overlay cares about.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Key {
        scancode: u32,
        mods: u16,
        down: bool,
        repeat: bool,
    },
    Text(String),
    MouseMotion {
        x: f32,
        y: f32,
    },
    MouseButton {
        button: u8,
        down: bool,
        x: f32,
        y: f32,
    },
    Wheel {
        x: f32,
        y: f32,
    },
    PadButton {
        button: u32,
        down: bool,
    },
    PadAxis {
        axis: u8,
        value: i16,
    },
    FocusLost,
    /// Anything else, by event type; always passed to the game.
    Other(u32),
}

impl Event {
    pub fn zeroed() -> Event {
        Event { padding: [0; 128] }
    }

    /// Decodes the event.
    ///
    /// # Safety
    /// `self` must be an event filled in by SDL (for text input, `text` must
    /// point to a NUL-terminated string or be null).
    pub unsafe fn decode(&self) -> Input {
        // SAFETY: every variant starts with the u32 type; the union is 128
        // bytes and fully initialized by SDL (or by `zeroed`).
        unsafe {
            match self.r#type {
                EVENT_KEY_DOWN | EVENT_KEY_UP => Input::Key {
                    scancode: self.key.scancode,
                    mods: self.key.r#mod,
                    down: self.key.down,
                    repeat: self.key.repeat,
                },
                EVENT_TEXT_INPUT => {
                    let p = self.text.text;
                    if p.is_null() {
                        Input::Text(String::new())
                    } else {
                        Input::Text(CStr::from_ptr(p).to_string_lossy().into_owned())
                    }
                }
                EVENT_MOUSE_MOTION => Input::MouseMotion {
                    x: self.motion.x,
                    y: self.motion.y,
                },
                EVENT_MOUSE_BUTTON_DOWN | EVENT_MOUSE_BUTTON_UP => Input::MouseButton {
                    button: self.button.button,
                    down: self.button.down,
                    x: self.button.x,
                    y: self.button.y,
                },
                EVENT_MOUSE_WHEEL => {
                    // SDL_MOUSEWHEEL_FLIPPED = 1: values are reversed.
                    let s = if self.wheel.direction == 1 { -1.0 } else { 1.0 };
                    Input::Wheel {
                        x: self.wheel.x * s,
                        y: self.wheel.y * s,
                    }
                }
                EVENT_GAMEPAD_BUTTON_DOWN | EVENT_GAMEPAD_BUTTON_UP => Input::PadButton {
                    button: self.gbutton.button as u32,
                    down: self.gbutton.down,
                },
                EVENT_GAMEPAD_AXIS_MOTION => Input::PadAxis {
                    axis: self.gaxis.axis,
                    value: self.gaxis.value,
                },
                EVENT_WINDOW_FOCUS_LOST => Input::FocusLost,
                t => Input::Other(t),
            }
        }
    }
}

// SDL_Scancode values used by key names and the egui key map.
pub mod sc {
    pub const A: u32 = 4; // ..Z = 29
    pub const N1: u32 = 30; // ..N9 = 38
    pub const N0: u32 = 39;
    pub const RETURN: u32 = 40;
    pub const ESCAPE: u32 = 41;
    pub const BACKSPACE: u32 = 42;
    pub const TAB: u32 = 43;
    pub const SPACE: u32 = 44;
    pub const MINUS: u32 = 45;
    pub const EQUALS: u32 = 46;
    pub const LEFTBRACKET: u32 = 47;
    pub const RIGHTBRACKET: u32 = 48;
    pub const BACKSLASH: u32 = 49;
    pub const SEMICOLON: u32 = 51;
    pub const APOSTROPHE: u32 = 52;
    pub const GRAVE: u32 = 53;
    pub const COMMA: u32 = 54;
    pub const PERIOD: u32 = 55;
    pub const SLASH: u32 = 56;
    pub const CAPSLOCK: u32 = 57;
    pub const F1: u32 = 58; // ..F12 = 69
    pub const PRINTSCREEN: u32 = 70;
    pub const SCROLLLOCK: u32 = 71;
    pub const PAUSE: u32 = 72;
    pub const INSERT: u32 = 73;
    pub const HOME: u32 = 74;
    pub const PAGEUP: u32 = 75;
    pub const DELETE: u32 = 76;
    pub const END: u32 = 77;
    pub const PAGEDOWN: u32 = 78;
    pub const RIGHT: u32 = 79;
    pub const LEFT: u32 = 80;
    pub const DOWN: u32 = 81;
    pub const UP: u32 = 82;
    pub const KP_DIVIDE: u32 = 84;
    pub const KP_MULTIPLY: u32 = 85;
    pub const KP_MINUS: u32 = 86;
    pub const KP_PLUS: u32 = 87;
    pub const KP_ENTER: u32 = 88;
    pub const KP_1: u32 = 89; // ..KP_9 = 97
    pub const KP_0: u32 = 98;
    pub const KP_PERIOD: u32 = 99;
    pub const F13: u32 = 104; // ..F24 = 115
    pub const LCTRL: u32 = 224;
    pub const LSHIFT: u32 = 225;
    pub const LALT: u32 = 226;
    pub const LGUI: u32 = 227;
    pub const RCTRL: u32 = 228;
    pub const RSHIFT: u32 = 229;
    pub const RALT: u32 = 230;
    pub const RGUI: u32 = 231;

    pub fn is_modifier(s: u32) -> bool {
        (LCTRL..=RGUI).contains(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn layouts_match_sdl3() {
        assert_eq!(size_of::<Event>(), 128);
        assert_eq!(offset_of!(KeyboardEvent, timestamp), 8);
        assert_eq!(offset_of!(KeyboardEvent, window_id), 16);
        assert_eq!(offset_of!(KeyboardEvent, scancode), 24);
        assert_eq!(offset_of!(KeyboardEvent, key), 28);
        assert_eq!(offset_of!(KeyboardEvent, r#mod), 32);
        assert_eq!(offset_of!(KeyboardEvent, raw), 34);
        assert_eq!(offset_of!(KeyboardEvent, down), 36);
        assert_eq!(offset_of!(KeyboardEvent, repeat), 37);
        assert_eq!(offset_of!(TextInputEvent, text), 24);
        assert_eq!(offset_of!(MouseMotionEvent, state), 24);
        assert_eq!(offset_of!(MouseMotionEvent, x), 28);
        assert_eq!(offset_of!(MouseButtonEvent, button), 24);
        assert_eq!(offset_of!(MouseButtonEvent, down), 25);
        assert_eq!(offset_of!(MouseButtonEvent, x), 28);
        assert_eq!(offset_of!(MouseWheelEvent, x), 24);
        assert_eq!(offset_of!(MouseWheelEvent, direction), 32);
        assert_eq!(offset_of!(MouseWheelEvent, integer_y), 48);
        assert_eq!(offset_of!(GamepadAxisEvent, which), 16);
        assert_eq!(offset_of!(GamepadAxisEvent, axis), 20);
        assert_eq!(offset_of!(GamepadAxisEvent, value), 24);
        assert_eq!(offset_of!(GamepadButtonEvent, button), 20);
        assert_eq!(offset_of!(GamepadButtonEvent, down), 21);
    }

    #[test]
    fn decodes_raw_bytes() {
        // A key-down for F10 with left shift, built byte by byte.
        let mut e = Event::zeroed();
        // SAFETY: plain bytes.
        let b = unsafe { &mut e.padding };
        b[0..4].copy_from_slice(&EVENT_KEY_DOWN.to_le_bytes());
        b[24..28].copy_from_slice(&67u32.to_le_bytes());
        b[32..34].copy_from_slice(&KMOD_LSHIFT.to_le_bytes());
        b[36] = 1;
        assert_eq!(
            unsafe { e.decode() },
            Input::Key {
                scancode: 67,
                mods: KMOD_LSHIFT,
                down: true,
                repeat: false
            }
        );
        let mut e = Event::zeroed();
        let b = unsafe { &mut e.padding };
        b[0..4].copy_from_slice(&EVENT_GAMEPAD_BUTTON_UP.to_le_bytes());
        b[20] = PAD_START as u8;
        assert_eq!(
            unsafe { e.decode() },
            Input::PadButton {
                button: PAD_START,
                down: false
            }
        );
        let text = c"é";
        let e = Event {
            text: TextInputEvent {
                r#type: EVENT_TEXT_INPUT,
                reserved: 0,
                timestamp: 0,
                window_id: 0,
                text: text.as_ptr(),
            },
        };
        assert_eq!(unsafe { e.decode() }, Input::Text("é".into()));
    }
}
