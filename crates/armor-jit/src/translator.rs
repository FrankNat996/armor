//! Translation of a basic block of x86-64 instructions into AArch64 code
//! (ADR-0021; docs/tech/jit.md, "Com'è fatto oggi").
//!
//! - **What is translated** (`supported`): integer instructions at 8 to 64
//!   bits with 64-bit addressing, SSE2 integer and scalar SSE floating
//!   point, `LOCK` forms; everything else ends the block and goes to the
//!   interpreter. In concurrent mode (ADR-0023) `supported_concurrent`:
//!   accesses are ordered like x86's TSO (`guest_load`, `guest_store`) and
//!   atomic instructions use LSE atomics (`Translator::atomic`).
//! - **Registers:** 14 guest registers live in host registers (`GUEST`),
//!   loaded by the prologue and stored by the epilogue; the others stay in
//!   the `Cpu`.
//! - **Flags are lazy:** the operands of the last flag-setting instruction
//!   wait in `FA`/`FB`, described by `KIND` across blocks, and the flags
//!   are computed only for a reader, an exit or a slow path.
//! - **Slow paths:** a failed check sends the instruction to the
//!   interpreter, with the guest state exactly as before it.
//!
//! A block is called as `extern "C" fn(cpu, page_table, context) -> u64`.
//! Its prologue saves x19..x30 on the stack; the only call it makes is to
//! the shared `flags_routine`. It returns with `Cpu::rip` set and one of
//! the `EXIT_*` statuses.
//!
//! **Chaining** (increment 5): with `Options::chain`, each exit to a known
//! address (`jmp`, both sides of `jcc`, `call`, the size cap) is a `b` to
//! a stub that returns `EXIT_CHAIN` and the address of that `b`
//! (`Context::site`). The engine then rewrites the `b` to jump straight
//! to the next block's `CHAINED_ENTRY`, past its prologue: `FLAGS` and
//! `BUDGET` stay in their registers from block to block. Every block
//! starts by taking its instruction count from `BUDGET` and returns to
//! the engine when the budget runs out, so that chained code still lets
//! signals and other threads in (ADR-0021 §6).

use alloc::vec;
use alloc::vec::Vec;
use core::mem::offset_of;

use armor_cpu::{
    Cpu, FLAG_AF, FLAG_CF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF, R11, RAX, RCX, RDX, RSP,
    STATUS_FLAGS, mmx_layout,
};
use armor_guest::{
    PAGE_READABLE_BIT, PAGE_READABLE_SPAN_BIT, PAGE_WRITABLE_DATA_BIT, PAGE_WRITABLE_DATA_SPAN_BIT,
    REGION_GUARD,
};
use armor_x86::{
    AluOp, Base, BitTestOp, Cond as X86Cond, Conversion, FloatOp, Instruction, IntOp, LaneOp,
    LogicOp, Mem, Op, Operand, Reg, Segment, ShiftCount, ShiftKind, ShiftOp, Size, SseOp, UnaryOp,
};

use crate::encoder::{
    Assembler, AtomicOp, Barrier, Cond, Data1, EncodeError, FpOp, Label, Lanes, Logic, MemSize,
    PairMode, Reg as HostReg, SP, Select, Shift, VReg, VShift, Vec3, VecFp, VecUnary, Width, ZR,
    encode_bitmask,
};

/// Continue at `Cpu::rip` (the block ended with a jump or at its size cap).
pub const EXIT_CONTINUE: u64 = 0;
/// The instruction at `Cpu::rip` must be run by the interpreter: the
/// translator does not handle it, or one of its memory accesses needs the
/// slow path (a fault, a page crossing, a write to executable memory).
pub const EXIT_INTERPRET: u64 = 1;

/// Continue at `Cpu::rip`, and the exit at `Context::site` may be chained
/// to the block there.
pub const EXIT_CHAIN: u64 = 2;

/// Exit status: a `syscall` (64-bit mode) ran, with `rcx` and `r11` set as
/// it sets them and rip after it: the engine returns `Step::Syscall`.
pub const EXIT_SYSCALL: u64 = 3;

/// The most guest instructions in one block.
pub const MAX_BLOCK_INSTRUCTIONS: usize = 64;

/// Offset of the entry that chained blocks jump to: the block's start (the
/// prologue is `enter_routine`, shared).
pub const CHAINED_ENTRY: usize = 0;

/// What a block reads and writes besides the `Cpu`.
#[derive(Debug, Default)]
#[repr(C)]
pub struct Context {
    /// Guest instructions left before returning to the engine; the block
    /// leaves what remains (may be negative).
    pub budget: i64,
    /// After `EXIT_CHAIN`: the executable address of the `b` to patch.
    pub site: u64,
    /// Executable address of the code from `flags_routine`, which merges
    /// the flags described by `KIND` into `FLAGS`.
    pub flags_routine: u64,
    /// Address of `FLAG_TABLE`.
    pub flag_table: u64,
    /// The exit status (`EXIT_*`) of the last block run.
    pub status: u64,
    /// Lazy flags across blocks: 0 when `FLAGS` is current, otherwise the
    /// pending operation (`Pending::code`) whose operands are in `FA`/`FB`
    /// (in memory since 2026-10-20: its register holds r9).
    pub kind: u64,
    /// Address of the indirect-jump table: `TARGET_ENTRIES` pairs (guest
    /// address, executable address of that block's `CHAINED_ENTRY`),
    /// indexed by `target_slot`. Kept by the engine.
    pub targets: u64,
    /// After `EXIT_CHAIN`: `KIND` as the exit left it, before the epilogue
    /// made `FLAGS` current (`Options::entry_kind` for the target).
    pub exit_kind: u64,
    /// Executable address of `leave_routine`, where blocks go back to the
    /// engine. Kept by the engine.
    pub leave: u64,
}

/// Entries of the indirect-jump table (`Context::targets`).
pub const TARGET_ENTRIES: usize = 1 << 12;

/// The slot of guest address `rip` in the indirect-jump table; generated
/// code computes the same (`Translator::exit_to`).
pub fn target_slot(rip: u64) -> usize {
    ((rip ^ (rip >> 12)) as usize) & (TARGET_ENTRIES - 1)
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// What the host CPU has beyond ARMv8.0 (concurrent mode uses it).
    pub features: HostFeatures,
    /// Make direct exits chainable.
    pub chain: bool,
    /// Concurrent mode (ADR-0023): other threads or processes may access
    /// the guest's memory in parallel, so accesses are ordered like
    /// x86's TSO (`guest_load`, `guest_store`, and barriers around
    /// vector accesses).
    pub concurrent: bool,
    /// Compatibility mode (ADR-0029): the guest's addresses wrap at
    /// 4 GiB, after the segment base too.
    pub compat: bool,
    /// ADR-0046, host protection on (`AddressSpace::host_protection`):
    /// reads below 2^`host_reads` need no table check, the host faults
    /// where the guest may not read, and the fault goes to the slow path
    /// (`Translation::faults`). `None`: every access checks the table.
    pub host_reads: Option<u32>,
    /// `armor-mem`'s permission table is at hand (`x1`); without it (the
    /// Windows backend, ADR-0047) a misaligned write that crosses a page
    /// goes to the interpreter.
    pub page_table: bool,
    /// `Context::kind` when the block was first reached: what the blocks
    /// before it usually leave pending. An instruction that needs only CF
    /// of those flags computes it inline when `KIND` still says so, and
    /// calls the flag routine otherwise (`carry_from_kind`). 0: none.
    pub entry_kind: u64,
}

/// A translated block: its words, and its guest loads that may fault on
/// the host (with host protection, `Options::host_reads`): (word offset of
/// the load, word offset of the instruction's slow path).
#[derive(Debug, Default)]
pub struct Translation {
    pub words: Vec<u32>,
    pub faults: Vec<(u32, u32)>,
}

/// The host CPU extensions the translator may use beyond ARMv8.0
/// (ADR-0032: armor runs on every ARMv8.0 CPU). Without them, concurrent
/// mode uses ARMv8.0 replacements: LDAR for LDAPR, exclusive loops
/// (LDAXR/STLXR) for the LSE atomics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostFeatures {
    /// FEAT_LSE: LD<op>AL, SWPAL, CASAL.
    pub lse: bool,
    /// FEAT_LRCPC: LDAPR.
    pub rcpc: bool,
}

impl HostFeatures {
    /// ARMv8.0 alone.
    pub const BASELINE: HostFeatures = HostFeatures {
        lse: false,
        rcpc: false,
    };

    /// The host's, from the kernel's `AT_HWCAP` (`HWCAP_ATOMICS`,
    /// `HWCAP_LRCPC`); `ARMOR_HOST_FEATURES=baseline` pretends there are
    /// none, to run the ARMv8.0 paths on any machine (`testing.md`).
    /// From the ID registers (`ID_AA64ISAR0_EL1` for LSE,
    /// `ID_AA64ISAR1_EL1` for LRCPC), which Linux lets user space read
    /// (it emulates the access): for code that has no auxiliary vector at
    /// hand, as the Windows backend (ADR-0047) in a Wine process.
    pub fn from_id_registers() -> HostFeatures {
        let (isar0, isar1): (u64, u64);
        // SAFETY: reads two ID registers, which the kernel emulates for
        // user space (HWCAP_CPUID).
        unsafe {
            core::arch::asm!("mrs {}, id_aa64isar0_el1", out(reg) isar0, options(nomem, nostack));
            core::arch::asm!("mrs {}, id_aa64isar1_el1", out(reg) isar1, options(nomem, nostack));
        }
        HostFeatures {
            lse: (isar0 >> 20) & 0xf >= 2,
            rcpc: (isar1 >> 20) & 0xf >= 1,
        }
    }

    #[cfg(feature = "std")]
    pub fn detect() -> HostFeatures {
        const HWCAP_ATOMICS: u64 = 1 << 8;
        const HWCAP_LRCPC: u64 = 1 << 15;
        if std::env::var_os("ARMOR_HOST_FEATURES").is_some_and(|v| v == "baseline") {
            return HostFeatures::BASELINE;
        }
        // SAFETY: getauxval reads the process's auxiliary vector.
        let hwcap = unsafe { libc::getauxval(libc::AT_HWCAP) };
        HostFeatures {
            lse: hwcap & HWCAP_ATOMICS != 0,
            rcpc: hwcap & HWCAP_LRCPC != 0,
        }
    }
}

/// An ordered access to an address that is not aligned to its size,
/// done out of line with a plain access and a barrier (LDAPR and STLR
/// fault on unaligned addresses without FEAT_LSE2).
#[derive(Debug, Clone, Copy)]
enum Unaligned {
    Load {
        signed: bool,
        size: Size,
        rt: HostReg,
        /// The slow path of a load the host checks (`Translator::faults`).
        fault: Option<Label>,
    },
    Store {
        size: Size,
        rt: HostReg,
        /// As for loads.
        fault: Option<Label>,
    },
}

// Host registers. Arguments: x0 = &mut Cpu, x1 = page table, x2 = &mut
// Context. Kept from block to block (chaining): the guest's rflags, the
// budget, the lazy flags and the mapped guest registers (`GUEST`).
const CPU: HostReg = HostReg::new(0);
const PAGES: HostReg = HostReg::new(1);
const CONTEXT: HostReg = HostReg::new(2);
const FLAGS: HostReg = HostReg::new(12);
const BUDGET: HostReg = HostReg::new(13);
/// The operands of the last flag-setting instruction whose flags are not
/// in `FLAGS` yet (lazy flags, `Translator::pending`). Callee-saved: the
/// prologue saves them on the stack and the epilogue restores them.
const FA: HostReg = HostReg::new(19);
const FB: HostReg = HostReg::new(20);
/// First operand, and destination value.
const A: HostReg = HostReg::new(4);
/// Second operand.
const B: HostReg = HostReg::new(5);
/// Result.
const R: HostReg = HostReg::new(6);
/// Guest address of the memory operand.
const ADDR: HostReg = HostReg::new(7);
/// Exit: the next guest rip (the status goes to `Context::status`). The
/// same register as `A`: an exit needs no operand any more.
const EXIT_RIP: HostReg = A;
const T1: HostReg = HostReg::new(10);
const T2: HostReg = HostReg::new(11);
const T3: HostReg = HostReg::new(14);
const T4: HostReg = HostReg::new(15);

/// Guest registers kept in host registers (ADR-0021 §4), by guest number:
/// rax..rdi in x22..x29, r12..r15 in x3, x9, x16, x17. r8..r11 stay in
/// `Cpu::regs`: in the python profile they were under 4% of the register
/// uses (`tech/performance.md`). The prologue loads them, the epilogue
/// stores them back; between chained blocks they stay where they are.
/// x16 and x17 are free here: generated code goes through no veneer.
const GUEST: [Option<HostReg>; 16] = {
    const fn h(n: u8) -> Option<HostReg> {
        Some(HostReg::new(n))
    }
    [
        h(22),
        h(23),
        h(24),
        h(25),
        h(26),
        h(27),
        h(28),
        h(29),
        h(8),
        h(21),
        h(18),
        h(30),
        h(3),
        h(9),
        h(16),
        h(17),
    ]
};
// Vector temporaries (caller-saved); xmm0–15 are in v16–v31 (`xreg`).
const V1: VReg = VReg::new(1);
// More temporaries of packed floating point (`sse_packed`); v3 and v4 are
// MMX's scratch registers only inside MMX instructions.
const V3: VReg = VReg::new(3);
const V4: VReg = VReg::new(4);
const V5: VReg = VReg::new(5);
const V6: VReg = VReg::new(6);
const V2: VReg = VReg::new(2);

/// Status flags from the host's NZCV and the low byte of a result, as
/// `rflags` bits (CF, PF, ZF, SF, OF; not AF). Generated code indexes it
/// with `borrow << 12 | NZCV << 8 | byte`: `borrow` says that the host
/// carry is an inverted borrow (subtractions), so that CF = !C.
pub static FLAG_TABLE: [u16; 1 << 13] = {
    const fn bit(set: usize, flag: u64) -> u16 {
        (set as u16) << flag.trailing_zeros()
    }
    let mut table = [0u16; 1 << 13];
    let mut i = 0;
    while i < 1 << 13 {
        let byte = (i & 0xff) as u8;
        let (n, z, c, v) = ((i >> 11) & 1, (i >> 10) & 1, (i >> 9) & 1, (i >> 8) & 1);
        let cf = c ^ ((i >> 12) & 1);
        let pf = byte.count_ones().is_multiple_of(2) as usize;
        table[i] = bit(cf, FLAG_CF)
            | bit(pf, FLAG_PF)
            | bit(z, FLAG_ZF)
            | bit(n, FLAG_SF)
            | bit(v, FLAG_OF);
        i += 1;
    }
    table
};

/// One decoded guest instruction and its address.
#[derive(Debug, Clone, Copy)]
pub struct GuestInstruction {
    pub rip: u64,
    pub insn: Instruction,
}

/// How a block ends after its last translated instruction (when that one
/// is not itself a jump).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockEnd {
    /// The size cap: continue with the instruction at this address.
    Continue(u64),
    /// The instruction at this address is not translated.
    Interpret(u64),
}

/// Whether the translator handles `insn` outside concurrent mode.
/// SSE floating point that follows MXCSR's control bits (rounding, FZ,
/// DAZ, exception masks): translated only while they have their default
/// value (`Jit::sync_mxcsr`).
pub fn uses_mxcsr_control(insn: &Instruction) -> bool {
    matches!(
        insn.op,
        Op::Sse {
            op: SseOp::FloatArith { .. }
                | SseOp::FloatCompare { .. }
                | SseOp::FloatCmpMask { .. }
                | SseOp::IntToFloat { .. }
                | SseOp::FloatToInt { .. }
                | SseOp::FloatToFloat { .. }
                | SseOp::PackedArith { .. }
                | SseOp::PackedCmpMask { .. }
                | SseOp::PackedConvert(_),
            ..
        }
    )
}

pub fn supported(insn: &Instruction) -> bool {
    // LOCK: outside concurrent mode nothing else accesses the guest's
    // memory, so a plain read-modify-write is exact. In concurrent mode
    // the engine asks `supported_concurrent` instead.
    if insn.lock
        && !matches!(
            insn.op,
            Op::Alu { .. }
                | Op::Unary { .. }
                | Op::Xchg { .. }
                | Op::Cmpxchg { .. }
                | Op::Xadd { .. }
        )
    {
        return false;
    }
    let wide = matches!(insn.size, Size::B32 | Size::B64);
    // The stack width: 64 bits, 32 in compatibility mode (16-bit stack
    // operations are not decoded).
    let stack = matches!(insn.size, Size::B32 | Size::B64);
    let not_both_mem = |a: &Operand, b: &Operand| !(a.is_mem() && b.is_mem());
    match insn.op {
        Op::Mov { dst, src } => reg_or_mem(&dst) && value(&src) && not_both_mem(&dst, &src),
        Op::Extend { dst, src, .. } => wide && gpr(dst).is_some() && reg_or_mem(&src),
        Op::Lea { dst, src } => wide && gpr(dst).is_some() && address(&src),
        Op::Alu { op, dst, src } => {
            // ADC and SBB at 8 and 16 bits would need the carry in at the
            // top of a register (`narrow_arith`): left to the interpreter.
            (wide || !matches!(op, AluOp::Adc | AluOp::Sbb))
                && reg_or_mem(&dst)
                && value(&src)
                && not_both_mem(&dst, &src)
        }
        Op::Test { a, b } => reg_or_mem(&a) && value(&b) && not_both_mem(&a, &b),
        Op::Unary { dst, .. } => reg_or_mem(&dst),
        Op::Shift { op, dst, count } => {
            let kind_ok = matches!(
                op,
                ShiftOp::Shl | ShiftOp::Shr | ShiftOp::Sar | ShiftOp::Rol | ShiftOp::Ror
            );
            // 32/64 bits: a register. 8/16 bits: a register, or memory with
            // a constant count (a count of 0 from cl must not write).
            let dst_ok = if wide {
                matches!(dst, Operand::Reg(Reg::Gpr(_)))
            } else {
                match dst {
                    Operand::Reg(reg) => location(reg).is_some(),
                    Operand::Mem(m) => address(&m) && count != ShiftCount::Cl,
                    Operand::Imm(_) => false,
                }
            };
            kind_ok && dst_ok
        }
        Op::Div { src, .. } | Op::MulWide { src, .. } => wide && reg_or_mem(&src),
        Op::Xchg { a, b } => {
            wide && reg_or_mem(&a) && reg_or_mem(&b) && !(a.is_mem() && b.is_mem())
        }
        Op::Cmpxchg { dst, src } | Op::Xadd { dst, src } => {
            wide && reg_or_mem(&dst) && gpr(src).is_some()
        }
        Op::BitScan { dst, src, .. } => wide && gpr(dst).is_some() && reg_or_mem(&src),
        // A register offset into memory moves the address: interpreter.
        Op::BitTest { dst, offset, .. } => {
            wide && match (dst, offset) {
                (Operand::Reg(Reg::Gpr(_)), Operand::Reg(Reg::Gpr(_)) | Operand::Imm(_)) => true,
                (Operand::Mem(m), Operand::Imm(_)) => address(&m),
                _ => false,
            }
        }
        Op::Bswap(reg) => wide && gpr(reg).is_some(),
        Op::Imul { dst, src, .. } | Op::Cmov { dst, src, .. } => {
            wide && gpr(dst).is_some() && reg_or_mem(&src)
        }
        Op::Setcc { dst, .. } => reg_or_mem(&dst),
        Op::SignExtendAccumulator | Op::SignExtendIntoRdx => insn.size != Size::B8,
        // MMX: some forms (`mmx_supported`).
        Op::Sse { op, dst, src } if is_mmx(op, &dst, &src) => mmx_supported(op, &dst, &src),
        Op::Sse { op, dst, src } => {
            let to_xmm = xmm(&dst).is_some();
            let from_xmm = xmm(&src).is_some();
            let gpr_dst = matches!(dst, Operand::Reg(Reg::Gpr(_)));
            match op {
                SseOp::Emms | SseOp::MmxConvert(_) => false,
                SseOp::Mov128 { .. } => xmm_or_mem(&dst) && xmm_or_mem(&src),
                SseOp::MovToXmm { .. } => to_xmm && (from_xmm || reg_or_mem(&src)),
                SseOp::MovFromXmm { .. } => from_xmm && (to_xmm || reg_or_mem(&dst)),
                // The lane operations with a NEON equivalent below
                // (`vector`); the others go to the interpreter.
                SseOp::Lane { op, .. } if !translated_lane_op(op) => false,
                SseOp::Lane { .. }
                | SseOp::Logic(_)
                | SseOp::Unpack { .. }
                | SseOp::Pack { .. }
                | SseOp::Pmuludq
                | SseOp::Pshufd(_)
                | SseOp::PshufWords { .. }
                | SseOp::Shuffle { .. } => to_xmm && xmm_or_mem(&src),
                SseOp::ShiftImm { .. } | SseOp::ByteShift { .. } => to_xmm,
                // pshufb: NEON's TBL.
                SseOp::Int(IntOp::Pshufb) => to_xmm && xmm_or_mem(&src),
                // palignr: NEON's EXT.
                SseOp::Int(IntOp::Palignr(_)) => to_xmm && xmm_or_mem(&src),
                SseOp::Pmovmskb | SseOp::Pextrw(_) => gpr_dst && from_xmm,
                SseOp::Pinsrw(_) => to_xmm && reg_or_mem(&src),
                SseOp::MovHalf { .. } | SseOp::MovScalar { .. } => {
                    (to_xmm && (src.is_mem() || from_xmm)) || (dst.is_mem() && from_xmm)
                }
                SseOp::MovHlps | SseOp::MovLhps => to_xmm && from_xmm,
                // rcpss / rsqrtss: interpreter.
                SseOp::FloatArith {
                    op: FloatOp::Rcp | FloatOp::Rsqrt,
                    ..
                } => false,
                SseOp::FloatArith { .. }
                | SseOp::FloatCmpMask { .. }
                | SseOp::FloatToFloat { .. } => {
                    to_xmm && (from_xmm || src.is_mem() && xmm_or_mem(&src))
                }
                SseOp::FloatCompare { .. } => {
                    to_xmm && (from_xmm || src.is_mem() && xmm_or_mem(&src))
                }
                SseOp::IntToFloat { .. } => to_xmm && reg_or_mem(&src),
                SseOp::FloatToInt { .. } => {
                    gpr_dst && (from_xmm || src.is_mem() && xmm_or_mem(&src))
                }
                SseOp::FloatSignMask { .. } => gpr_dst && from_xmm,
                // Packed floating point (`sse_packed`), but rcpps/rsqrtps
                // and cvtpd2dq.
                SseOp::PackedArith {
                    op: FloatOp::Rcp | FloatOp::Rsqrt,
                    ..
                }
                | SseOp::PackedConvert(Conversion::Pd2Dq { .. }) => false,
                SseOp::PackedArith { .. }
                | SseOp::PackedCmpMask { .. }
                | SseOp::PackedConvert(_) => to_xmm && xmm_or_mem(&src),
                // The rarer SSE2 integer forms and SSE3: interpreter
                // (docs/tech/jit.md).
                SseOp::AddSub { .. }
                | SseOp::Horizontal { .. }
                | SseOp::Int(_)
                | SseOp::Ptest
                | SseOp::PcmpStr { .. }
                | SseOp::Pmovx { .. }
                | SseOp::Extract { .. }
                | SseOp::Insert { .. }
                | SseOp::Insertps(_)
                | SseOp::Round { .. }
                | SseOp::Dp { .. }
                | SseOp::MovDup(_)
                | SseOp::ShiftXmm { .. }
                | SseOp::Pmaddwd
                | SseOp::Psadbw
                | SseOp::Maskmovdqu => false,
            }
        }
        Op::Push(src) => stack && value(&src),
        Op::Pop(dst) => stack && matches!(dst, Operand::Reg(Reg::Gpr(_))),
        Op::CallIndirect(src) | Op::JmpIndirect(src) => stack && reg_or_mem(&src),
        Op::Call { .. } | Op::Ret { .. } => stack,
        Op::Jmp { .. } | Op::Jcc { .. } | Op::Nop | Op::Fence | Op::Syscall => true,
        _ => false,
    }
}

/// Whether the translator handles `insn` in concurrent mode (ADR-0023):
/// x86's atomic instructions (`needs_atomic`) only when one LSE atomic
/// does the work (`atomic_rmw`); `lock adc`/`sbb`/`neg`/`bt*`,
/// `cmpxchg8b`/`16b` and the narrow forms the translator lacks go to the
/// interpreter, which makes them atomic on the host.
pub fn supported_concurrent(insn: &Instruction) -> bool {
    supported(insn) && (!needs_atomic(insn) || atomic_rmw(insn))
}

/// Instructions x86 makes atomic: LOCK-prefixed read-modify-writes, and
/// xchg with a memory operand.
pub fn needs_atomic(insn: &Instruction) -> bool {
    insn.lock || matches!(insn.op, Op::Xchg { a, b } if a.is_mem() || b.is_mem())
}

/// Atomic instructions with a single LSE equivalent (`Translator::atomic`):
/// add, sub, and, or, xor, inc, dec, not, xchg, cmpxchg and xadd on memory.
fn atomic_rmw(insn: &Instruction) -> bool {
    match insn.op {
        Op::Alu { op, dst, .. } => {
            dst.is_mem()
                && matches!(
                    op,
                    AluOp::Add | AluOp::Sub | AluOp::And | AluOp::Or | AluOp::Xor
                )
        }
        Op::Unary { op, dst } => dst.is_mem() && op != UnaryOp::Neg,
        Op::Xchg { a, b } => a.is_mem() || b.is_mem(),
        Op::Cmpxchg { dst, .. } | Op::Xadd { dst, .. } => dst.is_mem(),
        _ => false,
    }
}

/// Lane operations `vector` translates.
fn translated_lane_op(op: LaneOp) -> bool {
    matches!(
        op,
        LaneOp::Add
            | LaneOp::Sub
            | LaneOp::CmpEq
            | LaneOp::CmpGt
            | LaneOp::MinU
            | LaneOp::MaxU
            | LaneOp::MinS
            | LaneOp::MaxS
    )
}

/// Whether `insn` reads or writes the status flags.
fn touches_flags(insn: &Instruction) -> bool {
    match insn.op {
        Op::Alu { .. }
        | Op::Test { .. }
        | Op::Shift { .. }
        | Op::Imul { .. }
        | Op::Cmov { .. }
        | Op::Setcc { .. }
        | Op::BitScan { .. }
        | Op::BitTest { .. }
        | Op::MulWide { .. }
        | Op::Cmpxchg { .. }
        | Op::Xadd { .. }
        | Op::Jcc { .. } => true,
        Op::Unary { op, .. } => op != UnaryOp::Not,
        Op::Sse {
            op: SseOp::FloatCompare { .. },
            ..
        } => true,
        _ => false,
    }
}

/// Whether the translator defers the flags of `insn` (`defer`): they wait
/// as its operands and are computed only if something observes them.
/// inc and dec keep CF: the earlier CF goes into `FLAGS` first when it is
/// live (`materialize_carry`).
fn lazy_flags(insn: &Instruction) -> bool {
    match insn.op {
        Op::Alu { op, .. } => !matches!(op, AluOp::Adc | AluOp::Sbb),
        Op::Test { .. }
        | Op::Cmpxchg { .. }
        | Op::Xadd { .. }
        | Op::Unary { .. }
        | Op::Imul { .. }
        | Op::MulWide { .. } => true,
        Op::Shift { op, dst, .. } => lazy_shift(op, dst, insn.size),
        Op::Sse {
            op: SseOp::FloatCompare { .. },
            ..
        } => true,
        _ => false,
    }
}

/// shl, shr and sar of a 32- or 64-bit register defer their flags
/// (`FlagKind::Shift`); rotates and narrow shifts compute them.
fn lazy_shift(op: ShiftOp, dst: Operand, size: Size) -> bool {
    matches!(op, ShiftOp::Shl | ShiftOp::Shr | ShiftOp::Sar)
        && matches!(dst, Operand::Reg(Reg::Gpr(_)))
        && matches!(size, Size::B32 | Size::B64)
}

/// The host registers (bit n: xn) of the guest registers `insn` may
/// write; every mapped one for instructions not listed.
fn registers_written(insn: &Instruction) -> u32 {
    let reg = |o: &Operand| match o {
        Operand::Reg(Reg::Gpr(n) | Reg::HighByte(n)) => {
            GUEST[usize::from(*n)].map_or(0, |h| 1 << h.bits())
        }
        _ => 0,
    };
    let gpr = |n: usize| GUEST[n].map_or(0, |h| 1u32 << h.bits());
    let r = |r: &Reg| reg(&Operand::Reg(*r));
    match insn.op {
        Op::Mov { dst, .. }
        | Op::Setcc { dst, .. }
        | Op::Unary { dst, .. }
        | Op::Shift { dst, .. } => reg(&dst),
        Op::Alu { op, dst, .. } => {
            if op == AluOp::Cmp {
                0
            } else {
                reg(&dst)
            }
        }
        Op::BitTest { op, dst, .. } => {
            if op == BitTestOp::Bt {
                0
            } else {
                reg(&dst)
            }
        }
        Op::Extend { dst, .. }
        | Op::Lea { dst, .. }
        | Op::Imul { dst, .. }
        | Op::Cmov { dst, .. }
        | Op::BitScan { dst, .. } => r(&dst),
        Op::Bswap(dst) => r(&dst),
        Op::Test { .. }
        | Op::Jcc { .. }
        | Op::Jmp { .. }
        | Op::Nop
        | Op::Fence
        | Op::JmpIndirect(_) => 0,
        Op::Xchg { a, b } => reg(&a) | reg(&b),
        Op::Xadd { dst, src } => reg(&dst) | r(&src),
        Op::Cmpxchg { dst, .. } => reg(&dst) | gpr(RAX),
        Op::MulWide { .. } | Op::Div { .. } => gpr(RAX) | gpr(RDX),
        Op::SignExtendAccumulator => gpr(RAX),
        Op::SignExtendIntoRdx => gpr(RDX),
        Op::Push(_) | Op::Call { .. } | Op::CallIndirect(_) | Op::Ret { .. } => gpr(RSP),
        Op::Pop(dst) => reg(&dst) | gpr(RSP),
        Op::Sse { dst, .. } => reg(&dst),
        _ => u32::MAX,
    }
}

/// A mapped register that `insn` moves by a constant, and by how much:
/// `push` and `pop` (rsp, 64-bit stack), and `add`, `sub` of an immediate
/// and `lea` from itself plus a constant, at 64 bits. For the checked
/// addresses (`Translator::checked`), which follow it.
fn base_moved(insn: &Instruction) -> Option<(HostReg, i64)> {
    if insn.size != Size::B64 {
        return None;
    }
    let rsp = register_host(RSP as u8)?;
    match insn.op {
        Op::Push(_) => Some((rsp, -8)),
        Op::Pop(Operand::Reg(Reg::Gpr(n))) if usize::from(n) != RSP => Some((rsp, 8)),
        Op::Alu {
            op: op @ (AluOp::Add | AluOp::Sub),
            dst: Operand::Reg(Reg::Gpr(n)),
            src: Operand::Imm(imm),
        } => {
            let delta = if op == AluOp::Add {
                imm
            } else {
                imm.wrapping_neg()
            };
            Some((register_host(n)?, delta))
        }
        Op::Lea {
            dst: Reg::Gpr(n),
            src:
                Mem {
                    base: Base::Gpr(b),
                    index: None,
                    addr32: false,
                    disp,
                    ..
                },
        } if n == b => Some((register_host(n)?, disp)),
        _ => None,
    }
}

