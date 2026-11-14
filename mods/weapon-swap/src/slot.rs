//! The ranged slot on the character sheet: where it goes and how it looks.
//!
//! Positions are in the game's GUI space (one unit is one pixel of the
//! game's art), from the sheet's own equipment slot rects, so the slot
//! follows the sheet at every window size. It is drawn in the style of the
//! game's empty slots: a dark square with a shadowed top-left edge and a
//! lighter bottom-right one, and a grey-blue silhouette (here a bow, drawn
//! by the mod pixel by pixel) outlined in a darker shade.

use uk_qol_runtime::egui::{self, Color32, Pos2, Rect, Vec2};
use uk_qol_runtime::hud;

/// Sheet slot indices (`EquipSlot`): the helmet and the weapon.
pub const HELMET: usize = 0;
pub const WEAPON: usize = 4;

/// Colours of the game's empty equipment slots (measured in game 1.0.5).
pub const FILL: Color32 = Color32::from_rgb(28, 28, 33);
const SHADOW: Color32 = Color32::from_rgb(0, 0, 0);
const BEVEL: Color32 = Color32::from_rgb(63, 63, 65);
const SILHOUETTE: Color32 = Color32::from_rgb(45, 45, 57);
const SILHOUETTE_EDGE: Color32 = Color32::from_rgb(15, 15, 27);
/// Outline while an item that fits is on the cursor (like the game's).
pub const FITS: Color32 = Color32::from_rgb(70, 200, 70);
pub const NO_FIT: Color32 = Color32::from_rgb(190, 60, 50);
pub const HOVER: Color32 = Color32::from_rgb(110, 110, 120);
/// The fill behind a stored weapon's label.
pub const STORED: Color32 = Color32::from_rgb(60, 95, 50);

/// Where the ranged slot goes: the size of the helmet slot, on its row,
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

/// Gap between the weapon slot and the ranged slot, in GUI units.
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

/// Size of the bow silhouette's pixel grid.
const BOW_GRID: i32 = 27;

/// Distance from point `p` to the segment `a`-`b`, if `p` projects onto it.
fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> Option<f32> {
    let d = b - a;
    let k = (p - a).dot(d) / d.length_sq();
    (0.0..=1.0).contains(&k).then(|| p.distance(a + d * k))
}

/// The bow silhouette: `(x, y, edge)` per pixel of a `BOW_GRID` square. A
/// bow drawn diagonally like the game's weapon icons: a limb curving
/// through the top-left (thick at the grip, thin at the tips), its string,
/// and an arrow nocked on the string pointing out past the grip. Edge
/// pixels (and the string) take the darker outline shade.
pub fn bow_pixels() -> Vec<(i32, i32, bool)> {
    use std::f32::consts::PI;
    let c = Pos2::new(19.5, 19.5);
    let r = 13.0f32;
    let (mid, span) = (PI * 1.25, 68f32.to_radians());
    let at = |th: f32| c + r * Vec2::new(th.cos(), th.sin());
    let (end_a, end_b, grip) = (at(mid - span), at(mid + span), at(mid));
    let nock = end_a + (end_b - end_a) / 2.0;
    let u = (grip - nock).normalized();
    let (tail, head) = (nock - u * 3.0, grip + u * 5.5);
    let limb = |p: Pos2| {
        let d = ((p.y - c.y).atan2(p.x - c.x).rem_euclid(2.0 * PI) - mid).abs();
        d <= span && ((p - c).length() - r).abs() <= 0.9 + 1.5 * (1.0 - d / span)
    };
    let arrow = |p: Pos2| {
        if seg_dist(p, tail, head).is_some_and(|d| d <= 0.75) {
            return true;
        }
        // The head: a small triangle beyond the shaft.
        let q = p - (head + u * 1.5);
        let along = -q.dot(u);
        let across = q.dot(u.rot90()).abs();
        (0.0..=4.0).contains(&along) && across <= along * 0.75
    };
    let px = |x: i32, y: i32| Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
    let on = |x: i32, y: i32| {
        (0..BOW_GRID).contains(&x)
            && (0..BOW_GRID).contains(&y)
            && (limb(px(x, y)) || arrow(px(x, y)))
    };
    let mut out = Vec::new();
    for y in 0..BOW_GRID {
        for x in 0..BOW_GRID {
            if on(x, y) {
                let edge = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|(dx, dy)| !on(x + dx, y + dy));
                out.push((x, y, edge));
            } else if seg_dist(px(x, y), end_a, end_b).is_some_and(|d| d <= 0.5) {
                out.push((x, y, true));
            }
        }
    }
    out
}

/// The bow silhouette centred in the slot `gui`.
pub fn paint_bow(p: &egui::Painter, t: &hud::Transform, gui: Rect) {
    let px = bow_pixels();
    // Centre the drawing's bounding box, on whole pixels.
    let (x0, x1) = px
        .iter()
        .fold((i32::MAX, i32::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
    let (y0, y1) = px
        .iter()
        .fold((i32::MAX, i32::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let size = Vec2::new((x1 - x0 + 1) as f32, (y1 - y0 + 1) as f32);
    let o = gui.min + ((gui.size() - size) / 2.0).floor() - Vec2::new(x0 as f32, y0 as f32);
    for (x, y, edge) in px {
        let r = Rect::from_min_size(o + Vec2::new(x as f32, y as f32), Vec2::splat(1.0));
        // Slightly larger than a pixel, so neighbours don't show seams.
        let r = t.rect_to_points(r).expand(0.3);
        p.rect_filled(r, 0.0, if edge { SILHOUETTE_EDGE } else { SILHOUETTE });
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
    fn bow_silhouette() {
        let px = bow_pixels();
        assert!(px.len() > 80 && px.len() < 300, "{}", px.len());
        let inside = |&(x, y, _): &(i32, i32, bool)| {
            (0..BOW_GRID).contains(&x) && (0..BOW_GRID).contains(&y)
        };
        assert!(px.iter().all(inside));
        let has = |x, y| px.iter().any(|&(a, b, _)| (a, b) == (x, y));
        // Both tips, the grip, the string between the tips, the arrow's
        // shaft between string and grip.
        assert!(has(7, 24) && has(24, 7));
        assert!(has(10, 10));
        assert!(has(16, 16));
        assert!(has(14, 14));
        // The grip is thick enough to have inner (non-edge) pixels.
        assert!(px.iter().any(|&(_, _, e)| !e));
    }
}
