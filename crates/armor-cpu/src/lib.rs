//! Guest CPU state and interpreter.
//!
//! The interpreter runs guest instructions until one of them needs the
//! operating system (`syscall`), then returns so that the Linux layer can
//! handle it. An instruction either completes or leaves the state untouched;
//! the exception is a `rep` string instruction, which, like on hardware,
//! keeps the iterations completed before a fault.
//!
//! Flags are computed eagerly. See docs/architecture.md and
//! docs/tech/x86-64-flags.md.

#![forbid(unsafe_code)]
// No standard library: the Windows backend (ADR-0047) uses it too.
#![cfg_attr(not(test), no_std)]
use alloc::vec::Vec;

extern crate alloc;

mod alu;
mod avx;
mod avx_ops;
mod bmi;
pub mod cpuid;
mod f80;
mod float;
mod icache;
mod simd;
mod sse;
mod string;
mod x87;
pub mod xsave;

use core::fmt;

use armor_guest::{Access, GuestMemory, MemFault};
use armor_x86::{
    AluOp, Base, BitTestOp, BmiOp, Cond, DecodeError, FlagOp, Instruction, LoopKind,
    MAX_INSTRUCTION_LEN, Mem, Mode, Op, Operand, Reg, Segment, ShiftCount, Size, StateOp, UnaryOp,
    decode_in,
};

pub use alu::{
    FLAG_AF, FLAG_CF, FLAG_DF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF, STATUS_FLAGS, alu as alu_op,
};

/// Bit 1 of rflags, always set.
const FLAG_FIXED: u64 = 1 << 1;
/// Trap flag: single-step.
const FLAG_TF: u64 = 1 << 8;
/// The flags lahf and sahf copy: SF ZF AF PF CF.
const LAHF_FLAGS: u64 = FLAG_SF | FLAG_ZF | FLAG_AF | FLAG_PF | FLAG_CF;
/// The flags popf may change in user mode: the status flags, TF, DF, NT
/// (bit 14), AC (18) and ID (21). IF and IOPL stay as they are.
const POPF_FLAGS: u64 = STATUS_FLAGS | FLAG_TF | FLAG_DF | 1 << 14 | 1 << 18 | 1 << 21;

pub const RAX: usize = 0;
pub const RCX: usize = 1;
pub const RDX: usize = 2;
pub const RBX: usize = 3;
pub const RSP: usize = 4;
pub const RBP: usize = 5;
pub const RSI: usize = 6;
pub const RDI: usize = 7;
pub const R8: usize = 8;
pub const R9: usize = 9;
pub const R10: usize = 10;
pub const R11: usize = 11;
pub const R12: usize = 12;
pub const R13: usize = 13;
pub const R14: usize = 14;
pub const R15: usize = 15;

/// RFLAGS at process entry: reserved bit 1 and IF, as observed under
/// qemu-x86_64 (docs/tech/x86-64-flags.md).
pub const INITIAL_RFLAGS: u64 = 0x202;

/// MXCSR at process entry: all exceptions masked, round to nearest.
pub const INITIAL_MXCSR: u32 = 0x1f80;

/// MXCSR's control bits (DAZ, exception masks, rounding control, FZ); the
/// other low bits are the sticky exception flags.
pub const MXCSR_CONTROL: u32 = 0xffc0;

/// `repr(C)`: the JIT's generated code reaches the fields at fixed
/// offsets (`offset_of!`, ADR-0021).
#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(C)]
pub struct Cpu {
    /// rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8–r15.
    pub regs: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub xmm: [u128; 16],
    pub mxcsr: u32,
    pub(crate) x87: x87::X87,
    /// 64-bit mode, or compatibility mode for an i386 process (ADR-0029).
    pub mode: Mode,
    /// fs and gs selectors and the TLS entries of the GDT (compatibility
    /// mode).
    pub segments: Segments,
    /// Two 128-bit scratch registers (`Reg::Mm(8)`, `Reg::Mm(9)`) an MMX
    /// instruction runs its SSE equivalent on; zero between instructions.
    pub(crate) mm_scratch: [u128; 2],
    /// Bits 255:128 of ymm0–ymm15, whose low halves are `xmm`. Legacy SSE
    /// instructions leave them unchanged; fxsave and fxrstor do not save
    /// or load them; xsave and xrstor do when XCR0 has the AVX component
    /// (docs/tech/avx.md).
    pub ymm_hi: [u128; 16],
    /// Set while a VEX instruction runs (`avx.rs`): its memory operands
    /// need no alignment. False between instructions.
    pub(crate) vex_unaligned: bool,
    /// Decoded instructions; not architectural state.
    icache: icache::DecodeCache,
}

/// The x87 and MMX state, opaque: the JIT's lockstep saves, restores and
/// compares it now that translated MMX instructions change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FpuState(x87::X87);

impl Cpu {
    pub fn fpu_state(&self) -> FpuState {
        FpuState(self.x87)
    }

    pub fn set_fpu_state(&mut self, state: &FpuState) {
        self.x87 = state.0;
    }
}

/// Where the MMX state is in `Cpu`, for the JIT's generated code
/// (ADR-0021): an mm register is the mantissa of an x87 register, whose
/// sign and exponent become ones when it is written; an MMX instruction
/// sets TOP to 0 and marks every register valid (`x87.rs`); the two
/// scratch registers hold an MMX instruction's operands for its SSE
/// equivalent.
pub mod mmx_layout {
    use core::mem::{offset_of, size_of};

    use crate::f80::F80;
    use crate::{Cpu, x87};

    const X87: usize = offset_of!(Cpu, x87);
    const REGS: usize = X87 + x87::REGS_OFFSET;

    /// `Cpu::mm_scratch`: two `u128`s.
    pub const SCRATCH: usize = offset_of!(Cpu, mm_scratch);
    /// TOP, a byte.
    pub const TOP: usize = X87 + x87::TOP_OFFSET;
    /// The empty bits, a byte (bit i: physical register i).
    pub const EMPTY: usize = X87 + x87::EMPTY_OFFSET;

    /// mm`n`, a `u64`.
    pub const fn mantissa(n: u8) -> usize {
        REGS + n as usize * size_of::<F80>() + offset_of!(F80, mant)
    }

    /// The biased exponent of mm`n`'s register, a `u16`.
    pub const fn exponent(n: u8) -> usize {
        REGS + n as usize * size_of::<F80>() + offset_of!(F80, exp)
    }

    /// The sign of mm`n`'s register, a `bool` (one byte).
    pub const fn sign(n: u8) -> usize {
        REGS + n as usize * size_of::<F80>() + offset_of!(F80, sign)
    }
}

/// The first of the three GDT entries a thread sets with
/// `set_thread_area` (Linux x86's `GDT_ENTRY_TLS_MIN`).
pub const TLS_FIRST_ENTRY: u32 = 12;
pub const TLS_ENTRIES: usize = 3;
/// Linux's user data selector (`__USER_DS`), in ds, es and ss.
pub const USER_DS: u16 = 0x2b;
/// Linux's 32-bit user code selector (`__USER32_CS`).
pub const USER32_CS: u16 = 0x23;
/// Linux's 64-bit user code selector (`__USER_CS`).
pub const USER_CS: u16 = 0x33;