/// The count of a shift masked as x86 masks it, when it is a constant.
fn shift_constant(count: ShiftCount, size: Size) -> Option<u32> {
    let mask = if size == Size::B64 { 0x3f } else { 0x1f };
    match count {
        ShiftCount::One => Some(1),
        ShiftCount::Imm(n) => Some(u32::from(n) & mask),
        ShiftCount::Cl => None,
    }
}

/// Whether `mov r1, r2` (registers mapped whole) can leave its copy to
/// `next`, an in-place shift by a constant or ALU operation on r1 which
/// reads r1 only as its first operand: `next` then reads r2 there
/// (`Translator::first`), `lsr x1, x2, #n` instead of `mov x1, x2; lsr
/// x1, x1, #n`. Its deferred flags take that operand from r2 too (a
/// shift's `FA`), or need only the result and the second (`Held`). A
/// 32-bit copy only into a 32-bit operation (the zero-extension is the
/// operation's).
fn copy_fuses(mov: &Instruction, next: &Instruction) -> bool {
    let Op::Mov {
        dst: dst @ Operand::Reg(r1),
        src: src @ Operand::Reg(_),
    } = mov.op
    else {
        return false;
    };
    let (Some(h1), Some(h2)) = (whole_register(dst, mov.size), whole_register(src, mov.size))
    else {
        return false;
    };
    if h1 == h2
        || flags_read(next) != 0
        || whole_register(dst, next.size).is_none()
        || (mov.size == Size::B32 && next.size != Size::B32)
    {
        return false;
    }
    match next.op {
        Op::Shift {
            op: ShiftOp::Shl | ShiftOp::Shr | ShiftOp::Sar,
            dst: Operand::Reg(r),
            count,
        } => r == r1 && shift_constant(count, next.size).is_some_and(|n| n != 0),
        Op::Alu {
            op: AluOp::Add | AluOp::Sub | AluOp::And | AluOp::Or | AluOp::Xor,
            dst: Operand::Reg(r),
            src,
        } => {
            r == r1
                && (matches!(src, Operand::Imm(_)) || matches!(src, Operand::Reg(r3) if r3 != r1))
        }
        _ => false,
    }
}

/// The status flags `insn` writes, as the interpreter does, on every path
/// that goes on inside the block: for the liveness of `live_flags`. An
/// under-estimate is safe (flags then pass through as live). Shifts and
/// rotates by cl write none when the count is 0.
fn flags_written(insn: &Instruction) -> u64 {
    match insn.op {
        Op::Alu { .. }
        | Op::Test { .. }
        | Op::Imul { .. }
        | Op::MulWide { .. }
        | Op::Cmpxchg { .. }
        | Op::Xadd { .. } => STATUS_FLAGS,
        Op::Unary { op, .. } => match op {
            UnaryOp::Neg => STATUS_FLAGS,
            UnaryOp::Inc | UnaryOp::Dec => STATUS_FLAGS & !FLAG_CF,
            UnaryOp::Not => 0,
        },
        Op::Shift { op, count, .. } => {
            let rotate = matches!(op, ShiftOp::Rol | ShiftOp::Ror);
            match shift_constant(count, insn.size) {
                Some(0) => 0,
                Some(_) if rotate => FLAG_CF | FLAG_OF,
                Some(_) => STATUS_FLAGS,
                None => 0,
            }
        }
        Op::BitTest { .. } => FLAG_CF,
        Op::BitScan { .. } => FLAG_ZF,
        Op::Sse {
            op: SseOp::FloatCompare { .. },
            ..
        } => STATUS_FLAGS,
        _ => 0,
    }
}

/// The status flags `insn` may write on some path: those it always writes,
/// and for shifts and rotates by cl those of a count that is not 0.
fn flags_may_write(insn: &Instruction) -> u64 {
    match insn.op {
        Op::Shift {
            op: ShiftOp::Rol | ShiftOp::Ror,
            count: ShiftCount::Cl,
            ..
        } => FLAG_CF | FLAG_OF,
        Op::Shift {
            count: ShiftCount::Cl,
            ..
        } => STATUS_FLAGS,
        _ => flags_written(insn),
    }
}

/// The status flags `insn` reads: a condition's, the carry of adc and
/// sbb. A conditional jump leaves the block when taken, where every flag
/// is observed.
fn flags_read(insn: &Instruction) -> u64 {
    match insn.op {
        Op::Jcc { .. } | Op::Syscall => STATUS_FLAGS,
        Op::Setcc { cond, .. } | Op::Cmov { cond, .. } => condition_flags(cond),
        Op::Alu {
            op: AluOp::Adc | AluOp::Sbb,
            ..
        } => FLAG_CF,
        _ => 0,
    }
}

/// The flags an x86 condition tests.
fn condition_flags(cond: X86Cond) -> u64 {
    use X86Cond as C;
    match cond {
        C::O | C::No => FLAG_OF,
        C::B | C::Ae => FLAG_CF,
        C::E | C::Ne => FLAG_ZF,
        C::S | C::Ns => FLAG_SF,
        C::P | C::Np => FLAG_PF,
        C::Be | C::A => FLAG_CF | FLAG_ZF,
        C::L | C::Ge => FLAG_SF | FLAG_OF,
        C::Le | C::G => FLAG_ZF | FLAG_SF | FLAG_OF,
    }
}

/// Whether `insn` can only run to its end inside the block: registers
/// only, and no slow path. Any other instruction may hand the guest state
/// to the interpreter (a permission check, #DE, a floating-point case),
/// which needs every flag exact.
fn stays_in_block(insn: &Instruction) -> bool {
    let reg = |o: &Operand| !o.is_mem();
    match insn.op {
        Op::Mov { dst, src } | Op::Alu { dst, src, .. } => reg(&dst) && reg(&src),
        Op::Test { a, b } | Op::Xchg { a, b } => reg(&a) && reg(&b),
        Op::Unary { dst, .. }
        | Op::Shift { dst, .. }
        | Op::Setcc { dst, .. }
        | Op::Cmpxchg { dst, .. }
        | Op::Xadd { dst, .. } => reg(&dst),
        Op::Extend { src, .. }
        | Op::Imul { src, .. }
        | Op::MulWide { src, .. }
        | Op::BitScan { src, .. }
        | Op::Cmov { src, .. } => reg(&src),
        Op::BitTest { dst, offset, .. } => reg(&dst) && reg(&offset),
        Op::Lea { .. }
        | Op::Bswap(_)
        | Op::SignExtendAccumulator
        | Op::SignExtendIntoRdx
        | Op::Nop => true,
        _ => false,
    }
}

/// For each instruction of `block`, the status flags whose value after it
/// may be observed: by a later reader, at the block's exits, or by the
/// interpreter at a slow path. A backward pass from the end of the block,
/// where every flag is live.
fn live_flags(block: &[GuestInstruction]) -> Vec<u64> {
    let mut live = STATUS_FLAGS;
    let mut after = vec![0; block.len()];
    for (i, g) in block.iter().enumerate().rev() {
        after[i] = live;
        let insn = &g.insn;
        live = if stays_in_block(insn) {
            live & !flags_written(insn) | flags_read(insn)
        } else {
            STATUS_FLAGS
        };
    }
    after
}

/// The host condition that, after redoing the pending operation `p`,
/// holds exactly when x86 condition `cond` does; `None` when there is none
/// (parity, or CF when INC/DEC kept an older one, or CF | ZF after an
/// addition).
fn host_condition(p: Pending, cond: X86Cond) -> Option<Cond> {
    use X86Cond as C;
    if let FlagKind::FloatCompare { .. } = p.kind {
        // FCMP's NZCV: unordered 0011, less 1000, equal 0110, greater
        // 0010; x86's ZF PF CF: 111, 001, 100, 000.
        return match cond {
            C::A => Some(Cond::Gt),
            C::Ae => Some(Cond::Ge),
            C::B => Some(Cond::Lt),
            C::Be => Some(Cond::Le),
            C::P => Some(Cond::Vs),
            C::Np => Some(Cond::Vc),
            _ => None,
        };
    }
    // Only operations whose flags are the host NZCV of redoing them.
    if !matches!(p.kind, FlagKind::Add | FlagKind::Sub | FlagKind::Logic) {
        return None;
    }
    let carry_known = p.mask & FLAG_CF != 0;
    Some(match cond {
        C::O => Cond::Vs,
        C::No => Cond::Vc,
        C::E => Cond::Eq,
        C::Ne => Cond::Ne,
        C::S => Cond::Mi,
        C::Ns => Cond::Pl,
        C::L => Cond::Lt,
        C::Ge => Cond::Ge,
        C::Le => Cond::Le,
        C::G => Cond::Gt,
        C::P | C::Np => return None,
        _ if !carry_known => return None,
        // Subtraction: CF is the inverted host carry.
        C::B if p.kind == FlagKind::Sub => Cond::Lo,
        C::Ae if p.kind == FlagKind::Sub => Cond::Hs,
        C::Be if p.kind == FlagKind::Sub => Cond::Ls,
        C::A if p.kind == FlagKind::Sub => Cond::Hi,
        // Addition: CF is the host carry; logical: C is clear, CF = 0.
        C::B => Cond::Hs,
        C::Ae => Cond::Lo,
        C::Be if p.kind == FlagKind::Logic => Cond::Eq,
        C::A if p.kind == FlagKind::Logic => Cond::Ne,
        C::Be | C::A => return None,
    })
}

/// Whether `insn` ends a block: an unconditional change of `rip`. A
/// conditional jump leaves the block only when taken (a side exit), so
/// that what follows it shares the block's registers and lazy flags.
pub fn ends_block(insn: &Instruction) -> bool {
    matches!(
        insn.op,
        Op::Jmp { .. }
            | Op::Call { .. }
            | Op::CallIndirect(_)
            | Op::JmpIndirect(_)
            | Op::Ret { .. }
            | Op::Syscall
    )
}

fn gpr(reg: Reg) -> Option<u8> {
    match reg {
        Reg::Gpr(n) => Some(n),
        _ => None,
    }
}

/// An xmm register, or one of the two MMX scratch registers
/// (`Reg::Mm(8)`, `Reg::Mm(9)`) as 16 and 17: the SSE translations run
/// MMX instructions on them, as the interpreter does (`mmx`).
fn xmm(operand: &Operand) -> Option<u8> {
    match operand {
        Operand::Reg(Reg::Xmm(n)) => Some(*n),
        Operand::Reg(Reg::Mm(n @ 8..=9)) => Some(16 + n - 8),
        _ => None,
    }
}

/// An MMX register mm0–mm7.
fn mm(operand: &Operand) -> Option<u8> {
    match operand {
        Operand::Reg(Reg::Mm(n @ 0..=7)) => Some(*n),
        _ => None,
    }
}

/// The MMX instructions (an mm operand, emms, the conversions).
fn is_mmx(op: SseOp, dst: &Operand, src: &Operand) -> bool {
    matches!(op, SseOp::Emms | SseOp::MmxConvert(_)) || mm(dst).is_some() || mm(src).is_some()
}

/// The MMX instructions the translator handles: the moves, emms, and the
/// lane operations whose low 64 bits on zero-extended operands are the
/// MMX result, among those translated for xmm. The conversions, packs,
/// high unpacks and the rest go to the interpreter.
fn mmx_supported(op: SseOp, dst: &Operand, src: &Operand) -> bool {
    let mm_or_mem = |o: &Operand| match o {
        Operand::Mem(m) => address(m),
        _ => mm(o).is_some(),
    };
    let gpr_or_mem = |o: &Operand| match o {
        Operand::Mem(m) => address(m),
        Operand::Reg(Reg::Gpr(_)) => true,
        _ => false,
    };
    match op {
        SseOp::Emms => true,
        // movd/movq mm, r/m; movq mm, mm/m64; movq2dq xmm, mm.
        SseOp::MovToXmm { .. } => {
            (mm(dst).is_some() && (gpr_or_mem(src) || mm(src).is_some()))
                || (xmm(dst).is_some() && mm(src).is_some())
        }
        // movd/movq r/m, mm; movq/movntq mm/m64, mm; movdq2q mm, xmm.
        SseOp::MovFromXmm { .. } => {
            (mm(src).is_some() && (gpr_or_mem(dst) || mm(dst).is_some()))
                || (mm(dst).is_some() && xmm(src).is_some())
        }
        SseOp::Lane { op, .. } => translated_lane_op(op) && mm(dst).is_some() && mm_or_mem(src),
        SseOp::Logic(_)
        | SseOp::Pmuludq
        | SseOp::Unpack { high: false, .. }
        | SseOp::PshufWords { high: false, .. } => mm(dst).is_some() && mm_or_mem(src),
        SseOp::ShiftImm { .. } => mm(dst).is_some(),
        // pinsrw mm, r32/m16, imm: one word of the register.
        SseOp::Pinsrw(_) => mm(dst).is_some() && gpr_or_mem(src),
        _ => false,
    }
}

fn xmm_or_mem(operand: &Operand) -> bool {
    match operand {
        Operand::Mem(mem) => address(mem),
        _ => xmm(operand).is_some(),
    }
}

/// The target of a relative branch: `size` is the stack width, and with
/// 32 bits (compatibility mode) eip wraps at 4 GiB.
fn target(next: u64, rel: i64, size: Size) -> u64 {
    let target = next.wrapping_add(rel as u64);
    if size == Size::B32 {
        target & 0xffff_ffff
    } else {
        target
    }
}

/// 32-bit addresses (the address-size prefix in 64-bit mode, every
/// address in compatibility mode) are truncated by `Translator::address`.
fn address(mem: &Mem) -> bool {
    matches!(mem.scale, 1 | 2 | 4 | 8)
}

/// A general-purpose register (AH–BH included) or memory.
fn reg_or_mem(operand: &Operand) -> bool {
    match operand {
        Operand::Reg(reg) => location(*reg).is_some(),
        Operand::Mem(mem) => address(mem),
        Operand::Imm(_) => false,
    }
}

fn value(operand: &Operand) -> bool {
    matches!(operand, Operand::Imm(_)) || reg_or_mem(operand)
}

/// Translates `block` (every instruction `supported`; only the last may
/// end the block) into a function for `CodeBuffer::publish`. `end` says
/// how to leave when the last instruction falls through.
pub fn translate(
    block: &[GuestInstruction],
    end: BlockEnd,
    options: Options,
) -> Result<Vec<u32>, EncodeError> {
    translate_block(block, end, options).map(|t| t.words)
}

/// `translate`, with the loads that may fault (`Translation::faults`).
pub fn translate_block(
    block: &[GuestInstruction],
    end: BlockEnd,
    options: Options,
) -> Result<Translation, EncodeError> {
    let mut t = Translator::bare(options);
    // Whatever the block before left pending, described by `KIND`.
    t.lazy = Lazy::Dynamic;
    t.slow_paths.reserve(block.len());
    t.page_edges.reserve(block.len());
    t.faults.reserve(block.len());
    let epilogue = t.a.new_label();
    t.epilogue = Some(epilogue);
    // The block starts at its budget check: from the engine through
    // `enter_routine` (registers loaded, `FLAGS` current and
    // `Context::kind` 0, as `leave_routine` leaves them), from another
    // block straight here (`CHAINED_ENTRY`).
    // The budget: when it runs out, back to the engine before this block.
    let start = block.first().expect("a block has an instruction").rip;
    if t.options.chain {
        t.loop_start = Some(start);
    }
    let live = live_flags(block);
    let out_of_budget = t.a.new_label();
    t.a.subs_imm(Width::X, BUDGET, BUDGET, block.len() as u32)?;
    t.a.b_cond(Cond::Mi, out_of_budget);
    t.body(block, &live, end)?;
    t.side_exits_out_of_line()?;
    // The loop's copy, if an exit asked for one: from the state of that
    // exit, its own budget check and way out of it.
    while let Some(i) = t.loops.iter().position(|l| l.3) {
        let (lazy, normal, head, _) = t.loops[i];
        t.loops[i].3 = false;
        t.a.bind(head);
        t.lazy = lazy;
        t.normal = normal;
        t.checked.clear();
        t.nzcv_valid = false;
        t.nzcv_now = false;
        let copy_out_of_budget = t.a.new_label();
        t.a.subs_imm(Width::X, BUDGET, BUDGET, block.len() as u32)?;
        t.a.b_cond(Cond::Mi, copy_out_of_budget);
        t.body(block, &live, end)?;
        t.side_exits_out_of_line()?;
        t.a.bind(copy_out_of_budget);
        t.lazy = lazy;
        t.exit(start, EXIT_CONTINUE)?;
    }
    t.a.bind(out_of_budget);
    t.lazy = Lazy::Dynamic;
    t.exit(start, EXIT_CONTINUE)?;
    t.lazy = Lazy::Current;
    // Chainable exits: the `b` at `site` comes here until it is patched.
    for (site, stub, rip) in core::mem::take(&mut t.chain_stubs) {
        t.a.bind(stub);
        t.a.mov_imm64(EXIT_RIP, rip);
        t.a.adr(T1, site);
        t.a.str(MemSize::B64, T1, CONTEXT, field(offset_of!(Context, site)))?;
        t.a.ldr(MemSize::B64, T1, CONTEXT, field(offset_of!(Context, kind)))?;
        t.a.str(
            MemSize::B64,
            T1,
            CONTEXT,
            field(offset_of!(Context, exit_kind)),
        )?;
        let entry = t.status_entry(EXIT_CHAIN);
        t.a.b(entry);
    }
    // Unaligned writes the host checks: outside the region, the slow path;
    // within one page, the host; across two, both pages from the table.
    // In concurrent mode the unaligned case goes on to the plain access
    // and barrier (`misaligned`), not back to the store-release.
    for (edge, back, bytes, outside, slow, unaligned) in core::mem::take(&mut t.write_edges) {
        let back = unaligned.unwrap_or(back);
        t.a.bind(edge);
        t.a.logic_imm(Logic::Ands, Width::X, ZR, ADDR, outside)?;
        t.a.b_cond(Cond::Ne, slow);
        t.a.logic_imm(Logic::And, Width::X, T1, ADDR, 0xfff)?;
        t.a.subs_imm(Width::X, ZR, T1, 4096 - bytes)?;
        t.a.b_cond(Cond::Ls, back);
        if !t.options.page_table {
            t.a.b(slow);
            continue;
        }
        t.a.shift_imm(Shift::Lsr, Width::X, T1, ADDR, 12)?;
        t.a.load_store_reg(true, MemSize::B8, T1, PAGES, T1, false);
        t.a.tbz(false, T1, PAGE_WRITABLE_DATA_SPAN_BIT, slow);
        t.a.b(back);
    }
    // Unaligned reads in concurrent mode: outside the region, the slow
    // path; otherwise the plain load and barrier.
    for (edge, outside, slow, unaligned) in core::mem::take(&mut t.read_edges) {
        t.a.bind(edge);
        t.a.logic_imm(Logic::Ands, Width::X, ZR, ADDR, outside)?;
        t.a.b_cond(Cond::Ne, slow);
        t.a.b(unaligned);
    }
    for (unaligned, after) in core::mem::take(&mut t.misaligned_stubs) {
        t.a.bind(unaligned);
        t.a.b(after);
    }
    // Checks near the end of a page.
    for (edge, back, alone, bytes, slow) in core::mem::take(&mut t.page_edges) {
        t.page_edge(edge, back, alone, bytes, slow)?;
    }
    for (generic, back) in core::mem::take(&mut t.kind_carries) {
        t.a.bind(generic);
        t.lazy = Lazy::Dynamic;
        t.materialize()?;
        t.a.b(back);
    }
    // Ordered accesses to unaligned addresses (concurrent mode).
    for (entry, back, access) in core::mem::take(&mut t.unaligned) {
        t.unaligned_access(entry, back, access)?;
    }
    // Operands with a zero exponent: denormal or zero.
    for (entry, back, slow, w) in core::mem::take(&mut t.denormals) {
        t.a.bind(entry);
        t.a.shift_imm(Shift::Lsl, w, T1, T1, 1)?;
        t.a.cbz(true, w, T1, slow);
        t.a.b(back);
    }
    // Shifts by a cl of 0: the flags before them into `FLAGS`. The count
    // (`T3`) is 0 again for the deferred flags.
    for (entry, back, lazy) in core::mem::take(&mut t.zero_counts) {
        t.a.bind(entry);
        t.lazy = lazy;
        t.materialize()?;
        t.a.mov(Width::X, T3, ZR);
        t.a.b(back);
    }
    // UFC after packed floating point: cleared, then the slow path.
    for (entry, slow) in core::mem::take(&mut t.packed_underflows) {
        t.a.bind(entry);
        t.a.read_fpsr(T1);
        t.a.logic_imm(Logic::And, Width::X, T1, T1, !(1u64 << 3))?;
        t.a.write_fpsr(T1);
        t.a.b(slow);
    }
    // UFC raised by the host, made x86's UE.
    for (entry, back, slow, double) in core::mem::take(&mut t.underflows) {
        t.underflow_fixup(entry, back, slow, double)?;
    }
    // Slow paths: the instruction at `rip` goes to the interpreter, which
    // finds the state exactly as before it.
    for (label, rip, lazy) in core::mem::take(&mut t.slow_paths) {
        t.a.bind(label);
        t.lazy = lazy;
        t.exit(rip, EXIT_INTERPRET)?;
    }
    // The exit statuses, then the epilogue.
    for (status, entry) in core::mem::take(&mut t.status_entries) {
        t.a.bind(entry);
        t.set_status(status)?;
        t.a.b(epilogue);
    }
    // Back to the engine through the shared `leave_routine`.
    t.a.bind(epilogue);
    t.a.ldr(MemSize::B64, T1, CONTEXT, field(offset_of!(Context, leave)))?;
    t.a.br(T1);
    let faults = core::mem::take(&mut t.faults)
        .into_iter()
        .map(|(at, slow)| {
            let target = t.a.position(slow).expect("slow paths are bound");
            (at as u32, target as u32)
        })
        .collect();
    Ok(Translation {
        words: t.a.finish()?,
        faults,
    })
}

/// Where the engine enters translated code: `enter(cpu, page_table,
/// context, block)` saves x19–x30 (callee-saved, and the return address),
/// loads the guest's registers, `FLAGS` and the budget, and branches to the
/// block (its `CHAINED_ENTRY`), which leaves through `leave_routine`.
/// Shared by every block of a code buffer (`compat`: compatibility mode,
/// whose blocks keep r8–r15's host registers untouched).
pub fn enter_routine(compat: bool) -> Result<Vec<u32>, EncodeError> {
    let mut t = Translator::bare(routine_options(compat));
    // The block's address, before x3 takes a guest register.
    t.a.mov(Width::X, T1, HostReg::new(3));
    t.a.pair(
        false,
        PairMode::PreIndex,
        HostReg::new(19),
        HostReg::new(20),
        SP,
        -96,
    )?;
    for (i, n) in [21u8, 23, 25, 27, 29].into_iter().enumerate() {
        let offset = 16 * (i as i32 + 1);
        t.a.pair(
            false,
            PairMode::Offset,
            HostReg::new(n),
            HostReg::new(n + 1),
            SP,
            offset,
        )?;
    }
    t.guest_registers(true)?;
    t.vector_registers(true)?;
    t.a.ldr(MemSize::B64, FLAGS, CPU, field(offset_of!(Cpu, rflags)))?;
    t.a.ldr(
        MemSize::B64,
        BUDGET,
        CONTEXT,
        field(offset_of!(Context, budget)),
    )?;
    t.a.br(T1);
    t.a.finish()
}

/// Where translated code goes back to the engine (`Context::leave`), with
/// the exit's rip in `EXIT_RIP` and its status in `Context::status`: the
/// flags pending into `rflags`, the guest's registers and the budget back
/// into the `Cpu` and the `Context`, x19–x30 restored, the status returned.
pub fn leave_routine(compat: bool) -> Result<Vec<u32>, EncodeError> {
    let mut t = Translator::bare(routine_options(compat));
    t.lazy = Lazy::Dynamic;
    t.materialize()?;
    t.a.str(MemSize::B64, FLAGS, CPU, field(offset_of!(Cpu, rflags)))?;
    t.a.str(MemSize::B64, EXIT_RIP, CPU, field(offset_of!(Cpu, rip)))?;
    t.a.str(
        MemSize::B64,
        BUDGET,
        CONTEXT,
        field(offset_of!(Context, budget)),
    )?;
    t.guest_registers(false)?;
    t.vector_registers(false)?;
    for (i, n) in [21u8, 23, 25, 27, 29].into_iter().enumerate().rev() {
        let offset = 16 * (i as i32 + 1);
        t.a.pair(
            true,
            PairMode::Offset,
            HostReg::new(n),
            HostReg::new(n + 1),
            SP,
            offset,
        )?;
    }
    t.a.pair(
        true,
        PairMode::PostIndex,
        HostReg::new(19),
        HostReg::new(20),
        SP,
        96,
    )?;
    // Last: x0 is the `Cpu` pointer until here.
    t.a.ldr(
        MemSize::B64,
        HostReg::new(0),
        CONTEXT,
        field(offset_of!(Context, status)),
    )?;
    t.a.ret();
    t.a.finish()
}

fn routine_options(compat: bool) -> Options {
    Options {
        features: HostFeatures::BASELINE,
        chain: false,
        concurrent: false,
        compat,
        host_reads: None,
        page_table: true,
        entry_kind: 0,
    }
}

/// The routine that generated code calls (`blr`, address in
/// `Context::flags_routine`) to merge the flags `KIND` describes into
/// `FLAGS` when the operation is not known at translation time: at the
/// start of a block, whatever the block before left pending. Clears
/// `KIND`; uses `R` and `T1`–`T3`.
pub fn flags_routine() -> Result<Vec<u32>, EncodeError> {
    let mut t = Translator::bare(Options {
        features: HostFeatures::BASELINE,
        chain: false,
        concurrent: false,
        compat: false,
        host_reads: None,
        page_table: true,
        entry_kind: 0,
    });
    // A table of branches indexed by `KIND` (codes are below 32): one
    // computed jump to the code for that operation.
    let table = t.a.new_label();
    let done = t.a.new_label();
    t.a.ldr(MemSize::B64, T1, CONTEXT, field(offset_of!(Context, kind)))?;
    t.a.logic_imm(Logic::And, Width::X, T1, T1, 31)?;
    t.a.adr(T2, table);
    t.a.add(Width::X, T2, T2, T1, Shift::Lsl, 2)?;
    t.a.br(T2);
    let mut targets = vec![done; 32];
    let mut bodies = Vec::new();
    for kind in [FlagKind::Add, FlagKind::Sub, FlagKind::Logic] {
        for size in [Size::B8, Size::B16, Size::B32, Size::B64] {
            for partial in [false, true] {
                if partial && kind == FlagKind::Logic {
                    continue;
                }
                let mask = if partial {
                    STATUS_FLAGS & !FLAG_CF
                } else {
                    STATUS_FLAGS
                };
                let p = Pending::new(kind, size, mask);
                let body = t.a.new_label();
                targets[usize::from(p.code().expect("a routine kind"))] = body;
                bodies.push((body, p));
            }
        }
    }
    t.a.bind(table);
    for target in targets {
        t.a.b(target);
    }
    for (body, p) in bodies {
        t.a.bind(body);
        t.lazy = Lazy::Static(p);
        t.materialize()?;
        t.a.b(done);
    }
    // KIND 0 (nothing pending) lands here directly.
    t.a.bind(done);
    t.clear_kind()?;
    t.a.ret();
    t.a.finish()
}

fn field(offset: usize) -> u32 {
    u32::try_from(offset).expect("Cpu field offsets are small")
}

fn reg_offset(n: u8) -> u32 {
    field(offset_of!(Cpu, regs)) + 8 * u32::from(n)
}

/// Where a general-purpose register is (AH–BH: bits 8–15 of rax–rbx).
#[derive(Clone, Copy)]
enum Loc {
    /// A host register, from this bit.
    Host(HostReg, u32),
    /// Bytes in the `Cpu` at this offset.
    Mem(u32),
}

/// The host register of a guest register operand mapped whole, when
/// `size` lets an operation read it in place (`operand_in`).
fn whole_register(operand: Operand, size: Size) -> Option<HostReg> {
    match operand {
        Operand::Reg(reg) if matches!(size, Size::B32 | Size::B64) => match location(reg)? {
            Loc::Host(h, 0) => Some(h),
            _ => None,
        },
        _ => None,
    }
}

/// The host register of guest register `n` when it is mapped.
fn register_host(n: u8) -> Option<HostReg> {
    GUEST[usize::from(n)]
}

fn location(reg: Reg) -> Option<Loc> {
    let (n, high) = match reg {
        Reg::Gpr(n) => (n, false),
        Reg::HighByte(n) => (n, true),
        Reg::Xmm(_) | Reg::Mm(_) => return None,
    };
    Some(match (GUEST[usize::from(n)], high) {
        (Some(h), high) => Loc::Host(h, if high { 8 } else { 0 }),
        (None, high) => Loc::Mem(reg_offset(n) + u32::from(high)),
    })
}

/// The vector register of an xmm operand (`xmm`): v16–v31 for xmm0–15,
/// loaded by the prologue and stored by the epilogue like the mapped
/// general registers; v3 and v4 for the two MMX scratch registers, which
/// live only inside one instruction.
fn xreg(operand: &Operand) -> VReg {
    match xmm(operand).expect("checked by supported") {
        n @ 0..16 => VReg::new(16 + n),
        n => VReg::new(3 + n - 16),
    }
}

fn bytes(size: Size) -> u32 {
    size.bits() / 8
}

/// The bit of an xmm register's vector register in `Translator::normal`
/// (0 for the scratch registers, never known).
fn normal_bit(v: VReg) -> u32 {
    match v.bits() {
        n @ 16..32 => 1 << n,
        _ => 0,
    }
}

fn lanes(size: Size) -> Lanes {
    match size {
        Size::B8 => Lanes::B,
        Size::B16 => Lanes::H,
        Size::B32 => Lanes::S,
        Size::B64 => Lanes::D,
    }
}

fn width(size: Size) -> Width {
    match size {
        Size::B64 => Width::X,
        _ => Width::W,
    }
}

fn mem_size(size: Size) -> MemSize {
    match size {
        Size::B8 => MemSize::B8,
        Size::B16 => MemSize::B16,
        Size::B32 => MemSize::B32,
        Size::B64 => MemSize::B64,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    Read,
    Write,
}

/// How an arithmetic instruction sets CF, OF and AF.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FlagKind {
    /// CF is the host carry; OF the host overflow; AF from the operands.
    Add,
    /// CF is the inverted host carry (a borrow).
    Sub,
    /// CF, OF and AF are cleared.
    Logic,
    /// shl, shr or sar of the value in `FA` by a constant count, or by the
    /// count in `FB` (masked; 0 leaves the flags as they are in `FLAGS`).
    /// Known only inside the block: exits compute it first.
    Shift(ShiftOp, Option<u8>),
    /// A multiplication of `FA` by `FB` (both extended to 64 bits): CF and
    /// OF say that the product does not fit `size`. Inside the block only.
    Mul { signed: bool },
    /// comisd, comiss, ucomisd, ucomiss of the bits in `FA` and `FB`: ZF,
    /// PF and CF from the comparison, OF, SF and AF cleared. Inside the
    /// block only.
    FloatCompare { double: bool },
}

