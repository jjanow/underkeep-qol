//! `weapon-swap`: a second weapon slot per hero and keys that swap it with
//! the weapon in hand (README, "Mod designs").
//!
//! - The slot holds one weapon of any kind, kept by the mod (serialized: def
//!   name, count, cooldown) and saved in the save's sidecar; after a swap it
//!   holds the weapon that came out of the hand, so every swap goes both
//!   ways (sword ⇄ bow, sword and shield ⇄ two-hander, melee ⇄ melee).
//! - A swap creates the stored weapon and equips it through the game's own
//!   `Hero::EquipItem` (as from the inventory panel: class checks, on_equip /
//!   on_unequip scripts, stats, HUD). The weapon that was in hand lands in
//!   the inventory as with a manual equip; the mod then takes it out of the
//!   game into the slot. A two-handed weapon pushes the shield into the
//!   inventory, like a manual equip; swapping back to a one-handed weapon
//!   puts that shield back on if it is still in the inventory. So no second
//!   shield slot is needed: the shield only moves for a two-hander.
//! - In combat (the game's own "battle" check) a swap ends the hero's turn
//!   by default (`swap_costs_turn`), by queueing the TurnManager's round
//!   action 3, whose handler marks that hero as done and moves on (no game
//!   code was found that queues it itself; tested in a fight: the round
//!   goes on normally). It needs the hero to be able to act and the
//!   monsters not to be moving. Outside combat it's free.
//! - The character sheet shows the slot left of the helmet slot (an overlay
//!   widget placed from the sheet's own slot rects, drawn with the game's
//!   own slot stone, highlights and weapon-slot sword and a "II" mark:
//!   `slot`). It works like the game's slots, which pick an item up with a
//!   click and drop it with the next: clicking it with a weapon on the
//!   cursor (from the inventory or a hand) puts the weapon in, and so does
//!   a held drag released over it (the game itself cancels those: it picks
//!   an item up only when the button comes up over that item, so the mod
//!   remembers what the button went down on); clicking it while it holds a weapon picks that
//!   up (the mod's own pick-up: the weapon isn't in the game), and the next
//!   click drops it on the inventory panel or the weapon slot (a swap);
//!   anywhere else puts it back. A right click moves it to the inventory.
//!   With a gamepad there is no mouse cursor: the mod's section of the
//!   overlay menu has buttons for the same. On the HUD, a hero with a
//!   filled slot gets a small button on their weapon hand slot; clicking it
//!   swaps.
//! - The game counts item cooldowns down once per turn and once per rest
//!   step; a weapon in slot II counts down with them, like one in the
//!   inventory (`cooldown`).
//! - Heroes switching places in the party take their slot II with them.
//! - Keys and buttons only queue an action; `on_frame` runs it on the game
//!   thread between frames. Every action logs the item counts of the party
//!   plus the slots before and after: they must stay equal.

mod cooldown;
mod game;
mod quest;
mod slot;
mod state;
mod tooltip;

use std::collections::BTreeMap;

use uk_qol_runtime::egui;
use uk_qol_runtime::host::{FrameCtx, LoadCtx, Mod, ModSaveState};
use uk_qol_runtime::input::InputEvent;
use uk_qol_runtime::{hotkey, hud};

use game::{
    Game, KIND_EQUIPPED, KIND_INVENTORY, Readiness, SLOT_SHIELD, SLOT_WEAPON, UI_SHEET, busy_reason,
};
use state::{HEROES, Loaded, Parked, State, StoredItem};

const ID: &str = "weapon-swap";

/// The slot's tooltip line about losing its weapon (the full warning is in
/// the README).
const LOSS_HINT: &str = "Kept by the mod, not the game: move it to the inventory before \
                         uninstalling.";

const HERO_KEYS: [&str; HEROES] = ["swap_hero_1", "swap_hero_2", "swap_hero_3", "swap_hero_4"];
const SELECTED_KEY: &str = "swap_selected";
/// The HUD swap button (the stored weapon's initials): a square of this
/// many GUI units in the weapon hand slot's top-left corner (the slot is 29
/// units).
const HUD_BUTTON: f32 = 11.0;

/// The HUD swap button's rect in GUI space for a weapon slot rect.
fn hud_button_rect(slot: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(slot.min, egui::vec2(HUD_BUTTON, HUD_BUTTON))
}

/// A short label for an item def name: `item_short_bow` -> `SB`,
/// `item_sling` -> `Sl`.
fn hud_label(def: &str) -> String {
    let name = def.strip_prefix("item_").unwrap_or(def);
    let words: Vec<&str> = name.split('_').filter(|w| !w.is_empty()).collect();
    match words.as_slice() {
        [] => "?".into(),
        [one] => {
            let mut c = one.chars();
            c.next()
                .into_iter()
                .flat_map(char::to_uppercase)
                .chain(c.next())
                .collect()
        }
        [.., a, b] => [a, b]
            .iter()
            .filter_map(|w| w.chars().next())
            .flat_map(char::to_uppercase)
            .collect(),
    }
}

fn bool_setting(settings: &toml::Table, key: &str) -> bool {
    settings
        .get(key)
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
}

/// `Inner::refocus` value: the first button of the mod's section.
const FOCUS_FIRST: usize = usize::MAX;

/// Messages kept for the menu.
const MAX_MESSAGES: usize = 6;

#[derive(Debug, Clone, PartialEq)]
enum Action {
    /// Swap hero `i` (from a key or the menu).
    Swap(usize),
    /// Swap the hero selected in the game.
    SwapSelected,
    /// Put the weapon in hero `i`'s hand into their slot.
    StoreEquipped(usize),
    /// Put inventory item (pointer, def) into hero `i`'s slot.
    StoreFromInventory(usize, usize, String),
    /// Move hero `i`'s stored weapon to the inventory.
    ToInventory(usize),
    /// Move parked weapon `k` to the inventory.
    ParkedToInventory(usize),
    /// Put the item the game holds on the cursor (dropped on the sheet's
    /// second weapon slot of hero `hero`) into that slot.
    DropCarried {
        hero: usize,
        item: usize,
        def: String,
        kind: i32,
        src_hero: i32,
        src_slot: i32,
        /// Released over the slot after a held drag: the game cancelled its
        /// click (nothing is on the cursor any more), so there is no drag
        /// to end; the item is checked where it was pressed instead.
        held: bool,
    },
}

