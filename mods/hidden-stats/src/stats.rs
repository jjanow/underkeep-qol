//! The stats the mod adds, where each value comes from, and its English
//! text.
//!
//! # Where the values come from (1.0.5 and 1.0.6)
//! `Hero::RecalcStats` (1.0.5 0x14017a8b0) rebuilds a block of the hero's
//! fields from `Hero+0x10c` on whenever gear, level, statuses or passives
//! change: it zeroes them, sets the base values, adds the same-shaped
//! "stat block" of every equipped item, passive and active effect
//! (0x1400cfe80), the stat-derived parts (DEX to crit, ranged hit and
//! evasion; WIS and END to resistances), clamps some (0x1400d0110) and caps
//! evasion. The block's fields are named by the item/effect definition
//! parser (0x1400c2880), and the units by the item tooltip, which shows
//! each as a flat number or a percentage. The mod only reads those fields:
//! every number is the game's own, after gear, traits and effects.
//!
//! What uses them (checked in the code that reads them):
//! - melee hit: `100 + Hero+0x160 - target evasion - target melee evasion`
//!   (hero melee attack 0x14012f830); ranged hit: `Hero+0x164` (DEX x 9 in
//!   1.0.6, x 10 before, plus bonuses) minus the target's evasion and
//!   ranged evasion (0x140133160).
//! - armor against an enemy hit (0x14012e090): melee `armor + Hero+0x140`
//!   (+ `Hero+0x13c` if the attacker is an animal), ranged `armor +
//!   Hero+0x144`, minus the attacker's armor pierce.
//! - an enemy's hit chance on the hero: `100 - evasion` (`Hero+0x128`,
//!   capped at `60 + Hero+0x130`), minus `Hero+0x12c` too for a ranged
//!   attack, minus 50 if the attacker is blind.
//! - heals: `Hero+0x1e0` is added to heals the hero receives (0x14017d0d0),
//!   `Hero+0x1e8` to the hero's healing spells, `Hero+0x1e4` to Void Mend
//!   (0x1401839e0).
//! - immunities: bytes `Hero+0x200..0x206` (stun, critical hits, freeze,
//!   blindness, paralysis, disease, fall damage).

/// The stats page's sections the mod adds to (rows go at each one's end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Hero,
    Offense,
    Defense,
}

/// A value as the row shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Val {
    /// `62%`
    Pct(i32),
    /// `+15%`, `-5%`, `0%`
    SignedPct(i32),
    /// `+3`, `-2`, `0`
    Signed(i32),
    /// `12`
    Plain(i32),
    /// `Yes` / `No`
    Flag(bool),
}

impl Val {
    pub fn text(self) -> String {
        let signed = |v: i32| {
            if v > 0 {
                format!("+{v}")
            } else {
                v.to_string()
            }
        };
        match self {
            Val::Pct(v) => format!("{v}%"),
            Val::SignedPct(v) => format!("{}%", signed(v)),
            Val::Signed(v) => signed(v),
            Val::Plain(v) => v.to_string(),
            Val::Flag(true) => "Yes".into(),
            Val::Flag(false) => "No".into(),
        }
    }
}

/// Reads the hero: `i32(offset)` and `u8(offset)`; `None` if unreadable.
pub trait Hero {
    fn i32(&self, off: usize) -> Option<i32>;
    fn u8(&self, off: usize) -> Option<u8>;
}

/// The value and whether the row shows when "Show stats that are 0" is off.
pub struct Reading {
    pub val: Val,
    pub shown: bool,
}

