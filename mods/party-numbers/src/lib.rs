//! `party-numbers`: each hero's health and second bar (stamina, mana or
//! Zeal) as numbers on the portrait bars (README, "Mod designs").
//!
//! # What the game draws (1.0.5 0x14021e6b0, same in 1.0.6)
//! Under each portrait (`face_rect_N`) the game draws two bars, starting at
//! the portrait's corner plus `face_hp_mana_offset` ([`hud::bar_offset`]),
//! the second one [`hud::SECOND_BAR_DY`] lower:
//! - health: `Hero+0xfc` of `+0xec`;
//! - the second bar, by class (`Hero+0x94`): Paladin (1) Zeal pips,
//!   `+0x108` of `+0xf8`; Warrior, Thief and Ranger (0, 2, 3) stamina,
//!   `+0x100` of `+0xf0`; Voidcaller, Wizard and Blood Mage (4, 5, 6) mana,
//!   `+0x104` of `+0xf4`. A maximum of 0 leaves the bar empty (Blood Mages
//!   usually have no mana).
//!
//! The mod paints the numbers centred on those bars with the overlay. It
//! only reads the hero structs (through the runtime's party list) and
//! changes nothing in the game. Its drawing is display only, so it also
//! shows while the player picks a target (healing an ally, say).

use std::sync::OnceLock;

use uk_qol_runtime::egui::{self, Align2, Color32, FontId, Pos2, Vec2};
use uk_qol_runtime::game;
use uk_qol_runtime::host::{LoadCtx, Mod};
use uk_qol_runtime::hotkey;
use uk_qol_runtime::hud;
use uk_qol_runtime::input::InputEvent;
use uk_qol_runtime::savehook;
use uk_qol_runtime::sprite;
use uk_qol_runtime::toast;

const ID: &str = "party-numbers";
const KEYS: [&str; 2] = ["toggle_key", "toggle_pad"];

// Hero struct fields (i32), 1.0.5 and 1.0.6.
const HERO_MAX_HP: usize = 0xec;
const HERO_MAX_STAMINA: usize = 0xf0;
const HERO_MAX_MANA: usize = 0xf4;
const HERO_MAX_ZEAL: usize = 0xf8;
const HERO_HP: usize = 0xfc;
const HERO_STAMINA: usize = 0x100;
const HERO_MANA: usize = 0x104;
const HERO_ZEAL: usize = 0x108;

const CLASS_PALADIN: i32 = 1;

/// The game's HP bar sprite, for the bars' size and pivot.
const BAR_SPRITE: &str = "hud_hero_hpbar";
/// Bar size (GUI units) if the sprite can't be read: what the 1.0.5 HUD
/// draws.
const BAR_FALLBACK: Vec2 = Vec2::new(60.0, 5.0);

const HP_COLOR: Color32 = Color32::from_rgb(255, 214, 214);
const STAMINA_COLOR: Color32 = Color32::from_rgb(255, 244, 196);
const MANA_COLOR: Color32 = Color32::from_rgb(208, 228, 255);
const ZEAL_COLOR: Color32 = Color32::from_rgb(255, 236, 170);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `34/50`
    CurrentMax,
    /// `34`
    Current,
    /// `68%`
    Percent,
}

impl Format {
    fn parse(s: &str) -> Format {
        match s {
            "current" => Format::Current,
            "percent" => Format::Percent,
            _ => Format::CurrentMax,
        }
    }

    /// The text for `cur` of `max` (`max` > 0).
    pub fn text(self, cur: i32, max: i32) -> String {
        match self {
            Format::CurrentMax => format!("{cur}/{max}"),
            Format::Current => cur.to_string(),
            Format::Percent => {
                let p = (cur.max(0) as f64 * 100.0 / max.max(1) as f64).round() as i64;
                format!("{p}%")
            }
        }
    }
}

/// Which second bar a class has: (current, max) field offsets and text color.
pub fn second_bar(class: i32) -> (usize, usize, Color32) {
    match class {
        CLASS_PALADIN => (HERO_ZEAL, HERO_MAX_ZEAL, ZEAL_COLOR),
        0 | 2 | 3 => (HERO_STAMINA, HERO_MAX_STAMINA, STAMINA_COLOR),
        _ => (HERO_MANA, HERO_MAX_MANA, MANA_COLOR),
    }
}

