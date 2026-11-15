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
- [x] Hook layer (detours + byte patches), applied and removed per mod.
  - `runtime::hook` (`HookSet`): detours through a relay within ±2 GB, with
    a 14-byte absolute jump as fallback; byte patches restore the originals.
    A hook overwritten by another tool is left in place on disable.
- [x] Panic and SEH containment around every hook and callback; failing
      mod is disabled for the session.
  - `runtime::guard`: Rust panics are contained (`call`, `call_or` falls
    back to the original). Hardware exceptions aren't recovered: the crash
    handler logs the active mod and marks the session for safe mode.
- [x] Safe mode: crash counter across launches, Shift-at-start override.
  - `runtime::safe`, state in `uk_qol/session.toml`; two abnormal exits in a
    row turn all mods off. `uk-qol reset-safe-mode` clears it.
- [x] Config: `uk_qol/config.toml`, per-mod tables, defaults from the
      mod manifest, reload on change.
  - `runtime::config`, shared with the manager; unknown keys are kept, a
    file that doesn't parse is never overwritten; reload polls mtime/length.
- [x] Mod host: `Mod` trait (`on_load`, `on_unload`, `on_frame`,
      `on_input`, `on_save`, `on_load_save`), manifest + `patterns.toml` loading.
  - `runtime::host`; `runtime::bridge` wires it to the overlay, hotkeys,
    frames and config.
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
    Sidecars go there too (next item); the local fallback is
    `uk_qol/saves/<account>/<save>.qol.toml`, `<account>` from
    `SteamAPI_SteamUser_v023` / `SteamAPI_ISteamUser_GetSteamID` (low 32
    bits), `default` without Steam.
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
- [x] Sync sidecars through Steam Cloud (decided 2026-11-14): write and read
      them through `ISteamRemoteStorage` like the saves (e.g.
      `qol_save_1.toml`; the game's save list ignores names with a `.`), so
      mod state, and stored weapons above all, follows a save to other PCs
      and the Deck. Without Steam, stay local. Move existing local sidecars
      into the cloud once, without losing one that's newer locally.
  - 2026-11-14: `runtime::steam` (flat exports `SteamAPI_SteamRemoteStorage_v016`,
    FileWrite/Read/Exists/Delete, GetFileSize/Timestamp; any missing → local,
    logged once), `save::Store`, name `qol_<save>.toml`. Read = the newer of
    cloud and a local leftover (tie: cloud); write = cloud, local if
    FileWrite fails; delete = both. Migration runs at the first hook of each
    session with Steam: a local sidecar whose save is in the cloud is
    uploaded byte for byte if newer than the cloud copy, then moved to
    `saves/<account>/migrated/` (never replacing an older backup); others
    stay. `saves/default/` is never migrated. Unreadable guard keyed by save
    name, so it covers whichever copy was read.
  - Save list (1.0.5, `0x1401c27b0`): the only caller of the cloud listing
    (`GetFileCount`/`GetFileNameAndSize`, `0x1400633a0`) keeps names matching
    the anchored, case-sensitive glob `save_*` (`0x1400291a0`), then drops
    any with an extension. `qol_*.toml` fails both.
  - In game (Proton, 2026-11-14): the first load migrated `save_1` and
    `save_auto` sidecars (copied, local moved to `migrated/`) and read
    `qol_save_1.toml`; a new save wrote `qol_save_2.toml` in `remote/`, loading
    it read it, deleting it in game removed it; the load and save lists
    showed only the saves. After exit `remotecache.vdf` had the `qol_*`
    files synced (`syncstate` 1, `remotetime` set) and dropped
    `qol_save_2.toml`. A newer local `format = 99` sidecar was read, flagged,
    and left (local and cloud) when the game saved over that slot.
  - Not tested: a second PC / Deck pulling the files, the no-Steam path in
    game (unit tests only), a full cloud quota, an unreadable file in the
    cloud itself (same code path as local).
- [x] Note in the log which mods were active, for crash triage (the game's
      BugSplat reporter will also see our crashes).
  - `active mods: …` in `uk_qol.log` and in `session.toml`; the next start
    logs which mods the previous abnormal session had.

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
- [x] Patch format: path-based set/add/remove on records, declarative TOML.
  - `dft::patch`; documented in CONTRIBUTING, "Data patches".

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
    3.3 core and a 4.6 compatibility context.
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
- [x] HUD widgets for mods (`runtime::hud`, `Mod::has_hud` / `Mod::hud`;
      CONTRIBUTING, "HUD widgets").
  - 2026-11-14, game 1.0.5: the game's GUI is a virtual 640x360 space
    (y down), fitted into the window with `s = min(w/640, h/360)` and
    centred, `pixel = ((w - 640s)/2, (h - 360s)/2) + gui * s`, unrounded
    (the per-frame camera setup; the same values are its letterbox
    viewport, and the mouse goes back through the inverse). The HUD's named
    rects are f32 `{x, y, w, h}` in the `GuiLayout` global (`def_def`'s
    `gui` table), read live. The HUD is up when game state is 4, the HUD
    mode is 1, 2, 3 or 6 (0: no HUD; 11: the full-screen map, seen in
    game), and the menu state and HUD interaction mode are 0. Five core
    patterns.
  - The overlay runs a HUD pass only while an active mod's `has_hud` is true
    and the HUD is up; a mouse press inside a clickable widget of the last
    frame is swallowed with its release, everything else (and all motion)
    reaches the game. No gamepad focus on HUD widgets.
  - Checked in game with the `uk_qol/hud-debug` outlines: at 3440x1440
    (scale 4, bars of 440 px) and in a 1280x800 window (scale 2, bars of
    40 px) every layout rect sits on its HUD element. Clicks pushed as real
    SDL events at both sizes (a temporary test build): widget clicks reached
    the mods and were swallowed, a click on the dungeon view was delivered.
  - Cost: with the widgets of all four mods up, the overlay's draw takes
    about 0.14-0.19 ms of CPU per frame at 3440x1440 (600-frame averages).
    Nothing runs while no active mod has HUD widgets.
  - Not followed: the GUI camera's screen-shake offset. The character
    sheet (HUD mode 6) was seen in game with weapon-swap's sheet slot
    (2026-11-14). Not seen in game: alchemy (10, excluded), the widgets
    in a fight (static: nothing on the battle path sets the menu state or
    interaction mode; target selection hides them).
  - 2026-11-14 (Windows report: the cursor disappeared behind HUD
    widgets): the game draws its UI into an offscreen frame in three
    layers (panels, popups, the dragged item), copies it to the window,
    then draws its own cursor sprite, so widgets drawn at swap time covered
    both. The HUD (and toasts) now draw when the game submits its top UI
    layer (`overlay::layer`, core pattern `ui_layers_draw`), into the
    game's framebuffer (3440x1440 at a 3441x1440 window); the menu stays at
    swap time. Without the pattern the HUD draws at swap time and the
    game's cursor sprite (`g_cursor_cvar` / `g_cvars` pick cursor1..3) is
    redrawn over widgets under the pointer. Both checked in game 1.0.5 with
    pushed SDL events (a temporary test build): the cursor over the Blood
    Mage badge and the Sort button, a carried flail over the badge's
    tooltip and over the second weapon slot, the menu (F10); the fallback
    with `ui_layers_draw` broken by a pattern override. Not checked: a
    real mouse on Windows.

## 5. Reverse engineering per mod

Do in the current Ghidra project of the exe; record function names,
offsets and patterns in each mod's `patterns.toml` with a comment.

- [x] **Blood Mage:** the bone check and payment in the spell-cost path
      (start from the skill opcode handlers, e.g. `blood_pact` 0xa5).
      Where bones are counted (party inventory item?).
  - See `mods/bloodmage-cost/patterns.toml`.
- [x] **Inventory:** inventory container struct, item struct (category,
      rarity, value, stack size, max stack), "dragging" and trade state,
      the function that marks the inventory changed / refreshes the UI.
  - See `mods/inventory-sort/patterns.toml`.
- [x] **Weapon swap:** how the character sheet draws and handles equipment
      slots (to add a ranged slot that behaves like them), the game's equip
      function, how combat turns are spent (to charge a turn on swap), the
      `ranged_weapon` flag, two-handed weapons vs shields, how to serialize
      an item into the sidecar and rebuild it on load.
  - 2026-11-14, game 1.0.5 (`mods/weapon-swap/patterns.toml`):
    `Hero::EquipItem(hero, item, slot, mode, notify)`; mode 1 = from the
    inventory panel (mode 3, the HUD hand slot, reads the UI context
    through a pointer that is null outside the game's UI pass: crash). A two-handed weapon
    unequips the shield into the inventory. Ranged = item type 1, weapon
    type 5 (bows, crossbows and slings). An item is def name + count +
    cooldown (`Item::SaveState`); `Item_Create(def)` rebuilds it;
    `Item_Take(item, all, n)` (the game's "used up" path) removes and
    deletes one. In battle = the game's own check (living, aware monster);
    a hero's turn ends with round action 3 (`TurnManager` queue), which
    sets `+0x208`/`+0x209` and moves on. `g_state` is 4 after loading a
    save.
  - 2026-11-14 (game 1.0.5, Ghidra project `uk3`; `uk2` is an older exe):
    the GUI is an immediate-mode UI whose context the `cursor` global
    points at only during the UI pass (null between frames, pad or not);
    the context itself is in its own global (`ui_context`). Item drags are
    click to pick up, click to drop (drag style 1); the item stays where
    it was until a drop target equips or moves it, then the target ends
    the drag (`drag_end`). `drag_item` returns the carried item (kind 0: a
    hero's slot, 1: party inventory, 2: floor). The sheet (`char_sheet`,
    selected hero) rewrites its 11 slot rects every frame. All in
    `mods/weapon-swap/patterns.toml`.
- [x] **Skill slots:** how `selected_skill_slot` and the skill slot are
      stored per hero, the cast path from the HUD button, the HUD layout
      entries (`hud_skill_slot_*`, `face_hand_slot_*` in `def_def`).
  - 2026-11-14: `face_hand_slot_(2i+1)` / `(2i+2)` are hero i's weapon and
    skill slots (29x29 in GUI space), `face_rect_1..4` the portraits,
    `inventory_rect` the party inventory panel. The game's code reads a
    fixed set of rects, so new slots are overlay widgets (`runtime::hud`),
    not layout patches. The rest: `mods/extra-skill-slots/patterns.toml`.

- [x] **Skip logos:** where `anim_startup_logo` (Rake in Grass and FMOD
      logos, ~3 s on every launch) is started and how it hands over to the
      main menu.
  - The per-frame update's logo wait (`mods/skip-logos/patterns.toml`).
  - 2026-11-14, game 1.0.5: launch shows ~1 s of black window, the logos,
    then the main menu. The intro cinematic plays only on first run
    (`intro_was_played 1` in `game_config.txt`), so no mod is needed for it.

## 6. Mods

In order of difficulty; each ships when done, with settings and tests.

- [x] `skip-logos`: start on the main menu without the startup logos; one
      on/off setting, nothing saved.
  - Works in game 1.0.5; a startup gate holds the game until the hooks are in.
- [x] `bloodmage-cost`: per-hero `bones` / `health` toggle, hotkey, icon on
      the skill button, sidecar persistence.
  - 2026-11-14: HUD badge (**B** / **HP**) in each Blood Mage's skill slot
    corner, click toggles (`hud_badge`, default on); seen in game, the click
    path tested with pushed SDL clicks.
- [x] `inventory-sort`: category → rarity/value → name, merge stacks,
      skip equipped and quick slots; hotkey **S** first, then the overlay
      button anchored to the inventory panel.
  - 2026-11-14: **Sort** button in `inventory_rect`'s top-right corner,
    shown while a sort is allowed (window focus not required for showing),
    `hud_button` (default on); a pushed click sorted ("already sorted").
- [x] `weapon-swap`: ranged slot on the character sheet (ranged weapons
      only), drag in/out like other slots, swap via the game's equip function;
      Shift+1..4 and a selected-hero key; `swap_costs_turn` (default true);
      the slot's item saved to and restored from the sidecar.
  - 2026-11-14, 0.1.0 works in game 1.0.5: slot per hero in the mod's
    overlay menu section (store the bow in hand or one from the inventory,
    swap, move to inventory), symmetric swap through `Hero::EquipItem`,
    keys (tested with pad bindings), sidecar save / load / other save
    loaded with an empty slot; item counts logged before and after every
    action, all conserved.
  - Combat tested 2026-11-14 (new game, the giant rats): a swap ends the
    hero's turn (greyed out, next hero selected, monsters act after the
    last hero, next round normal); swaps during the monsters' turn are
    refused; `swap_costs_turn = false` swaps for free.
  - Not tested in game: a hero who already acted being refused, a level
    change (static: the level change runs `Game_SaveGameState` right
    before re-restoring the party, so its snapshot is current): no fight
    or stairs were reached in this session (the test save starts in the
    opening forest). New-game `g_state` 3 is static only.
  - 2026-11-14: the slot on the character sheet, an overlay widget left of
    the helmet slot (placed from the sheet's live slot rects), drawn like
    the game's empty slots (`sheet_slot`, default on; its bow silhouette
    was later replaced by the game's sword sprite and a "II", below). Tested in game with pushed SDL mouse and
    key events (3440x1440 and a 1706x960 window): a sling from the hand
    and from the inventory clicked into it, a shortbow onto the filled
    slot (the sling went to the inventory), the hand's bow onto the filled
    slot (a swap), a potion refused (red, stays on the cursor), the stored
    weapon clicked onto the sheet's weapon slot and the HUD hand slot
    (swaps) and onto the inventory, right-click to the inventory, Shift+3
    swaps; a two-handed sling pushed a buckler out and the swap back put
    it on. Item counts conserved in every action. A pushed click on the
    HUD hand slot didn't pick its weapon up while the sheet was open, so
    a bow from another hero's hand (same `Hero::Unequip` path) wasn't
    tried.
  - 2026-11-14: HUD button on the weapon hand slot of heroes with a filled
    slot, showing the stored weapon's initials; click swaps (`hud_button`,
    default on). Seen in game (a sling stored for Kabam: "Sl"); its click
    wasn't pushed (the same path as the other mods' buttons).
- [x] Ranged-slot loss warning: in README, as a first-use notice in game
      (acknowledged once per install, reopenable from the overlay) and in
      the manager before uninstall. The player handles emptying slots.
  - The notice is the mod section's first content until acknowledged
    (`uk_qol/weapon-swap.notice-acknowledged`); putting a weapon into a
    slot or swapping is refused until then.
- [x] `weapon-swap`: turn the ranged slot into a second weapon slot that
      takes any weapon (decided 2026-11-14), so the swap covers sword and
      shield ↔ two-hander and melee ↔ melee as well as melee ↔ ranged. Drop
      the ranged-only check (item type 1, weapon type 5); rename "ranged
      slot" in settings, overlay text, README, the loss warning and the
      manager's pre-uninstall text. Keep the shield behaviour (a two-hander
      puts it in the inventory, swapping back puts it on). Test in game:
      one-hander ↔ one-hander with a shield, two-hander ↔ sword and shield.
  - 2026-11-14: any item of type 1 (weapon) fits; shields are their own item
    type, so they never do. No second shield slot: a shield only moves for a
    two-hander, and the swap already puts it back. The sidecar key stays
    `ranged`, so existing sidecars load.
  - Icon (the user's choice): the game's own empty-weapon-slot sword with a
    pixel "II" drawn by the mod in the slot's top-right corner. The sword is
    read at run time from the player's install (`runtime::sprite`: sprite
    `hud_barbie_sword` in `base/_sprite.db`, its texture in `_texture.db`,
    cut out of the atlas PNG), so no game art is in the repo. The `.db`
    files are a format separate from DfT (`dft::db`, read-only). If the
    sprite can't be read, only the "II" is drawn (logged). The `png`
    decoder adds no DLL imports.
  - In game 1.0.5 (Proton, 2026-11-14, pushed SDL events at 3440x1440): the
    slot looks like the sheet's own slots. Mace in slot II with the rusty
    sword and a wooden shield on: Shift+1 swapped them, the shield stayed
    on; back again. Greatsword dropped on the filled slot (the mace went to
    the inventory), Shift+1: greatsword in hand, shield to the inventory;
    Shift+1: sword in hand, shield back on. Right-click emptied the slot.
    Item counts conserved in every action.
