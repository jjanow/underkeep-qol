//! The mod menu itself (egui). Platform-independent: tested headless.

use std::collections::HashMap;

use egui::{Align2, Color32, RichText};

use super::router::Router;
use super::{ModRow, ModState, OverlayHost, SettingKind, SettingRow};
use crate::i18n::{tr, trf};

type Id = (String, String);

/// Menu state kept across frames.
#[derive(Default)]
pub struct UiState {
    /// Values being dragged (sliders) or typed (text fields), not yet sent.
    drafts: HashMap<Id, toml::Value>,
    /// Last error per setting (key "" = the mod's on/off switch).
    errors: HashMap<Id, String>,
}

const GREEN: Color32 = Color32::from_rgb(110, 200, 110);
const RED: Color32 = Color32::from_rgb(230, 100, 90);
const AMBER: Color32 = Color32::from_rgb(230, 180, 80);

fn id(m: &str, k: &str) -> Id {
    (m.to_owned(), k.to_owned())
}

impl UiState {
    fn apply(&mut self, host: &dyn OverlayHost, m: &str, k: &str, v: toml::Value) {
        self.drafts.remove(&id(m, k));
        match host.set_setting(m, k, v) {
            Ok(()) => {
                self.errors.remove(&id(m, k));
            }
            Err(e) => {
                self.errors.insert(id(m, k), e);
            }
        }
    }
}

/// Draws the menu. Returns true if the user asked to close it.
pub fn menu(
    ctx: &egui::Context,
    host: Option<&dyn OverlayHost>,
    router: &mut Router,
    st: &mut UiState,
) -> bool {
    if let (Some(h), Some((m, k, spec))) = (host, router.captured.take()) {
        st.apply(h, &m, &k, toml::Value::String(spec));
    }
    let mut close = false;
    let mut focus_first = std::mem::take(&mut router.focus_first);
    let screen = ctx.content_rect();
    egui::Window::new(tr("menu.title"))
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .max_height(screen.height() * 0.8)
        .anchor(Align2::CENTER_TOP, [0.0, screen.height() * 0.08])
        .show(ctx, |ui| {
            ui.weak(trf("menu.version", &[crate::VERSION]));
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(screen.height() * 0.6)
                .show(ui, |ui| match host {
                    None => {
                        ui.label(tr("menu.no_host"));
                    }
                    Some(h) => {
                        let mods = h.mods();
                        if mods.is_empty() {
                            ui.label(tr("menu.no_mods"));
                        }
                        for row in &mods {
                            mod_section(ui, h, row, router, st, &mut focus_first);
                        }
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                let b = ui.button(tr("menu.close"));
                if focus_first {
                    b.request_focus();
                    focus_first = false;
                }
                if b.clicked() {
                    close = true;
                }
                ui.weak(trf(
                    "menu.close_hint",
                    &[&router.menu_key.to_string(), &router.menu_pad.to_string()],
                ));
            });
        });
    close
}

fn status(ui: &mut egui::Ui, state: &ModState) {
    match state {
        ModState::Active => ui.colored_label(GREEN, tr("status.active")),
        ModState::Off => ui.weak(tr("status.off")),
        ModState::Failed(r) => ui.colored_label(RED, trf("status.failed", &[r])),
        ModState::Unsupported(r) => ui.colored_label(AMBER, trf("status.unsupported", &[r])),
    };
}

fn mod_section(
    ui: &mut egui::Ui,
    h: &dyn OverlayHost,
    row: &ModRow,
    router: &mut Router,
    st: &mut UiState,
    focus_first: &mut bool,
) {
    let title = format!("{}  {}", row.name, row.version);
    let header = egui::CollapsingHeader::new(RichText::new(title).strong())
        .id_salt(&row.id)
        .default_open(false)
        .show(ui, |ui| {
            let mut on = row.enabled;
            if ui.checkbox(&mut on, tr("mod.enabled")).changed() {
                match h.set_enabled(&row.id, on) {
                    Ok(()) => st.errors.remove(&id(&row.id, "")),
                    Err(e) => st.errors.insert(id(&row.id, ""), e),
                };
            }
            if let Some(e) = st.errors.get(&id(&row.id, "")) {
                ui.colored_label(RED, trf("setting.error", &[e]));
            }
            status(ui, &row.state);
            if !row.description.is_empty() {
                ui.label(&row.description);
            }
            if !row.settings.is_empty() {
                ui.add_space(4.0);
                egui::Grid::new(("settings", &row.id))
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        for s in &row.settings {
                            setting(ui, h, &row.id, s, router, st);
                            ui.end_row();
                        }
                    });
            }
        });
    if std::mem::take(focus_first) {
        header.header_response.request_focus();
    }
    // Status in the collapsed header line too.
    if !header.fully_open() {
        ui.indent(("status", &row.id), |ui| status(ui, &row.state));
    }
}

