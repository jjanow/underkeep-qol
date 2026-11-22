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
    Tags publish a release with `SHA256SUMS`. A manual run can tick
    **Windows only** (`windows_only`) to skip the Linux and macOS managers
    for a quick test build (the macOS runner is slow to get).
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
- [x] Confirm it loads on **Windows** with no extra setup.
  - 2026-11-14, the user's first Windows test: it loads and runs. Found:
    the HUD widgets covered the game's cursor, slot II looked flat, and a
    held drag into slot II bounced back (all fixed, see §6 weapon-swap).
  - 2026-11-14, second Windows test (build d545bbd): the cursor layering is
    fixed; drops into slot II still bounce back to the hand, and the Sort
    button seemed to do nothing. Neither reproduces under Proton with a
    real (uinput) mouse (§6). The Sort button only logged its result (now a
    toast). For the drops, the log now carries what's needed: the build's
    commit on the first line, the window / pixel / point geometry
    (`overlay: window …`), and for the first 60 mouse presses while HUD
    widgets are up, where each landed and whether it hit a widget or went
    to the game (`overlay: mouse … down at …`). Next: the user's
    `uk_qol.log` from a build with these lines, the drag source and style
    (click-click or held), whether slot II turns green, and the display
    scale and window mode.
  - 2026-11-14, debugged on the user's Windows PC (3dd841a built there with
    the GNU toolchain + WinLibs MinGW; first native Windows build, imports
    unchanged): the drops reached the slot and were **refused because the
    second-slot notice wasn't acknowledged** (`uk_qol/` was fresh, so no
    `weapon-swap.notice-acknowledged`; the Proton install had one). The
    slot showed green and the refusal was a long, brief toast, so it looked
    like a bounce. The gate is gone (the user's decision: the warning lives
    in the README and slot II's tooltip only). Geometry was right
    (2560x1440, scale 4, presses on the sheet's weapon slot and on slot II).
  - Every module in the process that imports winmm binds to the proxy
    (Steam overlay, Discord, GPU driver DLLs, MangoHud). Check the Steam
    overlay still works, and diff our export list against a real
    `System32\winmm.dll`: a name missing from ours makes its importer fail to load.
  - Under Proton `GameOverlayRenderer64.dll` is mapped into the game with the
    proxy in place; the overlay itself wasn't opened. The export list now
    covers mingw-w64's Windows list, but hasn't been checked against a real
    `System32\winmm.dll`.
  - 2026-11-14, after the fixes above (824e396, built and run on the user's
    Windows PC): the user reports Windows working fine. The two checks above
    (overlay opened, export diff) weren't done separately; nothing in the
    process failed to load.
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
- [x] Fallback if winmm fails anywhere: `version.dll` (also imported by
      SDL3). Record the result here.
  - Not needed: winmm works on Proton and Windows (2026-11-14).
  - Dropped (2026-11-20): `version.dll` is what the Underkeep Mod Framework
    installs, so it is no longer free to use as a fallback. The two loaders
    can be installed side by side (README, "With other mod loaders").

## 2. Runtime core

- [x] Build detection: exe SHA-256, game version string if the exe has one.
  - `runtime::build`. The version comes from the `.rdata` literal
    `version X.Y.Z` (the title screen's), falling back to a lone bare `X.Y.Z`
    string. The exe has no VERSIONINFO resource. The runtime logs
    `exe sha256 <hex>, game version <v>` at start.
  - 1.0.5 (Steam, 2026-11-11): `d3158ad0f94d6770deb64b32d48aeb412a3c5ef3a0b2e37a0e092a7ce66bb4a4`.
  - 1.0.6 (Steam, 2026-11-19, "Character Respec"): `556f696519db3a948f1ef1f37e4fdc23293c330d00d93399380dd2c3606055ba`.
    Two required patterns missed, so weapon-swap and bloodmage-cost turned
    themselves off (a player's report: "weapon swap isn't working").
    `script_command` (still 0x1400dbc20) gained one command (switch bound
    0xa5 -> 0xa6) and a 0x10 smaller frame; its ids (0x18
    remove_party_item) and `script_condition`'s (0x10, 0x11) are unchanged.
    `CanCastSkill` moved to 0x140180fc0, recompiled with rsi/rdi but with the
    same five callers, return codes and offsets; the new function at
    0x140180f40 before it is called only by the new script command. Every
    other hooked function (mods and core) is the same code at a new address,
    apart from the skill tooltip builder (bigger frame, same arguments).
    `RecalcStats` now gives ranged DEX x 9 (was x 10). Globals moved +0x20.
    Fixed in 1.0.12 with per-build candidates. Proton, 2026-11-19: all ten
    mods active; Shift+1 swapped Odena's flail and sword; quicksave and
    quickload kept the slot (no mismatch); extra-skill-slots' tooltip shows
    the 1.0.6 text. Not tried on 1.0.6: a Blood Mage cast, quest-item
    scripts, Windows.
  - 2026-11-21: only the current Steam build is supported (Steam has no
    version branches, so every player gets the latest). The 1.0.5 pattern
    candidates are gone, every manifest lists the 1.0.6 build only, and the
    installed-game tests (patterns, `build::tests::game_exe_build`) check
    1.0.6. Enemy-hit armor re-checked in 1.0.6 (`0x14012e170`; the old
    `0x14012e090` note was a wrong address). RE comments that cite
    1.0.5 addresses are left as they are (each names its version).
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
  - Code writes are serialized (2026-11-14, first native Windows build):
    two writers on one page raced on `VirtualProtect`, one restoring
    read-execute mid-store of the other, and parallel `cargo test` died with
    0xc0000005 in about half the runs. 10/10 parallel runs pass since.
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
  - 2026-11-14: the result is a toast ("Inventory sorted", "… already
    sorted", "not sorted: not now", "sort failed"), and the button's tooltip
    says the order. Clicked with a real (uinput) mouse under Proton: works.
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
- [x] Slot II loss warning: in the README, plus a one-line reminder in
      slot II's tooltip. The player handles emptying slots.
  - 2026-11-14: the first-use notice that had to be acknowledged before the
    slot worked, the menu's notice and the manager's pre-uninstall text are
    removed (the user's decision: README and tooltip only, no blocker).
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
  - 2026-11-15 (player report: J'Axe refused as "a quest item"): quest
    weapons now go in slot II. The block existed because quest scripts look
    for them where the slot isn't: `encounter_jacob_release` asks
    `party_has_item item_jacob_hand_axe`, then `remove_party_item`s it (400
    XP), and `encounter_wizard_defeat` picks the good ending with
    `party_has_item item_soulforge_axe`. Those are the only weapons with
    `quest_item` (Item+0x433) and the only scripts that name them. The mod
    detours `script_condition` (0x1400da910; condition 0x10
    party_has_item, 0x11 party_has_all_items) and `script_command`
    (0x1400dbc20; command 0x18 remove_party_item). Before one runs, a slot
    II weapon named in its arguments (ctx+0x30) is created in the
    inventory; the slot is cleared on the mod's next frame or save, with a
    toast. In game (Proton, 2026-11-15): with a player patch adding
    `[party_has_item item_damaged_flail]` to the minor heal potion's
    `on_use`, the flail in Kabam's slot II went to the inventory when the
    potion was drunk (the hook read `["item_damaged_flail"]`), the slot
    emptied with the toast, and the potion worked as usual. The Jacob and
    wizard scenes themselves weren't reached, and `remove_party_item` was
    only checked statically (same argument layout, runs the same move).
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
  - 2026-11-14, after the user's Windows test (real mouse): three fixes.
    - The slot's background is now the game's stone, cut out of
      `hud_barbie_equip_slots` (the helmet's cell, x 39..77, y 0..37, stone
      top-left on the rect's), and the highlights are the game's
      `hud_barbie_slot_allow` / `_deny` / `_selected`; the flat drawing is
      the fallback if the sprite is missing or changes size.
  - 2026-11-14: the filled slot (and the weapon following the cursor after
    a click on it) shows the stored weapon's own icon instead of its
    initials on a green square. Each item def in `def/def_item*.txt`
    (`base/def_def_item*.txt.rfile`) names its sprite in `icon`, inherited
    through `parent` (`item_battle_axe` -> `item_axe_02`); the runtime reads
    them once (`sprite::item_icon`). 419 of the 420 item defs in game 1.0.5
    resolve to a sprite that loads (`item_coins` has none). Icons bigger
    than the slot (up to 29x97) are shrunk to fit, keeping their shape. The
    initials stay as the fallback, and on the 11-unit HUD swap button.
    - Held drags: the game's drag source (style 1) turns a press into a
      carry only when the button comes up over the pressed item; released
      elsewhere it cancels (`drag_end`), so a held drag moves nothing in the
      game either. While phase is 1 the context holds the pressed widget's
      id at +0x68 (HUD hand slot: `0x1c + 2 * hero, 0`; inventory: `panel,
      UI object id`), but no payload. The slot remembers the pressed item
      (inventory: `drag_item` with the payload filled in for the call, then
      restored; sheet weapon / two-hander's shield slot: by the pointer's
      position when the press is first seen) and drops it in when the
      phase goes from 1 to 0 with the pointer over the slot. In game (test
      build with pushed SDL events): Oris's two-handed sling held from the
      sheet's weapon slot onto slot II: green frame while held, in the slot
      on release, counts conserved. Held drags from the inventory: see the
      real-mouse test below.
    - The cursor covering: see the HUD widgets item (`ui_layers_draw`).
  - 2026-11-14, after the second Windows report (drops still bounce back),
    tested under Proton with a real mouse (a uinput device, so the whole OS
    input path, not pushed SDL events) on build d545bbd: from the sheet's
    weapon slot and from the inventory, click-then-click and held drags
    (also a 2 s hold and a slow drag) all put the weapon in slot II; counts
    conserved. Only a press and release within a frame or two bounced back
    (the press is never seen; no human drag is that fast). The HUD hand
    slot takes no clicks while the sheet is open (game behaviour), so it
    can't be a drag source there. Cause found on Windows: the unacknowledged
    notice gate (§1), now removed.
  - 2026-11-17 (player request: slot II showed no game tooltip): a filled
    slot II shows the game's own item tooltip (`tooltip.rs`). The game has
    one tooltip, armed per frame by `tooltip_show` 0x140167270 (text, pos,
    delay; clears the compare text 0x140371750) and drawn, then disarmed,
    by `tooltip_draw` 0x140166850 near the end of the frame (after the
    inventory panel 0x1401fc2c0). The HUD pass records the hovered slot;
    a detour of `tooltip_draw` arms the tooltip just before the original,
    at the layout point `inventory_item_tooltip_position` (lookup
    0x14011e950), with the compare text set to the weapon in hand, as the
    panel does for an inventory weapon. Texts come from `item_tooltip`
    0x140248ed0 (out, item, compare, hero, 0, 0) on a temporary item
    (`Item_Create`, then `Item_Take` at once), plus two appended lines (the
    right-click hint and the loss warning); rebuilt when the hover starts
    or the hero, weapon or hand weapon changes. The compare text must be
    set with String_Copy 0x140027970: 0x140027a00 *moves* (empties its
    source), which made the compare panel vanish after one frame. All
    tooltip patterns are optional; without them the slot keeps its egui
    text. In game (Proton, 1.0.5, uinput mouse, save 001): flail in
    Ingwam's slot (bare hands) shows its tooltip, "Required strength" red;
    in Kabam's slot it shows the arrows against her dagger and the
    dagger's **Equipped** panel, like the inventory flail; 15 hovers in and
    out with no flicker or stale tooltip; afterwards "items conserved" and
    nothing extra in the inventory or on the floor. Windows not tried.
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
  - 2026-11-15 (player request): a button is red while its skill is on
    cooldown ("on cooldown" in the hover text and the menu). Read
    where CanCastSkill (0x140180ed0) reads it: skill state +0x10 for a class
    skill, Item+0xd8+0x10 for an item skill; above 0 means "Cooldown not
    finished yet!". The game's own slot greys out for any refusal and shows
    no count; its tooltip prints the def's cooldown as a bare number, so the
    unit of what's left wasn't settled and no count is shown. Static RE
    only; not yet seen in game.
  - 2026-11-17 (player request): hovering a button with a skill shows the
    game's own skill tooltip (`tooltip.rs`), in the place the game's skill
    slot shows it, with two lines added: which slot and the clicks, and
    "On cooldown." while it is. The HUD skill slot (0x14021dd40, once per
    hero per frame, before the tooltip pass) builds its text with
    0x140246580 (out, hero, skill state, 1 class / 3 item skill, 1) and
    arms it at (dung_view.x - GuiLayout+0x40c - 6, dung_view.y). The mod
    detours that slot function and does the same for the hovered extra
    slot's hero (weapon-swap already holds the tooltip pass). All new
    patterns optional; without them the buttons keep their own text.
    Empty slots and skills the hero lost keep the mod's text. The added
    lines are English only. Proton, game 1.0.5: a class skill (Lay on
    Hands) showed the game's tooltip beside the dungeon view, where the
    game's own slot shows it; 10 hover cycles between the extra slot and
    the game's slot, no errors, nothing left behind. Not tried: an item
    skill, a slot on cooldown, Windows.
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
  - [x] Test in game: HDR and Alt+Tab on Windows (§1).
    - Proton, 2026-11-14, game 1.0.5: with `fullscreen 1` and
      `borderless_fullscreen_mode 0` (Performance; both are the game's
      defaults, so `game_config.txt` leaves them out), the game log says
      `Changing display mode to emulated borderless fullscreen...`. Without
      the mod the same settings log `...to borderless fullscreen...`.
    - Windows (user, 2026-11-14): works.
- [x] `item-highlight`: items on the ground, containers and switches are
      recolored in the dungeon view (default `#33FF33`, strength 0.85),
      each group switchable; **H** toggles, the choice is kept (`shown`).
  - How: a detour of QueueWorldSprite (`sprite_submit`) finds the entity
    from the sprite's pick id (`pick_lookup`) and its class from RTTI
    (Item, Container, Chest, Button, and Decorations named `*wall_lock*`);
    chests only while closed, alcoves only with items in them. The
    `DungObject` shader gets a branch for highlight amounts >= 2 through a
    `glShaderSource` wrapper (exe import of `SDL_GL_GetProcAddress`); the
    game's own highlight statement is kept for everything else.
  - Runtime: new `color` setting type (overlay and manager pickers) and
    `bridge::request_setting` for a mod's own setting changes.
  - [x] Proton, 2026-11-15, game 1.0.5: floor items, an alcove and the
    items in it glow; the toggle hides it and saves `shown`.
  - [ ] In game: a closed chest, a lever or wall button, a keyhole (only
    checked in the disassembly so far), the **H** key itself, and the
    fallback look when the mod is turned on mid-session.
  - [ ] Windows.
  - 2026-11-15: released in 1.0.2 (tag `v1.0.2`). The shader anchor (one
    statement of the game's GLSL, kept verbatim in the mod as its search
    string) was approved by the user.
- [x] `melee-accuracy` (asked 2026-11-16): melee hit chance gets
      `max(0, stat x per_point)` (default 2), the stat being the one the
      attack's damage scales on (DEX for `uses_dexterity` items, else STR,
      also unarmed). Off by default (balance change).
  - The rules (1.0.5): melee hit = `100 + Hero+0x160 - Monster+0x134 -
    Monster+0x130` in the hero melee attack 0x14012f830; ranged uses
    `Hero+0x164`, which `RecalcStats` (0x14017a8b0) fills with DEX x 10 +
    bonuses (DEX x 9 since game 1.0.6: `lea eax,[rcx+rcx*8]`). Only that function, ranged (0x140133160), thrown (0x140132230)
    and `SkillAddStatusEffectToEnemy` (0x140182910, a status chance)
    subtract monster evasion, so weapon skills share the melee roll.
  - How: detour of the melee attack (7 arguments); for the call it raises
    `Hero+0x160` by the bonus and puts it back unless `RecalcStats` rebuilt
    it meanwhile; party heroes only, no nesting. The mod checks the hit
    computation sits at attack+0x134, so a changed formula turns it off.
  - [x] Proton, 2026-11-16, game 1.0.5, rats in Fangroot Forest: a STR 13
    Warrior with a sword got +26, a DEX 5 Voidcaller with a dagger +10, a
    STR 6 Wizard's staff swing +12; the Ranger's sling shot didn't go
    through the hook; the Warrior's next attack started from the game's 0
    again.
  - [ ] Windows.
- [ ] `melee-accuracy` formulas (a player's request, 2026-11-17): setting
      `formula` picks the base chance that replaces the game's 100;
      the mod adds `base - 100` to `Hero+0x160` (now possibly negative).
  - `stat_bonus` (default, the 1.0.5 rule): `100 + max(0, WSTAT x
    per_point)`. `ranged_mirror` (the player's): `(STR + DEX + 5) / 2 x 9
    + RS x righteous_strike` (`x 10` until game 1.0.6 made ranged DEX x 9;
    changed 2026-11-19 at the user's request) (default 10, flat at every rank, the player's
    suggestion for Paladins). `custom`: `custom_formula`, a string parsed
    by `src/formula.rs` (+ - * / x, parentheses, min, max; STR DEX END WIS
    WSTAT RS; f64, rounded at the end). A parse error turns the effect off
    with a toast and a log line; a non-finite result leaves that attack to
    the game.
  - Effective END/WIS are `Hero+0x114`/`+0x118` (`RecalcStats` copies them
    beside STR `+0x10c` and DEX `+0x110`).
  - RS = `Hero+0x20a != 0 && a6 == 0`: the melee attack's own condition for
    Righteous Strike's damage (a6 = the weapon skill, null on a plain
    attack); the hero attack routine 0x1401313a0 clears the flag after the
    attack. The cast sets it for skill effect 0x18 when the class is 1 or 3
    (Paladin, or Archer?); unchecked which Archer skill, if any, uses it.
  - [ ] Proton: `ranged_mirror` on forest rats (a hero with STR + DEX < 18
    gets a negative change), a broken custom formula shows the toast. No
    Paladin in the test party, so RS is untested in game.
  - [ ] Windows.
- [x] `learned-recipes` (a player's request, 2026-11-16; built 2026-11-17):
      learned recipes get a check mark on their icon and a tooltip line.
  - Learned = the recipe id is in `Party+0xb30` (count) / `+0xb38`
    (`String` array), saved as `unlocked_recipes`; only a successful
    mortar craft (0x1402228f0 → unlock 0x140189490) and the debug command
    `unlock_all_recipes` add to it. Recipe items' `recipe` (ItemParams
    +0x2f0, so `Item+0x410`) match the 25 `alchemy_recipes` ids 1:1.
  - Tooltip: detour of the item tooltip function 0x140248ed0 (wraps the
    builder 0x140249150; the builder lists ingredients only when its 9th
    argument is 0, the shop's item panel passes 1). After it, the mod
    appends `Text_Get` of `uk_qol_recipe_learned` / `_not_learned` with
    `String_Concat` / `String_Assign` / `String_Free`. The keys come from
    the mod's patches on `text/text_common_<lang>.txt` (7 languages);
    a missing key falls back to built-in English.
  - Icon: detour of the item grid's slot drawing 0x140199f80 (inventory
    panel 0x1401fc2c0 and both shop lists in 0x14019c920 go through it).
    After it, for each slot (grid+0x38, 0x30 bytes: rect +0x10, Item*
    +0x20) holding a learned recipe, `DrawSprite` 0x1400730f0 queues
    `hud_icon_quest_completed` (found by name in the sprite table
    0x140379440, count 0x1404426c4) on the HUD batch `**(ui+0x630)` at
    the icon square's top-right corner (`min(w, h)`: shop rows are wide).
    Rows scrolled out of a list are clipped like the icons.
  - Runtime: `savehook::party()` exposes the resolved `g_party`.
  - [x] Proton, 2026-11-17, game 1.0.5: a test save with Healing learned
    and Minor Mana not; inventory marks and both tooltip lines; Barlo's
    shop (opened through a test-only encounter patch, with two extra
    recipes) marks the merchant's Healing row and the party's list; the
    shop panel shows the line; nothing drawn for rows scrolled out.
    English only; other languages and chest panels not checked.
  - [ ] Windows.
  - [ ] The English fallback (mod turned on mid-session, keys missing) is
    untested: in every test the keys were there.
  - [ ] While an item is moving (bought, sold, moved: the list at
    0x1404412c8 / the grid's `bVar10` case) the game skips its icon; the
    mark doesn't check that, so a learned recipe's check may show on an
    empty slot for a moment. Cosmetic; not seen in tests.
  - 2026-11-17, player report "one Heiltrank recipe checked, the other
    not": not a bug. Five potions have two recipe items with the same name
    and their own recipe ids (`item_recipe_healing` → `item_heal_potion`,
    flask + 2 Amberrose + Crimsonspire; `item_recipe_healing_dvojka` →
    `item_heal_potion_dvojka`, flask + 2 Minor Healing; likewise Mana,
    Endurance, Minor Mana, Cure Poison). Each is learned on its own. README
    says so. Possible follow-up (not decided): show the ingredients in the
    shop's item panel too, where the two look identical.
- [x] `shop-sort` (a player's request, 2026-11-18: "swords together, armor
      types together"): the merchant's list in shops stays sorted. User's
      choices: its own mod (not an inventory-sort setting), always on (no
      key), category → item type → weapon kind → buy price **low to high**
      → name, the party's side of the shop untouched.
  - Shop panel global 0x1404415c8 (byte +0 open). Open 0x14019c4f0 copies
    the stock list (+0x490, count +0x488, 0x20 each) into a temporary
    merchant `Inventory` at +0x160 (Inventory::Add 0x140198b90) shown by the
    grid at +0x188 (grid+0 = Inventory*, +0x100 changed flag, +0x10c
    selected slot = panel+0x294); close 0x14019c6f0 frees it. The party's
    grid is at +0x40 (on `g_partyInventory`).
  - Every grid's layout update 0x140199920 → layout 0x140199ad0 / 0x140199da0
    sorts the inventory's entries in place (0x140285440, comparator
    0x140199040: category 0x1400ed4f0(item_type), then entry seq) and lays
    out one slot per entry.
  - Buy 0x1401a0fd0 takes the selected slot's Item*, removes it from the
    temporary list by pointer and changes the stock by def name
    (0x14019f2e0); sell 0x1401a0b10 adds to the temporary list
    (0x140198d80), updates the grid, then finds the sold item's slot by
    pointer. So the temporary list's order is display only.
  - The mod detours 0x140199920; for the merchant grid with the changed
    flag set (never set by the mod) it renumbers the entries' seqs to its
    order (unique 0..n-1, next_seq raised to n) before the original runs.
    Sorting key: buy price `Item+0x42c`; weapon kind `Item+0x120` read only
    for item_type 1.
  - [x] Proton, 2026-11-18, game 1.0.5: Barlo's shop (shop test recipe)
    with 16 extra weapons and armor pieces added in mixed order. Sorted on
    first open (before any trade): swords, dagger, axes 30 → 60, maces
    24 → 60, crossbow; shields 35 → 80, body armor 30 → 55, helmet, gloves,
    boots 6 → 20 → 50; potions cheapest first, equal prices by name. Bought
    Chain Armor from the middle of its group: exactly that item, 55 coins.
    Sold a Gravecleft Mace: it showed between Flail 24 and Maul 60 and the
    list scrolled to it; buying it back took 35 coins and that item. No
    errors in the log. Close and reopen not tried (the test encounter
    opens the shop once; the stock counts are the game's own code, by
    name).
  - [ ] Windows.
- [ ] `party-numbers` (a player's request, 2026-11-20: "display character
      hit points, mana & stamina on screen"): numbers on the portrait bars.
      User's choice: its own mod, separate from enemy health (a pattern miss
      in one mustn't take the other down; enemy health is a balance choice).
      Released together with `enemy-health` and `hidden-stats`.
  - Bars (1.0.5 0x14021e6b0, same code in 1.0.6 with the hero array 0x20
    later): at `face_rect_N` + `face_hp_mana_offset` (GuiLayout+0xe0), the
    second 6 lower; sprite `hud_hero_hpbar` 60x5, pivot 0. HP `+0xfc`/`+0xec`;
    by class `+0x94`: Paladin Zeal pips `+0x108`/`+0xf8`, classes 0/2/3
    stamina `+0x100`/`+0xf0`, 4/5/6 mana `+0x104`/`+0xf4`; max 0 = no fill.
    The portrait position is `face_rect_N` except while a portrait is
    dragged.
  - No patterns of its own (core `g_party`, `g_gui_layout`). New runtime
    hook: `Mod::hud_display_only`, drawn also in interaction mode 1
    (target pick); clickable widgets stay plain-HUD only.
  - [x] Proton: numbers line up with the bars; Zeal 0/5 -> 1/5 after a
    hit, mana 44 -> 37 after Void Mend, HP 32 -> 36/37 healed; shown during
    the Void Mend target pick and on the character sheet.
  - Proton, 2026-11-20, game 1.0.6, 3440x1440, Abandoned Caves autosave
    (Paladin, Voidcaller, two Rangers) and a Fallen Swordsman fight; the
    user also checked everything in game and found it fine.
    Not tried: 720p / 1080p (the transform is unit-tested), a dead hero.
  - [ ] Windows.
- [ ] `enemy-health` (a player's request, 2026-11-20: "an enemy hit point
      display ... where their current health is"): numbers on the game's own
      enemy health bars. Off by default (balance choice). Released together
      with `party-numbers` and `hidden-stats`.
  - The game already draws enemy bars: enemy-bar HUD 0x1401ffc60 (1.0.6
    0x1401ffd50), called each frame by the dungeon HUD; up to two faced
    enemies (0x1401feaf0); skipped if Monster+0x128 (`invisible_hp_bar`);
    size by +0x1f1 (`hpbar_size` 1..3); fill +0xd0 / +0x120; name centred in
    the bar; y = dung_view.y + 12, x projected. Then the status row
    0x1401ff4a0 (1.0.6 0x1401ff590; one caller) gets (monster, {x, y+h, w, h}).
  - Monster: +0xd0 HP (TakeDamage 0x1401e4cf0 subtracts it), +0x120 max,
    +0x108 def `health` (copied to +0x10c/+0x120/+0xd0 by Init 0x1401de310).
  - Detours both; the status row records (hp, max, bar rect) while inside
    the bar function, which publishes the frame's list; hud() paints lists
    under 250 ms old. Display only (`hud_display_only`).
  - Not covered: the `enemy_dragon_territory` bar (third path, no status row).
  - [x] Proton: "79/79" above the Fallen Swordsman's bar, 69/79 after a
    10-damage hit, shown during a target pick.
  - Proton, 2026-11-20, game 1.0.6, 3440x1440, Abandoned Caves autosave
    (Paladin, Voidcaller, two Rangers) and a Fallen Swordsman fight; the
    user also checked everything in game and found it fine.
    Not tried: other bar sizes, two enemies side by side, `on_bar`.
  - [ ] Windows.
- [ ] `hidden-stats` (2026-11-20, asked with party-numbers and
      enemy-health): the hidden hero stats on the stats page. Own RE only
      (not taken from the Nexus mod of the same idea). On by default,
      setting `show_zero`.
  - Stats page (1.0.5 / 1.0.6): row rebuild 0x140236f20 / 0x140237010
    (page, hero), called every frame by the page draw 0x14023b290 /
    0x14023b380. Row vector at page+0x28 {count, cap, data}, rows 0x140:
    +0 kind (0 stat, 1 header, 2 effect), +8 key String, +0x20 value String;
    label = text(key) with %1 = value; description = text(key + "_desc") if
    it exists (Text_Exists 0x1400ecac0). Push 0x14027a860 / 0x14027af30
    moves the strings in.
  - Hero stat block: RecalcStats 0x14017a8b0 sums item/effect blocks
    (0x1400cfe80) into Hero+0x10c..; names from the parser 0x1400c2880;
    units from the item tooltip (0x140169cb0: 0 flat, 1 percent, 2 plain).
    Melee hit 100 + 0x160; ranged 0x164; armor vs melee 0x134+0x140
    (+0x13c animals), vs ranged 0x134+0x144 (1.0.5 0x14012e120, 1.0.6 0x14012e170); evasion cap
    clamp(60 + 0x130); ranged evasion 0x12c; heals 0x1e0 / 0x1e8 / 0x1e4;
    immunities bytes 0x200..0x206.
  - Detours the rebuild; appends rows with the game's push, then moves
    them bytewise to the end of Hero / Offense / Defense. Values made with
    String_Concat so the game frees them. melee-accuracy on (read from the
    config every 2 s): the melee row's description says so.
  - [x] Proton: gray rows at the end of Offense and Defense, Void
    Resistance in its color, descriptions in the side panel (Evasion Cap),
    mouse-wheel scrolling; Ranged Hit Chance 27% for DEX 3 (x 9, 1.0.6).
  - Proton, 2026-11-20, game 1.0.6, 3440x1440, Abandoned Caves autosave
    (Paladin, Voidcaller, two Rangers) and a Fallen Swordsman fight; the
    user also checked everything in game and found it fine.
    Not tried: pad scrolling, `show_zero`, the mod turned on mid-game.
  - [ ] Windows.
  - 2026-11-21 (1.0.17, a player's report: "-x mana cost for spells" was
    missing): row Spell Mana Cost in Offense, Voidcaller/Wizard only. It's
    not in the stat block: PaySkillCost (1.0.6 0x140184940), classes 4/5,
    pays cost + 1 with effect 0xa0 (Arcane Appetite) minus the bonuses of
    item feature 9 (`cloak_of_the_spells`: Archmage Hat 2, Cloak of the
    Archmage 1, Archmage's Seal 1, Staff of the Archmage 1) over the 11
    equipped items that pass CanUseItem (0x14017ba30, pure: race 3 no
    armor, class, STR/DEX/WIS/END), counted only when feature 9 is the
    item's first, a two-handed item not again in slot 5; at least 1. The
    mod mirrors that in Rust (no new pattern).
    - [ ] In game: Archmage items on a Wizard or Voidcaller.
    The Blood Mage's equivalent (`red_mask`, feature 0x13, HP cost) isn't
    shown yet.
- [ ] `item-cooldowns` (2026-11-21, a player's request: see a belt's
      ability cooldown without equipping it): a line at the bottom of an
      item's tooltip with the turns of cooldown left, or Ready. On by
      default, setting `show_ready`.
  - Game 1.0.6: an item's ability state is at Item+0xd8 (+0 SkillDef*,
    +0xc level, +0x10 = Item+0xe8 turns left); has-ability flag
    Item+0x598. Casting (0x140183ad0) sets turns left to SkillDef+0x4c;
    the party tick 0x140187da0 counts every inventory item down and the
    per-hero tick 0x140180370 every equipped one; equip/unequip don't touch
    it. The game's skill tooltip (0x140243780) shows only SkillDef+0x4c.
  - Detours the item tooltip builder 0x1402495f0 (9 arguments), since
    learned-recipes owns the outer item tooltip 0x140249370 and a function
    takes one detour. A real item is recognised by params == skill + 0x48
    (the other caller, 0x14024daa0, builds from a def on its stack). Ready
    only for party inventory and hero equipment items.
  - [x] Proton, 2026-11-21, game 1.0.6, 3440x1440, edited quicksave
    (Abandoned Caves): Belt of Second Wind "Cooldown left 37", 36 after one
    waited turn; Ring of Lightning Bolt "Ready" in green; a plain sword has
    no line. Not tried: an equipped item, slot II, other languages, shops.
  - [ ] Windows.

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
    Exercised against the 1.0.0 release (§8 test matrix).

## 8. Release

- [x] Tagged release via CI: Windows, Linux, macOS manager builds +
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
  - 2026-11-14: release `v1.0.0` published (run 43656770879): both manager
    tarballs, the Windows manager, the runtime and patterns zips,
    `SHA256SUMS`.
  - Found in the 1.0.0 downloads: the Linux and macOS managers come out of
    their tarballs without the execute bit (`actions/upload-artifact` drops
    file modes). The workflow now runs `chmod 755` before `tar` (1.0.1).
    Self-update was never affected (`unpack` sets the mode).
  - 2026-11-14: version 1.0.1 (workspace and every `mod.toml`), tag
    `v1.0.1`: weapon-swap's icon and no notice gate, the hook write lock,
    the update fixes above, executable manager downloads.
  - 2026-11-15: version 1.0.2, tag `v1.0.2`: the new `item-highlight` mod,
    the `color` setting type and `bridge::request_setting`.
  - 2026-11-15: version 1.0.3, tag `v1.0.3`: extra-skill-slots' HUD
    buttons turn red while their skill is on cooldown (a player's request).
  - 2026-11-15: version 1.0.4, tag `v1.0.4`: weapon-swap takes quest
    weapons (a player couldn't put J'Axe in the slot).
  - 2026-11-16: version 1.0.5, tag `v1.0.5`: the new `melee-accuracy`
    mod (off by default).
  - 2026-11-17: version 1.0.6, tag `v1.0.6`: `melee-accuracy` formula
    choices (`stat_bonus`, `ranged_mirror`, `custom`), a player's request.
  - 2026-11-17: version 1.0.7, tag `v1.0.7`: the new `learned-recipes`
    mod (a player's request).
  - 2026-11-17: version 1.0.8, tag `v1.0.8`: weapon-swap's slot II shows
    the game's own item tooltip (a player's request); README explains the
    same-named recipe pairs (a player's learned-recipes report).
  - 2026-11-17: version 1.0.9, tag `v1.0.9`: weapon-swap's `messages`
    setting (`all` / `problems` / `none`) for its popups; a player's
    request (the swap report covered the character sheet).
  - 2026-11-17: version 1.0.10, tag `v1.0.10`: extra-skill-slots' buttons
    show the game's own skill tooltip (a player's request).
  - 2026-11-18: version 1.0.11, tag `v1.0.11`: the new `shop-sort` mod
    (a player's request).
  - 2026-11-19: version 1.0.12, tag `v1.0.12`: patterns for game 1.0.6
    (weapon-swap and bloodmage-cost were off on it); `ranged_mirror` follows
    the game's new ranged rule (DEX x 9).
  - 2026-11-20: version 1.0.13, tag `v1.0.13`: three new mods from player
    requests, `party-numbers`, `enemy-health` (off by default) and
    `hidden-stats`; runtime `Mod::hud_display_only`.
  - 2026-11-20: version 1.0.14, tag `v1.0.14`: running next to the
    Underkeep Mod Framework (its `version.dll`, analysed from its files, not
    run): the manager adds `winmm=n,b` to an existing `WINEDLLOVERRIDES`
    instead of refusing, `uk-qol report` lists other loaders' DLLs and
    folders, README "With other mod loaders"; `tools/pattern-diff.py` for
    game updates; CONTRIBUTING: data patches before code hooks.
  - 2026-11-21: version 1.0.15, tag `v1.0.15`: weapon-swap fixes from a
    player's report: a weapon in slot II counts its cooldown down (per turn
    and per rest step, like the inventory; detours `party_round_tick` and
    `hero_rest_step`), and slot II follows its hero when heroes switch
    places. Tested in game 1.0.6 with Proton.
  - 2026-11-21: version 1.0.16, tag `v1.0.16`: the new `item-cooldowns`
    mod (a player's request).
  - 2026-11-21: version 1.0.17, tag `v1.0.17`: hidden-stats shows the
    spell mana cost change (a player's report). Unit-tested only.
- [x] Test matrix: Windows, Linux Proton, Steam Deck; fresh install,
      update, uninstall, save loads without mods.
  - Linux Proton, 2026-11-14, with the released 1.0.0 Linux manager (CLI):
    `detect`/`status` found the Flatpak install; a fresh `install` (no
    loader in the folder) downloaded the runtime zip, checked it against
    `SHA256SUMS` and wrote the release's `winmm.dll` byte for byte; the game
    loaded it (`uk-qol 1.0.0`, every pattern found). `update` with
    `UK_QOL_FAKE_VERSION=0.9.0`: refused while the game ran, then replaced
    the manager (executable, same hash) and wrote pattern files. `uninstall`
    removed everything it had added (`uk_qol/` gone). Without the loader,
    save 001 (made with mods) loaded and played; the load list showed only
    the game's saves.
  - Found and fixed (main): the update wrote every mod's pattern file
    next to a loader of the same release. A pattern file replaces a mod's
    built-in patterns whole, so after a later loader update these would
    shadow its newer patterns. Pattern files now go only to a loader of
    unknown version (`--dll` installs); every release's loader carries its
    patterns, and `install()` drops pattern files left by an earlier update
    (unless edited by hand). Also: `install` no longer asks to paste the
    launch option when it's set and the only other Steam account never ran
    the game.
  - Windows: the user's tests (§1). Steam Deck: not tested; the user has
    no Deck (decided 2026-11-14: left to user reports).
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
- [x] macOS: no native runtime (decided 2026-11-14). The game runs in a
      CrossOver/Whisky/Wine bottle, which loads the Windows `winmm.dll`; ship
      that plus the existing arm64 manager. Document the bottle's
      `winmm=n,b` override in README and try it once in a bottle.
  - 2026-11-14: README says how to set it (winecfg, *Libraries*, `winmm`,
    *Native then Builtin*). Tried in a fresh Wine 10 prefix (no Mac here)
    with a MinGW exe importing winmm next to the proxy: without the override
    Wine's builtin winmm is used and nothing loads; with the registry
    override (`HKCU\Software\Wine\DllOverrides`, `winmm` = `native,builtin`,
    what a bottle's winecfg writes) the proxy loads and forwards. A real
    CrossOver/Whisky bottle and the arm64 manager aren't tested: the user
    has no Mac (decided 2026-11-14: left to user reports).