/// `user_desc`'s seg_not_present: an entry no selector may load.
pub const TLS_NOT_PRESENT: u32 = 1 << 5;

/// A TLS entry of the GDT, as `set_thread_area` received it (`struct
/// user_desc` without the entry number).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TlsEntry {
    pub base: u32,
    pub limit: u32,
    /// The `user_desc` bit fields: seg_32bit, contents, read_exec_only,
    /// limit_in_pages, seg_not_present, useable, lm.
    pub flags: u32,
}

/// The segment state a 32-bit process can see (ADR-0029). In 64-bit mode
/// only `Cpu::fs_base` and `Cpu::gs_base` matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Segments {
    pub fs: u16,
    pub gs: u16,
    /// GDT entries `TLS_FIRST_ENTRY..`, per thread as on Linux.
    pub tls: [Option<TlsEntry>; TLS_ENTRIES],
}

/// CPU exceptions an instruction can raise, besides memory faults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    /// #DE: division by zero or quotient overflow (SIGFPE).
    DivideError,
    /// #GP: privileged instruction or misaligned SSE operand (SIGSEGV).
    GeneralProtection,
    /// #BP: int3 (SIGTRAP), a trap: execution resumes after the
    /// instruction, which is this many bytes long.
    Breakpoint(u8),
    /// #DB from int1 (SIGTRAP), a trap after the one-byte instruction.
    Debug,
    /// #XM: an SSE floating-point exception unmasked in MXCSR (SIGFPE,
    /// `si_code` from MXCSR's unmasked flags). The destination is not
    /// written; MXCSR holds the flags.
    SimdFloatingPoint,
}

/// Why the interpreter stopped without a pending syscall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuError {
    /// The bytes at `rip` are not an instruction armor can execute.
    Decode {
        rip: u64,
        bytes: Vec<u8>,
        error: DecodeError,
    },
    /// The instruction at `rip` accessed memory it may not access.
    Fault { rip: u64, fault: MemFault },
    /// The instruction at `rip` raised an exception.
    Exception { rip: u64, exception: Exception },
    /// The instruction at `rip` is supported, but not this use of it.
    Unsupported { rip: u64, what: &'static str },
}

impl fmt::Display for CpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CpuError::Decode { rip, bytes, error } => {
                write!(f, "{error} at rip {rip:#x} (bytes:")?;
                for byte in bytes {
                    write!(f, " {byte:02x}")?;
                }
                write!(f, ")")
            }
            CpuError::Fault { rip, fault } => write!(f, "{fault} at rip {rip:#x}"),
            CpuError::Unsupported { rip, what } => {
                write!(f, "not supported yet: {what} at rip {rip:#x}")
            }
            CpuError::Exception { rip, exception } => {
                let what = match exception {
                    Exception::DivideError => "divide error (#DE)",
                    Exception::GeneralProtection => "general protection fault (#GP)",
                    Exception::Breakpoint(_) => "breakpoint (#BP)",
                    Exception::Debug => "debug exception (#DB)",
                    Exception::SimdFloatingPoint => "SIMD floating-point exception (#XM)",
                };
                write!(f, "{what} at rip {rip:#x}")
            }
        }
    }
}

impl core::error::Error for CpuError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    /// A `syscall` instruction completed: rip already points past it.
    Syscall,
}

/// Anything that stops an instruction from completing.
#[derive(Debug)]
enum Trap {
    Mem(MemFault),
    Exception(Exception),
    /// A valid use of an instruction that armor does not implement.
    Unsupported(&'static str),
}

impl From<MemFault> for Trap {
    fn from(fault: MemFault) -> Self {
        Trap::Mem(fault)
    }
}

type Exec<T> = Result<T, Trap>;

/// Whether CPUID declares what `op` needs, for the instructions of the
/// features declared only on request (`cpuid::Features`); without it they
/// raise #UD. The decoder decodes them whatever CPUID says.
fn declared(op: &Op) -> bool {
    use cpuid::Features;
    let needs = match op {
        Op::Xgetbv
        | Op::Xsetbv
        | Op::State {
            op: StateOp::Xsave | StateOp::Xrstor,
            ..
        } => Features::XSAVE,
        Op::Movbe { .. } => Features::MOVBE,
        Op::Avx {
            op: armor_x86::AvxOp::Fma { .. },
            ..
        } => Features::FMA,
        Op::Avx {
            op: armor_x86::AvxOp::Ph2Ps | armor_x86::AvxOp::Ps2Ph(_),
            ..
        } => Features::F16C,
        Op::Vex { avx2: true, .. } | Op::Avx { avx2: true, .. } => Features::AVX2,
        Op::Vex { .. } | Op::Avx { .. } | Op::Vzero { .. } | Op::VexMxcsr { .. } => Features::AVX,
        Op::Bmi { op, .. } => match op {
            BmiOp::Andn | BmiOp::Bextr | BmiOp::Blsi | BmiOp::Blsmsk | BmiOp::Blsr => {
                Features::BMI1
            }
            _ => Features::BMI2,
        },
        _ => return true,
    };
    cpuid::declared().contains(needs)
}

/// Whether tzcnt (`reverse` false) or lzcnt counts zeros: CPUID declares
/// BMI1 or LZCNT. Otherwise F3 0F BC and BD run as bsf and bsr.
pub fn counts_zeros(reverse: bool) -> bool {
    let feature = if reverse {
        cpuid::Features::LZCNT
    } else {
        cpuid::Features::BMI1
    };
    cpuid::declared().contains(feature)
}

impl Cpu {
    /// CPU state at process entry: every register zero except rsp.
    pub fn new(entry: u64, stack_pointer: u64) -> Cpu {
        let mut regs = [0; 16];
        regs[RSP] = stack_pointer;
        Cpu {
            regs,
            rip: entry,
            rflags: INITIAL_RFLAGS,
            fs_base: 0,
            gs_base: 0,
            xmm: [0; 16],
            mxcsr: INITIAL_MXCSR,
            x87: x87::X87::default(),
            mode: Mode::Long,
            segments: Segments::default(),
            mm_scratch: [0; 2],
            ymm_hi: [0; 16],
            vex_unaligned: false,
            icache: icache::DecodeCache::new(),
        }
    }

    /// CPU state at entry of an i386 process (compatibility mode).
    pub fn new_compat(entry: u64, stack_pointer: u64) -> Cpu {
        Cpu {
            mode: Mode::Compat32,
            ..Cpu::new(entry, stack_pointer)
        }
    }

