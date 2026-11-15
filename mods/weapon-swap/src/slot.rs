//! The second weapon slot on the character sheet: where it goes and how it
//! looks.
//!
//! Positions are in the game's GUI space (one unit is one pixel of the
//! game's art), from the sheet's own equipment slot rects, so the slot
//! follows the sheet at every window size. It is drawn in the style of the
//! game's empty slots: a dark square with a shadowed top-left edge and a
//! lighter bottom-right one. Its icon is the game's own empty-weapon-slot
//! sword (read from the player's install at run time; no game art in the
//! repo) with a Roman numeral "II" drawn by the mod in the same palette.

use uk_qol_runtime::egui::{self, Color32, Pos2, Rect, Vec2};
use uk_qol_runtime::hud;

/// Sheet slot indices (`EquipSlot`): the helmet and the weapon.
pub const HELMET: usize = 0;
pub const WEAPON: usize = 4;

/// Colours of the game's empty equipment slots (measured in game 1.0.5).
pub const FILL: Color32 = Color32::from_rgb(28, 28, 33);
const SHADOW: Color32 = Color32::from_rgb(0, 0, 0);
const BEVEL: Color32 = Color32::from_rgb(63, 63, 65);
const SILHOUETTE_EDGE: Color32 = Color32::from_rgb(15, 15, 27);
/// Outline while an item that fits is on the cursor (like the game's).
pub const FITS: Color32 = Color32::from_rgb(70, 200, 70);
pub const NO_FIT: Color32 = Color32::from_rgb(190, 60, 50);
pub const HOVER: Color32 = Color32::from_rgb(110, 110, 120);
/// The fill behind a stored weapon's label.
pub const STORED: Color32 = Color32::from_rgb(60, 95, 50);

/// Where the second weapon slot goes: the size of the helmet slot, on its row,
/// in the leftmost column of the sheet's slots (left of the helmet), or
/// one slot pitch left of the helmet if that column isn't free. If neither
/// fits, beside the weapon slot. `slots` are the sheet's equipment slot
/// rects in GUI space, by `EquipSlot` index.
pub fn place(slots: &[Option<Rect>]) -> Option<Rect> {
    let all: Vec<Rect> = slots.iter().flatten().copied().collect();
    let free = |c: &Rect| {
        let space = Rect::from_min_size(Pos2::ZERO, hud::GUI_SIZE);
        space.contains_rect(*c) && all.iter().all(|o| !o.intersects(c.shrink(0.5)))
    };
    if let Some(helmet) = slots
        .get(HELMET)
        .copied()
        .flatten()
        .filter(|r| r.width() >= 4.0)
    {
        let left_x = all.iter().map(|r| r.min.x).fold(f32::INFINITY, f32::min);
        let column = Rect::from_min_size(Pos2::new(left_x, helmet.min.y), helmet.size());
        if left_x < helmet.min.x && free(&column) {
            return Some(column);
        }
        let pitch = helmet.width() + (helmet.width() * 0.2).round();
        let beside = helmet.translate(Vec2::new(-pitch, 0.0));
        if free(&beside) {
            return Some(beside);
        }
    }
    let weapon = slots.get(WEAPON).copied().flatten()?;
    place_beside(weapon, &all)
}

/// Gap between the weapon slot and the second slot, in GUI units.
const GAP: f32 = 2.0;

/// The fallback: the size of the weapon slot, beside it (below, left,
/// above, right: the first spot that overlaps none of `others` and stays
/// inside the GUI space).
pub fn place_beside(weapon: Rect, others: &[Rect]) -> Option<Rect> {
    let s = weapon.size();
    let d = [
        Vec2::new(0.0, s.y + GAP),
        Vec2::new(-(s.x + GAP), 0.0),
        Vec2::new(0.0, -(s.y + GAP)),
        Vec2::new(s.x + GAP, 0.0),
    ];
    let space = Rect::from_min_size(Pos2::ZERO, hud::GUI_SIZE);
    d.iter()
        .map(|&d| weapon.translate(d))
        .find(|c| space.contains_rect(*c) && others.iter().all(|o| !o.intersects(c.shrink(0.5))))
}

/// The slot's frame like the game's empty slots: `gui` in GUI space,
/// drawn through `t`; `outline` replaces the edges (hover, fits).
pub fn paint_frame(p: &egui::Painter, t: &hud::Transform, gui: Rect, outline: Option<Color32>) {
    let px = |r: Rect| t.rect_to_points(r);
    let corner = t.points_per_unit() * 2.0;
    p.rect_filled(px(gui), corner, FILL);
    let (x0, y0, x1, y1) = (gui.min.x, gui.min.y, gui.max.x, gui.max.y);
    let line = |r: Rect, c: Color32| p.rect_filled(px(r), 0.0, c);
    match outline {
        Some(c) => {
            let s = egui::Stroke::new(t.points_per_unit(), c);
            p.rect_stroke(px(gui), corner, s, egui::StrokeKind::Inside);
        }
        None => {
            // Shadow on the top and left inner edge, bevel bottom and right.
            line(
                Rect::from_min_max(Pos2::new(x0 + 2.0, y0 + 1.0), Pos2::new(x1 - 2.0, y0 + 2.0)),
                SHADOW,
            );
            line(
                Rect::from_min_max(Pos2::new(x0 + 1.0, y0 + 2.0), Pos2::new(x0 + 2.0, y1 - 2.0)),
                SHADOW,
            );
            line(
                Rect::from_min_max(Pos2::new(x0 + 2.0, y1 - 1.0), Pos2::new(x1 - 2.0, y1)),
                BEVEL,
            );
            line(
                Rect::from_min_max(Pos2::new(x1 - 1.0, y0 + 2.0), Pos2::new(x1, y1 - 2.0)),
                BEVEL,
            );
        }
    }
}

