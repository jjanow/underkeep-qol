//! Platform-independent x86-64 code generation for detours: jump encoding and
//! relocation of a function's first instructions into a trampoline.
//!
//! Everything here works on byte buffers and addresses, never on live memory,
//! so it is unit-tested on any host. Decoding and re-encoding use `iced-x86`
//! (decoder and block encoder only): its `BlockEncoder` re-targets RIP-relative
//! operands and relative `jmp`/`jcc`/`call`/`loop`/`jrcxz`/`xbegin` for the new
//! address, turning short branches into near ones (or into `jmp [rip+x]`
//! sequences when even rel32 can't reach) as needed.

use iced_x86::{
    BlockEncoder, BlockEncoderOptions, Code, Decoder, DecoderOptions, Instruction,
    InstructionBlock, Mnemonic, OpKind,
};

use super::HookError;

/// Length of `jmp rel32`.
pub const JMP_REL32_LEN: usize = 5;
/// Length of `jmp qword ptr [rip+0]` followed by the 8-byte target.
pub const JMP_ABS_LEN: usize = 14;

/// `jmp rel32` placed at `from`, jumping to `to`; `None` if `to` is out of
/// rel32 range.
pub fn jmp_rel32(from: u64, to: u64) -> Option<[u8; JMP_REL32_LEN]> {
    let next = from.wrapping_add(JMP_REL32_LEN as u64);
    let disp = i32::try_from(to.wrapping_sub(next) as i64).ok()?;
    let d = disp.to_le_bytes();
    Some([0xE9, d[0], d[1], d[2], d[3]])
}

/// Position-independent `jmp qword ptr [rip+0]; dq to` (works from anywhere).
pub fn jmp_abs(to: u64) -> [u8; JMP_ABS_LEN] {
    let mut b = [0u8; JMP_ABS_LEN];
    b[..6].copy_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]);
    b[6..].copy_from_slice(&to.to_le_bytes());
    b
}

/// Whether a `rel32` displacement at `from` (measured from the end of an
/// instruction of `len` bytes) can reach `to`.
pub fn rel32_reaches(from: u64, len: usize, to: u64) -> bool {
    i32::try_from(to.wrapping_sub(from.wrapping_add(len as u64)) as i64).is_ok()
}

/// A function prologue relocated to a new address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relocated {
    /// Trampoline code for its destination address: the copied instructions,
    /// re-encoded, then a jump back to `src + consumed` (omitted when the last
    /// copied instruction is a `jmp` or `ret`).
    pub code: Vec<u8>,
    /// Bytes of the original function the trampoline replaces: whole
    /// instructions, at least the requested minimum.
    pub consumed: usize,
}