    /// The base address of the segment `selector` names, if fs or gs may
    /// hold it: a null selector (base 0), Linux's flat user segments, or a
    /// TLS entry set with `set_thread_area`. None for anything else
    /// (#GP on hardware).
    pub fn segment_base(&self, selector: u16) -> Option<u64> {
        if selector & !3 == 0 {
            return Some(0);
        }
        // The LDT (bit 2) is not supported; user code runs at RPL 3.
        if selector & 4 != 0 || selector & 3 != 3 {
            return None;
        }
        match selector {
            USER32_CS | USER_DS | USER_CS => Some(0),
            _ => {
                let index = u32::from(selector >> 3).checked_sub(TLS_FIRST_ENTRY)?;
                let entry = self.segments.tls.get(index as usize)?.as_ref()?;
                (entry.flags & TLS_NOT_PRESENT == 0).then_some(u64::from(entry.base))
            }
        }
    }

    /// Sets TLS entry `index` of the GDT (`set_thread_area`, `clone`). A
    /// selector in fs or gs that names it takes the new base, as Linux
    /// reloads them (0 for an entry cleared or not present).
    pub fn set_tls_entry(&mut self, index: usize, entry: Option<TlsEntry>) {
        self.segments.tls[index] = entry;
        let selector = ((TLS_FIRST_ENTRY + index as u32) << 3 | 3) as u16;
        let base = self.segment_base(selector).unwrap_or(0);
        if self.segments.gs == selector {
            self.gs_base = base;
        }
        if self.segments.fs == selector {
            self.fs_base = base;
        }
    }

    /// Addresses wrap at 4 GiB in compatibility mode.
    fn address_mask(&self) -> u64 {
        match self.mode {
            Mode::Long => u64::MAX,
            Mode::Compat32 => 0xffff_ffff,
        }
    }

    /// The floating-point state Linux gives a signal handler (and a new
    /// process): x87 as after FNINIT with zeroed registers, MXCSR at its
    /// default, xmm registers zeroed (✅ Xeon X5675: a handler sees xmm
    /// 0, MXCSR 0x1f80, control word 0x37f, every tag empty, whatever the
    /// interrupted code had; the frame keeps that for sigreturn), and the
    /// upper ymm halves zeroed, the initial state of the AVX component.
    pub fn reset_fpu(&mut self) {
        self.x87 = x87::X87::default();
        self.mxcsr = INITIAL_MXCSR;
        self.xmm = [0; 16];
        self.ymm_hi = [0; 16];
    }

    /// Runs instructions until a `syscall` is reached.
    pub fn run_until_syscall(&mut self, mem: &impl GuestMemory) -> Result<(), CpuError> {
        loop {
            if self.step(mem)? == Step::Syscall {
                return Ok(());
            }
        }
    }

    /// Runs at most `budget` instructions, stopping early at a `syscall`.
    /// Returns whether a syscall is pending (`Step::Syscall`) or the budget
    /// ran out (`Step::Continue`), so that the caller can deliver signals.
    pub fn run(&mut self, mem: &impl GuestMemory, budget: u64) -> Result<Step, CpuError> {
        for _ in 0..budget {
            if self.step(mem)? == Step::Syscall {
                return Ok(Step::Syscall);
            }
        }
        Ok(Step::Continue)
    }

    /// Executes one instruction.
    pub fn step(&mut self, mem: &impl GuestMemory) -> Result<Step, CpuError> {
        let generation = mem.code_generation();
        let insn = match self.icache.get(self.rip, generation) {
            Some(insn) => insn,
            None => {
                let insn = self.fetch_and_decode(mem)?;
                self.icache.insert(self.rip, generation, insn);
                insn
            }
        };
        let rip = self.rip;
        self.execute(&insn, mem).map_err(|trap| match trap {
            Trap::Mem(fault) => CpuError::Fault { rip, fault },
            Trap::Exception(exception) => CpuError::Exception { rip, exception },
            Trap::Unsupported(what) => CpuError::Unsupported { rip, what },
        })
    }

    fn fetch_and_decode(&self, mem: &impl GuestMemory) -> Result<Instruction, CpuError> {
        let mut window = [0u8; MAX_INSTRUCTION_LEN];
        let available = mem.fetch(self.rip, &mut window);
        match decode_in(&window[..available], self.mode) {
            Ok(insn) if declared(&insn.op) => Ok(insn),
            // An instruction of a feature declared only on request
            // (`cpuid::Features`) and not declared: #UD.
            Ok(_) => Err(CpuError::Decode {
                rip: self.rip,
                bytes: window[..available].to_vec(),
                error: DecodeError::Invalid,
            }),
            // An instruction of an extension armor's CPUID does not report
            // (AVX, AVX-512, AES-NI, …): #UD, as on such a CPU (ADR-0026).
            Err(DecodeError::Unsupported(_))
                if armor_x86::length_in(&window[..available], self.mode)
                    .is_ok_and(|insn| !cpuid::reports(insn.extension)) =>
            {
                Err(CpuError::Decode {
                    rip: self.rip,
                    bytes: window[..available].to_vec(),
                    error: DecodeError::Invalid,
                })
            }
            Err(DecodeError::Truncated) if available < MAX_INSTRUCTION_LEN => {
                // The instruction runs into memory that is not executable.
                Err(CpuError::Fault {
                    rip: self.rip,
                    fault: MemFault {
                        addr: self.rip.wrapping_add(available as u64),
                        access: Access::Execute,
                    },
                })
            }
            Err(error) => Err(CpuError::Decode {
                rip: self.rip,
                bytes: window[..available].to_vec(),
                error,
            }),
        }
    }

