//! The mod's own state, independent of the game: what each hero's second
//! weapon slot holds, and how it goes into and comes out of a save's sidecar.
//!
//! A weapon in the slot is kept serialized (def name, count, cooldown: all
//! that the game itself saves of an item, see `Item::SaveState`); the game
//! object is created only when it is swapped in or moved to the inventory.

use std::collections::BTreeMap;

use uk_qol_runtime::host::ModSaveState;

/// Heroes in the party.
pub const HEROES: usize = 4;

/// An item as the game saves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredItem {
    pub def: String,
    pub count: i32,
    pub cooldown: i32,
}

impl StoredItem {
    fn to_toml(&self) -> toml::Value {
        let mut t = toml::Table::new();
        t.insert("def".into(), self.def.clone().into());
        t.insert("count".into(), i64::from(self.count).into());
        t.insert("cooldown".into(), i64::from(self.cooldown).into());
        toml::Value::Table(t)
    }

    fn from_toml(v: &toml::Value) -> Result<StoredItem, String> {
        let t = v.as_table().ok_or("not a table")?;
        let def = t
            .get("def")
            .and_then(toml::Value::as_str)
            .filter(|s| valid_def(s))
            .ok_or("missing or bad `def`")?
            .to_owned();
        let int = |k: &str, default: i64| -> Result<i32, String> {
            let v = t.get(k).map_or(Some(default), toml::Value::as_integer);
            v.and_then(|v| i32::try_from(v).ok())
                .ok_or_else(|| format!("bad `{k}`"))
        };
        let count = int("count", 1)?;
        if count < 1 {
            return Err("`count` below 1".into());
        }
        let cooldown = int("cooldown", 0)?.max(0);
        Ok(StoredItem {
            def,
            count,
            cooldown,
        })
    }
}

/// A def name as the game writes them: ASCII letters, digits, `_`.
pub fn valid_def(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One hero's part of the mod's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeroSlot {
    /// The weapon in the second slot (after a swap: the one swapped out).
    pub stored: Option<StoredItem>,
    /// The def of the shield a two-handed weapon pushed into the inventory
    /// on the last swap; put back on when a one-handed weapon comes back.
    pub shield: Option<String>,
}

impl HeroSlot {
    pub fn is_empty(&self) -> bool {
        self.stored.is_none() && self.shield.is_none()
    }
}

/// Stored weapons whose hero isn't in the party (by key), or that came
/// from a sidecar that doesn't match its save. Never dropped silently: the
/// menu offers to move them to the inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parked {
    pub key: String,
    pub item: StoredItem,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub heroes: [HeroSlot; HEROES],
    pub parked: Vec<Parked>,
}

/// Sidecar keys. `ranged` is the second slot's (named when it took only
/// ranged weapons; kept so existing sidecars still load).
const RANGED: &str = "ranged";
const SHIELD: &str = "shield";
const PARTY_ITEMS: &str = "party_items";
const PARKED: &str = "parked";

/// What [`State::from_save`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    /// The sidecar matches the save (or holds nothing).
    Ok(State),
    /// The sidecar's record of the party's items differs from the party
    /// just loaded: it was written for another state of this save (e.g. the
    /// game was saved while the mod was off). Restoring it could duplicate
    /// or lose a weapon, so its weapons are parked for the player to
    /// decide.
    Mismatch(State),
}

impl State {
    pub fn is_empty(&self) -> bool {
        self.heroes.iter().all(HeroSlot::is_empty) && self.parked.is_empty()
    }

    /// Writes the state for a save. `keys[i]` is the key of party hero `i`
    /// (`None` if the slot has no hero); `party_items` the fingerprint of
    /// the party's items at save time ([`fingerprint`]).
    pub fn to_save(&self, keys: &[Option<String>; HEROES], party_items: &str) -> ModSaveState {
        let mut out = ModSaveState::default();
        let mut parked = self.parked.clone();
        for (i, h) in self.heroes.iter().enumerate() {
            if h.is_empty() {
                continue;
            }
            let Some(key) = &keys[i] else {
                // No hero there any more: keep the weapon, parked.
                if let Some(item) = &h.stored {
                    parked.push(Parked {
                        key: format!("slot {}", i + 1),
                        item: item.clone(),
                    });
                }
                continue;
            };
            let mut t = toml::Table::new();
            if let Some(s) = &h.stored {
                t.insert(RANGED.into(), s.to_toml());
            }
            if let Some(s) = &h.shield {
                t.insert(SHIELD.into(), s.clone().into());
            }
            out.heroes.insert(key.clone(), t);
        }
        if !parked.is_empty() {
            let list = parked
                .iter()
                .map(|p| {
                    let mut t = p.item.to_toml();
                    if let toml::Value::Table(t) = &mut t {
                        t.insert("hero".into(), p.key.clone().into());
                    }
                    t
                })
                .collect();
            out.global.insert(PARKED.into(), toml::Value::Array(list));
        }
        if !self.is_empty() {
            out.global
                .insert(PARTY_ITEMS.into(), party_items.to_owned().into());
        }
        out
    }

