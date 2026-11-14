//! Translatable strings for the overlay (PLAN §4).
//!
//! Each string has an id; [`tr`] looks it up in the current language and
//! falls back to English. Only English is filled in. To translate, add
//! `(id, text)` pairs to the language's table; missing ids fall back to
//! English, so a partial table is fine. `{0}`, `{1}`… are placeholders for
//! [`trf`] and must be kept.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Lang {
    En,
    Cs,
    De,
    Es,
    Fr,
    Pl,
    Ru,
}

impl Lang {
    pub const ALL: [Lang; 7] = [
        Lang::En,
        Lang::Cs,
        Lang::De,
        Lang::Es,
        Lang::Fr,
        Lang::Pl,
        Lang::Ru,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Cs => "cs",
            Lang::De => "de",
            Lang::Es => "es",
            Lang::Fr => "fr",
            Lang::Pl => "pl",
            Lang::Ru => "ru",
        }
    }

    /// From a locale like `de`, `de_DE`, `de-AT` or `pt_BR.UTF-8`.
    pub fn from_locale(locale: &str) -> Option<Lang> {
        let lang = locale
            .split(['_', '-', '.', '@'])
            .next()?
            .to_ascii_lowercase();
        Lang::ALL.into_iter().find(|l| l.code() == lang)
    }

    fn table(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Lang::En => EN,
            Lang::Cs => CS,
            Lang::De => DE,
            Lang::Es => ES,
            Lang::Fr => FR,
            Lang::Pl => PL,
            Lang::Ru => RU,
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(Lang::En as u8);

static EXPLICIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Sets the language (e.g. from the user's config). Wins over the system
/// locale the overlay detects.
pub fn set_lang(lang: Lang) {
    EXPLICIT.store(true, Ordering::Relaxed);
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

/// Sets the language from the system locale, unless [`set_lang`] was called.
pub fn set_default_lang(lang: Lang) {
    if !EXPLICIT.load(Ordering::Relaxed) {
        CURRENT.store(lang as u8, Ordering::Relaxed);
    }
}

pub fn lang() -> Lang {
    let v = CURRENT.load(Ordering::Relaxed);
    Lang::ALL
        .into_iter()
        .find(|l| *l as u8 == v)
        .unwrap_or(Lang::En)
}

fn lookup(lang: Lang, id: &str) -> Option<&'static str> {
    lang.table().iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
}

/// The text for `id` in the current language, else English, else the id.
pub fn tr(id: &'static str) -> &'static str {
    lookup(lang(), id)
        .or_else(|| lookup(Lang::En, id))
        .unwrap_or(id)
}

/// [`tr`] with `{0}`, `{1}`… replaced by `args`.
pub fn trf(id: &'static str, args: &[&str]) -> String {
    let mut s = tr(id).to_owned();
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), a);
    }
    s
}

const EN: &[(&str, &str)] = &[
    ("menu.title", "Underkeep QoL"),
    ("menu.version", "Version {0}"),
    ("menu.close_hint", "{0} or {1} closes this menu."),
    ("menu.close", "Close"),
    (
        "menu.no_host",
        "The mod host isn't running, so no mods are loaded.",
    ),
    ("menu.no_mods", "No mods are installed."),
    ("mod.enabled", "Enabled"),
    ("mod.settings", "Settings"),
    ("status.active", "Active"),
    ("status.off", "Off"),
    ("status.failed", "Turned off after an error: {0}"),
    (
        "status.unsupported",
        "Not available on this game version: {0}",
    ),
    ("setting.rebind", "Change"),
    ("setting.unbound", "Not set"),
    ("setting.clear", "Clear"),
    ("setting.default", "Default"),
    (
        "setting.press",
        "Press a key or gamepad button (Esc cancels)",
    ),
    ("setting.error", "Couldn't change this setting: {0}"),
];

// Not translated yet: everything falls back to English.
const CS: &[(&str, &str)] = &[];
const DE: &[(&str, &str)] = &[];
const ES: &[(&str, &str)] = &[];
const FR: &[(&str, &str)] = &[];
const PL: &[(&str, &str)] = &[];
const RU: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(s: &str) -> Vec<usize> {
        (0..10)
            .filter(|i| s.contains(&format!("{{{i}}}")))
            .collect()
    }

    #[test]
    fn catalogs_are_consistent() {
        let mut seen = std::collections::HashSet::new();
        for (id, _) in EN {
            assert!(seen.insert(*id), "duplicate id {id}");
        }
        for lang in Lang::ALL {
            for (id, text) in lang.table() {
                let en =
                    lookup(Lang::En, id).unwrap_or_else(|| panic!("{lang:?}: unknown id {id}"));
                assert_eq!(placeholders(text), placeholders(en), "{lang:?} {id}");
            }
        }
    }

    #[test]
    fn falls_back_and_formats() {
        assert_eq!(lookup(Lang::De, "menu.close"), None);
        assert_eq!(tr("no.such.id"), "no.such.id");
        assert_eq!(trf("menu.version", &["0.1.0"]), "Version 0.1.0");
        assert_eq!(Lang::from_locale("de_DE.UTF-8"), Some(Lang::De));
        assert_eq!(Lang::from_locale("pt-BR"), None);
        assert_eq!(Lang::from_locale("RU"), Some(Lang::Ru));
    }
}
