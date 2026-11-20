//! `enemy-health`: each enemy's health as numbers on the game's own enemy
//! health bars (README, "Mod designs").
//!
//! # What the game draws (1.0.5; same code in 1.0.6)
//! The dungeon HUD calls the enemy-bar function (pattern `enemy_bars`,
//! 1.0.5 0x1401ffc60, 1.0.6 0x1401ffd50) every frame. It finds the (up to
//! two) enemies the party faces and, for each one whose `invisible_hp_bar`
//! flag (`Monster+0x128`) is clear, draws a health bar near the top of the
//! dungeon view (size by `hpbar_size`, `Monster+0x1f1`) filled
//! `Monster+0xd0` (health) / `Monster+0x120` (maximum), with the enemy's
//! name centred in it. Right after, it calls the status-icon row (pattern
//! `enemy_status_row`, 1.0.5 0x1401ff4a0, 1.0.6 0x1401ff590; its only
//! caller) with the monster and the rect just below the bar:
//! `{x, y + h, w, h}` in GUI space, where `{x, y, w, h}` is the bar.
//!
//! # What the mod does
//! Detours both: the bar function collects one entry per bar drawn this
//! frame (health, maximum, the bar's rect, recovered from the status row's
//! rect) and publishes the list when it returns; the overlay paints the
//! numbers over that frame. Only enemies the game itself shows a bar for
//! get numbers. Nothing in the game changes.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use uk_qol_runtime::egui::{self, Align2, Color32, FontId, Pos2, Rect, Vec2};
use uk_qol_runtime::game;
use uk_qol_runtime::guard;
use uk_qol_runtime::host::{LoadCtx, Mod};
use uk_qol_runtime::hotkey;
use uk_qol_runtime::hud;
use uk_qol_runtime::input::InputEvent;
use uk_qol_runtime::toast;

const ID: &str = "enemy-health";
const KEYS: [&str; 2] = ["toggle_key", "toggle_pad"];

// Monster struct fields (1.0.5 and 1.0.6).
const MONSTER_HP: usize = 0xd0; // i32
const MONSTER_MAX_HP: usize = 0x120; // i32

/// A published list older than this isn't drawn (the game stopped calling
/// its enemy-bar function: a menu, a cut scene, the map).
const STALE: Duration = Duration::from_millis(250);

const TEXT_COLOR: Color32 = Color32::from_rgb(255, 230, 230);

/// One enemy bar the game drew: health, maximum and the bar's rect (GUI
/// space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    pub hp: i32,
    pub max: i32,
    pub rect: Rect,
}

/// Bars being collected during the game's enemy-bar call.
static PENDING: Mutex<Vec<Bar>> = Mutex::new(Vec::new());
/// The last complete frame's bars and when they were published.
static SHOWN: Mutex<Option<(Instant, Vec<Bar>)>> = Mutex::new(None);
/// Inside the game's enemy-bar function (so a status-row call belongs to it).
static IN_BARS: AtomicBool = AtomicBool::new(false);

type BarsFn = unsafe extern "system" fn(usize);
type RowFn = unsafe extern "system" fn(usize, usize);
static ORIG_BARS: AtomicUsize = AtomicUsize::new(0);
static ORIG_ROW: AtomicUsize = AtomicUsize::new(0);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The bar's rect from the status row's rect `{x, y + h, w, h}`, if sane.
pub fn bar_rect([x, y, w, h]: [f32; 4]) -> Option<Rect> {
    hud::rect_from_layout([x, y - h, w, h]).filter(|r| r.width() > 0.0 && r.height() > 0.0)
}