/// Reads (current, max) at two hero fields; `None` if unreadable or the
/// maximum isn't positive (the game draws no fill then).
fn pair(hero: usize, cur: usize, max: usize) -> Option<(i32, i32)> {
    // SAFETY: fields of a party hero struct; reads are checked.
    let (c, m) = unsafe {
        (
            game::read::<i32>(hero + cur)?,
            game::read::<i32>(hero + max)?,
        )
    };
    (m > 0).then_some((c, m))
}

/// The bars' size and pivot in GUI units, from the game's sprite.
fn bar_geometry() -> (Vec2, Vec2) {
    static GEO: OnceLock<(Vec2, Vec2)> = OnceLock::new();
    *GEO.get_or_init(|| match sprite::get(BAR_SPRITE) {
        Some(s) => (s.size(), Vec2::new(s.pivot[0], s.pivot[1])),
        None => (BAR_FALLBACK, Vec2::ZERO),
    })
}

/// Centre of a portrait's bar `n` (0 health, 1 the second bar) in GUI
/// space, for the portrait's top-left corner `face`.
pub fn bar_centre(face: Pos2, offset: Vec2, size: Vec2, pivot: Vec2, n: usize) -> Pos2 {
    face + offset - pivot + size / 2.0 + Vec2::new(0.0, hud::SECOND_BAR_DY * n as f32)
}

/// `text` centred at `at` with a one-point dark outline.
fn outlined(p: &egui::Painter, at: Pos2, text: &str, font: FontId, color: Color32) {
    let o = (font.size / 9.0).clamp(1.0, 2.5);
    let shade = Color32::from_black_alpha(230);
    for d in [
        Vec2::new(-o, 0.0),
        Vec2::new(o, 0.0),
        Vec2::new(0.0, -o),
        Vec2::new(0.0, o),
        Vec2::new(-o, -o),
        Vec2::new(o, o),
        Vec2::new(-o, o),
        Vec2::new(o, -o),
    ] {
        p.text(at + d, Align2::CENTER_CENTER, text, font.clone(), shade);
    }
    p.text(at, Align2::CENTER_CENTER, text, font, color);
}

pub struct PartyNumbers {
    shown: bool,
    format: Format,
    second: bool,
    /// Text height in GUI units.
    size: f32,
}

impl Default for PartyNumbers {
    fn default() -> Self {
        PartyNumbers {
            shown: true,
            format: Format::CurrentMax,
            second: true,
            size: 6.0,
        }
    }
}

impl PartyNumbers {
    fn apply(&mut self, settings: &toml::Table) {
        let b = |k: &str| settings.get(k).and_then(|v| v.as_bool()).unwrap_or(true);
        self.shown = b("shown");
        self.second = b("second_bar");
        self.format = Format::parse(
            settings
                .get("format")
                .and_then(|v| v.as_str())
                .unwrap_or("current_max"),
        );
        self.size = settings
            .get("text_size")
            .and_then(|v| v.as_float())
            .unwrap_or(6.0)
            .clamp(4.0, 10.0) as f32;
    }
}

