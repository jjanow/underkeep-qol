//! The desktop app (egui), styled like Underkeeper: pick the game, install or
//! uninstall, switch mods and their settings, see what works on this game
//! build, set up the Linux launch option, update.
//!
//! Opening the app only reads. Files change only from a button press, and
//! never while the game runs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, ThemePreference, Vec2};

use crate::config::{self, ConfigFile};
use crate::install::{self, DllState, InstallOpts, UninstallOpts};
use crate::launch::{self, LaunchState};
use crate::mods::{Compat, SettingInfo, SettingKind};
use crate::paths::{self, GameInstall};
use crate::status::{self, Status};
use crate::update::{self, Release};
use crate::{process, report, tr};

const ACCENT: Color32 = Color32::from_rgb(0xE2, 0x8A, 0x3B); // ember
const ACCENT_DARK: Color32 = Color32::from_rgb(0xB8, 0x62, 0x1B);
const OK_GREEN: Color32 = Color32::from_rgb(0x4C, 0xAF, 0x50);
const BAD_RED: Color32 = Color32::from_rgb(0xE5, 0x48, 0x4D);
const WARN_AMBER: Color32 = Color32::from_rgb(0xD9, 0xA4, 0x41);

pub fn run(game: Option<PathBuf>) -> ExitCode {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(tr!("app_title"))
            .with_app_id("uk-qol")
            .with_inner_size([1080.0, 740.0])
            .with_min_inner_size([820.0, 520.0]),
        ..Default::default()
    };
    let res = eframe::run_native(
        "uk-qol",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, game)))),
    );
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("uk-qol: {e}");
            ExitCode::FAILURE
        }
    }
}

// --- small persistent preferences (theme) ------------------------------

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("uk-qol").join("settings.txt"))
}

fn load_setting(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(settings_path()?).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")).map(str::to_string))
}

fn store_setting(key: &str, value: &str) {
    let Some(p) = settings_path() else { return };
    let mut lines: Vec<String> = std::fs::read_to_string(&p)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with(&format!("{key}=")))
        .map(String::from)
        .collect();
    lines.push(format!("{key}={value}"));
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(p, lines.join("\n") + "\n");
}

fn open_in_file_manager(p: &std::path::Path) {
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd).arg(p).spawn();
}

fn setup_style(ctx: &egui::Context) {
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            use egui::{FontId, TextStyle};
            style.text_styles = [
                (TextStyle::Small, FontId::proportional(12.0)),
                (TextStyle::Body, FontId::proportional(15.0)),
                (TextStyle::Button, FontId::proportional(15.0)),
                (TextStyle::Monospace, FontId::monospace(14.0)),
                (TextStyle::Heading, FontId::proportional(22.0)),
            ]
            .into();
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            style.spacing.button_padding = Vec2::new(10.0, 5.0);
            style.spacing.interact_size.y = 26.0;
            let v = &mut style.visuals;
            let r = CornerRadius::same(6);
            v.widgets.noninteractive.corner_radius = r;
            v.widgets.inactive.corner_radius = r;
            v.widgets.hovered.corner_radius = r;
            v.widgets.active.corner_radius = r;
            v.widgets.open.corner_radius = r;
            v.window_corner_radius = CornerRadius::same(10);
            v.selection.bg_fill = if theme == egui::Theme::Dark {
                ACCENT_DARK
            } else {
                ACCENT
            };
            v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
            v.hyperlink_color = ACCENT;
            if theme == egui::Theme::Dark {
                v.panel_fill = Color32::from_rgb(0x17, 0x18, 0x1C);
                v.window_fill = Color32::from_rgb(0x1E, 0x20, 0x25);
                v.extreme_bg_color = Color32::from_rgb(0x10, 0x11, 0x14);
                v.faint_bg_color = Color32::from_rgb(0x22, 0x24, 0x2A);
            } else {
                v.panel_fill = Color32::from_rgb(0xF4, 0xF2, 0xEF);
                v.window_fill = Color32::from_rgb(0xFB, 0xFA, 0xF8);
                v.faint_bg_color = Color32::from_rgb(0xEC, 0xE9, 0xE4);
                v.extreme_bg_color = Color32::from_rgb(0xF0, 0xEE, 0xEA);
            }
        });
    }
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let fill = if ui.visuals().dark_mode {
        Color32::from_rgb(0x20, 0x22, 0x28)
    } else {
        Color32::WHITE
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).strong().size(15.0));
    ui.add_space(2.0);
}

