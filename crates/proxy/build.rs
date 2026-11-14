//! Generates the export stubs for winmm.dll from `winmm.exports`.
//!
//! Each export becomes a tiny stub that loads its index into `eax` and jumps
//! to a shared routine, which resolves the system winmm.dll on first use and
//! jumps to the real function. The export itself is declared with a `.drectve`
//! `-export:` directive so the linker puts it in the DLL's export table.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=winmm.exports");
    let list = fs::read_to_string("winmm.exports").expect("read winmm.exports");
    let names: Vec<&str> = list
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    let mut asm = String::new();
    let mut rs = String::new();
    writeln!(rs, "pub const COUNT: usize = {};", names.len()).unwrap();
    writeln!(rs, "pub static NAMES: [&[u8]; COUNT] = [").unwrap();
    for (i, name) in names.iter().enumerate() {
        assert!(
            name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "bad export name {name:?}"
        );
        writeln!(rs, "    b\"{name}\\0\",").unwrap();
        // `{{`/`}}` are escapes: this text is a `global_asm!` template.
        write!(
            asm,
            ".section .drectve,\"yn\"\n\
             .ascii \" -export:{name}\"\n\
             .text\n\
             .p2align 4\n\
             .globl {name}\n\
             {name}:\n\
             \x20   mov eax, {i}\n\
             \x20   jmp {{common}}\n"
        )
        .unwrap();
    }
    writeln!(rs, "];").unwrap();

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out.join("stubs.s"), asm).unwrap();
    fs::write(out.join("names.rs"), rs).unwrap();
}