/// A weapon the button went down on (the game's click not yet a carry),
/// for a held drag released over the second slot.
#[derive(Clone)]
struct HeldPress {
    id: [u64; 2],
    item: usize,
    def: String,
    fits: bool,
    kind: i32,
    src_hero: i32,
    src_slot: i32,
}

struct Inner {
    game: Game,
    state: State,
    queue: Vec<Action>,
    costs_turn: bool,
    messages: Vec<String>,
    /// Setting `messages`: which of them are toasts too.
    toasts: Toasts,
    /// For the menu: whether the last loaded sidecar didn't match its save.
    mismatch: bool,
    /// For the menu: focus this hero's first button (after a click), or
    /// with [`FOCUS_FIRST`] the section's first button.
    refocus: Option<usize>,
    /// The party's [`state::identity`] when the mod was turned off.
    off_identity: Option<String>,
    /// Last (in battle, turn phase) seen, to log changes.
    seen: Option<(bool, i32)>,
    /// The `hud_button` setting.
    hud_button: bool,
    /// The `sheet_slot` setting.
    sheet_slot: bool,
    /// The hero whose second-slot weapon the mod has on the cursor (picked
    /// up from the sheet's second slot; it stays in the slot until dropped).
    carry: Option<usize>,
    /// Logged the sheet's slot positions (once per session).
    sheet_logged: bool,
    /// Logged that the game's weapon-slot sprite couldn't be read.
    sprite_missing_logged: bool,
    /// Last item seen on the game's cursor on the sheet, to log changes.
    seen_carried: Option<usize>,
    /// The item under a held button (see [`HeldPress`]), `None` inside if
    /// it isn't one the slot can take.
    press: Option<Option<HeldPress>>,
    /// The party's hero keys as last seen, to follow heroes that switch
    /// places (the slots are kept by place).
    last_keys: [Option<String>; HEROES],
}

#[derive(Default)]
pub struct WeaponSwap {
    inner: Option<Inner>,
}

fn log(msg: &str) {
    uk_qol_runtime::log(&format!("{ID}: {msg}"));
}

/// Which weapon-swap messages are also shown over the game (setting
/// `messages`). The mod menu and the log always get all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toasts {
    All,
    Problems,
    None,
}

fn toasts_setting(settings: &toml::Table) -> Toasts {
    match settings.get("messages").and_then(toml::Value::as_str) {
        Some("problems") => Toasts::Problems,
        Some("none") => Toasts::None,
        _ => Toasts::All,
    }
}

fn costs_turn(settings: &toml::Table) -> bool {
    settings
        .get("swap_costs_turn")
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
}

impl Inner {
    /// A refusal, a problem or a notice the player didn't ask for.
    fn say(&mut self, msg: String) {
        self.tell(msg, false);
    }

    /// `done`: the report of an action that worked ("X swapped ...").
    fn tell(&mut self, msg: String, done: bool) {
        log(&msg);
        // Over the game too (key presses happen with the menu closed),
        // unless the player turned that off.
        let toast = match self.toasts {
            Toasts::All => true,
            Toasts::Problems => !done,
            Toasts::None => false,
        };
        if toast {
            uk_qol_runtime::toast::show(format!("Weapon swap: {msg}"));
        }
        self.messages.push(msg);
        if self.messages.len() > MAX_MESSAGES {
            self.messages.remove(0);
        }
    }

    /// Party items plus the slots: def → count.
    fn total(&self) -> BTreeMap<String, i64> {
        let mut t = self.game.tally();
        state::tally_stored(&self.state, &mut t);
        t
    }

    /// Clears the slots whose weapon a quest script needed (the detour
    /// already put it in the inventory) and says so.
    fn take_returned(&mut self) {
        for (h, s) in quest::drain() {
            if self.state.heroes[h].stored.as_ref() == Some(&s) {
                self.state.heroes[h].stored = None;
                let who = self.game.hero_name(h);
                self.say(format!(
                    "{who}: {} moved from the second weapon slot to the inventory (a quest \
                     asked for it)",
                    s.def
                ));
            } else {
                log(&format!(
                    "{} went to the inventory for a script, but hero {}'s slot changed meanwhile",
                    s.def,
                    h + 1
                ));
            }
        }
    }

    /// Hands the slots to the quest detours.
    fn mirror(&self) {
        quest::mirror(std::array::from_fn(|h| self.state.heroes[h].stored.clone()));
    }

    fn keys(&self) -> [Option<String>; HEROES] {
        std::array::from_fn(|i| self.game.hero_key(i))
    }

    /// Keeps the slots with their heroes when heroes switch places, and
    /// counts the stored weapons' cooldowns down with the game's turns.
    fn follow_game(&mut self) {
        let now = self.keys();
        if let Some(from) = state::reordered_heroes(&self.last_keys, &now) {
            self.state.reorder(&from);
            // Both hold hero places.
            self.carry = None;
            self.press = None;
            log(&format!(
                "heroes switched places ({from:?}); second slots follow them"
            ));
        }
        self.last_keys = now;
        let n = cooldown::take();
        if n > 0 {
            self.state.tick_cooldowns(n);
        }
    }

    fn fingerprint(&self) -> String {
        state::fingerprint(&self.game.tally())
    }