    fn execute(&mut self, insn: &Instruction, mem: &impl GuestMemory) -> Exec<Step> {
        let next = self.rip.wrapping_add(insn.len as u64);
        let size = insn.size;
        let mut target = next;
        let mut step = Step::Continue;

        match insn.op {
            Op::Mov { dst, src } => {
                let value = self.read(src, size, next, mem)?;
                self.write(dst, size, value, next, mem)?;
            }
            Op::Extend {
                signed,
                dst,
                src,
                src_size,
            } => {
                let value = self.read(src, src_size, next, mem)?;
                let value = if signed {
                    alu::sign_extend(value, src_size)
                } else {
                    value
                };
                self.write_reg(dst, size, value);
            }
            Op::Lea { dst, src } => {
                // LEA computes the offset only: no segment base.
                let addr = self.offset(&src, next);
                self.write_reg(dst, size, addr);
            }
            // LOCK read-modify-writes of memory, and xchg with memory (locked
            // by definition): atomic, so that other processes sharing the
            // memory (ADR-0022) see each update whole.
            Op::Alu {
                op,
                dst: Operand::Mem(m),
                src,
            } if insn.lock => {
                let b = self.read(src, size, next, mem)?;
                let carry = self.flag(FLAG_CF);
                let old =
                    self.locked(&m, size, next, mem, |a| alu::alu(op, size, a, b, carry).0)?;
                self.set_status(alu::alu(op, size, old, b, carry).1);
            }
            Op::Unary {
                op,
                dst: Operand::Mem(m),
            } if insn.lock => {
                let old = self.locked(&m, size, next, mem, |a| match op {
                    UnaryOp::Inc => a.wrapping_add(1),
                    UnaryOp::Dec => a.wrapping_sub(1),
                    UnaryOp::Not => !a,
                    UnaryOp::Neg => a.wrapping_neg(),
                })?;
                match op {
                    UnaryOp::Inc | UnaryOp::Dec => {
                        let alu_op = if op == UnaryOp::Inc {
                            AluOp::Add
                        } else {
                            AluOp::Sub
                        };
                        let (_, f) = alu::alu(alu_op, size, old, 1, false);
                        self.set_status((f & !FLAG_CF) | (self.rflags & FLAG_CF));
                    }
                    UnaryOp::Not => {}
                    UnaryOp::Neg => self.set_status(alu::alu(AluOp::Sub, size, 0, old, false).1),
                }
            }
            Op::Cmpxchg {
                dst: Operand::Mem(m),
                src,
            } if insn.lock => {
                let accumulator = self.read_reg(Reg::Gpr(RAX as u8), size);
                let value = self.read_reg(src, size);
                let old = self.locked(&m, size, next, mem, |current| {
                    if current == accumulator {
                        value
                    } else {
                        current
                    }
                })?;
                if old != accumulator {
                    self.write_reg(Reg::Gpr(RAX as u8), size, old);
                }
                self.set_status(alu::alu(AluOp::Cmp, size, accumulator, old, false).1);
            }
            Op::Xadd {
                dst: Operand::Mem(m),
                src,
            } if insn.lock => {
                let addend = self.read_reg(src, size);
                let old =
                    self.locked(&m, size, next, mem, |current| current.wrapping_add(addend))?;
                self.write_reg(src, size, old);
                self.set_status(alu::alu(AluOp::Add, size, old, addend, false).1);
            }
            Op::Xchg {
                a: Operand::Mem(m),
                b: Operand::Reg(reg),
            }
            | Op::Xchg {
                a: Operand::Reg(reg),
                b: Operand::Mem(m),
            } => {
                let value = self.read_reg(reg, size);
                let old = self.locked(&m, size, next, mem, |_| value)?;
                self.write_reg(reg, size, old);
            }
            Op::Alu { op, dst, src } => {
                let a = self.read(dst, size, next, mem)?;
                let b = self.read(src, size, next, mem)?;
                let (result, flags) = alu::alu(op, size, a, b, self.flag(FLAG_CF));
                if op != AluOp::Cmp {
                    self.write(dst, size, result, next, mem)?;
                }
                self.set_status(flags);
            }
            Op::Test { a, b } => {
                let a = self.read(a, size, next, mem)?;
                let b = self.read(b, size, next, mem)?;
                let (_, flags) = alu::alu(AluOp::And, size, a, b, false);
                self.set_status(flags);
            }
            Op::Unary { op, dst } => {
                let a = self.read(dst, size, next, mem)?;
                let (result, flags) = match op {
                    UnaryOp::Inc | UnaryOp::Dec => {
                        let alu_op = if op == UnaryOp::Inc {
                            AluOp::Add
                        } else {
                            AluOp::Sub
                        };
                        let (r, f) = alu::alu(alu_op, size, a, 1, false);
                        // INC and DEC leave CF unchanged.
                        (r, Some((f & !FLAG_CF) | (self.rflags & FLAG_CF)))
                    }
                    UnaryOp::Not => (!a & size.mask(), None),
                    UnaryOp::Neg => {
                        let (r, f) = alu::alu(AluOp::Sub, size, 0, a, false);
                        (r, Some(f))
                    }
                };
                self.write(dst, size, result, next, mem)?;
                if let Some(flags) = flags {
                    self.set_status(flags);
                }
            }
            Op::Shift { op, dst, count } => {
                let value = self.read(dst, size, next, mem)?;
                let count = match count {
                    ShiftCount::One => 1,
                    ShiftCount::Cl => self.regs[RCX] & 0xff,
                    ShiftCount::Imm(n) => u64::from(n),
                };
                if let Some(shifted) = alu::shift(op, size, value, count, self.rflags) {
                    self.write(dst, size, shifted.result, next, mem)?;
                    self.set_status(shifted.flags);
                }
            }
            Op::DoubleShift {
                left,
                dst,
                src,
                count,
            } => {
                let value = self.read(dst, size, next, mem)?;
                let fill = self.read_reg(src, size);
                let count = match count {
                    ShiftCount::One => 1,
                    ShiftCount::Cl => self.regs[RCX] & 0xff,
                    ShiftCount::Imm(n) => u64::from(n),
                };
                if let Some(shifted) = alu::double_shift(left, size, value, fill, count) {
                    self.write(dst, size, shifted.result, next, mem)?;
                    self.set_status(shifted.flags);
                }
            }
            Op::MulWide { signed, src } => {
                let b = self.read(src, size, next, mem)?;
                self.mul_wide(signed, size, b);
            }
            Op::Imul { dst, src, factor } => {
                let b = self.read(src, size, next, mem)?;
                let a = match factor {
                    Some(imm) => imm as u64,
                    None => self.read_reg(dst, size),
                };
                let full = signed(a, size) * signed(b, size);
                let result = full as u64 & size.mask();
                let overflow = signed(result, size) != full;
                self.write_reg(dst, size, result);
                self.set_mul_flags(result, size, overflow);
            }
            Op::Div { signed, src } => {
                let divisor = self.read(src, size, next, mem)?;
                self.divide(signed, size, divisor)?;
            }
            Op::SignExtendAccumulator => {
                let half = half_size(size);
                let value = alu::sign_extend(self.regs[RAX] & half.mask(), half);
                self.write_reg(Reg::Gpr(RAX as u8), size, value);
            }
            Op::SignExtendIntoRdx => {
                let negative = signed(self.regs[RAX], size) < 0;
                let value = if negative { u64::MAX } else { 0 };
                self.write_reg(Reg::Gpr(RDX as u8), size, value);
            }
            Op::Cmov { cond, dst, src } => {
                // The source is read (and may fault) whatever the condition;
                // a 32-bit destination is zero-extended even when not moved.
                let value = self.read(src, size, next, mem)?;
                let value = if self.condition(cond) {
                    value
                } else {
                    self.read_reg(dst, size)
                };
                self.write_reg(dst, size, value);
            }
            Op::Setcc { cond, dst } => {
                let value = u64::from(self.condition(cond));
                self.write(dst, Size::B8, value, next, mem)?;
            }
            Op::Xchg { a, b } => {
                let va = self.read(a, size, next, mem)?;
                let vb = self.read(b, size, next, mem)?;
                // Memory first, so that a fault leaves registers untouched.
                if b.is_mem() {
                    self.write(b, size, va, next, mem)?;
                    self.write(a, size, vb, next, mem)?;
                } else {
                    self.write(a, size, vb, next, mem)?;
                    self.write(b, size, va, next, mem)?;
                }
            }
            Op::Cmpxchg { dst, src } => {
                let accumulator = self.read_reg(Reg::Gpr(RAX as u8), size);
                let current = self.read(dst, size, next, mem)?;
                let (_, flags) = alu::alu(AluOp::Cmp, size, accumulator, current, false);
                if accumulator == current {
                    let value = self.read_reg(src, size);
                    self.write(dst, size, value, next, mem)?;
                } else {
                    // A memory destination is written back even on failure.
                    if dst.is_mem() {
                        self.write(dst, size, current, next, mem)?;
                    }
                    self.write_reg(Reg::Gpr(RAX as u8), size, current);
                }
                self.set_status(flags);
            }
            Op::Xadd { dst, src } => {
                let current = self.read(dst, size, next, mem)?;
                let addend = self.read_reg(src, size);
                let (sum, flags) = alu::alu(AluOp::Add, size, current, addend, false);
                // TEMP := SRC + DEST; SRC := DEST; DEST := TEMP.
                if dst.is_mem() {
                    self.write(dst, size, sum, next, mem)?;
                    self.write_reg(src, size, current);
                } else {
                    self.write_reg(src, size, current);
                    self.write(dst, size, sum, next, mem)?;
                }
                self.set_status(flags);
            }
            Op::BitTest { op, dst, offset } => {
                self.bit_test(op, dst, offset, size, insn.lock, next, mem)?
            }
            Op::Popcnt { dst, src } => {
                let value = self.read(src, size, next, mem)? & size.mask();
                self.write_reg(dst, size, u64::from(value.count_ones()));
                self.rflags &= !STATUS_FLAGS;
                if value == 0 {
                    self.rflags |= FLAG_ZF;
                }
            }
            Op::Crc32 { dst, src, src_size } => {
                let data = self.read(src, src_size, next, mem)?;
                let crc = self.read_reg(dst, Size::B32) as u32;
                let crc = simd::crc32c(crc, data, src_size.bytes());
                self.write_reg(dst, size, u64::from(crc));
            }
            Op::BitScan {
                reverse,
                dst,
                src,
                counts_zeros: true,
            } if counts_zeros(reverse) => {
                let value = self.read(src, size, next, mem)?;
                let (count, flags) = bmi::count_zeros(reverse, size, value);
                self.write_reg(dst, size, count);
                self.rflags = (self.rflags & !(FLAG_CF | FLAG_ZF)) | flags;
            }
            Op::BitScan {
                reverse, dst, src, ..
            } => {
                let value = self.read(src, size, next, mem)? & size.mask();
                if value == 0 {
                    // The destination is left unchanged (undefined per Intel).
                    self.rflags |= FLAG_ZF;
                } else {
                    let index = if reverse {
                        63 - value.leading_zeros()
                    } else {
                        value.trailing_zeros()
                    };
                    self.write_reg(dst, size, u64::from(index));
                    self.rflags &= !FLAG_ZF;
                }
            }
            Op::Movbe { dst, src } => {
                let value = self.read(src, size, next, mem)?;
                let swapped = match size {
                    Size::B16 => u64::from((value as u16).swap_bytes()),
                    Size::B32 => u64::from((value as u32).swap_bytes()),
                    _ => value.swap_bytes(),
                };
                self.write(dst, size, swapped, next, mem)?;
            }
            Op::Bmi {
                op: BmiOp::Mulx,
                dst,
                src,
                vvvv,
            } => {
                let factor = self.read(src, size, next, mem)?;
                let rdx = self.read_reg(Reg::Gpr(RDX as u8), size);
                let product = u128::from(rdx) * u128::from(factor);
                // The low half first: with the same register for both,
                // the high half is what stays (SDM).
                self.write_reg(vvvv, size, product as u64 & size.mask());
                self.write_reg(dst, size, (product >> size.bits()) as u64);
            }
            Op::Bmi { op, dst, src, vvvv } => {
                let value = self.read(src, size, next, mem)?;
                let (result, flags) = bmi::bmi(op, size, value, self.read_reg(vvvv, size));
                self.write_reg(dst, size, result);
                if let Some(flags) = flags {
                    self.set_status(flags);
                }
            }
            Op::Bswap(reg) => {
                let value = self.read_reg(reg, size);
                let swapped = match size {
                    Size::B32 => u64::from((value as u32).swap_bytes()),
                    _ => value.swap_bytes(),
                };
                self.write_reg(reg, size, swapped);
            }
            // Stack operations: `size` is the stack width (8 bytes, 4 in
            // compatibility mode).
            Op::Push(src) => {
                let value = self.read(src, size, next, mem)?;
                self.push(value, size, mem)?;
            }
            Op::Pop(dst) => {
                let value = self.load(self.regs[RSP], size, mem)?;
                // A memory destination based on rsp uses the incremented rsp.
                let old_rsp = self.regs[RSP];
                self.regs[RSP] = self.stack_offset(old_rsp, size.bytes() as u64);
                if let Err(trap) = self.write(dst, size, value, next, mem) {
                    self.regs[RSP] = old_rsp;
                    return Err(trap);
                }
            }
            Op::Pushf => self.push(self.rflags, size, mem)?,
            Op::Popf => {
                let value = self.load(self.regs[RSP], size, mem)?;
                if value & FLAG_TF != 0 {
                    // Single-stepping would need a SIGTRAP after the next
                    // instruction.
                    return Err(Trap::Unsupported("popf setting TF (single-step)"));
                }
                self.regs[RSP] = self.stack_offset(self.regs[RSP], size.bytes() as u64);
                self.rflags = (self.rflags & !POPF_FLAGS) | (value & POPF_FLAGS);
            }
            Op::Lahf => {
                let low = (self.rflags & LAHF_FLAGS) | FLAG_FIXED;
                self.write_reg(Reg::HighByte(RAX as u8), Size::B8, low);
            }
            Op::Sahf => {
                let ah = self.read_reg(Reg::HighByte(RAX as u8), Size::B8);
                self.rflags = (self.rflags & !LAHF_FLAGS) | (ah & LAHF_FLAGS);
            }
            Op::Enter { alloc, level } => self.enter(alloc, level, size, mem)?,
            Op::Xlat(segment) => {
                let offset = self.regs[RBX].wrapping_add(self.regs[RAX] & 0xff);
                let base = match segment {
                    Segment::Default => 0,
                    Segment::Fs => self.fs_base,
                    Segment::Gs => self.gs_base,
                };
                let addr = base.wrapping_add(offset) & self.address_mask();
                let value = self.load(addr, Size::B8, mem)?;
                self.write_reg(Reg::Gpr(RAX as u8), Size::B8, value);
            }
            Op::CmpxchgWide(m) => self.cmpxchg_wide(&m, size, next, mem)?,
            Op::Leave => {
                let rbp = self.regs[RBP];
                let value = self.load(rbp, size, mem)?;
                self.regs[RSP] = self.stack_offset(rbp, size.bytes() as u64);
                self.regs[RBP] = value;
            }
            Op::Jmp { rel } => target = next.wrapping_add(rel as u64),
            Op::JmpIndirect(src) => target = self.read(src, size, next, mem)?,
            Op::Jcc { cond, rel } => {
                if self.condition(cond) {
                    target = next.wrapping_add(rel as u64);
                }
            }
            Op::Loop { kind, rel } => {
                // The counter is ecx with 32-bit addresses (compatibility
                // mode).
                let counter = if insn.addr32 { Size::B32 } else { Size::B64 };
                let taken = if kind == LoopKind::Jrcxz {
                    self.read_reg(Reg::Gpr(RCX as u8), counter) == 0
                } else {
                    let count = self.regs[RCX].wrapping_sub(1);
                    self.write_reg(Reg::Gpr(RCX as u8), counter, count);
                    let zf = self.flag(FLAG_ZF);
                    self.read_reg(Reg::Gpr(RCX as u8), counter) != 0
                        && match kind {
                            LoopKind::Loope => zf,
                            LoopKind::Loopne => !zf,
                            _ => true,
                        }
                };
                if taken {
                    target = next.wrapping_add(rel as u64);
                }
            }
            Op::Call { rel } => {
                self.push(next, size, mem)?;
                target = next.wrapping_add(rel as u64);
            }
            Op::CallIndirect(src) => {
                let destination = self.read(src, size, next, mem)?;
                self.push(next, size, mem)?;
                target = destination;
            }
            Op::Ret { pop } => {
                target = self.load(self.regs[RSP], size, mem)?;
                let released = size.bytes() as u64 + u64::from(pop);
                self.regs[RSP] = self.stack_offset(self.regs[RSP], released);
            }
            Op::String { op, rep } => self.string(op, rep, size, insn.addr32, mem)?,
            Op::Flag(op) => match op {
                FlagOp::Clc => self.rflags &= !FLAG_CF,
                FlagOp::Stc => self.rflags |= FLAG_CF,
                FlagOp::Cmc => self.rflags ^= FLAG_CF,
                FlagOp::Cld => self.rflags &= !FLAG_DF,
                FlagOp::Std => self.rflags |= FLAG_DF,
            },
            Op::Cpuid => {
                let [a, b, c, d] = cpuid::query(self.regs[RAX] as u32, self.regs[RCX] as u32);
                self.regs[RAX] = u64::from(a);
                self.regs[RBX] = u64::from(b);
                self.regs[RCX] = u64::from(c);
                self.regs[RDX] = u64::from(d);
            }
            Op::Rdtsc => {
                let tsc = mem.time_stamp_counter();
                self.regs[RAX] = tsc & 0xffff_ffff;
                self.regs[RDX] = tsc >> 32;
            }
            // The i386 system call (ADR-0029): the Linux layer reads the
            // number and arguments from eax, ebx, ecx, edx, esi, edi, ebp.
            Op::Int80 => step = Step::Syscall,
            Op::ReadSegment { segment, dst } => {
                let selector = match segment {
                    Segment::Fs => self.segments.fs,
                    _ => self.segments.gs,
                };
                self.write(dst, size, u64::from(selector), next, mem)?;
            }
            Op::LoadSegment { segment, src } => {
                let selector = self.read(src, Size::B16, next, mem)? as u16;
                match segment {
                    Segment::Fs | Segment::Gs => {
                        let base = self
                            .segment_base(selector)
                            .ok_or(Trap::Exception(Exception::GeneralProtection))?;
                        if segment == Segment::Fs {
                            (self.segments.fs, self.fs_base) = (selector, base);
                        } else {
                            (self.segments.gs, self.gs_base) = (selector, base);
                        }
                    }
                    // ds, es, ss: Linux's flat data segment, the only one a
                    // program has a reason to load.
                    Segment::Default if selector == USER_DS => {}
                    Segment::Default => {
                        return Err(Trap::Unsupported(
                            "loading ds, es or ss with another selector",
                        ));
                    }
                }
            }
            Op::Syscall => {
                self.regs[RCX] = next;
                self.regs[R11] = self.rflags;
                step = Step::Syscall;
            }
            Op::Nop => {}
            // A full barrier, whatever the kind: lfence and sfence order
            // less on x86, and fences are rare (ADR-0023).
            Op::Fence => core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst),
            Op::Privileged | Op::Xsetbv => {
                return Err(Trap::Exception(Exception::GeneralProtection));
            }
            // XCR0 for ecx = 0; XCR1 (XINUSE) only with XGETBV_ECX_1,
            // which armor does not declare: #GP for any other ecx.
            Op::Xgetbv => {
                if self.regs[RCX] as u32 != 0 {
                    return Err(Trap::Exception(Exception::GeneralProtection));
                }
                let xcr0 = cpuid::xcr0(cpuid::declared());
                self.regs[RAX] = xcr0 & 0xffff_ffff;
                self.regs[RDX] = xcr0 >> 32;
            }
            Op::Int3 => return Err(Trap::Exception(Exception::Breakpoint(insn.len as u8))),
            Op::Int1 => return Err(Trap::Exception(Exception::Debug)),
            Op::Sse { op, dst, src } => self.sse(op, dst, src, next, mem)?,
            Op::Vex { .. } => self.vex(&insn.op, next, mem)?,
            Op::Avx { .. } => self.avx(&insn.op, next, mem)?,
            Op::Vzero { all } => self.vzero(all),
            Op::VexMxcsr { store, mem: m } => {
                let op = if store {
                    StateOp::Stmxcsr
                } else {
                    StateOp::Ldmxcsr
                };
                self.state(op, m, next, mem)?;
            }
            Op::State { op, mem: m } => self.state(op, m, next, mem)?,
            Op::X87 { op, opcode } => self.x87(op, opcode, next, mem)?,
        }
        self.rip = target & self.address_mask();
        Ok(step)
    }

