//! Drawing on the game's HUD: where the game's GUI is on screen, the named
//! rects of its HUD layout, and when the HUD is up.
//!
//! # The game's GUI space (game 1.0.5)
//!
//! The game lays out and draws its GUI in a virtual 640 x 360 space
//! (`GuiCamera_Setup`, run every frame: both constants are in the exe). The
//! space is fitted into the window keeping its aspect ratio and centred,
//! with black bars on the long side, and without rounding the scale:
//!
//! ```text
//! s      = min(window_w / 640, window_h / 360)
//! offset = ((window_w - 640 s) / 2, (window_h - 360 s) / 2)
//! pixel  = offset + gui * s          (y down, window pixels)
//! ```
//!
//! The game keeps the same values as its letterbox viewport, and the mouse
//! goes back through the inverse of this transform. (Its GUI camera also
//! adds a screen-shake offset while shaking; widgets here don't follow it.)
//!
//! This module works in egui points, the overlay's unit: pixels divided by
//! the overlay's pixels-per-point (see [`Transform`]).
//!
//! # Layout
//!
//! The HUD's rects come from the `gui` table of `def_def` (loaded once into
//! the `GuiLayout` global), as `{x, y, w, h}` (f32) in GUI space; the
//! names [`rect`] knows are in [`LAYOUT`]. The game code reads them every
//! frame, so they are where the HUD is drawn.
//!
//! # Drawing
//!
//! Mods draw with [`crate::host::Mod::hud`]; see CONTRIBUTING, "HUD
//! widgets". The overlay calls it each frame while [`shown`] and the mod
//! menu is closed, and lets through to the game every click that doesn't
//! land on a mod's clickable widget.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use egui::{Pos2, Rect, Vec2};

/// Size of the game's virtual GUI space.
pub const GUI_SIZE: Vec2 = Vec2::new(640.0, 360.0);

/// GUI space to overlay points for one window size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// Window pixels per GUI unit.
    pub scale: f32,
    /// Top-left corner of the GUI space in window pixels.
    pub offset: Vec2,
    /// The overlay's pixels per point.
    pub pixels_per_point: f32,
}

impl Transform {
    /// For a window of `w` x `h` pixels and the overlay's pixels per point.
    pub fn new(w: f32, h: f32, pixels_per_point: f32) -> Transform {
        let scale = (w / GUI_SIZE.x).min(h / GUI_SIZE.y).max(0.0);
        Transform {
            scale,
            offset: Vec2::new(
                (w - GUI_SIZE.x * scale) / 2.0,
                (h - GUI_SIZE.y * scale) / 2.0,
            ),
            pixels_per_point: pixels_per_point.max(f32::MIN_POSITIVE),
        }
    }

    /// A GUI point in window pixels.
    pub fn to_pixels(&self, gui: Pos2) -> Pos2 {
        Pos2::new(
            self.offset.x + gui.x * self.scale,
            self.offset.y + gui.y * self.scale,
        )
    }

    /// A GUI point in overlay points.
    pub fn to_points(&self, gui: Pos2) -> Pos2 {
        let p = self.to_pixels(gui);
        Pos2::new(p.x / self.pixels_per_point, p.y / self.pixels_per_point)
    }

    /// A GUI rect in overlay points.
    pub fn rect_to_points(&self, gui: Rect) -> Rect {
        Rect::from_min_max(self.to_points(gui.min), self.to_points(gui.max))
    }

    /// An overlay point in GUI space.
    pub fn from_points(&self, p: Pos2) -> Pos2 {
        let s = self.scale.max(f32::MIN_POSITIVE);
        Pos2::new(
            (p.x * self.pixels_per_point - self.offset.x) / s,
            (p.y * self.pixels_per_point - self.offset.y) / s,
        )
    }

    /// Overlay points per GUI unit (to size text and widgets like the HUD).
    pub fn points_per_unit(&self) -> f32 {
        self.scale / self.pixels_per_point
    }
}

static TRANSFORM: Mutex<Option<Transform>> = Mutex::new(None);