    fn run(&mut self, a: Action) {
        if let Action::DropCarried {
            item, held: false, ..
        } = &a
        {
            // The game's drop targets end the drag before acting on it.
            match self.game.carried() {
                Some(c) if c.item as usize == *item => self.game.end_drag(),
                _ => {
                    self.say(format!("{}: it's no longer on the cursor", describe(&a)));
                    return;
                }
            }
        }
        let r = self.game.readiness();
        if let Some(why) = busy_reason(&r) {
            self.say(format!("{}: not now, {why} ({})", describe(&a), raw(&r)));
            return;
        }
        log(&format!(
            "{} ({}, battle {})",
            describe(&a),
            raw(&r),
            self.game.in_battle()
        ));
        let before = self.total();
        let result = self.apply(&a, &r);
        let after = self.total();
        match result {
            Ok(msg) => self.tell(msg, true),
            Err(e) => self.say(format!("{}: {e}", describe(&a))),
        }
        if before == after {
            log(&format!("items conserved ({} kinds)", after.len()));
        } else {
            let mut diff = Vec::new();
            for k in before.keys().chain(after.keys()) {
                let (b, a) = (
                    before.get(k).copied().unwrap_or(0),
                    after.get(k).copied().unwrap_or(0),
                );
                if b != a
                    && !diff
                        .iter()
                        .any(|d: &String| d.starts_with(&format!("{k}:")))
                {
                    diff.push(format!("{k}: {b} -> {a}"));
                }
            }
            log(&format!("ITEM COUNTS CHANGED: {}", diff.join(", ")));
        }
    }

    fn apply(&mut self, a: &Action, r: &Readiness) -> Result<String, String> {
        match a {
            Action::Swap(i) => self.swap(*i, r),
            Action::SwapSelected => {
                let i = self.game.selected_hero().ok_or("no hero selected")?;
                self.swap(i, r)
            }
            Action::StoreEquipped(i) => {
                self.need_empty(*i)?;
                let s = self.game.store_equipped(*i)?;
                let msg = format!(
                    "{}: {} is now in the second weapon slot",
                    self.game.hero_name(*i),
                    s.def
                );
                self.state.heroes[*i].stored = Some(s);
                Ok(msg)
            }
            Action::StoreFromInventory(i, ptr, def) => {
                self.need_empty(*i)?;
                let s = self.game.store_from_inventory(*ptr as game::Item, def)?;
                let msg = format!(
                    "{}: {} is now in the second weapon slot",
                    self.game.hero_name(*i),
                    s.def
                );
                self.state.heroes[*i].stored = Some(s);
                Ok(msg)
            }
            Action::ToInventory(i) => {
                let s = self.state.heroes[*i]
                    .stored
                    .clone()
                    .ok_or("the second weapon slot is empty")?;
                self.game.put_in_inventory(&s)?;
                self.state.heroes[*i].stored = None;
                Ok(format!(
                    "{}: {} moved to the inventory",
                    self.game.hero_name(*i),
                    s.def
                ))
            }
            Action::ParkedToInventory(k) => {
                let p = self.state.parked.get(*k).cloned().ok_or("already moved")?;
                self.game.put_in_inventory(&p.item)?;
                self.state.parked.remove(*k);
                Ok(format!("{} moved to the inventory", p.item.def))
            }
            Action::DropCarried {
                hero,
                item,
                def,
                kind,
                src_hero,
                src_slot,
                ..
            } => self.drop_carried(
                *hero,
                *item as game::Item,
                def,
                *kind,
                (*src_hero, *src_slot),
                r,
            ),
        }
    }

    /// Puts a weapon dropped on hero `h`'s second slot in it: from the
    /// inventory, or from a hero's hand (`src`: hero and slot; the game's
    /// unequip first). A weapon already in the slot goes to the inventory,
    /// as when the game equips over an item; the weapon from `h`'s own hand on
    /// a filled slot is a swap.
    fn drop_carried(
        &mut self,
        h: usize,
        item: game::Item,
        def: &str,
        kind: i32,
        src: (i32, i32),
        r: &Readiness,
    ) -> Result<String, String> {
        if h >= HEROES || !self.game.hero_valid(h) {
            return Err("no hero there".into());
        }
        let new = match kind {
            KIND_INVENTORY => self.game.store_from_inventory(item, def)?,
            KIND_EQUIPPED => {
                let from = usize::try_from(src.0)
                    .ok()
                    .filter(|&s| s < HEROES && src.1 == SLOT_WEAPON)
                    .ok_or_else(|| format!("{def} isn't the weapon in a hand"))?;
                if self.game.equipped(from, SLOT_WEAPON) != item {
                    return Err(format!("{def} is no longer in that hand"));
                }
                if from == h && self.state.heroes[h].stored.is_some() {
                    return self.swap(h, r);
                }
                self.game.store_equipped(from)?
            }
            _ => return Err(format!("put {def} in the inventory first")),
        };
        let name = self.game.hero_name(h);
        let mut msg = format!("{name}: {} is now in the second weapon slot", new.def);
        if let Some(old) = self.state.heroes[h].stored.take() {
            if let Err(e) = self.game.put_in_inventory(&old) {
                // Keep the old one; the new one goes back to the game.
                self.state.heroes[h].stored = Some(old);
                if self.game.put_in_inventory(&new).is_err() {
                    self.state.parked.push(Parked {
                        key: format!("slot {}", h + 1),
                        item: new,
                    });
                }
                return Err(format!("can't move the slot's weapon out: {e}"));
            }
            msg.push_str(&format!("; {} moved to the inventory", old.def));
        }
        self.state.heroes[h].stored = Some(new);
        Ok(msg)
    }

    fn need_empty(&self, i: usize) -> Result<(), String> {
        if !self.game.hero_valid(i) {
            return Err("no hero there".into());
        }
        if self.state.heroes[i].stored.is_some() {
            return Err(
                "the second weapon slot is taken; move its weapon to the inventory first".into(),
            );
        }
        Ok(())
    }

    fn swap(&mut self, i: usize, r: &Readiness) -> Result<String, String> {
        // A swap puts the weapon in hand into the slot.
        if i >= HEROES || !self.game.hero_valid(i) {
            return Err("no hero there".into());
        }
        let stored = self.state.heroes[i]
            .stored
            .clone()
            .ok_or("the second weapon slot is empty")?;
        let battle = self.game.in_battle();
        let charge = battle && self.costs_turn;
        if battle {
            if r.tm_phase != 0 {
                return Err("wait for the monsters' turn to end".into());
            }
            if charge && !self.game.hero_can_act(i) {
                return Err("this hero can't act any more this round".into());
            }
        }
        let res = self.game.swap(i, &stored)?;
        let name = self.game.hero_name(i);
        let slot = &mut self.state.heroes[i];
        slot.stored = res.out_of_hand.clone();
        let mut msg = format!(
            "{name}: swapped to {}{}",
            stored.def,
            res.out_of_hand
                .as_ref()
                .map(|o| format!("; {} is in the second weapon slot", o.def))
                .unwrap_or_default()
        );
        if let Some(w) = res.warning {
            log(&w);
        }
        if let Some(sh) = res.shield_out {
            msg.push_str(&format!("; {sh} went to the inventory"));
            slot.shield = Some(sh);
        } else if let Some(sh) = slot.shield.take()
            && self.game.reequip_shield(i, &sh)
        {
            msg.push_str(&format!("; {sh} back on"));
        }
        if charge {
            self.game.end_turn(i);
            msg.push_str(" (turn used)");
        }
        Ok(msg)
    }
}