    /// bt/bts/btr/btc. CF gets the selected bit; ZF is unaffected and the
    /// other status flags are undefined (left unchanged).
    #[allow(clippy::too_many_arguments)]
    fn bit_test(
        &mut self,
        op: BitTestOp,
        dst: Operand,
        offset: Operand,
        size: Size,
        locked: bool,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        let bits = u64::from(size.bits());
        // An immediate offset is taken modulo the operand size; so is a
        // register offset with a register operand. With a memory operand, a
        // register offset is signed and moves the address in whole words.
        let (dst, bit) = match (dst, offset) {
            (Operand::Mem(m), Operand::Reg(reg)) => {
                let offset = signed(self.read_reg(reg, size), size);
                let words = offset.div_euclid(i128::from(bits)) as i64;
                let bit = offset.rem_euclid(i128::from(bits)) as u64;
                let moved = Mem {
                    disp: m.disp.wrapping_add(words.wrapping_mul(size.bytes() as i64)),
                    ..m
                };
                (Operand::Mem(moved), bit)
            }
            (dst, offset) => (dst, self.read(offset, size, next, mem)? % bits),
        };
        let change = |value: u64| match op {
            BitTestOp::Bt => value,
            BitTestOp::Bts => value | (1 << bit),
            BitTestOp::Btr => value & !(1 << bit),
            BitTestOp::Btc => value ^ (1 << bit),
        };
        let value = match dst {
            // LOCK: atomic, like the other locked read-modify-writes.
            Operand::Mem(m) if locked => self.locked(&m, size, next, mem, change)?,
            _ => {
                let value = self.read(dst, size, next, mem)?;
                if op != BitTestOp::Bt {
                    self.write(dst, size, change(value), next, mem)?;
                }
                value
            }
        };
        let selected = (value >> bit) & 1 != 0;
        self.rflags = (self.rflags & !FLAG_CF) | u64::from(selected);
        Ok(())
    }