/// Called by the overlay each frame with the window's size in pixels.
#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows glue
pub(crate) fn set_window(w: i32, h: i32, pixels_per_point: f32) {
    let t = (w > 0 && h > 0).then(|| Transform::new(w as f32, h as f32, pixels_per_point));
    *TRANSFORM.lock().unwrap_or_else(|e| e.into_inner()) = t;
}

/// The current transform; `None` before the first frame.
pub fn transform() -> Option<Transform> {
    *TRANSFORM.lock().unwrap_or_else(|e| e.into_inner())
}

/// A GUI-space point in overlay points.
pub fn to_screen(gui: Pos2) -> Option<Pos2> {
    transform().map(|t| t.to_points(gui))
}

/// A GUI-space rect in overlay points.
pub fn rect_to_screen(gui: Rect) -> Option<Rect> {
    transform().map(|t| t.rect_to_points(gui))
}

/// Overlay points per GUI unit; 1 before the first frame.
pub fn points_per_unit() -> f32 {
    transform().map_or(1.0, |t| t.points_per_unit())
}

// ---------------------------------------------------------------------------
// Layout

/// Named rects of the game's HUD layout: `def_def`'s `gui` table entry and
/// the offset of its `{x, y, w, h}` (f32) in the `GuiLayout` global.
/// `face_hand_slot_(2i+1)` is hero i's weapon hand, `face_hand_slot_(2i+2)`
/// the skill slot (hero 0 to 3 in party order).
pub const LAYOUT: &[(&str, usize)] = &[
    ("dung_view_rect", 0x40),
    ("dung_view_throw_rect", 0x50),
    ("dung_view_drop_rect", 0x60),
    ("inventory_rect", 0x70),
    ("barbie_rect", 0x80),
    ("face_rect_1", 0x90),
    ("face_rect_2", 0xa0),
    ("face_rect_3", 0xb0),
    ("face_rect_4", 0xc0),
    ("face_clickable_rect", 0xd0),
    ("minimap_rect", 0xf8),
    ("select_hero_popup_rect", 0x10c),
    ("face_hand_slot_1", 0x1a4),
    ("face_hand_slot_2", 0x1b4),
    ("face_hand_slot_3", 0x1c4),
    ("face_hand_slot_4", 0x1d4),
    ("face_hand_slot_5", 0x1e4),
    ("face_hand_slot_6", 0x1f4),
    ("face_hand_slot_7", 0x204),
    ("face_hand_slot_8", 0x214),
    ("misc_buttons_rect_1", 0x234),
    ("misc_buttons_rect_2", 0x244),
    ("misc_buttons_rect_3", 0x254),
    ("misc_buttons_rect_4", 0x264),
    ("arrows_rect_1", 0x27c),
    ("arrows_rect_2", 0x28c),
    ("arrows_rect_3", 0x29c),
    ("arrows_rect_4", 0x2ac),
    ("arrows_rect_5", 0x2bc),
    ("arrows_rect_6", 0x2cc),
    ("msgbox_rect", 0x2dc),
    ("encounter_rect", 0x2fc),
];

/// The weapon hand slot of hero `i` (0..4, party order).
pub fn weapon_slot_name(i: usize) -> String {
    format!("face_hand_slot_{}", 2 * i + 1)
}

/// The skill slot of hero `i` (0..4, party order).
pub fn skill_slot_name(i: usize) -> String {
    format!("face_hand_slot_{}", 2 * i + 2)
}

/// Absolute addresses from the core patterns (0 = not found).
static GUI_LAYOUT: AtomicUsize = AtomicUsize::new(0);
static GAME_STATE: AtomicUsize = AtomicUsize::new(0);
static HUD_MODE: AtomicUsize = AtomicUsize::new(0);
static MENU_STATE: AtomicUsize = AtomicUsize::new(0);
static INTERACTION: AtomicUsize = AtomicUsize::new(0);

