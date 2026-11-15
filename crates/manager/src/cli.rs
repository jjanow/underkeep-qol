//! The `uk-qol` command line.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::{self, ConfigFile};
use crate::install::{self, DllState, InstallManifest, InstallOpts, UninstallOpts};
use crate::launch::{self, LaunchState};
use crate::mods::{self, Compat};
use crate::paths::{self, GameInstall};
use crate::status;
use crate::{process, report, tr, update};

/// Commands with the options each accepts.
pub const COMMANDS: &[(&str, &[&str])] = &[
    ("detect", &[]),
    (
        "install",
        &["--game", "--dll", "--force", "--set-launch-option"],
    ),
    ("status", &["--game"]),
    ("enable", &["--game"]),
    ("disable", &["--game"]),
    ("config", &["--game", "--reset"]),
    ("uninstall", &["--game", "--yes", "--force", "--purge"]),
    ("report", &["--game", "--output", "--stdout"]),
    ("update", &["--game", "--yes", "--check"]),
    ("reset-safe-mode", &["--game"]),
    ("gui", &["--game"]),
];

/// Options that take a value.
const VALUE_OPTS: &[&str] = &["--game", "--dll", "--output"];

pub fn usage() -> String {
    tr!("usage", env!("CARGO_PKG_VERSION"), launch::LAUNCH_OPTION)
}

#[derive(Debug, Default)]
pub struct Args {
    pub cmd: String,
    pub pos: Vec<String>,
    pub game: Option<PathBuf>,
    pub dll: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub flags: Vec<String>,
}

impl Args {
    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }
}

pub fn parse(argv: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        if let Some(opt) = arg.strip_prefix("--").map(|_| arg.as_str()) {
            let (name, inline) = match opt.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (opt, None),
            };
            if VALUE_OPTS.contains(&name) {
                let v = match inline {
                    Some(v) => v,
                    None => it
                        .next()
                        .cloned()
                        .ok_or_else(|| tr!("err_opt_value", name))?,
                };
                let slot = match name {
                    "--game" => &mut a.game,
                    "--dll" => &mut a.dll,
                    _ => &mut a.output,
                };
                *slot = Some(PathBuf::from(v));
            } else if inline.is_some() {
                return Err(tr!("err_opt_unknown", arg));
            } else {
                a.flags.push(name.to_string());
            }
        } else if arg == "-y" {
            a.flags.push("--yes".into());
        } else if arg == "-o" {
            a.output = Some(PathBuf::from(
                it.next().ok_or_else(|| tr!("err_opt_value", "-o"))?,
            ));
        } else if a.cmd.is_empty() {
            a.cmd = arg.clone();
        } else {
            a.pos.push(arg.clone());
        }
    }
    if let Some((_, allowed)) = COMMANDS.iter().find(|(c, _)| *c == a.cmd) {
        let used = a
            .flags
            .iter()
            .map(String::as_str)
            .chain(a.game.as_ref().map(|_| "--game"))
            .chain(a.dll.as_ref().map(|_| "--dll"))
            .chain(a.output.as_ref().map(|_| "--output"));
        for f in used {
            if !allowed.contains(&f) {
                return Err(tr!("err_opt_not_for", f, a.cmd));
            }
        }
    }
    Ok(a)
}

fn err(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("uk-qol: {msg}");
    ExitCode::FAILURE
}

/// Asks a yes/no question on the terminal. No terminal means no.
fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The game folder to work on: `--game`, or the first install found.
pub fn pick_game(a: &Args) -> Result<GameInstall, String> {
    if let Some(p) = &a.game {
        if !paths::is_game_dir(p) {
            return Err(tr!("err_not_game_dir", p.display()));
        }
        return Ok(paths::describe(p));
    }
    let mut found = paths::find_installs();
    match found.len() {
        0 => Err(tr!("err_no_install")),
        1 => Ok(found.remove(0)),
        _ => {
            eprintln!("{}", tr!("note_several_installs", found[0].dir.display()));
            Ok(found.remove(0))
        }
    }
}

fn refuse_if_running() -> Result<(), String> {
    if process::game_running() {
        Err(tr!("err_game_running"))
    } else {
        Ok(())
    }
}