    /// One-operand MUL/IMUL: the double-width product goes to rdx:rax
    /// (ax for bytes).
    fn mul_wide(&mut self, is_signed: bool, size: Size, b: u64) {
        let a = self.regs[RAX] & size.mask();
        let bits = size.bits();
        let (low, high, overflow) = if is_signed {
            let full = signed(a, size) * signed(b, size);
            let low = full as u64 & size.mask();
            let high = (full >> bits) as u64 & size.mask();
            (low, high, signed(low, size) != full)
        } else {
            let full = u128::from(a) * u128::from(b & size.mask());
            let low = full as u64 & size.mask();
            let high = (full >> bits) as u64 & size.mask();
            (low, high, high != 0)
        };
        if size == Size::B8 {
            self.write_reg(Reg::Gpr(RAX as u8), Size::B16, low | (high << 8));
        } else {
            self.write_reg(Reg::Gpr(RAX as u8), size, low);
            self.write_reg(Reg::Gpr(RDX as u8), size, high);
        }
        self.set_mul_flags(low, size, overflow);
    }

    /// CF = OF = overflow. SF, ZF, AF, PF are undefined: set from the low
    /// half of the result, AF cleared.
    fn set_mul_flags(&mut self, low: u64, size: Size, overflow: bool) {
        let mut flags = alu::result_flags(low, size);
        if overflow {
            flags |= FLAG_CF | FLAG_OF;
        }
        self.set_status(flags);
    }

