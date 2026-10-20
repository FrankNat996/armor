//! The x87 FPU: register stack, control and status words, and the
//! instructions of `X87Op` (ADR-0016). The arithmetic is in `f80.rs`.
//!
//! Supported control word: every exception masked (the Linux default), any
//! rounding mode, any precision (24, 53 or 64 bits). Unmasked exceptions
//! stop armor with "not supported yet" (`check_control_word`). With
//! exceptions masked, faults produce their masked responses (Intel SDM vol. 1, 8.5): an
//! invalid operation or a stack fault gives the indefinite QNaN, and so on.
use alloc::vec::Vec;

use armor_guest::GuestMemory;
use armor_x86::{X87Arith, X87Constant, X87Format, X87Op, X87Operand, X87Transcendental};

use crate::f80::{self, Compare, Constant, F80, FLAG_INVALID, Mode, Out, Rounding};
use crate::{Cpu, Exec, FLAG_CF, FLAG_PF, FLAG_ZF, RAX, STATUS_FLAGS, Trap, alu::sign_extend};

/// Control word after FNINIT (and at process start on Linux).
pub const INITIAL_CONTROL_WORD: u16 = 0x037f;

/// Exception masks (bits 0..5): armor supports only all of them set.
const EXCEPTION_MASKS: u16 = 0x3f;
/// Precision control (bits 8, 9): 00 = 24 bits, 10 = 53, 11 = 64;
/// 01 is reserved.
const PRECISION_SHIFT: u16 = 8;
const PRECISION_RESERVED: u16 = 1;

// Status word bits.
const STACK_FAULT: u16 = 0x40;
const C0: u16 = 0x100;
const C1: u16 = 0x200;
const C2: u16 = 0x400;
const C3: u16 = 0x4000;
const EXCEPTION_FLAGS: u16 = 0x3f;
const TOP_SHIFT: u16 = 11;

/// The 32-bit environment of fnstenv / fldenv (Intel SDM vol. 1, 8.1.10),
/// as an Intel Xeon X5675 writes it (docs/testing.md, hardware oracle):
/// control, status and tag words at 0, 4 and 8, each followed by ffff;
/// FIP (low 32 bits) at 12, FCS at 16, FOP at 18, FDP (low 32 bits) at
/// 20, FDS at 24, then ffff.
const ENVIRONMENT_BYTES: usize = 28;
/// The selectors of a 64-bit Linux process, recorded with FIP and FDP.
const USER_CS: u16 = 0x33;
const USER_DS: u16 = 0x2b;
/// fnsave / frstor: the environment, then ST(0)..ST(7), 10 bytes each.
const SAVE_BYTES: usize = ENVIRONMENT_BYTES + 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X87 {
    /// Physical registers R0..R7; ST(i) is `regs[(top + i) % 8]`.
    regs: [F80; 8],
    /// Bit i set: physical register i is empty.
    empty: u8,
    top: u8,
    control: u16,
    /// Status word without TOP.
    status: u16,
    /// The last non-control instruction: its address and selector, its
    /// FOP, and the address and selector of its memory operand (kept from
    /// an earlier one when it has none). Stored by fnstenv and fnsave; not
    /// by fxsave, like qemu and AMD processors without a pending exception
    /// (the CPUID vendor is AMD's, ADR-0010).
    fip: u64,
    fcs: u16,
    fop: u16,
    fdp: u64,
    fds: u16,
}

/// Where the MMX state is inside `X87`, for the JIT (ADR-0021): the
/// registers' array, the empty bits and TOP.
pub(crate) const REGS_OFFSET: usize = core::mem::offset_of!(X87, regs);
pub(crate) const EMPTY_OFFSET: usize = core::mem::offset_of!(X87, empty);
pub(crate) const TOP_OFFSET: usize = core::mem::offset_of!(X87, top);

impl Default for X87 {
    fn default() -> X87 {
        X87 {
            regs: [F80::ZERO; 8],
            empty: 0xff,
            top: 0,
            control: INITIAL_CONTROL_WORD,
            status: 0,
            fip: 0,
            fcs: 0,
            fop: 0,
            fdp: 0,
            fds: 0,
        }
    }
}