- [x] `extra-skill-slots`: 3 extra slots (1–5), HUD layout data patch,
      right-click to assign, unbound keys, sidecar persistence.
  - Deviation: overlay HUD buttons instead of a layout data patch (§5).
  - 2026-11-14: HUD buttons in a column right of each hero's skill slot
    (9 GUI units, smaller for 4–5 slots), the skill's initials, full name
    on hover; left-click casts for that hero (the same queue path as the
    keys), right-click or a click on an empty slot opens the game's skill
    panel for it (`hud_buttons`, default on). In game: a pushed left click
    cast through the game's checks ("Cooldown not finished yet!"), a right
    click opened the panel, closing it put the original skill back. Text
    labels, not the game's skill icons (their textures weren't traced).
- [x] `borderless-compat`: borderless fullscreen always takes the game's
      Compatibility ("emulated borderless") path; one on/off setting, on
      by default, nothing saved.
  - Why: players report that on Windows the default Performance mode takes
    the screen over like exclusive fullscreen (OpenGL), which breaks HDR;
    the game's own fix is the Compatibility setting, which few find.
  - 2026-11-14: a 6-byte patch of the display-mode setter's `je` to the
    emulated path (`mods/borderless-compat/patterns.toml`); the pattern
    matches once in 1.0.5. Leaving the mode uses the game's own state
    flag, so windowed and exclusive still work.
  - [ ] Test in game: HDR and Alt+Tab on Windows (§1).
    - Proton, 2026-11-14, game 1.0.5: with `fullscreen 1` and
      `borderless_fullscreen_mode 0` (Performance; both are the game's
      defaults, so `game_config.txt` leaves them out), the game log says
      `Changing display mode to emulated borderless fullscreen...`. Without
      the mod the same settings log `...to borderless fullscreen...`.

