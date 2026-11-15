# Contributing

Thanks for helping. Building and the checks to run are in
[README.md → Building](README.md#building); the design is in the README and the
work list is in [PLAN.md](PLAN.md).

## The one hard rule: no game material

This repository is public. **Never commit anything from the game**, not even a
small piece:

- no executables or DLLs, no `base/` or `preload/` files, `.rfile`/`.db`
  data, textures, sounds or other assets;
- no save files or pieces of them, including hex dumps in tests;
- no extracted text, string tables, data tables or item lists;
- no decompiled or disassembled code, and no Ghidra/IDA project files.

The one exception is a few small, cropped screenshots of the game in
`docs/screenshots/` that show what a mod looks like, with the credit line
under README's *Screenshots*. Screenshots only: never the game's images,
sprites or other files cut out on their own.

What may go in: byte patterns (short instruction sequences with wildcards, used
to *find* code), offsets relative to a match, field and record names a mod
needs, and original code and documentation. Tests that need game data read it
from the player's own install (`UNDERKEEP_DIR`) and skip when it's not there;
build test inputs by hand otherwise (see `crates/dft` for an example).

`.gitignore` blocks common game file types, but it's only a safety net: check
`git status` before you commit.

## Adding a mod

Built-in mods live in `mods/<id>/`, each a crate named `uk-qol-mod-<id>`
(see `mods/skip-logos`), and are compiled into the runtime:

1. `mod.toml`: the manifest (below).
2. `patterns.toml`: every function or address the mod needs, as a named byte
   pattern, with a comment saying what it is and how it was found (below).
3. `patches/` (optional): declarative data patches for the game's definition
   files, applied in memory (see [Data patches](#data-patches)).
4. The code: a type implementing the runtime's `Mod` trait (`on_load`,
   `on_unload`, `on_frame`, `on_input`, `on_settings`, `on_save`,
   `on_load_save`, optionally `menu`), whose `manifest()` and `patterns()` return the two files
   with `include_str!` (and `patches()` its data patches, if any), and a
   `pub fn new() -> Box<dyn Mod>`.
5. Add the crate to the workspace members and `[workspace.dependencies]` in
   the root `Cargo.toml`, to `crates/proxy/Cargo.toml`, and its `new` to
   `BUILTIN` in `crates/proxy/src/lib.rs`; add `-p uk-qol-mod-<id>` to the
   Windows clippy line in `.github/workflows/build.yml`.

### `mod.toml`

```toml
id = "weapon-swap"            # lowercase letters, digits, single dashes; = Mod::id()
name = "Weapon swap"          # shown in the overlay and manager
version = "0.1.0"
author = "..."                # optional
description = "..."           # optional
# Exe SHA-256s (uk-qol report) the mod was tested on, or "any" for a mod
# that doesn't depend on the exe. Informational: on other builds the mod
# still loads when every required pattern resolves, marked "not tested".
game_builds = ["d3158ad0f94d6770deb64b32d48aeb412a3c5ef3a0b2e37a0e092a7ce66bb4a4"]
conflicts = ["other-mod"]     # optional: never active at the same time
dependencies = ["core-mod"]   # optional: must be active first
enabled = true                # optional (default true): on unless the user turns it off

[[settings]]                  # one table per setting, in display order
key = "swap_costs_turn"       # lowercase, digits, `_`; not `enabled`
type = "bool"                 # bool | int | float | string | enum | key
default = true
label = "Swapping costs a turn"
description = "..."           # optional

[[settings]]
key = "slots"
type = "int"
default = 3
min = 1                       # int and float only, optional, inclusive
max = 5
label = "Extra slots"

[[settings]]
key = "cost"
type = "enum"
choices = ["bones", "health"] # enum only
default = "bones"
label = "Spell cost"

[[settings]]
key = "sort_key"
type = "key"                  # hotkey: "" (unbound), "S", "Shift+1",
default = "S"                 # "Ctrl+Alt+F5", "Pad:North"
label = "Sort hotkey"
```

Unknown keys are errors, so typos don't pass silently. Every default is
checked against its type and limits. A mod with one on/off switch declares no
settings: `enabled` is that switch. The user's values come from
`uk_qol/config.toml` (`[mods.<id>]`); a value of the wrong type, out of range,
not among the choices or not a valid hotkey is logged and the default used.

### `patterns.toml`

One table per pattern; the name is what the mod looks up in
`LoadCtx::patterns` (an RVA; add `LoadCtx::base` for the address).

```toml
# (Example.) The function that starts the startup logo animation. Found from the xref
# to the "anim_startup_logo" string; this is the call right after it.
[start_logo]
pattern = "48 8B 0D ?? ?? ?? ?? E8 ?? ?? ?? ?? 84 C0"
offset = 7                    # optional: added to the match (may be negative)
call = true                   # optional: then follow the call/jmp rel32 there
# rip = { disp = 3, len = 7 } # or: then take the RIP-relative operand's
#                             # target (disp32 at +disp, instruction len bytes)
section = ".text"             # optional: default is every executable section
optional = false              # optional: a missing optional pattern doesn't
                              # disable the mod; it's just absent from the map

# Per-build alternatives: a list of candidates instead of one pattern.
[menu_state]
[[menu_state.candidates]]
pattern = "48 89 05 ?? ?? ?? ?? C6 05"
rip = { disp = 3, len = 7 }
builds = ["d3158ad0f94d6770deb64b32d48aeb412a3c5ef3a0b2e37a0e092a7ce66bb4a4"]
[[menu_state.candidates]]
pattern = "48 89 1D ?? ?? ?? ?? C6 05"
rip = { disp = 3, len = 7 }
```

- A pattern must match **exactly once**; zero or several matches is a miss.
  Patterns can't start with a wildcard.
- Resolution order: the match, plus `offset`, then `call` or `rip`.
- Candidates tagged with the running build (`builds`) are tried first and the
  first that resolves wins. Otherwise every candidate is tried, and those
  that resolve must agree on the address: two different answers are a miss,
  never a guess.
- If any required pattern misses, the mod is off with status "patterns not
  found" and the log has each pattern's reason. Patterns are resolved at
  start, before any mod's hooks change the code.
- `uk_qol/patterns/<id>.toml` in the game folder, if present, replaces the
  built-in file whole (the manager downloads updated patterns there). If it
  doesn't parse, the log says so and the built-in file is used.

### Lifecycle

- At start (outside safe mode) mods load in dependency order: `on_load`
  registers hooks on `LoadCtx::hooks`; the host enables them after `on_load`
  returns Ok. An Err turns the mod off with that reason.
- Turning a mod off in the overlay removes its hooks, then calls
  `on_unload`; mods that depend on it are unloaded first. Turning it on again
  calls `on_load` again with a fresh hook set, so `on_load` must work more
  than once.
- Every callback runs inside `guard::call`; a panic turns the mod off for the
  session. Detours wrap their body in `guard::call_or(id, body, || original(...))`.
- Statuses: active, off, off (safe mode), patterns not found, waiting (a
  dependency isn't active or a conflicting mod is; retried when that
  changes), failed.

A mod must:

- never write the game's save files or files in the game folder other than
  `uk_qol/`. State it has to keep goes in the save's sidecar
  (`ModSaveState`);
- leave the game playable when it's turned off or when its patterns aren't
  found;
- undo all its hooks and patches in `on_unload` (the host removes the hooks
  registered on `LoadCtx::hooks` itself).

### Save state

The runtime hooks the game's save, load, delete and new-game functions; a mod
only implements two callbacks, both called on the game's thread:

- `on_save(&mut ModSaveState)`: the game just saved. Fill in `global` and
  `heroes` (a TOML table per hero key). The state starts as it was loaded, so
  clear what no longer applies.
- `on_load_save(&ModSaveState)`: a save was loaded (the heroes are restored),
  or a new game started (empty state). Replace all per-game state with it.

Key per-hero state by `savehook::hero_key` (`"<hero_id>:<name>"`): the game's
`hero_id` is only the slot a hero was created in and repeats when a hero is
replaced. `savehook::party_heroes()` lists the party's heroes (struct
address, key, class id; `savehook::class_name`). Sidecars are in Steam
Cloud next to the saves (`qol_<save>.toml`, through `ISteamRemoteStorage`;
`runtime::steam`, `save::Store`), or local
(`uk_qol/saves/<Steam account id>/<save>.qol.toml`) without Steam. Every
call into `steam_api64.dll` goes through `runtime::steam`, which falls back
to local files when an export or the interface is missing.

### Controls in the overlay menu

Besides its settings, a mod can draw its own controls (buttons, status text)
in its section of the overlay menu: implement `Mod::menu(&mut self, ui:
&mut egui::Ui)` with the runtime's re-exported `uk_qol_runtime::egui`. It is
called every frame while the menu is open, the section expanded and the mod
active, on the game's render thread inside the mod's panic guard. Don't
touch the game from it: queue the work and do it in `on_frame`. Keep the
widgets in rows of one (the gamepad's d-pad moves between rows, not along
one), and when a click changes which buttons exist, `request_focus` a
button that's still there, or gamepad focus is lost.

### HUD widgets

A mod can also draw on the game's HUD while the menu is closed (a badge on
a skill slot, a button on the inventory panel). Implement two methods:

- `Mod::has_hud(&self) -> bool`: whether there is anything to draw now
  (default false). Checked every frame; the overlay runs its HUD pass only
  while some active mod says yes, so a setting that hides the widgets should
  make it return false.
- `Mod::hud(&mut self, ui: &mut egui::Ui)`: called every frame while the menu
  is closed, the mod's `has_hud` is true and the game's plain HUD is up
  (`hud::shown()`: playing, not on the map, no menu, dialog, shop, skill
  panel or target selection over it), on the game's render thread inside
  the mod's panic guard. `ui` covers the window; place widgets with
  `ui.put(rect, ...)` or `hud::button(ui, rect, text, fill)` (a small
  label-button in the HUD's style).

Positions come from `uk_qol_runtime::hud`, in overlay points:

- `hud::rect(name)`: a rect of the game's HUD layout by its `def_def` name
  (`hud::LAYOUT` lists them: `face_hand_slot_1..8`, `face_rect_1..4`,
  `inventory_rect`, `minimap_rect`, ...); `hud::skill_slot_name(i)` and
  `hud::weapon_slot_name(i)` name hero `i`'s hand slots. `hud::gui_rect(name)`
  gives the same rect in the game's GUI space.
- `hud::to_screen(p)` / `hud::rect_to_screen(r)` map GUI-space points and
  rects (640 x 360, y down) to points; `hud::points_per_unit()` sizes text
  and widgets like the HUD. The game fits that space into the window
  keeping its aspect ratio, centred (black bars on the long side), so
  widgets placed this way follow every window size.

Input: a mouse press on a widget that senses clicks (a button, anything
`allocate_rect`ed with `Sense::click()`) goes to the HUD and is swallowed
together with its release; every other click, and all mouse motion, still
reaches the game. Right clicks work (`secondary_clicked`). HUD widgets have
no gamepad focus: give each action a hotkey too. As in the menu, queue game
work for `on_frame` rather than doing it in `hud`.

To check positions, create an empty file `uk_qol/hud-debug` in the game
folder before starting the game: the overlay then outlines every layout
rect with its name and logs the rects and the window transform once.

## Data patches

A patch changes one of the game's definition documents (the DfT files in
`base/*.txt.rfile`) in memory, as the game parses it; the files on disk are
never touched. It is a TOML file read by `dft::patch::Patch`, either in a
mod's `patches/` folder (returned by `Mod::patches` as `(file name,
include_str!(...))` pairs) or dropped by the player into `uk_qol/patches/`
in the game folder:

```toml
# The document, by the game's name for it: <folder>/<name>.txt, stored as
# base/<folder>_<name>.txt.rfile. base/def_def_skill.txt.rfile is
# "def/def_skill.txt", base/text_text_common_en.txt.rfile is
# "text/text_common_en.txt" (see the include list in def/def.txt).
file = "def/def_skill.txt"