/// Takes the addresses from the resolved core patterns.
pub fn install(patterns: &std::collections::BTreeMap<String, u32>, base: usize) {
    let set = |name: &str, at: &AtomicUsize| match patterns.get(name) {
        Some(&rva) => at.store(base + rva as usize, Ordering::Release),
        None => crate::log(&format!("hud: {name} not found; HUD widgets are off")),
    };
    set("g_gui_layout", &GUI_LAYOUT);
    set("g_game_state", &GAME_STATE);
    set("g_hud_mode", &HUD_MODE);
    set("g_menu_state", &MENU_STATE);
    set("g_interaction_mode", &INTERACTION);
    if let Some(dir) = crate::data_dir()
        && dir.join(DEBUG_FILE).exists()
    {
        DEBUG.store(true, Ordering::Relaxed);
        crate::log(&format!(
            "hud: {DEBUG_FILE} found; outlining the layout rects"
        ));
    }
}

/// A layout rect in GUI space, by name (see [`LAYOUT`]).
pub fn gui_rect(name: &str) -> Option<Rect> {
    let off = LAYOUT.iter().find(|(n, _)| *n == name)?.1;
    let base = GUI_LAYOUT.load(Ordering::Acquire);
    if base == 0 {
        return None;
    }
    // SAFETY: a static of the exe; every read is checked.
    let v: [f32; 4] = unsafe { crate::game::read(base + off)? };
    rect_from_layout(v)
}

/// A GUI-space rect from the game's `{x, y, w, h}`, if it's sane (inside a
/// generous margin around the GUI space).
pub fn rect_from_layout([x, y, w, h]: [f32; 4]) -> Option<Rect> {
    let pos = |v: f32| (-2000.0..=4000.0).contains(&v);
    let size = |v: f32| (0.0..=4000.0).contains(&v);
    if !(pos(x) && pos(y) && size(w) && size(h)) {
        return None;
    }
    Some(Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h)))
}

/// A layout rect in overlay points, by name (see [`LAYOUT`]). `None` if the
/// name is unknown, the layout wasn't found, or before the first frame.
pub fn rect(name: &str) -> Option<Rect> {
    let t = transform()?;
    gui_rect(name).map(|r| t.rect_to_points(r))
}

// ---------------------------------------------------------------------------
// HUD state

/// The game's HUD-related state (dwords of the exe; `None` if not found).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HudState {
    /// 4 while playing (5, 6: menus; 1: startup logo; 3: excluded by the
    /// HUD's input check).
    pub game_state: Option<i32>,
    /// The HUD panel mode: 0 no HUD (the HUD returns at once), 1 plain HUD,
    /// 2 gamepad pick-from-floor, 3 gamepad inventory cursor, 6 character
    /// sheet, 10 alchemy, 11 the full-screen map, others.
    pub hud_mode: Option<i32>,
    /// A menu or dialog over the HUD (0 none).
    pub menu: Option<i32>,
    /// HUD interaction mode (0 none, 1 target selection, 5 shop, skill
    /// panel and others): a HUD panel or pick over the normal HUD.
    pub interaction: Option<i32>,
}

/// HUD modes in which the HUD (portraits, hand slots, inventory panel) is
/// on screen: the plain HUD, the gamepad floor and inventory cursors and
/// the character sheet. Not the map (11), alchemy (10) or other panels.
pub const HUD_MODES: &[i32] = &[1, 2, 3, 6];

impl HudState {
    /// The HUD is up and takes input: playing, a HUD mode of [`HUD_MODES`],
    /// no menu, dialog, shop, skill panel or target selection.
    pub fn shown(&self) -> bool {
        self.game_state == Some(4)
            && self.hud_mode.is_some_and(|m| HUD_MODES.contains(&m))
            && self.menu == Some(0)
            && self.interaction == Some(0)
    }
}

fn read_dword(at: &AtomicUsize) -> Option<i32> {
    let a = at.load(Ordering::Acquire);
    if a == 0 {
        return None;
    }
    // SAFETY: a dword static of the exe; the read is checked.
    unsafe { crate::game::read::<i32>(a) }
}

/// The game's HUD state now.
pub fn state() -> HudState {
    HudState {
        game_state: read_dword(&GAME_STATE),
        hud_mode: read_dword(&HUD_MODE),
        menu: read_dword(&MENU_STATE),
        interaction: read_dword(&INTERACTION),
    }
}

/// Whether the plain HUD is up (see [`HudState::shown`]); the overlay calls
/// `Mod::hud` only then.
pub fn shown() -> bool {
    state().shown()
}

// ---------------------------------------------------------------------------
// Widgets