## 7. Manager

- [x] Install discovery shared with Underkeeper (Steam, Flatpak, Proton,
      CrossOver/Whisky bottles).
  - `manager::paths`, adapted from Underkeeper's (also native Steam, Snap).
- [x] CLI: `detect`, `install`, `status`, `enable`, `disable`, `config`,
      `uninstall`, `report`.
  - Also `update` and `reset-safe-mode`. Uninstall always keeps sidecars.
- [x] GUI (egui): install/uninstall, mod switches and settings,
      compatibility panel, Linux launch-option copy button and check.
  - The launch option is also written to `localconfig.vdf` while Steam is closed.
- [x] Refuse to change files while the game runs.
  - `manager::process`; Steam's config only while Steam is closed too.
- [x] Self-update and mod/pattern updates from GitHub releases with
      SHA-256 checks.
  - `manager::update`: every download is checked against `SHA256SUMS`.
    Not yet exercised against a real release (none published).

## 8. Release

- [ ] Tagged release via CI: Windows, Linux, macOS manager builds +
      runtime zip + checksums.
  - Manual run (`workflow_dispatch`, 2026-11-14, run 43648018936): check
    (fmt, both clippys, tests), runtime, Windows and Linux managers passed;
    the DLL's imports are system DLLs only, the CI-built Linux manager runs
    (`update --check`: no release yet). The macOS job failed at once:
    `macos-14` images are retired; now `macos-15`, and the rerun (run
    43648802829) passed every job.
  - 2026-11-14: version 1.0.0 (workspace and every `mod.toml`), tag `v1.0.0`
    pushed for the first release; Windows testing (§1, §6) is done from
    that build.