impl Mod for PartyNumbers {
    fn id(&self) -> &'static str {
        ID
    }

    fn manifest(&self) -> &'static str {
        include_str!("../mod.toml")
    }

    fn patterns(&self) -> &'static str {
        include_str!("../patterns.toml")
    }

    fn on_load(&mut self, cx: &mut LoadCtx) -> Result<(), String> {
        self.apply(cx.settings);
        Ok(())
    }

    fn on_settings(&mut self, settings: &toml::Table) {
        self.apply(settings);
    }

    fn on_input(&mut self, ev: &InputEvent) -> bool {
        if !KEYS.iter().any(|k| hotkey::pressed(ID, k, ev)) {
            return false;
        }
        self.shown = !self.shown;
        uk_qol_runtime::bridge::request_setting(ID, "shown", toml::Value::Boolean(self.shown));
        toast::show(if self.shown {
            "Party numbers on"
        } else {
            "Party numbers off"
        });
        true
    }

    fn has_hud(&self) -> bool {
        self.shown
    }

    fn hud_display_only(&self) -> bool {
        true
    }

    fn hud(&mut self, ui: &mut egui::Ui) {
        let Some(t) = hud::transform() else { return };
        let Some(offset) = hud::bar_offset() else {
            return;
        };
        let (size, pivot) = bar_geometry();
        let ppu = t.points_per_unit();
        let font = FontId::proportional((self.size * ppu).max(6.0));
        let p = ui.painter();
        for h in savehook::party_heroes() {
            let Some(face) = hud::gui_rect(&hud::face_rect_name(h.index)) else {
                continue;
            };
            // A dead hero gets the game's tombstone bar: no numbers.
            let Some((hp, max_hp)) = pair(h.ptr, HERO_HP, HERO_MAX_HP) else {
                continue;
            };
            if hp <= 0 {
                continue;
            }
            let at = |n| t.to_points(bar_centre(face.min, offset, size, pivot, n));
            outlined(
                p,
                at(0),
                &self.format.text(hp, max_hp),
                font.clone(),
                HP_COLOR,
            );
            if !self.second {
                continue;
            }
            let (cur, max, color) = second_bar(h.class);
            if let Some((c, m)) = pair(h.ptr, cur, max) {
                outlined(p, at(1), &self.format.text(c, m), font.clone(), color);
            }
        }
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(PartyNumbers::default())
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
        assert!(
            PatternFile::parse(m.patterns())
                .unwrap()
                .patterns
                .is_empty()
        );
        let mut pn = PartyNumbers::default();
        pn.apply(&manifest.defaults());
        assert!(pn.shown && pn.second);
        assert_eq!(pn.format, Format::CurrentMax);
        assert_eq!(pn.size, 6.0);
        assert!(m.hud_display_only());
    }

    #[test]
    fn formats() {
        assert_eq!(Format::parse("current_max").text(34, 50), "34/50");
        assert_eq!(Format::parse("current").text(34, 50), "34");
        assert_eq!(Format::parse("percent").text(34, 50), "68%");
        assert_eq!(Format::parse("percent").text(1, 3), "33%");
        assert_eq!(Format::parse("percent").text(-2, 3), "0%");
        assert_eq!(Format::parse("junk"), Format::CurrentMax);
    }

    #[test]
    fn second_bar_by_class() {
        assert_eq!(second_bar(1).0, HERO_ZEAL);
        for c in [0, 2, 3] {
            assert_eq!(second_bar(c).0, HERO_STAMINA);
        }
        for c in [4, 5, 6] {
            assert_eq!(second_bar(c).0, HERO_MANA);
        }
        // Each current field is 0x10 past its maximum, as the game reads them.
        for c in 0..7 {
            let (cur, max, _) = second_bar(c);
            assert_eq!(cur - max, 0x10);
        }
        assert_eq!(HERO_HP - HERO_MAX_HP, 0x10);
    }

    #[test]
    fn bars_are_six_apart() {
        let face = Pos2::new(494.0, 5.0);
        let off = Vec2::new(2.0, 70.0);
        let size = Vec2::new(60.0, 3.0);
        let a = bar_centre(face, off, size, Vec2::ZERO, 0);
        assert_eq!(a, Pos2::new(526.0, 76.5));
        let b = bar_centre(face, off, size, Vec2::ZERO, 1);
        assert_eq!(b - a, Vec2::new(0.0, 6.0));
    }

    /// With `UNDERKEEP_DIR` set: the bar sprite is in the install.
    #[test]
    fn bar_sprite_in_install() {
        let Ok(dir) = std::env::var("UNDERKEEP_DIR") else {
            return;
        };
        let s = sprite::load(&std::path::Path::new(&dir).join("base"), BAR_SPRITE).unwrap();
        eprintln!("{BAR_SPRITE}: {:?} pivot {:?}", s.size(), s.pivot);
        assert!(s.size().x > 20.0 && s.size().y < 10.0);
    }
}
