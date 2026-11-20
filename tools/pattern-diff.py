#!/usr/bin/env python3
"""Check every mod's patterns against two builds of underkeep.exe.

After a game update, run it with the previous exe and the new one:

    python3 tools/pattern-diff.py OLD/underkeep.exe NEW/underkeep.exe
    python3 tools/pattern-diff.py OLD NEW --disasm      # also diff each target
    python3 tools/pattern-diff.py OLD NEW --disasm -v   # and print the diffs

For each pattern it prints where it resolves on each build, or MISS<n> (n
matches instead of exactly one). With --disasm, every function a pattern
resolves to on both builds is disassembled on both, addresses and RIP
offsets are normalized, and the number of changed instructions is printed:
0 means the code moved but didn't change, so a new pattern is enough;
anything else needs a look before the mod is trusted on the new build.
Data patterns (`rip`) aren't disassembled.

Patterns default to every `mods/*/patterns.toml` and
`crates/runtime/patterns/core.toml` in this repo; pass files to check only
those. Resolution follows the runtime (CONTRIBUTING.md, "patterns.toml"):
exactly one match, then `offset`, then `call` or `rip`; candidates tagged
with the build's SHA-256 first, otherwise all resolving candidates must
agree.

Needs Python 3.11+ (tomllib) and, for --disasm, a GNU objdump that reads
PE files: x86_64-w64-mingw32-objdump (mingw binutils) or a multiarch
objdump; set OBJDUMP to choose. Reads the exe files only; writes nothing.
"""

import argparse
import difflib
import hashlib
import os
import re
import shutil
import struct
import subprocess
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
IMAGE_SCN_MEM_EXECUTE = 0x20000000


class PE:
    def __init__(self, path):
        self.path = path
        d = self.data = Path(path).read_bytes()
        self.sha256 = hashlib.sha256(d).hexdigest()
        pe = struct.unpack_from("<I", d, 0x3C)[0]
        nsec = struct.unpack_from("<H", d, pe + 6)[0]
        opt_size = struct.unpack_from("<H", d, pe + 20)[0]
        self.image_base = struct.unpack_from("<Q", d, pe + 24 + 24)[0]
        o = pe + 24 + opt_size
        self.sections = []
        for _ in range(nsec):
            name = d[o : o + 8].rstrip(b"\0").decode(errors="replace")
            va = struct.unpack_from("<I", d, o + 12)[0]
            raw_size, raw_ptr = struct.unpack_from("<II", d, o + 16)
            flags = struct.unpack_from("<I", d, o + 36)[0]
            self.sections.append((name, va, raw_size, raw_ptr, flags))
            o += 40

    def rva(self, off):
        for _, va, size, ptr, _ in self.sections:
            if ptr <= off < ptr + size:
                return off - ptr + va
        return None

    def find(self, pattern, section=None):
        rx = re.compile(
            b"".join(
                b"." if t == "??" else re.escape(bytes([int(t, 16)]))
                for t in pattern.split()
            ),
            re.S,
        )
        hits = []
        for name, _, size, ptr, flags in self.sections:
            if section is not None and name != section:
                continue
            if section is None and not flags & IMAGE_SCN_MEM_EXECUTE:
                continue
            # finditer doesn't report overlapping matches; step one byte at
            # a time so a pattern that overlaps itself still counts as two.
            pos, end = ptr, ptr + size
            while (m := rx.search(self.data, pos, end)) is not None:
                hits.append(m.start())
                pos = m.start() + 1
        return hits

    def resolve_one(self, e):
        """(rva, None) or (None, number of matches)."""
        hits = self.find(e["pattern"], e.get("section"))
        if len(hits) != 1:
            return None, len(hits)
        off = hits[0] + e.get("offset", 0)
        r = self.rva(off)
        if r is None:
            return None, 0
        if e.get("call"):
            if self.data[off] not in (0xE8, 0xE9):
                return None, 0
            r = r + 5 + struct.unpack_from("<i", self.data, off + 1)[0]
        elif "rip" in e:
            rip = e["rip"]
            r = r + rip["len"] + struct.unpack_from("<i", self.data, off + rip["disp"])[0]
        return r, None

    def resolve(self, e):
        if "candidates" not in e:
            return self.resolve_one(e)
        cands = e["candidates"]
        tagged = [c for c in cands if self.sha256 in c.get("builds", [])]
        for c in tagged:
            r, _ = self.resolve_one(c)
            if r is not None:
                return r, None
        found = {r for r, _ in map(self.resolve_one, cands) if r is not None}
        if len(found) == 1:
            return found.pop(), None
        return None, len(found)