[[op]]
set = "hud_skill_slot_1.pos[0]"   # change an existing value
value = 120

[[op]]
add = "my_record"                 # a new entry; fails if it already exists
value = { parent = "engine_item", size = [1, 2] }

[[op]]
add = "my_record.size[]"          # append to a list ([n] inserts at n)
value = 3

[[op]]
remove = "old_record.unused"      # in a list, later items move up
```

- **Paths** are names joined by `.`, with list positions in brackets:
  `a.b[2].c`. Quote a name that contains `.`, `[`, `]`, `"` or spaces:
  `"odd.name".x` (`\"` and `\\` escape inside quotes). `[]` means "after the
  last item" and is only allowed at the end of an `add` path.
- **Values:** integer → int (must fit 32 bits), float → float (stored as
  f32), `true`/`false` → int 1/0, string → string (added to the document's
  string table if new), array → list, table → table of named entries. TOML
  sorts a table's keys, so entries added from an inline table are in key
  order.
- **`set` keeps the type** of the value it replaces: an int takes an integer
  or bool, a float takes a float or an integer, a string a string, and a
  table takes a table or an array and is replaced whole. Anything else is a
  type-mismatch error.
- **All or nothing:** if any op fails, the document is left unchanged and
  the error names the op, its path and the cause, for example
  ``op 2 (remove hud.q): path not found: no `hud.q` ``.