/// A small clickable label in the HUD's style: `fill` with a dark border,
/// `text` centred and sized to the rect, lighter while hovered. `rect` is
/// in points (from [`rect`] or [`rect_to_screen`]). Senses clicks (left and
/// right), so the click is the mod's, not the game's.
pub fn button(ui: &mut egui::Ui, rect: Rect, text: &str, fill: egui::Color32) -> egui::Response {
    let r = ui.allocate_rect(rect, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if r.hovered() {
            fill.lerp_to_gamma(egui::Color32::WHITE, 0.25)
        } else {
            fill
        };
        let border = (rect.height() / 14.0).clamp(1.0, 3.0);
        let p = ui.painter();
        p.rect_filled(rect, border, fill);
        p.rect_stroke(
            rect,
            border,
            egui::Stroke::new(border, egui::Color32::from_black_alpha(220)),
            egui::StrokeKind::Inside,
        );
        if !text.is_empty() {
            let size = (rect.height() * 0.62).max(6.0);
            let font = egui::FontId::proportional(size);
            // Shrink long labels to the width.
            let w = p
                .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
                .x;
            let room = rect.width() - 2.0 * border - 2.0;
            let font = if w > room && w > 0.0 {
                egui::FontId::proportional((size * room / w).max(5.0))
            } else {
                font
            };
            p.text(
                rect.center() + Vec2::new(1.0, 1.0),
                egui::Align2::CENTER_CENTER,
                text,
                font.clone(),
                egui::Color32::from_black_alpha(200),
            );
            p.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                font,
                egui::Color32::WHITE,
            );
        }
    }
    r
}

// ---------------------------------------------------------------------------
// Debug outlines

/// With this file in `uk_qol/` at startup, the overlay outlines every
/// layout rect while the HUD is shown (to check positions in a screenshot).
pub const DEBUG_FILE: &str = "hud-debug";
static DEBUG: AtomicBool = AtomicBool::new(false);
static DEBUG_LOGGED: AtomicBool = AtomicBool::new(false);

/// Whether the layout outlines are on.
pub fn debug() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

