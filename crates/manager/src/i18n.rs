//! Translatable text. Every message the player sees goes through
//! [`tr!`](crate::tr) with a key from the catalog below; `{}` marks are
//! filled in order. English is the only language so far: to add one, copy
//! `EN`, translate the right-hand sides (keeping every `{}`), and list it in
//! `LANGS`. Missing keys fall back to English.
//!
//! The language comes from `UK_QOL_LANG`, else `LC_ALL` / `LC_MESSAGES` /
//! `LANG` (`de_DE.UTF-8` → `de`). Bug reports (`uk-qol report`) stay in
//! English on purpose: they're read by the maintainers.

use std::fmt::Display;
use std::sync::OnceLock;

pub struct Lang {
    pub code: &'static str,
    pub strings: &'static [(&'static str, &'static str)],
}

pub static LANGS: &[Lang] = &[Lang {
    code: "en",
    strings: EN,
}];

fn lang_code() -> String {
    ["UK_QOL_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .map(|s| {
            s.split(['_', '.', '-'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
        })
        .unwrap_or_default()
}

fn current() -> &'static Lang {
    static CUR: OnceLock<&'static Lang> = OnceLock::new();
    CUR.get_or_init(|| {
        let code = lang_code();
        LANGS.iter().find(|l| l.code == code).unwrap_or(&LANGS[0])
    })
}

fn lookup(lang: &Lang, key: &str) -> Option<&'static str> {
    lang.strings
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
}

