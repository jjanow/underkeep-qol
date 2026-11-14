//! Live hooks on functions compiled into the test (Windows, or Wine as the
//! cargo runner). The targets are naked functions so their prologues are
//! exactly the instructions under test, whatever the optimizer does.

use std::arch::naked_asm;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

type Fn1 = extern "C" fn(u64) -> u64;

static DATA: u64 = 0x1234;

/// `mov rax,[rip+DATA]; add rax,rcx; ret`: RIP-relative first instruction.
#[unsafe(naked)]
extern "C" fn rip_fn(_x: u64) -> u64 {
    naked_asm!(
        "mov rax, qword ptr [rip + {d}]",
        "add rax, rcx",
        "ret",
        d = sym DATA,
    )
}

/// `test rcx,rcx; jz short; lea rax,[rcx+1]; ret; mov eax,42; ret`.
#[unsafe(naked)]
extern "C" fn jcc_fn(_x: u64) -> u64 {
    naked_asm!(
        "test rcx, rcx",
        "jz 2f",
        "lea rax, [rcx + 1]",
        "ret",
        "2:",
        "mov eax, 42",
        "ret",
    )
}

extern "C" fn triple(x: u64) -> u64 {
    x * 3
}

/// `sub rsp,40; call triple; add rax,7; add rsp,40; ret`: a call in the
/// copied range, returning into the trampoline.
#[unsafe(naked)]
extern "C" fn call_fn(_x: u64) -> u64 {
    naked_asm!(
        "sub rsp, 40",
        "call {t}",
        "add rax, 7",
        "add rsp, 40",
        "ret",
        t = sym triple,
    )
}

/// 15 bytes of plain instructions before `ret`: room for the 14-byte jump.
#[unsafe(naked)]
extern "C" fn long_fn(_x: u64) -> u64 {
    naked_asm!(
        "mov rax, rcx",
        "add rax, 0x11111",
        "add rax, 0x22222",
        "ret",
    )
}

/// `mov eax,5; ret`, for byte patches of the immediate.
#[unsafe(naked)]
extern "C" fn five() -> u32 {
    naked_asm!("mov eax, 5", "ret")
}

/// Calls through a pointer the optimizer can't see through.
fn call(f: usize, x: u64) -> u64 {
    let f: Fn1 = unsafe { std::mem::transmute(std::hint::black_box(f)) };
    f(x)
}

fn addr(f: Fn1) -> usize {
    f as usize
}

/// Detours a function and checks hook, trampoline, disable, re-enable and
/// drop. `detour` adds 1000 to what the original returns.
fn exercise(target: usize, orig: &'static AtomicUsize, detour: Fn1, far: bool, x: u64, want: u64) {
    let before = sys::read(target, 16).unwrap();
    assert_eq!(call(target, x), want);
    let mut set = HookSet::new("test");
    let tramp = unsafe { set.detour_with(target, addr(detour), far) }.unwrap();
    orig.store(tramp, Ordering::SeqCst);
    // The trampoline works before enable, and nothing is written yet.
    assert_eq!(call(tramp, x), want);
    assert_eq!(sys::read(target, 16).unwrap(), before);

    set.enable().unwrap();
    assert!(set.is_enabled());
    let first = sys::read(target, 1).unwrap()[0];
    assert_eq!(first, if far { 0xFF } else { 0xE9 });
    assert_eq!(call(target, x), want + 1000);
    assert_eq!(call(tramp, x), want);

    set.disable().unwrap();
    assert_eq!(sys::read(target, 16).unwrap(), before);
    assert_eq!(call(target, x), want);

    set.enable().unwrap();
    assert_eq!(call(target, x), want + 1000);
    drop(set);
    assert_eq!(sys::read(target, 16).unwrap(), before);
    assert_eq!(call(target, x), want);
}

macro_rules! detour_fn {
    ($name:ident, $orig:ident) => {
        static $orig: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn $name(x: u64) -> u64 {
            call($orig.load(Ordering::SeqCst), x) + 1000
        }
    };
}

detour_fn!(rip_detour, RIP_ORIG);
detour_fn!(jcc_detour, JCC_ORIG);
detour_fn!(call_detour, CALL_ORIG);
detour_fn!(long_detour, LONG_ORIG);

#[test]
fn detour_rip_relative_prologue() {
    exercise(addr(rip_fn), &RIP_ORIG, rip_detour, false, 1, 0x1235);
}

#[test]
fn detour_short_jcc_prologue() {
    exercise(addr(jcc_fn), &JCC_ORIG, jcc_detour, false, 0, 42);
    exercise(addr(jcc_fn), &JCC_ORIG, jcc_detour, false, 9, 10);
}

#[test]
fn detour_call_in_prologue() {
    exercise(addr(call_fn), &CALL_ORIG, call_detour, false, 5, 22);
}

#[test]
fn detour_with_fourteen_byte_jump() {
    exercise(
        addr(long_fn),
        &LONG_ORIG,
        long_detour,
        true,
        1,
        1 + 0x11111 + 0x22222,
    );
}