impl X87 {
    pub fn control_word(&self) -> u16 {
        self.control
    }

    pub fn status_word(&self) -> u16 {
        self.status | (u16::from(self.top) << TOP_SHIFT)
    }

    /// The abridged tag word of FXSAVE: bit i set if physical register i
    /// is not empty.
    pub fn abridged_tags(&self) -> u8 {
        !self.empty
    }

    /// FNINIT (also at the end of FNSAVE): default control word, clear
    /// status, every register empty. The registers keep their contents,
    /// as on the hardware and under qemu (FNSAVE shows them).
    fn init(&mut self) {
        *self = X87 {
            regs: self.regs,
            ..X87::default()
        };
    }

    /// The full tag word: two bits per physical register (11 empty).
    fn tag_word(&self) -> u16 {
        (0..8).fold(0, |word, p| {
            let tag = if self.empty & (1 << p) != 0 {
                3
            } else {
                f80::tag(self.regs[p])
            };
            word | (tag << (2 * p))
        })
    }

    /// The fnstenv image.
    fn environment(&self) -> [u8; ENVIRONMENT_BYTES] {
        let mut env = [0u8; ENVIRONMENT_BYTES];
        let words: [(usize, u16); 9] = [
            (0, self.control),
            (2, 0xffff),
            (4, self.status_word()),
            (6, 0xffff),
            (8, self.tag_word()),
            (10, 0xffff),
            (16, self.fcs),
            (18, self.fop),
            (24, self.fds),
        ];
        for (at, word) in words {
            env[at..at + 2].copy_from_slice(&word.to_le_bytes());
        }
        env[12..16].copy_from_slice(&(self.fip as u32).to_le_bytes());
        env[20..24].copy_from_slice(&(self.fdp as u32).to_le_bytes());
        env[26..28].copy_from_slice(&0xffffu16.to_le_bytes());
        env
    }

    /// Loads an fldenv image: control, status (with TOP) and the empty
    /// registers from the tag word.
    fn load_environment(&mut self, env: &[u8]) -> Exec<()> {
        let word = |at: usize| u16::from_le_bytes([env[at], env[at + 1]]);
        let control = word(0);
        check_control_word(control)?;
        self.control = control;
        let status = word(4);
        self.top = ((status >> TOP_SHIFT) & 7) as u8;
        self.status = status & !(7 << TOP_SHIFT);
        let dword = |at: usize| u32::from_le_bytes(env[at..at + 4].try_into().expect("4 bytes"));
        self.fip = u64::from(dword(12));
        self.fcs = word(16);
        self.fop = word(18) & 0x7ff;
        self.fdp = u64::from(dword(20));
        self.fds = word(24);
        let tags = word(8);
        self.empty = (0..8).fold(0, |empty, p| {
            if (tags >> (2 * p)) & 3 == 3 {
                empty | (1 << p)
            } else {
                empty
            }
        });
        Ok(())
    }

    /// ST(i) in stack order, as stored by FXSAVE.
    pub fn stack_register(&self, i: u8) -> F80 {
        self.regs[self.physical(i)]
    }

    /// The x87 part of FXRSTOR.
    pub(crate) fn restore(
        &mut self,
        control: u16,
        status: u16,
        tags: u8,
        stack: [F80; 8],
    ) -> Exec<()> {
        check_control_word(control)?;
        self.control = control;
        self.top = ((status >> TOP_SHIFT) & 7) as u8;
        self.status = status & !(7 << TOP_SHIFT);
        self.empty = !tags;
        for (i, value) in stack.into_iter().enumerate() {
            let physical = self.physical(i as u8);
            self.regs[physical] = value;
        }
        Ok(())
    }

    /// MMX register `n`: the significand of physical register Rn.
    pub(crate) fn mm(&self, n: u8) -> u64 {
        self.regs[usize::from(n)].mant
    }