/// How a stat is read.
#[derive(Debug, Clone, Copy)]
pub enum Source {
    /// `100 + i32` (melee hit chance).
    HundredPlus(usize),
    /// `clamp(60 + i32, 0, 100)` (the evasion cap).
    EvasionCap(usize),
    /// `base + extra`, shown while `extra` isn't 0 (armor vs animals:
    /// armor + melee protection + animal armor).
    SumIfExtra(&'static [usize], usize),
    /// The sum of the fields.
    Sum(&'static [usize]),
    /// One field.
    Field(usize),
    /// A byte flag.
    Flag(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Pct,
    SignedPct,
    Signed,
    Plain,
    Flag,
}

pub struct Stat {
    /// Row key suffix: the text keys are `uk_qol_hs_<key>` (label) and
    /// `uk_qol_hs_<key>_desc` (description).
    pub key: &'static str,
    pub section: Section,
    pub source: Source,
    pub unit: Unit,
    /// Shown even when 0 (else only with "Show stats that are 0").
    pub always: bool,
    /// Label (English).
    pub label: &'static str,
    /// The description panel's text (English), under the label.
    pub desc: &'static str,
    /// The label's color tag in the game's markup, for element rows.
    pub color: &'static str,
}

impl Stat {
    pub fn read(&self, h: &dyn Hero) -> Option<Reading> {
        let (v, nonzero) = match self.source {
            Source::HundredPlus(o) => {
                let v = 100 + h.i32(o)?;
                (v, true)
            }
            Source::EvasionCap(o) => ((60 + h.i32(o)?).clamp(0, 100), true),
            Source::SumIfExtra(base, extra) => {
                let e = h.i32(extra)?;
                let mut s = e;
                for &o in base {
                    s += h.i32(o)?;
                }
                (s, e != 0)
            }
            Source::Sum(fields) => {
                let mut s = 0;
                for &o in fields {
                    s += h.i32(o)?;
                }
                (s, s != 0)
            }
            Source::Field(o) => {
                let v = h.i32(o)?;
                (v, v != 0)
            }
            Source::Flag(o) => {
                let v = h.u8(o)? != 0;
                (v as i32, v)
            }
        };
        let val = match self.unit {
            Unit::Pct => Val::Pct(v),
            Unit::SignedPct => Val::SignedPct(v),
            Unit::Signed => Val::Signed(v),
            Unit::Plain => Val::Plain(v),
            Unit::Flag => Val::Flag(v != 0),
        };
        Some(Reading {
            val,
            shown: self.always || nonzero,
        })
    }
}

// Hero fields (i32 unless noted), 1.0.5 and 1.0.6.
const ARMOR: usize = 0x134;
const EVASION_RANGED: usize = 0x12c;
const EVASION_CAP: usize = 0x130;
const ARMOR_ANIMALS: usize = 0x13c;
const PROTECT_MELEE: usize = 0x140;
const PROTECT_RANGED: usize = 0x144;
const UNARMED_UP: usize = 0x148;
const MELEE_UP: usize = 0x14c;
const RANGED_UP: usize = 0x150;
const DMG_ANIMALS: usize = 0x154;
const DMG_BLEEDING: usize = 0x158;
const DMG_OBJECTS: usize = 0x15c;
const MELEE_HIT: usize = 0x160;
const RANGED_HIT: usize = 0x164;
const UNARMED_BONUS: usize = 0x168;
const MELEE_BONUS: usize = 0x16c;
const RANGED_BONUS: usize = 0x170;
const MELEE_PIERCE: usize = 0x174;
const RANGED_PIERCE: usize = 0x178;
const RESIST_VOID: usize = 0x198;
const XP_BONUS: usize = 0x19c;
const SPELL_BONUS: usize = 0x1a0;
const FIRE_SPELL_BONUS: usize = 0x1a4;
const ICE_SPELL_BONUS: usize = 0x1a8;
const LIGHTNING_SPELL_BONUS: usize = 0x1ac;
const BLOOD_SPELL_BONUS: usize = 0x1b0;
const FIRE_BONUS: usize = 0x1b4;
const ICE_BONUS: usize = 0x1b8;
const LIGHTNING_BONUS: usize = 0x1bc;
const SPELL_UP: usize = 0x1c0;
const FIRE_SPELL_UP: usize = 0x1c4;
const ICE_SPELL_UP: usize = 0x1c8;
const LIGHTNING_SPELL_UP: usize = 0x1cc;
const BLOOD_SPELL_UP: usize = 0x1d0;
const FIRE_UP: usize = 0x1d4;
const ICE_UP: usize = 0x1d8;
const LIGHTNING_UP: usize = 0x1dc;
const HEAL_RECEIVED: usize = 0x1e0;
const VOID_MEND_HEAL: usize = 0x1e4;
const SPELL_HEAL: usize = 0x1e8;
const BURN_CHANCE: usize = 0x1ec;
const BURN_DURATION: usize = 0x1f0;
const FREEZE_CHANCE: usize = 0x1f4;
const SPELL_FREEZE_CHANCE: usize = 0x1f8;
const BLEED_UP: usize = 0x1fc;
// u8 flags.
const IMMUNE_STUN: usize = 0x200;
const IMMUNE_CRIT: usize = 0x201;
const IMMUNE_FREEZE: usize = 0x202;
const IMMUNE_BLIND: usize = 0x203;
const IMMUNE_PARALYZE: usize = 0x204;
const IMMUNE_DISEASE: usize = 0x205;
const IMMUNE_FALL: usize = 0x206;

/// Key of the melee hit chance row while the melee-accuracy mod is on (same
/// label, a description that says so).
pub const MELEE_HIT_MA: &str = "melee_hit_ma";
pub const MELEE_HIT_MA_DESC: &str = "The chance to hit with a melee attack before the target's \
evasion, as the game rolls it: 100% plus melee hit bonuses from gear and effects.\\n\\nThe \
melee-accuracy mod is on: in an attack it puts its own formula's chance in place of the 100% \
(see its settings in the mod menu), so your real chance can differ from this number.";

macro_rules! stat {
    ($key:literal, $sec:ident, $src:expr, $unit:ident, $always:literal, $label:literal, $color:literal, $desc:expr) => {
        Stat {
            key: $key,
            section: Section::$sec,
            source: $src,
            unit: Unit::$unit,
            always: $always,
            label: $label,
            desc: $desc,
            color: $color,
        }
    };
}

use Source::{EvasionCap, Field, Flag, HundredPlus, Sum, SumIfExtra};

/// Every stat the mod adds, in display order within each section.
pub static STATS: &[Stat] = &[
    // --- Hero ---------------------------------------------------------------
    stat!(
        "xp_bonus",
        Hero,
        Field(XP_BONUS),
        SignedPct,
        false,
        "Experience Bonus",
        "",
        "Extra experience this hero gets, in percent of what they earn."
    ),
    stat!(
        "heal_received",
        Hero,
        Field(HEAL_RECEIVED),
        Signed,
        false,
        "Healing Received",
        "",
        "Health added to heals this hero receives."
    ),
    stat!(
        "spell_heal",
        Hero,
        Field(SPELL_HEAL),
        Signed,
        false,
        "Spell Healing",
        "",
        "Health added to every healing spell this hero casts."
    ),
    stat!(
        "void_mend_heal",
        Hero,
        Field(VOID_MEND_HEAL),
        Signed,
        false,
        "Void Mend Healing",
        "",
        "Health added to this hero's Void Mend."
    ),
    // --- Offense ------------------------------------------------------------
    stat!(
        "melee_hit",
        Offense,
        HundredPlus(MELEE_HIT),
        Pct,
        true,
        "Melee Hit Chance",
        "",
        "The chance to hit with a melee attack before the target's evasion: 100% plus melee \
hit bonuses from gear and effects. The target's evasion and melee evasion are taken off it \
when the attack is rolled."
    ),
    stat!(
        "ranged_hit",
        Offense,
        Field(RANGED_HIT),
        Pct,
        true,
        "Ranged Hit Chance",
        "",
        "The chance to hit with a ranged attack before the target's evasion. It comes from \
Dexterity, plus ranged hit bonuses from gear and effects. The target's evasion and ranged \
evasion are taken off it when the attack is rolled."
    ),
    stat!(
        "melee_damage",
        Offense,
        Field(MELEE_UP),
        Signed,
        true,
        "Melee Damage",
        "",
        "Flat damage added to this hero's melee hits, from gear, passives and effects."
    ),
    stat!(
        "melee_damage_bonus",
        Offense,
        Field(MELEE_BONUS),
        SignedPct,
        true,
        "Melee Damage Bonus",
        "",
        "Percent added to the weapon's damage for melee hits, from gear and effects."
    ),
    stat!(
        "ranged_damage",
        Offense,
        Field(RANGED_UP),
        Signed,
        true,
        "Ranged Damage",
        "",
        "Flat damage added to this hero's ranged hits, from gear, passives and effects."
    ),
    stat!(
        "ranged_damage_bonus",
        Offense,
        Field(RANGED_BONUS),
        SignedPct,
        true,
        "Ranged Damage Bonus",
        "",
        "Percent added to the weapon's damage for ranged hits, from gear and effects."
    ),
    stat!(
        "unarmed_damage",
        Offense,
        Field(UNARMED_UP),
        Signed,
        false,
        "Unarmed Damage",
        "",
        "Flat damage added to this hero's hits with bare hands."
    ),
    stat!(
        "unarmed_damage_bonus",
        Offense,
        Field(UNARMED_BONUS),
        SignedPct,
        false,
        "Unarmed Damage Bonus",
        "",
        "Percent added to this hero's damage with bare hands."
    ),
    stat!(
        "melee_pierce",
        Offense,
        Field(MELEE_PIERCE),
        Plain,
        true,
        "Armor Ignored (Melee)",
        "",
        "Points of the target's armor this hero's melee hits ignore, from gear and effects \
(a weapon's own armor pierce is in its tooltip)."
    ),
    stat!(
        "ranged_pierce",
        Offense,
        Field(RANGED_PIERCE),
        Plain,
        true,
        "Armor Ignored (Ranged)",
        "",
        "Points of the target's armor this hero's ranged hits ignore, from gear and effects \
(a weapon's own armor pierce is in its tooltip)."
    ),
    stat!(
        "damage_animals",
        Offense,
        Field(DMG_ANIMALS),
        Signed,
        false,
        "Damage to Animals",
        "",
        "Flat damage added to this hero's hits on animals."
    ),
    stat!(
        "damage_bleeding",
        Offense,
        Field(DMG_BLEEDING),
        Signed,
        false,
        "Damage to Bleeding Enemies",
        "",
        "Flat damage added to this hero's hits on enemies that are bleeding."
    ),
    stat!(
        "damage_objects",
        Offense,
        Field(DMG_OBJECTS),
        Signed,
        false,
        "Damage to Objects",
        "",
        "Damage added against objects."
    ),
    stat!(
        "fire_damage",
        Offense,
        Field(FIRE_UP),
        Signed,
        false,
        "Fire Damage",
        "{color_fire}",
        "Flat fire damage added by gear and effects."
    ),
    stat!(
        "fire_damage_bonus",
        Offense,
        Field(FIRE_BONUS),
        SignedPct,
        false,
        "Fire Damage Bonus",
        "{color_fire}",
        "Percent added to the fire damage this hero deals."
    ),
    stat!(
        "ice_damage",
        Offense,
        Field(ICE_UP),
        Signed,
        false,
        "Ice Damage",
        "{color_ice}",
        "Flat ice damage added by gear and effects."
    ),
    stat!(
        "ice_damage_bonus",
        Offense,
        Field(ICE_BONUS),
        SignedPct,
        false,
        "Ice Damage Bonus",
        "{color_ice}",
        "Percent added to the ice damage this hero deals."
    ),
    stat!(
        "lightning_damage",
        Offense,
        Field(LIGHTNING_UP),
        Signed,
        false,
        "Lightning Damage",
        "{color_lightning}",
        "Flat lightning damage added by gear and effects."
    ),
    stat!(
        "lightning_damage_bonus",
        Offense,
        Field(LIGHTNING_BONUS),
        SignedPct,
        false,
        "Lightning Damage Bonus",
        "{color_lightning}",
        "Percent added to the lightning damage this hero deals."
    ),
    stat!(
        "spell_damage",
        Offense,
        Field(SPELL_UP),
        Signed,
        false,
        "Spell Damage",
        "",
        "Flat damage added to this hero's spells."
    ),
    stat!(
        "spell_damage_bonus",
        Offense,
        Field(SPELL_BONUS),
        SignedPct,
        false,
        "Spell Damage Bonus",
        "",
        "Percent added to the damage of this hero's spells."
    ),
    stat!(
        "fire_spell_damage",
        Offense,
        Field(FIRE_SPELL_UP),
        Signed,
        false,
        "Fire Spell Damage",
        "{color_fire}",
        "Flat damage added to this hero's fire spells."
    ),
    stat!(
        "fire_spell_damage_bonus",
        Offense,
        Field(FIRE_SPELL_BONUS),
        SignedPct,
        false,
        "Fire Spell Damage Bonus",
        "{color_fire}",
        "Percent added to the damage of this hero's fire spells."
    ),
    stat!(
        "ice_spell_damage",
        Offense,
        Field(ICE_SPELL_UP),
        Signed,
        false,
        "Ice Spell Damage",
        "{color_ice}",
        "Flat damage added to this hero's ice spells."
    ),
    stat!(
        "ice_spell_damage_bonus",
        Offense,
        Field(ICE_SPELL_BONUS),
        SignedPct,
        false,
        "Ice Spell Damage Bonus",
        "{color_ice}",
        "Percent added to the damage of this hero's ice spells."
    ),
    stat!(
        "lightning_spell_damage",
        Offense,
        Field(LIGHTNING_SPELL_UP),
        Signed,
        false,
        "Lightning Spell Damage",
        "{color_lightning}",
        "Flat damage added to this hero's lightning spells."
    ),
    stat!(
        "lightning_spell_damage_bonus",
        Offense,
        Field(LIGHTNING_SPELL_BONUS),
        SignedPct,
        false,
        "Lightning Spell Damage Bonus",
        "{color_lightning}",
        "Percent added to the damage of this hero's lightning spells."
    ),
    stat!(
        "blood_spell_damage",
        Offense,
        Field(BLOOD_SPELL_UP),
        Signed,
        false,
        "Blood Spell Damage",
        "{color_blood_magic}",
        "Flat damage added to this hero's blood magic spells."
    ),
    stat!(
        "blood_spell_damage_bonus",
        Offense,
        Field(BLOOD_SPELL_BONUS),
        SignedPct,
        false,
        "Blood Spell Damage Bonus",
        "{color_blood_magic}",
        "Percent added to the damage of this hero's blood magic spells."
    ),
    stat!(
        "burn_chance",
        Offense,
        Field(BURN_CHANCE),
        Pct,
        false,
        "Chance to Burn",
        "{color_fire}",
        "Chance that this hero's hits set the target on fire."
    ),
    stat!(
        "burn_duration",
        Offense,
        Field(BURN_DURATION),
        Signed,
        false,
        "Burn Duration",
        "{color_fire}",
        "Rounds added to the burns this hero causes."
    ),
    stat!(
        "freeze_chance",
        Offense,
        Field(FREEZE_CHANCE),
        Pct,
        false,
        "Chance to Freeze",
        "{color_ice}",
        "Chance that this hero's hits freeze the target."
    ),
    stat!(
        "spell_freeze_chance",
        Offense,
        Field(SPELL_FREEZE_CHANCE),
        Pct,
        false,
        "Ice Spell Freeze",
        "{color_ice}",
        "Chance that this hero's ice spells freeze the target."
    ),
    stat!(
        "bleed_damage",
        Offense,
        Field(BLEED_UP),
        Signed,
        false,
        "Bleed Damage",
        "",
        "Damage added to each round of every Bleed this hero applies."
    ),
    // --- Defense ------------------------------------------------------------
    stat!(
        "armor_melee",
        Defense,
        Sum(&[ARMOR, PROTECT_MELEE]),
        Plain,
        true,
        "Armor vs Melee",
        "",
        "Armor against melee attacks: Armor plus melee protection from gear. The attacker's \
armor pierce is taken off it."
    ),
    stat!(
        "armor_ranged",
        Defense,
        Sum(&[ARMOR, PROTECT_RANGED]),
        Plain,
        true,
        "Armor vs Ranged",
        "",
        "Armor against ranged attacks: Armor plus ranged protection from gear. The attacker's \
armor pierce is taken off it."
    ),
    stat!(
        "armor_animals",
        Defense,
        SumIfExtra(&[ARMOR, PROTECT_MELEE], ARMOR_ANIMALS),
        Plain,
        false,
        "Armor vs Animals",
        "",
        "Armor against melee attacks by animals: Armor vs Melee plus armor against animals \
from gear."
    ),
    stat!(
        "ranged_evasion",
        Defense,
        Field(EVASION_RANGED),
        Pct,
        true,
        "Ranged Evasion",
        "",
        "Evasion against ranged attacks, on top of Evasion. It isn't limited by the evasion \
cap."
    ),
    stat!(
        "evasion_cap",
        Defense,
        EvasionCap(EVASION_CAP),
        Pct,
        true,
        "Evasion Cap",
        "",
        "The most Evasion this hero can have: 60% plus evasion cap bonuses from gear and \
effects, at most 100%."
    ),
    stat!(
        "void_resist",
        Defense,
        Field(RESIST_VOID),
        Plain,
        true,
        "Void Resistance",
        "{color_void}",
        "Reduces void damage taken, like the other resistances."
    ),
    stat!(
        "immune_stun",
        Defense,
        Flag(IMMUNE_STUN),
        Flag,
        false,
        "Immune to Stun",
        "",
        "This hero can't be stunned."
    ),
    stat!(
        "immune_critical",
        Defense,
        Flag(IMMUNE_CRIT),
        Flag,
        false,
        "Immune to Critical Hits",
        "",
        "Critical hits on this hero deal normal damage."
    ),
    stat!(
        "immune_freeze",
        Defense,
        Flag(IMMUNE_FREEZE),
        Flag,
        false,
        "Immune to Freeze",
        "",
        "This hero can't be frozen."
    ),
    stat!(
        "immune_blind",
        Defense,
        Flag(IMMUNE_BLIND),
        Flag,
        false,
        "Immune to Blindness",
        "",
        "This hero can't be blinded."
    ),
    stat!(
        "immune_paralyze",
        Defense,
        Flag(IMMUNE_PARALYZE),
        Flag,
        false,
        "Immune to Paralysis",
        "",
        "This hero can't be paralyzed."
    ),
    stat!(
        "immune_disease",
        Defense,
        Flag(IMMUNE_DISEASE),
        Flag,
        false,
        "Immune to Disease",
        "",
        "This hero can't be diseased."
    ),
    stat!(
        "immune_fall",
        Defense,
        Flag(IMMUNE_FALL),
        Flag,
        false,
        "Immune to Fall Damage",
        "",
        "This hero takes no damage from falls."
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Fake(HashMap<usize, i32>);
    impl Hero for Fake {
        fn i32(&self, off: usize) -> Option<i32> {
            Some(*self.0.get(&off).unwrap_or(&0))
        }
        fn u8(&self, off: usize) -> Option<u8> {
            Some(*self.0.get(&off).unwrap_or(&0) as u8)
        }
    }

    fn find(key: &str) -> &'static Stat {
        STATS.iter().find(|s| s.key == key).unwrap()
    }

    #[test]
    fn values_and_visibility() {
        let h = Fake(HashMap::from([
            (MELEE_HIT, -10),
            (RANGED_HIT, 54),
            (ARMOR, 7),
            (PROTECT_MELEE, 2),
            (EVASION_CAP, 50),
            (IMMUNE_STUN, 1),
            (MELEE_BONUS, 15),
        ]));
        let r = find("melee_hit").read(&h).unwrap();
        assert_eq!((r.val.text(), r.shown), ("90%".into(), true));
        assert_eq!(find("ranged_hit").read(&h).unwrap().val.text(), "54%");
        assert_eq!(find("armor_melee").read(&h).unwrap().val, Val::Plain(9));
        assert_eq!(find("armor_ranged").read(&h).unwrap().val, Val::Plain(7));
        let a = find("armor_animals").read(&h).unwrap();
        assert!(!a.shown, "no animal armor: hidden");
        assert_eq!(find("evasion_cap").read(&h).unwrap().val, Val::Pct(100));
        assert!(find("immune_stun").read(&h).unwrap().shown);
        let f = find("immune_fall").read(&h).unwrap();
        assert_eq!((f.val.text(), f.shown), ("No".into(), false));
        assert_eq!(
            find("melee_damage_bonus").read(&h).unwrap().val.text(),
            "+15%"
        );
        let x = find("xp_bonus").read(&h).unwrap();
        assert_eq!((x.val.text(), x.shown), ("0%".into(), false));
    }

    #[test]
    fn formats() {
        assert_eq!(Val::Signed(-2).text(), "-2");
        assert_eq!(Val::Signed(0).text(), "0");
        assert_eq!(Val::SignedPct(-5).text(), "-5%");
        assert_eq!(Val::Plain(3).text(), "3");
        assert_eq!(Val::Flag(true).text(), "Yes");
    }

    #[test]
    fn keys_unique_and_sections_ordered() {
        let mut keys: Vec<&str> = STATS.iter().map(|s| s.key).collect();
        keys.push(MELEE_HIT_MA);
        let n = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), n);
        for s in STATS {
            assert!(s.key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        }
        // Sections appear in page order.
        let order: Vec<Section> = STATS.iter().map(|s| s.section).collect();
        let mut sorted = order.clone();
        sorted.sort_by_key(|s| *s as u8);
        assert_eq!(order, sorted);
    }
}