    /// Reads the state of a save just loaded. `keys` and `party_items` as
    /// for [`State::to_save`], for the party as it is now. Entries that
    /// can't be read are logged through `warn` and skipped.
    pub fn from_save(
        st: &ModSaveState,
        keys: &[Option<String>; HEROES],
        party_items: &str,
        warn: &mut dyn FnMut(String),
    ) -> Loaded {
        let mut s = State::default();
        let mut by_key: BTreeMap<&str, usize> = BTreeMap::new();
        for (i, k) in keys.iter().enumerate() {
            if let Some(k) = k {
                by_key.insert(k, i);
            }
        }
        for (key, t) in &st.heroes {
            let stored = match t.get(RANGED).map(StoredItem::from_toml) {
                None => None,
                Some(Ok(v)) => Some(v),
                Some(Err(e)) => {
                    warn(format!(
                        "hero {key}: second weapon slot unreadable ({e}); ignored"
                    ));
                    None
                }
            };
            let shield = t
                .get(SHIELD)
                .and_then(toml::Value::as_str)
                .filter(|s| valid_def(s))
                .map(str::to_owned);
            match by_key.get(key.as_str()) {
                Some(&i) => s.heroes[i] = HeroSlot { stored, shield },
                None => {
                    if let Some(item) = stored {
                        warn(format!(
                            "hero {key} isn't in the party; their second-slot weapon is parked"
                        ));
                        s.parked.push(Parked {
                            key: key.clone(),
                            item,
                        });
                    }
                }
            }
        }
        if let Some(list) = st.global.get(PARKED).and_then(toml::Value::as_array) {
            for v in list {
                match StoredItem::from_toml(v) {
                    Ok(item) => {
                        let key = v
                            .get("hero")
                            .and_then(toml::Value::as_str)
                            .unwrap_or("?")
                            .to_owned();
                        s.parked.push(Parked { key, item });
                    }
                    Err(e) => warn(format!("parked weapon unreadable ({e}); ignored")),
                }
            }
        }
        if s.is_empty() {
            return Loaded::Ok(State::default());
        }
        let recorded = st.global.get(PARTY_ITEMS).and_then(toml::Value::as_str);
        if recorded == Some(party_items) {
            Loaded::Ok(s)
        } else {
            Loaded::Mismatch(s.into_parked(keys))
        }
    }

    /// Moves every stored weapon to the parked list (keyed by the hero it
    /// belonged to, `keys` as for [`State::to_save`]). Shields are only a
    /// convenience and are dropped.
    pub fn into_parked(self, keys: &[Option<String>; HEROES]) -> State {
        let mut p = State {
            parked: self.parked,
            ..State::default()
        };
        for (i, h) in self.heroes.into_iter().enumerate() {
            if let Some(item) = h.stored {
                let key = keys[i].clone().unwrap_or_else(|| format!("slot {}", i + 1));
                p.parked.push(Parked { key, item });
            }
        }
        p
    }
}

/// What the party looks like to the mod: hero keys and the item
/// fingerprint. If it changed while the mod was off, the slots in memory
/// may no longer belong to this party (another save was loaded).
pub fn identity(keys: &[Option<String>; HEROES], party_items: &str) -> String {
    let mut out = String::new();
    for k in keys {
        out.push_str(k.as_deref().unwrap_or("-"));
        out.push('|');
    }
    out.push_str(party_items);
    out
}