/// An on/off switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = ui.spacing().interact_size.y * Vec2::new(1.9, 0.9);
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(resp.id, *on);
        let vis = ui.style().interact_selectable(&resp, *on);
        let fill = if *on { ACCENT } else { vis.bg_fill };
        let radius = rect.height() / 2.0;
        ui.painter()
            .rect(rect, radius, fill, vis.bg_stroke, egui::StrokeKind::Inside);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        ui.painter().circle(
            egui::pos2(x, rect.center().y),
            0.75 * radius,
            Color32::WHITE,
            vis.fg_stroke,
        );
    }
    resp
}

// --- update state shared with the background thread --------------------

#[derive(Clone)]
struct UpdatePlan {
    rel: Release,
    self_newer: bool,
    runtime_newer: bool,
    patterns: Vec<(String, Vec<u8>)>,
}

#[derive(Clone)]
enum UpdateState {
    Idle,
    Checking {
        manual: bool,
    },
    NoRelease {
        manual: bool,
    },
    UpToDate {
        manual: bool,
    },
    Available(UpdatePlan),
    Working,
    /// Done; with the path to restart if the manager itself was updated.
    Done(String, Option<PathBuf>),
    Failed(String),
}

struct Msg {
    text: String,
    error: bool,
    at: Instant,
}

