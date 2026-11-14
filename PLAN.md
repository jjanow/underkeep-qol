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

- [x] Build detection: exe SHA-256, game version string if the exe has one.
  - `runtime::build`. The version comes from the `.rdata` literal
    `version X.Y.Z` (the title screen's), falling back to a lone bare `X.Y.Z`
    string. The exe has no VERSIONINFO resource. The runtime logs
    `exe sha256 <hex>, game version <v>` at start.
  - 1.0.5 (Steam, 2026-11-11): `d3158ad0f94d6770deb64b32d48aeb412a3c5ef3a0b2e37a0e092a7ce66bb4a4`.
  - Saves record the version they were made with (the log says "Game
    version: 1.0.3" when loading one).
  - Seen in game (Proton, 2026-11-14): the log shows the 1.0.5 hash above and
    `game version 1.0.5`, stamped in the same second as the load line.
- [x] Byte-pattern scanner with wildcards, unique-match rule, RIP-relative
      and call-target resolution helpers. Unit tests against the current exe.
  - `runtime::pe` (minimal PE32+ reader; `map_file` lays a file out as
    mapped, so offline tools and the game see the same RVAs) and
    `runtime::pattern` (`Pattern`, `Image::scan`/`scan_section`,
    `rip_target`, `call_target`). Patterns can't start with a wildcard;
    code scans cover executable sections only.
  - The real-exe test stores no game bytes: it finds the `lea` that loads a
    known `.rdata` string, then cuts a pattern from that site and scans it
    back. A full `.text` scan takes ~150 ms in a debug build.
- [ ] Hook layer (detours + byte patches), applied and removed per mod.
- [ ] Panic and SEH containment around every hook and callback; failing
      mod is disabled for the session.
- [ ] Safe mode: crash counter across launches, Shift-at-start override.
- [ ] Config: `uk_qol/config.toml`, per-mod tables, defaults from the
      mod manifest, reload on change.
- [ ] Mod host: `Mod` trait (`on_load`, `on_unload`, `on_frame`,
      `on_input`, `on_save`, `on_load_save`), manifest + `patterns.toml` loading.
- [x] Save hooks: find the game's save and load functions; write and read
      `<save name>.qol.toml` sidecars keyed by hero.
  - `runtime::savehook`, patterns in `crates/runtime/patterns/core.toml`
    (override: `uk_qol/patterns/core.toml`; mod id `core` is reserved).
    Detours, each calling the original first: SaveGame_Write (every save
    path) → `Host::save` and write the sidecar; Game_LoadFromSlot →
    read it and `Host::load_save` (no sidecar = empty state);
    Save_DeleteFile → delete the sidecar; Party_InitFromPartySetup (only
    called for a new game) → empty state. Saving starts from the state of
    the game in play, not from the target file's old sidecar.
  - The game writes saves only through `ISteamRemoteStorage` (1.0.5).
    Sidecars stay **local**: `uk_qol/saves/<account>/<save>.qol.toml`,
    `<account>` from `SteamAPI_SteamUser_v023` /
    `SteamAPI_ISteamUser_GetSteamID` (low 32 bits), `default` without Steam.
    Writing them through FileWrite would sync them; that's an open question.
  - Hero keys: `hero_id` is only the creation slot, so mods key heroes by
    `savehook::hero_key` = `"<hero_id>:<name>"`; `savehook::party_heroes()`
    reads the 4 heroes at `g_party+0x10` (stride 0x2b8; valid +0, hero_id
    +8, name String +0x10, class +0x94), every read checked with
    `VirtualQuery` (`runtime::game`).
  - In game (Proton, 2026-11-14): loading save_1 logged `no sidecar at
    …\uk_qol\saves\4229148\save_1.qol.toml` and the party
    `0:Odena (paladin), 1:Kabam (thief), 2:Brazkin (bloodmage), 3:Ingwam
    (voidcaller)`; a new save wrote `save_2.qol.toml`, loading it read it,
    deleting it in game removed it; "Autosave and Quit" wrote
    `save_auto.qol.toml`; Quick Start logged `new-game hook: new party; mod
    state reset`. The account id matched Steam's `userdata/4229148`.
  - A sidecar that can't be read (tested with `format = 99`) is logged on
    load and left as it is when the game saves over that slot (merged build
    with the overlay, same day).
- [ ] Note in the log which mods were active, for crash triage (the game's
      BugSplat reporter will also see our crashes).

## 3. Data patching

- [x] `dft` crate: read and write the DfT format (header, string table,
      varint records, value types). Round-trip test on every `base/*.txt.rfile`.
      Reuse Underkeeper's `dft.rs` as the starting point. (Done in §0.)
- [x] Hook the rfile loader and apply in-memory patches from `patches/`.
  - `runtime::datapatch`: detour of DefTable_Parse (data, size, name), the
    funnel of every DfT parse (defs, text, levels, saves, profile). Patches
    from active mods (`Mod::patches`) then `uk_qol/patches/*.toml`, fixed at
    start; the patched buffer is leaked (kept for the process lifetime) and
    reused for identical input. Saves and `player_profile` are refused.
  - Names the loader passes are `<folder>/<name>.txt` (`def/def_skill.txt`,
    `text/text_common_en.txt`, `menu/def_menu.txt`), as `dft::patch`
    assumed; `log_defs = true` under `[runtime]` in config.toml lists them.
    Saves arrive as `save_1` / `save_auto`, the profile as `player_profile`,
    levels as `level/<Name>.level`.
  - In game (2026-11-14): a user patch setting
    `text_common_en.menu_new_game` showed "QOL PATCHED" on the main menu.
  - Timing: core hooks installed at .519, the startup gate released the game
    at .519 ("waited 11 ms"), the first DefTable_Parse (`def/def.txt`) came
    1.27 s later, so every def load is patched.
- [ ] Patch format: path-based set/add/remove on records, declarative TOML.

## 4. Overlay and input

- [x] Hook `SDL_GL_SwapWindow` and `SDL_PollEvent` (named SDL3 exports).
  - Done by IAT patching of `underkeep.exe` (`runtime::iat`), not detours:
    the exe imports both by name from `SDL3.dll`, and each name occurs once
    in the exe (only in the import table), so it doesn't fetch them through
    `GetProcAddress`/`SDL_GL_GetProcAddress`. SDL's internal calls aren't
    redirected (not needed). `overlay::install()` runs from the runtime
    thread right after the first log line; the exe's imports are bound by
    then. A slot that doesn't hold SDL's own address is chained and logged.
  - Also hooked: `SDL_GetGamepadButton` and `SDL_GetGamepadAxis`, which the
    game imports to poll pad state; they read "released" while the menu is
    open and for buttons whose press was swallowed. Optional: missing imports
    are logged, not fatal.
  - The game ships SDL 3.4.18 (`SDL-release-3.4.18` in `SDL3.dll`). Event
    layouts and constants (`overlay::sdl`) are from the 3.4.18 headers, with
    size/offset tests.
  - Panic containment: each hook calls the original SDL function exactly
    once, outside `guard::call_or("overlay", …)`; a panic in the overlay
    delivers the event / presents the frame, and after one the hooks are
    pass-throughs.
  - Wine test (2026-11-14, Proton 11 wine, fresh prefix): a MinGW SDL3 GL
    program linked against the game's `SDL3.dll` (copied to scratch, not the
    repo) with the proxy: hooks installed, a synthetic F10 opens the menu
    and never reaches the program, and GL state compared before and after
    every `SDL_GL_SwapWindow` (program, VAO, buffers, texture unit and
    binding, viewport, scissor, blend, depth, cull, stencil, sRGB, color
    mask, unpack state, framebuffers) is identical over ~2400 frames on a
    3.3 core and a 4.6 compatibility context. Not yet tried in the game.
- [x] egui on the game's GL context; F10 opens the mod menu.
  - Renderer: **egui + egui_glow** (0.37), not Dear ImGui: no MinGW C++
    compiler is installed, a C++ build would add a C toolchain to CI and risk
    libstdc++/libgcc imports, and egui is pure Rust and matches the manager.
    GL functions are loaded through `SDL_GL_GetProcAddress` (opengl32 is not
    imported). Default fonts cover Latin, Czech/Polish and Cyrillic.
  - The DLL grows to ~4.7 MB and imports `bcryptprimitives.dll` (std's
    random seed for egui's hash maps; a system DLL on Windows 10+ and Wine).
  - Every GL state egui touches is saved and restored around the frame
    (`overlay::gl`, a drop guard, so a panic also restores); the painter is
    created inside that scope. The context version and profile are logged:
    `overlay: GL 3.3 core profile; GL_VERSION …`. A new GL context (e.g. after
    a mode change) gets a new renderer.
  - UI scale: 1 at 1080 lines and below (Steam Deck), proportional above.
    While open the cursor is shown if the game hid it, and restored on close.
- [x] Mod list, settings forms generated from manifests, per-mod status
      and failure reasons.
  - The host registers `overlay::set_host(Arc<dyn OverlayHost>)` (mod rows:
    name, version, on/off, `ModState` Active/Off/Failed(reason)/
    Unsupported(reason), settings with `SettingKind` bool/int/float/string/
    enum/key) and `overlay::set_input_sink(fn(&InputEvent) -> bool)`.
    `SettingKind::from_manifest` reads `type`, `min`, `max`, `choices` from
    a `[settings.<key>]` table. Not wired to the host yet.
  - The sink gets presses and releases (a release's return value is
    ignored). Host and sink run inside the overlay's guard, so the host must
    wrap each mod call in `guard::call(mod_id, …)`. If the overlay itself
    fails, the menu is force-closed so pad polling and the cursor recover.
  - The log notes each change of the game's text-input state
    (`overlay: game text input on/off`), since hotkeys pause while it's on.
- [x] Hotkey system on SDL events: keyboard and gamepad, rebindable,
      swallows the event when a mod handles it.
  - `runtime::hotkey`: specs `S`, `Shift+1`, `Ctrl+Alt+F5`, `Num5`, `Pad:Y`,
    `Pad:LB+Pad:X`, `Pad:LT` (triggers count as pressed past half way);
    modifiers match exactly. A `key` setting is a hotkey: the host calls
    `hotkey::set_binding`, mods test `hotkey::pressed` in `on_input`.
  - A swallowed press's release and repeats are swallowed too; releases of
    keys the game saw pressed always reach it (no stuck keys when the menu
    opens). No mod input while the game has SDL text input active or the
    menu is open.
  - Rebinding in the menu: "Change", then a key (with modifiers) or pad
    buttons; a pad chord is captured when its first button is released.
- [x] Gamepad navigation of the overlay (Steam Deck).
  - L3+R3 (`Pad:LS+Pad:RS`) opens and closes the menu: stick clicks are
    the least likely game binding. The first stick's press reaches the game.
    D-pad or left stick moves focus (with repeat), A activates, B closes
    (or leaves a text field), LB/RB page. Not yet checked with a real pad or
    whether the game uses L3/R3.
- [x] Translatable strings (en first; cs, de, es, fr, pl, ru later).
  - `runtime::i18n`: ids with English text; empty tables for cs, de, es, fr,
    pl, ru fall back to English. The language comes from SDL's preferred
    locales unless `i18n::set_lang` was called.

## 5. Reverse engineering per mod

Do in the current Ghidra project of the exe; record function names,
offsets and patterns in each mod's `patterns.toml` with a comment.

- [ ] **Blood Mage:** the bone check and payment in the spell-cost path
      (start from the skill opcode handlers, e.g. `blood_pact` 0xa5).
      Where bones are counted (party inventory item?).
- [ ] **Inventory:** inventory container struct, item struct (category,
      rarity, value, stack size, max stack), "dragging" and trade state,
      the function that marks the inventory changed / refreshes the UI.
- [x] **Weapon swap:** how the character sheet draws and handles equipment
      slots (to add a ranged slot that behaves like them), the game's equip
      function, how combat turns are spent (to charge a turn on swap), the
      `ranged_weapon` flag, two-handed weapons vs shields, how to serialize
      an item into the sidecar and rebuild it on load.
  - 2026-11-14, game 1.0.5 (`mods/weapon-swap/patterns.toml`):
    `Hero::EquipItem(hero, item, slot, mode, notify)`; mode 1 = from the
    inventory panel (mode 3, the HUD hand slot, reads the mouse cursor
    object, which is null with a gamepad: crash). A two-handed weapon
    unequips the shield into the inventory. Ranged = item type 1, weapon
    type 5 (bows, crossbows and slings). An item is def name + count +
    cooldown (`Item::SaveState`); `Item_Create(def)` rebuilds it;
    `Item_Take(item, all, n)` (the game's "used up" path) removes and
    deletes one. In battle = the game's own check (living, aware monster);
    a hero's turn ends with round action 3 (`TurnManager` queue), which
    sets `+0x208`/`+0x209` and moves on. `g_state` is 4 after loading a
    save.
  - Still open: the character sheet's slot drawing and the drag state.
    Leads: the cursor object `*0x140442bc8` (+0x58 drag phase, +0x60 drag
    source kind, +0x7c/+0x80 slot/hero; null with a gamepad), the HUD
    hand-slot drop in `0x14021d310`, equipment slot rects at
    `0x140440dc4 + slot*0x28` while UI mode `0x140440430` is 6.
- [ ] **Skill slots:** how `selected_skill_slot` and the skill slot are
      stored per hero, the cast path from the HUD button, the HUD layout
      entries (`hud_skill_slot_*`, `face_hand_slot_*` in `def_def`).

- [ ] **Skip logos:** where `anim_startup_logo` (Rake in Grass and FMOD
      logos, ~3 s on every launch) is started and how it hands over to the
      main menu.
  - 2026-11-14, game 1.0.5: launch shows ~1 s of black window, the logos,
    then the main menu. The intro cinematic plays only on first run
    (`intro_was_played 1` in `game_config.txt`), so no mod is needed for it.

## 6. Mods

In order of difficulty; each ships when done, with settings and tests.

- [ ] `skip-logos`: start on the main menu without the startup logos; one
      on/off setting, nothing saved.
- [ ] `bloodmage-cost`: per-hero `bones` / `health` toggle, hotkey, icon on
      the skill button, sidecar persistence.
- [ ] `inventory-sort`: category → rarity/value → name, merge stacks,
      skip equipped and quick slots; hotkey **S** first, then the overlay
      button anchored to the inventory panel.
- [ ] `weapon-swap`: ranged slot on the character sheet (ranged weapons
      only), drag in/out like other slots, swap via the game's equip function;
      Shift+1..4 and a selected-hero key; `swap_costs_turn` (default true);
      the slot's item saved to and restored from the sidecar.
  - 2026-11-14, 0.1.0 works in game 1.0.5: slot per hero in the mod's
    overlay menu section (store the bow in hand or one from the inventory,
    swap, move to inventory), symmetric swap through `Hero::EquipItem`,
    keys (tested with pad bindings), sidecar save / load / other save
    loaded with an empty slot; item counts logged before and after every
    action, all conserved.
  - Not tested in game: a swap in combat (turn cost), a level change, a
    two-handed bow pushing a shield out and back.
  - Open: the slot on the character sheet with drag and drop (see §5).
- [x] Ranged-slot loss warning: in README, as a first-use notice in game
      (acknowledged once per install, reopenable from the overlay) and in
      the manager before uninstall. The player handles emptying slots.
  - The notice is the mod section's first content until acknowledged
    (`uk_qol/weapon-swap.notice-acknowledged`); putting a weapon into a
    slot or swapping is refused until then.
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

- Should sidecars sync through Steam Cloud? The game writes its saves only
  through `ISteamRemoteStorage::FileWrite`; writing sidecars the same way
  (e.g. `qol_save_1.toml`; the save list ignores names with a `.`) would
  sync them, but uses the game's cloud quota and puts our files in the
  player's cloud. Now they are local (`uk_qol/saves/`, §2): mod state is
  per computer. User decision.
- Is a macOS build of the runtime needed, or only the manager (game runs in
  CrossOver/Whisky, so the Windows DLL applies)?