fn setting(
    ui: &mut egui::Ui,
    h: &dyn OverlayHost,
    m: &str,
    s: &SettingRow,
    router: &mut Router,
    st: &mut UiState,
) {
    let label = if s.label.is_empty() { &s.key } else { &s.label };
    let l = ui.label(label);
    if !s.help.is_empty() {
        l.on_hover_text(&s.help);
    }
    let key = id(m, &s.key);
    ui.vertical(|ui| {
        ui.horizontal(|ui| match &s.kind {
            SettingKind::Bool => {
                let mut v = s.value.as_bool().unwrap_or(false);
                if ui.checkbox(&mut v, "").changed() {
                    st.apply(h, m, &s.key, toml::Value::Boolean(v));
                }
            }
            SettingKind::Int { min, max } => {
                let mut v = st
                    .drafts
                    .get(&key)
                    .and_then(toml::Value::as_integer)
                    .or(s.value.as_integer())
                    .unwrap_or(0);
                let r = match (min, max) {
                    (Some(lo), Some(hi)) => ui.add(egui::Slider::new(&mut v, *lo..=*hi)),
                    _ => ui.add(
                        egui::DragValue::new(&mut v)
                            .range(min.unwrap_or(i64::MIN)..=max.unwrap_or(i64::MAX)),
                    ),
                };
                number_commit(st, h, m, &s.key, &r, toml::Value::Integer(v));
            }
            SettingKind::Float { min, max } => {
                let mut v = st
                    .drafts
                    .get(&key)
                    .and_then(toml::Value::as_float)
                    .or(s.value.as_float())
                    .unwrap_or(0.0);
                let r = match (min, max) {
                    (Some(lo), Some(hi)) => ui.add(egui::Slider::new(&mut v, *lo..=*hi)),
                    _ => ui.add(
                        egui::DragValue::new(&mut v)
                            .speed(0.1)
                            .range(min.unwrap_or(f64::MIN)..=max.unwrap_or(f64::MAX)),
                    ),
                };
                number_commit(st, h, m, &s.key, &r, toml::Value::Float(v));
            }
            SettingKind::String => {
                let mut text = st
                    .drafts
                    .get(&key)
                    .and_then(toml::Value::as_str)
                    .or(s.value.as_str())
                    .unwrap_or("")
                    .to_owned();
                let r = ui.text_edit_singleline(&mut text);
                if r.changed() {
                    st.drafts
                        .insert(key.clone(), toml::Value::String(text.clone()));
                }
                if r.lost_focus() && st.drafts.contains_key(&key) {
                    st.apply(h, m, &s.key, toml::Value::String(text));
                }
            }
            SettingKind::Enum(choices) => {
                let current = s.value.as_str().unwrap_or("");
                for c in choices {
                    if ui.selectable_label(current == c, c).clicked() && current != c {
                        st.apply(h, m, &s.key, toml::Value::String(c.clone()));
                    }
                }
            }
            SettingKind::Key => {
                let capturing = router
                    .capture
                    .as_ref()
                    .is_some_and(|c| c.mod_id == m && c.key == s.key);
                if capturing {
                    ui.colored_label(AMBER, tr("setting.press"));
                } else {
                    let spec = s.value.as_str().unwrap_or("");
                    let shown = if spec.is_empty() {
                        tr("setting.unbound")
                    } else {
                        spec
                    };
                    ui.monospace(shown);
                    if ui.button(tr("setting.rebind")).clicked() {
                        router.start_capture(m, &s.key);
                    }
                    if !spec.is_empty() && ui.button(tr("setting.clear")).clicked() {
                        st.apply(h, m, &s.key, toml::Value::String(String::new()));
                    }
                }
            }
        });
        if !matches!(s.kind, SettingKind::Bool)
            && s.value != s.default
            && ui.small_button(tr("setting.default")).clicked()
        {
            st.apply(h, m, &s.key, s.default.clone());
        }
        if let Some(e) = st.errors.get(&key) {
            ui.colored_label(RED, trf("setting.error", &[e]));
        }
    });
}