/// Outlines every layout rect with its name (debug mode).
#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows glue
pub(crate) fn draw_debug(ui: &mut egui::Ui) {
    let Some(t) = transform() else { return };
    let log = !DEBUG_LOGGED.swap(true, Ordering::Relaxed);
    if log {
        crate::log(&format!(
            "hud: window transform scale {} offset ({}, {}) ppp {}",
            t.scale, t.offset.x, t.offset.y, t.pixels_per_point
        ));
    }
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 0, 255));
    painter.rect_stroke(
        t.rect_to_points(Rect::from_min_size(Pos2::ZERO, GUI_SIZE)),
        0.0,
        stroke,
        egui::StrokeKind::Inside,
    );
    for (name, _) in LAYOUT {
        let Some(g) = gui_rect(name) else { continue };
        if log {
            crate::log(&format!(
                "hud: {name} = ({}, {}, {}, {})",
                g.min.x,
                g.min.y,
                g.width(),
                g.height()
            ));
        }
        let r = t.rect_to_points(g);
        painter.rect_stroke(r, 0.0, stroke, egui::StrokeKind::Inside);
        painter.text(
            r.left_top() + Vec2::splat(2.0),
            egui::Align2::LEFT_TOP,
            name.trim_start_matches("face_"),
            egui::FontId::monospace(9.0),
            egui::Color32::from_rgb(255, 120, 255),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Pos2, b: Pos2) -> bool {
        (a - b).length() < 0.01
    }

    #[test]
    fn transform_fits_and_centres() {
        // 21:9: bars left and right (3440 - 2560) / 2 = 440.
        let t = Transform::new(3440.0, 1440.0, 1440.0 / 1080.0);
        assert_eq!(t.scale, 4.0);
        assert_eq!(t.offset, Vec2::new(440.0, 0.0));
        assert!(close(
            t.to_pixels(Pos2::new(640.0, 360.0)),
            Pos2::new(3000.0, 1440.0)
        ));
        assert!((t.points_per_unit() - 3.0).abs() < 1e-5);
        // 16:9: no bars.
        let t = Transform::new(1920.0, 1080.0, 1.0);
        assert_eq!(t.scale, 3.0);
        assert_eq!(t.offset, Vec2::ZERO);
        // 16:10 (Steam Deck): bars top and bottom, scale 2.
        let t = Transform::new(1280.0, 800.0, 1.0);
        assert_eq!(t.scale, 2.0);
        assert_eq!(t.offset, Vec2::new(0.0, 40.0));
        // A fractional scale isn't rounded.
        let t = Transform::new(1366.0, 768.0, 1.0);
        assert!((t.scale - 768.0 / 360.0).abs() < 1e-5);
        assert!((t.offset.x - (1366.0 - 640.0 * t.scale) / 2.0).abs() < 1e-3);
        assert!(t.offset.x > 0.3 && t.offset.y.abs() < 1e-3);
    }

    #[test]
    fn points_round_trip() {
        let t = Transform::new(3440.0, 1440.0, 1440.0 / 1080.0);
        let g = Pos2::new(492.0, 92.0);
        let p = t.to_points(g);
        assert!(close(
            p,
            Pos2::new((440.0 + 492.0 * 4.0) * 0.75, 92.0 * 4.0 * 0.75)
        ));
        assert!(close(t.from_points(p), g));
        let r = t.rect_to_points(Rect::from_min_size(g, Vec2::splat(29.0)));
        assert!((r.width() - 29.0 * 3.0).abs() < 1e-3);
    }

    #[test]
    fn layout_rects() {
        assert_eq!(
            rect_from_layout([10.0, 20.0, 29.0, 29.0]),
            Some(Rect::from_min_size(
                Pos2::new(10.0, 20.0),
                Vec2::splat(29.0)
            ))
        );
        assert_eq!(rect_from_layout([10.0, 20.0, -1.0, 29.0]), None);
        assert_eq!(rect_from_layout([f32::NAN, 0.0, 1.0, 1.0]), None);
        assert_eq!(rect_from_layout([1e9, 0.0, 1.0, 1.0]), None);
        // Each name once, offsets 4-aligned and inside the struct.
        for (i, (n, off)) in LAYOUT.iter().enumerate() {
            assert_eq!(off % 4, 0, "{n}");
            assert!(*off < 0x400, "{n}");
            assert!(LAYOUT[i + 1..].iter().all(|(m, _)| m != n), "{n} twice");
        }
        assert_eq!(skill_slot_name(0), "face_hand_slot_2");
        assert_eq!(weapon_slot_name(3), "face_hand_slot_7");
        // A layout read from a fake GuiLayout.
        let mut fake = [0f32; 0x100];
        fake[0x1b4 / 4..0x1b4 / 4 + 4].copy_from_slice(&[525.0, 92.0, 29.0, 29.0]);
        GUI_LAYOUT.store(fake.as_ptr() as usize, Ordering::Release);
        assert_eq!(
            gui_rect("face_hand_slot_2"),
            Some(Rect::from_min_size(
                Pos2::new(525.0, 92.0),
                Vec2::splat(29.0)
            ))
        );
        assert_eq!(gui_rect("no_such_rect"), None);
        GUI_LAYOUT.store(0, Ordering::Release);
        assert_eq!(gui_rect("face_hand_slot_2"), None);
    }

    #[test]
    fn hud_shown_only_in_plain_play() {
        let play = HudState {
            game_state: Some(4),
            hud_mode: Some(1),
            menu: Some(0),
            interaction: Some(0),
        };
        assert!(play.shown());
        assert!(
            HudState {
                hud_mode: Some(6),
                ..play
            }
            .shown()
        );
        assert!(
            !HudState {
                hud_mode: Some(0),
                ..play
            }
            .shown()
        );
        assert!(
            !HudState {
                hud_mode: Some(11),
                ..play
            }
            .shown()
        );
        assert!(
            !HudState {
                game_state: Some(5),
                ..play
            }
            .shown()
        );
        assert!(
            !HudState {
                menu: Some(2),
                ..play
            }
            .shown()
        );
        assert!(
            !HudState {
                interaction: Some(1),
                ..play
            }
            .shown()
        );
        assert!(!HudState { menu: None, ..play }.shown());
        assert!(!HudState::default().shown());
    }
}