fn describe(a: &Action) -> String {
    match a {
        Action::Swap(i) => format!("swap hero {}", i + 1),
        Action::SwapSelected => "swap selected hero".into(),
        Action::StoreEquipped(i) => format!("store hero {}'s weapon in hand", i + 1),
        Action::StoreFromInventory(i, _, d) => format!("put {d} in hero {}'s slot", i + 1),
        Action::ToInventory(i) => format!("move hero {}'s slot weapon to the inventory", i + 1),
        Action::ParkedToInventory(_) => "move a parked weapon to the inventory".into(),
        Action::DropCarried { hero, def, .. } => {
            format!("put {def} from the cursor in hero {}'s slot", hero + 1)
        }
    }
}

fn raw(r: &Readiness) -> String {
    format!(
        "state {} ui {} target {} drag {} turn {}/{}",
        r.game_state, r.ui_mode, r.target_mode, r.drag, r.tm_phase, r.tm_count
    )
}

impl Mod for WeaponSwap {
    fn id(&self) -> &'static str {
        ID
    }

    fn manifest(&self) -> &'static str {
        include_str!("../mod.toml")
    }

    fn patterns(&self) -> &'static str {
        include_str!("../patterns.toml")
    }

    fn on_load(&mut self, cx: &mut LoadCtx) -> Result<(), String> {
        let base = cx.base;
        let pats = cx.patterns;
        // SAFETY: addresses from this mod's verified patterns.
        let game = unsafe {
            Game::new(|n| {
                pats.get(n)
                    .map(|&rva| base + rva as usize)
                    .ok_or_else(|| format!("pattern {n} not resolved"))
            })
        }?;
        let at = |n: &str| {
            pats.get(n)
                .map(|&rva| base + rva as usize)
                .ok_or_else(|| format!("pattern {n} not resolved"))
        };
        // SAFETY: the resolved function starts of these two patterns.
        unsafe { quest::install(cx.hooks, at("script_condition")?, at("script_command")?) }?;
        // SAFETY: the resolved addresses of the tooltip patterns.
        unsafe { tooltip::install(cx.hooks, |n| at(n).ok()) };
        // SAFETY: the resolved starts of the two tick functions (or none).
        unsafe {
            cooldown::install(
                cx.hooks,
                at("party_round_tick").ok(),
                at("hero_rest_step").ok(),
                game.hero(0),
            )
        };
        // Turning the mod off and on again keeps the slots of this session,
        // unless the party changed meanwhile (e.g. another save was loaded
        // while the mod was off, which it isn't told about): then they
        // could belong to another save, so they're parked.
        let (mut state, off_identity) = match self.inner.take() {
            Some(i) => (i.state, i.off_identity),
            None => (State::default(), None),
        };
        let keys: [Option<String>; HEROES] = std::array::from_fn(|i| game.hero_key(i));
        let now = state::identity(&keys, &state::fingerprint(&game.tally()));
        let parked_on_load = !state.is_empty() && off_identity.as_deref() != Some(now.as_str());
        if parked_on_load {
            log("turned back on, but the party changed while off; second-slot weapons are parked");
            state = state.into_parked(&keys);
        }
        self.inner = Some(Inner {
            game,
            state,
            queue: Vec::new(),
            costs_turn: costs_turn(cx.settings),
            messages: Vec::new(),
            toasts: toasts_setting(cx.settings),
            mismatch: parked_on_load,
            refocus: None,
            off_identity: None,
            seen: None,
            hud_button: bool_setting(cx.settings, "hud_button"),
            sheet_slot: bool_setting(cx.settings, "sheet_slot"),
            carry: None,
            sheet_logged: false,
            sprite_missing_logged: false,
            seen_carried: None,
            press: None,
            last_keys: keys,
        });
        quest::drain();
        quest::set_game(Some(game));
        tooltip::set_game(Some(game));
        if let Some(i) = &self.inner {
            i.mirror();
        }
        Ok(())
    }

    fn on_unload(&mut self) {
        quest::set_game(None);
        tooltip::set_game(None);
        if let Some(i) = &mut self.inner {
            i.take_returned();
            let keys = i.keys();
            i.off_identity = Some(state::identity(&keys, &i.fingerprint()));
        }
        if let Some(i) = &self.inner
            && !i.state.is_empty()
        {
            log("turned off with weapons in second slots; they stay with the mod for this session");
        }
    }

    fn on_settings(&mut self, settings: &toml::Table) {
        if let Some(i) = &mut self.inner {
            i.costs_turn = costs_turn(settings);
            i.toasts = toasts_setting(settings);
            i.hud_button = bool_setting(settings, "hud_button");
            i.sheet_slot = bool_setting(settings, "sheet_slot");
            if !i.sheet_slot {
                i.carry = None;
            }
        }
    }

    fn on_input(&mut self, ev: &InputEvent) -> bool {
        let Some(inner) = &mut self.inner else {
            return false;
        };
        for (i, k) in HERO_KEYS.iter().enumerate() {
            if hotkey::pressed(ID, k, ev) {
                inner.queue.push(Action::Swap(i));
                return true;
            }
        }
        if hotkey::pressed(ID, SELECTED_KEY, ev) {
            inner.queue.push(Action::SwapSelected);
            return true;
        }
        false
    }

    fn on_frame(&mut self, _cx: &mut FrameCtx) {
        let Some(inner) = &mut self.inner else { return };
        inner.take_returned();
        // After take_returned: the quest detours' hero places are last
        // frame's.
        inner.follow_game();
        for a in std::mem::take(&mut inner.queue) {
            inner.run(a);
        }
        inner.mirror();
        // The mod's pick-up ends when the sheet closes or something covers
        // it (the weapon never left the slot).
        if inner.carry.is_some() && !(inner.game.ui_mode() == UI_SHEET && hud::shown()) {
            inner.carry = None;
        }
        // Log battle and turn-phase changes, for triage of the combat path.
        if inner.game.playing() {
            let r = inner.game.readiness();
            let now = (inner.game.in_battle(), r.tm_phase);
            if inner.seen != Some(now) {
                if inner.seen.is_some() {
                    log(&format!("battle {} ({})", now.0, raw(&r)));
                }
                inner.seen = Some(now);
            }
        }
    }

    fn on_save(&mut self, st: &mut ModSaveState) {
        let Some(inner) = &mut self.inner else { return };
        inner.take_returned();
        inner.follow_game();
        inner.mirror();
        *st = inner.state.to_save(&inner.keys(), &inner.fingerprint());
    }

    fn on_load_save(&mut self, st: &ModSaveState) {
        let Some(inner) = &mut self.inner else { return };
        // Weapons a script took out of the slots belonged to the game state
        // being replaced.
        quest::drain();
        // Turns of the game state being replaced.
        cooldown::take();
        inner.queue.clear();
        inner.carry = None;
        inner.press = None;
        let keys = inner.keys();
        inner.last_keys = keys.clone();
        let fp = inner.fingerprint();
        let mut warns = Vec::new();
        let loaded = State::from_save(st, &keys, &fp, &mut |w| warns.push(w));
        for w in warns {
            log(&w);
        }
        match loaded {
            Loaded::Ok(s) => {
                inner.mismatch = false;
                let n = s.heroes.iter().filter(|h| h.stored.is_some()).count();
                log(&format!(
                    "save loaded: {n} second weapon slot(s) filled, {} parked",
                    s.parked.len()
                ));
                inner.state = s;
            }
            Loaded::Mismatch(s) => {
                inner.mismatch = true;
                inner.say(format!(
                    "this save's mod file doesn't match the save (saved while the mod was off?); \
                     {} weapon(s) parked in the mod menu",
                    s.parked.len()
                ));
                inner.state = s;
            }
        }
        inner.mirror();
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        let Some(inner) = &mut self.inner else { return };
        menu(inner, ui);
    }

    fn has_hud(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| {
            (i.hud_button && i.state.heroes.iter().any(|h| h.stored.is_some()))
                || (i.sheet_slot && i.game.playing() && i.game.ui_mode() == UI_SHEET)
        })
    }

    fn hud(&mut self, ui: &mut egui::Ui) {
        let Some(inner) = &mut self.inner else { return };
        let Some(t) = hud::transform() else { return };
        if !inner.game.playing() {
            return;
        }
        if inner.hud_button {
            hand_buttons(inner, ui, &t);
        }
        sheet_slot(inner, ui, &t);
    }
}