- [ ] Test matrix: Windows, Linux Proton, Steam Deck; fresh install,
      update, uninstall, save loads without mods.
- [x] README: replace "planned" with real install steps and screenshots.
  - 2026-11-14: *Installing* section (Windows, Linux, Deck, macOS, by hand,
    update, uninstall, safe mode), checked against the manager's labels.
    "(none published yet)" dropped for 1.0.0.
  - 2026-11-14: *Screenshots* section with `docs/screenshots/` (HUD widgets,
    slot II on the character sheet, the F10 menu, the manager). In-game
    shots are cropped and credited; the user decided they are the one
    exception to "no game material" (README and CONTRIBUTING say so). The
    manager shot is cropped to its right column (no home path or Steam
    account ids).
- [x] Process for game patches: pattern update turnaround, a pinned issue
      for "game updated, mod X off".
  - CONTRIBUTING, "When the game updates (maintainers)": a day or two,
    steps, and the pinned-issue text; the issue is opened at the first
    game update.
- [ ] macOS: no native runtime (decided 2026-11-14). The game runs in a
      CrossOver/Whisky/Wine bottle, which loads the Windows `winmm.dll`; ship
      that plus the existing arm64 manager. Document the bottle's
      `winmm=n,b` override in README and try it once in a bottle.