    /// Writing an MMX register sets the exponent and sign bits of Rn to
    /// ones (SDM vol. 1, 9.5.1).
    pub(crate) fn set_mm(&mut self, n: u8, value: u64) {
        self.regs[usize::from(n)] = F80 {
            sign: true,
            exp: 0x7fff,
            mant: value,
        };
    }

    /// An MMX instruction (other than emms): TOP = 0 and every register
    /// valid.
    pub(crate) fn enter_mmx(&mut self) {
        self.top = 0;
        self.empty = 0;
    }

    /// emms: every register empty.
    pub(crate) fn emms(&mut self) {
        self.empty = 0xff;
    }

    fn physical(&self, i: u8) -> usize {
        usize::from((self.top + i) & 7)
    }

    /// Rounding and precision control (`check_control_word` refused the
    /// reserved precision value).
    fn mode(&self) -> Mode {
        let precision = match (self.control >> PRECISION_SHIFT) & 3 {
            0 => 24,
            2 => 53,
            _ => 64,
        };
        Mode::register(Rounding::from_control_word(self.control), precision)
    }

    fn is_empty(&self, i: u8) -> bool {
        self.empty & (1 << self.physical(i)) != 0
    }

    /// ST(i), or the masked response to a stack underflow.
    fn get(&mut self, i: u8) -> Option<F80> {
        if self.is_empty(i) {
            self.stack_fault(false);
            None
        } else {
            Some(self.regs[self.physical(i)])
        }
    }

    fn set(&mut self, i: u8, value: F80) {
        let p = self.physical(i);
        self.regs[p] = value;
        self.empty &= !(1 << p);
    }

    fn push(&mut self, value: F80) {
        self.top = (self.top + 7) & 7;
        if !self.is_empty(0) {
            // Stack overflow: the masked response pushes the indefinite.
            self.stack_fault(true);
            self.set(0, F80::INDEFINITE);
            return;
        }
        self.set(0, value);
    }

    fn pop(&mut self) {
        let p = self.physical(0);
        self.empty |= 1 << p;
        self.top = (self.top + 1) & 7;
    }

    /// IE and SF; C1 tells an overflow (1) from an underflow (0).
    fn stack_fault(&mut self, overflow: bool) {
        self.status |= FLAG_INVALID | STACK_FAULT;
        self.set_c1(overflow);
    }

    fn set_c1(&mut self, set: bool) {
        if set {
            self.status |= C1;
        } else {
            self.status &= !C1;
        }
    }

    /// Records the flags of an operation; C1 is the round-up indicator.
    fn flags<T>(&mut self, result: &Out<T>) {
        self.status |= result.flags;
        self.set_c1(result.rounded_up);
    }

    fn set_condition(&mut self, compare: Compare) {
        let codes = match compare {
            Compare::Greater => 0,
            Compare::Less => C0,
            Compare::Equal => C3,
            Compare::Unordered => C3 | C2 | C0,
        };
        self.status = (self.status & !(C0 | C1 | C2 | C3)) | codes;
    }
}

fn check_control_word(control: u16) -> Exec<()> {
    if control & EXCEPTION_MASKS != EXCEPTION_MASKS {
        return Err(Trap::Unsupported(
            "x87 control word with unmasked exceptions",
        ));
    }
    if (control >> PRECISION_SHIFT) & 3 == PRECISION_RESERVED {
        return Err(Trap::Unsupported(
            "x87 control word with the reserved precision value",
        ));
    }
    Ok(())
}