#[test]
fn patch_and_busy() {
    let f = five as extern "C" fn() -> u32 as usize;
    let five = |f: usize| -> u32 {
        let f: extern "C" fn() -> u32 = unsafe { std::mem::transmute(std::hint::black_box(f)) };
        f()
    };
    let mut a = HookSet::new("a");
    unsafe { a.patch(f + 1, &[9]) }.unwrap();
    assert_eq!(five(f), 5);
    a.enable().unwrap();
    assert_eq!(five(f), 9);

    // Another set can't touch the same bytes.
    let mut b = HookSet::new("b");
    assert!(matches!(
        unsafe { b.patch(f + 1, &[7, 0]) },
        Err(HookError::Busy(_))
    ));
    assert!(matches!(unsafe { b.detour(f, f) }, Err(HookError::Busy(_))));

    a.disable().unwrap();
    assert_eq!(five(f), 5);
    // Registering while enabled writes at once.
    a.enable().unwrap();
    unsafe { a.patch(f + 2, &[1]) }.unwrap();
    assert_eq!(five(f), 9 + 256);
    drop(a);
    assert_eq!(five(f), 5);
    // Claims are released with the set.
    unsafe { b.patch(f + 1, &[7]) }.unwrap();
}

/// `mov rax, rcx; add rax, 3; ret` with room for a 5-byte jump.
#[unsafe(naked)]
extern "C" fn panicky_fn(_x: u64) -> u64 {
    naked_asm!("mov rax, rcx", "add rax, 3", "ret")
}

static PANICKY_ORIG: AtomicUsize = AtomicUsize::new(0);

/// A detour whose mod panics on odd inputs: the guard falls back to the
/// original and the mod stays off.
extern "C" fn panicky_detour(x: u64) -> u64 {
    let orig = || call(PANICKY_ORIG.load(Ordering::SeqCst), x);
    crate::guard::call_or(
        "hook-test-panicky",
        || {
            assert!(x.is_multiple_of(2), "odd input");
            orig() * 10
        },
        orig,
    )
}

#[test]
fn panicking_detour_falls_back_to_original() {
    let target = addr(panicky_fn);
    let mut set = HookSet::new("hook-test-panicky");
    let t = unsafe { set.detour(target, addr(panicky_detour)) }.unwrap();
    PANICKY_ORIG.store(t, Ordering::SeqCst);
    set.enable().unwrap();
    assert_eq!(call(target, 2), 50);
    assert_eq!(call(target, 3), 6);
    assert!(crate::guard::is_failed("hook-test-panicky"));
    assert_eq!(call(target, 2), 5);
}

/// `mov rax, rcx; nop x4; ret`, re-hooked by "other software" while ours is
/// on: disable keeps their jump and bypasses our detour.
#[unsafe(naked)]
extern "C" fn rehooked_fn(_x: u64) -> u64 {
    naked_asm!("mov rax, rcx", "nop", "nop", "nop", "nop", "ret")
}

detour_fn!(rehook_detour, REHOOK_ORIG);
detour_fn!(foreign_detour, FOREIGN_ORIG);

#[test]
fn foreign_hook_on_top_is_left_in_place() {
    let target = addr(rehooked_fn);
    let before = sys::read(target, 16).unwrap();
    let mut ours = HookSet::new("ours");
    REHOOK_ORIG.store(
        unsafe { ours.detour(target, addr(rehook_detour)) }.unwrap(),
        Ordering::SeqCst,
    );
    ours.enable().unwrap();
    assert_eq!(call(target, 1), 1001);

    // Simulate a foreign hooker: relocate our jump into its own trampoline
    // and write its own jump at the target.
    let code = sys::read_code(target, 64).unwrap();
    let slot = sys::alloc_slot(Some(target)).unwrap().unwrap();
    let ft = slot + RELAY_LEN;
    let r = x64::relocate(&code, target as u64, ft as u64, 5).unwrap();
    let mut bytes = relay_bytes(addr(foreign_detour)).to_vec();
    bytes.extend_from_slice(&r.code);
    unsafe { sys::write(slot, &bytes) }.unwrap();
    FOREIGN_ORIG.store(ft, Ordering::SeqCst);
    let fj = x64::jmp_rel32(target as u64, slot as u64).unwrap();
    unsafe { sys::write(target, &fj) }.unwrap();
    assert_eq!(call(target, 1), 2001);

    // Ours goes away, theirs stays.
    ours.disable().unwrap();
    assert_eq!(sys::read(target, 5).unwrap(), fj);
    assert_eq!(call(target, 1), 1001);
    assert!(matches!(ours.enable(), Err(HookError::Busy(_))));
    drop(ours);
    unsafe { sys::write(target, &before[..5]) }.unwrap();
    assert_eq!(call(target, 1), 1);
}

/// `mov eax, 5; ret`: 6 bytes, enough for 5-byte jump, not the 14-byte one.
#[unsafe(naked)]
extern "C" fn short_fn() -> u32 {
    naked_asm!("mov eax, 5", "ret")
}

#[test]
fn refuses_too_short_function() {
    let mut set = HookSet::new("short");
    let f = short_fn as extern "C" fn() -> u32 as usize;
    let r = unsafe { set.detour_with(f, addr(rip_detour), true) };
    assert!(matches!(r, Err(HookError::Relocate(_))), "{r:?}");
}
