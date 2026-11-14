//! The sort order, independent of the game's memory (unit-tested).
//!
//! The game itself keeps the party inventory sorted: whenever the inventory
//! panel rebuilds its layout it sorts the entry array by (category, entry
//! sequence number), where the category comes from the item type (weapons,
//! armor, jewelry, potions, consumables, food, keys, other, recipes) and the
//! sequence number is an insertion counter. So the order inside a category
//! is "oldest first", and the mod changes that order by renumbering the
//! sequence numbers. It also moves the entries themselves into the new
//! order, so the array, the numbers and the panel agree even before the
//! panel next rebuilds (the save writes the array in its current order).

use std::cmp::Ordering;

/// One inventory entry as the sort sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The game's `ItemType` (item + 0x100).
    pub item_type: i32,
    /// Sell price (item params `price`).
    pub price: i32,
    /// Item definition name (internal id, not the translated name).
    pub name: String,
    /// Stack size.
    pub count: i32,
    /// The entry's current sequence number (keeps the sort stable).
    pub seq: i32,
}

/// The game's inventory category for an item type: the panel's section
/// order (0 weapons, 1 armor and shields, 2 jewelry, 3 potions,
/// 4 consumables, 5 food, 6 keys, 7 everything else, 8 recipes).
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

/// The mod's order: category (the game's, so the panel's sections stay as
/// they are), then item type inside the category (all helmets together,
/// all rings together), then price high to low, then name, then the
/// bigger stack first, then the old order.
pub fn compare(a: &Entry, b: &Entry) -> Ordering {
    category(a.item_type)
        .cmp(&category(b.item_type))
        .then(a.item_type.cmp(&b.item_type))
        .then(b.price.cmp(&a.price))
        .then_with(|| a.name.cmp(&b.name))
        .then(b.count.cmp(&a.count))
        .then(a.seq.cmp(&b.seq))
}

/// Indices of `entries` in the sorted order.
pub fn order(entries: &[Entry]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..entries.len()).collect();
    idx.sort_by(|&i, &j| compare(&entries[i], &entries[j]));
    idx
}

/// Whether `order` (from [`order`]) changes anything: entries already in
/// that order with sequence numbers already rising.
pub fn is_sorted(entries: &[Entry], order: &[usize]) -> bool {
    order.iter().enumerate().all(|(i, &j)| i == j)
        && entries.windows(2).all(|w| w[0].seq < w[1].seq)
}

/// Pairs (keep, merge) of stackable entries with the same definition name:
/// the second of each pair is to be merged into the first. `stackable[i]`
/// says whether entry `i` stacks.
pub fn duplicate_stacks(entries: &[Entry], stackable: &[bool]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for j in 0..entries.len() {
        if !stackable[j] {
            continue;
        }
        if let Some(i) = (0..j).find(|&i| stackable[i] && entries[i].name == entries[j].name) {
            out.push((i, j));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(item_type: i32, price: i32, name: &str, seq: i32) -> Entry {
        Entry {
            item_type,
            price,
            name: name.into(),
            count: 1,
            seq,
        }
    }

    #[test]
    fn categories_follow_the_game() {
        assert_eq!(category(1), 0); // weapon
        for t in [2, 5, 7, 8, 9, 10] {
            assert_eq!(category(t), 1);
        }
        for t in [3, 4, 6] {
            assert_eq!(category(t), 2);
        }
        assert_eq!(category(11), 3);
        assert_eq!(category(17), 4);
        assert_eq!(category(12), 5);
        assert_eq!(category(18), 6);
        assert_eq!(category(19), 8);
        for t in [0, 13, 14, 15, 16, 99, -1] {
            assert_eq!(category(t), 7);
        }
    }

    #[test]
    fn sorts_by_category_type_price_name() {
        let v = vec![
            e(12, 5, "bread", 0),     // food
            e(9, 10, "boots_b", 1),   // armor: boots
            e(1, 3, "dagger", 2),     // weapon
            e(7, 10, "helmet", 3),    // armor: helmet
            e(1, 30, "sword", 4),     // weapon, pricier
            e(9, 10, "boots_a", 5),   // armor: boots, same price
            e(5, 50, "chainmail", 6), // armor: body
        ];
        let o = order(&v);
        let names: Vec<&str> = o.iter().map(|&i| v[i].name.as_str()).collect();
        assert_eq!(
            names,
            [
                "sword",
                "dagger",
                "chainmail",
                "helmet",
                "boots_a",
                "boots_b",
                "bread"
            ]
        );
        assert!(!is_sorted(&v, &o));
    }

    #[test]
    fn stable_for_equal_items() {
        let v = vec![e(11, 5, "potion", 7), e(11, 5, "potion", 3)];
        assert_eq!(order(&v), [1, 0]);
        let sorted = vec![e(11, 5, "potion", 0), e(11, 5, "potion", 1)];
        assert!(is_sorted(&sorted, &order(&sorted)));
    }

    #[test]
    fn already_sorted_needs_rising_seq() {
        let v = vec![e(1, 30, "sword", 5), e(1, 3, "dagger", 5)];
        assert_eq!(order(&v), [0, 1]);
        assert!(!is_sorted(&v, &[0, 1]));
    }

    #[test]
    fn finds_duplicate_stacks() {
        let v = vec![
            e(16, 1, "bone", 0),
            e(1, 3, "dagger", 1),
            e(16, 1, "bone", 2),
            e(1, 3, "dagger", 3),
            e(16, 1, "bone", 4),
        ];
        let stack = [true, false, true, false, true];
        assert_eq!(duplicate_stacks(&v, &stack), [(0, 2), (0, 4)]);
    }
}
