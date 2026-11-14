# Underkeep QoL

A mod loader and a set of quality-of-life mods for **Underkeep**, for the
**Steam** version of the game, on Windows and on Linux / Steam Deck (Proton).

Underkeep has no official mod support. This project adds it in a way that:

- **never modifies the game's own files**, so Steam's *Verify integrity* and
  game updates can't break or undo it, and uninstalling is just removing one
  DLL;
- **survives game patches**: mods find the code they change by byte patterns,
  not fixed addresses. A mod whose patterns aren't found on your game version
  turns itself off and says so, and the game still starts normally;
- **keeps your saves loadable without the mods**: the save file format is never
  changed. Anything a mod has to remember lives in a separate file next to the save.

> Status: design stage. Nothing below is released yet. This README is the plan.

## Planned mods

| Mod | What it does |
|---|---|
| `bloodmage-cost` | Choose how Blood Mages pay for spells: bones, or their own health. |
| `inventory-sort` | Sort an inventory with one key press or button. |
| `weapon-swap` | A second, stored weapon per hero (e.g. a bow) and a key to swap to it. |
| `extra-skill-slots` | More active-skill slots per hero, so you stop swapping skills in and out. |

Details for each are in [Mod designs](#mod-designs).

## How it works

```
underkeep.exe ──loads──> SDL3.dll / fmod.dll ──import──> winmm.dll  (our proxy, in the game folder)
                                                            │
                                                            ├─ forwards every winmm call to the real system winmm.dll
                                                            └─ starts the mod runtime (uk_qol)
                                                                 ├─ identifies the game build (exe hash)
                                                                 ├─ loads enabled mods from  mods/
                                                                 ├─ resolves each mod's byte patterns, installs hooks
                                                                 ├─ patches game data in memory as it is loaded
                                                                 └─ draws the in-game overlay (settings, buttons)
```

### 1. Loader: a proxy `winmm.dll`

The game exe imports only `SDL3.dll`, `fmod.dll`, `steam_api64.dll` and core
Windows DLLs. `SDL3.dll` and `fmod.dll` both import `winmm.dll`, which isn't a
Windows "KnownDLL", so Windows (and Wine) look for it in the game folder first.
Our `winmm.dll` forwards all of winmm's exports to the real one in the system
folder and starts the mod runtime on a separate thread.

Why not the alternatives:

- Replacing `SDL3.dll`, `fmod.dll` or `steam_api64.dll` means overwriting game
  files, which updates and file verification put back.
- An external injector would need to run every time you start the game.

On **Linux** (Proton) Wine prefers its built-in winmm, so the game needs a DLL
override. In Steam, *Properties → Launch Options*:
`WINEDLLOVERRIDES="winmm=n,b" %command%`

The manager sets this up or prints the exact line to paste.

### 2. Mod runtime

Written in Rust, built as a Windows DLL (`x86_64-pc-windows-gnu`, cross-compiled
from Linux too).

- **Build detection.** Identifies the game build by the exe's hash, so each
  build is treated as its own binary.
- **Byte-pattern scanning.** Each mod lists the functions it needs as patterns
  with wildcards (`48 8B ?? ?? 0F 85 ...`) plus how to get from the match to the
  target. Patterns are resolved once at startup. If a required pattern finds no
  match or more than one, that mod is disabled and the reason is logged and
  shown in the overlay. Other mods keep working.
- **Hooks.** Function detours and small in-place patches, applied and removed
  as a unit per mod, so a mod can be turned off at runtime.
- **Data patches.** The game's definition files (`base/*.txt.rfile`, a binary
  "DfT" format) are patched in memory as the game reads them: change a value,
  add a record, move a HUD element. The files on disk are never touched.
