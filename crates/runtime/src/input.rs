//! Input events as mods see them, independent of SDL (PLAN §4). The overlay
//! converts SDL events into these.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyMods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// SDL scancode (`SDL_Scancode` value), modifiers held, auto-repeat flag.
    KeyDown {
        scancode: u32,
        mods: KeyMods,
        repeat: bool,
    },
    KeyUp {
        scancode: u32,
        mods: KeyMods,
    },
    /// SDL gamepad button (`SDL_GamepadButton` value).
    PadDown {
        button: u32,
    },
    PadUp {
        button: u32,
    },
}