/// The game's sprite for the character sheet's empty weapon slot.
pub const SWORD_SPRITE: &str = "hud_barbie_sword";

/// The sheet's slot backgrounds: one sprite with all 11 stone slots and the
/// cords between them (115x202 in game 1.0.5).
pub const SLOTS_SPRITE: &str = "hud_barbie_equip_slots";
const SLOTS_SPRITE_SIZE: Vec2 = Vec2::new(115.0, 202.0);
/// The helmet's stone in [`SLOTS_SPRITE`], without the cords below it:
/// pixels x 39..77, y 0..37 (the stone itself starts at (39, 3), with a
/// crack rising above it).
const HELMET_CELL: Rect = Rect::from_min_max(Pos2::new(39.0, 0.0), Pos2::new(77.0, 37.0));
/// The stone's top-left inside [`HELMET_CELL`]; in game it sits on the
/// slot rect's top-left.
const HELMET_STONE: Vec2 = Vec2::new(0.0, 3.0);

/// The game's own slot highlights: green frame while the carried item fits,
/// red veil while it doesn't, yellow frame on hover.
pub const ALLOW_SPRITE: &str = "hud_barbie_slot_allow";
pub const DENY_SPRITE: &str = "hud_barbie_slot_deny";
pub const SELECTED_SPRITE: &str = "hud_barbie_slot_selected";

/// The slot's background: the game's stone slot, cut out of the sheet's
/// slot sprite (the helmet's cell). `false` if the sprite can't be read or
/// isn't the layout this was measured on; draw [`paint_frame`] then.
pub fn paint_stone(ctx: &egui::Context, p: &egui::Painter, t: &hud::Transform, gui: Rect) -> bool {
    let Some((tex, size)) = uk_qol_runtime::sprite::texture(ctx, SLOTS_SPRITE) else {
        return false;
    };
    if size != SLOTS_SPRITE_SIZE {
        return false;
    }
    let uv = Rect::from_min_max(
        (HELMET_CELL.min.to_vec2() / size).to_pos2(),
        (HELMET_CELL.max.to_vec2() / size).to_pos2(),
    );
    let at = gui.min - HELMET_STONE;
    p.image(
        tex.id(),
        t.rect_to_points(Rect::from_min_size(at, HELMET_CELL.size())),
        uv,
        Color32::WHITE,
    );
    true
}

/// One of the game's highlight sprites (`name`), centred on the slot on
/// whole GUI units. `false` if it can't be read.
pub fn paint_highlight(
    ctx: &egui::Context,
    p: &egui::Painter,
    t: &hud::Transform,
    gui: Rect,
    name: &str,
) -> bool {
    let Some((tex, size)) = uk_qol_runtime::sprite::texture(ctx, name) else {
        return false;
    };
    let at = (gui.center() - size / 2.0).floor();
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    p.image(
        tex.id(),
        t.rect_to_points(Rect::from_min_size(at, size)),
        uv,
        Color32::WHITE,
    );
    true
}

/// The weapon icon of the second slot: the game's own empty-weapon-slot
/// sprite (read from the player's install, see `runtime::sprite`) centred
/// in `gui` on whole GUI units, as the sheet draws it. `false` if the
/// sprite can't be read (only the "II" mark is drawn then).
pub fn paint_sword(ctx: &egui::Context, p: &egui::Painter, t: &hud::Transform, gui: Rect) -> bool {
    let Some((tex, size)) = uk_qol_runtime::sprite::texture(ctx, SWORD_SPRITE) else {
        return false;
    };
    let at = gui.min + ((gui.size() - size) / 2.0).floor();
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    p.image(
        tex.id(),
        t.rect_to_points(Rect::from_min_size(at, size)),
        uv,
        Color32::WHITE,
    );
    true
}

/// The "II" mark: a Roman numeral two, 5x5 pixels (serifs top and bottom,
/// two stems), outlined in the silhouettes' dark edge colour.
const NUMERAL: [&str; 5] = ["#####", ".#.#.", ".#.#.", ".#.#.", "#####"];
/// Lighter than the silhouette, so the mark reads on the empty and the
/// filled slot.
const NUMERAL_FILL: Color32 = Color32::from_rgb(92, 92, 110);