type Emit<T = ()> = Result<T, EncodeError>;

/// The address of the instruction being translated, and of the next one
/// (the base of RIP-relative operands).
#[derive(Clone, Copy)]
struct Site {
    rip: u64,
    next: u64,
}

/// Flags computed lazily: the status flags of the last flag-setting
/// instruction, not yet merged into `FLAGS`, follow from its operands in
/// `FA` and `FB` (for logical operations, its result in `FA`).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pending {
    kind: FlagKind,
    size: Size,
    /// The flags the instruction sets (INC and DEC keep CF).
    mask: u64,
    /// Where the operands are (for a logical operation, `a` is the
    /// result): `FA` and `FB`, or still where the instruction found them,
    /// a guest register mapped whole or a constant (`Translator::spill`
    /// copies them to `FA`/`FB` when they are about to change or leave).
    a: Held,
    b: Held,
    /// `a` holds the result of the addition or subtraction rather than
    /// its first operand (`a` = result - b, or result + b): the
    /// instruction wrote its destination in place.
    result: bool,
}

/// Where a pending operand is (`Pending`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Held {
    Fa,
    Fb,
    /// A guest register mapped whole, unchanged since the instruction.
    Reg(HostReg),
    /// A constant (zero-extended from the size).
    Imm(u64),
}

impl Pending {
    /// Operands in `FA` and `FB`.
    fn new(kind: FlagKind, size: Size, mask: u64) -> Pending {
        Pending {
            kind,
            size,
            mask,
            a: Held::Fa,
            b: Held::Fb,
            result: false,
        }
    }

    /// Whether the operands are in `FA`/`FB`, as the flag routine and the
    /// shift and multiplication kinds expect.
    fn plain(self) -> bool {
        self.a == Held::Fa && self.b == Held::Fb && !self.result
    }

    /// The guest registers holding operands.
    fn held_registers(self) -> impl Iterator<Item = HostReg> {
        [self.a, self.b].into_iter().filter_map(|h| match h {
            Held::Reg(r) => Some(r),
            _ => None,
        })
    }

    /// The value of `KIND` for this operation, never 0; `None` for the
    /// kinds `flags_routine` does not know, which exits compute first.
    fn code(self) -> Option<u16> {
        if !self.plain() {
            return None;
        }
        let kind = match self.kind {
            FlagKind::Add => 1,
            FlagKind::Sub => 2,
            FlagKind::Logic => 3,
            FlagKind::Shift(..) | FlagKind::Mul { .. } | FlagKind::FloatCompare { .. } => {
                return None;
            }
        };
        let size = match self.size {
            Size::B8 => 0,
            Size::B16 => 1,
            Size::B32 => 2,
            Size::B64 => 3,
        };
        Some(kind | size << 2 | u16::from(self.mask & FLAG_CF == 0) << 4)
    }

    /// The operation of a `KIND` value (`code`'s inverse), operands in
    /// `FA`/`FB`.
    fn from_code(code: u64) -> Option<Pending> {
        let kind = match code & 3 {
            1 => FlagKind::Add,
            2 => FlagKind::Sub,
            3 => FlagKind::Logic,
            _ => return None,
        };
        if code >> 5 != 0 {
            return None;
        }
        let size = [Size::B8, Size::B16, Size::B32, Size::B64][(code >> 2 & 3) as usize];
        let mask = if code & 16 != 0 {
            STATUS_FLAGS & !FLAG_CF
        } else {
            STATUS_FLAGS
        };
        Some(Pending::new(kind, size, mask))
    }
}

/// Where the status flags are, at a point of the generated code.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lazy {
    /// In `FLAGS`.
    Current,
    /// To be computed from `FA`/`FB`: the operation is known here.
    Static(Pending),
    /// As `KIND` says (at the start of a block).
    Dynamic,
}

struct Translator {
    a: Assembler,
    epilogue: Option<Label>,
    /// Entries of the epilogue that first record an exit status (status,
    /// label), shared by every exit with that status.
    status_entries: Vec<(u64, Label)>,
    /// Slow paths: label, instruction, lazy flags at that instruction.
    slow_paths: Vec<(Label, u64, Lazy)>,
    /// Out of line: the taken side of a conditional jump inside the block
    /// (label, target, lazy flags at the jump).
    side_exits: Vec<(Label, u64, Lazy, [u32; 2])>,
    /// The block's first instruction, when exits may become loops (not in
    /// lockstep): an exit back to it with the flags known goes to a copy
    /// of the block translated from that state (`loops`).
    loop_start: Option<u64>,
    /// Copies of the block's body (`exit_direct`): the state they are
    /// translated from (the flags pending, the xmm values known), their
    /// head, and whether they are still to be emitted.
    loops: Vec<(Lazy, [u32; 2], Label, bool)>,
    /// The instruction being translated is the block's last.
    last: bool,
    /// Lazy flags at the current point of the emitted code.
    lazy: Lazy,
    /// Chainable exits: the patchable `b`, its stub, the target.
    chain_stubs: Vec<(Label, Label, u64)>,
    options: Options,
    /// Out of line: `check` for accesses near the end of a page whose next
    /// page does not allow them (entry, back, bit, size, slow path).
    page_edges: Vec<(Label, Label, u32, u32, Label)>,
    /// Out of line: ordered accesses to unaligned addresses (entry, back).
    unaligned: Vec<(Label, Label, Unaligned)>,
    /// Out of line: UFC raised by the host, made x86's UE (entry, back,
    /// slow path, double; `underflow`).
    underflows: Vec<(Label, Label, Label, bool)>,
    /// xmm registers whose low lane is known not to be denormal, as a
    /// double (`[0]`) and as a single (`[1]`), by host vector register
    /// (bits 16–31): `slow_if_denormal` needs no test for them
    /// (`update_normal`).
    normal: [u32; 2],
    /// Out of line: the rest of `slow_if_denormal` (entry, back, slow
    /// path, width).
    denormals: Vec<(Label, Label, Label, Width)>,
    /// Out of line: a shift by cl whose count is 0 (entry, back, lazy
    /// flags before it): the pending flags into `FLAGS`, where the shift's
    /// deferred flags then find them (`lazy_shift`).
    zero_counts: Vec<(Label, Label, Lazy)>,
    /// Out of line: CF when `KIND` is not the expected one (entry, back):
    /// the flag routine (`carry_from_kind`).
    kind_carries: Vec<(Label, Label)>,
    /// The host NZCV holds the pending operation's flags, redone for a
    /// condition (`redo_nzcv`), up to the end of the current instruction.
    nzcv_valid: bool,
    /// For the current instruction: the host NZCV still holds the pending
    /// operation's flags from the previous one (`nzcv_valid`), which a
    /// jcc, setcc or cmov that reads registers only may use as they are.
    nzcv_now: bool,
    /// Where the next in-place operation finds its first operand, when
    /// the copy before it was left to it (`copy_fuses`).
    first: Option<HostReg>,
    /// The current instruction's copy is left to the next (`copy_fuses`).
    fuse_copy: bool,
    /// The slow path of the read `check` just let the host check
    /// (`Options::host_reads`): the next guest load records it in
    /// `faults` (`mark_access`).
    access_slow: Option<Label>,
    /// The address `address_into` just left in `ADDR`, when it is a mapped
    /// base register plus a constant: (base, constant, the assembler's
    /// position right after it, so that `check` uses it only for that
    /// address).
    simple_address: Option<(HostReg, u64, usize)>,
    /// The address `address_into` just left in `ADDR`, when it is known at
    /// translation time (RIP-relative or absolute, without an index), and
    /// the assembler's position right after it: `check` decides its region
    /// test then.
    const_address: Option<(u64, usize)>,
    /// Addresses whose region test passed in this block, with the base
    /// register unchanged since (base, constant, the alignment tested):
    /// an access near one needs none (`REGION_GUARD`). Dropped when an
    /// instruction writes the base.
    checked: Vec<(HostReg, u64, u32)>,
    /// `check` found the access covered by `checked`, aligned to its size:
    /// the next ordered access needs no alignment test.
    known_aligned: bool,
    /// Guest loads the host checks: (word offset, slow path).
    faults: Vec<(usize, Label)>,
    /// Out of line: writes the host checks whose address is not aligned
    /// (entry, back, size, the bits outside the region, slow path).
    write_edges: Vec<(Label, Label, u32, u64, Label, Option<Label>)>,
    /// Out of line: reads the host checks whose address is not aligned, in
    /// concurrent mode (entry, the bits outside the region, slow path,
    /// where an unaligned access goes: `misaligned`).
    read_edges: Vec<(Label, u64, Label, Label)>,
    /// In concurrent mode with host protection, the check of the access
    /// about to be made also tested its alignment (`check`): where its
    /// unaligned case goes (taken by `guest_load` or `guest_store` for
    /// their out-of-line access), and the point right after the check.
    misaligned: Option<(Label, Label)>,
    /// Out of line: an unaligned case no ordered access took (an access
    /// of another kind): back to right after the check.
    misaligned_stubs: Vec<(Label, Label)>,
    /// Out of line: UFC after packed floating point (entry, slow path;
    /// `slow_if_underflow`).
    packed_underflows: Vec<(Label, Label)>,
    /// The status flags live after the current instruction (`live_flags`).
    live: u64,
    /// The current instruction's flags are dead: nothing computes them.
    skip_flags: bool,
    /// The flags the current instruction's code writes (`defer`,
    /// `merge_flags`), checked against `flags_written` in debug builds.
    flags_emitted: u64,
}

impl Translator {
    /// The block's instructions, and its exit when the last one falls
    /// through.
    fn body(&mut self, block: &[GuestInstruction], live: &[u64], end: BlockEnd) -> Emit {
        let mut fell_through = true;
        for (i, (g, &after)) in block.iter().zip(live).enumerate() {
            self.live = after;
            self.last = i + 1 == block.len();
            debug_assert!(
                supported(&g.insn),
                "translating an unsupported {:?}",
                g.insn
            );
            // Vector and floating-point accesses to guest memory have no
            // acquire/release forms: a full barrier before (release, for a
            // store) and a load barrier after (acquire, for a load).
            let vector_access = self.options.concurrent
                && matches!(g.insn.op, Op::Sse { dst, src, .. } if dst.is_mem() || src.is_mem());
            if vector_access {
                self.a.dmb(Barrier::Ish);
            }
            self.fuse_copy = block
                .get(i + 1)
                .is_some_and(|n| copy_fuses(&g.insn, &n.insn));
            fell_through = self.instruction(g)?;
            if !core::mem::take(&mut self.fuse_copy) {
                debug_assert!(self.first.is_none(), "a copy left to {:?}", g.insn);
                self.first = None;
            }
            self.flush_misaligned();
            self.update_normal(&g.insn);
            // Checked addresses follow a base that moved by a constant;
            // those of any other base the instruction wrote are dropped.
            let mut written = registers_written(&g.insn);
            if let Some((moved, delta)) = base_moved(&g.insn) {
                written &= !(1 << moved.bits());
                for (base, disp, _) in &mut self.checked {
                    if *base == moved {
                        *disp = disp.wrapping_sub(delta as u64);
                    }
                }
            }
            self.checked
                .retain(|&(base, _, _)| written & (1 << base.bits()) == 0);
            if vector_access {
                self.a.dmb(Barrier::IshLd);
            }
        }
        if fell_through {
            match end {
                BlockEnd::Continue(rip) => self.exit_direct(rip)?,
                BlockEnd::Interpret(rip) => self.exit(rip, EXIT_INTERPRET)?,
            }
        }
        Ok(())
    }

    /// The taken sides of the conditional jumps inside the block: each
    /// starts with the state of the point it is reached from.
    fn side_exits_out_of_line(&mut self) -> Emit {
        for (label, rip, lazy, normal) in core::mem::take(&mut self.side_exits) {
            self.a.bind(label);
            self.lazy = lazy;
            self.normal = normal;
            self.exit_direct(rip)?;
        }
        Ok(())
    }

    /// A translator with nothing emitted, the flags current.
    fn bare(options: Options) -> Translator {
        Translator {
            a: Assembler::new(),
            epilogue: None,
            status_entries: Vec::new(),
            slow_paths: Vec::new(),
            side_exits: Vec::new(),
            loop_start: None,
            loops: Vec::new(),
            last: false,
            chain_stubs: Vec::new(),
            page_edges: Vec::new(),
            unaligned: Vec::new(),
            underflows: Vec::new(),
            denormals: Vec::new(),
            zero_counts: Vec::new(),
            kind_carries: Vec::new(),
            nzcv_valid: false,
            nzcv_now: false,
            first: None,
            fuse_copy: false,
            access_slow: None,
            simple_address: None,
            const_address: None,
            checked: Vec::new(),
            known_aligned: false,
            normal: [0; 2],
            faults: Vec::new(),
            write_edges: Vec::new(),
            read_edges: Vec::new(),
            misaligned: None,
            misaligned_stubs: Vec::new(),
            packed_underflows: Vec::new(),
            live: STATUS_FLAGS,
            skip_flags: false,
            flags_emitted: 0,
            options,
            lazy: Lazy::Current,
        }
    }

    /// Emits one instruction; returns whether execution falls through to
    /// the next one.
    fn instruction(&mut self, g: &GuestInstruction) -> Emit<bool> {
        let insn = &g.insn;
        let next = g.rip.wrapping_add(insn.len as u64);
        let size = insn.size;
        let w = width(size);
        // The accesses the host checks belong to one instruction's checks.
        self.access_slow = None;
        // The NZCV of the pending flags survives only into a condition
        // reader that emits nothing before reading it.
        let valid = core::mem::take(&mut self.nzcv_valid);
        self.nzcv_now = valid
            && match insn.op {
                Op::Jcc { .. } => true,
                Op::Setcc { .. } => true,
                Op::Cmov { dst, src, .. } => {
                    !src.is_mem() && whole_register(Operand::Reg(dst), size).is_some()
                }
                _ => false,
            };
        // Allocated lazily: only instructions that touch memory have one.
        let mut slow = None;
        let at = Site { rip: g.rip, next };
        // Flags nothing will observe (`live_flags`): not computed, and the
        // lazy state stays as it was. Every flag this instruction writes is
        // overwritten before anything reads it, so the older state it
        // leaves behind is never seen either.
        let written = flags_written(insn);
        let reads = flags_read(insn);
        let may_write = flags_may_write(insn);
        self.skip_flags = may_write != 0 && may_write & self.live == 0 && reads == 0;
        // An instruction that sets every status flag from its operands
        // alone does not need the earlier ones.
        let overwrites = written == STATUS_FLAGS && reads == 0;
        // Instructions that read flags, or write only some of them, need
        // the lazy flags in `FLAGS` first; inc and dec (which keep CF) and
        // adc and sbb (which read it) only the pending CF, and inc and dec
        // not even that when CF is dead.
        // Pending operands held in a register this instruction writes:
        // to `FA`/`FB` first, unless it replaces the pending flags with
        // its own (its slow paths, before any write, still find them).
        if let Lazy::Static(p) = self.lazy
            && !p.plain()
        {
            let written_regs = registers_written(insn);
            let replaces = lazy_flags(insn)
                && !self.skip_flags
                && !matches!(
                    insn.op,
                    Op::Unary {
                        op: UnaryOp::Not,
                        ..
                    }
                );
            // Flags pending that nothing observes after this instruction
            // (`live`, after it): nothing need survive the write. (The
            // instruction itself reads them before it writes a register.)
            let dead = self.live & p.mask == 0;
            if !replaces
                && !dead
                && p.held_registers()
                    .any(|r| written_regs & (1 << r.bits()) != 0)
            {
                self.spill()?;
            }
        }
        let keeps_carry = matches!(
            insn.op,
            Op::Unary {
                op: UnaryOp::Inc | UnaryOp::Dec,
                ..
            }
        );
        let needs_earlier = touches_flags(insn)
            && !self.skip_flags
            && !matches!(insn.op, Op::Jcc { .. } | Op::Cmov { .. } | Op::Setcc { .. });
        if needs_earlier && keeps_carry {
            if self.live & FLAG_CF != 0 {
                self.materialize_carry()?;
            }
        } else if needs_earlier && reads == FLAG_CF && written == STATUS_FLAGS {
            self.materialize_carry()?;
        } else if needs_earlier && !lazy_flags(insn) && !overwrites {
            self.materialize()?;
        }
        self.flags_emitted = 0;
        if self.options.concurrent && needs_atomic(insn) {
            self.atomic(insn, at, &mut slow)?;
            self.flags_done(insn, written);
            return Ok(true);
        }
        match insn.op {
            Op::Nop => {}
            // syscall in 64-bit mode, as the interpreter runs it: rcx = the
            // next rip, r11 = rflags (exact), then to the engine, which
            // hands it to the kernel's emulation. In compatibility mode, the
            // interpreter.
            Op::Syscall => {
                if self.options.compat {
                    self.exit(g.rip, EXIT_INTERPRET)?;
                    return Ok(false);
                }
                self.materialize()?;
                self.a.mov_imm64(B, next);
                self.store_reg(Reg::Gpr(RCX as u8), Size::B64, B)?;
                self.store_reg(Reg::Gpr(R11 as u8), Size::B64, FLAGS)?;
                self.exit(next, EXIT_SYSCALL)?;
                return Ok(false);
            }
            // lfence, mfence, sfence: nothing to order with one runner; a
            // full barrier in concurrent mode (ADR-0023).
            Op::Fence => {
                if self.options.concurrent {
                    self.a.dmb(Barrier::Ish);
                }
            }
            Op::Mov { dst, src } => match whole_register(dst, size) {
                // Straight into the mapped register: a load or an
                // immediate lands there, a register is one move.
                Some(h) => {
                    let b = self.operand_in(h, src, size, at, &mut slow)?;
                    if self.fuse_copy {
                        self.first = Some(b);
                    // `mov r32, same r32` still clears the top half.
                    } else if b != h || (size == Size::B32 && src == dst) {
                        self.a.mov(w, h, b);
                    }
                }
                // A store writes only `size` bytes, and a 64-bit register
                // kept in memory takes all of a mapped one.
                None if matches!(dst, Operand::Mem(_)) || size == Size::B64 => {
                    let b = self.operand_in(B, src, size, at, &mut slow)?;
                    self.write_operand(dst, size, b, at, &mut slow)?;
                }
                // A narrower register kept in memory: 64 bits, zero-extended
                // by `read_operand`.
                None => {
                    self.read_operand(B, src, size, at, &mut slow)?;
                    self.write_operand(dst, size, B, at, &mut slow)?;
                }
            },
            Op::Extend {
                signed,
                dst,
                src,
                src_size,
            } => {
                // Straight into a mapped destination of 32 or 64 bits.
                let rt = whole_register(Operand::Reg(dst), size).unwrap_or(B);
                match src {
                    Operand::Reg(reg) => self.read_reg(signed, rt, reg, src_size)?,
                    Operand::Mem(m) => {
                        match self.direct(&m, bytes(src_size), Check::Read, at.rip, &mut slow) {
                            Some((base, offset)) => {
                                self.load(signed, src_size, rt, base, offset)?
                            }
                            None => {
                                self.address(&m, next, true)?;
                                self.check(Check::Read, bytes(src_size), at.rip, &mut slow)?;
                                self.guest_load(signed, src_size, rt)?;
                            }
                        }
                    }
                    Operand::Imm(_) => unreachable!("checked by supported"),
                }
                // Zero-extended loads and UBFX already clear the top half.
                if w == Width::W && signed {
                    self.a.mov(Width::W, rt, rt);
                }
                if rt == B {
                    self.store_reg(dst, size, B)?;
                }
            }
            Op::Lea { dst, src } => {
                // LEA computes the offset only: no segment base. Straight
                // into a mapped destination when the base is mapped too.
                let rd = match (whole_register(Operand::Reg(dst), size), src.base) {
                    (Some(h), Base::Gpr(n)) if register_host(n).is_some() => h,
                    _ => ADDR,
                };
                // A 32-bit offset computed in W registers (zero-extended).
                self.address_into_w(rd, &src, next, false, w == Width::W)?;
                if rd == ADDR {
                    self.store_reg(dst, size, ADDR)?;
                }
            }
            Op::Alu { op, dst, src }
                if !matches!(op, AluOp::Adc | AluOp::Sbb)
                    && whole_register(dst, size).is_some() =>
            {
                let h = whole_register(dst, size).expect("checked by the guard");
                self.alu_in_place(op, h, src, size, at, &mut slow)?;
            }
            Op::Alu { op, dst, src } => {
                let a = match dst {
                    // A comparison's memory operand straight to `FA`, as in
                    // `alu_in_place`: its only access.
                    Operand::Mem(m) if op == AluOp::Cmp && !self.skip_flags => {
                        self.address(&m, at.next, true)?;
                        self.check(Check::Read, bytes(size), at.rip, &mut slow)?;
                        self.guest_load(false, size, FA)?;
                        FA
                    }
                    _ => self.destination_in(dst, size, at, &mut slow, op != AluOp::Cmp)?,
                };
                // A comparison's constant straight to `FB` too (after its
                // only access).
                let rt = if op == AluOp::Cmp && !self.skip_flags && matches!(src, Operand::Imm(_)) {
                    FB
                } else {
                    B
                };
                let b = self.operand_in(rt, src, size, at, &mut slow)?;
                let kind = match op {
                    AluOp::Add => {
                        self.arith(false, size, a, b)?;
                        FlagKind::Add
                    }
                    // cmp: nothing but the deferred operands.
                    AluOp::Cmp => FlagKind::Sub,
                    AluOp::Sub => {
                        self.arith(true, size, a, b)?;
                        FlagKind::Sub
                    }
                    AluOp::Adc => {
                        // Host carry = CF (32 and 64 bits only).
                        self.a.shift_imm(Shift::Lsl, Width::X, T1, FLAGS, 29)?;
                        self.a.logic_imm(Logic::And, Width::X, T1, T1, 1 << 29)?;
                        self.a.write_flags(T1);
                        self.a.adc(w, R, a, b, true);
                        FlagKind::Add
                    }
                    AluOp::Sbb => {
                        // Host carry = !CF (AArch64 subtracts NOT carry).
                        self.a.shift_imm(Shift::Lsl, Width::X, T1, FLAGS, 29)?;
                        self.a.logic_imm(Logic::Eor, Width::X, T1, T1, 1 << 29)?;
                        self.a.logic_imm(Logic::And, Width::X, T1, T1, 1 << 29)?;
                        self.a.write_flags(T1);
                        self.a.sbc(w, R, a, b, true);
                        FlagKind::Sub
                    }
                    AluOp::And | AluOp::Or | AluOp::Xor => {
                        let logic = match op {
                            AluOp::And => Logic::And,
                            AluOp::Or => Logic::Orr,
                            _ => Logic::Eor,
                        };
                        self.a.logic(logic, false, w, R, a, b, Shift::Lsl, 0)?;
                        FlagKind::Logic
                    }
                };
                // To memory, the store first: with host protection it may
                // fault, and the slow path wants the flags as they were.
                // The host flags of the operation survive a store.
                let stored = dst.is_mem() && op != AluOp::Cmp;
                if stored {
                    self.write_destination(dst, size, R)?;
                }
                if matches!(op, AluOp::Adc | AluOp::Sbb) {
                    self.flags(kind, a, b, STATUS_FLAGS)?;
                } else if op == AluOp::Cmp
                    && a == FA
                    && whole_register(src, size).is_some()
                    && !self.skip_flags
                {
                    // `cmp [mem], reg`: the register stays where it is,
                    // held (`Held::Reg`) until it changes.
                    self.flags_emitted |= STATUS_FLAGS;
                    let mut pending = Pending::new(kind, size, STATUS_FLAGS);
                    pending.b = Held::Reg(b);
                    self.lazy = Lazy::Static(pending);
                } else {
                    self.defer(kind, size, a, b, STATUS_FLAGS);
                }
                if op != AluOp::Cmp && !stored {
                    self.write_destination(dst, size, R)?;
                }
            }
            // test of a mapped register with itself, at 32 or 64 bits: the
            // result is the register, held there (`Held`) until it changes.
            Op::Test { a, b }
                if a == b
                    && matches!(size, Size::B32 | Size::B64)
                    && whole_register(a, size).is_some() =>
            {
                if !self.skip_flags {
                    let h = whole_register(a, size).expect("checked by the guard");
                    self.flags_emitted |= STATUS_FLAGS;
                    let mut pending = Pending::new(FlagKind::Logic, size, STATUS_FLAGS);
                    pending.a = Held::Reg(h);
                    self.lazy = Lazy::Static(pending);
                }
            }
            Op::Test { a, b } => {
                // The result goes straight to `FA`; with dead flags only
                // the operands are read (a memory operand may fault).
                let a = self.operand_in(A, a, size, at, &mut slow)?;
                let imm = match b {
                    Operand::Imm(imm) => Some(imm as u64 & size.mask()),
                    _ => None,
                };
                let encodable = imm.is_some_and(|v| {
                    encode_bitmask(v, if w == Width::X { 64 } else { 32 }).is_some()
                });
                let b = match imm {
                    Some(_) if encodable => ZR,
                    _ => self.operand_in(B, b, size, at, &mut slow)?,
                };
                if !self.skip_flags {
                    match imm {
                        Some(v) if encodable => self.a.logic_imm(Logic::And, w, FA, a, v)?,
                        _ => self
                            .a
                            .logic(Logic::And, false, w, FA, a, b, Shift::Lsl, 0)?,
                    }
                    self.defer_result(size);
                }
            }
            Op::Unary { op, dst } if whole_register(dst, size).is_some() => {
                let h = whole_register(dst, size).expect("checked by the guard");
                self.unary_in_place(op, h, size)?;
            }
            Op::Unary { op, dst } => {
                self.read_destination(dst, size, at, &mut slow, true)?;
                match op {
                    UnaryOp::Inc | UnaryOp::Dec => {
                        self.a.movz(Width::X, B, 1, 0)?;
                        let sub = op == UnaryOp::Dec;
                        self.arith(sub, size, A, B)?;
                        let kind = if sub { FlagKind::Sub } else { FlagKind::Add };
                        // INC and DEC leave CF unchanged: in `FLAGS`, put
                        // there before the instruction when it is live
                        // (`materialize_carry`); a dead CF is the host's.
                        let mask = if self.live & FLAG_CF == 0 {
                            STATUS_FLAGS
                        } else {
                            STATUS_FLAGS & !FLAG_CF
                        };
                        // To memory, the store before the flags (as ALU).
                        if dst.is_mem() {
                            self.write_destination(dst, size, R)?;
                        }
                        self.defer(kind, size, A, B, mask);
                    }
                    UnaryOp::Neg => {
                        // 0 - a.
                        self.arith(true, size, ZR, A)?;
                        if dst.is_mem() {
                            self.write_destination(dst, size, R)?;
                        }
                        self.defer(FlagKind::Sub, size, ZR, A, STATUS_FLAGS);
                    }
                    UnaryOp::Not => self.a.logic(Logic::Orr, true, w, R, ZR, A, Shift::Lsl, 0)?,
                }
                if !dst.is_mem() || op == UnaryOp::Not {
                    self.write_destination(dst, size, R)?;
                }
            }
            Op::Shift { op, dst, count } => match dst {
                Operand::Reg(reg) if matches!(size, Size::B32 | Size::B64) => {
                    self.shift(op, reg, count, size)?
                }
                _ => self.narrow_shift(op, dst, count, size, at, &mut slow)?,
            },
            Op::Div { signed, src } => self.divide(signed, src, size, at, &mut slow)?,
            Op::MulWide { signed, src } => {
                // rdx:rax = rax * src.
                let (rax, rdx) = (Reg::Gpr(RAX as u8), Reg::Gpr(RDX as u8));
                match src {
                    Operand::Reg(reg) => self.read_reg(signed, B, reg, size)?,
                    Operand::Mem(m) => {
                        self.address(&m, next, true)?;
                        self.check(Check::Read, bytes(size), at.rip, &mut slow)?;
                        self.guest_load(signed, size, B)?;
                    }
                    Operand::Imm(_) => unreachable!("checked by supported"),
                }
                self.read_reg(signed, A, rax, size)?;
                if size == Size::B64 {
                    self.a.madd(Width::X, R, A, B, ZR);
                    self.a.mulh(signed, T3, A, B);
                } else {
                    // The whole product fits in 64 bits.
                    self.a.madd(Width::X, T4, A, B, ZR);
                    let high = if signed { Shift::Asr } else { Shift::Lsr };
                    self.a.shift_imm(high, Width::X, T3, T4, 32)?;
                    self.a.mov(Width::W, T3, T3);
                    self.a.mov(Width::W, R, T4);
                }
                self.store_reg(rax, size, R)?;
                self.store_reg(rdx, size, T3)?;
                self.defer_operation(FlagKind::Mul { signed }, size, A, Some(B));
            }
            Op::Xchg { a, b } => {
                // The memory operand (if any) is checked for writing first.
                let (mem, reg) = if a.is_mem() { (a, b) } else { (b, a) };
                self.read_destination(mem, size, at, &mut slow, true)?;
                self.read_operand(B, reg, size, at, &mut slow)?;
                self.write_destination(mem, size, B)?;
                self.write_destination(reg, size, A)?;
            }
            Op::Cmpxchg { dst, src } => {
                // Flags of cmp rax, dst; then dst = src if equal, rax = dst
                // otherwise (a memory dst keeps its bytes).
                self.read_destination(dst, size, at, &mut slow, true)?;
                self.load_reg(B, Reg::Gpr(RAX as u8), size)?;
                self.arith(true, size, B, A)?;
                if dst.is_mem() {
                    // x86 writes memory either way (its old value when
                    // they differ): the store first, as it may fault, with
                    // the flags still as they were.
                    self.load_reg(T3, src, size)?;
                    self.a.csel(Select::Csel, w, T3, T3, A, Cond::Eq);
                    self.write_destination(dst, size, T3)?;
                    self.defer(FlagKind::Sub, size, B, A, STATUS_FLAGS);
                    let equal = self.a.new_label();
                    self.a.b_cond(Cond::Eq, equal);
                    self.store_reg(Reg::Gpr(RAX as u8), size, A)?;
                    self.a.bind(equal);
                    self.flags_done(insn, written);
                    return Ok(true);
                }
                self.defer(FlagKind::Sub, size, B, A, STATUS_FLAGS);
                let (differ, done) = (self.a.new_label(), self.a.new_label());
                self.a.b_cond(Cond::Ne, differ);
                self.load_reg(T3, src, size)?;
                self.write_destination(dst, size, T3)?;
                self.a.b(done);
                self.a.bind(differ);
                self.store_reg(Reg::Gpr(RAX as u8), size, A)?;
                self.a.bind(done);
            }
            Op::Xadd { dst, src } => {
                // TEMP = DEST + SRC; SRC = DEST; DEST = TEMP.
                self.read_destination(dst, size, at, &mut slow, true)?;
                self.load_reg(B, src, size)?;
                self.arith(false, size, A, B)?;
                if dst.is_mem() {
                    // The store first (it may fault), then the flags.
                    self.write_destination(dst, size, R)?;
                    self.defer(FlagKind::Add, size, A, B, STATUS_FLAGS);
                    self.store_reg(src, size, A)?;
                } else {
                    self.defer(FlagKind::Add, size, A, B, STATUS_FLAGS);
                    self.store_reg(src, size, A)?;
                    self.write_destination(dst, size, R)?;
                }
            }
            Op::BitScan { reverse, dst, src } => {
                // Only ZF changes; a zero source leaves the destination.
                self.read_operand(B, src, size, at, &mut slow)?;
                let (zero, done) = (self.a.new_label(), self.a.new_label());
                self.a.cbz(false, Width::X, B, zero);
                if reverse {
                    self.a.data1(Data1::Clz, Width::X, R, B);
                    self.a.movz(Width::X, T1, 63, 0)?;
                    self.a.sub(Width::X, R, T1, R, Shift::Lsl, 0)?;
                } else {
                    self.a.data1(Data1::Rbit, Width::X, R, B);
                    self.a.data1(Data1::Clz, Width::X, R, R);
                }
                self.store_reg(dst, size, R)?;
                if !self.skip_flags {
                    self.a
                        .logic_imm(Logic::And, Width::X, FLAGS, FLAGS, !FLAG_ZF)?;
                }
                self.a.b(done);
                self.a.bind(zero);
                if !self.skip_flags {
                    self.a
                        .logic_imm(Logic::Orr, Width::X, FLAGS, FLAGS, FLAG_ZF)?;
                    self.flags_emitted |= FLAG_ZF;
                }
                self.a.bind(done);
            }
            Op::BitTest { op, dst, offset } => {
                // CF = the selected bit; bts/btr/btc also change it.
                let bits = size.bits();
                let writes = op != BitTestOp::Bt;
                self.read_destination(dst, size, at, &mut slow, writes)?;
                match offset {
                    Operand::Imm(imm) => {
                        self.a
                            .movz(Width::X, T3, (imm as u64 % u64::from(bits)) as u16, 0)?
                    }
                    Operand::Reg(reg) => {
                        self.load_reg(T3, reg, size)?;
                        self.a
                            .logic_imm(Logic::And, Width::X, T3, T3, u64::from(bits - 1))?;
                    }
                    Operand::Mem(_) => unreachable!("checked by supported"),
                }
                self.a.shift_reg(Shift::Lsr, Width::X, T1, A, T3);
                if writes {
                    self.a.movz(Width::X, B, 1, 0)?;
                    self.a.shift_reg(Shift::Lsl, Width::X, B, B, T3);
                    let (logic, invert) = match op {
                        BitTestOp::Bts => (Logic::Orr, false),
                        BitTestOp::Btr => (Logic::And, true),
                        _ => (Logic::Eor, false),
                    };
                    self.a
                        .logic(logic, invert, width(size), R, A, B, Shift::Lsl, 0)?;
                    self.write_destination(dst, size, R)?;
                }
                // CF is bit 0 of rflags.
                if !self.skip_flags {
                    self.a.bfm_insert(Width::X, FLAGS, T1, 0, 0)?;
                    self.flags_emitted |= FLAG_CF;
                }
            }
            Op::Bswap(reg) => {
                self.load_reg(A, reg, size)?;
                self.a.data1(Data1::Rev, width(size), R, A);
                self.store_reg(reg, size, R)?;
            }
            Op::Imul { dst, src, factor } if whole_register(Operand::Reg(dst), size).is_some() => {
                // Into the mapped destination; the factors go to `FA` and
                // `FB`, sign-extended, only when the flags are wanted.
                let h = whole_register(Operand::Reg(dst), size).expect("checked by the guard");
                let b = self.operand_in(B, src, size, at, &mut slow)?;
                let a = match factor {
                    Some(imm) => {
                        self.a.mov_imm64(A, imm as u64);
                        A
                    }
                    None => h,
                };
                if self.skip_flags {
                    self.a.madd(w, h, a, b, ZR);
                } else {
                    for (to, from) in [(FA, a), (FB, b)] {
                        if size == Size::B64 {
                            self.a.mov(Width::X, to, from);
                        } else {
                            self.a.bfm(true, Width::X, to, from, 0, 31)?;
                        }
                    }
                    self.a.madd(w, h, FA, FB, ZR);
                    self.flags_emitted |= STATUS_FLAGS;
                    self.lazy = Lazy::Static(Pending::new(
                        FlagKind::Mul { signed: true },
                        size,
                        STATUS_FLAGS,
                    ));
                }
            }
            Op::Imul { dst, src, factor } => {
                // Both factors sign-extended to 64 bits.
                match src {
                    Operand::Reg(reg) => self.read_reg(true, B, reg, size)?,
                    Operand::Mem(m) => {
                        self.address(&m, next, true)?;
                        self.check(Check::Read, bytes(size), at.rip, &mut slow)?;
                        self.guest_load(true, size, B)?;
                    }
                    Operand::Imm(_) => unreachable!("checked by supported"),
                }
                match factor {
                    Some(imm) => self.a.mov_imm64(A, imm as u64),
                    None => self.read_reg(true, A, dst, size)?,
                }
                // The flags (CF = OF: the result does not fit `size`) wait
                // as the factors (`mul_flags`).
                self.a.madd(width(size), R, A, B, ZR);
                self.store_reg(dst, size, R)?;
                self.defer_operation(FlagKind::Mul { signed: true }, size, A, Some(B));
            }
            Op::Cmov { cond, dst, src } if whole_register(Operand::Reg(dst), size).is_some() => {
                // Into the mapped destination: CSEL writes it whatever the
                // condition, zero-extending a 32-bit one as x86 does.
                let h = whole_register(Operand::Reg(dst), size).expect("checked by the guard");
                let b = self.operand_in(B, src, size, at, &mut slow)?;
                let moves = match self.host_condition_now(cond)? {
                    Some(host) => host,
                    None => {
                        let (reg, bit, when_set) = self.condition_bit(cond)?;
                        self.a.logic_imm(Logic::Ands, Width::X, ZR, reg, 1 << bit)?;
                        if when_set { Cond::Ne } else { Cond::Eq }
                    }
                };
                self.a.csel(Select::Csel, w, h, b, h, moves);
            }
            Op::Cmov { cond, dst, src } => {
                // The source is read (and may fault) whatever the
                // condition; a 32-bit destination is zero-extended even
                // when nothing moves.
                self.read_operand(B, src, size, at, &mut slow)?;
                self.load_reg(A, dst, size)?;
                let moves = match self.host_condition_now(cond)? {
                    Some(host) => host,
                    None => {
                        let (reg, bit, when_set) = self.condition_bit(cond)?;
                        self.a.logic_imm(Logic::Ands, Width::X, ZR, reg, 1 << bit)?;
                        if when_set { Cond::Ne } else { Cond::Eq }
                    }
                };
                self.a.csel(Select::Csel, w, R, B, A, moves);
                self.store_reg(dst, size, R)?;
            }
            Op::Setcc { cond, dst } => {
                match self.host_condition_now(cond)? {
                    Some(host) => self.a.cset(Width::X, B, host),
                    None => {
                        let (reg, bit, when_set) = self.condition_bit(cond)?;
                        self.a.shift_imm(Shift::Lsr, Width::X, B, reg, bit)?;
                        self.a.logic_imm(Logic::And, Width::X, B, B, 1)?;
                        if !when_set {
                            self.a.logic_imm(Logic::Eor, Width::X, B, B, 1)?;
                        }
                    }
                }
                self.write_operand(dst, Size::B8, B, at, &mut slow)?;
                // A store's check changes the host NZCV.
                if dst.is_mem() {
                    self.nzcv_valid = false;
                }
            }
            Op::SignExtendAccumulator => {
                // cbw, cwde, cdqe: the lower half of the accumulator.
                let half = match size {
                    Size::B64 => Size::B32,
                    Size::B32 => Size::B16,
                    _ => Size::B8,
                };
                self.read_reg(true, B, Reg::Gpr(RAX as u8), half)?;
                if size == Size::B32 {
                    self.a.mov(Width::W, B, B);
                }
                self.store_reg(Reg::Gpr(RAX as u8), size, B)?;
            }
            Op::SignExtendIntoRdx => {
                // cwd, cdq, cqo: rdx = all ones if the accumulator is
                // negative.
                self.read_reg(true, B, Reg::Gpr(RAX as u8), size)?;
                self.a.shift_imm(Shift::Asr, Width::X, B, B, 63)?;
                if size == Size::B32 {
                    self.a.mov(Width::W, B, B);
                }
                self.store_reg(Reg::Gpr(RDX as u8), size, B)?;
            }
            Op::Sse { op, dst, src } if is_mmx(op, &dst, &src) => {
                self.mmx(op, dst, src, at, &mut slow)?
            }
            Op::Sse { op, dst, src } => match op {
                SseOp::Mov128 { .. } | SseOp::MovToXmm { .. } | SseOp::MovFromXmm { .. } => {
                    self.sse_move(op, dst, src, at, &mut slow)?
                }
                SseOp::FloatArith { .. }
                | SseOp::FloatCompare { .. }
                | SseOp::FloatCmpMask { .. }
                | SseOp::IntToFloat { .. }
                | SseOp::FloatToInt { .. }
                | SseOp::FloatToFloat { .. }
                | SseOp::FloatSignMask { .. } => self.sse_float(op, dst, src, at, &mut slow)?,
                SseOp::PackedArith { .. }
                | SseOp::PackedCmpMask { .. }
                | SseOp::PackedConvert(_) => self.sse_packed(op, dst, src, at, &mut slow)?,
                _ => self.sse_integer(op, dst, src, at, &mut slow)?,
            },
            // Stack and branch instructions: `size` is the stack width,
            // 32 bits in compatibility mode, where rip and esp wrap at
            // 4 GiB.
            Op::Push(src) => {
                let value = self.operand_in(B, src, size, at, &mut slow)?;
                self.push(value, size, at.rip, &mut slow)?;
            }
            Op::Pop(dst) => {
                let Operand::Reg(reg) = dst else {
                    unreachable!("checked by supported")
                };
                let bytes = size.bytes() as u32;
                let rsp = register_host(RSP as u8).expect("rsp is mapped");
                // ADR-0046: one load with writeback (`push`).
                if let (Some(bits), false, Size::B64, Some(h)) = (
                    self.options.host_reads,
                    self.options.concurrent,
                    size,
                    whole_register(dst, size),
                ) && h != rsp
                {
                    let slow = self.slow_path(at.rip, &mut slow);
                    if !self.covered(rsp, 0, 8, 1) {
                        self.a
                            .logic_imm(Logic::Ands, Width::X, ZR, rsp, !((1u64 << bits) - 1))?;
                        self.a.b_cond(Cond::Ne, slow);
                        self.checked.push((rsp, 0, 1));
                    }
                    self.access_slow = Some(slow);
                    self.mark_access();
                    self.a
                        .load_store_indexed(true, MemSize::B64, h, rsp, 8, false)?;
                    self.flags_done(insn, written);
                    return Ok(true);
                }
                self.a.mov(Width::X, ADDR, rsp);
                self.check(Check::Read, bytes, at.rip, &mut slow)?;
                // Straight into a mapped destination, but `pop rsp` loads
                // rsp after the increment.
                let rt = match whole_register(dst, size) {
                    Some(h) if h != rsp => h,
                    _ => B,
                };
                self.guest_load(false, size, rt)?;
                self.a.add_imm(w, rsp, ADDR, bytes)?;
                if rt == B {
                    self.store_reg(reg, size, B)?;
                }
            }
            Op::Jmp { rel } => {
                self.exit_direct(target(next, rel, size))?;
                return Ok(false);
            }
            // Inside the block, the taken side leaves out of line and the
            // block goes on: the flags stay as they are for what follows.
            Op::Jcc { cond, rel } if !self.last => {
                let taken = self.a.new_label();
                self.branch_if(cond, taken)?;
                // Back to the block's start: straight into the loop's copy,
                // rather than through a side exit that only branches there.
                let to = target(next, rel, size);
                match self.loop_head(to) {
                    Some(head) => self.a.retarget(taken, head),
                    None => self.side_exits.push((taken, to, self.lazy, self.normal)),
                }
                return Ok(true);
            }
            Op::Jcc { cond, rel } => {
                let taken = self.a.new_label();
                self.branch_if(cond, taken)?;
                let to = target(next, rel, size);
                if let Some(head) = self.loop_head(to) {
                    // Straight into the loop's copy, as above.
                    self.a.retarget(taken, head);
                    self.exit_direct(next)?;
                    return Ok(false);
                }
                // Each exit publishes the flags from the state at the
                // jump: the first one's code may change `lazy` (it
                // computes or copies them), on its own path only.
                let lazy = self.lazy;
                self.exit_direct(next)?;
                self.a.bind(taken);
                self.lazy = lazy;
                self.exit_direct(to)?;
                return Ok(false);
            }
            Op::Call { rel } => {
                self.a.mov_imm64(B, next);
                self.push(B, size, at.rip, &mut slow)?;
                self.exit_direct(target(next, rel, size))?;
                return Ok(false);
            }
            Op::CallIndirect(src) => {
                // The target is read before the push changes rsp.
                self.read_operand(A, src, size, at, &mut slow)?;
                self.a.mov_imm64(B, next);
                self.push(B, size, at.rip, &mut slow)?;
                self.exit_to(A)?;
                return Ok(false);
            }
            Op::JmpIndirect(src) => {
                self.read_operand(A, src, size, at, &mut slow)?;
                self.exit_to(A)?;
                return Ok(false);
            }
            Op::Ret { pop } => {
                let bytes = size.bytes() as u64;
                let rsp = register_host(RSP as u8).expect("rsp is mapped");
                // ADR-0046: one load with writeback (`push`).
                if let (Some(bits), false, Size::B64) =
                    (self.options.host_reads, self.options.concurrent, size)
                    && 8 + u32::from(pop) <= 255
                {
                    let slow = self.slow_path(at.rip, &mut slow);
                    if !self.covered(rsp, 0, 8, 1) {
                        self.a
                            .logic_imm(Logic::Ands, Width::X, ZR, rsp, !((1u64 << bits) - 1))?;
                        self.a.b_cond(Cond::Ne, slow);
                        self.checked.push((rsp, 0, 1));
                    }
                    self.access_slow = Some(slow);
                    self.mark_access();
                    self.a.load_store_indexed(
                        true,
                        MemSize::B64,
                        A,
                        rsp,
                        8 + i32::from(pop),
                        false,
                    )?;
                    self.exit_to(A)?;
                    return Ok(false);
                }
                self.a.mov(Width::X, ADDR, rsp);
                self.check(Check::Read, bytes as u32, at.rip, &mut slow)?;
                self.guest_load(false, size, A)?;
                self.add_constant(ADDR, bytes + u64::from(pop))?;
                self.store_reg(Reg::Gpr(RSP as u8), size, ADDR)?;
                self.exit_to(A)?;
                return Ok(false);
            }
            _ => unreachable!("checked by supported"),
        }
        self.flags_done(insn, written);
        Ok(true)
    }

