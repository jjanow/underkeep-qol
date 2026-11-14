# Plan

What needs to be done next, in order. The design is in [README.md](README.md).
Check items off as they land; add findings under the item they belong to.

## 0. Groundwork

- [ ] Cargo workspace: `crates/proxy`, `crates/runtime`, `crates/dft`,
      `crates/manager`; edition 2024, release profile as in Underkeeper.
- [ ] Cross-compile setup: `x86_64-pc-windows-gnu` target and MinGW-w64 on
      Linux; document it in README *Building*.
- [ ] CI (GitHub Actions): build the Windows DLLs and the Linux/Windows
      manager on PRs and tags; `cargo fmt`, `clippy`, tests.
- [x] `.gitignore` blocking game file types (no game material in the repo).
- [ ] `CONTRIBUTING.md`: how to add a mod and update patterns, and the rule
      that no game files, assets, extracted text or decompiled code go in
      the repo.
- [ ] Issue templates (bug report asks for `uk-qol report`).

## 1. Prove the loader (go/no-go for the whole design)

- [ ] `winmm.dll` proxy: forward every winmm export to the system DLL,
      start a thread that writes `uk_qol/uk_qol.log`.
- [ ] Confirm it loads on **Steam + Proton** with
      `WINEDLLOVERRIDES="winmm=n,b" %command%`.
- [ ] Confirm it loads on **Windows** with no extra setup.
- [ ] Confirm the game runs normally with the proxy and no mods: audio
      (fmod uses winmm), controller input, save/load, quit.
- [ ] Fallback if winmm fails anywhere: `version.dll` (also imported by
      SDL3). Record the result here.

## 2. Runtime core

- [ ] Build detection: exe SHA-256, game version string if the exe has one.
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
- [ ] Note in the log which mods were active, for crash triage (the game's
      BugSplat reporter will also see our crashes).

## 3. Data patching

- [ ] `dft` crate: read and write the DfT format (header, string table,
      varint records, value types). Round-trip test on every `base/*.txt.rfile`.
      Reuse Underkeeper's `dft.rs` as the starting point.
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
  didn't write? Test; if not, document it (state is per computer).
- Is a macOS build of the runtime needed, or only the manager (game runs in
  CrossOver/Whisky, so the Windows DLL applies)?
