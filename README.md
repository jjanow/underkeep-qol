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

> Status: in development, nothing released yet. The loader, runtime, overlay
> (F10 / L3+R3), manager and all five mods work in game 1.0.5 under Proton;
> Windows and Steam Deck are untested. See PLAN.md for what's left.
> It's built for a public release, so installing, updating and recovering
> from a game patch must work without any technical knowledge.

## Mods

| Mod | What it does |
|---|---|
| `bloodmage-cost` | Choose how Blood Mages pay for spells: bones, or their own health. |
| `inventory-sort` | Sort an inventory with one key press or button. |
| `weapon-swap` | A second weapon slot on each hero's character sheet and a key to swap to it. |
| `extra-skill-slots` | More active-skill slots per hero, so you stop swapping skills in and out. |
| `skip-logos` | Go straight to the main menu: no studio and audio logos at startup. |

Details for each are in [Mod designs](#mod-designs).

## Installing

Downloads are on the [releases page](https://github.com/jjanow/underkeep-qol/releases)
(none published yet). Each release has the manager for Windows, Linux and
macOS, the loader (`uk-qol-runtime-windows-x64.zip`, one `winmm.dll`) and
`SHA256SUMS`. You only need the manager: it downloads the matching loader
itself, or uses a `winmm.dll` placed next to it.

Close the game first; the manager won't change files while it runs.

**Windows**

1. Download `uk-qol-windows-x64.exe` and run it.
2. It finds the game (or click **Browse…** and pick the folder with
   `underkeep.exe`). Click **Install**.
3. Start the game from Steam. **F10** opens the mod menu.

**Linux (Proton)**

1. Download `uk-qol-linux-x64.tar.gz`, unpack it and run `uk-qol`
   (`./uk-qol gui` from a terminal).
2. Click **Install**.
3. The game needs one Steam launch option, so Proton loads the mod loader:
   `WINEDLLOVERRIDES="winmm=n,b" %command%`. With Steam closed the manager
   can set it for you (**Set it in Steam for me**); otherwise click **Copy**
   and paste it in
   Steam, under the game's *Properties → General → Launch Options*.
4. Start the game. **F10** (or **L3+R3** on a gamepad) opens the mod menu.

**Steam Deck**: switch to Desktop Mode and follow the Linux steps, then go
back to Gaming Mode and play. Open the mod menu with **L3+R3**.

**macOS (CrossOver, Whisky, Wine)**: download `uk-qol-macos-arm64.tar.gz`,
run `uk-qol`, choose the game folder inside the bottle and click
**Install**. Then set a DLL override in the bottle (`winmm` = *native, then
builtin*). Not tested yet.

**By hand**: unzip `winmm.dll` from the runtime zip into the game folder
(next to `underkeep.exe`), plus the launch option above on Linux. To remove
it, delete that `winmm.dll` and the `uk_qol` folder.

From the command line the same is `uk-qol install` (add
`--set-launch-option` on Linux); `uk-qol help` lists every command.

**Updating**: the manager checks for a new release when it starts and asks
before updating itself, the loader and the mod patterns (`uk-qol update`
from a terminal).

**Uninstalling**: move every weapon out of a second weapon slot into your
inventory in game first (see [`weapon-swap`](#weapon-swap)), close the game,
then **Uninstall** in the manager (`uk-qol uninstall`). On Linux remove the
launch option too. The files next to your saves are kept, so reinstalling
later brings the mods' state back.

**If the game misbehaves**: hold **Shift** while it starts to run it with
every mod off. After a game update a mod may turn itself off until its
patterns are updated; the mod menu says which one and why.

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

This works under Steam (Flatpak) with Proton: the runtime loads in game
version 1.0.5 and audio, gamepad, save/load and quit work normally. Windows
is still to be tested.

The manager sets this up or prints the exact line to paste.

### 2. Mod runtime

Written in Rust, built as a Windows DLL (`x86_64-pc-windows-gnu`, cross-compiled
from Linux too).

- **Build detection.** Identifies the game build by the exe's SHA-256, so each
  build is treated as its own binary, and reads the version string the title
  screen shows (`version 1.0.5`). Both go into the log and bug reports.
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
  `SDL_GL_SwapWindow` and `SDL_PollEvent` (named SDL3 imports of the exe,
  redirected in its import table, not patterns) to draw an
  [egui](https://github.com/emilk/egui) overlay on the game's GL context and
  read input. **F10** (or **L3+R3** on a gamepad) opens the mod menu: enable
  and disable mods, change their settings, rebind their keys, see which ones
  failed to load and why. While it's open the game gets no input.
- **Hotkeys** are read from SDL input events, so they work the same with
  keyboard and gamepad and don't fight the game's own key bindings. Each mod's
  keys are configurable (`S`, `Shift+1`, `Pad:Y`, `Pad:LB+Pad:X`, …), and a
  key a mod handles never reaches the game. Hotkeys are off while you type in
  a game text field.
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
- State a mod must keep (the weapon in a second weapon slot, extra skill slot
  assignments, per-hero toggles) is written to a sidecar file next to the
  save, keyed by hero, whenever the game saves (menu, quick save,
  autosave), and read when it loads. Deleting a save in game deletes its
  sidecar; a new game starts every mod empty.
- Saves always load without the mods, with one exception the player must
  know about: **an item in a second weapon slot exists only in the sidecar file.**
  If the mods are removed, or the sidecar is lost (a deleted file, a
  computer that plays the save without the mods and Steam Cloud), that
  weapon is gone. See [`weapon-swap`](#weapon-swap).
- The game keeps its saves in Steam Cloud storage (its log says "Save
  location: Steam cloud"); on disk they are `save_1`, `save_auto`, … in
  Steam's `userdata/<account>/2411200/remote/` folder, not in the game folder
  or the Wine prefix. The game writes them only through Steam's cloud API.
- Sidecars go there too, as `qol_save_1.toml`, `qol_save_auto.toml`, …,
  written through the same API, so mod state follows a save to other
  computers and the Steam Deck. The game's load list doesn't show them.
- Without Steam the sidecars are kept locally, in
  `uk_qol/saves/<Steam account id or default>/<save>.qol.toml` in the game
  folder. Local sidecars from older versions are moved into the cloud the
  first time the game saves or loads with Steam (the newer copy wins); the
  local files are kept in `uk_qol/saves/<account>/migrated/` as a backup.

### 5. Manager

**Underkeep QoL Manager**, a desktop app for Windows, Linux / Steam Deck and
macOS (CrossOver / Whisky bottles), built like Underkeeper: one self-contained
file, nothing else to install, dark and light mode.

- Finds Steam installs on its own (Steam libraries, Flatpak, Proton, Wine
  bottles), or lets you pick the folder.
- **Install / Uninstall** buttons. Before uninstalling, it warns that any
  weapon still in a second weapon slot will be lost and tells the player to move
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
                               # about weapons left in second slots first)
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

Toggled with a hotkey while the hero is selected, or by clicking the badge on
the Blood Mage's skill slot (**B** for bones, **HP** for health; setting
`hud_badge`). Implementation: hook the bone check in
the spell-cost path (the opcode handlers for Blood Mage skills feed into it)
and report "no bones" in `health` mode, so the game's own health payment runs.

### `inventory-sort`

Sorts the open inventory (party stash or a hero's bag).

- Order: item category (weapons, armor, jewelry, consumables, materials,
  quest items), then rarity / value high to low, then name.
- Stackable items are merged into full stacks first.
- Equipped items and hotbar / quick slots are never moved.
- A hotkey (default **Z**: the game uses S), and a small **Sort** button
  drawn by the overlay in the inventory panel's top-right corner while a sort
  is possible (setting `hud_button`).
- Runs only when nothing is being dragged and no trade window is mid-transaction.

### `weapon-swap`

Adds a **second weapon slot** to each hero's character sheet, left of the
helmet slot. It holds one spare weapon of any kind: a bow, a sword, a
two-handed weapon. A key swaps it with the equipped weapon in one action:
sword ⇄ bow, sword and shield ⇄ two-hander, one melee weapon ⇄ another.
Keys: **Shift+1..4** for heroes 1–4, plus one for the selected hero; all
rebindable.

- Move a weapon into the slot like into any equipment slot (the game picks
  items up with a click and drops them with the next). Requirements are
  checked when it's swapped in, like a normal equip.
- The weapon in the second slot gives no stats until it's swapped in.
- There is no second shield slot: the shield only has to move for a
  two-handed weapon, and the swap handles that (below).
- **In combat, a swap costs the hero's turn** by default. Setting:
  `swap_costs_turn = true | false`. Outside combat it's always free.
- The game's save format has no room for this slot, so the weapon in it is
  stored in the save's sidecar file (see *Save safety*), not in the save.

How it works now (0.1.0; tested in game 1.0.5 with a gamepad, mouse and
keyboard):

- On the character sheet the slot sits left of the helmet slot, drawn like
  the game's empty weapon slot (its own sword icon, read from your game
  files) with a Roman numeral **II** in the corner (setting `sheet_slot`).
  Click it with a weapon on the cursor (picked up from the inventory or a
  hand) to put it in; a weapon already there goes to the inventory. Click
  the filled slot to pick its weapon up, then click the inventory panel
  (moves it there) or the weapon slot (swaps); a click anywhere else puts
  it back. A right click moves it straight to the inventory. The slot shows
  the stored weapon's initials (no item icons) and turns green or red while
  an item that fits or doesn't is on the cursor.
- With a gamepad (no mouse cursor) the same is in the mod's section of the
  overlay menu: per hero, **Put the weapon in hand in the slot**, **Put
  *item* in the slot** (for each weapon in the inventory), **Swap now** and
  **Move to inventory**. The slot can always be emptied back into the
  inventory there. A hero with a filled slot also gets a small button on
  their weapon hand slot on the HUD, showing the stored weapon's initials;
  clicking it swaps (setting `hud_button`).
- A swap is symmetric: after sword ⇄ bow the slot holds the sword, and the
  next swap brings it back. Only weapons go in; items that stack and quest
  items never do.
- The swap goes through the game's own equip function, as when equipping
  from the inventory: class checks, the item's equip scripts, stats and HUD
  update like a manual equip. A weapon the hero can't use stays where it
  was, with nothing lost.
- A two-handed weapon pushes the shield into the inventory, as a manual
  equip does. Swapping back to a one-handed weapon puts that shield back on
  if it is still in the inventory. Between two one-handed weapons the
  shield stays on.
- In combat (the game's own "battle" check) a swap needs the hero to be able
  to act and the monsters not to be moving, then ends that hero's turn
  (`swap_costs_turn`) through the turn queue's "hero is done" action: the
  hero is greyed out, the next hero is selected, and the monsters act when
  everyone is done. During the monsters' turn a swap is refused. With
  `swap_costs_turn = false` the swap is free.
- If a save's sidecar doesn't match the save (e.g. it was saved while the
  mod was off), its weapons aren't put back in the slots; the menu lists
  them with a **Move to inventory** button, warning that they may already
  be in the party.

**What players are told** (in the README, on the mod's first use in game,
and in the manager before uninstalling):

```
A weapon in a second weapon slot is kept by the mod, not by the game.
If you remove the mod, reinstall the game, play the save on another computer,
or the mod's file next to your save is lost, that weapon is gone for good.
Before uninstalling, move every second-slot weapon back to your inventory.
```

The first-use notice must be acknowledged once per install; it can be
reopened from the overlay.

### `extra-skill-slots`

Today each hero has one active-skill slot next to the basic attack. This mod
adds 3 more (configurable 1–5).

- New slots appear as small buttons in a column right of the hero's skill
  slot on the HUD, drawn by the overlay at the game's own layout positions
  and labelled with the skill's initials (the full name on hover). The
  game's layout can't take new slots (its code reads a fixed set of rects).
- Each new slot gets its own key binding (unbound by default, set in the
  overlay) for the selected hero, and its button casts it for that hero on a
  left click. Right-click (or a click on an empty slot) assigns a skill with
  the game's own skill panel; the mod's menu section has a list too.
- Assignments are stored in the save's sidecar file. Without the mod the hero
  just has their original slot.
- To investigate: how the combat code reads the selected skill slot
  (`selected_skill_slot`) and whether casting from a non-selected slot can reuse
  that path directly.

### `skip-logos`

Each launch shows the Rake in Grass and FMOD logos (about 3 s) before the
main menu. This mod skips them, so the game starts on the main menu.

- The intro cinematic needs nothing: the game plays it once and then skips
  it on its own (`intro_was_played` in `game_config.txt`). The main menu's
  own reveal animation is left as is.
- To investigate: where the startup logo animation (`anim_startup_logo`)
  is started and how it hands over to the menu; whether ending it early
  (or a data patch of its length) is enough, or the hand-over has to be
  called directly.

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
- **Steam Deck.** The overlay works with a gamepad (L3+R3 opens it, the d-pad
  or left stick moves, A selects, B closes), and the Linux launch option is all
  that's needed.
- **CI.** Every release is built by CI, with checksums; CI runs only for
  release tags and manual runs.

## Repository layout

```
crates/
  proxy/        winmm.dll: export forwarding + runtime bootstrap
  runtime/      pattern scanner, hooks, data patcher, overlay, config, mod host
  dft/          reader/writer for the game's DfT definition files
  manager/      Manager app and uk-qol CLI
mods/
  bloodmage-cost/  inventory-sort/  weapon-swap/  extra-skill-slots/  skip-logos/
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
the DLL on Windows itself hasn't been tried yet; use Linux or WSL. With
`UNDERKEEP_DIR` pointing at the game folder, the tests also use the local
install: `dft` round-trips every data file, and the runtime tests run build
detection and the pattern scanner against `underkeep.exe`.

Release builds use `panic = "unwind"`, unlike Underkeeper: the runtime
catches a panic in a mod and turns that mod off instead of ending the game.

CI (`.github/workflows/build.yml`) runs only for `v*` tags and manual runs
from the Actions tab, not on pushes or pull requests.

## Supporting a new game version

When the game updates:

1. `uk-qol status` lists every pattern that no longer resolves.
2. Find the moved function in the new exe (start from the old match) and
   update `patterns.toml`.
3. Patterns are kept per build, so players still on an older build keep working.
4. Publish a release with the new pattern files; installed managers offer
   them on their next start (`uk-qol update`), without a new loader.

Players follow a pinned "game updated" issue; the full process is in
[CONTRIBUTING](CONTRIBUTING.md#when-the-game-updates-maintainers).

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