    /// After an instruction that fell through: with its flags skipped,
    /// nothing changed; otherwise its code must have written every flag
    /// `flags_written` claims (debug builds), and an instruction that set
    /// them all eagerly leaves `FLAGS` current.
    fn flags_done(&mut self, insn: &Instruction, written: u64) {
        if core::mem::take(&mut self.skip_flags) {
            return;
        }
        debug_assert_eq!(
            written & !self.flags_emitted,
            0,
            "{insn:?} does not write the flags flags_written gives"
        );
        if written == STATUS_FLAGS && !lazy_flags(insn) {
            self.lazy = Lazy::Current;
        }
    }

    /// `R` = `a` ± `b` at `size`, with the host NZCV right for `size`: 8-
    /// and 16-bit operands are moved to the top of a 32-bit register,
    /// where the host's carry and overflow are the x86 ones. `R` ends up
    /// zero-extended from `size`.
    fn arith(&mut self, sub: bool, size: Size, a: HostReg, b: HostReg) -> Emit {
        let w = width(size);
        let op = |s: &mut Self, rd, rn, rm, amount| {
            if sub {
                s.a.subs(w, rd, rn, rm, Shift::Lsl, amount)
            } else {
                s.a.adds(w, rd, rn, rm, Shift::Lsl, amount)
            }
        };
        if matches!(size, Size::B32 | Size::B64) {
            return op(self, R, a, b, 0);
        }
        let top = 32 - size.bits();
        self.a.shift_imm(Shift::Lsl, Width::W, T3, a, top)?;
        op(self, R, T3, b, top)?;
        self.a.shift_imm(Shift::Lsr, Width::W, R, R, top)
    }

    /// Sets the host N and Z from `R` (zero-extended from `size`), C and V
    /// cleared: the flags of a logical operation.
    fn test_result(&mut self, size: Size) -> Emit {
        if matches!(size, Size::B32 | Size::B64) {
            return self
                .a
                .logic(Logic::Ands, false, width(size), ZR, R, R, Shift::Lsl, 0);
        }
        let top = 32 - size.bits();
        self.a.shift_imm(Shift::Lsl, Width::W, T3, R, top)?;
        self.a
            .logic(Logic::Ands, false, Width::W, ZR, T3, T3, Shift::Lsl, 0)
    }

    /// Shifts and rotates of a 32- or 64-bit register, as
    /// `armor_cpu::alu::shift`: nothing at all happens when the masked
    /// count is 0. shl, shr and sar defer their flags (`shift_flags`);
    /// rotates compute CF and OF here.
    fn shift(&mut self, op: ShiftOp, reg: Reg, count: ShiftCount, size: Size) -> Emit {
        let w = width(size);
        let bits = size.bits();
        let mask = if size == Size::B64 { 0x3f } else { 0x1f };
        let done = self.a.new_label();
        let shift = match op {
            ShiftOp::Shl => Shift::Lsl,
            ShiftOp::Shr => Shift::Lsr,
            ShiftOp::Sar => Shift::Asr,
            _ => Shift::Ror,
        };
        let rotate = shift == Shift::Ror;
        let constant = shift_constant(count, size);
        // shl, shr, sar by a constant of a register mapped whole: in place.
        if let (Some(n @ 1..), false, Some(h)) =
            (constant, rotate, whole_register(Operand::Reg(reg), size))
        {
            let from = self.first.take().unwrap_or(h);
            if !self.skip_flags {
                // The value zero-extended from `size`, before it changes.
                self.a.mov(w, FA, from);
            }
            self.a.shift_imm(shift, w, h, from, n)?;
            let kind = FlagKind::Shift(op, Some(n as u8));
            if !self.skip_flags {
                self.flags_emitted |= STATUS_FLAGS;
                self.lazy = Lazy::Static(Pending::new(kind, size, STATUS_FLAGS));
            }
            return Ok(());
        }
        // shl, shr, sar by cl of a register mapped whole: in place too.
        // A count of 0 writes nothing (not even the top half at 32 bits).
        if let (None, false, Some(h)) = (constant, rotate, whole_register(Operand::Reg(reg), size))
        {
            match register_host(RCX as u8) {
                Some(rcx) => self.a.logic_imm(Logic::And, Width::X, T3, rcx, mask)?,
                None => {
                    self.load_reg(T3, Reg::Gpr(RCX as u8), Size::B8)?;
                    self.a.logic_imm(Logic::And, Width::X, T3, T3, mask)?;
                }
            }
            if self.skip_flags {
                if w == Width::W {
                    self.a.cbz(false, Width::X, T3, done);
                }
                self.a.shift_reg(shift, w, h, h, T3);
                self.a.bind(done);
                return Ok(());
            }
            // The value before, for the deferred flags; the flags pending
            // before stay as they are for a count of 0 (`zero_counts`).
            self.a.mov(Width::X, A, h);
            let (zero, back) = (self.a.new_label(), self.a.new_label());
            self.zero_counts.push((zero, back, self.lazy));
            self.a.cbz(false, Width::X, T3, zero);
            self.a.shift_reg(shift, w, h, h, T3);
            self.a.bind(back);
            self.defer_operation(FlagKind::Shift(op, None), size, A, Some(T3));
            return Ok(());
        }
        self.load_reg(A, reg, size)?;
        match constant {
            Some(0) => return Ok(()),
            Some(n) if !rotate => {
                self.a.shift_imm(shift, w, R, A, n)?;
                self.store_reg(reg, size, R)?;
                let kind = FlagKind::Shift(op, Some(n as u8));
                self.defer_operation(kind, size, A, None);
                return Ok(());
            }
            Some(n) => {
                let n = if op == ShiftOp::Rol { bits - n } else { n };
                self.a.shift_imm(Shift::Ror, w, R, A, n)?;
            }
            None => {
                self.load_reg(T3, Reg::Gpr(RCX as u8), Size::B8)?;
                self.a.logic_imm(Logic::And, Width::X, T3, T3, mask)?;
                if !rotate && !self.skip_flags {
                    // A count of 0 writes nothing, and the flags stay as
                    // they were: out of line they go into `FLAGS`, where
                    // the deferred flags of a count of 0 leave them.
                    let (zero, back) = (self.a.new_label(), self.a.new_label());
                    self.zero_counts.push((zero, back, self.lazy));
                    self.a.cbz(false, Width::X, T3, zero);
                    self.a.shift_reg(shift, w, R, A, T3);
                    self.store_reg(reg, size, R)?;
                    self.a.bind(back);
                    self.defer_operation(FlagKind::Shift(op, None), size, A, Some(T3));
                    return Ok(());
                }
                self.a.cbz(false, Width::X, T3, done);
                if op == ShiftOp::Rol {
                    self.a.movz(Width::X, T4, bits as u16, 0)?;
                    self.a.sub(Width::X, T4, T4, T3, Shift::Lsl, 0)?;
                    self.a.shift_reg(Shift::Ror, w, R, A, T4);
                } else {
                    self.a.shift_reg(shift, w, R, A, T3);
                }
            }
        }
        if !rotate || self.skip_flags {
            // Dead flags (`live_flags`): the result only.
            self.store_reg(reg, size, R)?;
            self.a.bind(done);
            return Ok(());
        }
        // T1 = CF | OF << 11. T2 = the sign bit of the result.
        self.a.shift_imm(Shift::Lsr, w, T2, R, bits - 1)?;
        if op == ShiftOp::Rol {
            // CF = bit 0; OF = sign ^ CF.
            self.a.logic_imm(Logic::And, Width::X, T1, R, 1)?;
            self.a
                .logic(Logic::Eor, false, Width::X, T2, T2, T1, Shift::Lsl, 0)?;
        } else {
            // CF = sign; OF = sign ^ the bit below it.
            self.a.mov(Width::X, T1, T2);
            self.a.shift_imm(Shift::Lsr, w, T3, R, bits - 2)?;
            self.a
                .logic(Logic::Eor, false, Width::X, T2, T2, T3, Shift::Lsl, 0)?;
            self.a.logic_imm(Logic::And, Width::X, T2, T2, 1)?;
        }
        self.or_into_t1(T2, 11)?;
        self.merge_flags(FLAG_CF | FLAG_OF)?;
        self.store_reg(reg, size, R)?;
        self.a.bind(done);
        Ok(())
    }

    /// Shifts and rotates at 8 and 16 bits, as `armor_cpu::alu::shift`.
    /// The count (masked to 5 bits) may exceed the size: the value is
    /// zero-extended in a 64-bit register, where the interpreter's results
    /// come out without special cases (a count of 9 shifts everything out,
    /// and the bit that falls into CF is 0).
    fn narrow_shift(
        &mut self,
        op: ShiftOp,
        dst: Operand,
        count: ShiftCount,
        size: Size,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        let bits = size.bits();
        let mask = size.mask();
        let constant = match count {
            ShiftCount::One => Some(1),
            ShiftCount::Imm(n) => Some(u32::from(n) & 0x1f),
            ShiftCount::Cl => None,
        };
        // The interpreter reads the destination even for a count of 0.
        self.read_destination(dst, size, at, slow, constant != Some(0))?;
        if constant == Some(0) {
            return Ok(());
        }
        let done = self.a.new_label();
        // T3 = count, T4 = count - 1 (shifts) or count mod size (rotates).
        match constant {
            Some(n) => self.a.movz(Width::X, T3, n as u16, 0)?,
            None => {
                self.load_reg(T3, Reg::Gpr(RCX as u8), Size::B8)?;
                self.a.logic_imm(Logic::And, Width::X, T3, T3, 0x1f)?;
                self.a.cbz(false, Width::X, T3, done);
            }
        }
        let rotate = matches!(op, ShiftOp::Rol | ShiftOp::Ror);
        if rotate {
            self.a
                .logic_imm(Logic::And, Width::X, T4, T3, u64::from(bits - 1))?;
            self.a.movz(Width::X, B, bits as u16, 0)?;
            self.a.sub(Width::X, B, B, T4, Shift::Lsl, 0)?;
        } else {
            self.a.sub_imm(Width::X, T4, T3, 1)?;
        }
        match op {
            ShiftOp::Shl => {
                self.a.shift_reg(Shift::Lsl, Width::X, R, A, T3);
                // CF: the bit shifted to position `bits`.
                self.a.shift_imm(Shift::Lsr, Width::X, T1, R, bits)?;
            }
            ShiftOp::Shr => {
                self.a.shift_reg(Shift::Lsr, Width::X, R, A, T3);
                self.a.shift_reg(Shift::Lsr, Width::X, T1, A, T4);
            }
            ShiftOp::Sar => {
                self.a.bfm(true, Width::X, B, A, 0, bits - 1)?;
                self.a.shift_reg(Shift::Asr, Width::X, R, B, T3);
                self.a.shift_reg(Shift::Asr, Width::X, T1, B, T4);
            }
            ShiftOp::Rol => {
                self.a.shift_reg(Shift::Lsl, Width::X, R, A, T4);
                self.a.shift_reg(Shift::Lsr, Width::X, T1, A, B);
                self.a
                    .logic(Logic::Orr, false, Width::X, R, R, T1, Shift::Lsl, 0)?;
            }
            _ => {
                self.a.shift_reg(Shift::Lsr, Width::X, R, A, T4);
                self.a.shift_reg(Shift::Lsl, Width::X, T1, A, B);
                self.a
                    .logic(Logic::Orr, false, Width::X, R, R, T1, Shift::Lsl, 0)?;
            }
        }
        self.a.logic_imm(Logic::And, Width::X, R, R, mask)?;
        // To memory, the store before the flags: it may fault (host
        // protection), and the slow path wants them as they were.
        if self.skip_flags || dst.is_mem() {
            self.write_destination(dst, size, R)?;
        }
        if self.skip_flags {
            // Dead flags (`live_flags`): the result only.
            self.a.bind(done);
            return Ok(());
        }
        // T2 = the sign bit of the result.
        self.a.shift_imm(Shift::Lsr, Width::X, T2, R, bits - 1)?;
        if rotate {
            if op == ShiftOp::Rol {
                // CF = bit 0; OF = sign ^ CF.
                self.a.logic_imm(Logic::And, Width::X, T1, R, 1)?;
                self.a
                    .logic(Logic::Eor, false, Width::X, T2, T2, T1, Shift::Lsl, 0)?;
            } else {
                // CF = sign; OF = sign ^ the bit below it.
                self.a.mov(Width::X, T1, T2);
                self.a.shift_imm(Shift::Lsr, Width::X, T3, R, bits - 2)?;
                self.a
                    .logic(Logic::Eor, false, Width::X, T2, T2, T3, Shift::Lsl, 0)?;
                self.a.logic_imm(Logic::And, Width::X, T2, T2, 1)?;
            }
            self.a
                .logic(Logic::Orr, false, Width::X, T1, T1, T2, Shift::Lsl, 11)?;
            self.merge_flags(FLAG_CF | FLAG_OF)?;
        } else {
            self.a.logic_imm(Logic::And, Width::X, T1, T1, 1)?;
            match op {
                // OF = sign(result) ^ CF.
                ShiftOp::Shl => {
                    self.a
                        .logic(Logic::Eor, false, Width::X, T2, T2, T1, Shift::Lsl, 0)?
                }
                // OF = sign(value).
                ShiftOp::Shr => self.a.shift_imm(Shift::Lsr, Width::X, T2, A, bits - 1)?,
                // OF = 0.
                _ => self.a.mov(Width::X, T2, ZR),
            }
            self.a
                .logic(Logic::Orr, false, Width::X, T1, T1, T2, Shift::Lsl, 11)?;
            self.result_flags(size, STATUS_FLAGS)?;
        }
        if !dst.is_mem() {
            self.write_destination(dst, size, R)?;
        }
        self.a.bind(done);
        Ok(())
    }

    /// div/idiv at 32 and 64 bits: rdx:rax / src. When the dividend fits
    /// in 64 bits (rdx is zero, or the sign of rax, at 64 bits; always at
    /// 32 bits), one hardware division. Division by zero, a quotient that
    /// does not fit, and wider dividends go to the interpreter (slow path),
    /// which raises #DE or computes it. Flags are left unchanged, as by
    /// the interpreter (they are undefined).
    fn divide(
        &mut self,
        signed: bool,
        src: Operand,
        size: Size,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        let slow = {
            let rip = at.rip;
            self.slow_path(rip, slow)
        };
        // The divisor, extended to 64 bits as its signedness says: a
        // mapped 64-bit register in place, else `B`.
        let b = match src {
            Operand::Reg(_) if size == Size::B64 && whole_register(src, size).is_some() => {
                whole_register(src, size).expect("checked by the guard")
            }
            Operand::Reg(reg) => {
                self.read_reg(signed, B, reg, size)?;
                B
            }
            Operand::Mem(m) => {
                self.address(&m, at.next, true)?;
                let mut slow = Some(slow);
                self.check(Check::Read, bytes(size), at.rip, &mut slow)?;
                self.guest_load(signed, size, B)?;
                B
            }
            Operand::Imm(_) => unreachable!("checked by supported"),
        };
        self.a.cbz(false, Width::X, b, slow);
        let (rax, rdx) = (Reg::Gpr(RAX as u8), Reg::Gpr(RDX as u8));
        if size == Size::B64 {
            // rdx:rax where they are mapped, else in `A` and `T1`.
            let (a, d) = match (register_host(RAX as u8), register_host(RDX as u8)) {
                (Some(a), Some(d)) => (a, d),
                _ => {
                    self.load_reg(A, rax, Size::B64)?;
                    self.load_reg(T1, rdx, Size::B64)?;
                    (A, T1)
                }
            };
            if signed {
                // rdx must be the sign of rax; MIN / -1 overflows.
                self.a.shift_imm(Shift::Asr, Width::X, T2, a, 63)?;
                self.a.subs(Width::X, ZR, d, T2, Shift::Lsl, 0)?;
                self.a.b_cond(Cond::Ne, slow);
                let fine = self.a.new_label();
                self.a.adds_imm(Width::X, ZR, b, 1)?;
                self.a.b_cond(Cond::Ne, fine);
                self.a.mov_imm64(T2, 1 << 63);
                self.a.subs(Width::X, ZR, a, T2, Shift::Lsl, 0)?;
                self.a.b_cond(Cond::Eq, slow);
                self.a.bind(fine);
            } else {
                self.a.cbz(true, Width::X, d, slow);
            }
            self.a.div(signed, Width::X, R, a, b);
            if a != A {
                // The remainder straight into rdx (its sources are read
                // first), then the quotient into rax.
                self.a.msub(Width::X, d, R, b, a);
                self.a.mov(Width::X, a, R);
                return Ok(());
            }
        } else {
            // The 64-bit dividend edx:eax.
            self.load_reg(A, rax, Size::B32)?;
            self.load_reg(T1, rdx, Size::B32)?;
            self.a
                .logic(Logic::Orr, false, Width::X, A, A, T1, Shift::Lsl, 32)?;
            self.a.div(signed, Width::X, R, A, b);
            // The quotient must fit in 32 bits.
            if signed {
                self.a.bfm(true, Width::X, T2, R, 0, 31)?;
                self.a.subs(Width::X, ZR, T2, R, Shift::Lsl, 0)?;
                self.a.b_cond(Cond::Ne, slow);
            } else {
                self.a.shift_imm(Shift::Lsr, Width::X, T2, R, 32)?;
                self.a.cbz(true, Width::X, T2, slow);
            }
        }
        // Remainder = dividend - quotient * divisor.
        self.a.msub(Width::X, T1, R, b, A);
        if size == Size::B32 {
            self.a.mov(Width::W, R, R);
            self.a.mov(Width::W, T1, T1);
        }
        self.store_reg(rax, size, R)?;
        self.store_reg(rdx, size, T1)
    }

