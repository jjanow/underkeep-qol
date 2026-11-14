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

> Status: early development. The loader (`winmm.dll`) builds and forwards
> calls under Wine; no mods yet and nothing is released. This README is the plan.
> It's built for a public release, so installing, updating and recovering
> from a game patch must work without any technical knowledge.

## Planned mods

| Mod | What it does |
|---|---|
| `bloodmage-cost` | Choose how Blood Mages pay for spells: bones, or their own health. |
| `inventory-sort` | Sort an inventory with one key press or button. |
| `weapon-swap` | A ranged-weapon slot on each hero's character sheet and a key to swap to it. |
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
- State a mod must keep (the weapon in a ranged slot, extra skill slot
  assignments, per-hero toggles) is written to `<save name>.qol.toml` next to
  the save, keyed by hero, whenever the game saves, and read when it loads.
- Saves always load without the mods, with one exception the player must
  know about: **an item in a ranged slot exists only in the sidecar file.**
  If the mods are removed, or the sidecar is lost (another computer, cloud
  sync, a deleted file), that weapon is gone. See [`weapon-swap`](#weapon-swap).
- The sidecar files may **not** be synced by Steam Cloud, which only
  syncs the game's own files. Mod state is then local to one
  computer; the saves themselves still sync.

### 5. Manager

**Underkeep QoL Manager**, a desktop app for Windows, Linux / Steam Deck and
macOS (CrossOver / Whisky bottles), built like Underkeeper: one self-contained
file, nothing else to install, dark and light mode.

- Finds Steam installs on its own (Steam libraries, Flatpak, Proton, Wine
  bottles), or lets you pick the folder.
- **Install / Uninstall** buttons. Before uninstalling, it warns that any
  weapon still in a ranged slot will be lost and tells the player to move
  them to the inventory in game first. It doesn't move them itself. On Linux it shows the launch option to
  paste, with a copy button, and checks whether it's set in Steam.
- A mod list with on/off switches and each mod's settings.
- A **compatibility** panel: your game build, and for each mod whether it works
  on it.
- **Updates itself** and the installed mods from this repository's GitHub
  releases, after asking, with SHA-256 checks.
- Refuses to install or change files while the game is running.

The same actions are available as a command line, for scripts and support:

```
uk-qol detect                  # find Steam installs (Steam libraries, Flatpak,
                               # Wine bottles), show the game build
uk-qol install [--game PATH]   # copy winmm.dll + runtime + mods, print/set the
                               # Linux launch option
uk-qol status                  # installed version, game build, which mods work on it
uk-qol enable  <mod>           # same as toggling in the overlay
uk-qol disable <mod>
uk-qol config  <mod> <key> <value>
uk-qol uninstall               # remove everything the installer added (warns
                               # about weapons left in ranged slots first)
uk-qol report                  # bundle log, config and build info for a bug report
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

Toggled in the overlay or with a hotkey while the hero is selected, shown as a
small icon on the hero's skill button. Implementation: hook the bone check in
the spell-cost path (the opcode handlers for Blood Mage skills feed into it)
and report "no bones" in `health` mode, so the game's own health payment runs.

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

Adds a **ranged slot** to each hero's character sheet, next to the weapon
and shield slots. It holds one ranged weapon (bow or other weapon the game
flags as ranged). A key swaps it with the equipped weapon in one action, e.g.
sword ⇄ bow. Keys: **Shift+1..4** for heroes 1–4, plus one for the selected
hero; all rebindable.

- Drag a ranged weapon into the slot like any equipment slot. Requirements
  are checked when it's swapped in, like a normal equip.
- The weapon in the ranged slot gives no stats until it's swapped in.
- **In combat, a swap costs the hero's turn** by default. Setting:
  `swap_costs_turn = true | false`. Outside combat it's always free.
- The game's save format has no room for this slot, so the weapon in it is
  stored in the save's sidecar file (`<save>.qol.toml`), not in the save.
- To investigate: the slot is drawn and handled by the mod (the game's own
  equipment slots are fixed); what happens to a shield when a two-handed
  ranged weapon is swapped in; the swap goes through the game's own equip
  function so that stats, skills and the HUD update the same way as a manual
  equip.

**What players are told** (in the README, on the mod's first use in game,
and in the manager before uninstalling):

```
A weapon in a ranged slot is kept by the mod, not by the game.
If you remove the mod, reinstall the game, play the save on another computer,
or the mod's file next to your save is lost, that weapon is gone for good.
Before uninstalling, move every ranged-slot weapon back to your inventory.
```

The first-use notice must be acknowledged once per install; it can be
reopened from the overlay.

### `extra-skill-slots`

Today each hero has one active-skill slot next to the basic attack. This mod
adds 3 more (configurable 1–5).

- New slots appear beside the existing one on the hero's HUD panel. HUD
  positions come from the game's GUI definitions, so the layout is moved with a
  data patch rather than code.
- Each new slot gets its own key binding (unbound by default, set in the
  overlay) and can be clicked like the original. Assigning a skill works
  like the original slot (right-click).
- Assignments are stored in the save's sidecar file. Without the mod the hero
  just has their original slot.
- To investigate: how the combat code reads the selected skill slot
  (`selected_skill_slot`) and whether casting from a non-selected slot can reuse
  that path directly.

## Reliability (for a public release)

- **Safe mode.** If the game exits abnormally twice in a row after the mods
  load, the next start runs with all mods off and the overlay says why. Hold
  **Shift** while the game starts to force safe mode.
- **Contained failures.** Every hook and mod callback catches its own errors
  (Rust panics included). A failing mod is turned off for the session and
  logged; the game keeps running.
- **Unknown builds.** On a game build no release has seen, each mod still
  tries its patterns, and turns on only if every one matches exactly once.
  A mod is never run on a "best guess" address.
- **Pattern updates without a new release.** Pattern files are data. After a
  game patch, the manager can download updated patterns, so most game
  updates are fixed in minutes, without new code.
- **Crash reports.** The game ships its own crash reporter. Crashes caused by
  a mod must not end up in front of the game's developers as game bugs, so the
  runtime notes in its log which mod was active, and `uk-qol report` makes a
  bundle to attach to an issue here.
- **Languages.** Overlay and manager text is translatable, starting with the
  game's own languages: English, Czech, German, Spanish, French, Polish, Russian.
- **Steam Deck.** The overlay works with a gamepad, and the Linux launch
  option is all that's needed.
- **CI.** Every push builds Windows and Linux binaries. Every release is built
  by CI with checksums.

## Repository layout

`crates/` exists; `mods/` and `tools/` are planned.

```
crates/
  proxy/        winmm.dll: export forwarding + runtime bootstrap
  runtime/      pattern scanner, hooks, data patcher, overlay, config, mod host
  dft/          reader/writer for the game's DfT definition files
  manager/      Manager app and uk-qol CLI
mods/
  bloodmage-cost/  inventory-sort/  weapon-swap/  extra-skill-slots/
tools/          pattern extraction and build-diffing helpers
```

## Building

Requires Rust (stable, edition 2024). The mod loader is a Windows DLL (the
game is a Windows program, also under Proton/Wine), built on Linux by
cross-compiling with MinGW-w64:

```sh
rustup target add x86_64-pc-windows-gnu

# MinGW-w64 linker, one of:
sudo apt install gcc-mingw-w64-x86-64     # Debian, Ubuntu
sudo dnf install mingw64-gcc              # Fedora
sudo pacman -S mingw-w64-gcc              # Arch

# winmm.dll (proxy + runtime) → target/x86_64-pc-windows-gnu/release/winmm.dll
cargo build --release --target x86_64-pc-windows-gnu -p uk-qol-proxy

# Manager / uk-qol CLI for this computer → target/release/uk-qol
cargo build --release -p uk-qol-manager

# Checks
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`.cargo/config.toml` sets the MinGW linker for the Windows target. Building
the DLL on Windows itself hasn't been tried yet; use Linux or WSL. The `dft` tests also round-trip every data file of a local
game install when `UNDERKEEP_DIR` points at the game folder.

Release builds use `panic = "unwind"`, unlike Underkeeper: the runtime
catches a panic in a mod and turns that mod off instead of ending the game.

CI (`.github/workflows/build.yml`) runs only for `v*` tags and manual runs
from the Actions tab, not on pushes or pull requests.

## Supporting a new game version

When the game updates:

1. `uk-qol status` lists every pattern that no longer resolves.
2. Find the moved function in the new exe (the `tools/` scripts diff the old
   and new builds around the old match) and update `patterns.toml`.
3. Patterns are kept per build, so players still on an older build keep working.
4. Publish the new pattern files; installed managers pick them up.

## What this repository contains

Only original code and documentation. **No game files or anything taken from
them** are committed: no executables or DLLs, no `base/` files or assets, no
extracted text, data tables or decompiled code. Mods describe what to change
(byte patterns, field names, new values); the runtime finds the game's own
data on the player's computer at run time. `.gitignore` blocks game file
types as a safety net.

## Disclaimer

Unofficial and not affiliated with the developers or publishers of Underkeep.
Back up your saves. Use at your own risk.

## License

[MIT](LICENSE)