pub struct App {
    installs: Vec<GameInstall>,
    game: Option<GameInstall>,
    status: Option<Status>,
    dll_override: Option<PathBuf>,
    theme: ThemePreference,
    msg: Option<Msg>,
    game_running: bool,
    steam_running: bool,
    last_proc_check: Instant,
    confirm_uninstall: bool,
    purge: bool,
    /// Replace a winmm.dll uk-qol didn't install.
    force: bool,
    update: Arc<Mutex<UpdateState>>,
    update_dismissed: bool,
    /// Text being typed into text/key settings, by (mod id, key).
    edits: HashMap<(String, String), String>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, game: Option<PathBuf>) -> App {
        setup_style(&cc.egui_ctx);
        let theme = match load_setting("theme").as_deref() {
            Some("dark") => ThemePreference::Dark,
            Some("light") => ThemePreference::Light,
            _ => ThemePreference::System,
        };
        cc.egui_ctx.set_theme(theme);
        let installs = paths::find_installs();
        let game = match game {
            Some(p) if paths::is_game_dir(&p) => Some(paths::describe(&p)),
            _ => load_setting("game")
                .map(PathBuf::from)
                .filter(|p| paths::is_game_dir(p))
                .map(|p| paths::describe(&p))
                .or_else(|| installs.first().cloned()),
        };
        let mut app = App {
            installs,
            game,
            status: None,
            dll_override: None,
            theme,
            msg: None,
            game_running: false,
            steam_running: false,
            last_proc_check: Instant::now() - Duration::from_secs(60),
            confirm_uninstall: false,
            purge: false,
            force: false,
            update: Arc::new(Mutex::new(UpdateState::Idle)),
            update_dismissed: false,
            edits: HashMap::new(),
        };
        app.refresh();
        app.start_update_check(&cc.egui_ctx, false);
        app
    }

    fn refresh(&mut self) {
        self.status = self.game.as_ref().map(status::gather);
    }

    fn set_msg(&mut self, text: impl Into<String>, error: bool) {
        self.msg = Some(Msg {
            text: text.into(),
            error,
            at: Instant::now(),
        });
    }

    fn check_processes(&mut self) {
        if self.last_proc_check.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_proc_check = Instant::now();
        let procs = process::processes();
        self.game_running = procs.iter().any(process::is_game);
        self.steam_running = procs.iter().any(process::is_steam);
    }

    fn select_game(&mut self, g: GameInstall) {
        store_setting("game", &g.dir.to_string_lossy());
        self.game = Some(g);
        self.edits.clear();
        self.refresh();
    }

    // --- updates ---

    fn update_state(&self) -> UpdateState {
        self.update
            .lock()
            .map(|s| s.clone())
            .unwrap_or(UpdateState::Idle)
    }

    fn start_update_check(&mut self, ctx: &egui::Context, manual: bool) {
        if matches!(
            self.update_state(),
            UpdateState::Checking { .. } | UpdateState::Working
        ) {
            return;
        }
        *self.update.lock().unwrap() = UpdateState::Checking { manual };
        self.update_dismissed = false;
        let shared = self.update.clone();
        let ctx = ctx.clone();
        let game = self.game.clone();
        std::thread::spawn(move || {
            let st = match plan_update(game.as_ref()) {
                Ok(None) => UpdateState::NoRelease { manual },
                Ok(Some(p)) if !p.self_newer && !p.runtime_newer && p.patterns.is_empty() => {
                    UpdateState::UpToDate { manual }
                }
                Ok(Some(p)) => UpdateState::Available(p),
                // A failed automatic check stays quiet.
                Err(e) if manual => UpdateState::Failed(e),
                Err(_) => UpdateState::Idle,
            };
            *shared.lock().unwrap() = st;
            ctx.request_repaint();
        });
    }

    fn start_update_install(&mut self, ctx: &egui::Context, plan: UpdatePlan) {
        *self.update.lock().unwrap() = UpdateState::Working;
        let shared = self.update.clone();
        let ctx = ctx.clone();
        let game = self.game.clone();
        std::thread::spawn(move || {
            let st = match apply_update(game.as_ref(), &plan) {
                Ok((msg, restart)) => UpdateState::Done(msg, restart),
                Err(e) => UpdateState::Failed(e),
            };
            *shared.lock().unwrap() = st;
            ctx.request_repaint();
        });
    }

    // --- actions ---

    fn do_install(&mut self) {
        let Some(g) = self.game.clone() else { return };
        if process::game_running() {
            self.set_msg(tr!("err_game_running"), true);
            return;
        }
        let src = match &self.dll_override {
            Some(p) => std::fs::read(p)
                .map(|b| (b, "unknown".to_string()))
                .map_err(|e| tr!("err_read", p.display(), e)),
            None => match install::bundled_dll() {
                Some(p) => std::fs::read(&p)
                    .map(|b| (b, update::current_version()))
                    .map_err(|e| tr!("err_read", p.display(), e)),
                None => update::latest().and_then(|r| {
                    let rel = r.ok_or_else(|| tr!("err_no_dll_no_release"))?;
                    Ok((update::fetch_runtime(&rel)?, rel.version))
                }),
            },
        };
        let res = src.and_then(|(dll, version)| {
            install::install(
                &g.dir,
                &InstallOpts {
                    dll: &dll,
                    runtime_version: version,
                    force: self.force,
                },
            )
        });
        match res {
            Ok(o) if o.updated => self.set_msg(tr!("done_updated", g.dir.display()), false),
            Ok(_) => self.set_msg(tr!("done_installed", g.dir.display()), false),
            Err(e) => self.set_msg(e, true),
        }
        self.refresh();
    }

    fn do_uninstall(&mut self) {
        let Some(g) = self.game.clone() else { return };
        if process::game_running() {
            self.set_msg(tr!("err_game_running"), true);
            return;
        }
        match install::uninstall(
            &g.dir,
            &UninstallOpts {
                force: self.force,
                purge: self.purge,
            },
        ) {
            Ok(o) => {
                let mut text = tr!("done_uninstalled");
                if o.sidecars_kept {
                    text.push(' ');
                    text.push_str(&tr!("kept_sidecars", install::SAVES_DIR));
                }
                self.set_msg(text, false);
            }
            Err(e) => self.set_msg(e, true),
        }
        self.refresh();
    }

    fn save_config(&mut self, c: &ConfigFile) {
        if process::game_running() {
            self.set_msg(tr!("err_game_running"), true);
            self.refresh();
            return;
        }
        if let Err(e) = c.save() {
            self.set_msg(e, true);
        }
        self.refresh();
    }

    fn save_report(&mut self) {
        let Some(st) = &self.status else { return };
        let text = report::build_report(st);
        let Some(path) = rfd::FileDialog::new()
            .set_title(tr!("gui_report_title"))
            .set_file_name("uk-qol-report.txt")
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, text) {
            Ok(()) => self.set_msg(tr!("done_report", path.display()), false),
            Err(e) => self.set_msg(tr!("err_write", path.display(), e), true),
        }
    }

    // --- panels ---

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(RichText::new(tr!("app_title")).strong());
            ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let before = self.theme;
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(match self.theme {
                        ThemePreference::Dark => tr!("gui_theme_dark"),
                        ThemePreference::Light => tr!("gui_theme_light"),
                        ThemePreference::System => tr!("gui_theme_system"),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.theme,
                            ThemePreference::System,
                            tr!("gui_theme_system"),
                        );
                        ui.selectable_value(
                            &mut self.theme,
                            ThemePreference::Dark,
                            tr!("gui_theme_dark"),
                        );
                        ui.selectable_value(
                            &mut self.theme,
                            ThemePreference::Light,
                            tr!("gui_theme_light"),
                        );
                    });
                if before != self.theme {
                    ui.ctx().set_theme(self.theme);
                    store_setting(
                        "theme",
                        match self.theme {
                            ThemePreference::Dark => "dark",
                            ThemePreference::Light => "light",
                            ThemePreference::System => "system",
                        },
                    );
                }
                let busy = matches!(
                    self.update_state(),
                    UpdateState::Checking { .. } | UpdateState::Working
                );
                if ui
                    .add_enabled(!busy, egui::Button::new(tr!("gui_check_updates")))
                    .clicked()
                {
                    self.start_update_check(ui.ctx(), true);
                }
            });
        });
    }

    fn update_banner(&mut self, ui: &mut egui::Ui) {
        let state = self.update_state();
        ui.horizontal_wrapped(|ui| match state {
            UpdateState::Available(plan) => {
                let mut parts = Vec::new();
                if plan.self_newer {
                    parts.push(tr!("gui_update_part_self"));
                }
                if plan.runtime_newer {
                    parts.push(tr!("gui_update_part_runtime"));
                }
                if !plan.patterns.is_empty() {
                    parts.push(tr!("gui_update_part_patterns", plan.patterns.len()));
                }
                ui.label(
                    RichText::new(tr!(
                        "gui_update_available",
                        plan.rel.version,
                        parts.join(", ")
                    ))
                    .strong(),
                );
                ui.hyperlink_to(tr!("gui_release_notes"), &plan.rel.page_url);
                let blocked =
                    (plan.runtime_newer || !plan.patterns.is_empty()) && self.game_running;
                if ui
                    .add_enabled(!blocked, egui::Button::new(tr!("gui_update_now")))
                    .on_disabled_hover_text(tr!("err_game_running"))
                    .clicked()
                {
                    self.start_update_install(ui.ctx(), plan);
                }
                if ui.button(tr!("gui_later")).clicked() {
                    self.update_dismissed = true;
                }
            }
            UpdateState::Checking { .. } => {
                ui.spinner();
                ui.label(tr!("update_checking"));
            }
            UpdateState::Working => {
                ui.spinner();
                ui.label(tr!("gui_updating"));
            }
            UpdateState::Done(text, restart) => {
                ui.label(RichText::new(text).color(OK_GREEN));
                if let Some(path) = restart
                    && ui.button(tr!("gui_restart")).clicked()
                {
                    match update::relaunch(&path) {
                        Ok(()) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(e) => self.set_msg(e, true),
                    }
                }
                if ui.button(tr!("gui_ok")).clicked() {
                    *self.update.lock().unwrap() = UpdateState::Idle;
                    self.refresh();
                }
            }
            UpdateState::Failed(e) => {
                ui.label(RichText::new(e).color(BAD_RED));
                if ui.button(tr!("gui_ok")).clicked() {
                    *self.update.lock().unwrap() = UpdateState::Idle;
                }
            }
            UpdateState::NoRelease { .. } => {
                ui.label(tr!("update_none_published"));
                if ui.button(tr!("gui_ok")).clicked() {
                    *self.update.lock().unwrap() = UpdateState::Idle;
                }
            }
            UpdateState::UpToDate { .. } => {
                ui.label(tr!("update_up_to_date"));
                if ui.button(tr!("gui_ok")).clicked() {
                    *self.update.lock().unwrap() = UpdateState::Idle;
                }
            }
            UpdateState::Idle => {}
        });
    }

    fn update_banner_visible(&self) -> bool {
        match self.update_state() {
            UpdateState::Idle => false,
            UpdateState::Available(_) => !self.update_dismissed,
            UpdateState::Checking { manual }
            | UpdateState::NoRelease { manual }
            | UpdateState::UpToDate { manual } => manual,
            UpdateState::Working | UpdateState::Done(..) | UpdateState::Failed(_) => true,
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            card(ui, |ui| self.game_card(ui));
            ui.add_space(8.0);
            if launch::needed() && self.game.is_some() {
                card(ui, |ui| self.launch_card(ui));
                ui.add_space(8.0);
            }
            if self.status.is_some() {
                card(ui, |ui| {
                    section_title(ui, &tr!("gui_help_title"));
                    ui.label(RichText::new(tr!("gui_report_help")).weak());
                    if ui.button(tr!("gui_save_report")).clicked() {
                        self.save_report();
                    }
                });
            }
        });
    }

    fn game_card(&mut self, ui: &mut egui::Ui) {
        section_title(ui, &tr!("label_game"));
        let selected = self
            .game
            .as_ref()
            .map_or_else(|| tr!("gui_no_game"), |g| g.dir.display().to_string());
        let mut pick: Option<GameInstall> = None;
        egui::ComboBox::from_id_salt("game")
            .width(ui.available_width() - 4.0)
            .selected_text(RichText::new(selected).small())
            .show_ui(ui, |ui| {
                for g in &self.installs {
                    let label = format!("{}  [{}]", g.dir.display(), g.source);
                    if ui
                        .selectable_label(self.game.as_ref().is_some_and(|s| s.dir == g.dir), label)
                        .clicked()
                    {
                        pick = Some(g.clone());
                    }
                }
            });
        ui.horizontal(|ui| {
            if ui.button(tr!("gui_browse")).clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .set_title(tr!("gui_pick_game"))
                    .pick_folder()
            {
                if paths::is_game_dir(&p) {
                    pick = Some(paths::describe(&p));
                } else {
                    self.set_msg(tr!("err_not_game_dir", p.display()), true);
                }
            }
            if ui.button(tr!("gui_rescan")).clicked() {
                self.installs = paths::find_installs();
                self.refresh();
            }
            if let Some(g) = &self.game
                && ui.button(tr!("gui_open_folder")).clicked()
            {
                open_in_file_manager(&g.dir);
            }
        });
        if let Some(g) = pick {
            self.select_game(g);
        }
        let Some(st) = &self.status else {
            ui.label(RichText::new(tr!("err_no_install")).weak());
            return;
        };
        ui.add_space(4.0);
        egui::Grid::new("gameinfo")
            .num_columns(2)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                ui.label(RichText::new(tr!("label_source")).weak());
                ui.label(&st.install.source);
                ui.end_row();
                ui.label(RichText::new(tr!("label_version")).weak());
                match &st.build {
                    Ok(b) => ui.label(b.version.as_deref().unwrap_or("?")),
                    Err(_) => ui.label(RichText::new(tr!("gui_unknown")).color(WARN_AMBER)),
                };
                ui.end_row();
                ui.label(RichText::new("uk-qol").weak());
                let (text, color) = match (&st.dll, &st.manifest) {
                    (DllState::Missing, _) => (tr!("dll_missing"), None),
                    (DllState::Ours, Ok(Some(m))) => {
                        (tr!("dll_ours", m.runtime_version), Some(OK_GREEN))
                    }
                    (DllState::Modified, _) => (tr!("dll_modified"), Some(WARN_AMBER)),
                    _ => (tr!("dll_foreign"), Some(WARN_AMBER)),
                };
                let mut rt = RichText::new(text);
                if let Some(c) = color {
                    rt = rt.color(c);
                }
                ui.label(rt);
                ui.end_row();
            });
        ui.add_space(6.0);

        let installed = matches!(st.dll, DllState::Ours | DllState::Modified);
        let dll_note = match &self.dll_override {
            Some(p) => tr!("gui_dll_file", p.display()),
            None => match install::bundled_dll() {
                Some(_) => tr!("gui_dll_bundled"),
                None => tr!("gui_dll_download"),
            },
        };
        ui.label(RichText::new(dll_note).small().weak());
        if matches!(st.dll, DllState::Foreign | DllState::Modified) {
            ui.checkbox(&mut self.force, tr!("gui_force"));
        }
        let running = self.game_running;
        ui.horizontal(|ui| {
            let label = if installed {
                tr!("gui_reinstall")
            } else {
                tr!("gui_install")
            };
            if ui
                .add_enabled(
                    !running,
                    egui::Button::new(RichText::new(label).strong()).fill(ACCENT_DARK),
                )
                .on_disabled_hover_text(tr!("err_game_running"))
                .clicked()
            {
                self.do_install();
            }
            if ui
                .add_enabled(
                    installed && !running,
                    egui::Button::new(tr!("gui_uninstall")),
                )
                .on_disabled_hover_text(tr!("gui_uninstall_disabled"))
                .clicked()
            {
                self.purge = false;
                self.confirm_uninstall = true;
            }
            if ui.small_button(tr!("gui_choose_dll")).clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .set_title(tr!("gui_choose_dll"))
                    .add_filter("winmm.dll", &["dll"])
                    .pick_file()
            {
                self.dll_override = Some(p);
            }
        });
    }

    fn launch_card(&mut self, ui: &mut egui::Ui) {
        section_title(ui, &tr!("label_launch"));
        ui.label(RichText::new(tr!("gui_launch_help")).weak());
        ui.horizontal_wrapped(|ui| {
            ui.code(launch::LAUNCH_OPTION);
            if ui.button(tr!("gui_copy")).clicked() {
                ui.ctx().copy_text(launch::LAUNCH_OPTION.to_string());
                self.set_msg(tr!("gui_copied"), false);
            }
        });
        let Some(st) = &self.status else { return };
        if st.launch.is_empty() {
            ui.label(RichText::new(tr!("launch_unknown")).color(WARN_AMBER));
            return;
        }
        let mut settable = Vec::new();
        for acc in &st.launch {
            let who = launch::short_account(&acc.account);
            let (text, color) = match &acc.state {
                LaunchState::Set => (tr!("launch_state_set"), OK_GREEN),
                LaunchState::Empty => {
                    settable.push(acc.config.clone());
                    (tr!("launch_state_empty"), WARN_AMBER)
                }
                LaunchState::Other(v) => (tr!("launch_state_other", v), WARN_AMBER),
                LaunchState::NoApp => (tr!("launch_state_noapp"), WARN_AMBER),
                LaunchState::Unreadable(e) => (tr!("launch_state_unreadable", e), BAD_RED),
            };
            ui.label(RichText::new(format!("{} {who}: {text}", tr!("gui_account"))).color(color));
        }
        if !settable.is_empty() {
            let ok = !self.steam_running && !self.game_running;
            if ui
                .add_enabled(ok, egui::Button::new(tr!("gui_set_launch")))
                .on_disabled_hover_text(tr!("launch_steam_running"))
                .clicked()
            {
                let mut errors = Vec::new();
                for cfg in settable {
                    if let Err(e) = launch::set(&cfg) {
                        errors.push(e);
                    }
                }
                if errors.is_empty() {
                    self.set_msg(tr!("gui_launch_set_done"), false);
                } else {
                    self.set_msg(errors.join("; "), true);
                }
                self.refresh();
            }
            if self.steam_running {
                ui.label(RichText::new(tr!("launch_steam_running")).small().weak());
            }
        }
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let Some(st) = &self.status else {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.heading(tr!("gui_welcome"));
                ui.label(tr!("gui_welcome_help"));
            });
            return;
        };
        let mut changed: Option<ConfigFile> = None;
        let config_err = st.config.as_ref().err().cloned();
        let locked = self.game_running;
        egui::ScrollArea::vertical().show(ui, |ui| {
            card(ui, |ui| {
                section_title(ui, &tr!("gui_compat_title"));
                match &st.build {
                    Ok(b) => ui.label(tr!(
                        "gui_compat_build",
                        b.version.as_deref().unwrap_or("?"),
                        &b.sha256_hex()[..16]
                    )),
                    Err(e) => ui.label(RichText::new(tr!("build_unknown", e)).color(WARN_AMBER)),
                };
                if let Some(l) = &st.log {
                    if let Some(line) = &l.loaded {
                        ui.label(
                            RichText::new(format!("{}: {line}", tr!("label_last_run")))
                                .small()
                                .weak(),
                        );
                    }
                    if l.other_build {
                        ui.label(
                            RichText::new(tr!("note_log_other_build"))
                                .small()
                                .color(WARN_AMBER),
                        );
                    }
                }
                if st.mods.is_empty() {
                    ui.label(RichText::new(tr!("no_mods")).weak());
                }
                for m in &st.mods {
                    let color = match &m.compat {
                        Compat::Works => OK_GREEN,
                        Compat::Broken(_) => BAD_RED,
                        Compat::Unknown(_) => WARN_AMBER,
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&m.info.name).strong());
                        ui.label(RichText::new(crate::cli::compat_text(&m.compat)).color(color));
                        if m.pattern_override {
                            ui.label(RichText::new(tr!("patterns_downloaded")).small().weak());
                        }
                    });
                    if let Some(l) = &m.last_log {
                        ui.label(
                            RichText::new(format!("{}: {l}", tr!("label_log")))
                                .small()
                                .weak(),
                        );
                    }
                }
            });
            ui.add_space(8.0);
            if let Some(e) = &config_err {
                ui.label(RichText::new(e).color(BAD_RED));
            }
            if locked {
                ui.label(RichText::new(tr!("gui_locked")).color(WARN_AMBER));
            }
            let Ok(cfg) = &st.config else { return };
            for m in &st.mods {
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        let mut on = m.enabled;
                        ui.add_enabled_ui(!locked, |ui| {
                            if toggle(ui, &mut on).changed() {
                                let mut c = clone_config(cfg);
                                if c.set_enabled(&m.info.id, on).is_ok() {
                                    changed = Some(c);
                                }
                            }
                        });
                        ui.label(RichText::new(&m.info.name).strong().size(16.0));
                        ui.label(
                            RichText::new(format!("{} {}", m.info.id, m.info.version))
                                .small()
                                .weak(),
                        );
                    });
                    if !m.info.description.is_empty() {
                        ui.label(&m.info.description);
                    }
                    if m.info.settings.is_empty() {
                        return;
                    }
                    ui.add_space(4.0);
                    ui.add_enabled_ui(!locked, |ui| {
                        egui::Grid::new(("settings", &m.info.id))
                            .num_columns(3)
                            .spacing([12.0, 6.0])
                            .show(ui, |ui| {
                                for s in &m.info.settings {
                                    let r = ui.label(&s.label);
                                    if !s.description.is_empty() {
                                        r.on_hover_text(&s.description);
                                    }
                                    if let Some(v) =
                                        setting_widget(ui, cfg, &m.info.id, s, &mut self.edits)
                                    {
                                        let mut c = clone_config(cfg);
                                        if c.set(&m.info.id, &s.key, v).is_ok() {
                                            changed = Some(c);
                                        }
                                    }
                                    if cfg.get(&m.info.id, &s.key).is_some()
                                        && ui
                                            .small_button(tr!("gui_reset"))
                                            .on_hover_text(tr!("gui_reset_help"))
                                            .clicked()
                                    {
                                        let mut c = clone_config(cfg);
                                        c.reset(&m.info.id, &s.key);
                                        self.edits.remove(&(m.info.id.clone(), s.key.clone()));
                                        changed = Some(c);
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                });
                ui.add_space(8.0);
            }
        });
        if let Some(c) = changed {
            self.save_config(&c);
        }
    }

    fn uninstall_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_uninstall {
            return;
        }
        let modal = egui::Modal::new(egui::Id::unique("uninstall")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading(tr!("gui_uninstall_title"));
            ui.add_space(6.0);
            ui.label(RichText::new(tr!("weapon_warning")).color(WARN_AMBER));
            ui.add_space(6.0);
            ui.label(tr!("gui_uninstall_keeps"));
            ui.checkbox(&mut self.purge, tr!("gui_purge"));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new(tr!("gui_uninstall")).color(BAD_RED))
                    .clicked()
                {
                    self.confirm_uninstall = false;
                    self.do_uninstall();
                }
                if ui.button(tr!("gui_cancel")).clicked() {
                    self.confirm_uninstall = false;
                }
            });
        });
        if modal.should_close() {
            self.confirm_uninstall = false;
        }
    }
}