def objdump():
    exe = os.environ.get("OBJDUMP")
    if exe:
        return exe
    for name in ("x86_64-w64-mingw32-objdump", "objdump"):
        if shutil.which(name):
            return name
    sys.exit("--disasm needs objdump (install mingw-w64 binutils or set OBJDUMP)")


def disasm(tool, pe, rva, length=0x700):
    """Normalized instructions of the function at `rva`, up to its end."""
    start = pe.image_base + rva
    out = subprocess.run(
        [
            tool, "-d", "-M", "intel", "--no-show-raw-insn",
            f"--start-address={start:#x}", f"--stop-address={start + length:#x}",
            pe.path,
        ],
        capture_output=True, text=True, check=True,
    ).stdout
    ins = []
    for line in out.splitlines():
        m = re.match(r"\s+[0-9a-f]+:\s+(.*)", line)
        if not m:
            continue
        t = re.sub(r"#.*", "", m.group(1))
        t = re.sub(r"0x14[0-9a-f]{7}", "ADDR", t)
        t = re.sub(r"rip\+0x[0-9a-f]+", "rip+X", t)
        t = re.sub(r"<[^>]*>", "", t).strip()
        ins.append(t)
        # End of the function: padding after a ret or tail jump.
        if t.startswith("int3") and len(ins) > 3 and ins[-2].startswith(("ret", "jmp", "int3")):
            break
    return ins


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("old", help="underkeep.exe of the build the patterns were made for")
    ap.add_argument("new", help="underkeep.exe of the new build")
    ap.add_argument("patterns", nargs="*", help="patterns.toml files (default: all in the repo)")
    ap.add_argument("--disasm", action="store_true", help="diff each resolved function")
    ap.add_argument("-v", "--verbose", action="store_true", help="print the diffs (with --disasm)")
    a = ap.parse_args()

    old, new = PE(a.old), PE(a.new)
    print(f"old {old.sha256}\nnew {new.sha256}\n")
    files = [Path(p) for p in a.patterns] or sorted(REPO.glob("mods/*/patterns.toml")) + [
        REPO / "crates/runtime/patterns/core.toml"
    ]
    tool = objdump() if a.disasm else None

    def show(pe, r, n):
        return f"{pe.image_base + r:#x}" if r is not None else f"MISS{n}"

    misses = 0
    for f in files:
        owner = f.parent.name if f.name == "patterns.toml" else f.stem
        for key, e in tomllib.loads(f.read_text()).items():
            ro, no = old.resolve(e)
            rn, nn = new.resolve(e)
            flags = " ".join(
                x for x in ("optional" if e.get("optional") else "", "data" if "rip" in e else "") if x
            )
            line = f"{owner:18} {key:30} old={show(old, ro, no):14} new={show(new, rn, nn):14} {flags}"
            if rn is None and not e.get("optional"):
                misses += 1
            if tool and ro is not None and rn is not None and "rip" not in e:
                x, y = disasm(tool, old, ro), disasm(tool, new, rn)
                diff = [
                    l for l in difflib.unified_diff(x, y, lineterm="", n=0)
                    if not l.startswith(("---", "+++", "@@"))
                ]
                line += f" changed={len(diff)}"
                print(line)
                if diff and a.verbose:
                    print("\n".join("    " + l for l in diff[:80]))
            else:
                print(line)
    print(f"\n{misses} required pattern(s) miss on the new build")
    return 1 if misses else 0


if __name__ == "__main__":
    sys.exit(main())