    /// movaps/movups/movdqa/movdqu, movd/movq to and from xmm: the vector
    /// registers themselves, a 128-bit load or store for memory.
    fn sse_move(
        &mut self,
        op: SseOp,
        dst: Operand,
        src: Operand,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        match op {
            SseOp::Mov128 { aligned } => match (dst, src) {
                (Operand::Mem(m), _) => {
                    let s = xreg(&src);
                    self.address(&m, at.next, true)?;
                    self.check_xmm_access(Check::Write, aligned, at.rip, slow)?;
                    self.mark_access();
                    self.a.str_q(s, ADDR, 0)
                }
                (_, Operand::Mem(m)) => {
                    let d = xreg(&dst);
                    self.address(&m, at.next, true)?;
                    self.check_xmm_access(Check::Read, aligned, at.rip, slow)?;
                    self.mark_access();
                    self.a.ldr_q(d, ADDR, 0)
                }
                _ => {
                    let (d, s) = (xreg(&dst), xreg(&src));
                    if d != s {
                        self.a.mov_vector(d, s);
                    }
                    Ok(())
                }
            },
            SseOp::MovToXmm { bytes } => {
                // The rest of the register is zeroed (FMOV from a general
                // register zeroes it).
                let size = if bytes == 8 { Size::B64 } else { Size::B32 };
                match xmm(&src) {
                    Some(_) => self.a.fmov_to_gpr(bytes == 8, B, xreg(&src)),
                    None => self.read_operand(B, src, size, at, slow)?,
                }
                self.a.fmov_from_gpr(bytes == 8, xreg(&dst), B);
                Ok(())
            }
            SseOp::MovFromXmm { bytes } => {
                let size = if bytes == 8 { Size::B64 } else { Size::B32 };
                self.a.fmov_to_gpr(bytes == 8, B, xreg(&src));
                match xmm(&dst) {
                    // movq xmm, xmm (66 0F D6) zeroes the upper half.
                    Some(_) => {
                        self.a.fmov_from_gpr(true, xreg(&dst), B);
                        Ok(())
                    }
                    None => self.write_operand(dst, size, B, at, slow),
                }
            }
            _ => unreachable!("checked by supported"),
        }
    }

    /// An MMX instruction (`mmx_supported`), as the interpreter runs it
    /// (`armor_cpu`'s `sse.rs`): the moves directly; the lane operations
    /// as their SSE translation on the two scratch registers (`v3`, `v4`
    /// here; the interpreter's are in `Cpu::mm_scratch`, which translated
    /// code leaves zero), which hold the zero-extended mm operands, the low
    /// 64 bits of the result going back to the destination. Writing mm`n`
    /// sets the sign and exponent of its x87 register to ones; any mm
    /// operand sets TOP to 0 and every register valid; emms makes them all
    /// empty.
    fn mmx(
        &mut self,
        op: SseOp,
        dst: Operand,
        src: Operand,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        const SCRATCH_A: Operand = Operand::Reg(Reg::Mm(8));
        const SCRATCH_B: Operand = Operand::Reg(Reg::Mm(9));
        match op {
            SseOp::Emms => {
                self.a.mov_imm64(T1, 0xff);
                return self.a.str(MemSize::B8, T1, CPU, field(mmx_layout::EMPTY));
            }
            SseOp::Pinsrw(index) => {
                let d = mm(&dst).expect("checked by supported");
                self.read_operand(T2, src, Size::B16, at, slow)?;
                let word = field(mmx_layout::mantissa(d)) + 2 * u32::from(index);
                self.a.str(MemSize::B16, T2, CPU, word)?;
                self.mark_mm(d)?;
            }
            SseOp::MovToXmm { bytes } | SseOp::MovFromXmm { bytes } => {
                let size = if bytes == 8 { Size::B64 } else { Size::B32 };
                // The value, zero-extended to 64 bits, in T2.
                match (mm(&src), xmm(&src)) {
                    (Some(n), _) => {
                        self.load(false, size, T2, CPU, field(mmx_layout::mantissa(n)))?
                    }
                    // movdq2q: the low half.
                    (None, Some(_)) => self.a.fmov_to_gpr(true, T2, xreg(&src)),
                    _ => self.read_operand(T2, src, size, at, slow)?,
                }
                match (mm(&dst), xmm(&dst)) {
                    (Some(n), _) => self.write_mm(n, T2)?,
                    // movq2dq: the upper half is zeroed.
                    (None, Some(_)) => self.a.fmov_from_gpr(true, xreg(&dst), T2),
                    _ => self.write_operand(dst, size, T2, at, slow)?,
                }
            }
            _ => {
                let d = mm(&dst).expect("checked by supported");
                self.a
                    .ldr(MemSize::B64, T2, CPU, field(mmx_layout::mantissa(d)))?;
                self.a.fmov_from_gpr(true, xreg(&SCRATCH_A), T2);
                let source = match src {
                    Operand::Imm(_) => src,
                    _ => {
                        match mm(&src) {
                            Some(n) => {
                                self.a
                                    .ldr(MemSize::B64, T2, CPU, field(mmx_layout::mantissa(n)))?
                            }
                            None => self.read_operand(T2, src, Size::B64, at, slow)?,
                        }
                        self.a.fmov_from_gpr(true, xreg(&SCRATCH_B), T2);
                        SCRATCH_B
                    }
                };
                self.sse_integer(op, SCRATCH_A, source, at, slow)?;
                self.a.fmov_to_gpr(true, T2, xreg(&SCRATCH_A));
                self.write_mm(d, T2)?;
            }
        }
        if mm(&dst).is_some() || mm(&src).is_some() {
            // TOP = 0 and every register valid.
            self.a.str(MemSize::B8, ZR, CPU, field(mmx_layout::TOP))?;
            self.a.str(MemSize::B8, ZR, CPU, field(mmx_layout::EMPTY))?;
        }
        Ok(())
    }

    /// mm`n` = `value` (64 bits), its sign and exponent set to ones.
    fn write_mm(&mut self, n: u8, value: HostReg) -> Emit {
        self.a
            .str(MemSize::B64, value, CPU, field(mmx_layout::mantissa(n)))?;
        self.mark_mm(n)
    }

    /// mm`n` was written: the sign and exponent of its x87 register become
    /// ones.
    fn mark_mm(&mut self, n: u8) -> Emit {
        self.a.mov_imm64(T1, 0x7fff);
        self.a
            .str(MemSize::B16, T1, CPU, field(mmx_layout::exponent(n)))?;
        self.a.mov_imm64(T1, 1);
        self.a.str(MemSize::B8, T1, CPU, field(mmx_layout::sign(n)))
    }

    /// The SSE2 integer operations, shuffles and half or scalar moves, as
    /// `armor_cpu`'s `sse.rs`, on the vector registers: lane-wise
    /// operations straight into the destination; shuffles through `V2`,
    /// so that a source that is also the destination is read whole first.
    fn sse_integer(
        &mut self,
        op: SseOp,
        dst: Operand,
        src: Operand,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        match op {
            SseOp::Lane { op, lane } => {
                let op = match op {
                    LaneOp::Add => Vec3::Add,
                    LaneOp::Sub => Vec3::Sub,
                    LaneOp::CmpEq => Vec3::Cmeq,
                    LaneOp::CmpGt => Vec3::Cmgt,
                    LaneOp::MinU => Vec3::Umin,
                    LaneOp::MaxU => Vec3::Umax,
                    LaneOp::MinS => Vec3::Smin,
                    LaneOp::MaxS => Vec3::Smax,
                    _ => unreachable!("checked by supported"),
                };
                self.vector_op(op, lanes(lane), false, dst, src, at, slow)
            }
            SseOp::Logic(op) => {
                let (op, swap) = match op {
                    LogicOp::And => (Vec3::And, false),
                    // !dst & src = src BIC dst.
                    LogicOp::AndNot => (Vec3::Bic, true),
                    LogicOp::Or => (Vec3::Orr, false),
                    LogicOp::Xor => (Vec3::Eor, false),
                };
                self.vector_op(op, Lanes::B, swap, dst, src, at, slow)
            }
            SseOp::Unpack { high, lane } => {
                let op = if high { Vec3::Zip2 } else { Vec3::Zip1 };
                self.vector_op(op, lanes(lane), false, dst, src, at, slow)
            }
            // x86 takes byte src[i] & 15 of dst, or 0 when bit 7 of src[i]
            // is set; TBL gives 0 for an index of 16 or more, so the index
            // keeps bit 7 and the low four bits.
            SseOp::Int(IntOp::Pshufb) => {
                let d = xreg(&dst);
                let s = self.vector_source(V1, src, at, slow)?;
                self.a.movi_bytes(V2, 0x8f);
                self.a.vec3(Vec3::And, Lanes::B, V1, s, V2)?;
                self.a.tbl(d, d, V1);
                Ok(())
            }
            // dst:src shifted right by `count` bytes, the low 16 kept: EXT
            // of src and dst; from 16 on, dst and zeros; from 32, zero.
            SseOp::Int(IntOp::Palignr(count)) => {
                let d = xreg(&dst);
                let count = u32::from(count);
                let s = self.vector_source(V1, src, at, slow)?;
                match count {
                    0..16 => self.a.ext(d, s, d, count),
                    16..32 => {
                        self.a.movi_zero(V2);
                        self.a.ext(d, d, V2, count - 16)
                    }
                    _ => {
                        self.a.movi_zero(d);
                        Ok(())
                    }
                }
            }
            SseOp::Pack { from, signed } => {
                let d = xreg(&dst);
                let s = self.vector_source(V1, src, at, slow)?;
                // dst's lanes narrowed into the low half, src's into the
                // high half.
                self.a.narrow(!signed, false, lanes(from), V2, d)?;
                self.a.narrow(!signed, true, lanes(from), V2, s)?;
                self.a.mov_vector(d, V2);
                Ok(())
            }
            SseOp::ShiftImm { kind, lane, count } => {
                let d = xreg(&dst);
                let bits = lane.bits();
                let count = u32::from(count);
                let l = lanes(lane);
                match kind {
                    ShiftKind::Left if count < bits => self.a.vshift(VShift::Shl, l, d, d, count),
                    ShiftKind::RightLogical if count == 0 => Ok(()),
                    ShiftKind::RightLogical if count < bits => {
                        self.a.vshift(VShift::Ushr, l, d, d, count)
                    }
                    ShiftKind::RightArithmetic => {
                        // Counts above the lane size fill with the sign.
                        let count = count.min(bits - 1);
                        if count > 0 {
                            self.a.vshift(VShift::Sshr, l, d, d, count)?;
                        }
                        Ok(())
                    }
                    _ => {
                        self.a.movi_zero(d);
                        Ok(())
                    }
                }
            }
            SseOp::ByteShift { left, count } => {
                let d = xreg(&dst);
                let count = u32::from(count);
                if count == 0 {
                    return Ok(());
                }
                if count > 15 {
                    self.a.movi_zero(d);
                    return Ok(());
                }
                self.a.movi_zero(V2);
                if left {
                    self.a.ext(d, V2, d, 16 - count)
                } else {
                    self.a.ext(d, d, V2, count)
                }
            }
            SseOp::Pmovmskb => {
                // The sign bit of each byte, gathered with a multiply: bit
                // 8i + 7 times 2^(7 (7 - i)) lands on bit 56 + i, and no
                // two partial products share a bit.
                let Operand::Reg(reg) = dst else {
                    unreachable!("checked by supported")
                };
                self.xmm_halves(xreg(&src), A, B)?;
                self.a.mov_imm64(T3, 0x0002_0408_1020_4081);
                for half in [A, B] {
                    self.a
                        .logic_imm(Logic::And, Width::X, half, half, 0x8080_8080_8080_8080)?;
                    self.a.madd(Width::X, half, half, T3, ZR);
                    self.a.shift_imm(Shift::Lsr, Width::X, half, half, 56)?;
                }
                self.a
                    .logic(Logic::Orr, false, Width::X, R, A, B, Shift::Lsl, 8)?;
                self.store_reg(reg, Size::B32, R)
            }
            SseOp::Pshufd(order) => {
                let d = xreg(&dst);
                let s = self.vector_source(V1, src, at, slow)?;
                for i in 0..4 {
                    let pick = u32::from((order >> (2 * i)) & 3);
                    self.a.ins_element(Lanes::S, V2, i, s, pick)?;
                }
                self.a.mov_vector(d, V2);
                Ok(())
            }
            SseOp::PshufWords { high, order } => {
                // The shuffled half from src's words; the other half is
                // src's too.
                let d = xreg(&dst);
                let s = self.vector_source(V1, src, at, slow)?;
                let half = if high { 4 } else { 0 };
                self.a.mov_vector(V2, s);
                for i in 0..4 {
                    let pick = u32::from((order >> (2 * i)) & 3);
                    self.a.ins_element(Lanes::H, V2, half + i, s, half + pick)?;
                }
                self.a.mov_vector(d, V2);
                Ok(())
            }
            SseOp::Pextrw(index) => {
                let Operand::Reg(reg) = dst else {
                    unreachable!("checked by supported")
                };
                self.a.umov(Lanes::H, B, xreg(&src), u32::from(index))?;
                self.store_reg(reg, Size::B32, B)
            }
            SseOp::Pinsrw(index) => {
                self.read_operand(B, src, Size::B16, at, slow)?;
                self.a.ins_gpr(Lanes::H, xreg(&dst), u32::from(index), B)
            }
            SseOp::Shuffle { double, order } => {
                // The low half from dst's lanes, the high half from src's.
                let d = xreg(&dst);
                let s = self.vector_source(V1, src, at, slow)?;
                if double {
                    let pick = |i: u8| u32::from((order >> i) & 1);
                    self.a.ins_element(Lanes::D, V2, 0, d, pick(0))?;
                    self.a.ins_element(Lanes::D, V2, 1, s, pick(1))?;
                } else {
                    let pick = |i: u8| u32::from((order >> (2 * i)) & 3);
                    self.a.ins_element(Lanes::S, V2, 0, d, pick(0))?;
                    self.a.ins_element(Lanes::S, V2, 1, d, pick(1))?;
                    self.a.ins_element(Lanes::S, V2, 2, s, pick(2))?;
                    self.a.ins_element(Lanes::S, V2, 3, s, pick(3))?;
                }
                self.a.mov_vector(d, V2);
                Ok(())
            }
            SseOp::Pmuludq => {
                // The even dword lanes, multiplied into 64 bits.
                let d = xreg(&dst);
                let s = self.vector_source(V1, src, at, slow)?;
                self.a.umov(Lanes::S, A, d, 0)?;
                self.a.umov(Lanes::S, B, s, 0)?;
                self.a.umov(Lanes::S, T3, d, 2)?;
                self.a.umov(Lanes::S, T4, s, 2)?;
                self.a.madd(Width::X, A, A, B, ZR);
                self.a.madd(Width::X, T3, T3, T4, ZR);
                self.a.fmov_from_gpr(true, d, A);
                self.a.ins_gpr(Lanes::D, d, 1, T3)
            }
            SseOp::MovHalf { high } => {
                let half = u32::from(high);
                match (xmm(&dst), xmm(&src)) {
                    (Some(_), None) => {
                        self.read_operand(B, src, Size::B64, at, slow)?;
                        self.a.ins_gpr(Lanes::D, xreg(&dst), half, B)
                    }
                    (None, Some(_)) => {
                        self.a.umov(Lanes::D, B, xreg(&src), half)?;
                        self.write_operand(dst, Size::B64, B, at, slow)
                    }
                    _ => unreachable!("checked by supported"),
                }
            }
            SseOp::MovHlps | SseOp::MovLhps => {
                let (from, to) = if op == SseOp::MovHlps { (1, 0) } else { (0, 1) };
                self.a
                    .ins_element(Lanes::D, xreg(&dst), to, xreg(&src), from)
            }
            SseOp::MovScalar { double } => {
                let lanes = if double { Lanes::D } else { Lanes::S };
                let bytes = if double { 8 } else { 4 };
                match (dst, src) {
                    // Register to register: only the low lane changes.
                    (Operand::Reg(_), Operand::Reg(_)) => {
                        self.a.ins_element(lanes, xreg(&dst), 0, xreg(&src), 0)
                    }
                    // Load: the rest of the register is zeroed.
                    (Operand::Reg(_), Operand::Mem(m)) => {
                        self.address(&m, at.next, true)?;
                        self.check(Check::Read, bytes, at.rip, slow)?;
                        self.mark_access();
                        self.a.load_store_fp(true, double, xreg(&dst), ADDR, 0)
                    }
                    (Operand::Mem(m), Operand::Reg(_)) => {
                        self.address(&m, at.next, true)?;
                        self.check(Check::Write, bytes, at.rip, slow)?;
                        self.mark_access();
                        self.a.load_store_fp(false, double, xreg(&src), ADDR, 0)
                    }
                    _ => unreachable!("checked by supported"),
                }
            }
            _ => unreachable!("checked by supported"),
        }
    }

    /// The two 64-bit halves of vector `v` into general registers.
    fn xmm_halves(&mut self, v: VReg, low: HostReg, high: HostReg) -> Emit {
        self.a.fmov_to_gpr(true, low, v);
        self.a.umov(Lanes::D, high, v, 1)
    }

    /// Scalar SSE floating point, as `armor_cpu`'s `float.rs`. The host's
    /// arithmetic is the same IEEE 754 with rounding to nearest even (the
    /// only MXCSR mode armor accepts, ADR-0015); what differs is NaNs,
    /// which x86 propagates and creates differently, and out-of-range
    /// conversions to integers. Those cases take the slow path (the
    /// interpreter applies the x86 rules). min/max and the comparisons
    /// need no slow path: their x86 results follow from host conditions.
    /// The operands are the low lanes of the vector registers; a result
    /// goes into the destination's low lane with INS, which keeps the
    /// other lanes, as x86 does.
    ///
    /// MXCSR's exception flags come from the host's FPSR, which the engine
    /// folds into MXCSR after each run (ADR-0024). Where the host's flags
    /// differ from x86's:
    /// - a denormal operand goes to the interpreter before the operation:
    ///   x86 raises DE, the host nothing (conversions to integers raise no
    ///   DE on x86 either);
    /// - underflow: the host detects tininess before rounding, x86 after,
    ///   so a multiplication, division or `cvtsd2ss` whose result rounds
    ///   up to ±the smallest normal number raises UFC on the host and no
    ///   UE on x86 (`underflow`).
    ///
    /// The rest needs no correction: a NaN result goes to the interpreter
    /// with the host's IOC raised exactly when x86 raises IE (an SNaN
    /// operand or an invalid operation), which the interpreter raises
    /// again; an out-of-range conversion to an integer raises IOC alone on
    /// the host and IE alone on x86 (✅ measured on the host); comparisons
    /// that signal on quiet NaNs (`comis`, min/max, the ordered `cmp`
    /// predicates) use FCMPE, the others FCMP: the same IOC as x86's IE.
    fn sse_float(
        &mut self,
        op: SseOp,
        dst: Operand,
        src: Operand,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        match op {
            SseOp::FloatArith { op, double } => {
                let d = xreg(&dst);
                let b = self.scalar_source(V1, double, src, at, slow)?;
                if op != FloatOp::Sqrt {
                    self.slow_if_denormal(double, d, at.rip, slow)?;
                }
                self.slow_if_denormal(double, b, at.rip, slow)?;
                match op {
                    FloatOp::Min | FloatOp::Max => {
                        // x86: a < b ? a : b (a > b for max): b when
                        // unordered or equal. IE for any NaN.
                        self.a.fcmpe(double, d, b);
                        let cond = if op == FloatOp::Min {
                            Cond::Mi
                        } else {
                            Cond::Gt
                        };
                        self.a.fcsel(double, V2, d, b, cond);
                    }
                    _ => {
                        match op {
                            FloatOp::Add => self.a.fp_arith(FpOp::Add, double, V2, d, b),
                            FloatOp::Sub => self.a.fp_arith(FpOp::Sub, double, V2, d, b),
                            FloatOp::Mul => self.a.fp_arith(FpOp::Mul, double, V2, d, b),
                            FloatOp::Div => self.a.fp_arith(FpOp::Div, double, V2, d, b),
                            FloatOp::Sqrt => self.a.fsqrt(double, V2, b),
                            _ => unreachable!("checked by supported"),
                        }
                        self.slow_if_nan(double, V2, at.rip, slow);
                        if matches!(op, FloatOp::Mul | FloatOp::Div) {
                            self.underflow(double, V2, at.rip, slow)?;
                        }
                    }
                }
                self.scalar_result(double, d)
            }
            SseOp::FloatCompare { double, signaling } => {
                // ZF, PF, CF: unordered 1 1 1, less 0 0 1, greater 0 0 0,
                // equal 1 0 0; OF, SF, AF cleared.
                let d = xreg(&dst);
                let b = self.scalar_source(V1, double, src, at, slow)?;
                self.slow_if_denormal(double, d, at.rip, slow)?;
                self.slow_if_denormal(double, b, at.rip, slow)?;
                self.compare(signaling, double, d, b);
                if self.skip_flags {
                    // Dead flags: the comparison only, for MXCSR's IE.
                    return Ok(());
                }
                // Deferred as the operands' bits (`FlagKind::FloatCompare`);
                // the host NZCV is the comparison's for a condition reader
                // right after.
                self.a.fmov_to_gpr(double, FA, d);
                self.a.fmov_to_gpr(double, FB, b);
                self.flags_emitted |= STATUS_FLAGS;
                self.lazy = Lazy::Static(Pending::new(
                    FlagKind::FloatCompare { double },
                    Size::B64,
                    STATUS_FLAGS,
                ));
                self.nzcv_valid = true;
                Ok(())
            }
            SseOp::FloatCmpMask { double, predicate } => {
                // All ones in the low lane when the predicate holds; lt,
                // le, nlt and nle signal on quiet NaNs.
                let d = xreg(&dst);
                let b = self.scalar_source(V1, double, src, at, slow)?;
                self.slow_if_denormal(double, d, at.rip, slow)?;
                self.slow_if_denormal(double, b, at.rip, slow)?;
                self.compare(matches!(predicate, 1 | 2 | 5 | 6), double, d, b);
                let cond = match predicate {
                    0 => Cond::Eq,
                    1 => Cond::Mi,
                    2 => Cond::Ls,
                    3 => Cond::Vs,
                    4 => Cond::Ne,
                    5 => Cond::Pl,
                    6 => Cond::Hi,
                    _ => Cond::Vc,
                };
                let (w, lanes) = if double {
                    (Width::X, Lanes::D)
                } else {
                    (Width::W, Lanes::S)
                };
                self.a.csel(Select::Csinv, w, B, ZR, ZR, cond.invert());
                self.a.ins_gpr(lanes, d, 0, B)
            }
            SseOp::IntToFloat { double, int_bytes } => {
                let size = if int_bytes == 8 { Size::B64 } else { Size::B32 };
                match src {
                    Operand::Reg(reg) => self.read_reg(true, B, reg, size)?,
                    Operand::Mem(m) => {
                        self.address(&m, at.next, true)?;
                        self.check(Check::Read, bytes(size), at.rip, slow)?;
                        self.guest_load(true, size, B)?;
                    }
                    Operand::Imm(_) => unreachable!("checked by supported"),
                }
                // Sign-extended to 64 bits: the same value.
                self.a.scvtf(Width::X, double, V2, B);
                self.scalar_result(double, xreg(&dst))
            }
            SseOp::FloatToInt {
                double,
                int_bytes,
                truncate,
            } => {
                let Operand::Reg(reg) = dst else {
                    unreachable!("checked by supported")
                };
                let b = self.scalar_source(V1, double, src, at, slow)?;
                self.slow_if_nan(double, b, at.rip, slow);
                let (w, size, max) = if int_bytes == 8 {
                    (Width::X, Size::B64, i64::MAX as u64)
                } else {
                    (Width::W, Size::B32, i32::MAX as u64)
                };
                // No DE check: x86 raises none for a conversion to an
                // integer.
                self.a.fcvt_to_int(truncate, w, double, B, b);
                // The host saturates where x86 gives the integer
                // indefinite: the extremes go to the interpreter.
                let slow_label = self.slow_path(at.rip, slow);
                self.a.mov_imm64(T1, max);
                self.a.subs(w, ZR, B, T1, Shift::Lsl, 0)?;
                self.a.b_cond(Cond::Eq, slow_label);
                self.a
                    .logic(Logic::Eor, true, w, T1, ZR, T1, Shift::Lsl, 0)?;
                self.a.subs(w, ZR, B, T1, Shift::Lsl, 0)?;
                self.a.b_cond(Cond::Eq, slow_label);
                self.store_reg(reg, size, B)
            }
            SseOp::FloatToFloat { to_double } => {
                let a = self.scalar_source(V1, !to_double, src, at, slow)?;
                self.slow_if_nan(!to_double, a, at.rip, slow);
                self.slow_if_denormal(!to_double, a, at.rip, slow)?;
                self.a.fcvt(to_double, V2, a);
                if !to_double {
                    self.underflow(false, V2, at.rip, slow)?;
                }
                // Exact to a double: no flag.
                self.scalar_result(to_double, xreg(&dst))
            }
            SseOp::FloatSignMask { double } => {
                // The sign bit of each lane, into the low bits.
                let Operand::Reg(reg) = dst else {
                    unreachable!("checked by supported")
                };
                self.xmm_halves(xreg(&src), A, B)?;
                if double {
                    self.a.shift_imm(Shift::Lsr, Width::X, R, A, 63)?;
                    self.a.shift_imm(Shift::Lsr, Width::X, T1, B, 63)?;
                    self.a
                        .logic(Logic::Orr, false, Width::X, R, R, T1, Shift::Lsl, 1)?;
                } else {
                    self.a.mov(Width::X, R, ZR);
                    for (i, (half, bit)) in
                        [(A, 31), (A, 63), (B, 31), (B, 63)].into_iter().enumerate()
                    {
                        self.a.shift_imm(Shift::Lsr, Width::X, T1, half, bit)?;
                        self.a.logic_imm(Logic::And, Width::X, T1, T1, 1)?;
                        self.a.logic(
                            Logic::Orr,
                            false,
                            Width::X,
                            R,
                            R,
                            T1,
                            Shift::Lsl,
                            i as u32,
                        )?;
                    }
                }
                self.store_reg(reg, Size::B32, R)
            }
            _ => unreachable!("checked by supported"),
        }
    }

    /// Packed floating point (ps and pd), as `armor_cpu`'s `float.rs` does
    /// lane by lane, with the scalar path's rules (`sse_float`) for every
    /// lane: a denormal operand or a NaN result in any lane goes to the
    /// interpreter, as does an underflow (UFC, whose tininess differs from
    /// x86's: the host's flag is cleared and the interpreter decides UE).
    /// min/max and the compares need no NaN path: their x86 results are
    /// host compares and selects (FCMGT and FCMGE signal on any NaN, like
    /// x86's lt, le, min and max; FCMEQ is quiet, like eq, unord, ord).
    /// The result goes through `V2`, so that the destination changes only
    /// when no slow path is taken.
    fn sse_packed(
        &mut self,
        op: SseOp,
        dst: Operand,
        src: Operand,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        let d = xreg(&dst);
        match op {
            SseOp::PackedArith { op, double } => {
                let b = self.vector_source(V1, src, at, slow)?;
                let both = [d, b];
                let operands: &[VReg] = if op == FloatOp::Sqrt {
                    &both[1..]
                } else {
                    &both
                };
                self.slow_if_denormal_lanes(double, operands, at.rip, slow)?;
                match op {
                    // x86: dst < src ? dst : src (dst > src for max), src
                    // when unordered or equal.
                    FloatOp::Min | FloatOp::Max => {
                        let (n, m) = if op == FloatOp::Min { (b, d) } else { (d, b) };
                        self.a.vec_fp(VecFp::Cmgt, double, V2, n, m);
                        self.a.bsl(V2, d, b);
                    }
                    _ => {
                        match op {
                            FloatOp::Add => self.a.vec_fp(VecFp::Add, double, V2, d, b),
                            FloatOp::Sub => self.a.vec_fp(VecFp::Sub, double, V2, d, b),
                            FloatOp::Mul => self.a.vec_fp(VecFp::Mul, double, V2, d, b),
                            FloatOp::Div => self.a.vec_fp(VecFp::Div, double, V2, d, b),
                            FloatOp::Sqrt => self.a.vec_unary(VecUnary::Fsqrt { double }, V2, b),
                            _ => unreachable!("checked by supported"),
                        }
                        // UFC first: the NaN's slow path must not find it
                        // in FPSR (the interpreter decides UE).
                        if matches!(op, FloatOp::Mul | FloatOp::Div) {
                            self.slow_if_underflow(at.rip, slow);
                        }
                        self.slow_if_nan_lanes(double, V2, at.rip, slow);
                    }
                }
            }
            SseOp::PackedCmpMask { double, predicate } => {
                let b = self.vector_source(V1, src, at, slow)?;
                self.slow_if_denormal_lanes(double, &[d, b], at.rip, slow)?;
                match predicate & 3 {
                    0 => self.a.vec_fp(VecFp::Cmeq, double, V2, d, b),
                    1 => self.a.vec_fp(VecFp::Cmgt, double, V2, b, d),
                    2 => self.a.vec_fp(VecFp::Cmge, double, V2, b, d),
                    // Ordered: both equal to themselves.
                    _ => {
                        self.a.vec_fp(VecFp::Cmeq, double, V3, d, d);
                        self.a.vec_fp(VecFp::Cmeq, double, V2, b, b);
                        self.a.vec3(Vec3::And, Lanes::B, V2, V2, V3)?;
                    }
                }
                // 4 to 7 are the negations of 0 to 3, and 3 (unordered)
                // is the negation of ordered.
                if (predicate >= 4) != (predicate & 3 == 3) {
                    self.a.vec_unary(VecUnary::Not, V2, V2);
                }
            }
            SseOp::PackedConvert(conversion) => match conversion {
                Conversion::Dq2Ps => {
                    let b = self.vector_source(V1, src, at, slow)?;
                    self.a.vec_unary(VecUnary::Scvtf { double: false }, V2, b);
                }
                Conversion::Ps2Dq { truncate } => {
                    let b = self.vector_source(V1, src, at, slow)?;
                    self.slow_if_nan_operand(false, b, at.rip, slow)?;
                    let convert = if truncate {
                        VecUnary::Fcvtzs { double: false }
                    } else {
                        VecUnary::Fcvtns { double: false }
                    };
                    self.a.vec_unary(convert, V2, b);
                    // The host saturates a large value to 0x7fffffff where
                    // x86 gives 0x80000000 (as it does, like the host, for
                    // large negative values): to the interpreter.
                    let slow_label = self.slow_path(at.rip, slow);
                    self.a.mov_imm64(T1, 0x7fff_ffff);
                    self.a.dup(false, V3, T1);
                    self.a.vec3(Vec3::Cmeq, Lanes::S, V3, V2, V3)?;
                    self.a.vec_unary(VecUnary::Umaxv, V3, V3);
                    self.a.fmov_to_gpr(false, T1, V3);
                    self.a.cbz(true, Width::W, T1, slow_label);
                }
                Conversion::Dq2Pd => {
                    let b = self.low_half_source(src, at, slow)?;
                    self.a.vec_unary(VecUnary::Sxtl, V2, b);
                    self.a.vec_unary(VecUnary::Scvtf { double: true }, V2, V2);
                }
                Conversion::Ps2Pd => {
                    let b = self.low_half_source(src, at, slow)?;
                    self.slow_if_denormal_lanes(false, &[b], at.rip, slow)?;
                    self.slow_if_nan_operand(false, b, at.rip, slow)?;
                    self.a.vec_unary(VecUnary::Fcvtl, V2, b);
                }
                Conversion::Pd2Ps => {
                    let b = self.vector_source(V1, src, at, slow)?;
                    self.slow_if_denormal_lanes(true, &[b], at.rip, slow)?;
                    self.slow_if_nan_operand(true, b, at.rip, slow)?;
                    self.a.vec_unary(VecUnary::Fcvtn, V2, b);
                    self.slow_if_underflow(at.rip, slow);
                }
                Conversion::Pd2Dq { .. } => unreachable!("checked by supported"),
            },
            _ => unreachable!("checked by supported"),
        }
        self.a.mov_vector(d, V2);
        Ok(())
    }