/// The swap buttons on the HUD's weapon hand slots.
fn hand_buttons(inner: &mut Inner, ui: &mut egui::Ui, t: &hud::Transform) {
    for h in 0..HEROES {
        let Some(stored) = inner.state.heroes[h].stored.clone() else {
            continue;
        };
        if !inner.game.hero_valid(h) {
            continue;
        }
        let Some(slot) = hud::gui_rect(&hud::weapon_slot_name(h)) else {
            continue;
        };
        let r = hud::button(
            ui,
            t.rect_to_points(hud_button_rect(slot)),
            &hud_label(&stored.def),
            egui::Color32::from_rgb(60, 95, 50),
        )
        .on_hover_text(format!(
            "Weapon slot II: {}. Click: swap with the weapon in hand.",
            item_label(&stored)
        ));
        if r.clicked() {
            inner.queue.push(Action::Swap(h));
        }
    }
}

/// The second weapon slot's rect on the character sheet, in GUI space.
fn sheet_rect(game: &Game) -> Option<egui::Rect> {
    let slots: Vec<Option<egui::Rect>> = (0..game::SLOTS as i32)
        .map(|k| game.sheet_slot_rect(k).and_then(hud::rect_from_layout))
        .collect();
    slot::place(&slots)
}

/// Text centred in `rect` (points), sized to it, with a shadow.
fn slot_text(p: &egui::Painter, rect: egui::Rect, text: &str, color: egui::Color32) {
    let font = egui::FontId::proportional((rect.height() * 0.45).max(6.0));
    p.text(
        rect.center() + egui::vec2(1.0, 1.0),
        egui::Align2::CENTER_CENTER,
        text,
        font.clone(),
        egui::Color32::from_black_alpha(200),
    );
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        font,
        color,
    );
}