impl Cpu {
    pub(crate) fn x87(
        &mut self,
        op: X87Op,
        opcode: u16,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        if !is_control(op) {
            self.x87.fip = self.rip;
            self.x87.fcs = match self.mode {
                armor_x86::Mode::Long => USER_CS,
                armor_x86::Mode::Compat32 => crate::USER32_CS,
            };
            self.x87.fop = opcode;
            if let Some(m) = memory_operand(op) {
                self.x87.fdp = self.address(&m, next);
                self.x87.fds = USER_DS;
            }
        }
        let mode = self.x87.mode();
        let rc = mode.rc;
        match op {
            X87Op::Arith {
                op,
                src,
                to_st,
                pop,
            } => {
                // st0 op src, or st(i) op st0.
                let (dest_index, other) = match (to_st, src) {
                    (true, X87Operand::St(i)) => (i, self.x87.get(0)),
                    _ => (0, self.x87_read(src, next, mem)?),
                };
                let dest = self.x87.get(dest_index);
                let result = match (dest, other) {
                    (Some(dest), Some(other)) => {
                        let result = arith(op, dest, other, mode);
                        self.x87.flags(&result);
                        result.value
                    }
                    _ => F80::INDEFINITE,
                };
                self.x87.set(dest_index, result);
                if pop {
                    self.x87.pop();
                }
            }
            X87Op::Compare {
                src,
                unordered,
                pops,
            } => {
                let other = self.x87_read(src, next, mem)?;
                let compare = match (self.x87.get(0), other) {
                    (Some(a), Some(b)) => {
                        let result = f80::compare(a, b, unordered);
                        self.x87.status |= result.flags;
                        result.value
                    }
                    _ => Compare::Unordered,
                };
                self.x87.set_condition(compare);
                for _ in 0..pops {
                    self.x87.pop();
                }
            }
            X87Op::CompareFlags {
                index,
                unordered,
                pop,
            } => {
                let compare = match (self.x87.get(0), self.x87.get(index)) {
                    (Some(a), Some(b)) => {
                        let result = f80::compare(a, b, unordered);
                        self.x87.status |= result.flags;
                        result.value
                    }
                    _ => Compare::Unordered,
                };
                self.x87.set_c1(false);
                let flags = match compare {
                    Compare::Greater => 0,
                    Compare::Less => FLAG_CF,
                    Compare::Equal => FLAG_ZF,
                    Compare::Unordered => FLAG_ZF | FLAG_PF | FLAG_CF,
                };
                self.rflags = (self.rflags & !STATUS_FLAGS) | flags;
                if pop {
                    self.x87.pop();
                }
            }
            X87Op::Test => {
                let compare = match self.x87.get(0) {
                    Some(a) => {
                        let result = f80::compare(a, F80::ZERO, false);
                        self.x87.status |= result.flags;
                        result.value
                    }
                    None => Compare::Unordered,
                };
                self.x87.set_condition(compare);
            }
            X87Op::Load(src) => {
                let value = self.x87_read(src, next, mem)?.unwrap_or(F80::INDEFINITE);
                self.x87.set_c1(false);
                self.x87.push(value);
            }
            X87Op::LoadOne => {
                self.x87.set_c1(false);
                self.x87.push(F80::ONE);
            }
            X87Op::LoadZero => {
                self.x87.set_c1(false);
                self.x87.push(F80::ZERO);
            }
            X87Op::Store { dst, pop } => {
                let value = self.x87.get(0);
                self.x87_write(dst, value, rc, next, mem)?;
                if pop {
                    self.x87.pop();
                }
            }
            X87Op::Exchange(i) => {
                let (a, b) = (self.x87.get(0), self.x87.get(i));
                if a.is_some() && b.is_some() {
                    // Without a stack fault (which sets C1 itself).
                    self.x87.set_c1(false);
                }
                let a = a.unwrap_or(F80::INDEFINITE);
                let b = b.unwrap_or(F80::INDEFINITE);
                self.x87.set(0, b);
                self.x87.set(i, a);
            }
            X87Op::ChangeSign | X87Op::Abs | X87Op::Sqrt => {
                let result = match self.x87.get(0) {
                    Some(a) => {
                        let result = match op {
                            X87Op::ChangeSign => f80_out(a.negate()),
                            X87Op::Abs => f80_out(a.abs()),
                            _ => f80::sqrt(a, mode),
                        };
                        self.x87.flags(&result);
                        result.value
                    }
                    None => F80::INDEFINITE,
                };
                self.x87.set(0, result);
            }
            X87Op::ConditionalMove { cond, index } => {
                if self.condition(cond) {
                    let value = self.x87.get(index).unwrap_or(F80::INDEFINITE);
                    self.x87.set(0, value);
                }
            }
            X87Op::LoadControlWord(m) => {
                let mut bytes = [0u8; 2];
                mem.read(self.address(&m, next), &mut bytes)?;
                let control = u16::from_le_bytes(bytes);
                check_control_word(control)?;
                self.x87.control = control;
            }
            X87Op::StoreControlWord(m) => {
                let addr = self.address(&m, next);
                mem.write(addr, &self.x87.control.to_le_bytes())?;
            }
            X87Op::StoreStatusWord(target) => {
                let status = self.x87.status_word();
                match target {
                    Some(m) => mem.write(self.address(&m, next), &status.to_le_bytes())?,
                    None => self.regs[RAX] = (self.regs[RAX] & !0xffff) | u64::from(status),
                }
            }
            X87Op::Init => self.x87.init(),
            X87Op::ClearExceptions => {
                // FNCLEX: the exception flags, SF, ES and B.
                self.x87.status &= !(EXCEPTION_FLAGS | STACK_FAULT | 0x80 | 0x8000);
            }
            X87Op::Free(i) => {
                let p = self.x87.physical(i);
                self.x87.empty |= 1 << p;
            }
            X87Op::FreeAndPop(i) => {
                let p = self.x87.physical(i);
                self.x87.empty |= 1 << p;
                self.x87.pop();
            }
            X87Op::Nop | X87Op::Wait => {}
            X87Op::LoadConstant(c) => {
                let c = match c {
                    X87Constant::Pi => Constant::Pi,
                    X87Constant::Log2E => Constant::Log2E,
                    X87Constant::Log2Ten => Constant::Log2Ten,
                    X87Constant::Log10Two => Constant::Log10Two,
                    X87Constant::LnTwo => Constant::LnTwo,
                };
                self.x87.set_c1(false);
                self.x87.push(f80::constant(c, rc));
            }
            X87Op::Examine => {
                // Also for an empty register (no stack fault); C1 is the
                // sign of whatever the register holds.
                let p = self.x87.physical(0);
                let value = self.x87.regs[p];
                let (c3, c2, c0) = if self.x87.empty & (1 << p) != 0 {
                    (true, false, true)
                } else {
                    f80::examine(value)
                };
                let bit = |set: bool, flag: u16| if set { flag } else { 0 };
                self.x87.status = (self.x87.status & !(C0 | C1 | C2 | C3))
                    | bit(c3, C3)
                    | bit(c2, C2)
                    | bit(c0, C0)
                    | bit(value.sign, C1);
            }
            X87Op::RoundToInteger => {
                let result = match self.x87.get(0) {
                    Some(a) => {
                        let result = f80::round_to_integral(a, rc);
                        self.x87.flags(&result);
                        result.value
                    }
                    None => F80::INDEFINITE,
                };
                self.x87.set(0, result);
            }
            X87Op::Scale => {
                let result = match (self.x87.get(0), self.x87.get(1)) {
                    (Some(a), Some(b)) => {
                        let result = f80::scale(a, b, rc);
                        self.x87.flags(&result);
                        result.value
                    }
                    _ => F80::INDEFINITE,
                };
                self.x87.set(0, result);
            }
            X87Op::Extract => match self.x87.get(0) {
                Some(a) => {
                    let result = f80::extract(a);
                    self.x87.status |= result.flags;
                    self.x87.set_c1(false);
                    let (exponent, significand) = result.value;
                    self.x87.set(0, exponent);
                    self.x87.push(significand);
                }
                None => {
                    self.x87.set(0, F80::INDEFINITE);
                    self.x87.push(F80::INDEFINITE);
                }
            },
            X87Op::PartialRemainder { ieee } => {
                let (value, quotient, complete) = match (self.x87.get(0), self.x87.get(1)) {
                    (Some(a), Some(b)) => {
                        let result = f80::remainder(a, b, ieee);
                        self.x87.status |= result.flags;
                        let r = result.value;
                        (r.value, r.quotient, r.complete)
                    }
                    _ => (F80::INDEFINITE, 0, true),
                };
                // Complete: C0 = Q2, C3 = Q1, C1 = Q0, C2 = 0. Partial:
                // C2 = 1 and the others clear, as on an Intel Xeon X5675
                // (the hardware oracle, docs/testing.md; the SDM's
                // pseudocode does not assign them).
                let bit = |set: bool, flag: u16| if set { flag } else { 0 };
                self.x87.status = (self.x87.status & !(C0 | C1 | C2 | C3))
                    | if complete {
                        bit(quotient & 4 != 0, C0)
                            | bit(quotient & 2 != 0, C3)
                            | bit(quotient & 1 != 0, C1)
                    } else {
                        C2
                    };
                self.x87.set(0, value);
            }
            X87Op::Transcendental(op) => self.transcendental(op, rc),
            X87Op::IncrementTop | X87Op::DecrementTop => {
                let step = if op == X87Op::IncrementTop { 1 } else { 7 };
                self.x87.top = (self.x87.top + step) & 7;
                self.x87.set_c1(false);
            }
            X87Op::StoreEnvironment(m) => {
                let env = self.x87.environment();
                mem.write(self.address(&m, next), &env)?;
                // fnstenv then masks every exception: armor already
                // requires them masked.
            }
            X87Op::LoadEnvironment(m) => {
                let mut env = [0u8; ENVIRONMENT_BYTES];
                mem.read(self.address(&m, next), &mut env)?;
                self.x87.load_environment(&env)?;
            }
            X87Op::Save(m) => {
                let mut area = [0u8; SAVE_BYTES];
                area[..ENVIRONMENT_BYTES].copy_from_slice(&self.x87.environment());
                for i in 0..8 {
                    let at = ENVIRONMENT_BYTES + 10 * i;
                    area[at..at + 10].copy_from_slice(&self.x87.stack_register(i as u8).to_bytes());
                }
                mem.write(self.address(&m, next), &area)?;
                self.x87.init();
            }
            X87Op::Restore(m) => {
                let mut area = [0u8; SAVE_BYTES];
                mem.read(self.address(&m, next), &mut area)?;
                self.x87.load_environment(&area[..ENVIRONMENT_BYTES])?;
                for i in 0..8 {
                    let at = ENVIRONMENT_BYTES + 10 * i;
                    let value = F80::from_bytes(area[at..at + 10].try_into().expect("10 bytes"));
                    let p = self.x87.physical(i as u8);
                    self.x87.regs[p] = value;
                }
            }
            X87Op::StoreTruncate(dst) => {
                let value = self.x87.get(0);
                self.x87_write(dst, value, Rounding::Zero, next, mem)?;
                self.x87.pop();
            }
        }
        Ok(())
    }