    /// DIV/IDIV of rdx:rax (ax for bytes) by `divisor`. Flags are undefined
    /// and left unchanged.
    fn divide(&mut self, is_signed: bool, size: Size, divisor: u64) -> Exec<()> {
        let divide_error = || Trap::Exception(Exception::DivideError);
        let divisor = divisor & size.mask();
        if divisor == 0 {
            return Err(divide_error());
        }
        let bits = size.bits();
        let (high, low) = if size == Size::B8 {
            ((self.regs[RAX] >> 8) & 0xff, self.regs[RAX] & 0xff)
        } else {
            (self.regs[RDX] & size.mask(), self.regs[RAX] & size.mask())
        };
        let dividend = (u128::from(high) << bits) | u128::from(low);
        let (quotient, remainder) = if is_signed {
            // Sign-extend the (2 * bits)-wide dividend.
            let shift = 128 - 2 * bits;
            let n = ((dividend << shift) as i128) >> shift;
            let d = signed(divisor, size);
            let (q, r) = (n / d, n % d);
            let limit = 1i128 << (bits - 1);
            if q < -limit || q >= limit {
                return Err(divide_error());
            }
            (q as u64 & size.mask(), r as u64 & size.mask())
        } else {
            let q = dividend / u128::from(divisor);
            if q > u128::from(size.mask()) {
                return Err(divide_error());
            }
            (q as u64, (dividend % u128::from(divisor)) as u64)
        };
        if size == Size::B8 {
            self.write_reg(Reg::Gpr(RAX as u8), Size::B16, quotient | (remainder << 8));
        } else {
            self.write_reg(Reg::Gpr(RAX as u8), size, quotient);
            self.write_reg(Reg::Gpr(RDX as u8), size, remainder);
        }
        Ok(())
    }

    /// enter, as in the Intel pseudocode for 64-bit mode. Registers change
    /// only once every stack access succeeded.
    /// `size` is the stack width.
    fn enter(&mut self, alloc: u16, level: u8, size: Size, mem: &impl GuestMemory) -> Exec<()> {
        let level = level % 32;
        let n = size.bytes();
        let mask = self.address_mask();
        let down = move |addr: u64, by: u64| addr.wrapping_sub(by) & mask;
        let mut rsp = down(self.regs[RSP], n as u64);
        mem.write(rsp, &self.regs[RBP].to_le_bytes()[..n])?;
        let frame = rsp;
        if level > 0 {
            let mut rbp = self.regs[RBP];
            for _ in 1..level {
                rbp = down(rbp, n as u64);
                let link = self.load(rbp, size, mem)?;
                rsp = down(rsp, n as u64);
                mem.write(rsp, &link.to_le_bytes()[..n])?;
            }
            rsp = down(rsp, n as u64);
            mem.write(rsp, &frame.to_le_bytes()[..n])?;
        }
        self.regs[RBP] = frame;
        self.regs[RSP] = down(rsp, u64::from(alloc));
        Ok(())
    }

    /// cmpxchg8b (`size` 32) and cmpxchg16b (`size` 64): compare rdx:rax
    /// with the operand; equal: ZF = 1 and the operand = rcx:rbx; else
    /// ZF = 0 and rdx:rax = the operand. The operand is written back in
    /// both cases, as on hardware (a read-only page faults either way).
    /// Always atomic (with or without LOCK, which x86 needs for atomicity
    /// and armor does not distinguish): a host compare-and-swap of 8 or 16
    /// bytes (`GuestMemory::atomic_update`, `compare_exchange_16`).
    fn cmpxchg_wide(&mut self, m: &Mem, size: Size, next: u64, mem: &impl GuestMemory) -> Exec<()> {
        let addr = self.address(m, next);
        let half = size.bytes();
        if size == Size::B64 && !addr.is_multiple_of(16) {
            return Err(Trap::Exception(Exception::GeneralProtection));
        }
        let pair = |low: u64, high: u64| u128::from(low) | (u128::from(high) << (8 * half));
        let reg = |n: usize| self.read_reg(Reg::Gpr(n as u8), size);
        let (expected, new) = (pair(reg(RAX), reg(RDX)), pair(reg(RBX), reg(RCX)));
        let old = if size == Size::B64 {
            mem.compare_exchange_16(addr, expected, new)?
        } else {
            let old = mem.atomic_update(addr, 8, &mut |current| {
                if u128::from(current) == expected {
                    new as u64
                } else {
                    current
                }
            })?;
            u128::from(old)
        };
        let equal = old == expected;
        let (low, high) = (
            (old as u64) & size.mask(),
            ((old >> (8 * half)) as u64) & size.mask(),
        );
        if equal {
            self.rflags |= FLAG_ZF;
        } else {
            self.rflags &= !FLAG_ZF;
            self.write_reg(Reg::Gpr(RAX as u8), size, low);
            self.write_reg(Reg::Gpr(RDX as u8), size, high);
        }
        Ok(())
    }

