# Plan

What needs to be done next, in order. The design is in [README.md](README.md).
Check items off as they land; add findings under the item they belong to.

## 0. Groundwork

- [x] Cargo workspace: `crates/proxy`, `crates/runtime`, `crates/dft`,
      `crates/manager`; edition 2024, release profile as in Underkeeper.
  - Deviation: `panic = "unwind"` (Underkeeper uses abort). Panic containment
    in §2 needs unwinding, and Cargo can't set `panic` per package.
  - `dft` is Underkeeper's `dft.rs`, with the save-fragment hex test replaced
    by a hand-built document (no game data in the repo). With `UNDERKEEP_DIR`
    set, all 107 DfT files of the current install round-trip byte for byte.
  - `manager` is a command skeleton (`uk-qol`, commands print "not implemented").
- [x] Cross-compile setup: `x86_64-pc-windows-gnu` target and MinGW-w64 on
      Linux; document it in README *Building*.
  - The built `winmm.dll` imports only kernel32, ntdll, msvcrt and
    api-ms-win-core-synch; no MinGW runtime DLLs.
- [x] CI (GitHub Actions): build the Windows DLLs and the Linux/Windows
      manager; `cargo fmt`, `clippy`, tests.
  - Runs **only on `v*` tags and manual runs** (`workflow_dispatch`), not on
    pushes or PRs: builds happen only for a release or a deliberate test.
    Tags publish a release with `SHA256SUMS`.
- [x] `.gitignore` blocking game file types (no game material in the repo).
- [x] `CONTRIBUTING.md`: how to add a mod and update patterns, and the rule
      that no game files, assets, extracted text or decompiled code go in
      the repo.
- [x] Issue templates (bug report asks for `uk-qol report`; game-update template).

## 1. Prove the loader (go/no-go for the whole design)