    /// A source operand: ST(i) (`None` if empty, after the stack fault)
    /// or memory, converted exactly to 80 bits.
    fn x87_read(
        &mut self,
        src: X87Operand,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<Option<F80>> {
        let (m, format) = match src {
            X87Operand::St(i) => return Ok(self.x87.get(i)),
            X87Operand::Mem(m, format) => (m, format),
        };
        let addr = self.address(&m, next);
        let mut bytes = [0u8; 10];
        let len = format_size(format);
        mem.read(addr, &mut bytes[..len])?;
        let raw = u64::from_le_bytes(bytes[..8].try_into().expect("8 bytes"));
        let value = match format {
            X87Format::F80 => F80::from_bytes(bytes),
            X87Format::F32 | X87Format::F64 => {
                let result = f80::from_float(raw, format == X87Format::F64);
                self.x87.status |= result.flags;
                result.value
            }
            X87Format::I16 => f80::from_int(sign_extend(raw & 0xffff, armor_x86::Size::B16) as i64),
            X87Format::I32 => {
                f80::from_int(sign_extend(raw & 0xffff_ffff, armor_x86::Size::B32) as i64)
            }
            X87Format::I64 => f80::from_int(raw as i64),
            X87Format::Bcd => from_bcd(bytes),
        };
        Ok(Some(value))
    }

    /// Stores `value` (`None`: an empty ST(0), stored as the indefinite of
    /// the destination format).
    fn x87_write(
        &mut self,
        dst: X87Operand,
        value: Option<F80>,
        rc: Rounding,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        let (m, format) = match dst {
            X87Operand::St(i) => {
                self.x87.set(i, value.unwrap_or(F80::INDEFINITE));
                return Ok(());
            }
            X87Operand::Mem(m, format) => (m, format),
        };
        let value = value.unwrap_or(F80::INDEFINITE);
        let bytes: Vec<u8> = match format {
            X87Format::F80 => {
                self.x87.set_c1(false);
                value.to_bytes().to_vec()
            }
            X87Format::F32 | X87Format::F64 => {
                let result = f80::to_float(value, format == X87Format::F64, rc);
                self.x87.flags(&result);
                result.value.to_le_bytes()[..format_size(format)].to_vec()
            }
            X87Format::I16 | X87Format::I32 | X87Format::I64 => {
                let bits = 8 * format_size(format) as u32;
                let result = f80::to_int(value, bits, rc);
                self.x87.flags(&result);
                result.value.to_le_bytes()[..format_size(format)].to_vec()
            }
            X87Format::Bcd => {
                let result = to_bcd(value, rc);
                self.x87.flags(&result);
                result.value.to_vec()
            }
        };
        mem.write(self.address(&m, next), &bytes)?;
        Ok(())
    }
}

/// The control instructions (SDM vol. 1, 8.1.8), which leave the
/// last-instruction pointers as they are.
impl Cpu {
    /// f2xm1, fyl2x, fyl2xp1, fpatan, fsin, fcos, fsincos, fptan, correctly
    /// rounded (ADR-0030, `f80::transcendental`). The precision control
    /// does not apply. C1 is the round-up indicator; the trigonometric
    /// ones set C2 when |st0| >= 2^63 and leave st0 unchanged.
    fn transcendental(&mut self, op: X87Transcendental, rc: Rounding) {
        use X87Transcendental as T;
        match op {
            T::Exp2m1 => {
                let result = match self.x87.get(0) {
                    Some(a) => {
                        let result = f80::exp2m1(a, rc);
                        self.x87.flags(&result);
                        result.value
                    }
                    None => F80::INDEFINITE,
                };
                self.x87.set(0, result);
            }
            T::Ylog2x | T::Ylog2xp1 | T::Atan => {
                let result = match (self.x87.get(0), self.x87.get(1)) {
                    (Some(x), Some(y)) => {
                        let result = match op {
                            T::Ylog2x => f80::ylog2x(y, x, rc),
                            T::Ylog2xp1 => f80::ylog2xp1(y, x, rc),
                            _ => f80::atan2(y, x, rc),
                        };
                        self.x87.flags(&result);
                        result.value
                    }
                    _ => F80::INDEFINITE,
                };
                self.x87.set(1, result);
                self.x87.pop();
            }
            T::Sin | T::Cos | T::Tan | T::SinCos => {
                let Some(x) = self.x87.get(0) else {
                    self.x87.set(0, F80::INDEFINITE);
                    if matches!(op, T::Tan | T::SinCos) {
                        self.x87.push(F80::INDEFINITE);
                    }
                    return;
                };
                let first = match op {
                    T::Sin | T::SinCos => f80::sin(x, rc),
                    T::Cos => f80::cos(x, rc),
                    _ => f80::tan(x, rc),
                };
                let first = match first {
                    f80::Trig::OutOfRange => {
                        self.x87.status |= C2;
                        return;
                    }
                    f80::Trig::Done(result) => result,
                };
                self.x87.status &= !C2;
                self.x87.flags(&first);
                self.x87.set(0, first.value);
                match op {
                    T::Tan => self.x87.push(F80::ONE),
                    T::SinCos => {
                        // In range, as sin was.
                        if let f80::Trig::Done(cos) = f80::cos(x, rc) {
                            self.x87.status |= cos.flags;
                            // C1 for the last result written ⚠️.
                            self.x87.set_c1(cos.rounded_up);
                            self.x87.push(cos.value);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn is_control(op: X87Op) -> bool {
    matches!(
        op,
        X87Op::Init
            | X87Op::ClearExceptions
            | X87Op::LoadControlWord(_)
            | X87Op::StoreControlWord(_)
            | X87Op::StoreStatusWord(_)
            | X87Op::StoreEnvironment(_)
            | X87Op::LoadEnvironment(_)
            | X87Op::Save(_)
            | X87Op::Restore(_)
            | X87Op::Wait
    )
}

/// The memory operand of a non-control instruction, if it has one.
fn memory_operand(op: X87Op) -> Option<armor_x86::Mem> {
    match op {
        X87Op::Arith {
            src: X87Operand::Mem(m, _),
            ..
        }
        | X87Op::Compare {
            src: X87Operand::Mem(m, _),
            ..
        }
        | X87Op::Load(X87Operand::Mem(m, _))
        | X87Op::Store {
            dst: X87Operand::Mem(m, _),
            ..
        }
        | X87Op::StoreTruncate(X87Operand::Mem(m, _)) => Some(m),
        _ => None,
    }
}

fn f80_out(value: F80) -> Out<F80> {
    Out {
        value,
        flags: 0,
        rounded_up: false,
    }
}

/// `dest op other`, with the operands in the order of the arithmetic
/// expression (it decides which NaN propagates when both are NaNs with
/// equal significands).
fn arith(op: X87Arith, dest: F80, other: F80, mode: Mode) -> Out<F80> {
    match op {
        X87Arith::Add => f80::add(dest, other, mode),
        X87Arith::Mul => f80::mul(dest, other, mode),
        X87Arith::Sub => f80::sub(dest, other, mode),
        X87Arith::SubR => f80::sub(other, dest, mode),
        X87Arith::Div => f80::div(dest, other, mode),
        X87Arith::DivR => f80::div(other, dest, mode),
    }
}

fn format_size(format: X87Format) -> usize {
    match format {
        X87Format::F32 | X87Format::I32 => 4,
        X87Format::F64 | X87Format::I64 => 8,
        X87Format::F80 | X87Format::Bcd => 10,
        X87Format::I16 => 2,
    }
}

/// The largest packed BCD integer, 18 nines.
const BCD_MAX: u64 = 999_999_999_999_999_999;
/// What fbstp stores for an invalid operand (SDM vol. 1, 4.7: the packed
/// BCD integer indefinite, FFFF C000 0000 0000 0000h).
const BCD_INDEFINITE: [u8; 10] = [0, 0, 0, 0, 0, 0, 0, 0xc0, 0xff, 0xff];

/// fbld: exact, as 18 digits fit in 64 bits; the sign of zero is kept.
/// Nibbles above 9 are undefined on x86; here they count with their value.
fn from_bcd(bytes: [u8; 10]) -> F80 {
    let magnitude = bytes[..9].iter().rev().fold(0u64, |n, &b| {
        n * 100 + u64::from(b >> 4) * 10 + u64::from(b & 15)
    });
    let sign = bytes[9] & 0x80 != 0;
    let value = f80::from_int(magnitude as i64);
    F80 { sign, ..value }
}

/// fbstp: ST(0) rounded to an integer by `rc`, as 18 digits and the sign
/// (also of a zero); a NaN, an infinity or more than 18 digits is invalid
/// and stores the indefinite.
fn to_bcd(value: F80, rc: Rounding) -> Out<[u8; 10]> {
    let result = f80::to_int(value, 64, rc);
    let magnitude = (result.value as i64).unsigned_abs();
    if result.flags & FLAG_INVALID != 0 || magnitude > BCD_MAX {
        return Out {
            value: BCD_INDEFINITE,
            flags: FLAG_INVALID,
            rounded_up: false,
        };
    }
    let mut bytes = [0u8; 10];
    let mut n = magnitude;
    for byte in &mut bytes[..9] {
        *byte = (((n / 10 % 10) << 4) | (n % 10)) as u8;
        n /= 100;
    }
    if value.sign {
        bytes[9] = 0x80;
    }
    Out {
        value: bytes,
        flags: result.flags,
        rounded_up: result.rounded_up,
    }
}