/// Sliders and drag fields send the value when the drag ends (or right away
/// for clicks and keyboard / gamepad changes), not every frame of a drag.
fn number_commit(
    st: &mut UiState,
    h: &dyn OverlayHost,
    m: &str,
    k: &str,
    r: &egui::Response,
    v: toml::Value,
) {
    if r.changed() {
        if r.dragged() {
            st.drafts.insert(id(m, k), v);
        } else {
            st.apply(h, m, k, v);
        }
    } else if r.drag_stopped()
        && let Some(v) = st.drafts.remove(&id(m, k))
    {
        st.apply(h, m, k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeHost {
        rows: Mutex<Vec<ModRow>>,
        calls: Mutex<Vec<String>>,
    }

    impl OverlayHost for FakeHost {
        fn mods(&self) -> Vec<ModRow> {
            self.rows.lock().unwrap().clone()
        }
        fn set_enabled(&self, m: &str, on: bool) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("{m} enabled={on}"));
            Ok(())
        }
        fn set_setting(&self, m: &str, k: &str, v: toml::Value) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("{m}.{k}={v}"));
            if k == "bad" {
                Err("nope".into())
            } else {
                Ok(())
            }
        }
    }

    fn row() -> ModRow {
        let s = |key: &str, kind, value: toml::Value| SettingRow {
            key: key.into(),
            label: String::new(),
            help: "help".into(),
            kind,
            default: value.clone(),
            value,
        };
        ModRow {
            id: "inventory-sort".into(),
            name: "Inventory sort".into(),
            version: "0.1.0".into(),
            description: "Sorts.".into(),
            enabled: true,
            state: ModState::Failed("pattern `sort_fn` not found".into()),
            settings: vec![
                s("on", SettingKind::Bool, true.into()),
                s(
                    "n",
                    SettingKind::Int {
                        min: Some(1),
                        max: Some(5),
                    },
                    3.into(),
                ),
                s(
                    "f",
                    SettingKind::Float {
                        min: None,
                        max: None,
                    },
                    0.5.into(),
                ),
                s("name", SettingKind::String, "x".into()),
                s(
                    "mode",
                    SettingKind::Enum(vec!["bones".into(), "health".into()]),
                    "bones".into(),
                ),
                s("key", SettingKind::Key, "S".into()),
            ],
        }
    }

    fn frame(
        ctx: &egui::Context,
        h: &FakeHost,
        r: &mut Router,
        st: &mut UiState,
        events: Vec<egui::Event>,
    ) -> bool {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Default::default(),
                egui::vec2(1280.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let mut close = false;
        ctx.run_ui(raw, |ui| close = menu(ui.ctx(), Some(h), r, st))
            .drop_without_applying_deltas();
        close
    }

    #[test]
    fn renders_and_applies_capture() {
        let ctx = egui::Context::default();
        let h = FakeHost {
            rows: Mutex::new(vec![row()]),
            calls: Mutex::new(Vec::new()),
        };
        let mut r = Router::new();
        let mut st = UiState::default();
        for _ in 0..3 {
            assert!(!frame(&ctx, &h, &mut r, &mut st, vec![]));
        }
        r.captured = Some(("inventory-sort".into(), "key".into(), "Shift+S".into()));
        frame(&ctx, &h, &mut r, &mut st, vec![]);
        assert_eq!(
            *h.calls.lock().unwrap(),
            vec!["inventory-sort.key=\"Shift+S\"".to_string()]
        );
        // Errors stick to the setting.
        r.captured = Some(("inventory-sort".into(), "bad".into(), "".into()));
        frame(&ctx, &h, &mut r, &mut st, vec![]);
        assert_eq!(
            st.errors
                .get(&id("inventory-sort", "bad"))
                .map(String::as_str),
            Some("nope")
        );
    }

    #[test]
    fn keyboard_focus_reaches_close() {
        // Opened from a pad: focus lands on the first widget, and Enter on
        // the Close button closes (what the pad's A does).
        let ctx = egui::Context::default();
        let h = FakeHost {
            rows: Mutex::new(vec![]),
            calls: Mutex::new(Vec::new()),
        };
        let mut r = Router::new();
        let mut st = UiState::default();
        r.focus_first = true;
        frame(&ctx, &h, &mut r, &mut st, vec![]);
        frame(&ctx, &h, &mut r, &mut st, vec![]);
        let key = |pressed| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        };
        let closed = frame(&ctx, &h, &mut r, &mut st, vec![key(true), key(false)]);
        assert!(closed);
    }

    #[test]
    fn no_host() {
        let ctx = egui::Context::default();
        let mut r = Router::new();
        let mut st = UiState::default();
        let raw = egui::RawInput::default();
        ctx.run_ui(raw, |ui| {
            assert!(!menu(ui.ctx(), None, &mut r, &mut st));
        })
        .drop_without_applying_deltas();
    }
}