fn clone_config(c: &ConfigFile) -> ConfigFile {
    ConfigFile::parse(&c.path, &c.text()).expect("re-parse of a parsed config")
}

/// Draws the widget for one setting; returns a new value when it changed.
fn setting_widget(
    ui: &mut egui::Ui,
    cfg: &ConfigFile,
    id: &str,
    s: &SettingInfo,
    edits: &mut HashMap<(String, String), String>,
) -> Option<toml_edit::Value> {
    let user = cfg.get(id, &s.key);
    let default = s.default.as_ref();
    match &s.kind {
        SettingKind::Bool => {
            let mut v = user
                .and_then(toml_edit::Value::as_bool)
                .or_else(|| default.and_then(toml::Value::as_bool))
                .unwrap_or(false);
            ui.checkbox(&mut v, "").changed().then(|| v.into())
        }
        SettingKind::Int => {
            let mut v = user
                .and_then(toml_edit::Value::as_integer)
                .or_else(|| default.and_then(toml::Value::as_integer))
                .unwrap_or(0);
            let mut dv = egui::DragValue::new(&mut v);
            if let (Some(lo), Some(hi)) = (s.min, s.max) {
                dv = dv.range(lo as i64..=hi as i64);
            }
            ui.add(dv).changed().then(|| v.into())
        }
        SettingKind::Float => {
            let mut v = user
                .and_then(toml_edit::Value::as_float)
                .or_else(|| default.and_then(toml::Value::as_float))
                .unwrap_or(0.0);
            let mut dv = egui::DragValue::new(&mut v).speed(0.05);
            if let (Some(lo), Some(hi)) = (s.min, s.max) {
                dv = dv.range(lo..=hi);
            }
            ui.add(dv).changed().then(|| v.into())
        }
        SettingKind::Choice(opts) => {
            let cur = user
                .and_then(toml_edit::Value::as_str)
                .map(str::to_string)
                .or_else(|| default.and_then(toml::Value::as_str).map(str::to_string))
                .unwrap_or_default();
            let mut picked = None;
            egui::ComboBox::from_id_salt(("choice", id, &s.key))
                .selected_text(&cur)
                .show_ui(ui, |ui| {
                    for o in opts {
                        if ui.selectable_label(*o == cur, o).clicked() && *o != cur {
                            picked = Some(o.clone());
                        }
                    }
                });
            picked.map(Into::into)
        }
        SettingKind::Text | SettingKind::Key => {
            let key = (id.to_string(), s.key.clone());
            let cur = user
                .map(config::show_value)
                .or_else(|| default.map(config::show_toml))
                .unwrap_or_default();
            let buf = edits.entry(key.clone()).or_insert_with(|| cur.clone());
            let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(180.0));
            let commit = resp.lost_focus() && *buf != cur;
            if commit {
                let v = buf.clone();
                edits.remove(&key);
                Some(v.into())
            } else {
                None
            }
        }
    }
}