    /// The low 64 bits of an xmm register, or 8 bytes of memory (no
    /// alignment) loaded into `V1` with the rest zeroed.
    fn low_half_source(&mut self, src: Operand, at: Site, slow: &mut Option<Label>) -> Emit<VReg> {
        match src {
            Operand::Mem(m) => {
                self.address(&m, at.next, true)?;
                self.check(Check::Read, 8, at.rip, slow)?;
                self.mark_access();
                self.a.load_store_fp(true, true, V1, ADDR, 0)?;
                Ok(V1)
            }
            _ => Ok(xreg(&src)),
        }
    }

    /// To the slow path if any lane of the registers in `regs` (singles,
    /// or `double`) is denormal: x86 raises DE. A lane is denormal when
    /// its bits without the sign, minus 1, are below twice the smallest
    /// normal number minus 1 (unsigned): exponent 0 and a fraction that is
    /// not 0. Uses `T1`, `V3`–`V6`.
    fn slow_if_denormal_lanes(
        &mut self,
        double: bool,
        regs: &[VReg],
        rip: u64,
        slow: &mut Option<Label>,
    ) -> Emit {
        let slow = self.slow_path(rip, slow);
        let (l, limit) = if double {
            (Lanes::D, (2u64 << 52) - 1)
        } else {
            (Lanes::S, (2u64 << 23) - 1)
        };
        self.a.mov_imm64(T1, 1);
        self.a.dup(double, V5, T1);
        self.a.mov_imm64(T1, limit);
        self.a.dup(double, V6, T1);
        for (i, &r) in regs.iter().enumerate() {
            let t = if i == 0 { V4 } else { V3 };
            self.a.vshift(VShift::Shl, l, t, r, 1)?;
            self.a.vec3(Vec3::Sub, l, t, t, V5)?;
            self.a.cmhi(double, t, V6, t);
            if i > 0 {
                self.a.vec3(Vec3::Orr, Lanes::B, V4, V4, V3)?;
            }
        }
        self.a.vec_unary(VecUnary::Umaxv, V4, V4);
        self.a.fmov_to_gpr(false, T1, V4);
        self.a.cbz(true, Width::W, T1, slow);
        Ok(())
    }

    /// To the slow path if any lane of the operand `v` is a NaN, with
    /// integer compares: a floating-point compare would raise IOC for a
    /// signaling NaN before the interpreter runs the instruction. A lane
    /// is a NaN when its bits without the sign are above those of
    /// infinity. Uses `T1`, `V3`, `V5`.
    fn slow_if_nan_operand(
        &mut self,
        double: bool,
        v: VReg,
        rip: u64,
        slow: &mut Option<Label>,
    ) -> Emit {
        let slow = self.slow_path(rip, slow);
        let (l, infinity) = if double {
            (Lanes::D, 0x7ff0_0000_0000_0000u64 << 1)
        } else {
            (Lanes::S, 0x7f80_0000u64 << 1)
        };
        self.a.mov_imm64(T1, infinity);
        self.a.dup(double, V5, T1);
        self.a.vshift(VShift::Shl, l, V3, v, 1)?;
        self.a.cmhi(double, V3, V3, V5);
        self.a.vec_unary(VecUnary::Umaxv, V3, V3);
        self.a.fmov_to_gpr(false, T1, V3);
        self.a.cbz(true, Width::W, T1, slow);
        Ok(())
    }

    /// To the slow path if any lane of `v` is a NaN. Uses `T1`, `V3`.
    fn slow_if_nan_lanes(&mut self, double: bool, v: VReg, rip: u64, slow: &mut Option<Label>) {
        let slow = self.slow_path(rip, slow);
        // All ones where the lane equals itself (FCMEQ is quiet).
        self.a.vec_fp(VecFp::Cmeq, double, V3, v, v);
        self.a.vec_unary(VecUnary::Uminv, V3, V3);
        self.a.fmov_to_gpr(false, T1, V3);
        self.a.cbz(false, Width::W, T1, slow);
    }

    /// After a packed multiplication, division or `cvtpd2ps`: if the host
    /// raised UFC, out of line it is cleared and the interpreter runs the
    /// instruction (x86 detects tininess after rounding, the host before;
    /// translated code never leaves UFC in FPSR, `underflow`). Uses `T1`.
    fn slow_if_underflow(&mut self, rip: u64, slow: &mut Option<Label>) {
        const UFC: u32 = 3;
        let slow = self.slow_path(rip, slow);
        let entry = self.a.new_label();
        self.a.read_fpsr(T1);
        self.a.tbz(true, T1, UFC, entry);
        self.packed_underflows.push((entry, slow));
    }

    /// The scalar result in `V2` into the low lane of `d`, keeping the
    /// other lanes.
    fn scalar_result(&mut self, double: bool, d: VReg) -> Emit {
        let lanes = if double { Lanes::D } else { Lanes::S };
        self.a.ins_element(lanes, d, 0, V2, 0)
    }

