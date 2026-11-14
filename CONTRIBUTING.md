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

What may go in: byte patterns (short instruction sequences with wildcards, used
to *find* code), offsets relative to a match, field and record names a mod
needs, and original code and documentation. Tests that need game data read it
from the player's own install (`UNDERKEEP_DIR`) and skip when it's not there;
build test inputs by hand otherwise (see `crates/dft` for an example).

`.gitignore` blocks common game file types, but it's only a safety net: check
`git status` before you commit.

## Adding a mod

Built-in mods live in `mods/<name>/` and are compiled into the runtime.

1. `mod.toml`: name, version, description, supported game builds, conflicts,
   and the settings with their defaults (the overlay draws a form from them).
2. `patterns.toml`: every function or address the mod needs, as a named byte
   pattern, with a comment saying what it is and how it was found.
3. `patches/` (optional): declarative data patches for the game's definition
   files, applied in memory.
4. The code: a type implementing the runtime's `Mod` trait (`on_load`,
   `on_unload`, `on_frame`, `on_input`, `on_save`, `on_load_save`).

A mod must:

- never write the game's save files or files in the game folder other than
  `uk_qol/`. State it has to keep goes in the `<save>.qol.toml` sidecar;
- leave the game playable when it's turned off or when its patterns aren't
  found;
- undo all its hooks and patches in `on_unload`.

## Updating patterns after a game patch

1. Run `uk-qol status` against the new build; it lists every pattern that no
   longer resolves.
2. Find the moved code in the new exe (start from the old match), write a new
   pattern and check that it matches **exactly once**.
3. Patterns are per build: add the new one next to the old one instead of
   replacing it, so players still on the old build keep working.
4. In the pull request, give the build (exe SHA-256 from `uk-qol report`) you
   tested on.

## Pull requests

- Keep each PR to one change; run fmt, clippy and the tests first.
- CI doesn't run on pull requests automatically. A maintainer starts a build
  from the Actions tab when one is needed.
- By contributing you agree your work is released under the [MIT license](LICENSE).