    /// A locked read-modify-write of memory operand `m`
    /// (`GuestMemory::atomic_update`): `update` maps the old value to the
    /// new one; returns the old value.
    fn locked(
        &self,
        m: &Mem,
        size: Size,
        next: u64,
        mem: &impl GuestMemory,
        mut update: impl FnMut(u64) -> u64,
    ) -> Exec<u64> {
        let addr = self.address(m, next);
        let mask = size.mask();
        Ok(mem.atomic_update(addr, size.bytes(), &mut |old| update(old & mask) & mask)?)
    }

    /// Pushes the low `size` bytes of `value` (the stack width).
    fn push(&mut self, value: u64, size: Size, mem: &impl GuestMemory) -> Exec<()> {
        let n = size.bytes();
        let rsp = self.stack_offset(self.regs[RSP], (n as u64).wrapping_neg());
        mem.write(rsp, &value.to_le_bytes()[..n])?;
        self.regs[RSP] = rsp;
        Ok(())
    }

    /// `addr + delta` on the stack: esp wraps at 4 GiB in compatibility
    /// mode.
    fn stack_offset(&self, addr: u64, delta: u64) -> u64 {
        addr.wrapping_add(delta) & self.address_mask()
    }

    fn flag(&self, flag: u64) -> bool {
        self.rflags & flag != 0
    }

    fn condition(&self, cond: Cond) -> bool {
        let (cf, zf, sf, of, pf) = (
            self.flag(FLAG_CF),
            self.flag(FLAG_ZF),
            self.flag(FLAG_SF),
            self.flag(FLAG_OF),
            self.flag(FLAG_PF),
        );
        match cond {
            Cond::O => of,
            Cond::No => !of,
            Cond::B => cf,
            Cond::Ae => !cf,
            Cond::E => zf,
            Cond::Ne => !zf,
            Cond::Be => cf || zf,
            Cond::A => !cf && !zf,
            Cond::S => sf,
            Cond::Ns => !sf,
            Cond::P => pf,
            Cond::Np => !pf,
            Cond::L => sf != of,
            Cond::Ge => sf == of,
            Cond::Le => zf || sf != of,
            Cond::G => !zf && sf == of,
        }
    }

    fn set_status(&mut self, flags: u64) {
        self.rflags = (self.rflags & !STATUS_FLAGS) | (flags & STATUS_FLAGS);
    }

    /// `base + index * scale + disp`, without segment base, truncated to 32
    /// bits under the address-size prefix.
    fn offset(&self, mem: &Mem, next: u64) -> u64 {
        let base = match mem.base {
            Base::None => 0,
            Base::Gpr(n) => self.regs[usize::from(n)],
            Base::Rip => next,
        };
        let index = mem.index.map_or(0, |n| {
            self.regs[usize::from(n)].wrapping_mul(u64::from(mem.scale))
        });
        let offset = base.wrapping_add(index).wrapping_add(mem.disp as u64);
        if mem.addr32 {
            offset & 0xffff_ffff
        } else {
            offset
        }
    }

    /// The linear address: in compatibility mode the sum with the segment
    /// base wraps at 4 GiB too (i386 glibc reaches its TLS with negative
    /// offsets from gs).
    fn address(&self, mem: &Mem, next: u64) -> u64 {
        let segment_base = match mem.segment {
            Segment::Default => 0,
            Segment::Fs => self.fs_base,
            Segment::Gs => self.gs_base,
        };
        segment_base.wrapping_add(self.offset(mem, next)) & self.address_mask()
    }

    fn read(&self, operand: Operand, size: Size, next: u64, mem: &impl GuestMemory) -> Exec<u64> {
        Ok(match operand {
            Operand::Reg(reg) => self.read_reg(reg, size),
            Operand::Mem(m) => self.load(self.address(&m, next), size, mem)?,
            Operand::Imm(imm) => imm as u64 & size.mask(),
        })
    }

    fn write(
        &mut self,
        operand: Operand,
        size: Size,
        value: u64,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        match operand {
            Operand::Reg(reg) => self.write_reg(reg, size, value),
            Operand::Mem(m) => {
                let addr = self.address(&m, next);
                mem.write(addr, &value.to_le_bytes()[..size.bytes()])?;
            }
            Operand::Imm(_) => unreachable!("the decoder never produces an immediate destination"),
        }
        Ok(())
    }

    fn load(&self, addr: u64, size: Size, mem: &impl GuestMemory) -> Exec<u64> {
        let mut bytes = [0u8; 8];
        mem.read(addr, &mut bytes[..size.bytes()])?;
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn read_reg(&self, reg: Reg, size: Size) -> u64 {
        match reg {
            Reg::Gpr(n) => self.regs[usize::from(n)] & size.mask(),
            Reg::HighByte(n) => (self.regs[usize::from(n)] >> 8) & 0xff,
            Reg::Xmm(n) => self.xmm[usize::from(n)] as u64 & size.mask(),
            Reg::Mm(n) => self.mm(n) as u64 & size.mask(),
        }
    }

    /// mm0–mm7 (the significands of the x87 registers), or a scratch
    /// register (8, 9).
    pub(crate) fn mm(&self, n: u8) -> u128 {
        match n {
            0..8 => u128::from(self.x87.mm(n)),
            _ => self.mm_scratch[usize::from(n - 8)],
        }
    }

    pub(crate) fn set_mm(&mut self, n: u8, value: u128) {
        match n {
            0..8 => self.x87.set_mm(n, value as u64),
            _ => self.mm_scratch[usize::from(n - 8)] = value,
        }
    }

    /// 32-bit writes zero-extend to 64 bits; 8- and 16-bit writes preserve
    /// the rest of the register.
    pub fn write_reg(&mut self, reg: Reg, size: Size, value: u64) {
        match reg {
            Reg::Gpr(n) => {
                let r = &mut self.regs[usize::from(n)];
                *r = match size {
                    Size::B64 => value,
                    Size::B32 => value & 0xffff_ffff,
                    Size::B16 | Size::B8 => (*r & !size.mask()) | (value & size.mask()),
                };
            }
            Reg::HighByte(n) => {
                let r = &mut self.regs[usize::from(n)];
                *r = (*r & !0xff00) | ((value & 0xff) << 8);
            }
            Reg::Xmm(_) => unreachable!("xmm registers are written by the SSE executor"),
            // movd zero-extends: the value has `size` bits.
            Reg::Mm(n) => self.set_mm(n, u128::from(value & size.mask())),
        }
    }
}

/// `value` (of `size` bits) as a signed number.
fn signed(value: u64, size: Size) -> i128 {
    i128::from(alu::sign_extend(value & size.mask(), size) as i64)
}

/// The source size of cbw/cwde/cdqe.
fn half_size(size: Size) -> Size {
    match size {
        Size::B64 => Size::B32,
        Size::B32 => Size::B16,
        _ => Size::B8,
    }
}

#[cfg(test)]
mod tests;