/// A fingerprint of the party's items (inventory and equipment; second
/// weapon slots not included): def names with counts, order-independent, as hex.
/// Equal for a save and the same save loaded back.
pub fn fingerprint(items: &BTreeMap<String, i64>) -> String {
    // FNV-1a over the sorted list.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (def, n) in items {
        for b in def.bytes().chain([0]).chain(n.to_le_bytes()) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// Adds every stored weapon to a def → count tally (for the conservation
/// check).
pub fn tally_stored(s: &State, into: &mut BTreeMap<String, i64>) {
    let items = s
        .heroes
        .iter()
        .filter_map(|h| h.stored.as_ref())
        .chain(s.parked.iter().map(|p| &p.item));
    for it in items {
        *into.entry(it.def.clone()).or_default() += i64::from(it.count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(def: &str) -> StoredItem {
        StoredItem {
            def: def.into(),
            count: 1,
            cooldown: 2,
        }
    }

    fn keys() -> [Option<String>; HEROES] {
        [
            Some("0:Ann".into()),
            Some("1:Bo".into()),
            None,
            Some("3:Di".into()),
        ]
    }

    fn no_warn(s: String) {
        panic!("unexpected warning: {s}");
    }

    #[test]
    fn round_trip() {
        let mut s = State::default();
        s.heroes[0].stored = Some(item("item_bow"));
        s.heroes[1].shield = Some("item_shield".into());
        s.heroes[3].stored = Some(item("item_sword"));
        s.heroes[3].shield = Some("item_shield".into());
        let saved = s.to_save(&keys(), "abc");
        assert_eq!(saved.heroes.len(), 3);
        assert_eq!(saved.global["party_items"].as_str(), Some("abc"));
        let text = toml::to_string(&saved.heroes).unwrap();
        assert!(text.contains("item_bow"));
        assert_eq!(
            State::from_save(&saved, &keys(), "abc", &mut no_warn),
            Loaded::Ok(s)
        );
    }

    #[test]
    fn empty_state_writes_nothing() {
        let saved = State::default().to_save(&keys(), "abc");
        assert_eq!(saved, ModSaveState::default());
        assert_eq!(
            State::from_save(&saved, &keys(), "zzz", &mut no_warn),
            Loaded::Ok(State::default())
        );
    }

    #[test]
    fn mismatch_parks_weapons() {
        let mut s = State::default();
        s.heroes[0].stored = Some(item("item_bow"));
        s.heroes[0].shield = Some("item_shield".into());
        let saved = s.to_save(&keys(), "abc");
        let Loaded::Mismatch(p) = State::from_save(&saved, &keys(), "other", &mut no_warn) else {
            panic!("expected a mismatch");
        };
        assert!(p.heroes.iter().all(HeroSlot::is_empty));
        assert_eq!(
            p.parked,
            vec![Parked {
                key: "0:Ann".into(),
                item: item("item_bow")
            }]
        );
    }

    #[test]
    fn unknown_hero_is_parked_and_parked_round_trips() {
        let mut s = State::default();
        s.heroes[2].stored = Some(item("item_bow")); // no hero in slot 3
        s.parked.push(Parked {
            key: "9:Old".into(),
            item: item("item_axe"),
        });
        let saved = s.to_save(&keys(), "f");
        let Loaded::Ok(l) = State::from_save(&saved, &keys(), "f", &mut no_warn) else {
            panic!()
        };
        assert_eq!(l.parked.len(), 2);
        let mut other = keys();
        other[0] = Some("0:Someone".into());
        let mut st = State::default();
        st.heroes[0].stored = Some(item("item_bow"));
        let saved = st.to_save(&keys(), "f");
        let mut warns = Vec::new();
        let Loaded::Ok(l) = State::from_save(&saved, &other, "f", &mut |w| warns.push(w)) else {
            panic!()
        };
        assert_eq!(l.parked[0].item.def, "item_bow");
        assert_eq!(warns.len(), 1);
    }

    #[test]
    fn bad_entries_are_skipped() {
        let mut st = ModSaveState::default();
        let t: toml::Table =
            toml::from_str("ranged = { def = \"bad name\", count = 1 }\nshield = \"ok_shield\"")
                .unwrap();
        st.heroes.insert("0:Ann".into(), t);
        st.global.insert("party_items".into(), "f".into());
        let mut warns = Vec::new();
        let Loaded::Ok(l) = State::from_save(&st, &keys(), "f", &mut |w| warns.push(w)) else {
            panic!()
        };
        assert_eq!(l.heroes[0].stored, None);
        assert_eq!(l.heroes[0].shield.as_deref(), Some("ok_shield"));
        assert_eq!(warns.len(), 1);
        assert!(StoredItem::from_toml(&toml::Value::from(3)).is_err());
        let zero: toml::Value =
            toml::from_str::<toml::Table>("x = { def = \"a\", count = 0 }").unwrap()["x"].clone();
        assert!(StoredItem::from_toml(&zero).is_err());
    }

    #[test]
    fn fingerprint_is_order_independent_and_sensitive() {
        let mut a = BTreeMap::new();
        a.insert("item_bow".to_string(), 1);
        a.insert("item_sword".to_string(), 2);
        let mut b = BTreeMap::new();
        b.insert("item_sword".to_string(), 2);
        b.insert("item_bow".to_string(), 1);
        assert_eq!(fingerprint(&a), fingerprint(&b));
        b.insert("item_bow".to_string(), 2);
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn into_parked_keeps_every_weapon() {
        let mut s = State::default();
        s.heroes[0].stored = Some(item("item_bow"));
        s.heroes[0].shield = Some("item_shield".into());
        s.heroes[2].stored = Some(item("item_sling"));
        let p = s.into_parked(&keys());
        assert!(p.heroes.iter().all(HeroSlot::is_empty));
        let parked: Vec<(&str, &str)> = p
            .parked
            .iter()
            .map(|p| (p.key.as_str(), p.item.def.as_str()))
            .collect();
        assert_eq!(
            parked,
            vec![("0:Ann", "item_bow"), ("slot 3", "item_sling")]
        );
        assert_ne!(identity(&keys(), "a"), identity(&keys(), "b"));
        let mut other = keys();
        other[1] = None;
        assert_ne!(identity(&keys(), "a"), identity(&other, "a"));
    }

    #[test]
    fn tally() {
        let mut s = State::default();
        s.heroes[0].stored = Some(item("item_bow"));
        s.parked.push(Parked {
            key: "x".into(),
            item: item("item_bow"),
        });
        let mut t = BTreeMap::new();
        tally_stored(&s, &mut t);
        assert_eq!(t["item_bow"], 2);
    }
}