pub fn run(a: Args) -> ExitCode {
    let res = match a.cmd.as_str() {
        "detect" => detect(),
        "install" => cmd_install(&a),
        "status" => cmd_status(&a),
        "enable" | "disable" => cmd_enable(&a, a.cmd == "enable"),
        "config" => cmd_config(&a),
        "uninstall" => cmd_uninstall(&a),
        "report" => cmd_report(&a),
        "update" => cmd_update(&a),
        "reset-safe-mode" => cmd_reset_safe_mode(&a),
        other => {
            eprint!("{}\n\n{}", tr!("err_unknown_command", other), usage());
            return ExitCode::from(2);
        }
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => err(e),
    }
}

fn build_line(dir: &std::path::Path) -> String {
    match uk_qol_runtime::build::detect_from_file(&dir.join(paths::GAME_EXE)) {
        Ok(b) => tr!(
            "build_line",
            b.version.as_deref().unwrap_or("?"),
            &b.sha256_hex()[..16]
        ),
        Err(e) => tr!("build_unknown", e),
    }
}

fn detect() -> Result<(), String> {
    let found = paths::find_installs();
    if found.is_empty() {
        return Err(tr!("err_no_install"));
    }
    for g in &found {
        println!("{}", g.dir.display());
        println!("  {}: {}", tr!("label_source"), g.source);
        println!("  {}: {}", tr!("label_build"), build_line(&g.dir));
        let m = InstallManifest::read(&g.dir).ok().flatten();
        let state = match install::dll_state(&g.dir, m.as_ref()) {
            DllState::Missing => tr!("dll_missing"),
            DllState::Ours => tr!("dll_ours", m.map(|m| m.runtime_version).unwrap_or_default()),
            DllState::Modified => tr!("dll_modified"),
            DllState::Foreign => tr!("dll_foreign"),
        };
        println!("  uk-qol: {state}");
    }
    Ok(())
}

/// Where the DLL to install comes from, and its version.
fn dll_source(a: &Args) -> Result<(Vec<u8>, String), String> {
    if let Some(p) = &a.dll {
        let bytes = std::fs::read(p).map_err(|e| tr!("err_read", p.display(), e))?;
        return Ok((bytes, "unknown".into()));
    }
    if let Some(p) = install::bundled_dll() {
        println!("{}", tr!("note_using_dll", p.display()));
        let bytes = std::fs::read(&p).map_err(|e| tr!("err_read", p.display(), e))?;
        return Ok((bytes, update::current_version()));
    }
    println!("{}", tr!("note_downloading_dll"));
    let rel = update::latest()?.ok_or_else(|| tr!("err_no_dll_no_release"))?;
    let bytes = update::fetch_runtime(&rel)?;
    Ok((bytes, rel.version))
}

fn cmd_install(a: &Args) -> Result<(), String> {
    let g = pick_game(a)?;
    refuse_if_running()?;
    let (dll, version) = dll_source(a)?;
    let out = install::install(
        &g.dir,
        &InstallOpts {
            dll: &dll,
            runtime_version: version,
            force: a.has("--force"),
        },
    )?;
    if out.backed_up {
        println!("{}", tr!("note_backed_up", install::BACKUP_DLL));
    }
    println!(
        "{}",
        if out.updated {
            tr!("done_updated", g.dir.display())
        } else {
            tr!("done_installed", g.dir.display())
        }
    );
    if launch::needed() {
        launch_option_step(&g, a.has("--set-launch-option"))?;
    }
    Ok(())
}

/// Prints (and with `set`, writes) the Linux/Wine launch option.
fn launch_option_step(g: &GameInstall, set: bool) -> Result<(), String> {
    let accounts = g
        .steam_root
        .as_deref()
        .map(launch::check)
        .unwrap_or_default();
    // Accounts that never ran the game (no entry) don't need it.
    if accounts.iter().any(|a| a.state == LaunchState::Set)
        && accounts
            .iter()
            .all(|a| matches!(a.state, LaunchState::Set | LaunchState::NoApp))
    {
        println!("{}", tr!("launch_already_set"));
        return Ok(());
    }
    if set {
        if accounts.is_empty() {
            println!("{}", tr!("launch_no_accounts"));
        } else if process::steam_running() {
            println!("{}", tr!("launch_steam_running"));
        } else {
            for acc in &accounts {
                let who = launch::short_account(&acc.account);
                match &acc.state {
                    LaunchState::Set => {}
                    LaunchState::Empty => match launch::set(&acc.config) {
                        Ok(Some(backup)) => {
                            println!("{}", tr!("launch_set_done", who, backup.display()))
                        }
                        Ok(None) => {}
                        Err(e) => println!("{}", tr!("launch_set_failed", who, e)),
                    },
                    LaunchState::Other(v) => println!("{}", tr!("launch_other", who, v)),
                    LaunchState::NoApp => println!("{}", tr!("launch_no_app", who)),
                    LaunchState::Unreadable(e) => println!("{}", tr!("launch_set_failed", who, e)),
                }
            }
            let after = g
                .steam_root
                .as_deref()
                .map(launch::check)
                .unwrap_or_default();
            if after.iter().any(|a| a.state == LaunchState::Set) {
                return Ok(());
            }
        }
    }
    println!("{}", tr!("launch_paste", launch::LAUNCH_OPTION));
    if !set {
        println!("{}", tr!("launch_hint_set"));
    }
    Ok(())
}