/// The mark's pixels `(x, y, edge)` with its 1-pixel outline, in a 7x7 box.
pub fn numeral_pixels() -> Vec<(i32, i32, bool)> {
    let on = |x: i32, y: i32| {
        (0..5).contains(&x)
            && (0..5).contains(&y)
            && NUMERAL[y as usize].as_bytes()[x as usize] == b'#'
    };
    let mut out = Vec::new();
    for y in -1..=5 {
        for x in -1..=5 {
            if on(x, y) {
                out.push((x + 1, y + 1, false));
            } else if (-1..=1).any(|dy| (-1..=1).any(|dx| on(x + dx, y + dy))) {
                out.push((x + 1, y + 1, true));
            }
        }
    }
    out
}

/// The "II" mark in the slot's top-right corner, inside the bevel (where
/// the diagonal sword leaves room).
pub fn paint_numeral(p: &egui::Painter, t: &hud::Transform, gui: Rect) {
    let o = Pos2::new(gui.max.x - 2.0 - 7.0, gui.min.y + 2.0);
    for (x, y, edge) in numeral_pixels() {
        let r = Rect::from_min_size(o + Vec2::new(x as f32, y as f32), Vec2::splat(1.0));
        // Slightly larger than a pixel, so neighbours don't show seams.
        let r = t.rect_to_points(r).expand(0.3);
        p.rect_filled(r, 0.0, if edge { SILHOUETTE_EDGE } else { NUMERAL_FILL });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f32, y: f32) -> Rect {
        Rect::from_min_size(Pos2::new(x, y), Vec2::splat(33.0))
    }

    /// The sheet's slots as seen in game 1.0.5 (3 columns 39 apart, rows 40
    /// apart; helmet top centre).
    fn sheet() -> Vec<Option<Rect>> {
        let (a, b, c) = (350.0, 389.0, 428.0);
        vec![
            Some(r(b, 45.0)),  // helmet
            Some(r(b, 85.0)),  // amulet
            Some(r(c, 85.0)),  // cloak
            Some(r(b, 125.0)), // armor
            Some(r(a, 125.0)), // weapon
            Some(r(c, 125.0)), // shield
            Some(r(a, 85.0)),  // gloves
            Some(r(b, 205.0)), // boots
            Some(r(b, 165.0)), // belt
            Some(r(a, 165.0)), // ring 1
            Some(r(c, 165.0)), // ring 2
        ]
    }

    #[test]
    fn left_of_the_helmet() {
        let s = sheet();
        assert_eq!(place(&s), Some(r(350.0, 45.0)));
        // The helmet in the left column: one pitch (slot + 20 %) left of it.
        let mut s1 = s.clone();
        s1[HELMET] = Some(r(350.0, 45.0));
        assert_eq!(place(&s1), Some(r(350.0 - 40.0, 45.0)));
        // Left of the helmet taken: beside the weapon (left of it here).
        let mut s2 = s.clone();
        s2.push(Some(r(350.0, 45.0)));
        assert_eq!(place(&s2), Some(r(315.0, 125.0)));
        // No helmet slot: beside the weapon.
        let mut s3 = s.clone();
        s3[HELMET] = None;
        let p = place(&s3).unwrap();
        assert!(s3.iter().flatten().all(|o| !o.intersects(p.shrink(0.5))));
    }

    #[test]
    fn beside_the_weapon() {
        let q = |x, y| Rect::from_min_size(Pos2::new(x, y), Vec2::splat(20.0));
        let weapon = q(300.0, 100.0);
        assert_eq!(place_beside(weapon, &[weapon]), Some(q(300.0, 122.0)));
        let below = q(300.0, 125.0);
        assert_eq!(
            place_beside(weapon, &[weapon, below]),
            Some(q(278.0, 100.0))
        );
        let all = [
            weapon,
            below,
            q(275.0, 100.0),
            q(300.0, 75.0),
            q(325.0, 100.0),
        ];
        assert_eq!(place_beside(weapon, &all), None);
        assert_eq!(place_beside(q(0.0, 345.0), &[]), Some(q(0.0, 323.0)));
    }

    #[test]
    fn numeral() {
        let px = numeral_pixels();
        let inside = |&(x, y, _): &(i32, i32, bool)| (0..7).contains(&x) && (0..7).contains(&y);
        assert!(px.iter().all(inside));
        let fill: Vec<(i32, i32)> = px.iter().filter(|p| !p.2).map(|p| (p.0, p.1)).collect();
        // 5 + 5 serif pixels, 2 stems of 3.
        assert_eq!(fill.len(), 16);
        assert!(fill.contains(&(2, 3)) && fill.contains(&(4, 3)) && !fill.contains(&(3, 3)));
        // Every fill pixel's neighbours are fill or outline.
        for &(x, y) in &fill {
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                assert!(px.iter().any(|p| (p.0, p.1) == (x + dx, y + dy)));
            }
        }
    }
}
