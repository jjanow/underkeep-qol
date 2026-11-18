//! The merchant list's order, independent of the game's memory (unit-tested).
//!
//! The game keeps every item list sorted by (category, entry sequence
//! number): whenever a list's layout is rebuilt, its inventory entries are
//! sorted in place with that comparator. The sequence number is an insertion
//! counter, so inside a category the merchant's items show in stock order.
//! The mod renumbers the merchant entries' sequence numbers just before the
//! game sorts them, so the game's own sort produces this order.

use std::cmp::Ordering;

/// Item type of weapons (`ItemType` 1), the only items with a weapon kind.
pub const ITEM_TYPE_WEAPON: i32 = 1;

/// One merchant entry as the sort sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The game's `ItemType` (item + 0x100).
    pub item_type: i32,
    /// The weapon kind (item params `weapon_type`) for weapons, else 0.
    pub weapon_type: i32,
    /// What the merchant asks for it (item params `buy_price`).
    pub buy_price: i32,
    /// Item definition name (internal id, not the translated name).
    pub name: String,
    /// The entry's current sequence number (keeps the sort stable).
    pub seq: i32,
}

/// The game's list category for an item type (the comparator's first key,
/// 0x1400ed4f0): weapons, armor, jewelry, potions, consumables, food, keys,
/// other, recipes. Same mapping as inventory-sort's.
pub fn category(item_type: i32) -> i32 {
    match item_type {
        1 => 0,
        2 | 5 | 7 | 8 | 9 | 10 => 1,
        3 | 4 | 6 => 2,
        11 => 3,
        17 => 4,
        12 => 5,
        18 => 6,
        19 => 8,
        _ => 7,
    }
}

/// The mod's order: category (the game's, so the list's sections stay as
/// they are), then item type inside the category (shields, body armor,
/// helmets, ... each together), then weapon kind (swords together, axes
/// together, ...), then price, cheapest first, then name, then the old
/// order.
pub fn compare(a: &Entry, b: &Entry) -> Ordering {
    category(a.item_type)
        .cmp(&category(b.item_type))
        .then(a.item_type.cmp(&b.item_type))
        .then(a.weapon_type.cmp(&b.weapon_type))
        .then(a.buy_price.cmp(&b.buy_price))
        .then_with(|| a.name.cmp(&b.name))
        .then(a.seq.cmp(&b.seq))
}

/// For each entry (by index), its new sequence number: its rank in the
/// mod's order. Every number in 0..n is used once, so the game's sort (not
/// necessarily stable) has no ties to break.
pub fn new_seqs(entries: &[Entry]) -> Vec<i32> {
    let mut idx: Vec<usize> = (0..entries.len()).collect();
    idx.sort_by(|&a, &b| compare(&entries[a], &entries[b]));
    let mut seqs = vec![0; entries.len()];
    for (rank, &i) in idx.iter().enumerate() {
        seqs[i] = rank as i32;
    }
    seqs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(item_type: i32, weapon_type: i32, buy_price: i32, name: &str, seq: i32) -> Entry {
        Entry {
            item_type,
            weapon_type,
            buy_price,
            name: name.into(),
            seq,
        }
    }

    /// The names in the order the game would show them after the renumbering
    /// (it sorts by category, then seq).
    fn shown(entries: &[Entry]) -> Vec<String> {
        let seqs = new_seqs(entries);
        let mut idx: Vec<usize> = (0..entries.len()).collect();
        idx.sort_by_key(|&i| (category(entries[i].item_type), seqs[i]));
        idx.iter().map(|&i| entries[i].name.clone()).collect()
    }

    #[test]
    fn weapons_by_kind_then_cheapest_first() {
        let stock = [
            e(1, 2, 90, "axe_big", 0),
            e(1, 0, 60, "sword_long", 1),
            e(1, 2, 30, "axe_small", 2),
            e(1, 0, 20, "sword_short", 3),
            e(1, 5, 50, "bow", 4),
        ];
        assert_eq!(
            shown(&stock),
            ["sword_short", "sword_long", "axe_small", "axe_big", "bow"]
        );
    }

    #[test]
    fn armor_by_slot_then_cheapest_first() {
        let stock = [
            e(9, 0, 40, "boots_iron", 0),
            e(7, 0, 50, "helmet_iron", 1),
            e(5, 0, 80, "armor_chain", 2),
            e(9, 0, 10, "boots_leather", 3),
            e(7, 0, 15, "helmet_leather", 4),
            e(2, 0, 25, "shield_wood", 5),
        ];
        assert_eq!(
            shown(&stock),
            [
                "shield_wood",
                "armor_chain",
                "helmet_leather",
                "helmet_iron",
                "boots_leather",
                "boots_iron",
            ]
        );
    }

    #[test]
    fn categories_keep_the_game_order() {
        // Potion before the weapon in stock; recipes always last.
        let stock = [
            e(19, 0, 5, "recipe", 0),
            e(11, 0, 5, "potion", 1),
            e(5, 0, 5, "armor", 2),
            e(1, 3, 5, "mace", 3),
        ];
        assert_eq!(shown(&stock), ["mace", "armor", "potion", "recipe"]);
    }

    #[test]
    fn same_price_by_name_then_stock_order() {
        let stock = [
            e(11, 0, 10, "potion_b", 0),
            e(11, 0, 10, "potion_a", 1),
            e(11, 0, 10, "potion_a", 2),
        ];
        let seqs = new_seqs(&stock);
        assert_eq!(seqs, [2, 0, 1]);
    }

    #[test]
    fn seqs_are_a_permutation() {
        let stock: Vec<Entry> = (0..20)
            .map(|i| e(i % 7, i % 3, 100 - i, &format!("x{}", i % 4), i))
            .collect();
        let mut s = new_seqs(&stock);
        s.sort();
        assert_eq!(s, (0..20).collect::<Vec<i32>>());
    }

    #[test]
    fn empty() {
        assert!(new_seqs(&[]).is_empty());
    }
}