- Setting a value to what it already is changes nothing, byte for byte.
- **Order:** the active mods' patches in load order, then the player's
  files sorted by name; for the same value, the last one wins.
- **When:** the set of patches is fixed at start (the game parses its
  definitions once, during init), so turning a mod on or off changes its
  patches from the next launch.
- **Failures:** a patch file that doesn't parse is logged and skipped; a patch
  whose ops fail on the document is logged and skipped (the others still
  apply); a document that doesn't parse reaches the game unchanged.
- Saves (`save_*`) and the player profile go through the same parser;
  patches naming them are refused.
- To see every document name the game parses, set `log_defs = true` under
  `[runtime]` in `uk_qol/config.toml`; the log lists them.

## Updating patterns after a game patch

1. Run `uk-qol status` against the new build; it lists every pattern that no
   longer resolves.
2. Find the moved code in the new exe (start from the old match), write a new
   pattern and check that it matches **exactly once**.
3. Patterns are per build: add the new one next to the old one instead of
   replacing it, so players still on the old build keep working.
4. In the pull request, give the build (exe SHA-256 from `uk-qol report`) you
   tested on.

## When the game updates (maintainers)

The goal is a pattern update within a day or two of a game patch, so
players aren't left with a mod turned off for long. Most updates move code
around without changing it, and need only new patterns, no code.

1. **Open a pinned issue** as soon as the update is out (or the first
   game-update report arrives), titled `Game <version> (<date>): <mods>
   off`, label `game-update`, with the text below. Close new game-update
   reports as duplicates of it.
2. **Check the new build**: `uk-qol status` on the updated install lists
   each mod and every pattern that no longer resolves; the runtime log
   gives the exe SHA-256 and version.
3. **Fix the patterns** as in [Updating patterns](#updating-patterns-after-a-game-patch),
   keeping the old build's entries. If the code itself changed (a struct
   offset, a function's arguments), the mod needs a code fix and a new
   loader instead.
4. **Test in game** what each changed pattern does (the mod's own in-game
   checks in PLAN.md), on the new build.
5. **Release**: bump the version in `Cargo.toml`, tag `v<version>`. CI
   builds and publishes the loader (with the new patterns built in), the
   managers and `uk-qol-patterns.zip`. Installed managers offer the new
   loader; the patterns zip is only for a loader of unknown version (one
   installed with `--dll`).
6. **Update the pinned issue** with the release and close it after a few
   days.

Pinned issue text:

```markdown
Underkeep <version> came out on <date>. These mods turn themselves off on it
until their patterns are updated: <mods>. The other mods and the game work
normally.

Nothing is lost: settings and the mods' save data are kept, and the mods
come back on by themselves after the update.

When it's ready, the manager offers the update when it starts (or run
`uk-qol update`). This issue will say when.

Please don't open a new issue for this update; add a comment here if
something else broke.
```

## Pull requests

- Keep each PR to one change; run fmt, clippy and the tests first.
- CI doesn't run on pull requests automatically. A maintainer starts a build
  from the Actions tab when one is needed.
- By contributing you agree your work is released under the [MIT license](LICENSE).