    /// A scalar source operand: the low lane of an xmm register, or memory
    /// without an alignment requirement, loaded into `vt`.
    fn scalar_source(
        &mut self,
        vt: VReg,
        double: bool,
        src: Operand,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit<VReg> {
        match src {
            Operand::Mem(m) => {
                self.address(&m, at.next, true)?;
                self.check(Check::Read, if double { 8 } else { 4 }, at.rip, slow)?;
                self.mark_access();
                self.a.load_store_fp(true, double, vt, ADDR, 0)?;
                Ok(vt)
            }
            _ => Ok(xreg(&src)),
        }
    }

    /// FCMPE (`signaling`: IOC also for quiet NaNs) or FCMP.
    fn compare(&mut self, signaling: bool, double: bool, vn: VReg, vm: VReg) {
        if signaling {
            self.a.fcmpe(double, vn, vm);
        } else {
            self.a.fcmp(double, vn, vm);
        }
    }

    /// To the slow path if the low lane of `v` is a denormal single or
    /// double: x86 raises DE, the host nothing. The bits come from the
    /// vector register (guest memory is not read twice, as another thread
    /// may write it in between). Uses `T1`.
    fn slow_if_denormal(
        &mut self,
        double: bool,
        v: VReg,
        rip: u64,
        slow: &mut Option<Label>,
    ) -> Emit {
        // Known not denormal; after the test, it is.
        let bit = normal_bit(v);
        let width = usize::from(!double);
        if self.normal[width] & bit != 0 {
            return Ok(());
        }
        self.normal[width] |= bit;
        let slow = self.slow_path(rip, slow);
        let entry = self.a.new_label();
        let back = self.a.new_label();
        let (w, exponent) = if double {
            (Width::X, 0x7ff0_0000_0000_0000)
        } else {
            (Width::W, 0x7f80_0000)
        };
        self.a.fmov_to_gpr(double, T1, v);
        self.a.logic_imm(Logic::Ands, w, ZR, T1, exponent)?;
        // Exponent 0, out of line: denormal unless the fraction is 0 too
        // (shifting the sign out).
        self.a.b_cond(Cond::Eq, entry);
        self.a.bind(back);
        self.denormals.push((entry, back, slow, w));
        Ok(())
    }

    /// After a host operation that may underflow (a multiplication, a
    /// division, `cvtsd2ss`) with result `v`: out of line if its exponent
    /// is 0 or 1 (one test of the exponent's top bits), which includes
    /// every `|v|` at most the smallest normal number, the only results
    /// for which the host can raise UFC. There (`underflow_fixup`) one
    /// above it comes back at once; a denormal result
    /// goes to the interpreter (so that a result that comes back is never
    /// denormal, `update_normal`); for 0 or ±the smallest normal number
    /// UFC, if raised, is cleared, and becomes UE in MXCSR unless `v` is
    /// ±the smallest normal number: x86 detects tininess after rounding,
    /// and that result is not tiny for it. Translated code thus never
    /// leaves UFC in FPSR, so a UFC seen there was raised by the operation
    /// just done. Uses `T1`, `T2` and the host's flags.
    fn underflow(&mut self, double: bool, v: VReg, rip: u64, slow: &mut Option<Label>) -> Emit {
        let slow = self.slow_path(rip, slow);
        let (w, top_of_exponent) = if double {
            (Width::X, 0x7fe0_0000_0000_0000)
        } else {
            (Width::W, 0x7f00_0000)
        };
        let entry = self.a.new_label();
        let back = self.a.new_label();
        self.a.fmov_to_gpr(double, T1, v);
        self.a.logic_imm(Logic::Ands, w, ZR, T1, top_of_exponent)?;
        self.a.b_cond(Cond::Eq, entry);
        self.a.bind(back);
        self.underflows.push((entry, back, slow, double));
        Ok(())
    }

    /// The out-of-line part of `underflow`; `T1` holds `v`'s bits.
    fn underflow_fixup(&mut self, entry: Label, back: Label, slow: Label, double: bool) -> Emit {
        const UFC: u32 = 3;
        const UE: u64 = 1 << 4;
        let (w, smallest) = if double {
            (Width::X, 1u64 << 52)
        } else {
            (Width::W, 1 << 23)
        };
        let not_denormal = self.a.new_label();
        self.a.bind(entry);
        // `|v|` (the sign shifted out) compared with the smallest normal
        // number: above it, nothing to do.
        self.a.shift_imm(Shift::Lsl, w, T1, T1, 1)?;
        self.a.mov_imm64(T2, smallest << 1);
        self.a.subs(w, ZR, T1, T2, Shift::Lsl, 0)?;
        self.a.b_cond(Cond::Hi, back);
        // ±the smallest normal number, or 0: comes back. (CBZ keeps NZCV.)
        self.a.b_cond(Cond::Eq, not_denormal);
        self.a.cbz(false, Width::X, T1, not_denormal);
        // Denormal: the interpreter, without the host's UFC.
        self.a.read_fpsr(T2);
        self.a
            .logic_imm(Logic::And, Width::X, T2, T2, !(1u64 << UFC))?;
        self.a.write_fpsr(T2);
        self.a.b(slow);
        self.a.bind(not_denormal);
        self.a.read_fpsr(T2);
        self.a.tbz(false, T2, UFC, back);
        self.a
            .logic_imm(Logic::And, Width::X, T2, T2, !(1u64 << UFC))?;
        self.a.write_fpsr(T2);
        // ±the smallest normal number: not tiny on x86.
        self.a.b_cond(Cond::Eq, back);
        let mxcsr = field(offset_of!(Cpu, mxcsr));
        self.a.ldr(MemSize::B32, T1, CPU, mxcsr)?;
        self.a.logic_imm(Logic::Orr, Width::W, T1, T1, UE)?;
        self.a.str(MemSize::B32, T1, CPU, mxcsr)?;
        self.a.b(back);
        Ok(())
    }

    /// After `insn`: what is known of the xmm registers' low lanes
    /// (`normal`). A result is known not denormal when it cannot be one
    /// (an integer converted, a square root, min or max of tested
    /// operands, a multiplication, division or `cvtsd2ss` that passed
    /// `underflow`, a comparison's mask, a register zeroed with xor), or
    /// copied from one known; anything else an instruction writes is not.
    /// The operands `slow_if_denormal` tested are known from then on.
    fn update_normal(&mut self, insn: &Instruction) {
        let Op::Sse { op, dst, src } = insn.op else {
            if matches!(insn.op, Op::State { .. }) {
                self.normal = [0; 2];
            }
            return;
        };
        let bit = |o: &Operand| match o {
            Operand::Reg(Reg::Xmm(n)) => 1u32 << (16 + u32::from(*n)),
            _ => 0,
        };
        let (d, s) = (bit(&dst), bit(&src));
        let [double_known, single_known] = &mut self.normal;
        // The width a result is known at (`Some(double)`), the other not.
        let result = |known: Option<bool>, dk: &mut u32, sk: &mut u32| {
            *dk &= !d;
            *sk &= !d;
            match known {
                Some(true) => *dk |= d,
                Some(false) => *sk |= d,
                None => {}
            }
        };
        match op {
            SseOp::FloatArith { op, double } => {
                let known = !matches!(op, FloatOp::Add | FloatOp::Sub);
                result(known.then_some(double), double_known, single_known);
            }
            SseOp::FloatCompare { .. } | SseOp::FloatToInt { .. } => {}
            SseOp::FloatCmpMask { double, .. } | SseOp::IntToFloat { double, .. } => {
                result(Some(double), double_known, single_known)
            }
            SseOp::FloatToFloat { to_double } => {
                result(Some(to_double), double_known, single_known)
            }
            SseOp::Mov128 { .. } if d != 0 && s != 0 => {
                let (dk, sk) = (*double_known & s != 0, *single_known & s != 0);
                result(None, double_known, single_known);
                if dk {
                    *double_known |= d;
                }
                if sk {
                    *single_known |= d;
                }
            }
            SseOp::MovScalar { double } if d != 0 && s != 0 => {
                // The low lane from src: as a double too when it is one.
                let (dk, sk) = (*double_known & s != 0, *single_known & s != 0);
                result(None, double_known, single_known);
                if double && dk {
                    *double_known |= d;
                }
                if sk {
                    *single_known |= d;
                }
            }
            SseOp::Logic(LogicOp::Xor) if d != 0 && d == s => {
                *double_known |= d;
                *single_known |= d;
            }
            // pcmpestrm and pcmpistrm write xmm0.
            SseOp::PcmpStr { .. } => self.normal = [0; 2],
            _ => result(None, double_known, single_known),
        }
    }

    /// To the slow path if `v` is a NaN.
    fn slow_if_nan(&mut self, double: bool, v: VReg, rip: u64, slow: &mut Option<Label>) {
        let slow = self.slow_path(rip, slow);
        self.a.fcmp(double, v, v);
        self.a.b_cond(Cond::Vs, slow);
    }

    /// dst = dst op src (`swap`: src op dst) on 128-bit vectors, straight
    /// into the destination's register.
    #[allow(clippy::too_many_arguments)]
    fn vector_op(
        &mut self,
        op: Vec3,
        lanes: Lanes,
        swap: bool,
        dst: Operand,
        src: Operand,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        let d = xreg(&dst);
        let s = self.vector_source(V1, src, at, slow)?;
        let (n, m) = if swap { (s, d) } else { (d, s) };
        self.a.vec3(op, lanes, d, n, m)
    }

    /// A 128-bit source operand: an xmm register's vector register, or
    /// 16-byte aligned memory (checked) loaded into `vt`.
    fn vector_source(
        &mut self,
        vt: VReg,
        src: Operand,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit<VReg> {
        match src {
            Operand::Mem(m) => {
                self.address(&m, at.next, true)?;
                self.check_xmm_access(Check::Read, true, at.rip, slow)?;
                self.mark_access();
                self.a.ldr_q(vt, ADDR, 0)?;
                Ok(vt)
            }
            _ => Ok(xreg(&src)),
        }
    }

    /// The checks of a 16-byte access at `ADDR`: alignment when the
    /// instruction requires it (#GP otherwise, from the interpreter), then
    /// the page.
    fn check_xmm_access(
        &mut self,
        check: Check,
        aligned: bool,
        rip: u64,
        slow: &mut Option<Label>,
    ) -> Emit {
        if aligned && self.options.host_reads.is_some() {
            return self.check_aligned(check, 16, 16, rip, slow);
        }
        if aligned {
            let slow = self.slow_path(rip, slow);
            self.a.logic_imm(Logic::Ands, Width::X, ZR, ADDR, 15)?;
            self.a.b_cond(Cond::Ne, slow);
        }
        self.check(check, 16, rip, slow)
    }

    /// Leaves the block for `rip` with `status`.
    fn exit(&mut self, rip: u64, status: u64) -> Emit {
        self.publish_lazy()?;
        self.a.mov_imm64(EXIT_RIP, rip);
        let entry = self.status_entry(status);
        self.a.b(entry);
        Ok(())
    }

    /// Leaves the block for a known address: chainable if enabled.
    fn exit_direct(&mut self, rip: u64) -> Emit {
        // Back to the block's start with the flags known: into a copy of
        // the block translated from this state, which then loops in itself
        // (the budget checked at its head). One copy per block; a state
        // that does not imply the copy's leaves as any exit.
        if let Some(head) = self.loop_head(rip) {
            self.a.b(head);
            return Ok(());
        }
        // Before the `b`: once patched, it goes straight to the next block.
        self.publish_lazy()?;
        if !self.options.chain {
            return self.exit(rip, EXIT_CONTINUE);
        }
        let (site, stub) = (self.a.new_label(), self.a.new_label());
        self.a.bind(site);
        self.a.b(stub);
        self.chain_stubs.push((site, stub, rip));
        Ok(())
    }

    /// The head of the block's copy for a jump to `rip` from the current
    /// state, when `rip` is the block's start and the state implies the
    /// copy's (the copy registered if there is none yet): `exit_direct`.
    fn loop_head(&mut self, rip: u64) -> Option<Label> {
        if Some(rip) != self.loop_start || !matches!(self.lazy, Lazy::Static(_) | Lazy::Current) {
            return None;
        }
        let (lazy, normal) = (self.lazy, self.normal);
        let fits =
            |l: &Lazy, n: &[u32; 2]| *l == lazy && n[0] & !normal[0] == 0 && n[1] & !normal[1] == 0;
        if let Some(&(_, _, head, _)) = self.loops.iter().find(|(l, n, _, _)| fits(l, n)) {
            return Some(head);
        }
        if !self.loops.is_empty() {
            return None;
        }
        let head = self.a.new_label();
        self.loops.push((lazy, normal, head, true));
        Some(head)
    }

    /// Leaves the block for the address in `target` (looked up in the
    /// indirect-jump table first, when chaining).
    fn exit_to(&mut self, target: HostReg) -> Emit {
        self.publish_lazy()?;
        if self.options.chain {
            // The indirect-jump table: on a hit, straight to the next
            // block's chained entry, which checks the budget.
            let miss = self.a.new_label();
            self.a.logic(
                Logic::Eor,
                false,
                Width::X,
                T1,
                target,
                target,
                Shift::Lsr,
                12,
            )?;
            self.a
                .logic_imm(Logic::And, Width::X, T1, T1, TARGET_ENTRIES as u64 - 1)?;
            self.a.ldr(
                MemSize::B64,
                T2,
                CONTEXT,
                field(offset_of!(Context, targets)),
            )?;
            self.a.add(Width::X, T2, T2, T1, Shift::Lsl, 4)?;
            self.a.pair(true, PairMode::Offset, T3, T4, T2, 0)?;
            self.a.subs(Width::X, ZR, T3, target, Shift::Lsl, 0)?;
            self.a.b_cond(Cond::Ne, miss);
            self.a.br(T4);
            self.a.bind(miss);
        }
        self.a.mov(Width::X, EXIT_RIP, target);
        let entry = self.status_entry(EXIT_CONTINUE);
        self.a.b(entry);
        Ok(())
    }

    /// The slow path of the instruction at `rip`, created on first use.
    fn slow_path(&mut self, rip: u64, slow: &mut Option<Label>) -> Label {
        *slow.get_or_insert_with(|| {
            let label = self.a.new_label();
            self.slow_paths.push((label, rip, self.lazy));
            label
        })
    }

    /// Before a guest load at `ADDR`: if its read was left to the host
    /// (`check`), records where it is, for the fault handler.
    fn mark_access(&mut self) -> Option<Label> {
        let slow = self.access_slow?;
        self.faults.push((self.a.len(), slow));
        Some(slow)
    }

    /// Loads `size` bytes of guest memory at `ADDR` (checked) into `rt`,
    /// zero- or sign-extended to 64 bits. In concurrent mode a load-acquire
    /// (LDAPR, ADR-0023), out of line with a barrier if `ADDR` is not
    /// aligned to the size.
    fn guest_load(&mut self, signed: bool, size: Size, rt: HostReg) -> Emit {
        if !self.options.concurrent {
            self.mark_access();
            return self.load(signed, size, rt, ADDR, 0);
        }
        let fault = self.access_slow;
        let access = Unaligned::Load {
            signed,
            size,
            rt,
            fault,
        };
        let back = match self.misaligned.take() {
            Some((entry, _)) => Some(self.unaligned_at(entry, access)),
            None if core::mem::take(&mut self.known_aligned) => None,
            None => self.unaligned_test(size, access),
        };
        self.mark_access();
        if self.options.features.rcpc {
            self.a.ldapr(mem_size(size), rt, ADDR);
        } else {
            self.a.ldar(mem_size(size), rt, ADDR);
        }
        if signed && size != Size::B64 {
            // No sign-extending LDAPR without FEAT_LRCPC2.
            self.a.bfm(true, Width::X, rt, rt, 0, size.bits() - 1)?;
        }
        if let Some(back) = back {
            self.a.bind(back);
        }
        Ok(())
    }

    /// Stores the low `size` bytes of `rt` to guest memory at `ADDR`
    /// (checked). In concurrent mode a store-release (STLR, ADR-0023), out
    /// of line with a barrier if `ADDR` is not aligned to the size.
    fn guest_store(&mut self, size: Size, rt: HostReg) -> Emit {
        if !self.options.concurrent {
            self.mark_access();
            return self.a.str(mem_size(size), rt, ADDR, 0);
        }
        let fault = self.access_slow;
        let access = Unaligned::Store { size, rt, fault };
        let back = match self.misaligned.take() {
            Some((entry, _)) => Some(self.unaligned_at(entry, access)),
            None if core::mem::take(&mut self.known_aligned) => None,
            None => self.unaligned_test(size, access),
        };
        self.mark_access();
        self.a.stlr(mem_size(size), rt, ADDR);
        if let Some(back) = back {
            self.a.bind(back);
        }
        Ok(())
    }

    /// Branches to an out-of-line `access` if `ADDR` is not aligned to
    /// `size`, with `tbnz` (no flags, no register). Returns where the
    /// out-of-line code comes back, `None` for bytes (always aligned).
    fn unaligned_test(&mut self, size: Size, access: Unaligned) -> Option<Label> {
        let low_bits = size.bytes().trailing_zeros();
        if low_bits == 0 {
            return None;
        }
        let (entry, back) = (self.a.new_label(), self.a.new_label());
        for bit in 0..low_bits {
            self.a.tbz(true, ADDR, bit, entry);
        }
        self.unaligned.push((entry, back, access));
        Some(back)
    }

    /// Whether an access of `bytes` at `base + disp`, aligned to `align`
    /// as tested, needs no region test: near an address that passed, with
    /// the same base (`checked`). Below that address the access is in the
    /// region or faults (nothing is mapped below the region, and an address
    /// that wrapped is not canonical); above it, within `REGION_GUARD`.
    fn covered(&self, base: HostReg, disp: u64, bytes: u32, align: u32) -> bool {
        self.checked.iter().any(|&(b, d, tested)| {
            let delta = disp.wrapping_sub(d) as i64;
            b == base
                && delta.unsigned_abs() + u64::from(bytes) <= REGION_GUARD
                && (align == 1 || (tested >= align && delta.rem_euclid(i64::from(align)) == 0))
        })
    }

    /// A plain access of `bytes` at `mem` that can go straight to the
    /// host's `[base, #offset]`, without computing the address: host
    /// protection, not concurrent mode, a mapped base plus a constant that
    /// the instruction encodes (scaled by the size), and covered by an
    /// earlier check (`covered`). The slow path is recorded for the fault.
    fn direct(
        &mut self,
        mem: &Mem,
        bytes: u32,
        check: Check,
        rip: u64,
        slow: &mut Option<Label>,
    ) -> Option<(HostReg, u32)> {
        if self.options.host_reads.is_none() || self.options.concurrent || mem.addr32 {
            return None;
        }
        let (Base::Gpr(n), Segment::Default) = (mem.base, mem.segment) else {
            return None;
        };
        let base = register_host(n)?;
        let disp = mem.disp as u64;
        let n = u64::from(bytes);
        if mem.disp < 0 || !disp.is_multiple_of(n) || disp / n >= 4096 {
            return None;
        }
        if let Some(index) = mem.index {
            // A read at base + index + a constant within the guard's
            // distance: base + index tested, the constant in the load.
            let (bits, index) = (self.options.host_reads?, register_host(index)?);
            if check != Check::Read || disp == 0 || disp + n > REGION_GUARD {
                return None;
            }
            self.flush_misaligned();
            let slow = self.slow_path(rip, slow);
            let shift = u32::from(mem.scale).trailing_zeros();
            self.a
                .add(Width::X, ADDR, base, index, Shift::Lsl, shift)
                .ok()?;
            self.a
                .logic_imm(Logic::Ands, Width::X, ZR, ADDR, !((1u64 << bits) - 1))
                .ok()?;
            self.a.b_cond(Cond::Ne, slow);
            self.access_slow = Some(slow);
            self.mark_access();
            return Some((ADDR, disp as u32));
        }
        let align = if bytes > 1 && check == Check::Write {
            bytes.next_power_of_two()
        } else {
            1
        };
        if !self.covered(base, disp, bytes, align) {
            // A read at a constant within the guard's distance: the base's
            // own test covers it (below the region's end, base + constant
            // is in the region or in the guard), and the accesses near the
            // base after it.
            let bits = self.options.host_reads?;
            if check != Check::Read || disp + n > REGION_GUARD {
                return None;
            }
            self.flush_misaligned();
            let slow = self.slow_path(rip, slow);
            self.a
                .logic_imm(Logic::Ands, Width::X, ZR, base, !((1u64 << bits) - 1))
                .ok()?;
            self.a.b_cond(Cond::Ne, slow);
            self.checked.push((base, 0, 1));
            self.access_slow = Some(slow);
            self.mark_access();
            return Some((base, disp as u32));
        }
        self.flush_misaligned();
        self.access_slow = Some(self.slow_path(rip, slow));
        self.mark_access();
        Some((base, disp as u32))
    }

    /// The out-of-line `access` for an unaligned address that `check`
    /// already sends to `entry`; returns where it comes back.
    fn unaligned_at(&mut self, entry: Label, access: Unaligned) -> Label {
        let back = self.a.new_label();
        self.unaligned.push((entry, back, access));
        back
    }

    /// An unaligned case of `check` that no ordered access took goes back
    /// to right after the check (the access made there allows it).
    fn flush_misaligned(&mut self) {
        if let Some(stub) = self.misaligned.take() {
            self.misaligned_stubs.push(stub);
        }
        self.known_aligned = false;
    }

    /// The out-of-line part of an unaligned ordered access: a plain access
    /// and a barrier on the side that gives acquire (after a load) or
    /// release (before a store).
    fn unaligned_access(&mut self, entry: Label, back: Label, access: Unaligned) -> Emit {
        self.a.bind(entry);
        match access {
            Unaligned::Load {
                signed,
                size,
                rt,
                fault,
            } => {
                if let Some(slow) = fault {
                    self.faults.push((self.a.len(), slow));
                }
                self.load(signed, size, rt, ADDR, 0)?;
                self.a.dmb(Barrier::IshLd);
            }
            Unaligned::Store { size, rt, fault } => {
                if let Some(slow) = fault {
                    // The store after the barrier.
                    self.faults.push((self.a.len() + 1, slow));
                }
                self.a.dmb(Barrier::Ish);
                self.a.str(mem_size(size), rt, ADDR, 0)?;
            }
        }
        self.a.b(back);
        Ok(())
    }

    /// Loads `size` bytes at `[base + offset]` into `rt`, zero- or
    /// sign-extended to 64 bits.
    fn load(&mut self, signed: bool, size: Size, rt: HostReg, base: HostReg, offset: u32) -> Emit {
        if signed && size != Size::B64 {
            self.a.ldrs(mem_size(size), rt, base, offset)
        } else {
            self.a.ldr(mem_size(size), rt, base, offset)
        }
    }

    /// `rt` = a general-purpose register at `size`, zero-extended.
    fn load_reg(&mut self, rt: HostReg, reg: Reg, size: Size) -> Emit {
        self.read_reg(false, rt, reg, size)
    }

    /// `rt` = a general-purpose register at `size`, zero- or sign-extended
    /// to 64 bits.
    fn read_reg(&mut self, signed: bool, rt: HostReg, reg: Reg, size: Size) -> Emit {
        match location(reg).expect("checked by supported") {
            Loc::Mem(offset) => self.load(signed, size, rt, CPU, offset),
            Loc::Host(h, 0) if size == Size::B64 => {
                self.a.mov(Width::X, rt, h);
                Ok(())
            }
            Loc::Host(h, 0) if size == Size::B32 && !signed => {
                self.a.mov(Width::W, rt, h);
                Ok(())
            }
            // UBFX / SBFX of the field.
            Loc::Host(h, lsb) => self
                .a
                .bfm(signed, Width::X, rt, h, lsb, lsb + size.bits() - 1),
        }
    }

    /// Writes `rt` to a general-purpose register at `size`. A 32-bit value
    /// must already be zero-extended (it is after any W-register
    /// operation): x86 zero-extends 32-bit register writes, while 8- and
    /// 16-bit writes keep the rest of the register.
    fn store_reg(&mut self, reg: Reg, size: Size, rt: HostReg) -> Emit {
        match location(reg).expect("checked by supported") {
            Loc::Mem(offset) => {
                let size = if size == Size::B32 { Size::B64 } else { size };
                self.a.str(mem_size(size), rt, CPU, offset)
            }
            Loc::Host(h, _) if size == Size::B64 => {
                self.a.mov(Width::X, h, rt);
                Ok(())
            }
            Loc::Host(h, _) if size == Size::B32 => {
                self.a.mov(Width::W, h, rt);
                Ok(())
            }
            // BFI: the low `size` bits of `rt` into the field.
            Loc::Host(h, lsb) => {
                self.a
                    .bfm_insert(Width::X, h, rt, (64 - lsb) % 64, size.bits() - 1)
            }
        }
    }

    /// Loads (`load`) or stores the guest registers kept in host
    /// registers from or to `Cpu::regs`, in pairs.
    fn guest_registers(&mut self, load: bool) -> Emit {
        for (first, second) in [
            (0, 1),
            (2, 3),
            (4, 5),
            (6, 7),
            (8, 9),
            (10, 11),
            (12, 13),
            (14, 15),
        ] {
            // Compatibility mode has no r8–r15: their host registers stay
            // untouched (x18 is the TEB under Windows, ADR-0047), and the
            // prologue keeps its length.
            if self.options.compat && first >= 8 {
                self.a.nop();
                continue;
            }
            let (a, b) = (GUEST[first], GUEST[second]);
            let (a, b) = (a.expect("mapped"), b.expect("mapped"));
            self.a.pair(
                load,
                PairMode::Offset,
                a,
                b,
                CPU,
                reg_offset(first as u8) as i32,
            )?;
        }
        Ok(())
    }

    /// Loads (`load`) or stores the xmm registers, kept in v16–v31, from
    /// or to `Cpu::xmm`, in pairs.
    fn vector_registers(&mut self, load: bool) -> Emit {
        let base = offset_of!(Cpu, xmm) as i32;
        for pair in 0..8u8 {
            self.a.pair_q(
                load,
                VReg::new(16 + 2 * pair),
                VReg::new(17 + 2 * pair),
                CPU,
                base + 32 * i32::from(pair),
            )?;
        }
        Ok(())
    }

    /// The entry of the epilogue that records `status` first.
    fn status_entry(&mut self, status: u64) -> Label {
        if let Some(&(_, entry)) = self.status_entries.iter().find(|(s, _)| *s == status) {
            return entry;
        }
        let entry = self.a.new_label();
        self.status_entries.push((status, entry));
        entry
    }

    /// Records the exit status for the epilogue.
    fn set_status(&mut self, status: u64) -> Emit {
        self.a.mov_imm64(T1, status);
        self.a.str(
            MemSize::B64,
            T1,
            CONTEXT,
            field(offset_of!(Context, status)),
        )
    }

    /// Reads a register, memory or immediate operand into `rt`,
    /// zero-extended from `size`.
    fn read_operand(
        &mut self,
        rt: HostReg,
        operand: Operand,
        size: Size,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        match operand {
            Operand::Reg(reg) => self.load_reg(rt, reg, size),
            Operand::Mem(m) => {
                if let Some((base, offset)) =
                    self.direct(&m, bytes(size), Check::Read, at.rip, slow)
                {
                    return self.load(false, size, rt, base, offset);
                }
                self.address(&m, at.next, true)?;
                self.check(Check::Read, bytes(size), at.rip, slow)?;
                self.guest_load(false, size, rt)
            }
            Operand::Imm(imm) => {
                self.a.mov_imm64(rt, imm as u64 & size.mask());
                Ok(())
            }
        }
    }

    /// The host register holding `operand` at `size` without a copy: the
    /// guest register itself when it is mapped whole and `size` is 32 or
    /// 64 bits (a W operation reads only the low half), otherwise `rt`
    /// after `read_operand`.
    fn operand_in(
        &mut self,
        rt: HostReg,
        operand: Operand,
        size: Size,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit<HostReg> {
        if let Some(h) = whole_register(operand, size) {
            return Ok(h);
        }
        self.read_operand(rt, operand, size, at, slow)?;
        Ok(rt)
    }

    /// `read_destination`, or the mapped register itself as in
    /// `operand_in`.
    fn destination_in(
        &mut self,
        dst: Operand,
        size: Size,
        at: Site,
        slow: &mut Option<Label>,
        writes: bool,
    ) -> Emit<HostReg> {
        if let Some(h) = whole_register(dst, size) {
            return Ok(h);
        }
        self.read_destination(dst, size, at, slow, writes)?;
        Ok(A)
    }

    /// Writes `rt` to a register or memory operand (see `store_reg`).
    fn write_operand(
        &mut self,
        operand: Operand,
        size: Size,
        rt: HostReg,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        match operand {
            Operand::Reg(reg) => self.store_reg(reg, size, rt),
            Operand::Mem(m) => {
                if let Some((base, offset)) =
                    self.direct(&m, bytes(size), Check::Write, at.rip, slow)
                {
                    return self.a.str(mem_size(size), rt, base, offset);
                }
                self.address(&m, at.next, true)?;
                self.check(Check::Write, bytes(size), at.rip, slow)?;
                self.guest_store(size, rt)
            }
            Operand::Imm(_) => unreachable!("checked by supported"),
        }
    }

    /// Reads the destination of a read-modify-write instruction into `A`.
    /// For memory, the address stays in `ADDR` for `write_destination`,
    /// and the check is for writing when the instruction writes.
    fn read_destination(
        &mut self,
        dst: Operand,
        size: Size,
        at: Site,
        slow: &mut Option<Label>,
        writes: bool,
    ) -> Emit {
        match dst {
            Operand::Mem(m) => {
                self.address(&m, at.next, true)?;
                // A writable page is readable (Prot::allows).
                let check = if writes { Check::Write } else { Check::Read };
                self.check(check, bytes(size), at.rip, slow)?;
                self.guest_load(false, size, A)
            }
            _ => self.read_operand(A, dst, size, at, slow),
        }
    }

    /// Writes the result of a read-modify-write instruction (address in
    /// `ADDR` for memory, already checked by `read_destination`).
    fn write_destination(&mut self, dst: Operand, size: Size, rt: HostReg) -> Emit {
        match dst {
            Operand::Reg(reg) => self.store_reg(reg, size, rt),
            Operand::Mem(_) => self.guest_store(size, rt),
            Operand::Imm(_) => unreachable!("checked by supported"),
        }
    }

    /// An atomic instruction in concurrent mode (`atomic_rmw`, ADR-0023
    /// step 4): one LSE atomic with acquire and release, like the host
    /// compare-and-swap of the interpreter, so that both engines can
    /// update the same word from different threads. The old value lands
    /// in `A`; result and flags then come as in the plain translation.
    /// An unaligned operand (a split lock: LSE atomics need alignment)
    /// takes the slow path, before anything is written.
    fn atomic(&mut self, insn: &Instruction, at: Site, slow: &mut Option<Label>) -> Emit {
        let size = insn.size;
        let w = width(size);
        let m = match insn.op {
            Op::Alu { dst, .. }
            | Op::Unary { dst, .. }
            | Op::Cmpxchg { dst, .. }
            | Op::Xadd { dst, .. } => dst,
            Op::Xchg { a, b } => {
                if a.is_mem() {
                    a
                } else {
                    b
                }
            }
            _ => unreachable!("checked by atomic_rmw"),
        };
        let Operand::Mem(m) = m else {
            unreachable!("checked by atomic_rmw")
        };
        self.address(&m, at.next, true)?;
        // For writing even when cmpxchg fails (x86 writes then too); a
        // write to code goes to the interpreter, which invalidates.
        self.check(Check::Write, bytes(size), at.rip, slow)?;
        let slow_path = self.slow_path(at.rip, slow);
        for bit in 0..size.bytes().trailing_zeros() {
            self.a.tbz(true, ADDR, bit, slow_path);
        }
        let ms = mem_size(size);
        match insn.op {
            Op::Alu { op, src, .. } => {
                self.read_operand(B, src, size, at, slow)?;
                let kind = match op {
                    AluOp::Add => {
                        self.atomic_op(AtomicOp::Add, ms, B, A)?;
                        self.arith(false, size, A, B)?;
                        FlagKind::Add
                    }
                    AluOp::Sub => {
                        self.a.sub(Width::X, T2, ZR, B, Shift::Lsl, 0)?;
                        self.atomic_op(AtomicOp::Add, ms, T2, A)?;
                        self.arith(true, size, A, B)?;
                        FlagKind::Sub
                    }
                    _ => {
                        let (atomic, logic) = match op {
                            AluOp::And => {
                                // Clears the bits set in NOT B.
                                self.a.logic(
                                    Logic::Orr,
                                    true,
                                    Width::X,
                                    T2,
                                    ZR,
                                    B,
                                    Shift::Lsl,
                                    0,
                                )?;
                                (AtomicOp::Clr, Logic::And)
                            }
                            AluOp::Or => (AtomicOp::Set, Logic::Orr),
                            AluOp::Xor => (AtomicOp::Eor, Logic::Eor),
                            _ => unreachable!("checked by atomic_rmw"),
                        };
                        let operand = if op == AluOp::And { T2 } else { B };
                        self.atomic_op(atomic, ms, operand, A)?;
                        self.a.logic(logic, false, w, R, A, B, Shift::Lsl, 0)?;
                        FlagKind::Logic
                    }
                };
                self.defer(kind, size, A, B, STATUS_FLAGS);
            }
            Op::Unary { op, .. } => match op {
                UnaryOp::Inc | UnaryOp::Dec => {
                    let sub = op == UnaryOp::Dec;
                    self.a.movz(Width::X, B, 1, 0)?;
                    if sub {
                        self.a.sub(Width::X, T2, ZR, B, Shift::Lsl, 0)?;
                        self.atomic_op(AtomicOp::Add, ms, T2, A)?;
                    } else {
                        self.atomic_op(AtomicOp::Add, ms, B, A)?;
                    }
                    self.arith(sub, size, A, B)?;
                    let kind = if sub { FlagKind::Sub } else { FlagKind::Add };
                    // CF unchanged, as in the plain translation.
                    self.defer(kind, size, A, B, STATUS_FLAGS & !FLAG_CF);
                }
                UnaryOp::Not => {
                    self.a
                        .logic(Logic::Orr, true, Width::X, T2, ZR, ZR, Shift::Lsl, 0)?;
                    self.atomic_op(AtomicOp::Eor, ms, T2, A)?;
                }
                UnaryOp::Neg => unreachable!("checked by atomic_rmw"),
            },
            Op::Xchg { a, b } => {
                let Operand::Reg(reg) = (if a.is_mem() { b } else { a }) else {
                    unreachable!("checked by supported")
                };
                self.load_reg(B, reg, size)?;
                self.atomic_op(AtomicOp::Swp, ms, B, A)?;
                self.store_reg(reg, size, A)?;
            }
            Op::Xadd { src, .. } => {
                self.load_reg(B, src, size)?;
                self.atomic_op(AtomicOp::Add, ms, B, A)?;
                self.arith(false, size, A, B)?;
                self.defer(FlagKind::Add, size, A, B, STATUS_FLAGS);
                self.store_reg(src, size, A)?;
            }
            Op::Cmpxchg { src, .. } => {
                // CASAL compares with rax and leaves the old value in `A`;
                // then the flags of cmp rax, old, and rax = old if they
                // differ (a 32-bit rax is not written when they are equal).
                let rax = Reg::Gpr(RAX as u8);
                self.load_reg(A, rax, size)?;
                self.load_reg(T3, src, size)?;
                self.compare_and_swap(ms, A, T3)?;
                self.load_reg(B, rax, size)?;
                self.arith(true, size, B, A)?;
                self.defer(FlagKind::Sub, size, B, A, STATUS_FLAGS);
                let equal = self.a.new_label();
                self.a.b_cond(Cond::Eq, equal);
                self.store_reg(rax, size, A)?;
                self.a.bind(equal);
            }
            _ => unreachable!("checked by atomic_rmw"),
        }
        Ok(())
    }

    /// LD<op>AL or SWPAL at `ADDR`: `rt` gets the old value. Without
    /// FEAT_LSE, an exclusive loop with the same effect and ordering
    /// (acquire and release); it uses T1 and T4.
    fn atomic_op(&mut self, op: AtomicOp, ms: MemSize, rs: HostReg, rt: HostReg) -> Emit {
        if self.options.features.lse {
            self.mark_access();
            self.a.atomic(op, ms, rs, rt, ADDR);
            return Ok(());
        }
        let again = self.a.new_label();
        self.a.bind(again);
        self.mark_access();
        self.a.ldaxr(ms, rt, ADDR);
        match op {
            AtomicOp::Add => self.a.add(Width::X, T1, rt, rs, Shift::Lsl, 0)?,
            AtomicOp::Clr => self
                .a
                .logic(Logic::And, true, Width::X, T1, rt, rs, Shift::Lsl, 0)?,
            AtomicOp::Eor => {
                self.a
                    .logic(Logic::Eor, false, Width::X, T1, rt, rs, Shift::Lsl, 0)?
            }
            AtomicOp::Set => {
                self.a
                    .logic(Logic::Orr, false, Width::X, T1, rt, rs, Shift::Lsl, 0)?
            }
            AtomicOp::Swp => self.a.mov(Width::X, T1, rs),
        }
        self.mark_access();
        self.a.stlxr(ms, T4, T1, ADDR);
        self.a.cbz(true, Width::W, T4, again);
        Ok(())
    }

    /// CASAL at `ADDR`: if the bytes there equal `expected` (zero-extended)
    /// they become `new`; `expected` gets the old value. Without FEAT_LSE,
    /// an exclusive loop; a failed comparison drops the monitor and is
    /// still a full barrier, as x86's locked cmpxchg (it uses T1 and T4).
    fn compare_and_swap(&mut self, ms: MemSize, expected: HostReg, new: HostReg) -> Emit {
        if self.options.features.lse {
            self.mark_access();
            self.a.casal(ms, expected, new, ADDR);
            return Ok(());
        }
        let (again, differ, done) = (self.a.new_label(), self.a.new_label(), self.a.new_label());
        self.a.mov(Width::X, T1, expected);
        self.a.bind(again);
        self.mark_access();
        self.a.ldaxr(ms, expected, ADDR);
        self.a.subs(Width::X, ZR, expected, T1, Shift::Lsl, 0)?;
        self.a.b_cond(Cond::Ne, differ);
        self.mark_access();
        self.a.stlxr(ms, T4, new, ADDR);
        self.a.cbz(true, Width::W, T4, again);
        self.a.b(done);
        self.a.bind(differ);
        self.a.clrex();
        self.a.dmb(Barrier::Ish);
        self.a.bind(done);
        Ok(())
    }

    /// Pushes `value`: the slot is checked before rsp changes.
    /// Pushes the low `size` bytes of `value` (the stack width): esp wraps
    /// at 4 GiB in compatibility mode (a 32-bit subtraction).
    fn push(&mut self, value: HostReg, size: Size, rip: u64, slow: &mut Option<Label>) -> Emit {
        let bytes = size.bytes() as u32;
        let rsp = register_host(RSP as u8).expect("rsp is mapped");
        // ADR-0046: one store with writeback when the host checks writes
        // and rsp is below the region's end and 8-aligned: the slot then
        // cannot cross a page, and below the region (or above, in the
        // guard) the host faults. A misaligned rsp: the interpreter.
        if let (Some(bits), false, Size::B64) =
            (self.options.host_reads, self.options.concurrent, size)
            && value != rsp
        {
            let slow = self.slow_path(rip, slow);
            // Unless an earlier check of rsp covers the slot (`covered`).
            if !self.covered(rsp, (-8i64) as u64, 8, 8) {
                self.a
                    .logic_imm(Logic::Ands, Width::X, ZR, rsp, !((1u64 << bits) - 1) | 7)?;
                self.a.b_cond(Cond::Ne, slow);
                self.checked.push((rsp, 0, 8));
            }
            self.access_slow = Some(slow);
            self.mark_access();
            return self
                .a
                .load_store_indexed(false, MemSize::B64, value, rsp, -8, true);
        }
        self.a.sub_imm(width(size), ADDR, rsp, bytes)?;
        self.check(Check::Write, bytes, rip, slow)?;
        self.guest_store(size, value)?;
        self.store_reg(Reg::Gpr(RSP as u8), size, ADDR)
    }

    /// `ADDR` = the address of `mem` (with the segment base if `segment`).
    fn address(&mut self, mem: &Mem, next: u64, segment: bool) -> Emit {
        self.address_into(ADDR, mem, next, segment)
    }

    /// `rd` = the address of `mem`. A base and an index mapped to host
    /// registers are added without copies. `rd` other than `ADDR` (lea)
    /// requires a mapped base, so that no partial sum overwrites a
    /// register still to be read. Uses `T1`.
    fn address_into(&mut self, rd: HostReg, mem: &Mem, next: u64, segment: bool) -> Emit {
        self.address_into_w(rd, mem, next, segment, false)
    }

    /// `address_into`, as a 32-bit offset when `w32` (lea of a 32-bit
    /// register) or the address is 32 bits: computed modulo 2^32 in W
    /// registers, whose writes clear the top half, without a zero-extension
    /// after.
    fn address_into_w(
        &mut self,
        rd: HostReg,
        mem: &Mem,
        next: u64,
        segment: bool,
        w32: bool,
    ) -> Emit {
        let w = if w32 || mem.addr32 {
            Width::W
        } else {
            Width::X
        };
        self.simple_address = None;
        self.const_address = None;
        let mut constant = None;
        let simple = match (mem.base, mem.index) {
            (Base::Gpr(n), None) if rd == ADDR && w == Width::X => register_host(n),
            _ => None,
        };
        let mut disp = mem.disp as u64;
        let shift = u32::from(mem.scale).trailing_zeros();
        match mem.base {
            Base::Gpr(n) => {
                let base = match register_host(n) {
                    Some(h) => h,
                    None => {
                        debug_assert_eq!(rd, ADDR, "an unmapped base goes through ADDR");
                        self.load_reg(rd, Reg::Gpr(n), Size::B64)?;
                        rd
                    }
                };
                match mem.index {
                    Some(index) => {
                        let index = self.register_in(T1, index)?;
                        self.a.add(w, rd, base, index, Shift::Lsl, shift)?;
                    }
                    None => {
                        self.offset_into_w(rd, base, disp, w)?;
                        disp = 0;
                    }
                }
            }
            Base::None | Base::Rip => {
                let value = if mem.base == Base::Rip {
                    next.wrapping_add(disp)
                } else {
                    disp
                };
                disp = 0;
                match mem.index {
                    Some(index) => {
                        // The scaled index first: `rd` may be the index.
                        let index = self.register_in(T1, index)?;
                        self.a.shift_imm(Shift::Lsl, w, rd, index, shift)?;
                        self.add_constant_w(rd, value, w)?;
                    }
                    None => {
                        let value = if w == Width::W {
                            value & u64::from(u32::MAX)
                        } else {
                            value
                        };
                        self.a.mov_imm64(rd, value);
                        constant = Some(value);
                    }
                }
            }
        }
        // The address-size prefix, and every address in compatibility
        // mode: a 32-bit offset, already zero-extended by the W operations.
        self.add_constant_w(rd, disp, w)?;
        if segment {
            let base = match mem.segment {
                Segment::Default => None,
                Segment::Fs => Some(offset_of!(Cpu, fs_base)),
                Segment::Gs => Some(offset_of!(Cpu, gs_base)),
            };
            if let Some(offset) = base {
                self.a.ldr(MemSize::B64, T1, CPU, field(offset))?;
                // In compatibility mode the sum wraps at 4 GiB too (i386
                // glibc reads its TLS at negative offsets from gs).
                let w = if self.options.compat {
                    Width::W
                } else {
                    Width::X
                };
                self.a.add(w, ADDR, ADDR, T1, Shift::Lsl, 0)?;
                return Ok(());
            }
        }
        if let Some(base) = simple {
            self.simple_address = Some((base, mem.disp as u64, self.a.len()));
        }
        if let (Some(value), true) = (constant, rd == ADDR) {
            self.const_address = Some((value, self.a.len()));
        }
        Ok(())
    }

    /// The host register of guest register `n` (64 bits), or `rt` loaded
    /// from the `Cpu`.
    fn register_in(&mut self, rt: HostReg, n: u8) -> Emit<HostReg> {
        match register_host(n) {
            Some(h) => Ok(h),
            None => {
                self.load_reg(rt, Reg::Gpr(n), Size::B64)?;
                Ok(rt)
            }
        }
    }

    /// `rd` = `base` + `value` at width `w`, in one instruction when it
    /// fits an immediate (uses `T1` otherwise); with W, modulo 2^32 and
    /// zero-extended (a copy too, when `rd` is `base`).
    fn offset_into_w(&mut self, rd: HostReg, base: HostReg, value: u64, w: Width) -> Emit {
        let value = if w == Width::W {
            value & u64::from(u32::MAX)
        } else {
            value
        };
        let negated = if w == Width::W {
            u64::from((value as u32).wrapping_neg())
        } else {
            value.wrapping_neg()
        };
        if value < 1 << 12 {
            if value == 0 {
                if rd != base || w == Width::W {
                    self.a.mov(w, rd, base);
                }
                return Ok(());
            }
            self.a.add_imm(w, rd, base, value as u32)
        } else if negated < 1 << 12 {
            self.a.sub_imm(w, rd, base, negated as u32)
        } else {
            self.a.mov_imm64(T1, value);
            self.a.add(w, rd, base, T1, Shift::Lsl, 0)
        }
    }

    /// add, sub, cmp, and, or and xor of a guest register mapped whole, at
    /// 32 or 64 bits: the operation writes the register itself, without
    /// host flags, and an immediate that fits goes in the instruction. The
    /// flags are deferred, as `defer` does: the operands in `FA`/`FB`
    /// (copied before the register changes), or the logical result in
    /// `FA`.
    fn alu_in_place(
        &mut self,
        op: AluOp,
        h: HostReg,
        src: Operand,
        size: Size,
        at: Site,
        slow: &mut Option<Label>,
    ) -> Emit {
        let w = width(size);
        let arith = matches!(op, AluOp::Add | AluOp::Sub | AluOp::Cmp);
        // An immediate encodable in the instruction: (subtract, value).
        let imm = match src {
            Operand::Imm(imm) => {
                let v = imm as u64 & size.mask();
                let negated = v.wrapping_neg() & size.mask();
                let bits = if w == Width::X { 64 } else { 32 };
                if arith && v < 1 << 12 {
                    Some((false, v))
                } else if arith && negated < 1 << 12 {
                    Some((true, negated))
                } else if !arith && encode_bitmask(v, bits).is_some() {
                    Some((false, v))
                } else {
                    None
                }
            }
            _ => None,
        };
        let defer = !self.skip_flags;
        // The first operand, when a copy before was left to this
        // operation (`copy_fuses`); the deferred flags hold the result.
        let from = self.first.take().unwrap_or(h);
        // A memory operand of an addition, subtraction or comparison goes
        // straight to `FB`, where the deferred flags keep it: the load is
        // the instruction's only access, and a load that faults leaves its
        // register as it was, so the slow path still finds the flags
        // pending before (in `FA`/`FB`) intact.
        let rt = if arith && defer && matches!(src, Operand::Mem(_) | Operand::Imm(_)) {
            FB
        } else {
            B
        };
        let b = match imm {
            Some(_) => None,
            None => Some(self.operand_in(rt, src, size, at, slow)?),
        };
        if arith {
            if defer {
                // The operands stay where they are (`Held`): the second a
                // mapped register, a constant, or (from memory) `FB`; the
                // first the destination, which holds the result after an
                // addition or a subtraction. Unless the two are the same
                // register: then nothing could give the first back.
                let b_held = match (b, src) {
                    (Some(FB), _) => Some(Held::Fb),
                    (Some(B), _) => {
                        self.a.mov(w, FB, B);
                        Some(Held::Fb)
                    }
                    (Some(b), _) if b != h => Some(Held::Reg(b)),
                    (Some(_), _) => None,
                    (None, Operand::Imm(imm)) => Some(Held::Imm(imm as u64 & size.mask())),
                    _ => unreachable!("an immediate or a register"),
                };
                let kind = if op == AluOp::Add {
                    FlagKind::Add
                } else {
                    FlagKind::Sub
                };
                self.flags_emitted |= STATUS_FLAGS;
                let mut pending = Pending::new(kind, size, STATUS_FLAGS);
                match b_held {
                    Some(b) => {
                        pending.a = Held::Reg(h);
                        pending.b = b;
                        pending.result = op != AluOp::Cmp;
                    }
                    None => {
                        self.a.mov(w, FA, h);
                        self.a.mov(w, FB, h);
                    }
                }
                self.lazy = Lazy::Static(pending);
            }
            if op == AluOp::Cmp {
                return Ok(());
            }
            let sub = op == AluOp::Sub;
            match (b, imm) {
                (Some(b), _) if sub => self.a.sub(w, h, from, b, Shift::Lsl, 0)?,
                (Some(b), _) => self.a.add(w, h, from, b, Shift::Lsl, 0)?,
                (None, Some((negated, v))) if sub != negated => {
                    self.a.sub_imm(w, h, from, v as u32)?
                }
                (None, Some((_, v))) => self.a.add_imm(w, h, from, v as u32)?,
                (None, None) => unreachable!("an immediate or a register"),
            }
            return Ok(());
        }
        let logic = match op {
            AluOp::And => Logic::And,
            AluOp::Or => Logic::Orr,
            _ => Logic::Eor,
        };
        match (b, imm) {
            (Some(b), _) => self.a.logic(logic, false, w, h, from, b, Shift::Lsl, 0)?,
            (None, Some((_, v))) => self.a.logic_imm(logic, w, h, from, v)?,
            (None, None) => unreachable!("an immediate or a register"),
        }
        if defer {
            // The result stays in the destination.
            self.defer_result(size);
            if let Lazy::Static(p) = &mut self.lazy {
                p.a = Held::Reg(h);
            }
        }
        Ok(())
    }

    /// inc, dec, neg and not of a guest register mapped whole, at 32 or 64
    /// bits, in place; the flags deferred as `defer` does (the operands
    /// copied before the register changes).
    fn unary_in_place(&mut self, op: UnaryOp, h: HostReg, size: Size) -> Emit {
        let w = width(size);
        let defer = !self.skip_flags;
        let pending = |kind, mask| Lazy::Static(Pending::new(kind, size, mask));
        match op {
            UnaryOp::Inc | UnaryOp::Dec => {
                let sub = op == UnaryOp::Dec;
                if defer {
                    // As the generic path: CF kept while it is live.
                    let mask = if self.live & FLAG_CF == 0 {
                        STATUS_FLAGS
                    } else {
                        STATUS_FLAGS & !FLAG_CF
                    };
                    self.flags_emitted |= mask;
                    let kind = if sub { FlagKind::Sub } else { FlagKind::Add };
                    // The result in the register, and 1 (`Held`).
                    self.lazy = pending(kind, mask);
                    if let Lazy::Static(p) = &mut self.lazy {
                        p.a = Held::Reg(h);
                        p.b = Held::Imm(1);
                        p.result = true;
                    }
                }
                if sub {
                    self.a.sub_imm(w, h, h, 1)
                } else {
                    self.a.add_imm(w, h, h, 1)
                }
            }
            UnaryOp::Neg => {
                // 0 - a.
                if defer {
                    self.a.mov(Width::X, FA, ZR);
                    self.a.mov(w, FB, h);
                    self.flags_emitted |= STATUS_FLAGS;
                    self.lazy = pending(FlagKind::Sub, STATUS_FLAGS);
                }
                self.a.sub(w, h, ZR, h, Shift::Lsl, 0)
            }
            UnaryOp::Not => self.a.logic(Logic::Orr, true, w, h, ZR, h, Shift::Lsl, 0),
        }
    }

    /// The flags of a logical result already in `FA` are pending.
    fn defer_result(&mut self, size: Size) {
        self.flags_emitted |= STATUS_FLAGS;
        self.lazy = Lazy::Static(Pending::new(FlagKind::Logic, size, STATUS_FLAGS));
    }

    /// `reg += value` (modulo 2^64).
    fn add_constant(&mut self, reg: HostReg, value: u64) -> Emit {
        self.add_constant_w(reg, value, Width::X)
    }

    /// `reg += value` at width `w` (with W, modulo 2^32: `reg` must already
    /// be zero-extended, as nothing is written for 0).
    fn add_constant_w(&mut self, reg: HostReg, value: u64, w: Width) -> Emit {
        let (value, negated) = if w == Width::W {
            let v = value as u32;
            (u64::from(v), u64::from(v.wrapping_neg()))
        } else {
            (value, value.wrapping_neg())
        };
        if value == 0 {
            Ok(())
        } else if value < 1 << 12 {
            self.a.add_imm(w, reg, reg, value as u32)
        } else if negated < 1 << 12 {
            self.a.sub_imm(w, reg, reg, negated as u32)
        } else {
            self.a.mov_imm64(T1, value);
            self.a.add(w, reg, reg, T1, Shift::Lsl, 0)
        }
    }

    /// Branches to the slow path unless the guest may access `bytes` bytes
    /// at `ADDR` (ADR-0021): the address is below 2^47, the access stays
    /// in one 4K page, and the page allows it. Writes to executable pages
    /// take the slow path too, so that the interpreter invalidates the
    /// translated code (self-modifying code).
    fn check(&mut self, check: Check, bytes: u32, rip: u64, slow: &mut Option<Label>) -> Emit {
        self.check_aligned(check, bytes, 1, rip, slow)
    }

    /// `check`, for an access the instruction requires aligned to
    /// `required` bytes (#GP otherwise, from the interpreter): with host
    /// protection one test covers both, as the two go to the slow path.
    fn check_aligned(
        &mut self,
        check: Check,
        bytes: u32,
        required: u32,
        rip: u64,
        slow: &mut Option<Label>,
    ) -> Emit {
        self.flush_misaligned();
        let slow = self.slow_path(rip, slow);
        // ADR-0046: the host checks reads below the guest's region's end;
        // above it, the interpreter.
        if let Some(bits) = self.options.host_reads {
            let outside = !((1u64 << bits) - 1);
            self.access_slow = Some(slow);
            let misaligned = u64::from(bytes.next_power_of_two() - 1);
            // The alignment this access needs tested: writes, and ordered
            // reads, must not cross into a page they may not touch.
            let align = if bytes > 1 && (check == Check::Write || self.options.concurrent) {
                bytes.next_power_of_two()
            } else {
                1
            }
            .max(required);
            let here = self.a.len();
            // An address known now: the test is decided now.
            if let Some((value, at)) = self.const_address.take()
                && at == here
                && value
                    .checked_add(u64::from(bytes))
                    .is_some_and(|last| last <= 1u64 << bits)
                && value.is_multiple_of(u64::from(align))
            {
                self.known_aligned = align > 1;
                return Ok(());
            }
            let simple = self
                .simple_address
                .take()
                .filter(|&(_, _, at)| at == here)
                .map(|(base, disp, _)| (base, disp));
            if let Some((base, disp)) = simple {
                if self.covered(base, disp, bytes, align) {
                    self.known_aligned = align > 1;
                    return Ok(());
                }
                // The alignment proven: only a required one, whose
                // misaligned case goes to the slow path (a write's or an
                // ordered read's goes on out of line).
                self.checked.push((base, disp, required));
            }
            if required > 1 {
                // Misaligned or outside: the interpreter (#GP, or the
                // region). An aligned access cannot cross a page.
                self.a.logic_imm(
                    Logic::Ands,
                    Width::X,
                    ZR,
                    ADDR,
                    outside | u64::from(required - 1),
                )?;
                self.a.b_cond(Cond::Ne, slow);
                self.known_aligned = true;
                return Ok(());
            }
            if check == Check::Read && bytes > 1 && self.options.concurrent {
                // Ordered loads need alignment: one test for both, the
                // unaligned case out of line (`read_edges`).
                let (edge, after, unaligned) =
                    (self.a.new_label(), self.a.new_label(), self.a.new_label());
                self.a
                    .logic_imm(Logic::Ands, Width::X, ZR, ADDR, outside | misaligned)?;
                self.a.b_cond(Cond::Ne, edge);
                self.a.bind(after);
                self.read_edges.push((edge, outside, slow, unaligned));
                self.misaligned = Some((unaligned, after));
                return Ok(());
            }
            if check == Check::Read || bytes == 1 {
                // A read faults or not as a whole; a byte cannot cross.
                self.a.logic_imm(Logic::Ands, Width::X, ZR, ADDR, outside)?;
                self.a.b_cond(Cond::Ne, slow);
                return Ok(());
            }
            // A write must not cross into a page it may not write: the
            // host may write the first part before the fault, x86 writes
            // nothing. An aligned one cannot cross: one test for both, the
            // rest out of line (`write_edges`).
            let (edge, back) = (self.a.new_label(), self.a.new_label());
            self.a
                .logic_imm(Logic::Ands, Width::X, ZR, ADDR, outside | misaligned)?;
            self.a.b_cond(Cond::Ne, edge);
            self.a.bind(back);
            let unaligned = self.options.concurrent.then(|| self.a.new_label());
            if let Some(unaligned) = unaligned {
                self.misaligned = Some((unaligned, back));
            }
            self.write_edges
                .push((edge, back, bytes, outside, slow, unaligned));
            return Ok(());
        }
        self.a
            .logic_imm(Logic::Ands, Width::X, ZR, ADDR, !((1u64 << 47) - 1))?;
        self.a.b_cond(Cond::Ne, slow);
        self.a.shift_imm(Shift::Lsr, Width::X, T1, ADDR, 12)?;
        self.a
            .load_store_reg(true, MemSize::B8, T1, PAGES, T1, false);
        // Bits the page table derives from the permissions: readable (any
        // permission, as on x86) or writable and not executable; for this
        // page alone, or for it and the next one.
        let (alone, span) = match check {
            Check::Read => (PAGE_READABLE_BIT, PAGE_READABLE_SPAN_BIT),
            Check::Write => (PAGE_WRITABLE_DATA_BIT, PAGE_WRITABLE_DATA_SPAN_BIT),
        };
        if bytes == 1 {
            // A single byte cannot cross into the next page.
            self.a.tbz(false, T1, alone, slow);
            return Ok(());
        }
        // With the next page allowed too, crossing into it does not
        // matter. Otherwise (the last page before a hole or a change of
        // permissions), out of line: this page alone, without crossing.
        let (edge, back) = (self.a.new_label(), self.a.new_label());
        self.a.tbz(false, T1, span, edge);
        self.a.bind(back);
        self.page_edges.push((edge, back, alone, bytes, slow));
        Ok(())
    }

    /// The out-of-line part of `check` for an access in a page whose next
    /// page does not allow it: `T1` holds the page's byte.
    fn page_edge(&mut self, edge: Label, back: Label, alone: u32, bytes: u32, slow: Label) -> Emit {
        self.a.bind(edge);
        self.a.tbz(false, T1, alone, slow);
        self.a.logic_imm(Logic::And, Width::X, T1, ADDR, 0xfff)?;
        self.a.subs_imm(Width::X, ZR, T1, 4096 - bytes)?;
        self.a.b_cond(Cond::Hi, slow);
        self.a.b(back);
        Ok(())
    }

    /// Updates the status flags in `FLAGS` (only those in `mask`) after an
    /// operation on `a` and `b` that left its result in `R` and set the
    /// host NZCV. Matches `armor_cpu::alu_op`.
    fn flags(&mut self, kind: FlagKind, a: HostReg, b: HostReg, mask: u64) -> Emit {
        // T1 = CF, PF, ZF, SF, OF from the host NZCV and `R`.
        self.table_flags(T1, kind == FlagKind::Sub)?;
        if kind != FlagKind::Logic {
            // AF: the carry out of bit 3.
            self.a
                .logic(Logic::Eor, false, Width::X, T2, a, b, Shift::Lsl, 0)?;
            self.a
                .logic(Logic::Eor, false, Width::X, T2, T2, R, Shift::Lsl, 0)?;
            self.a.logic_imm(Logic::And, Width::X, T2, T2, FLAG_AF)?;
            self.or_into_t1(T2, 0)?;
        }
        self.merge_flags(mask)
    }

    /// With CF and OF already in `T1`: ZF, SF and PF from `R` (zero-
    /// extended from `size`), AF cleared, merged into `FLAGS` under `mask`.
    fn result_flags(&mut self, size: Size, mask: u64) -> Emit {
        // After a logical test the host C and V are clear: the table adds
        // nothing to CF and OF.
        self.test_result(size)?;
        self.table_flags(T2, false)?;
        self.or_into_t1(T2, 0)?;
        self.merge_flags(mask)
    }

    /// `rd` = `FLAG_TABLE[borrow << 12 | NZCV << 8 | R & 0xff]`. Uses `T3`.
    fn table_flags(&mut self, rd: HostReg, borrow: bool) -> Emit {
        self.a.read_flags(rd);
        self.a.shift_imm(Shift::Lsr, Width::X, rd, rd, 20)?;
        self.a.logic_imm(Logic::And, Width::X, T3, R, 0xff)?;
        self.a
            .logic(Logic::Orr, false, Width::X, rd, rd, T3, Shift::Lsl, 0)?;
        if borrow {
            self.a.logic_imm(Logic::Orr, Width::X, rd, rd, 1 << 12)?;
        }
        self.a.ldr(
            MemSize::B64,
            T3,
            CONTEXT,
            field(offset_of!(Context, flag_table)),
        )?;
        self.a.load_store_reg(true, MemSize::B16, rd, T3, rd, true);
        Ok(())
    }

    /// Records the flags of an operation on `a` and `b` (result in `R`)
    /// as pending, instead of computing them: the next flag-setting
    /// instruction may overwrite them first.
    fn defer(&mut self, kind: FlagKind, size: Size, a: HostReg, b: HostReg, mask: u64) {
        if self.skip_flags {
            return;
        }
        self.flags_emitted |= mask;
        // A W move zero-extends: a 32-bit operand may be a mapped guest
        // register with other bits above (`operand_in`).
        let w = if size == Size::B64 {
            Width::X
        } else {
            Width::W
        };
        if kind == FlagKind::Logic {
            self.a.mov(Width::X, FA, R);
        } else {
            if a != FA {
                self.a.mov(w, FA, a);
            }
            if b != FB {
                self.a.mov(w, FB, b);
            }
        }
        self.lazy = Lazy::Static(Pending::new(kind, size, mask));
    }

    /// Defers the flags of a shift or a multiplication (`FlagKind::Shift`,
    /// `FlagKind::Mul`): `a` (and `b`, when there is one) go to `FA` and
    /// `FB` whole, and `materialize` redoes the operation.
    fn defer_operation(&mut self, kind: FlagKind, size: Size, a: HostReg, b: Option<HostReg>) {
        if self.skip_flags {
            return;
        }
        self.flags_emitted |= STATUS_FLAGS;
        self.a.mov(Width::X, FA, a);
        if let Some(b) = b {
            self.a.mov(Width::X, FB, b);
        }
        self.lazy = Lazy::Static(Pending::new(kind, size, STATUS_FLAGS));
    }

    /// Before an instruction that keeps CF (inc, dec) while CF is live:
    /// only the pending CF goes into `FLAGS`; the other pending flags are
    /// all replaced by the instruction's. Uses `R`, `T1`, `T3`.
    fn materialize_carry(&mut self) -> Emit {
        match self.lazy {
            Lazy::Current => Ok(()),
            Lazy::Static(p) if p.mask & FLAG_CF == 0 => Ok(()),
            Lazy::Static(Pending {
                kind: FlagKind::Logic,
                ..
            }) => self
                .a
                .logic_imm(Logic::And, Width::X, FLAGS, FLAGS, !FLAG_CF),
            Lazy::Static(Pending {
                kind: kind @ (FlagKind::Add | FlagKind::Sub),
                size,
                ..
            }) => {
                let sub = kind == FlagKind::Sub;
                let Lazy::Static(p) = self.lazy else {
                    unreachable!("matched above")
                };
                let _ = size;
                self.redo(p)?;
                // A subtraction's CF is the inverted host carry.
                self.a
                    .cset(Width::X, T1, if sub { Cond::Lo } else { Cond::Hs });
                self.a.bfm_insert(Width::X, FLAGS, T1, 0, 0)
            }
            Lazy::Dynamic => match Pending::from_code(self.options.entry_kind) {
                Some(p) => self.carry_from_kind(p),
                None => self.materialize(),
            },
            _ => self.materialize(),
        }
    }

    /// CF into `FLAGS` from the operation `KIND` describes, inline for `p`
    /// (the one expected there), the flag routine otherwise. The flags stay
    /// `Lazy::Dynamic`: `KIND` and `FA`/`FB` still describe them, and CF in
    /// `FLAGS` agrees with them (the routine, when it runs, makes all of
    /// `FLAGS` current and clears `KIND`). Uses `R`, `T1`, `T3`, `T4`.
    fn carry_from_kind(&mut self, p: Pending) -> Emit {
        let code = p.code().expect("a plain operation");
        let (generic, back) = (self.a.new_label(), self.a.new_label());
        self.a
            .ldr(MemSize::B64, T1, CONTEXT, field(offset_of!(Context, kind)))?;
        self.a.subs_imm(Width::X, ZR, T1, u32::from(code))?;
        self.a.b_cond(Cond::Ne, generic);
        if p.mask & FLAG_CF != 0 {
            match p.kind {
                FlagKind::Logic => {
                    self.a
                        .logic_imm(Logic::And, Width::X, FLAGS, FLAGS, !FLAG_CF)?;
                }
                kind => {
                    self.redo(p)?;
                    let sub = kind == FlagKind::Sub;
                    self.a
                        .cset(Width::X, T1, if sub { Cond::Lo } else { Cond::Hs });
                    self.a.bfm_insert(Width::X, FLAGS, T1, 0, 0)?;
                }
            }
        }
        // (An increment's or decrement's pending flags leave CF in `FLAGS`.)
        self.a.bind(back);
        self.kind_carries.push((generic, back));
        self.nzcv_valid = false;
        self.nzcv_now = false;
        Ok(())
    }

    /// The register holding `h`, a constant loaded into `scratch`.
    fn held(&mut self, h: Held, scratch: HostReg) -> HostReg {
        match h {
            Held::Fa => FA,
            Held::Fb => FB,
            Held::Reg(r) => r,
            Held::Imm(v) => {
                self.a.mov_imm64(scratch, v);
                scratch
            }
        }
    }

    /// The first operand of a pending addition or subtraction, derived
    /// from the result when `p.result` (in `scratch`). Uses `T3`.
    fn first_operand(&mut self, p: Pending, scratch: HostReg) -> Emit<HostReg> {
        if !p.result {
            return Ok(self.held(p.a, scratch));
        }
        let r = self.held(p.a, scratch);
        let w = width(p.size);
        // a = r - b after an addition, r + b after a subtraction.
        self.arith_held_plain(p.kind == FlagKind::Add, w, scratch, r, p.b)?;
        Ok(scratch)
    }

    /// `rd` = `a` ± `b` (flags set), `b` where it is held; a constant goes
    /// in the instruction, as `alu_in_place` only holds encodable ones
    /// (below 4096, or negated: then the other operation, with the same
    /// NZCV, as the constant is never the size's minimum). Uses `T3`.
    fn arith_held(&mut self, sub: bool, w: Width, rd: HostReg, a: HostReg, b: Held) -> Emit {
        match b {
            Held::Imm(v) => {
                let bits = if w == Width::X { 64 } else { 32 };
                let mask = if bits == 64 { u64::MAX } else { 0xffff_ffff };
                let negated = v.wrapping_neg() & mask;
                match (v < 1 << 12, sub) {
                    (true, true) => self.a.subs_imm(w, rd, a, v as u32),
                    (true, false) => self.a.adds_imm(w, rd, a, v as u32),
                    (false, true) if negated < 1 << 12 => self.a.adds_imm(w, rd, a, negated as u32),
                    (false, false) if negated < 1 << 12 => {
                        self.a.subs_imm(w, rd, a, negated as u32)
                    }
                    _ => {
                        self.a.mov_imm64(T3, v);
                        if sub {
                            self.a.subs(w, rd, a, T3, Shift::Lsl, 0)
                        } else {
                            self.a.adds(w, rd, a, T3, Shift::Lsl, 0)
                        }
                    }
                }
            }
            _ => {
                let b = self.held(b, T3);
                if sub {
                    self.a.subs(w, rd, a, b, Shift::Lsl, 0)
                } else {
                    self.a.adds(w, rd, a, b, Shift::Lsl, 0)
                }
            }
        }
    }

    /// `arith_held` without setting flags (the value only).
    fn arith_held_plain(&mut self, sub: bool, w: Width, rd: HostReg, a: HostReg, b: Held) -> Emit {
        match b {
            Held::Imm(v) => {
                let mask = if w == Width::X { u64::MAX } else { 0xffff_ffff };
                let v = if sub { v.wrapping_neg() & mask } else { v };
                // rd = a + v (mod 2^bits).
                let negated = v.wrapping_neg() & mask;
                if v < 1 << 12 {
                    self.a.add_imm(w, rd, a, v as u32)
                } else if negated < 1 << 12 {
                    self.a.sub_imm(w, rd, a, negated as u32)
                } else {
                    self.a.mov_imm64(T3, v);
                    self.a.add(w, rd, a, T3, Shift::Lsl, 0)
                }
            }
            _ => {
                let b = self.held(b, T3);
                if sub {
                    self.a.sub(w, rd, a, b, Shift::Lsl, 0)
                } else {
                    self.a.add(w, rd, a, b, Shift::Lsl, 0)
                }
            }
        }
    }

    /// Copies the pending operands to `FA`/`FB` (the form the flag routine
    /// and `KIND` describe), deriving the first one from a result. Before
    /// a held register changes, before an exit, and in every slow path:
    /// the registers still hold the operands there, as translated code
    /// writes guest registers only after its last slow path. Uses `T3`.
    fn spill(&mut self) -> Emit {
        let Lazy::Static(p) = self.lazy else {
            return Ok(());
        };
        if p.plain() {
            return Ok(());
        }
        let w = width(p.size);
        match p.kind {
            FlagKind::Logic => {
                let r = self.held(p.a, T3);
                self.a.mov(Width::X, FA, r);
            }
            FlagKind::Add | FlagKind::Sub => {
                if p.result {
                    let r = self.held(p.a, FA);
                    self.arith_held_plain(p.kind == FlagKind::Add, w, FA, r, p.b)?;
                } else {
                    let a = self.held(p.a, FA);
                    if a != FA {
                        self.a.mov(w, FA, a);
                    }
                }
                match p.b {
                    Held::Imm(v) => self.a.mov_imm64(FB, v),
                    Held::Fb => {}
                    b => {
                        let b = self.held(b, FB);
                        self.a.mov(w, FB, b);
                    }
                }
            }
            _ => unreachable!("only additions, subtractions and logic hold operands"),
        }
        self.lazy = Lazy::Static(Pending::new(p.kind, p.size, p.mask));
        Ok(())
    }

    /// `Context::kind` = 0: nothing pending.
    fn clear_kind(&mut self) -> Emit {
        self.a
            .str(MemSize::B64, ZR, CONTEXT, field(offset_of!(Context, kind)))
    }

    /// Before leaving the block: `KIND` says what is pending (the flags
    /// are computed by whoever needs them, at the latest by the epilogue
    /// on the way back to the engine). Operations the flag routine does
    /// not know are computed here. Does not touch `A`.
    fn publish_lazy(&mut self) -> Emit {
        match self.lazy {
            Lazy::Current => self.clear_kind()?,
            Lazy::Static(p)
                if !p.plain()
                    && matches!(p.kind, FlagKind::Add | FlagKind::Sub | FlagKind::Logic) =>
            {
                self.spill()?;
                return self.publish_lazy();
            }
            Lazy::Static(p) => match p.code() {
                Some(code) => {
                    self.a.movz(Width::X, T1, code, 0)?;
                    self.a
                        .str(MemSize::B64, T1, CONTEXT, field(offset_of!(Context, kind)))?;
                }
                None => {
                    self.materialize()?;
                    self.clear_kind()?;
                }
            },
            Lazy::Dynamic => {}
        }
        Ok(())
    }

    /// Merges the pending flags, if any, into `FLAGS` (from `FA` and
    /// `FB`, redoing the operation for the host NZCV). Uses `R`, `T1`–`T3`.
    fn materialize(&mut self) -> Emit {
        self.spill()?;
        let p = match core::mem::replace(&mut self.lazy, Lazy::Current) {
            Lazy::Current => return Ok(()),
            Lazy::Static(p) => p,
            Lazy::Dynamic => {
                let skip = self.a.new_label();
                self.a
                    .ldr(MemSize::B64, T1, CONTEXT, field(offset_of!(Context, kind)))?;
                self.a.cbz(false, Width::X, T1, skip);
                self.a.ldr(
                    MemSize::B64,
                    T1,
                    CONTEXT,
                    field(offset_of!(Context, flags_routine)),
                )?;
                // x30 holds a guest register (r11): kept on the stack
                // around the call, which overwrites it with the return
                // address.
                // (`R` goes with it to make a pair: the routine clobbers it
                // anyway.)
                self.a
                    .pair(false, PairMode::PreIndex, HostReg::new(30), R, SP, -16)?;
                self.a.blr(T1);
                self.a
                    .pair(true, PairMode::PostIndex, HostReg::new(30), R, SP, 16)?;
                self.a.bind(skip);
                return Ok(());
            }
        };
        match p.kind {
            FlagKind::Logic => {
                self.a.mov(Width::X, R, FA);
                self.test_result(p.size)?;
            }
            FlagKind::Shift(op, count) => return self.shift_flags(op, p.size, count),
            FlagKind::Mul { signed } => return self.mul_flags(signed, p.size),
            FlagKind::FloatCompare { double } => {
                self.float_compare_nzcv(double);
                self.a.cset(Width::X, T1, Cond::Lt);
                self.a.cset(Width::X, T2, Cond::Vs);
                self.a.cset(Width::X, T3, Cond::Eq);
                self.a
                    .logic(Logic::Orr, false, Width::X, T3, T3, T2, Shift::Lsl, 0)?;
                self.or_into_t1(T2, FLAG_PF.trailing_zeros())?;
                self.or_into_t1(T3, FLAG_ZF.trailing_zeros())?;
                return self.merge_flags(STATUS_FLAGS);
            }
            kind => self.arith(kind == FlagKind::Sub, p.size, FA, FB)?,
        }
        self.flags(p.kind, FA, FB, p.mask)
    }

    /// The flags of shl, shr or sar of `FA` (zero-extended from `size`) by
    /// `count`, or by `FB` (masked; 0 leaves `FLAGS` as it is), as
    /// `armor_cpu::alu::shift`: CF is the last bit shifted out; OF is
    /// sign(result) ^ CF for shl, the value's sign for shr, 0 for sar.
    /// Uses `R`, `T1`–`T3`.
    fn shift_flags(&mut self, op: ShiftOp, size: Size, count: Option<u8>) -> Emit {
        let w = width(size);
        let bits = size.bits();
        let skip = self.a.new_label();
        let shift = match op {
            ShiftOp::Shl => Shift::Lsl,
            ShiftOp::Shr => Shift::Lsr,
            _ => Shift::Asr,
        };
        match count {
            Some(n) => {
                let n = u32::from(n);
                self.a.shift_imm(shift, w, R, FA, n)?;
                match op {
                    ShiftOp::Shl => self.a.shift_imm(Shift::Lsr, w, T1, FA, bits - n)?,
                    _ => self.a.shift_imm(shift, w, T1, FA, n - 1)?,
                }
            }
            None => {
                self.a.cbz(false, Width::X, FB, skip);
                self.a.shift_reg(shift, w, R, FA, FB);
                if op == ShiftOp::Shl {
                    self.a.movz(Width::X, T2, bits as u16, 0)?;
                    self.a.sub(Width::X, T2, T2, FB, Shift::Lsl, 0)?;
                    self.a.shift_reg(Shift::Lsr, w, T1, FA, T2);
                } else {
                    self.a.sub_imm(Width::X, T2, FB, 1)?;
                    self.a.shift_reg(shift, w, T1, FA, T2);
                }
            }
        }
        // T1 = CF | OF << 11. T2 = the sign bit of the result.
        self.a.logic_imm(Logic::And, Width::X, T1, T1, 1)?;
        self.a.shift_imm(Shift::Lsr, w, T2, R, bits - 1)?;
        match op {
            ShiftOp::Shl => self
                .a
                .logic(Logic::Eor, false, Width::X, T2, T2, T1, Shift::Lsl, 0)?,
            ShiftOp::Shr => self.a.shift_imm(Shift::Lsr, w, T2, FA, bits - 1)?,
            _ => self.a.mov(Width::X, T2, ZR),
        }
        self.or_into_t1(T2, 11)?;
        self.result_flags(size, STATUS_FLAGS)?;
        self.a.bind(skip);
        Ok(())
    }

    /// The flags of imul, or of a one-operand mul or imul, of `FA` by `FB`
    /// (extended to 64 bits as `signed` says): CF = OF = the product does
    /// not fit `size` (the high half is not the low half's extension); ZF,
    /// SF and PF from the low half. Uses `R`, `T1`–`T3`.
    fn mul_flags(&mut self, signed: bool, size: Size) -> Emit {
        if size == Size::B64 {
            self.a.madd(Width::X, R, FA, FB, ZR);
            self.a.mulh(signed, T3, FA, FB);
            if signed {
                self.a.shift_imm(Shift::Asr, Width::X, T2, R, 63)?;
                self.a.subs(Width::X, ZR, T3, T2, Shift::Lsl, 0)?;
            } else {
                self.a.subs(Width::X, ZR, T3, ZR, Shift::Lsl, 0)?;
            }
        } else {
            // The whole product fits in 64 bits.
            self.a.madd(Width::X, T3, FA, FB, ZR);
            if signed {
                self.a.bfm(true, Width::X, T2, T3, 0, 31)?;
            } else {
                self.a.mov(Width::W, T2, T3);
            }
            self.a.subs(Width::X, ZR, T3, T2, Shift::Lsl, 0)?;
            self.a.mov(Width::W, R, T3);
        }
        self.a.cset(Width::X, T1, Cond::Ne);
        self.or_into_t1(T1, 11)?;
        self.result_flags(size, STATUS_FLAGS)
    }

    fn or_into_t1(&mut self, rm: HostReg, shift: u32) -> Emit {
        self.a
            .logic(Logic::Orr, false, Width::X, T1, T1, rm, Shift::Lsl, shift)
    }

    /// `FLAGS` = (`FLAGS` & !`mask`) | `T1`.
    fn merge_flags(&mut self, mask: u64) -> Emit {
        self.flags_emitted |= mask;
        self.a.mov_imm64(T2, mask);
        if mask != STATUS_FLAGS {
            // T1 has every status flag; keep only those in `mask`.
            self.a
                .logic(Logic::And, false, Width::X, T1, T1, T2, Shift::Lsl, 0)?;
        }
        self.a
            .logic(Logic::And, true, Width::X, FLAGS, FLAGS, T2, Shift::Lsl, 0)?;
        self.a
            .logic(Logic::Orr, false, Width::X, FLAGS, FLAGS, T1, Shift::Lsl, 0)
    }

    /// Branches to `target` if the x86 condition holds on `FLAGS`.
    /// For `cmov` and `setcc`: with flags pending from a known operation,
    /// redoes it for the host NZCV and returns the host condition, leaving
    /// the flags pending (as `branch_if`); otherwise materializes them and
    /// returns `None`, for `condition_bit`. Uses `R` and `T3`, not `A` or
    /// `B`.
    fn host_condition_now(&mut self, cond: X86Cond) -> Emit<Option<Cond>> {
        if let Lazy::Static(p) = self.lazy
            && let Some(host) = host_condition(p, cond)
        {
            self.redo_nzcv(p)?;
            return Ok(Some(host));
        }
        self.materialize()?;
        Ok(None)
    }

    /// The host NZCV of the pending operation `p` (add, sub or logical),
    /// redone from `FA` and `FB`. Uses `R` and `T3`.
    fn redo_nzcv(&mut self, p: Pending) -> Emit {
        // The previous instruction (a jcc, setcc or cmov) left the host
        // NZCV of the same pending operation, and nothing since changed it.
        if core::mem::take(&mut self.nzcv_now) {
            self.nzcv_valid = true;
            return Ok(());
        }
        self.nzcv_valid = true;
        self.redo(p)
    }

    /// The host NZCV of the pending operation `p`, redone from its operands
    /// wherever they are. Uses `R`, `T3`, `T4`.
    fn redo(&mut self, p: Pending) -> Emit {
        if let FlagKind::FloatCompare { double } = p.kind {
            self.float_compare_nzcv(double);
            return Ok(());
        }
        if !p.plain() {
            let w = width(p.size);
            return match p.kind {
                FlagKind::Logic => {
                    let r = self.held(p.a, T4);
                    self.a.logic(Logic::Ands, false, w, ZR, r, r, Shift::Lsl, 0)
                }
                kind => {
                    let sub = kind == FlagKind::Sub;
                    let a = self.first_operand(p, T4)?;
                    self.arith_held(sub, w, R, a, p.b)
                }
            };
        }
        match p.kind {
            FlagKind::Logic if matches!(p.size, Size::B32 | Size::B64) => {
                self.a
                    .logic(Logic::Ands, false, width(p.size), ZR, FA, FA, Shift::Lsl, 0)
            }
            FlagKind::Logic => {
                self.a.mov(Width::X, R, FA);
                self.test_result(p.size)
            }
            kind if matches!(p.size, Size::B8 | Size::B16) => {
                // The host NZCV only: the operands at the top of a 32-bit
                // register, without shifting the result back.
                let top = 32 - p.size.bits();
                self.a.shift_imm(Shift::Lsl, Width::W, T3, FA, top)?;
                if kind == FlagKind::Sub {
                    self.a.subs(Width::W, R, T3, FB, Shift::Lsl, top)
                } else {
                    self.a.adds(Width::W, R, T3, FB, Shift::Lsl, top)
                }
            }
            kind => self.arith(kind == FlagKind::Sub, p.size, FA, FB),
        }
    }

    /// The host NZCV of a deferred floating-point comparison
    /// (`FlagKind::FloatCompare`), redone from the bits in `FA` and `FB`.
    /// FCMP raises IOC only where the comparison did (for a signaling
    /// NaN; FCMPE's quiet NaNs are not redone with it), and IE is sticky:
    /// MXCSR can be written only by an instruction that leaves the block,
    /// and an exit computes these flags first, so no comparison is redone
    /// after it. Uses `V1`, `V2`.
    fn float_compare_nzcv(&mut self, double: bool) {
        self.a.fmov_from_gpr(double, V1, FA);
        self.a.fmov_from_gpr(double, V2, FB);
        self.a.fcmp(double, V1, V2);
    }

    fn branch_if(&mut self, cond: X86Cond, target: Label) -> Emit {
        // Flags pending from a known operation: redo it for the host NZCV
        // and branch on the matching host condition, leaving the flags
        // pending (the exits only record `KIND`).
        if let Lazy::Static(p) = self.lazy
            && let Some(host) = host_condition(p, cond)
        {
            self.redo_nzcv(p)?;
            self.a.b_cond(host, target);
            return Ok(());
        }
        self.materialize()?;
        let (reg, bit, when_set) = self.condition_bit(cond)?;
        self.a.tbz(when_set, reg, bit, target);
        Ok(())
    }

    /// Reduces an x86 condition to one bit: returns a register, a bit
    /// position in it, and whether the condition holds when the bit is
    /// set (rather than clear). Uses `T1` for the combined conditions.
    fn condition_bit(&mut self, cond: X86Cond) -> Emit<(HostReg, u32, bool)> {
        let bit = |flag: u64| flag.trailing_zeros();
        let combine = |s: &mut Self, op: Logic, rn: HostReg, shift: Shift, amount: u32| {
            s.a.logic(op, false, Width::X, T1, rn, FLAGS, shift, amount)
        };
        Ok(match cond {
            X86Cond::O | X86Cond::No => (FLAGS, bit(FLAG_OF), cond == X86Cond::O),
            X86Cond::B | X86Cond::Ae => (FLAGS, bit(FLAG_CF), cond == X86Cond::B),
            X86Cond::E | X86Cond::Ne => (FLAGS, bit(FLAG_ZF), cond == X86Cond::E),
            X86Cond::S | X86Cond::Ns => (FLAGS, bit(FLAG_SF), cond == X86Cond::S),
            X86Cond::P | X86Cond::Np => (FLAGS, bit(FLAG_PF), cond == X86Cond::P),
            X86Cond::Be | X86Cond::A => {
                // Bit 0: CF | ZF.
                let shift = bit(FLAG_ZF) - bit(FLAG_CF);
                combine(self, Logic::Orr, FLAGS, Shift::Lsr, shift)?;
                (T1, bit(FLAG_CF), cond == X86Cond::Be)
            }
            X86Cond::L | X86Cond::Ge => {
                // Bit 7: SF ^ OF.
                let shift = bit(FLAG_OF) - bit(FLAG_SF);
                combine(self, Logic::Eor, FLAGS, Shift::Lsr, shift)?;
                (T1, bit(FLAG_SF), cond == X86Cond::L)
            }
            X86Cond::Le | X86Cond::G => {
                // Bit 7: ZF | (SF ^ OF).
                let shift = bit(FLAG_OF) - bit(FLAG_SF);
                combine(self, Logic::Eor, FLAGS, Shift::Lsr, shift)?;
                let shift = bit(FLAG_SF) - bit(FLAG_ZF);
                combine(self, Logic::Orr, T1, Shift::Lsl, shift)?;
                (T1, bit(FLAG_SF), cond == X86Cond::Le)
            }
        })
    }
}