- [x] `winmm.dll` proxy: forward every winmm export to the system DLL,
      start a thread that writes `uk_qol/uk_qol.log`.
  - The game's only winmm users: `SDL3.dll` imports `timeBeginPeriod` and
    `timeEndPeriod`, `fmod.dll` imports `timeGetTime`.
  - Exports (`crates/proxy/winmm.exports`, 217 names) are the union of Wine's
    winmm (186) and mingw-w64's `winmm.def` (209), which follows Windows. The
    31 Windows-only names are WOW thunks, `*32Message` driver entry points,
    the `gfx*` effects API, logon and debug helpers; 8 names are Wine-only
    (`Drv*`, `OpenDriverA`, `GetDriverFlags`, `mmioInstallIOProc16`). A name
    the system DLL lacks resolves to a stub returning 0, and the log says how
    many (31 under Proton). Ordinal-only exports (Windows' nameless ordinal 2)
    aren't forwarded.
  - Each export is an asm stub jumping through a table that is filled on the
    first call (LoadLibrary of `<system dir>\winmm.dll`), not in DllMain.
    PE forwarders can't be used: `winmm.x` would resolve back to the proxy.
  - The DLL also exports `DllMain` (harmless; the real winmm doesn't).
  - Smoke test (2026-11-14): a MinGW test exe calling `timeBeginPeriod`,
    `timeGetTime`, `timeEndPeriod`, run with Proton 11's wine and
    `WINEDLLOVERRIDES="winmm=n,b"` in a fresh prefix: calls forwarded (no
    self-recursion through the full-path LoadLibrary), exits cleanly, log
    line written.
  - In the game folder only `SDL3.dll` and `fmod.dll` import winmm; the
    BugSplat reporter (`BsSndRpt64.exe`) doesn't, so it won't start the runtime.
- [x] Confirm it loads on **Steam + Proton** with
      `WINEDLLOVERRIDES="winmm=n,b" %command%`.
  - 2026-11-14: Flatpak Steam, Proton Hotfix (`hotfix-20261110`), game 1.0.5.
    The log shows `uk-qol 0.1.0 loaded into S:\steamapps\common\Underkeep\underkeep.exe`.
    The launch option is set in `userdata/<account>/config/localconfig.vdf`
    (`LaunchOptions` in the app's block) while Steam is closed.
- [ ] Confirm it loads on **Windows** with no extra setup.
  - Every module in the process that imports winmm binds to the proxy
    (Steam overlay, Discord, GPU driver DLLs, MangoHud). Check the Steam
    overlay still works, and diff our export list against a real
    `System32\winmm.dll`: a name missing from ours makes its importer fail to load.
  - Under Proton `GameOverlayRenderer64.dll` is mapped into the game with the
    proxy in place; the overlay itself wasn't opened. The export list now
    covers mingw-w64's Windows list, but hasn't been checked against a real
    `System32\winmm.dll`.
- [x] Confirm the game runs normally with the proxy and no mods: audio
      (fmod uses winmm), controller input, save/load, quit.
  - Proton, 2026-11-14: the game's audio stream plays (a 4 s capture of it
    is not silent); a virtual Xbox 360 pad (uinput, `python-evdev`) is opened
    by SDL and drives the menus; a save loads, a new save is written and
    deleted in game; quit works from the menu and by closing the window.
    Not yet checked with a real pad or through Steam Input, nor on Windows.
  - Saves are in Steam Cloud storage: `userdata/<account>/2411200/remote/`
    (`save_1`, `save_auto`, `player_profile`); the game log says "Save
    location: Steam cloud". `game_log.txt` and `game_config.txt` are in the
    prefix's `Documents/Underkeep/`. The log is rewritten each launch.
- [ ] Fallback if winmm fails anywhere: `version.dll` (also imported by
      SDL3). Record the result here.
  - Not needed on Proton. Open until Windows is tested.

## 2. Runtime core

- [ ] Build detection: exe SHA-256, game version string if the exe has one.
  - The title screen shows `version 1.0.5`; saves record the version they
    were made with (the log says "Game version: 1.0.3" when loading one).
- [ ] Byte-pattern scanner with wildcards, unique-match rule, RIP-relative
      and call-target resolution helpers. Unit tests against the current exe.
- [ ] Hook layer (detours + byte patches), applied and removed per mod.
- [ ] Panic and SEH containment around every hook and callback; failing
      mod is disabled for the session.
- [ ] Safe mode: crash counter across launches, Shift-at-start override.
- [ ] Config: `uk_qol/config.toml`, per-mod tables, defaults from the
      mod manifest, reload on change.
- [ ] Mod host: `Mod` trait (`on_load`, `on_unload`, `on_frame`,
      `on_input`, `on_save`, `on_load_save`), manifest + `patterns.toml` loading.
- [ ] Save hooks: find the game's save and load functions; write and read
      `<save name>.qol.toml` sidecars keyed by hero.
  - The game saves through Steam Cloud (see §1), so "next to the save" means
    Steam's `remote/` folder, outside the game folder. Find out whether the
    game writes there itself or through `ISteamRemoteStorage`, and pick the
    sidecar location from that.
- [ ] Note in the log which mods were active, for crash triage (the game's
      BugSplat reporter will also see our crashes).

## 3. Data patching

- [x] `dft` crate: read and write the DfT format (header, string table,
      varint records, value types). Round-trip test on every `base/*.txt.rfile`.
      Reuse Underkeeper's `dft.rs` as the starting point. (Done in §0.)
- [ ] Hook the rfile loader and apply in-memory patches from `patches/`.
- [ ] Patch format: path-based set/add/remove on records, declarative TOML.

## 4. Overlay and input

- [ ] Hook `SDL_GL_SwapWindow` and `SDL_PollEvent` (named SDL3 exports).
- [ ] Dear ImGui on the game's GL context; F10 opens the mod menu.
- [ ] Mod list, settings forms generated from manifests, per-mod status
      and failure reasons.
- [ ] Hotkey system on SDL events: keyboard and gamepad, rebindable,
      swallows the event when a mod handles it.
- [ ] Gamepad navigation of the overlay (Steam Deck).
- [ ] Translatable strings (en first; cs, de, es, fr, pl, ru later).

## 5. Reverse engineering per mod

Do in the current Ghidra project of the exe; record function names,
offsets and patterns in each mod's `patterns.toml` with a comment.

- [ ] **Blood Mage:** the bone check and payment in the spell-cost path
      (start from the skill opcode handlers, e.g. `blood_pact` 0xa5).
      Where bones are counted (party inventory item?).
- [ ] **Inventory:** inventory container struct, item struct (category,
      rarity, value, stack size, max stack), "dragging" and trade state,
      the function that marks the inventory changed / refreshes the UI.
- [ ] **Weapon swap:** how the character sheet draws and handles equipment
      slots (to add a ranged slot that behaves like them), the game's equip
      function, how combat turns are spent (to charge a turn on swap), the
      `ranged_weapon` flag, two-handed weapons vs shields, how to serialize
      an item into the sidecar and rebuild it on load.
- [ ] **Skill slots:** how `selected_skill_slot` and the skill slot are
      stored per hero, the cast path from the HUD button, the HUD layout
      entries (`hud_skill_slot_*`, `face_hand_slot_*` in `def_def`).

## 6. Mods

In order of difficulty; each ships when done, with settings and tests.

- [ ] `bloodmage-cost`: per-hero `bones` / `health` toggle, hotkey, icon on
      the skill button, sidecar persistence.
- [ ] `inventory-sort`: category → rarity/value → name, merge stacks,
      skip equipped and quick slots; hotkey **S** first, then the overlay
      button anchored to the inventory panel.
- [ ] `weapon-swap`: ranged slot on the character sheet (ranged weapons
      only), drag in/out like other slots, swap via the game's equip function;
      Shift+1..4 and a selected-hero key; `swap_costs_turn` (default true);
      the slot's item saved to and restored from the sidecar.
- [ ] Ranged-slot loss warning: in README, as a first-use notice in game
      (acknowledged once per install, reopenable from the overlay) and in
      the manager before uninstall. The player handles emptying slots.
- [ ] `extra-skill-slots`: 3 extra slots (1–5), HUD layout data patch,
      right-click to assign, unbound keys, sidecar persistence.

## 7. Manager

- [ ] Install discovery shared with Underkeeper (Steam, Flatpak, Proton,
      CrossOver/Whisky bottles).
- [ ] CLI: `detect`, `install`, `status`, `enable`, `disable`, `config`,
      `uninstall`, `report`.
- [ ] GUI (egui): install/uninstall, mod switches and settings,
      compatibility panel, Linux launch-option copy button and check.
- [ ] Refuse to change files while the game runs.
- [ ] Self-update and mod/pattern updates from GitHub releases with
      SHA-256 checks.

## 8. Release

- [ ] Tagged release via CI: Windows, Linux, macOS manager builds +
      runtime zip + checksums.
- [ ] Test matrix: Windows, Linux Proton, Steam Deck; fresh install,
      update, uninstall, save loads without mods.
- [ ] README: replace "planned" with real install steps and screenshots.
- [ ] Process for game patches: pattern update turnaround, a pinned issue
      for "game updated, mod X off".

## Open questions

- Does Steam Cloud sync files next to the saves that the game
  didn't write? Test; if not, document it (state is per computer). The saves
  are in Steam's `userdata/<account>/2411200/remote/` (see §1).
- Is a macOS build of the runtime needed, or only the manager (game runs in
  CrossOver/Whisky, so the Windows DLL applies)?