/// The second weapon slot on the character sheet (the selected hero's).
fn sheet_slot(inner: &mut Inner, ui: &mut egui::Ui, t: &hud::Transform) {
    let hero = inner
        .game
        .selected_hero()
        .filter(|&h| inner.game.hero_valid(h));
    let (Some(h), true, true) = (hero, inner.sheet_slot, inner.game.ui_mode() == UI_SHEET) else {
        inner.carry = None;
        inner.press = None;
        return;
    };
    if inner.carry.is_some_and(|c| c != h) {
        inner.carry = None;
    }
    let Some(gui) = sheet_rect(&inner.game) else {
        return;
    };
    if !inner.sheet_logged {
        inner.sheet_logged = true;
        log(&format!(
            "character sheet: helmet slot {:?}, weapon slot {:?}, second weapon slot {gui:?} (GUI space)",
            inner.game.sheet_slot_rect(slot::HELMET as i32),
            inner.game.sheet_slot_rect(SLOT_WEAPON)
        ));
    }
    let rect = t.rect_to_points(gui);
    let stored = inner.state.heroes[h].stored.clone();
    let carried = inner.game.carried();
    // SAFETY: the item the game holds on the cursor is live (it stays in
    // the inventory or on its hero while carried).
    let carried_info = carried.and_then(|c| unsafe { inner.game.item_info(c.item) });
    let seen = carried.map(|c| c.item as usize);
    if seen != inner.seen_carried {
        inner.seen_carried = seen;
        if let (Some(c), Some(i)) = (carried, &carried_info) {
            log(&format!(
                "on the cursor: {} (from {}, hero {} slot {})",
                i.def, c.kind, c.hero, c.slot
            ));
        }
    }
    let fits = carried_info
        .as_ref()
        .map(|i| i.weapon && i.slot_problem().is_none());

    // A held drag: the button went down on a weapon elsewhere and comes up
    // over this slot. The game cancels its click then (it picks an item up
    // only when the button comes up over that item), so the press is
    // remembered while it's held and turned into a drop here.
    let pointer = ui.ctx().input(|i| i.pointer.latest_pos());
    let over = pointer.is_some_and(|p| rect.contains(p));
    match inner.game.pressed() {
        Some(p) => {
            if inner
                .press
                .as_ref()
                .is_none_or(|k| k.as_ref().is_none_or(|k| k.id != p.id))
            {
                let at = pointer.map(|p| t.from_points(p));
                inner.press = Some(held_press(&inner.game, &p, at));
            }
        }
        None => {
            if let Some(Some(k)) = inner.press.take()
                && over
                && carried.is_none()
                && inner.carry.is_none()
            {
                if k.fits {
                    inner.queue.push(Action::DropCarried {
                        hero: h,
                        item: k.item,
                        def: k.def,
                        kind: k.kind,
                        src_hero: k.src_hero,
                        src_slot: k.src_slot,
                        held: true,
                    });
                } else {
                    inner.say(format!("{} doesn't fit the second weapon slot", k.def));
                }
            }
        }
    }
    let held_fits = match &inner.press {
        Some(Some(k)) if over => Some(k.fits),
        _ => None,
    };
    let fits = fits.or(held_fits);

    let r = ui.allocate_rect(rect, egui::Sense::click_and_drag());
    let ctx = ui.ctx().clone();
    {
        let p = ui.painter();
        let (outline, highlight) = match fits {
            Some(true) => (Some(slot::FITS), Some(slot::ALLOW_SPRITE)),
            Some(false) => (Some(slot::NO_FIT), Some(slot::DENY_SPRITE)),
            None if r.hovered() => (Some(slot::HOVER), Some(slot::SELECTED_SPRITE)),
            None => (None, None),
        };
        // The game's stone and highlight sprites; the mod's own drawing if
        // they can't be read.
        let stone = slot::paint_stone(&ctx, p, t, gui);
        if !stone {
            slot::paint_frame(p, t, gui, outline);
        }
        match &stored {
            Some(s) if inner.carry.is_none() => {
                // The weapon's own icon, as the game's slots show it; its
                // initials on a plain square if the icon can't be read.
                let room = t.rect_to_points(gui.shrink(2.0));
                let scale = t.points_per_unit();
                if !slot::paint_item(&ctx, p, room, scale, &s.def, egui::Color32::WHITE) {
                    let label = t.rect_to_points(gui.shrink(3.0));
                    p.rect_filled(label, 0.0, slot::STORED);
                    slot_text(p, label, &hud_label(&s.def), egui::Color32::WHITE);
                }
            }
            _ => {
                if !slot::paint_sword(&ctx, p, t, gui) && !inner.sprite_missing_logged {
                    inner.sprite_missing_logged = true;
                    log("the game's weapon-slot icon isn't available; drawing the slot without it");
                }
            }
        }
        slot::paint_numeral(p, t, gui);
        if stone
            && let (Some(c), Some(h)) = (outline, highlight)
            && !slot::paint_highlight(&ctx, p, t, gui, h)
        {
            let s = egui::Stroke::new(t.points_per_unit(), c);
            p.rect_stroke(rect, 0.0, s, egui::StrokeKind::Inside);
        }
    }
    // A weapon in the slot gets the game's own tooltip (with the slot's
    // hints appended), as the sheet's other slots do; the mod's own text
    // stays for the empty slot, a weapon that doesn't fit, and as the
    // fallback.
    let game_tooltip = tooltip::active()
        && fits.is_none()
        && carried.is_none()
        && inner.carry.is_none()
        && inner.press.as_ref().is_none_or(|p| p.is_none());
    if let (true, true, Some(s)) = (game_tooltip, r.hovered(), &stored) {
        tooltip::want(h, s);
    }
    let r = if game_tooltip && stored.is_some() {
        r
    } else {
        r.on_hover_text(match (&stored, fits) {
            (_, Some(false)) => "Weapon slot II: only weapons fit.".into(),
            (Some(s), _) => format!(
                "Weapon slot II: {}. Click to pick it up, then click the inventory or the weapon \
             slot (swap). Right-click: move it to the inventory.\n{LOSS_HINT}",
                item_label(s)
            ),
            (None, _) => format!(
                "Weapon slot II: put a spare weapon here (click it with a weapon on the cursor).\n\
             {LOSS_HINT}"
            ),
        })
    };

    if r.clicked() {
        match (carried, carried_info) {
            (Some(c), Some(info)) if fits == Some(true) => {
                inner.queue.push(Action::DropCarried {
                    hero: h,
                    item: c.item as usize,
                    def: info.def,
                    kind: c.kind,
                    src_hero: c.hero,
                    src_slot: c.slot,
                    held: false,
                });
            }
            (Some(_), Some(info)) => {
                inner.say(format!("{} doesn't fit the second weapon slot", info.def));
            }
            (Some(_), None) => {}
            (None, _) if stored.is_some() => {
                inner.carry = if inner.carry.is_some() { None } else { Some(h) };
            }
            (None, _) => {}
        }
    } else if r.secondary_clicked() && carried.is_none() && stored.is_some() {
        inner.carry = None;
        inner.queue.push(Action::ToInventory(h));
    } else if r.drag_started() && carried.is_none() && stored.is_some() {
        inner.carry = Some(h);
    } else if r.drag_stopped() && inner.carry.is_some() {
        // Pressed on the slot, released elsewhere: drop there.
        if let Some(p) = ui.ctx().input(|i| i.pointer.latest_pos())
            && !rect.contains(p)
        {
            drop_at(inner, h, t.from_points(p));
        }
        inner.carry = None;
    }

    // The mod's pick-up: the weapon's label follows the pointer, and the
    // next click anywhere is the mod's (so it can't reach the game).
    if inner.carry == Some(h) && !r.dragged() {
        let screen = ui.clip_rect();
        let catch = ui.interact(
            screen,
            egui::Id::unique("weapon-swap-carry"),
            egui::Sense::click(),
        );
        if catch.clicked() {
            if let Some(p) = catch.interact_pointer_pos() {
                drop_at(inner, h, t.from_points(p));
            }
            inner.carry = None;
        } else if catch.secondary_clicked() {
            inner.carry = None;
        }
    }
    if inner.carry == Some(h)
        && let (Some(s), Some(p)) = (&stored, ui.ctx().input(|i| i.pointer.latest_pos()))
    {
        let ghost = egui::Rect::from_center_size(p, rect.size());
        let painter = ui.painter();
        let ctx = ui.ctx().clone();
        let tint = egui::Color32::WHITE.gamma_multiply(0.85);
        if !slot::paint_item(&ctx, painter, ghost, t.points_per_unit(), &s.def, tint) {
            let ghost = ghost.scale_from_center(0.8);
            painter.rect_filled(ghost, 0.0, slot::STORED.gamma_multiply(0.85));
            slot_text(painter, ghost, &hud_label(&s.def), egui::Color32::WHITE);
        }
    }
}