- **Overlay.** The game renders with OpenGL through SDL3. The runtime hooks
  `SDL_GL_SwapWindow` and `SDL_PollEvent` (stable named exports, not patterns)
  to draw an [Dear ImGui](https://github.com/ocornut/imgui) overlay and read
  input. **F10** opens the mod menu: enable and disable mods, change their
  settings, see which ones failed to load and why.
- **Hotkeys** are read from SDL input events, so they work the same with
  keyboard and gamepad and don't fight the game's own key bindings. Each mod's
  keys are configurable.
- **Logging** to `uk_qol/uk_qol.log` in the game folder.

### 3. Mods

Each mod is one folder with a manifest:

```
mods/
  bloodmage-cost/
    mod.toml          # id, name, version, author, description
                      # supported game builds, conflicts, dependencies
                      # settings schema with defaults (drawn in the overlay)
    patterns.toml     # named byte patterns, per build if they differ
    patches/          # in-memory data patches for rfile definitions
```

Built-in mods are compiled into the runtime (Rust modules behind a common
`Mod` trait: `on_load`, `on_unload`, `on_frame`, `on_input`, `on_save`,
`on_load_save`). The folder layout and manifest are the same so that
data-only mods (patches, no code) can be dropped in by anyone later.

User settings live in `uk_qol/config.toml` in the game folder, one table per
mod. The overlay edits the same file.

### 4. Save safety

- The game's save files are never rewritten by a mod.
- State a mod must keep (a stored second weapon, extra skill slot
  assignments, per-hero toggles) is written to `<save name>.qol.toml` next to
  the save, keyed by hero, whenever the game saves, and read when it loads.
- If you uninstall the mods, the game loads your saves as if the mods were
  never there. Anything held only by a mod (e.g. a stored weapon) is first
  given back: **uninstall from the manager**, which puts stored items back into
  the inventory before removing the loader. (See the open question under
  `weapon-swap`.)
- The sidecar files may **not** be synced by Steam Cloud, which only
  syncs the game's own files. Mod state is then local to one
  computer; the saves themselves still sync.

### 5. Manager

`uk-qol`, a command-line tool for Windows and Linux (a GUI may come later):

```
uk-qol detect                  # find Steam installs (Steam libraries, Flatpak,
                               # Wine bottles), show the game build
uk-qol install [--game PATH]   # copy winmm.dll + runtime + mods, print/set the
                               # Linux launch option
uk-qol status                  # installed version, game build, which mods work on it
uk-qol enable  <mod>           # same as toggling in the overlay
uk-qol disable <mod>
uk-qol config  <mod> <key> <value>
uk-qol uninstall               # return held items to inventories, remove everything
```

Install discovery and the DfT file parser are shared with
[Underkeeper](https://github.com/jjanow/underkeeper), the save editor.

## Mod designs

### `bloodmage-cost`

The game's rule: Blood Mage spells consume bones, and when you have none they
cost the Blood Mage's own health. This mod adds a setting per Blood Mage:

| Mode | Behavior |
|---|---|
| `bones` (game default) | Use bones; health only when out of bones. |
| `health` | Always pay with health, keep your bones. |
| `bones-only` | Use bones; refuse to cast instead of spending health. |

Toggled in the overlay or with a hotkey while the hero is selected, shown as a
small icon on the hero's skill button. Implementation: hook the bone check in
the spell-cost path (the opcode handlers for Blood Mage skills feed into it)
and override its answer per mode.

### `inventory-sort`

Sorts the open inventory (party stash or a hero's bag).

- Order: item category (weapons, armor, jewelry, consumables, materials,
  quest items), then rarity / value high to low, then name.
- Stackable items are merged into full stacks first.
- Equipped items and hotbar / quick slots are never moved.
- Phase 1: a hotkey (default **S** while an inventory is open).
  Phase 2: a **Sort** button drawn next to the inventory panel. Placing a
  button inside the game's own GUI needs GUI injection, so it starts as an
  overlay button anchored to the panel.
- Runs only when nothing is being dragged and no trade window is mid-transaction.

### `weapon-swap`

Each hero gets one stored weapon (and optional off-hand) alongside the
equipped one. A hotkey (default **Q**, per hero: **Shift+1..4**) swaps them in
one action, e.g. sword + shield ⇄ bow.

- The stored weapon gives no stats and isn't seen by the game's stat
  calculation, so no new equipment slot has to exist in the game's data or saves.
- Requirements are checked on swap like a normal equip.
- Shown as a small slot under the hero's weapon slot in the character panel.
- To investigate: the game's equipment slots are fixed (helmet, armor, sword,
  shield, rings, ...) and weapons already carry a `ranged_weapon` flag used by
  ranged skills; whether swapping costs a combat turn should match the game's
  own equip rules.

### `extra-skill-slots`

Today each hero has one active-skill slot next to the basic attack. This mod
adds more (default: 3 extra, configurable 1–5).

- New slots appear beside the existing one on the hero's HUD panel. HUD
  positions come from the game's GUI definitions, so the layout is moved with a
  data patch rather than code.
- Each new slot gets its own key binding (default: unbound, set in the
  overlay) and can be clicked like the original.
- Assignments are stored in the save's sidecar file. Without the mod the hero
  just has their original slot.
- To investigate: how the combat code reads the selected skill slot
  (`selected_skill_slot`) and whether casting from a non-selected slot can reuse
  that path directly.

## Repository layout (planned)

```
crates/
  proxy/        winmm.dll: export forwarding + runtime bootstrap
  runtime/      pattern scanner, hooks, data patcher, overlay, config, mod host
  dft/          reader/writer for the game's DfT definition files
  manager/      uk-qol CLI
mods/
  bloodmage-cost/  inventory-sort/  weapon-swap/  extra-skill-slots/
tools/          pattern extraction and build-diffing helpers
```

## Building

Requires Rust (stable) and, on Linux, the MinGW-w64 target:

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

## Supporting a new game version

When the game updates:

1. `uk-qol status` lists every pattern that no longer resolves.
2. Find the moved function in the new exe (the `tools/` scripts diff the old
   and new builds around the old match) and update `patterns.toml`.
3. Patterns are kept per build, so players still on an older build keep working.

## Disclaimer

Unofficial and not affiliated with the developers or publishers of Underkeep.
Back up your saves. Use at your own risk.

## License

[MIT](LICENSE)