/// The enemy-bar function: collects this frame's bars, then publishes them.
unsafe extern "system" fn enemy_bars(this: usize) {
    lock(&PENDING).clear();
    IN_BARS.store(true, Ordering::Relaxed);
    // SAFETY: set before the hook was enabled; the game's argument.
    unsafe {
        let orig: BarsFn = std::mem::transmute(ORIG_BARS.load(Ordering::Relaxed));
        orig(this);
    }
    IN_BARS.store(false, Ordering::Relaxed);
    let bars = std::mem::take(&mut *lock(&PENDING));
    *lock(&SHOWN) = Some((Instant::now(), bars));
}

/// The status-icon row under an enemy's bar: records the bar.
unsafe extern "system" fn status_row(monster: usize, rect: usize) {
    if IN_BARS.load(Ordering::Relaxed) {
        guard::call(ID, || {
            // SAFETY: the game's arguments, read on its thread; every read
            // is checked.
            let bar = unsafe {
                let r = game::read::<[f32; 4]>(rect);
                let hp = game::read::<i32>(monster + MONSTER_HP);
                let max = game::read::<i32>(monster + MONSTER_MAX_HP);
                match (r.and_then(bar_rect), hp, max) {
                    (Some(rect), Some(hp), Some(max)) if max > 0 => Some(Bar { hp, max, rect }),
                    _ => None,
                }
            };
            if let Some(b) = bar {
                let mut p = lock(&PENDING);
                if p.len() < 8 {
                    p.push(b);
                }
            }
        });
    }
    // SAFETY: set before the hook was enabled; the game's arguments.
    unsafe {
        let orig: RowFn = std::mem::transmute(ORIG_ROW.load(Ordering::Relaxed));
        orig(monster, rect);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `23/40`
    CurrentMax,
    /// `23`
    Current,
    /// `58%`
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
        let cur = cur.max(0);
        match self {
            Format::CurrentMax => format!("{cur}/{max}"),
            Format::Current => cur.to_string(),
            Format::Percent => {
                let p = (cur as f64 * 100.0 / max.max(1) as f64).round() as i64;
                format!("{p}%")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// Centred just above the bar (the name is centred in the bar).
    Above,
    /// Right-aligned inside the bar.
    OnBar,
}

impl Position {
    fn parse(s: &str) -> Position {
        match s {
            "on_bar" => Position::OnBar,
            _ => Position::Above,
        }
    }

    /// Where the text goes for a bar `r` (GUI space) and text height `size`,
    /// and how it's anchored there.
    pub fn place(self, r: Rect, size: f32) -> (Pos2, Align2) {
        match self {
            Position::Above => (
                Pos2::new(r.center().x, r.min.y - 1.0),
                Align2::CENTER_BOTTOM,
            ),
            Position::OnBar => (
                Pos2::new(r.max.x - (size * 0.5).max(3.0), r.center().y),
                Align2::RIGHT_CENTER,
            ),
        }
    }
}

/// `text` at `at` (anchored by `align`) with a dark outline.
fn outlined(p: &egui::Painter, at: Pos2, align: Align2, text: &str, font: FontId, color: Color32) {
    let o = (font.size / 9.0).clamp(1.0, 2.5);
    let shade = Color32::from_black_alpha(230);
    for (dx, dy) in [
        (-o, 0.0),
        (o, 0.0),
        (0.0, -o),
        (0.0, o),
        (-o, -o),
        (o, o),
        (-o, o),
        (o, -o),
    ] {
        p.text(at + Vec2::new(dx, dy), align, text, font.clone(), shade);
    }
    p.text(at, align, text, font, color);
}

pub struct EnemyHealth {
    shown: bool,
    format: Format,
    position: Position,
    /// Text height in GUI units.
    size: f32,
}

impl Default for EnemyHealth {
    fn default() -> Self {
        EnemyHealth {
            shown: true,
            format: Format::CurrentMax,
            position: Position::Above,
            size: 6.0,
        }
    }
}

impl EnemyHealth {
    fn apply(&mut self, settings: &toml::Table) {
        let s = |k: &str, d: &'static str| {
            settings
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or(d)
                .to_owned()
        };
        self.shown = settings
            .get("shown")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        self.format = Format::parse(&s("format", "current_max"));
        self.position = Position::parse(&s("position", "above"));
        self.size = settings
            .get("text_size")
            .and_then(|v| v.as_float())
            .unwrap_or(6.0)
            .clamp(4.0, 10.0) as f32;
    }
}

impl Mod for EnemyHealth {
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
        let at = |name: &str| {
            cx.patterns
                .get(name)
                .map(|&rva| cx.base + rva as usize)
                .ok_or_else(|| format!("pattern {name} not resolved"))
        };
        let (bars, row) = (at("enemy_bars")?, at("enemy_status_row")?);
        self.apply(cx.settings);
        let e = |e: uk_qol_runtime::hook::HookError| e.to_string();
        // SAFETY: each target is the start of the named function (verified
        // patterns); each detour has the same signature and ABI. The
        // trampolines are stored before the host enables the hooks.
        unsafe {
            ORIG_BARS.store(
                cx.hooks
                    .detour(bars, enemy_bars as BarsFn as usize)
                    .map_err(e)?,
                Ordering::Relaxed,
            );
            ORIG_ROW.store(
                cx.hooks
                    .detour(row, status_row as RowFn as usize)
                    .map_err(e)?,
                Ordering::Relaxed,
            );
        }
        Ok(())
    }

    fn on_unload(&mut self) {
        *lock(&SHOWN) = None;
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
            "Enemy health on"
        } else {
            "Enemy health off"
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
        let bars = match &*lock(&SHOWN) {
            Some((when, bars)) if when.elapsed() < STALE => bars.clone(),
            _ => return,
        };
        let font = FontId::proportional((self.size * t.points_per_unit()).max(6.0));
        let p = ui.painter();
        for b in bars {
            let (at, align) = self.position.place(b.rect, self.size);
            outlined(
                p,
                t.to_points(at),
                align,
                &self.format.text(b.hp, b.max),
                font.clone(),
                TEXT_COLOR,
            );
        }
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(EnemyHealth::default())
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
        assert!(!manifest.enabled, "off by default: a balance choice");
        assert_eq!(PatternFile::parse(m.patterns()).unwrap().patterns.len(), 2);
        let mut eh = EnemyHealth::default();
        eh.apply(&manifest.defaults());
        assert!(eh.shown);
        assert_eq!(eh.format, Format::CurrentMax);
        assert_eq!(eh.position, Position::Above);
        assert_eq!(eh.size, 6.0);
        assert!(m.hud_display_only());
    }

    #[test]
    fn formats() {
        assert_eq!(Format::parse("current_max").text(23, 40), "23/40");
        assert_eq!(Format::parse("current").text(23, 40), "23");
        assert_eq!(Format::parse("percent").text(23, 40), "58%");
        assert_eq!(Format::parse("current_max").text(-3, 40), "0/40");
        assert_eq!(Format::parse("junk"), Format::CurrentMax);
    }

    #[test]
    fn bar_from_status_row_rect() {
        let r = bar_rect([100.0, 60.0, 80.0, 10.0]).unwrap();
        assert_eq!(
            r,
            Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(80.0, 10.0))
        );
        assert_eq!(bar_rect([100.0, 60.0, 0.0, 10.0]), None);
        assert_eq!(bar_rect([f32::NAN, 60.0, 80.0, 10.0]), None);
    }

    #[test]
    fn placement() {
        let r = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(80.0, 10.0));
        let (at, a) = Position::Above.place(r, 6.0);
        assert_eq!((at, a), (Pos2::new(140.0, 49.0), Align2::CENTER_BOTTOM));
        let (at, a) = Position::OnBar.place(r, 6.0);
        assert_eq!((at, a), (Pos2::new(177.0, 55.0), Align2::RIGHT_CENTER));
        assert_eq!(Position::parse("x"), Position::Above);
    }
}
