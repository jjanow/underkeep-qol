//! Short messages drawn over the game while the mod menu is closed (a mode
//! switched by a hotkey, ...). Any thread may call [`show`]; the overlay
//! draws the visible ones at the bottom centre of the screen, newest last,
//! and fades each out at the end of its time.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a toast stays up.
pub const DURATION: Duration = Duration::from_millis(2500);
/// The last part of [`DURATION`] over which a toast fades out.
const FADE: Duration = Duration::from_millis(500);
/// At most this many toasts are kept; a new one drops the oldest.
const MAX: usize = 4;

struct Toast {
    text: String,
    until: Instant,
}

static TOASTS: Mutex<Vec<Toast>> = Mutex::new(Vec::new());

fn toasts() -> std::sync::MutexGuard<'static, Vec<Toast>> {
    TOASTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shows `text` for [`DURATION`]. The same text again replaces the earlier
/// one (pressing a toggle twice doesn't stack two messages).
pub fn show(text: impl Into<String>) {
    push(text.into(), Instant::now());
}

fn push(text: String, now: Instant) {
    let mut t = toasts();
    t.retain(|x| x.text != text && x.until > now);
    if t.len() >= MAX {
        t.remove(0);
    }
    t.push(Toast {
        text,
        until: now + DURATION,
    });
}

/// Whether any toast is visible (the overlay draws a frame only then).
pub fn any() -> bool {
    visible_at(Instant::now()).next().is_some()
}

/// The visible toasts, oldest first, with their opacity (0..=1).
pub fn visible() -> Vec<(String, f32)> {
    visible_at(Instant::now()).collect()
}

fn visible_at(now: Instant) -> impl Iterator<Item = (String, f32)> {
    let mut t = toasts();
    t.retain(|x| x.until > now);
    let out: Vec<_> = t
        .iter()
        .map(|x| {
            let left = x.until.saturating_duration_since(now);
            let alpha = (left.as_secs_f32() / FADE.as_secs_f32()).min(1.0);
            (x.text.clone(), alpha)
        })
        .collect();
    out.into_iter()
}

/// Draws the visible toasts (overlay only).
#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows glue
pub(crate) fn draw(ctx: &egui::Context) {
    let list = visible();
    if list.is_empty() {
        return;
    }
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::unique("uk_qol_toasts"))
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -screen.height() * 0.22])
        .interactable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                for (text, alpha) in &list {
                    let frame = egui::Frame::popup(ui.style()).multiply_with_opacity(*alpha);
                    frame.show(ui, |ui| {
                        ui.set_opacity(*alpha);
                        ui.label(egui::RichText::new(text).size(22.0));
                    });
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test, since the queue is global.
    #[test]
    fn queue() {
        let t0 = Instant::now();
        push("a".into(), t0);
        push("b".into(), t0);
        push("a".into(), t0 + Duration::from_millis(10));
        let v: Vec<_> = visible_at(t0 + Duration::from_millis(20))
            .map(|x| x.0)
            .collect();
        assert_eq!(v, ["b", "a"]);
        // Fading near the end, gone after.
        let late: Vec<_> = visible_at(t0 + DURATION - Duration::from_millis(250)).collect();
        assert_eq!(late.len(), 2);
        assert!(late[0].1 < 1.0 && late[0].1 > 0.0);
        assert!(
            visible_at(t0 + DURATION + Duration::from_secs(1))
                .next()
                .is_none()
        );
        for i in 0..10 {
            push(i.to_string(), t0);
        }
        assert_eq!(visible_at(t0).count(), MAX);
        toasts().clear();
        // Draws without a GL context.
        show("hello");
        assert!(any());
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| draw(ui.ctx()));
        assert!(!out.shapes.is_empty());
        out.textures_delta.clear();
        toasts().clear();
    }
}