/// What the button went down on (`p`, at GUI point `at` when first seen),
/// if it's an item the second slot could take a held drag of: an inventory
/// item, or a hero's weapon (from the HUD hand slot, or the sheet's weapon
/// or shield slot showing a two-handed weapon). `None` for anything else.
fn held_press(game: &Game, p: &game::Pressed, at: Option<egui::Pos2>) -> Option<HeldPress> {
    let (item, src_hero, src_slot) = match p.kind {
        KIND_INVENTORY => (game.pressed_inventory_item(p)?, -1, -1),
        KIND_EQUIPPED => {
            let on = |k: i32| {
                at.is_some_and(|at| {
                    game.sheet_slot_rect(k)
                        .and_then(hud::rect_from_layout)
                        .is_some_and(|r| r.contains(at))
                })
            };
            let hero = match Game::pressed_hand_slot(p) {
                Some(h) => h,
                None if on(SLOT_WEAPON) => game.selected_hero()?,
                None if on(SLOT_SHIELD) => {
                    let h = game.selected_hero()?;
                    if !game.equipped(h, SLOT_SHIELD).is_null() {
                        return None;
                    }
                    h
                }
                None => return None,
            };
            (game.equipped(hero, SLOT_WEAPON), hero as i32, SLOT_WEAPON)
        }
        _ => return None,
    };
    // SAFETY: on a hero or in the inventory (checked above).
    let info = unsafe { game.item_info(item) }?;
    let fits = info.weapon && info.slot_problem().is_none();
    log(&format!(
        "held on: {} (from {}, hero {src_hero}), fits {fits}",
        info.def, p.kind
    ));
    Some(HeldPress {
        id: p.id,
        item: item as usize,
        def: info.def,
        fits,
        kind: p.kind,
        src_hero,
        src_slot,
    })
}

/// Drops the mod's pick-up of hero `h`'s second-slot weapon at GUI point
/// `p`: on the inventory panel it moves there, on the weapon slot (the
/// sheet's or the HUD's hand slot) it swaps; anywhere else nothing happens.
fn drop_at(inner: &mut Inner, h: usize, p: egui::Pos2) {
    let on = |r: Option<egui::Rect>| r.is_some_and(|r| r.contains(p));
    if on(hud::gui_rect("inventory_rect")) {
        inner.queue.push(Action::ToInventory(h));
    } else if on(inner
        .game
        .sheet_slot_rect(SLOT_WEAPON)
        .and_then(hud::rect_from_layout))
        || on(hud::gui_rect(&hud::weapon_slot_name(h)))
    {
        inner.queue.push(Action::Swap(h));
    }
}

const WARN: egui::Color32 = egui::Color32::from_rgb(230, 180, 80);

fn menu(inner: &mut Inner, ui: &mut egui::Ui) {
    ui.add_space(6.0);
    for m in &inner.messages {
        ui.weak(m);
    }
    if !inner.game.playing() {
        ui.label("Load a game to use the second weapon slots.");
        return;
    }
    if !inner.state.parked.is_empty() {
        ui.separator();
        ui.colored_label(
            WARN,
            if inner.mismatch {
                "Weapons from this save's mod file that don't match the save. They may \
                 already be in your party: check before moving them to the inventory."
            } else {
                "Weapons from second weapon slots of heroes not in the party:"
            },
        );
        for (k, p) in inner.state.parked.iter().enumerate() {
            ui.label(format!("{} ({})", item_label(&p.item), p.key));
            let r = ui.button("Move to inventory");
            if inner.refocus == Some(FOCUS_FIRST) {
                r.request_focus();
                inner.refocus = None;
            }
            if r.clicked() {
                inner.queue.push(Action::ParkedToInventory(k));
                inner.refocus = Some(FOCUS_FIRST);
            }
        }
    }
    let inventory: Vec<game::ItemInfo> = inner
        .game
        .inventory()
        .into_iter()
        // SAFETY: inventory items are live.
        .filter_map(|it| unsafe { inner.game.item_info(it) })
        .filter(|i| i.weapon && i.slot_problem().is_none())
        .collect();
    for h in 0..HEROES {
        if !inner.game.hero_valid(h) {
            continue;
        }
        let hand = inner.game.equipped(h, SLOT_WEAPON);
        // SAFETY: the equipped weapon.
        let hand = unsafe { inner.game.item_info(hand) };
        let stored = inner.state.heroes[h].stored.clone();
        ui.horizontal_wrapped(|ui| {
            ui.strong(format!("{}. {}", h + 1, inner.game.hero_name(h)));
            ui.label(format!(
                "hand: {}  |  weapon slot II: {}",
                hand.as_ref().map_or("nothing".into(), |i| i.def.clone()),
                stored.as_ref().map_or("empty".into(), item_label)
            ));
        });
        // Buttons as (label, action), one per row: the d-pad moves between
        // rows reliably, not between buttons of one row. A click changes
        // which buttons exist, so the focus moves to this hero's first
        // button next frame.
        let mut buttons: Vec<(String, Action)> = Vec::new();
        match &stored {
            Some(_) => {
                buttons.push(("Swap now".into(), Action::Swap(h)));
                buttons.push(("Move to inventory".into(), Action::ToInventory(h)));
            }
            None => {
                if hand.as_ref().is_some_and(|i| i.weapon) {
                    buttons.push((
                        "Put the weapon in hand in the slot".into(),
                        Action::StoreEquipped(h),
                    ));
                }
                for b in &inventory {
                    buttons.push((
                        format!("Put {} in the slot", b.def),
                        Action::StoreFromInventory(h, b.ptr, b.def.clone()),
                    ));
                }
            }
        }
        ui.vertical(|ui| {
            for (k, (label, action)) in buttons.into_iter().enumerate() {
                let r = ui.button(label);
                if k == 0 && (inner.refocus == Some(h) || inner.refocus == Some(FOCUS_FIRST)) {
                    r.request_focus();
                    inner.refocus = None;
                }
                if r.clicked() {
                    inner.queue.push(action);
                    inner.refocus = Some(h);
                }
            }
        });
    }
}