/// Checks the latest release against the manager, the installed loader and
/// the installed pattern files. Only reads.
fn plan_update(game: Option<&GameInstall>) -> Result<Option<UpdatePlan>, String> {
    let Some(rel) = update::latest()? else {
        return Ok(None);
    };
    let manifest = game.and_then(|g| install::InstallManifest::read(&g.dir).ok().flatten());
    let runtime_newer = manifest
        .as_ref()
        .is_some_and(|m| update::is_newer(&rel.version, &m.runtime_version));
    let patterns = match (game, &manifest) {
        (Some(g), Some(_)) => update::changed_patterns(&g.dir, &update::fetch_patterns(&rel)?),
        _ => Vec::new(),
    };
    Ok(Some(UpdatePlan {
        self_newer: update::is_newer(&rel.version, &update::current_version())
            && update::manager_asset().is_some(),
        runtime_newer,
        patterns,
        rel,
    }))
}

fn apply_update(
    game: Option<&GameInstall>,
    plan: &UpdatePlan,
) -> Result<(String, Option<PathBuf>), String> {
    let mut done = Vec::new();
    if let Some(g) = game
        && (plan.runtime_newer || !plan.patterns.is_empty())
    {
        if process::game_running() {
            return Err(tr!("err_game_running"));
        }
        if plan.runtime_newer {
            let dll = update::fetch_runtime(&plan.rel)?;
            install::install(
                &g.dir,
                &InstallOpts {
                    dll: &dll,
                    runtime_version: plan.rel.version.clone(),
                    force: false,
                },
            )?;
            done.push(tr!("done_runtime_updated", plan.rel.version));
        }
        for (id, bytes) in &plan.patterns {
            install::install_pattern(&g.dir, id, bytes)?;
            done.push(tr!("done_patterns_updated", id));
        }
    }
    let mut restart = None;
    if plan.self_newer {
        restart = Some(update::update_self(&plan.rel)?);
        done.push(tr!("done_self_updated", plan.rel.version));
    }
    Ok((done.join(" "), restart))
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.check_processes();
        ui.ctx().request_repaint_after(Duration::from_secs(2));
        if let Some(m) = &self.msg
            && !m.error
            && m.at.elapsed() > Duration::from_secs(10)
        {
            self.msg = None;
        }

        egui::Panel::top("top")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(Margin::symmetric(14, 10)))
            .show(ui, |ui| self.top_bar(ui));
        if self.update_banner_visible() {
            egui::Panel::top("update")
                .frame(
                    egui::Frame::side_top_panel(ui.style()).inner_margin(Margin::symmetric(14, 8)),
                )
                .show(ui, |ui| self.update_banner(ui));
        }
        if self.game_running {
            egui::Panel::top("running")
                .frame(
                    egui::Frame::side_top_panel(ui.style()).inner_margin(Margin::symmetric(14, 8)),
                )
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(tr!("gui_game_running"))
                            .strong()
                            .color(BAD_RED),
                    );
                });
        }
        if self.msg.is_some() {
            egui::Panel::bottom("msg")
                .frame(
                    egui::Frame::side_top_panel(ui.style()).inner_margin(Margin::symmetric(14, 6)),
                )
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if let Some(m) = &self.msg {
                            let color = if m.error { BAD_RED } else { OK_GREEN };
                            ui.label(RichText::new(&m.text).color(color));
                        }
                        if ui.small_button("✕").clicked() {
                            self.msg = None;
                        }
                    });
                });
        }
        egui::Panel::left("side")
            .resizable(true)
            .default_size(360.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(Margin::same(12)))
            .show(ui, |ui| self.side_panel(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(Margin::same(16)))
            .show(ui, |ui| self.central(ui));
        self.uninstall_dialog(ui.ctx());
    }
}
