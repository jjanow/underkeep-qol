//! The mod's texts: one data patch per game language adding the rows'
//! labels and descriptions to the game's `text_common_<lang>` table, so the
//! stats page finds them with its own text lookup (label: the key's text
//! with `%1` = the value; description: key + `_desc`).
//!
//! Every language gets the English text for now; a translation replaces the
//! strings of its language here.

use std::sync::OnceLock;

use crate::stats::{MELEE_HIT_MA, MELEE_HIT_MA_DESC, STATS, Stat};

/// The game's languages (`text_common_<lang>` tables).
pub const LANGS: [&str; 7] = ["en", "de", "fr", "es", "pl", "cs", "ru"];

/// Prefix of every text key the mod adds.
pub const PREFIX: &str = "uk_qol_hs_";

/// The label's text: the stat's name on the left, the value (`%1`) on the
/// right, both in the page's dim text color (an element's color for
/// element stats), so they read as the mod's rows.
pub fn label(s: &Stat) -> String {
    let color = if s.color.is_empty() {
        "{color_text_darker}"
    } else {
        s.color
    };
    format!("{color}{}{{a:right}}{{color_text_darker}}%1", s.label)
}

/// The description panel's text, in the style of the game's own: the name
/// in white, then the explanation.
pub fn desc(label: &str, body: &str) -> String {
    format!("{{color_white}}{label}{{color_text}}\\n\\n{{color_text}}{body}")
}

/// A TOML basic string.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Every (key, text) pair the mod adds.
pub fn entries() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for s in STATS {
        out.push((format!("{PREFIX}{}", s.key), label(s)));
        out.push((format!("{PREFIX}{}_desc", s.key), desc(s.label, s.desc)));
    }
    let melee = STATS
        .iter()
        .find(|s| s.key == "melee_hit")
        .expect("melee_hit row");
    out.push((format!("{PREFIX}{MELEE_HIT_MA}"), label(melee)));
    out.push((
        format!("{PREFIX}{MELEE_HIT_MA}_desc"),
        desc(melee.label, MELEE_HIT_MA_DESC),
    ));
    out
}

/// The patch for one language.
pub fn patch(lang: &str) -> String {
    let table = format!("text_common_{lang}");
    let mut t = format!(
        "# hidden-stats: labels and descriptions of the stats page rows.\nfile = \"text/{table}.txt\"\n"
    );
    for (k, v) in entries() {
        t.push_str(&format!(
            "\n[[op]]\nadd = \"{table}.{k}\"\nvalue = {}\n",
            toml_str(&v)
        ));
    }
    t
}

/// `Mod::patches`: (file name, TOML) per language, built once.
pub fn patches() -> &'static [(&'static str, &'static str)] {
    static P: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    P.get_or_init(|| {
        LANGS
            .iter()
            .map(|l| {
                let name: &'static str =
                    Box::leak(format!("text_common_{l}.toml").into_boxed_str());
                let text: &'static str = Box::leak(patch(l).into_boxed_str());
                (name, text)
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_parse_and_apply() {
        assert_eq!(patches().len(), LANGS.len());
        for (name, text) in patches() {
            let p = dft::patch::Patch::parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            // A hand-built text document like the game's: one table.
            let lang = name
                .trim_start_matches("text_common_")
                .trim_end_matches(".toml");
            let mut doc = dft::Doc {
                version: 1,
                strings: Vec::new(),
                len_widths: Vec::new(),
                root: Vec::new(),
            };
            let table = doc.intern(&format!("text_common_{lang}"));
            let k = doc.intern("stats_armor");
            let v = doc.intern("Armor{a:right}{color_white}%1");
            doc.root.push(dft::Entry::new(
                dft::Key::Name(table),
                dft::Value::Table(vec![dft::Entry::new(dft::Key::Name(k), dft::Value::Str(v))]),
            ));
            p.apply(&mut doc).unwrap();
            let dft::Value::Table(t) = &doc.root[0].value else {
                panic!()
            };
            assert_eq!(t.len(), 1 + entries().len());
        }
    }

    #[test]
    fn texts() {
        let e = entries();
        let get = |k: &str| e.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(
            get("uk_qol_hs_melee_hit"),
            "{color_text_darker}Melee Hit Chance{a:right}{color_text_darker}%1"
        );
        assert!(get("uk_qol_hs_fire_damage").starts_with("{color_fire}Fire Damage{a:right}"));
        assert!(get("uk_qol_hs_melee_hit_desc").starts_with("{color_white}Melee Hit Chance"));
        assert!(get("uk_qol_hs_melee_hit_ma_desc").contains("melee-accuracy"));
        // Literal \n (the game's line break markup), not a raw newline.
        assert!(get("uk_qol_hs_xp_bonus_desc").contains("\\n\\n"));
        assert!(!get("uk_qol_hs_xp_bonus_desc").contains('\n'));
        assert_eq!(toml_str("a\\n\"b\""), "\"a\\\\n\\\"b\\\"\"");
    }
}