/// Relocates the first instructions of the function at `src` (whose bytes
/// start `code`) to run at `dst`, copying whole instructions until at least
/// `min_len` bytes are covered, and appends the jump back.
///
/// Refuses (`HookError::Relocate`) when:
/// - the bytes don't decode, or run out before `min_len`;
/// - the function ends (`ret`, `jmp`, `int3`, `ud2`, ...) before `min_len`
///   bytes: it's too short to hold the hook jump;
/// - a copied branch targets the middle of a copied instruction, or a copied
///   instruction reads memory inside the copied range (both would see the
///   hook jump instead of the original code);
/// - a RIP-relative operand can't reach its target from `dst` (more than
///   ±2 GB away).
///
/// It can't see jumps from *later* code back into the first `consumed` bytes
/// (a loop whose head is the function's first instruction, say); those would
/// land inside the hook jump. Compilers rarely emit that for a prologue, and
/// targets are found by pattern, so look at the function before hooking it.
pub fn relocate(code: &[u8], src: u64, dst: u64, min_len: usize) -> Result<Relocated, HookError> {
    let mut decoder = Decoder::with_ip(64, code, src, DecoderOptions::NONE);
    let mut instrs: Vec<Instruction> = Vec::new();
    let mut consumed = 0usize;
    let mut ends_flow = false;
    while consumed < min_len {
        if !decoder.can_decode() {
            return Err(HookError::Relocate(format!(
                "ran out of bytes after {consumed} of {min_len}"
            )));
        }
        let ins = decoder.decode();
        if ins.is_invalid() {
            return Err(HookError::Relocate(format!(
                "undecodable instruction at +{consumed:#x}"
            )));
        }
        let m = ins.mnemonic();
        if matches!(
            m,
            Mnemonic::Int3
                | Mnemonic::Int1
                | Mnemonic::Ud0
                | Mnemonic::Ud1
                | Mnemonic::Ud2
                | Mnemonic::Hlt
        ) {
            return Err(HookError::Relocate(format!(
                "trap instruction ({m:?}) at +{consumed:#x}: not a function start"
            )));
        }
        consumed += ins.len();
        ends_flow = matches!(
            m,
            Mnemonic::Ret | Mnemonic::Retf | Mnemonic::Iret | Mnemonic::Iretd | Mnemonic::Iretq
        ) || m == Mnemonic::Jmp;
        instrs.push(ins);
        if ends_flow && consumed < min_len {
            return Err(HookError::Relocate(format!(
                "function ends after {consumed} bytes, the hook needs {min_len}"
            )));
        }
    }

    let end = src + consumed as u64;
    let inside = |a: u64| a > src && a < end;
    let is_boundary = |a: u64| instrs.iter().any(|i| i.ip() == a);
    for ins in &instrs {
        let off = ins.ip() - src;
        let is_branch = (0..ins.op_count()).any(|i| {
            matches!(
                ins.op_kind(i),
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
            )
        });
        if is_branch && inside(ins.near_branch_target()) && !is_boundary(ins.near_branch_target()) {
            return Err(HookError::Relocate(format!(
                "branch at +{off:#x} into the middle of a copied instruction"
            )));
        }
        if ins.is_ip_rel_memory_operand()
            && ins.mnemonic() != Mnemonic::Lea
            && (src..end).contains(&ins.ip_rel_memory_address())
        {
            return Err(HookError::Relocate(format!(
                "instruction at +{off:#x} reads the code being replaced"
            )));
        }
    }

    if !ends_flow {
        // Its own IP stays 0: the block encoder treats a branch to the
        // instruction's own IP as a branch to itself.
        let back = Instruction::with_branch(Code::Jmp_rel32_64, end)
            .map_err(|e| HookError::Relocate(e.to_string()))?;
        instrs.push(back);
    }

    let block = InstructionBlock::new(&instrs, dst);
    let res = BlockEncoder::encode(64, block, BlockEncoderOptions::NONE)
        .map_err(|e| HookError::Relocate(format!("re-encoding for {dst:#x}: {e}")))?;
    Ok(Relocated {
        code: res.code_buffer,
        consumed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: u64 = 0x1_4000_1000;
    const NEAR: u64 = 0x1_3FFF_0000;
    const FAR: u64 = 0x7FF0_0000_0000;

    fn decode_all(code: &[u8], ip: u64) -> Vec<Instruction> {
        let mut d = Decoder::with_ip(64, code, ip, DecoderOptions::NONE);
        let mut v = Vec::new();
        while d.can_decode() {
            v.push(d.decode());
        }
        v
    }

    /// Follows a relocated sequence: the final target of a `jmp`, either
    /// rel32 or `jmp [rip+x]` whose qword lives in the trampoline itself.
    fn jmp_target(ins: &Instruction, code: &[u8], base: u64) -> u64 {
        assert_eq!(ins.mnemonic(), Mnemonic::Jmp);
        if ins.is_ip_rel_memory_operand() {
            let at = (ins.ip_rel_memory_address() - base) as usize;
            u64::from_le_bytes(code[at..at + 8].try_into().unwrap())
        } else {
            ins.near_branch_target()
        }
    }

    #[test]
    fn jump_encodings_decode_back() {
        let j = jmp_rel32(SRC, NEAR).unwrap();
        let i = decode_all(&j, SRC);
        assert_eq!(i.len(), 1);
        assert_eq!(i[0].near_branch_target(), NEAR);
        assert!(jmp_rel32(SRC, FAR).is_none());
        assert!(jmp_rel32(SRC, SRC + 5 + i32::MAX as u64).is_some());
        assert!(jmp_rel32(SRC, SRC + 5 + i32::MAX as u64 + 1).is_none());

        let a = jmp_abs(FAR);
        let i = decode_all(&a[..6], 0x1000);
        assert_eq!(i[0].mnemonic(), Mnemonic::Jmp);
        assert_eq!(i[0].ip_rel_memory_address(), 0x1006);
        assert_eq!(jmp_target(&i[0], &a, 0x1000), FAR);
    }

    #[test]
    fn rip_relative_operand_keeps_its_target() {
        // mov rax,[rip+0x10]; add rax,rcx; ret
        let code = [0x48, 0x8B, 0x05, 0x10, 0, 0, 0, 0x48, 0x01, 0xC8, 0xC3];
        let r = relocate(&code, SRC, NEAR, 5).unwrap();
        assert_eq!(r.consumed, 7);
        let i = decode_all(&r.code, NEAR);
        assert_eq!(i[0].code(), Code::Mov_r64_rm64);
        assert_eq!(i[0].ip_rel_memory_address(), SRC + 7 + 0x10);
        assert_eq!(jmp_target(&i[1], &r.code, NEAR), SRC + 7);

        // Needing 14 bytes copies the add too.
        let r = relocate(&code, SRC, NEAR, 14);
        assert!(matches!(r, Err(HookError::Relocate(_))), "{r:?}"); // ret at 10
    }

    #[test]
    fn rip_relative_out_of_range_is_refused() {
        let code = [0x48, 0x8B, 0x05, 0x10, 0, 0, 0, 0x90, 0xC3];
        let r = relocate(&code, SRC, FAR, 5);
        assert!(matches!(r, Err(HookError::Relocate(_))), "{r:?}");
    }

    #[test]
    fn short_jcc_is_widened_near_and_far() {
        // test rcx,rcx; jz +5; lea rax,[rcx+1]; ret; ...
        let code = [0x48, 0x85, 0xC9, 0x74, 0x05, 0x48, 0x8D, 0x41, 0x01, 0xC3];
        let target = SRC + 5 + 5;
        for dst in [NEAR, FAR] {
            let r = relocate(&code, SRC, dst, 5).unwrap();
            assert_eq!(r.consumed, 5);
            let i = decode_all(&r.code, dst);
            assert_eq!(i[0].code(), Code::Test_rm64_r64);
            if dst == NEAR {
                assert_eq!(i[1].mnemonic(), Mnemonic::Je);
                assert_eq!(i[1].near_branch_target(), target);
                assert_eq!(jmp_target(&i[2], &r.code, dst), SRC + 5);
            } else {
                // jne over an absolute jmp to the target, then the jump back.
                assert_eq!(i[1].mnemonic(), Mnemonic::Jne);
                assert_eq!(jmp_target(&i[2], &r.code, dst), target);
                let after = i.iter().position(|x| x.ip() == i[1].near_branch_target());
                let back = &i[after.expect("jne lands on an instruction")];
                assert_eq!(jmp_target(back, &r.code, dst), SRC + 5);
            }
        }
    }

    #[test]
    fn call_rel32_keeps_its_target() {
        // sub rsp,0x28; call +0x100; add rsp,0x28; ret
        let code = [
            0x48, 0x83, 0xEC, 0x28, 0xE8, 0x00, 0x01, 0, 0, 0x48, 0x83, 0xC4, 0x28, 0xC3,
        ];
        let callee = SRC + 9 + 0x100;
        for dst in [NEAR, FAR] {
            let r = relocate(&code, SRC, dst, 5).unwrap();
            assert_eq!(r.consumed, 9);
            let i = decode_all(&r.code, dst);
            assert_eq!(i[1].mnemonic(), Mnemonic::Call);
            let t = if i[1].is_ip_rel_memory_operand() {
                let at = (i[1].ip_rel_memory_address() - dst) as usize;
                u64::from_le_bytes(r.code[at..at + 8].try_into().unwrap())
            } else {
                i[1].near_branch_target()
            };
            assert_eq!(t, callee);
            assert_eq!(jmp_target(&i[2], &r.code, dst), SRC + 9);
        }
    }

    #[test]
    fn leading_jmp_is_copied_without_jump_back() {
        // jmp rel32 (a function that is itself a thunk, or already hooked).
        let code = [0xE9, 0x00, 0x10, 0, 0, 0xCC];
        let r = relocate(&code, SRC, NEAR, 5).unwrap();
        assert_eq!(r.consumed, 5);
        let i = decode_all(&r.code, NEAR);
        assert_eq!(i.len(), 1);
        assert_eq!(i[0].near_branch_target(), SRC + 5 + 0x1000);
    }

    #[test]
    fn refusals() {
        let bad = |code: &[u8], min| {
            let r = relocate(code, SRC, NEAR, min);
            assert!(
                matches!(r, Err(HookError::Relocate(_))),
                "{code:02x?}: {r:?}"
            );
        };
        bad(&[0xC3, 0xCC, 0xCC, 0xCC, 0xCC], 5); // ret first
        bad(&[0x31, 0xC0, 0xC3, 0xCC, 0xCC, 0xCC], 5); // xor eax,eax; ret
        bad(&[0xCC, 0x90, 0x90, 0x90, 0x90], 5); // int3
        bad(&[0x06, 0x90, 0x90, 0x90, 0x90], 5); // invalid in 64-bit
        bad(&[0x90, 0x90], 5); // too few bytes
        // jz +1 lands inside the following 3-byte test.
        bad(&[0x74, 0x01, 0x48, 0x85, 0xC9, 0xC3], 5);
        // mov eax,[rip-6]: reads its own bytes.
        bad(&[0x8B, 0x05, 0xFA, 0xFF, 0xFF, 0xFF, 0xC3], 5);
    }

    #[test]
    fn branch_to_copied_boundary_is_linked() {
        // xor eax,eax; jz -4 (to the xor, the function start); nop...
        let code = [0x31, 0xC0, 0x74, 0xFC, 0x90, 0x90, 0xC3];
        let r = relocate(&code, SRC, NEAR, 5).unwrap();
        let i = decode_all(&r.code, NEAR);
        assert_eq!(i[1].mnemonic(), Mnemonic::Je);
        assert_eq!(i[1].near_branch_target(), NEAR);
    }

    #[test]
    fn fourteen_byte_minimum_covers_whole_instructions() {
        // mov rax,rcx; add rax,0x11111; add rax,0x22222; ret
        let code = [
            0x48, 0x89, 0xC8, 0x48, 0x05, 0x11, 0x11, 0x01, 0x00, 0x48, 0x05, 0x22, 0x22, 0x02,
            0x00, 0xC3,
        ];
        let r = relocate(&code, SRC, FAR, JMP_ABS_LEN).unwrap();
        assert_eq!(r.consumed, 15);
        assert_eq!(&r.code[..15], &code[..15]);
        let i = decode_all(&r.code, FAR);
        assert_eq!(jmp_target(&i[3], &r.code, FAR), SRC + 15);
    }

    #[test]
    fn rel32_reach() {
        assert!(rel32_reaches(SRC, 5, NEAR));
        assert!(!rel32_reaches(SRC, 5, FAR));
    }
}
