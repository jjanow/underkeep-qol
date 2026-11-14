//! Connects the mod host to the overlay (menu rows, switches, settings),
//! to input (hotkeys, `on_input`), to the frame loop (`on_frame`) and to the
//! config file (saved on change in the menu, reloaded when edited outside).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{self, Config, Watcher};
use crate::host::{self, Host, SettingKind as HK, Status};
use crate::input::InputEvent;
use crate::overlay::{self, ModRow, ModState, OverlayHost, SettingKind as OK, SettingRow};

/// How often the config file is checked for outside edits.
const CONFIG_POLL: Duration = Duration::from_secs(1);

struct Watch {
    watcher: Watcher,
    last: Instant,
}

static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

/// Wires everything up. Call once after the host is installed.
pub fn install() {
    if let Some(dir) = crate::data_dir() {
        let mut watcher = Watcher::new(config::config_path(&dir));
        watcher.sync();
        *WATCH.lock().unwrap_or_else(|e| e.into_inner()) = Some(Watch {
            watcher,
            last: Instant::now(),
        });
    }
    host::with(bind_all);
    overlay::set_host(Arc::new(Bridge));
    overlay::set_input_sink(input_sink);
    overlay::set_frame_sink(frame_sink);
}

/// Registers every mod's key settings with the hotkey system.
fn bind_all(h: &mut Host) {
    for m in h.mods() {
        bind_mod(h, &m.id);
    }
}

fn bind_mod(h: &Host, id: &str) {
    crate::hotkey::clear(id);
    let (Some(manifest), Some(settings)) = (h.manifest(id), h.settings(id)) else {
        return;
    };
    for s in manifest.settings.iter().filter(|s| s.kind == HK::Key) {
        let spec = settings.get(&s.key).and_then(|v| v.as_str()).unwrap_or("");
        if let Err(e) = crate::hotkey::set_binding(id, &s.key, spec) {
            crate::log(&format!("mod {id}: key {}: {e}", s.key));
        }
    }
}

fn input_sink(ev: &InputEvent) -> bool {
    host::with(|h| h.input(ev)).unwrap_or(false)
}

fn frame_sink() {
    host::with(|h| h.frame());
    let changed = {
        let mut g = WATCH.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_mut() {
            Some(w) if w.last.elapsed() >= CONFIG_POLL => {
                w.last = Instant::now();
                w.watcher.poll()
            }
            _ => None,
        }
    };
    if let Some(cfg) = changed {
        crate::log("config.toml changed; applying it");
        host::with(|h| {
            h.apply_config(&cfg);
            bind_all(h);
        });
    }
}

/// Writes the host's config to `config.toml` and tells the watcher, so the
/// write isn't taken for an outside edit.
fn save(cfg: &Config) -> Result<(), String> {
    let dir = crate::data_dir().ok_or("no game folder")?;
    cfg.save(&config::config_path(&dir))
        .map_err(|e| format!("couldn't save config.toml: {e}"))?;
    if let Some(w) = WATCH.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        w.watcher.sync();
    }
    Ok(())
}

fn kind(k: &HK) -> OK {
    match k {
        HK::Bool => OK::Bool,
        HK::Int { min, max } => OK::Int {
            min: *min,
            max: *max,
        },
        HK::Float { min, max } => OK::Float {
            min: *min,
            max: *max,
        },
        HK::String => OK::String,
        HK::Enum { choices } => OK::Enum(choices.clone()),
        HK::Key => OK::Key,
    }
}

fn state(status: &Status, reason: Option<String>) -> ModState {
    let why = |r: String| reason.clone().unwrap_or(r);
    match status {
        Status::Active => ModState::Active,
        Status::DisabledByUser => ModState::Off,
        Status::SafeMode => ModState::Unsupported(why("safe mode: all mods are off".into())),
        Status::PatternsMissing(p) => ModState::Unsupported(why(format!(
            "doesn't work on this game version (not found: {})",
            p.join(", ")
        ))),
        Status::Blocked(r) => ModState::Unsupported(why(r.clone())),
        Status::Failed(r) => ModState::Failed(why(r.clone())),
    }
}

fn rows(h: &Host) -> Vec<ModRow> {
    let cfg = h.config();
    h.mods()
        .into_iter()
        .map(|m| {
            let manifest = h.manifest(&m.id);
            let current = h.settings(&m.id);
            let settings = manifest
                .map(|man| {
                    man.settings
                        .iter()
                        .map(|s| SettingRow {
                            key: s.key.clone(),
                            label: s.label.clone(),
                            help: s.description.clone(),
                            kind: kind(&s.kind),
                            value: current
                                .and_then(|t| t.get(&s.key))
                                .cloned()
                                .unwrap_or_else(|| s.default.clone()),
                            default: s.default.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let default_on = manifest.is_some_and(|man| man.enabled);
            ModRow {
                enabled: cfg.is_enabled(&m.id, default_on),
                state: state(&m.status, m.reason.clone()),
                id: m.id,
                name: m.name,
                version: m.version,
                description: m.description,
                settings,
            }
        })
        .collect()
}

struct Bridge;

impl OverlayHost for Bridge {
    fn mods(&self) -> Vec<ModRow> {
        host::with(|h| rows(h)).unwrap_or_default()
    }

    fn set_enabled(&self, mod_id: &str, enabled: bool) -> Result<(), String> {
        let cfg = host::with(|h| {
            let status = h.set_enabled(mod_id, enabled)?;
            crate::log(&format!(
                "mod {mod_id}: switched {} in the menu: {status}",
                if enabled { "on" } else { "off" }
            ));
            bind_mod(h, mod_id);
            Ok::<_, String>(h.config())
        })
        .ok_or("the mod host is busy")??;
        save(&cfg)
    }

    fn set_setting(&self, mod_id: &str, key: &str, value: toml::Value) -> Result<(), String> {
        let cfg = host::with(|h| {
            let def = h
                .manifest(mod_id)
                .and_then(|m| m.setting(key))
                .ok_or_else(|| format!("{mod_id} has no setting {key}"))?;
            let value = def.check(&value)?;
            let mut cfg = h.config();
            cfg.set_setting(mod_id, key, value);
            h.apply_config(&cfg);
            bind_mod(h, mod_id);
            Ok::<_, String>(h.config())
        })
        .ok_or("the mod host is busy")??;
        save(&cfg)
    }

    fn mod_menu(&self, mod_id: &str, ui: &mut egui::Ui) {
        host::with(|h| h.menu(mod_id, ui));
    }

    fn wants_hud(&self) -> bool {
        host::with(|h| h.wants_hud()).unwrap_or(false)
    }

    fn hud(&self, ui: &mut egui::Ui) {
        host::with(|h| h.hud(ui));
    }
}