fn launch_state_text(s: &LaunchState) -> String {
    match s {
        LaunchState::Set => tr!("launch_state_set"),
        LaunchState::Empty => tr!("launch_state_empty"),
        LaunchState::Other(v) => tr!("launch_state_other", v),
        LaunchState::NoApp => tr!("launch_state_noapp"),
        LaunchState::Unreadable(e) => tr!("launch_state_unreadable", e),
    }
}

pub fn compat_text(c: &Compat) -> String {
    match c {
        Compat::Works => tr!("compat_works"),
        Compat::Broken(f) => tr!(
            "compat_broken",
            f.iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Compat::Unknown(e) => tr!("compat_unknown", e),
    }
}

fn cmd_status(a: &Args) -> Result<(), String> {
    let g = pick_game(a)?;
    let st = status::gather(&g);
    println!("{}: {} [{}]", tr!("label_game"), g.dir.display(), g.source);
    println!("{}: {}", tr!("label_build"), build_line(&g.dir));
    let installed = match (&st.manifest, &st.dll) {
        (_, DllState::Missing) => tr!("dll_missing"),
        (Ok(Some(m)), DllState::Ours) => tr!("dll_ours", m.runtime_version),
        (_, DllState::Modified) => tr!("dll_modified"),
        (_, _) => tr!("dll_foreign"),
    };
    println!("uk-qol: {installed}");
    if let Err(e) = &st.manifest {
        println!("  {e}");
    }
    if launch::needed() {
        if st.launch.is_empty() {
            println!("{}: {}", tr!("label_launch"), tr!("launch_unknown"));
        }
        for acc in &st.launch {
            println!(
                "{} ({}): {}",
                tr!("label_launch"),
                launch::short_account(&acc.account),
                launch_state_text(&acc.state)
            );
        }
        if !st.launch.iter().any(|a| a.state == LaunchState::Set) {
            println!("  {}", tr!("launch_paste", launch::LAUNCH_OPTION));
        }
    }
    if process::game_running() {
        println!("{}", tr!("note_game_running"));
    }
    if st.safe_mode_next {
        println!("{}", tr!("note_safe_mode", st.abnormal_exits));
    }
    if let Err(e) = &st.config {
        println!("{e}");
    }
    if let Some(l) = &st.log {
        if let Some(line) = &l.loaded {
            println!("{}: {line}", tr!("label_last_run"));
        }
        if l.other_build {
            println!("  {}", tr!("note_log_other_build"));
        }
    }
    println!();
    if st.mods.is_empty() {
        println!("{}", tr!("no_mods"));
    }
    for m in &st.mods {
        println!(
            "{:<20} {:<4} {}{}",
            m.info.id,
            if m.enabled { tr!("on") } else { tr!("off") },
            compat_text(&m.compat),
            if m.pattern_override {
                tr!("patterns_downloaded")
            } else {
                String::new()
            }
        );
        if m.untested {
            println!("{:<20} {}", "", tr!("note_untested"));
        }
        if let Some(e) = &m.info.manifest_error {
            println!("{:<20} {}", "", tr!("note_manifest_error", e));
        }
        if let Some(l) = &m.last_log {
            println!("{:<20} {}: {l}", "", tr!("label_log"));
        }
    }
    Ok(())
}

fn find_mod(id: &str) -> Result<mods::ModInfo, String> {
    let all = mods::builtin();
    if let Some(m) = mods::find(&all, id) {
        return Ok(m.clone());
    }
    if all.is_empty() {
        return Err(tr!("err_no_mods_built_in"));
    }
    Err(tr!(
        "err_unknown_mod",
        id,
        all.iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn cmd_enable(a: &Args, on: bool) -> Result<(), String> {
    let [id] = a.pos.as_slice() else {
        return Err(tr!("err_usage_enable", a.cmd));
    };
    let m = find_mod(id)?;
    let g = pick_game(a)?;
    refuse_if_running()?;
    let mut c = ConfigFile::load(&config::path(&g.dir))?;
    c.set_enabled(&m.id, on)?;
    c.save()?;
    println!(
        "{}",
        if on {
            tr!("done_enabled", m.id)
        } else {
            tr!("done_disabled", m.id)
        }
    );
    Ok(())
}

fn cmd_config(a: &Args) -> Result<(), String> {
    let (id, key, value) = match a.pos.as_slice() {
        [id] => (id, None, None),
        [id, k] => (id, Some(k), None),
        [id, k, v] => (id, Some(k), Some(v)),
        _ => return Err(tr!("err_usage_config")),
    };
    let m = find_mod(id)?;
    let g = pick_game(a)?;
    let mut c = ConfigFile::load(&config::path(&g.dir))?;
    let current = |c: &ConfigFile, s: &mods::SettingInfo| {
        c.get(&m.id, &s.key)
            .map(config::show_value)
            .or_else(|| {
                s.default
                    .as_ref()
                    .map(|d| format!("{} ({})", config::show_toml(d), tr!("default")))
            })
            .unwrap_or_else(|| "-".into())
    };
    let Some(key) = key else {
        println!(
            "{} ({}): {}",
            m.name,
            m.id,
            if c.is_enabled(&m) {
                tr!("on")
            } else {
                tr!("off")
            }
        );
        if m.settings.is_empty() {
            println!("{}", tr!("no_settings"));
        }
        for s in &m.settings {
            println!(
                "  {:<20} = {:<16} [{}] {}",
                s.key,
                current(&c, s),
                config::kind_name(&s.kind),
                s.label
            );
        }
        return Ok(());
    };
    let Some(s) = m.setting(key) else {
        return Err(tr!(
            "err_unknown_setting",
            key,
            m.id,
            m.settings
                .iter()
                .map(|s| s.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    if a.has("--reset") {
        refuse_if_running()?;
        c.reset(&m.id, key);
        c.save()?;
        println!("{}", tr!("done_reset", m.id, key));
        return Ok(());
    }
    let Some(value) = value else {
        println!("{} = {}", s.key, current(&c, s));
        println!("  {} [{}]", s.label, config::kind_name(&s.kind));
        if !s.description.is_empty() {
            println!("  {}", s.description);
        }
        return Ok(());
    };
    let v = config::coerce(s, value)?;
    refuse_if_running()?;
    c.set(&m.id, key, v.clone())?;
    c.save()?;
    println!("{}", tr!("done_set", m.id, key, config::show_value(&v)));
    Ok(())
}

fn cmd_uninstall(a: &Args) -> Result<(), String> {
    let g = pick_game(a)?;
    refuse_if_running()?;
    let manifest = InstallManifest::read(&g.dir)?;
    if manifest.is_none() && install::dll_state(&g.dir, None) == DllState::Missing {
        return Err(tr!("err_not_installed"));
    }
    if !a.has("--yes") && !confirm(&tr!("confirm_uninstall", g.dir.display())) {
        return Err(tr!("err_not_confirmed"));
    }
    let out = install::uninstall(
        &g.dir,
        &UninstallOpts {
            force: a.has("--force"),
            purge: a.has("--purge"),
        },
    )?;
    for f in &out.removed {
        println!("  {}", tr!("removed", f));
    }
    for f in &out.kept_modified {
        println!("  {}", tr!("kept_modified", f));
    }
    if out.restored_backup {
        println!("{}", tr!("restored_backup"));
    }
    if out.config_kept {
        println!("{}", tr!("kept_config"));
    }
    if out.sidecars_kept {
        println!("{}", tr!("kept_sidecars", install::SAVES_DIR));
    }
    println!("{}", tr!("done_uninstalled"));
    if launch::needed() {
        println!("{}", tr!("launch_after_uninstall"));
    }
    Ok(())
}

fn cmd_reset_safe_mode(a: &Args) -> Result<(), String> {
    let g = pick_game(a)?;
    refuse_if_running()?;
    uk_qol_runtime::safe::reset(&g.dir.join(install::DATA_DIR));
    println!("{}", tr!("done_safe_mode_reset"));
    Ok(())
}

fn cmd_report(a: &Args) -> Result<(), String> {
    let g = pick_game(a)?;
    let text = report::build_report(&status::gather(&g));
    if a.has("--stdout") {
        print!("{text}");
        return Ok(());
    }
    let path = a.output.clone().unwrap_or_else(|| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        PathBuf::from(format!("uk-qol-report-{secs}.txt"))
    });
    std::fs::write(&path, text).map_err(|e| tr!("err_write", path.display(), e))?;
    println!("{}", tr!("done_report", path.display()));
    Ok(())
}

fn cmd_update(a: &Args) -> Result<(), String> {
    println!("{}", tr!("update_checking"));
    let Some(rel) = update::latest()? else {
        println!("{}", tr!("update_none_published"));
        return Ok(());
    };
    let current = update::current_version();
    let self_newer = update::is_newer(&rel.version, &current);
    // The game part is optional: no install found just skips it.
    let game = if a.game.is_some() {
        Some(pick_game(a)?)
    } else {
        pick_game(a).ok()
    };
    let manifest = game
        .as_ref()
        .and_then(|g| InstallManifest::read(&g.dir).ok().flatten());
    let runtime_newer = manifest
        .as_ref()
        .is_some_and(|m| update::is_newer(&rel.version, &m.runtime_version));
    let patterns = match (&game, &manifest) {
        (Some(g), Some(m)) => update::pattern_updates(&g.dir, m, &rel)?,
        _ => Vec::new(),
    };

    println!("{}", tr!("update_latest", rel.version, rel.page_url));
    if self_newer {
        println!("  {}", tr!("update_offer_self", current, rel.version));
    }
    if runtime_newer {
        println!(
            "  {}",
            tr!(
                "update_offer_runtime",
                manifest
                    .as_ref()
                    .map(|m| m.runtime_version.as_str())
                    .unwrap_or("?"),
                rel.version
            )
        );
    }
    for (id, _) in &patterns {
        println!("  {}", tr!("update_offer_patterns", id));
    }
    if !self_newer && !runtime_newer && patterns.is_empty() {
        println!("{}", tr!("update_up_to_date"));
        return Ok(());
    }
    if a.has("--check") {
        return Ok(());
    }
    if !a.has("--yes") && !confirm(&tr!("confirm_update")) {
        return Err(tr!("err_not_confirmed"));
    }
    if let (Some(g), true) = (&game, runtime_newer || !patterns.is_empty()) {
        refuse_if_running()?;
        if runtime_newer {
            let dll = update::fetch_runtime(&rel)?;
            install::install(
                &g.dir,
                &InstallOpts {
                    dll: &dll,
                    runtime_version: rel.version.clone(),
                    force: false,
                },
            )?;
            println!("{}", tr!("done_runtime_updated", rel.version));
        }
        for (id, bytes) in &patterns {
            install::install_pattern(&g.dir, id, bytes)?;
            println!("{}", tr!("done_patterns_updated", id));
        }
    }
    if self_newer {
        update::update_self(&rel)?;
        println!("{}", tr!("done_self_updated", rel.version));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Result<Args, String> {
        parse(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_options() {
        let a = args(&["install", "--game", "/g", "--dll=/d.dll", "--force"]).unwrap();
        assert_eq!(a.cmd, "install");
        assert_eq!(a.game, Some(PathBuf::from("/g")));
        assert_eq!(a.dll, Some(PathBuf::from("/d.dll")));
        assert!(a.has("--force"));
        let a = args(&["config", "m", "k", "v", "--game", "/g"]).unwrap();
        assert_eq!(a.pos, vec!["m", "k", "v"]);
        assert!(args(&["status", "--purge"]).is_err());
        assert!(args(&["install", "--game"]).is_err());
        assert!(args(&["uninstall", "-y"]).unwrap().has("--yes"));
    }

    #[test]
    fn usage_mentions_every_command() {
        let u = usage();
        for (c, _) in COMMANDS {
            assert!(u.contains(c), "{c}");
        }
        assert!(u.contains(launch::LAUNCH_OPTION));
    }
}