fn item_label(s: &StoredItem) -> String {
    if s.count > 1 {
        format!("{} x{}", s.def, s.count)
    } else {
        s.def.clone()
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(WeaponSwap::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uk_qol_runtime::host::{Manifest, PatternFile};

    #[test]
    fn toasts_choice() {
        let t = |v: &str| {
            let mut m = toml::Table::new();
            m.insert("messages".into(), toml::Value::String(v.into()));
            toasts_setting(&m)
        };
        assert_eq!(t("all"), Toasts::All);
        assert_eq!(t("problems"), Toasts::Problems);
        assert_eq!(t("none"), Toasts::None);
        assert_eq!(t("bogus"), Toasts::All);
        assert_eq!(toasts_setting(&toml::Table::new()), Toasts::All);
    }

    #[test]
    fn hud_labels() {
        assert_eq!(hud_label("item_sling"), "Sl");
        assert_eq!(hud_label("item_short_bow"), "SB");
        assert_eq!(hud_label("item_heavy_long_bow"), "LB");
        assert_eq!(hud_label("item_"), "?");
        let slot = egui::Rect::from_min_size(egui::pos2(500.0, 200.0), egui::vec2(29.0, 29.0));
        assert!(slot.contains_rect(hud_button_rect(slot)));
    }

    #[test]
    fn manifest_and_patterns_parse() {
        let m = new();
        let manifest = Manifest::parse(m.manifest()).unwrap();
        assert_eq!(manifest.id, m.id());
        for k in HERO_KEYS.iter().chain([&SELECTED_KEY]) {
            assert!(manifest.setting(k).is_some(), "{k}");
        }
        assert!(manifest.setting("swap_costs_turn").is_some());
        assert!(manifest.setting("sheet_slot").is_some());
        assert!(manifest.setting("messages").is_some());
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 31);
    }

    /// Resolves every pattern against the game's exe when it's installed
    /// (skipped otherwise) and checks the addresses found in 1.0.6.
    #[test]
    fn patterns_resolve_on_installed_game() {
        use uk_qol_runtime::host::resolve_pattern;
        use uk_qol_runtime::pattern::Image;
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let exe = std::path::PathBuf::from(home)
            .join(".local/share/Steam/steamapps/common/Underkeep/underkeep.exe");
        let Ok(file) = std::fs::read(&exe) else {
            return;
        };
        let mapped = uk_qol_runtime::pe::map_file(&file).unwrap();
        let img = Image::new(&mapped).unwrap();
        let expected: BTreeMap<&str, u32> = [
            ("hero_equip_item", 0x17dea0),
            ("hero_unequip", 0x17ece0),
            ("item_create", 0x140fb0),
            ("item_take", 0x141f80),
            ("inventory_add", 0x198c80),
            ("in_battle", 0x1916f0),
            ("round_action_push", 0x1c84b0),
            ("turn_manager", 0x43f430),
            ("party", 0x43f640),
            ("party_inventory", 0x440308),
            ("selected_hero", 0x44013c),
            ("game_state", 0x43ca70),
            ("ui_mode", 0x440450),
            ("cursor", 0x442be8),
            ("target_mode", 0x4413a4),
            ("drag_item", 0x141950),
            ("drag_end", 0x166040),
            ("char_sheet", 0x440dd0),
            ("ui_context", 0x43ca88),
            ("script_condition", 0xda910),
            ("script_command", 0xdbc20),
            ("tooltip_draw", 0x166900),
            ("tooltip_show", 0x167320),
            ("tooltip_compare", 0x371750),
            ("item_tooltip", 0x249370),
            ("tooltip_layout_point", 0x11e9a0),
            ("string_concat", 0x27de0),
            ("string_copy", 0x27970),
            ("string_free", 0x3000),
            ("party_round_tick", 0x187da0),
            ("hero_rest_step", 0x17dbb0),
        ]
        .into_iter()
        .collect();
        let pats = PatternFile::parse(include_str!("../patterns.toml")).unwrap();
        let build = uk_qol_runtime::build::detect_from_file(&exe)
            .unwrap()
            .sha256_hex();
        if build != "556f696519db3a948f1ef1f37e4fdc23293c330d00d93399380dd2c3606055ba" {
            return; // another game version
        }
        for p in &pats.patterns {
            let r = resolve_pattern(&img, p, Some(&build))
                .unwrap_or_else(|e| panic!("{}: {e}", p.name));
            assert_eq!(Some(&r.rva), expected.get(p.name.as_str()), "{}", p.name);
        }
        eprintln!(
            "{} patterns resolved on the installed game",
            pats.patterns.len()
        );
    }
}