/// The text for `key`, or the key itself if no language has it.
pub fn t(key: &'static str) -> &'static str {
    lookup(current(), key)
        .or_else(|| lookup(&LANGS[0], key))
        .unwrap_or(key)
}

/// Fills the `{}` marks of `key`'s text with `args`, in order.
pub fn fmt(key: &'static str, args: &[&dyn Display]) -> String {
    let mut out = String::new();
    let mut parts = t(key).split("{}");
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    for (i, part) in parts.enumerate() {
        match args.get(i) {
            Some(a) => out.push_str(&a.to_string()),
            None => out.push_str("{}"),
        }
        out.push_str(part);
    }
    out
}

#[macro_export]
macro_rules! tr {
    ($key:expr) => {
        $crate::i18n::t($key).to_string()
    };
    ($key:expr, $($arg:expr),+ $(,)?) => {
        $crate::i18n::fmt($key, &[$(&$arg as &dyn ::std::fmt::Display),+])
    };
}

static EN: &[(&str, &str)] = &[
    // --- command line ---
    (
        "usage",
        "uk-qol {} - installs, configures and updates Underkeep QoL

Usage: uk-qol [command] [options]

Commands:
  detect                      find Underkeep installs and show their game build
  install [--game PATH] [--dll PATH] [--force] [--set-launch-option]
                              install the mod loader (winmm.dll) into the game
  status [--game PATH]        game build, install, launch option, mods and
                              whether each works on this build
  enable <mod>                turn a mod on
  disable <mod>               turn a mod off
  config <mod> [<key> [<value>]] [--reset]
                              list a mod's settings, show one, or change one
  uninstall [--yes] [--force] [--purge]
                              remove what install added (asks first)
  report [--output FILE | --stdout]
                              write a text report to attach to a bug report
  update [--yes] [--check]    update uk-qol, the loader and mod patterns from
                              GitHub releases (asks first)
  reset-safe-mode             start the game with mods again after safe mode
                              turned them off (two crashes in a row)
  gui                         open the app

Options:
  --game PATH          the game folder (the one with underkeep.exe); default:
                       the first install found
  --dll PATH           install this winmm.dll; default: the one next to uk-qol,
                       else the latest release's
  --force              install over a winmm.dll uk-qol didn't install (it's
                       kept and put back on uninstall); uninstall files that
                       changed since install
  --set-launch-option  Linux/Wine: also set the Steam launch option, if Steam
                       isn't running and the game has no other launch options
  --purge              uninstall: also remove settings and logs; save
                       sidecars (Steam Cloud, uk_qol/saves) are always kept
  --reset              config: go back to the mod's default for <key>
  -y, --yes            don't ask for confirmation
  --check              update: only show what's available
  -h, --help           this help
  -V, --version        the version

Started without a command, uk-qol opens the app, except from a terminal,
where it shows this help. Files are never changed while the game runs.

On Linux / Steam Deck (Proton) the game needs this launch option in Steam
(Properties > Launch Options):
  {}
",
    ),
    ("err_opt_value", "{} needs a value"),
    ("err_opt_unknown", "unknown option {}"),
    ("err_opt_not_for", "{} doesn't apply to `{}`"),
    ("err_unknown_command", "uk-qol: unknown command `{}`"),
    ("err_usage_enable", "usage: uk-qol {} <mod>"),
    (
        "err_usage_config",
        "usage: uk-qol config <mod> [<key> [<value>]]",
    ),
    (
        "err_not_confirmed",
        "cancelled; nothing was changed (use --yes to skip the question)",
    ),
    (
        "note_several_installs",
        "Several Underkeep installs found; using {}. Pick another with --game.",
    ),
    ("label_source", "source"),
    ("label_build", "build"),
    ("label_game", "Game"),
    ("label_version", "version"),
    ("label_launch", "Launch option"),
    ("label_last_run", "last run"),
    ("label_log", "log"),
    ("build_line", "version {} (exe sha256 {}…)"),
    ("build_unknown", "unknown ({})"),
    ("dll_missing", "not installed"),
    ("dll_ours", "installed (loader {})"),
    ("dll_modified", "installed, but winmm.dll changed since"),
    (
        "dll_foreign",
        "a winmm.dll is there that uk-qol didn't install",
    ),
    ("note_using_dll", "Using {}"),
    (
        "note_downloading_dll",
        "No winmm.dll next to uk-qol; downloading it from the latest release…",
    ),
    (
        "err_no_dll_no_release",
        "no winmm.dll next to uk-qol and no release to download it from; pass --dll PATH",
    ),
    (
        "note_backed_up",
        "The winmm.dll that was there is kept in {} and put back on uninstall.",
    ),
    ("done_installed", "Installed into {}."),
    ("done_updated", "Updated the loader in {}."),
    ("launch_already_set", "The Steam launch option is set."),
    (
        "launch_no_accounts",
        "No Steam account config found for this install.",
    ),
    (
        "launch_steam_running",
        "Steam is running, so its settings can't be changed now (Steam would overwrite them). Close Steam completely and try again, or paste the launch option yourself.",
    ),
    (
        "launch_set_done",
        "Launch option set for Steam account {} (backup: {}).",
    ),
    (
        "launch_set_failed",
        "Couldn't set the launch option for account {}: {}",
    ),
    (
        "launch_other",
        "Account {} has launch options that already set winmm ({}); change them by hand to load the mods.",
    ),
    (
        "launch_no_app",
        "Account {} has no Steam entry for Underkeep yet; start the game once from Steam, then try again.",
    ),
    (
        "launch_paste",
        "In Steam: Underkeep > Properties > Launch Options, paste:\n  {}",
    ),
    (
        "launch_hint_set",
        "(or close Steam and run `uk-qol install --set-launch-option`)",
    ),
    ("launch_state_set", "set"),
    ("launch_state_empty", "not set"),
    ("launch_state_other", "not set (other options: {})"),
    (
        "launch_state_other_merge",
        "not set (other options: {}; setting it keeps them)",
    ),
    ("launch_state_noapp", "no Steam entry for the game yet"),
    ("launch_state_unreadable", "can't read Steam's config ({})"),
    ("launch_unknown", "can't check (Steam install not found)"),
    (
        "launch_after_uninstall",
        "The launch option can stay in Steam (it does nothing without the loader), or remove it under Properties > Launch Options.",
    ),
    ("compat_works", "works on this build"),
    (
        "compat_broken",
        "doesn't work on this build (patterns not found: {})",
    ),
    ("compat_unknown", "not checked ({})"),
    ("label_mods", "Mods"),
    ("no_mods", "No mods are built into this version yet."),
    (
        "note_safe_mode",
        "The game exited abnormally {} times in a row, so it starts in safe mode (all mods off). `uk-qol reset-safe-mode` or the in-game menu turns the mods back on.",
    ),
    (
        "done_safe_mode_reset",
        "Safe mode is off; mods load again on the next start.",
    ),
    ("note_untested", "(not tested on this game build yet)"),
    ("note_manifest_error", "manifest problem: {}"),
    ("gui_reset_safe_mode", "Leave safe mode"),
    ("on", "on"),
    ("off", "off"),
    ("patterns_downloaded", " (downloaded patterns)"),
    (
        "note_game_running",
        "Underkeep is running: nothing can be changed until it's closed.",
    ),
    (
        "note_log_other_build",
        "The log is from a different game build (the game was updated since).",
    ),
    (
        "err_no_mods_built_in",
        "no mods are built into this version yet",
    ),
    ("err_unknown_mod", "unknown mod `{}` (mods: {})"),
    (
        "err_unknown_setting",
        "`{}` isn't a setting of {} (settings: {})",
    ),
    ("no_settings", "  (no settings)"),
    ("default", "default"),
    (
        "done_enabled",
        "{} is on; it takes effect the next time the game starts.",
    ),
    (
        "done_disabled",
        "{} is off; it takes effect the next time the game starts.",
    ),
    ("done_set", "{}: {} = {}"),
    ("done_reset", "{}: {} is back to its default"),
    ("confirm_uninstall", "Remove Underkeep QoL from {}?"),
    ("removed", "removed {}"),
    (
        "kept_modified",
        "kept {} (changed since install; --force removes it)",
    ),
    (
        "restored_backup",
        "Put back the winmm.dll that was there before.",
    ),
    (
        "kept_config",
        "Your settings (uk_qol/config.toml) are kept for a reinstall.",
    ),
    (
        "kept_sidecars",
        "Mod data for your saves ({}) is kept, so reinstalling brings it back.",
    ),
    ("done_uninstalled", "Underkeep QoL is uninstalled."),
    (
        "done_report",
        "Report written to {}. Attach it to your issue on GitHub.",
    ),
    ("update_checking", "Checking for updates…"),
    (
        "update_none_published",
        "No release has been published yet.",
    ),
    ("update_latest", "Latest release: {} ({})"),
    ("update_offer_self", "uk-qol {} → {}"),
    ("update_offer_runtime", "loader and mods {} → {}"),
    ("update_offer_patterns", "new patterns for {}"),
    ("update_up_to_date", "Everything is up to date."),
    ("confirm_update", "Install these updates?"),
    ("done_runtime_updated", "Loader and mods updated to {}."),
    ("done_patterns_updated", "Patterns for {} updated."),
    (
        "done_self_updated",
        "uk-qol updated to {}; start it again to use the new version.",
    ),
    // --- errors from the library parts ---
    ("err_read", "can't read {}: {}"),
    ("err_write", "can't write {}: {}"),
    ("err_remove", "can't remove {}: {}"),
    ("err_config_parse", "{} isn't valid TOML: {}"),
    ("err_config_shape", "`{}` in config.toml isn't a table"),
    (
        "err_bad_value",
        "`{}` isn't a valid value for {} (expected {})",
    ),
    ("err_out_of_range", "{} is out of range ({} to {})"),
    ("err_bad_choice", "`{}` isn't one of: {}"),
    ("err_bad_setting", "{}: {}"),
    ("kind_int", "whole number"),
    ("kind_float", "number"),
    ("kind_text", "text"),
    ("kind_key", "key"),
    ("kind_color", "color like #33FF33"),
    ("err_manifest_parse", "{} is damaged: {}"),
    (
        "manifest_header",
        "# Written by uk-qol: what it installed into this game folder, so that\n# `uk-qol uninstall` removes exactly these files. Don't edit.",
    ),
    (
        "err_not_game_dir",
        "{} isn't an Underkeep folder (no underkeep.exe in it)",
    ),
    ("err_not_dll", "that file isn't a 64-bit Windows DLL"),
    (
        "err_foreign_dll",
        "the game folder already has a winmm.dll that uk-qol didn't install (another mod?). Use --force to replace it; it's kept and put back on uninstall.",
    ),
    ("err_bad_mod_id", "bad mod id `{}`"),
    ("err_not_installed", "Underkeep QoL isn't installed there"),
    (
        "err_no_record",
        "there's a winmm.dll but no uk_qol/install.toml, so uk-qol can't tell whether it's its own; use --force to remove it anyway",
    ),
    (
        "err_dll_changed",
        "winmm.dll changed since uk-qol installed it (another mod or a manual copy?); use --force to remove it anyway",
    ),
    (
        "err_no_install",
        "no Underkeep install found; pass the game folder with --game PATH",
    ),
    (
        "err_game_running",
        "Underkeep is running. Quit the game first: files can't be changed while it runs.",
    ),
    ("err_vdf", "can't read Steam's config: {}"),
    (
        "err_launch_no_app",
        "Steam has no entry for Underkeep in this account yet",
    ),
    (
        "err_launch_other",
        "the game's launch options already set winmm another way; change them by hand",
    ),
    ("err_update_reach", "couldn't reach GitHub: {}"),
    ("err_update_status", "GitHub answered with status {}"),
    ("err_update_reply", "unexpected reply from GitHub"),
    (
        "err_update_reply_detail",
        "unexpected reply from GitHub: {}",
    ),
    (
        "err_update_sum_mismatch",
        "the checksums published for {} disagree; not using it",
    ),
    (
        "err_update_no_sum",
        "{} has no SHA-256 checksum in the release, so it can't be verified",
    ),
    ("err_update_no_asset", "release {} has no {}"),
    (
        "err_update_corrupt",
        "the download is corrupt (expected SHA-256 {}, got {}); nothing was changed",
    ),
    ("err_update_no_dll", "{} has no winmm.dll in it"),
    (
        "err_update_platform",
        "automatic updates of uk-qol aren't available for this system; download it from the Releases page",
    ),
    ("err_update_self", "can't find the running program: {}"),
    ("err_update_unpack", "couldn't unpack the update: {}"),
    (
        "err_update_restart",
        "updated, but couldn't restart: {}; start uk-qol again",
    ),
    ("err_download", "download failed: {}"),
    // --- app ---
    ("app_title", "Underkeep QoL"),
    ("gui_theme_dark", "Dark"),
    ("gui_theme_light", "Light"),
    ("gui_theme_system", "Match system"),
    ("gui_check_updates", "Check for updates"),
    ("gui_update_available", "Update {} available: {}."),
    ("gui_update_part_self", "this app"),
    ("gui_update_part_runtime", "loader and mods"),
    ("gui_update_part_patterns", "patterns for {} mod(s)"),
    ("gui_release_notes", "What's new"),
    ("gui_update_now", "Update now"),
    ("gui_later", "Later"),
    ("gui_updating", "Updating…"),
    ("gui_restart", "Restart the app"),
    ("gui_ok", "OK"),
    (
        "gui_game_running",
        "Underkeep is running. Quit the game to install, uninstall or change settings (use the in-game menu, F10, meanwhile).",
    ),
    ("gui_no_game", "No game folder"),
    ("gui_browse", "Browse…"),
    ("gui_pick_game", "Underkeep game folder"),
    ("gui_rescan", "Search again"),
    ("gui_open_folder", "Open folder"),
    ("gui_unknown", "unknown"),
    (
        "gui_dll_bundled",
        "Installs the winmm.dll next to this app.",
    ),
    (
        "gui_dll_download",
        "Downloads winmm.dll from the latest release.",
    ),
    ("gui_dll_file", "Installs {}"),
    ("gui_install", "Install"),
    ("gui_reinstall", "Reinstall"),
    ("gui_uninstall", "Uninstall"),
    (
        "gui_uninstall_disabled",
        "Not installed, or the game is running",
    ),
    (
        "gui_force",
        "Replace the winmm.dll that's there (it's kept and put back on uninstall)",
    ),
    ("gui_choose_dll", "Other winmm.dll…"),
    (
        "gui_launch_help",
        "Proton needs this in Steam: Underkeep > Properties > Launch Options.",
    ),
    ("gui_copy", "Copy"),
    (
        "gui_copied",
        "Copied. Paste it into Steam: Underkeep > Properties > Launch Options.",
    ),
    ("gui_account", "Steam account"),
    ("gui_set_launch", "Set it in Steam for me"),
    (
        "gui_launch_set_done",
        "Launch option set (Steam's config was backed up first).",
    ),
    ("gui_help_title", "Problems?"),
    (
        "gui_report_help",
        "Save a report and attach it to an issue on GitHub. Your home folder name is left out.",
    ),
    ("gui_save_report", "Save bug report…"),
    ("gui_report_title", "Save the bug report"),
    ("gui_welcome", "Underkeep not found"),
    (
        "gui_welcome_help",
        "Pick the game folder (the one with underkeep.exe) with Browse… on the left.",
    ),
    ("gui_compat_title", "Compatibility"),
    ("gui_compat_build", "Your game: version {} (exe sha256 {}…)"),
    ("gui_locked", "Settings are locked while the game runs."),
    ("gui_reset", "Default"),
    ("gui_reset_help", "Go back to the mod's default"),
    ("gui_uninstall_title", "Uninstall Underkeep QoL?"),
    (
        "gui_uninstall_keeps",
        "Your saves are not touched. Mod data for your saves (in Steam Cloud, or uk_qol/saves without Steam) is kept, so reinstalling brings it back.",
    ),
    ("gui_purge", "Also remove my settings and logs"),
    ("gui_cancel", "Cancel"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_placeholders() {
        assert_eq!(fmt("done_set", &[&"m", &"k", &3]), "m: k = 3");
        assert_eq!(t("no such key"), "no such key");
        assert_eq!(crate::tr!("on"), "on");
    }

    #[test]
    fn keys_are_unique_and_placeholders_match_english() {
        for lang in LANGS {
            for (i, (k, v)) in lang.strings.iter().enumerate() {
                assert!(
                    !lang.strings[..i].iter().any(|(k2, _)| k2 == k),
                    "duplicate key {k}"
                );
                if let Some(en) = lookup(&LANGS[0], k) {
                    assert_eq!(
                        v.matches("{}").count(),
                        en.matches("{}").count(),
                        "{} {k}",
                        lang.code
                    );
                }
            }
        }
    }

    /// Every `tr!("key", …)` in the sources has an English text, with as many
    /// `{}` as arguments.
    #[test]
    fn every_used_key_exists() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut checked = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.file_name() == "i18n.rs" {
                continue;
            }
            let src = std::fs::read_to_string(e.path()).unwrap();
            for (pos, _) in src.match_indices("tr!(") {
                let rest = src[pos + 4..].trim_start();
                let Some(rest) = rest.strip_prefix('"') else {
                    continue;
                };
                let key = &rest[..rest.find('"').unwrap()];
                let en = lookup(&LANGS[0], key).unwrap_or_else(|| panic!("missing key {key}"));
                // Count arguments up to the matching parenthesis.
                let after = &rest[key.len() + 1..];
                let mut depth = 1;
                let mut args = 0;
                let mut in_str = false;
                let mut prev = ' ';
                for c in after.chars() {
                    if in_str {
                        if c == '"' && prev != '\\' {
                            in_str = false;
                        }
                    } else {
                        match c {
                            '"' => in_str = true,
                            '(' | '[' | '{' => depth += 1,
                            ')' | ']' | '}' => depth -= 1,
                            ',' if depth == 1 => args += 1,
                            _ => {}
                        }
                    }
                    if depth == 0 {
                        break;
                    }
                    prev = c;
                }
                assert_eq!(en.matches("{}").count(), args, "placeholders of {key}");
                checked += 1;
            }
        }
        assert!(checked > 100);
    }
}
