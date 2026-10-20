//! SSE2 execution on the 128-bit xmm registers.
//!
//! Legacy (non-VEX) SSE instructions require 16-byte aligned memory operands
//! for 128-bit accesses, except the explicitly unaligned moves (movups,
//! movdqu, movupd); a misaligned access raises #GP.
use alloc::vec::Vec;

use armor_guest::GuestMemory;
use armor_x86::{
    Conversion, Dup, FloatOp, IntOp, LaneOp, LogicOp, Mem, MmxConversion, Operand, Reg, ShiftKind,
    Size, SseOp, StateOp,
};

use crate::f80::F80;
use crate::float;
use crate::{Cpu, Exception, Exec, FLAG_CF, FLAG_PF, FLAG_ZF, STATUS_FLAGS, Trap};

const LOW64: u128 = u64::MAX as u128;

/// MXCSR bits that software may set; others raise #GP in ldmxcsr and
/// fxrstor. Also stored as MXCSR_MASK by fxsave.
const MXCSR_MASK: u32 = 0xffff;

// FXSAVE area layout (Intel SDM vol. 1, 10.5.1). As on an Intel Xeon X5675
// (the hardware oracle, docs/testing.md): the 6 bytes after each 80-bit
// x87 register are written as zeros, 416..512 keep their contents.
const FXSAVE_MXCSR: usize = 24;
const FXSAVE_ST: usize = 32;
const X87_REGISTER_BYTES: usize = 10;
const FXSAVE_MXCSR_MASK: usize = 28;
const FXSAVE_XMM: usize = 160;
const FXSAVE_WRITTEN: usize = 416;

impl Cpu {
    /// fxsave, fxrstor, ldmxcsr, stmxcsr, with the x87 state of `x87.rs`.
    pub(crate) fn state(
        &mut self,
        op: StateOp,
        m: Mem,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        let addr = self.address(&m, next);
        match op {
            StateOp::Fxsave => {
                check_alignment(addr, true)?;
                // Read first, so that reserved bytes are written back as
                // they were (pages that can be written can be read on x86).
                let mut area = [0u8; FXSAVE_WRITTEN];
                mem.read(addr, &mut area)?;
                self.save_fx_state(&mut area);
                mem.write(addr, &area)?;
            }
            StateOp::Fxrstor => {
                check_alignment(addr, true)?;
                let mut area = [0u8; FXSAVE_WRITTEN];
                mem.read(addr, &mut area)?;
                self.restore_fx_state(&area)?;
            }
            StateOp::Ldmxcsr => {
                let mut bytes = [0u8; 4];
                mem.read(addr, &mut bytes)?;
                self.set_mxcsr(u32::from_le_bytes(bytes))?;
            }
            StateOp::Stmxcsr => mem.write(addr, &self.mxcsr.to_le_bytes())?,
            // Faults like a one-byte load (SDM, CLFLUSH); no cache to flush.
            StateOp::Clflush => mem.read(addr, &mut [0u8; 1])?,
        }
        Ok(())
    }

    /// Writes the defined fields of the FXSAVE image into `area`; reserved
    /// bytes keep their contents.
    fn save_fx_state(&self, area: &mut [u8; FXSAVE_WRITTEN]) {
        // FCW, FSW, abridged FTW; FOP, FIP and FDP are 0: armor does not
        // track the last x87 instruction. FOP is written as the SDM says,
        // though qemu leaves it unchanged.
        area[..FXSAVE_MXCSR].fill(0);
        area[..2].copy_from_slice(&self.x87.control_word().to_le_bytes());
        area[2..4].copy_from_slice(&self.x87.status_word().to_le_bytes());
        area[4] = self.x87.abridged_tags();
        for i in 0..8 {
            let at = FXSAVE_ST + 16 * i;
            area[at..at + X87_REGISTER_BYTES]
                .copy_from_slice(&self.x87.stack_register(i as u8).to_bytes());
            // The 6 reserved bytes after each register: zeros, as an Intel
            // Xeon X5675 writes them (the hardware oracle; qemu leaves them
            // unchanged, docs/testing.md).
            area[at + X87_REGISTER_BYTES..at + 16].fill(0);
        }
        area[FXSAVE_MXCSR..FXSAVE_MXCSR + 4].copy_from_slice(&self.mxcsr.to_le_bytes());
        area[FXSAVE_MXCSR_MASK..FXSAVE_MXCSR_MASK + 4].copy_from_slice(&MXCSR_MASK.to_le_bytes());
        for (i, xmm) in self.xmm.iter().enumerate() {
            let at = FXSAVE_XMM + 16 * i;
            area[at..at + 16].copy_from_slice(&xmm.to_le_bytes());
        }
    }

    fn restore_fx_state(&mut self, area: &[u8; FXSAVE_WRITTEN]) -> Exec<()> {
        let mxcsr = u32::from_le_bytes(
            area[FXSAVE_MXCSR..FXSAVE_MXCSR + 4]
                .try_into()
                .expect("4 bytes"),
        );
        self.set_mxcsr(mxcsr)?;
        let word = |at: usize| u16::from_le_bytes([area[at], area[at + 1]]);
        let stack = core::array::from_fn(|i| {
            let at = FXSAVE_ST + 16 * i;
            F80::from_bytes(area[at..at + 10].try_into().expect("10 bytes"))
        });
        self.x87.restore(word(0), word(2), area[4], stack)?;
        for (i, xmm) in self.xmm.iter_mut().enumerate() {
            let at = FXSAVE_XMM + 16 * i;
            *xmm = u128::from_le_bytes(area[at..at + 16].try_into().expect("16 bytes"));
        }
        Ok(())
    }

    /// The x87 and SSE state as a 512-byte FXSAVE image (the rest zero),
    /// as Linux stores it in a signal frame.
    pub fn fx_image(&self) -> [u8; 512] {
        let mut area = [0u8; FXSAVE_WRITTEN];
        self.save_fx_state(&mut area);
        let mut image = [0u8; 512];
        image[..FXSAVE_WRITTEN].copy_from_slice(&area);
        image
    }

    /// Loads a 512-byte FXSAVE image (signal return). `Err` names what is
    /// wrong: reserved MXCSR bits or a control state armor does not
    /// support.
    pub fn load_fx_image(&mut self, image: &[u8; 512]) -> Result<(), &'static str> {
        let area: &[u8; FXSAVE_WRITTEN] = image[..FXSAVE_WRITTEN].try_into().expect("416 bytes");
        self.restore_fx_state(area).map_err(|trap| match trap {
            Trap::Unsupported(what) => what,
            _ => "reserved MXCSR bits in a signal frame",
        })
    }

    /// The MXCSR control bits the floating-point operations follow.
    fn float_env(&self) -> float::Env {
        float::Env::from_mxcsr(self.mxcsr)
    }

    /// Any control bits: every rounding mode, FZ, DAZ, unmasked
    /// exceptions (ADR-0024). Reserved bits raise #GP.
    fn set_mxcsr(&mut self, value: u32) -> Exec<()> {
        if value & !MXCSR_MASK != 0 {
            return Err(Trap::Exception(Exception::GeneralProtection));
        }
        self.mxcsr = value;
        Ok(())
    }

    /// Adds the exception flags of an SSE floating-point instruction to
    /// MXCSR, or raises #XM if one of them is unmasked; the caller writes
    /// its result only after this. x86 checks the flags in two rounds
    /// (✅ Xeon X5675, `c/sse_exceptions`): if an unmasked IE, DE or ZE
    /// is among them, those raised before the computation (in every lane)
    /// are the only ones set; otherwise all of them are, OE, UE and PE
    /// included. Old flags that are set and unmasked do not raise #XM:
    /// only the instruction's own do.
    fn raise(&mut self, flags: u16) -> Exec<()> {
        const BEFORE: u32 = 0x07;
        let flags = u32::from(flags);
        let unmasked = !(self.mxcsr >> 7) & 0x3f;
        if flags & unmasked == 0 {
            self.mxcsr |= flags;
            return Ok(());
        }
        self.mxcsr |= if flags & unmasked & BEFORE != 0 {
            flags & BEFORE
        } else {
            flags
        };
        Err(Trap::Exception(Exception::SimdFloatingPoint))
    }

    pub(crate) fn sse(
        &mut self,
        op: SseOp,
        dst: Operand,
        src: Operand,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        let is_mm = |o: Operand| matches!(o, Operand::Reg(Reg::Mm(n)) if n < 8);
        if op == SseOp::Emms {
            self.x87.emms();
            return Ok(());
        }
        // The conversions with an MMX operand come here even when it is in
        // memory (cvtpi2ps xmm, m64).
        if is_mm(dst) || is_mm(src) || matches!(op, SseOp::MmxConvert(_)) {
            return self.mmx(op, dst, src, next, mem);
        }
        self.sse_ops(op, dst, src, next, mem)
    }

    /// An MMX instruction: its SSE equivalent on the scratch registers,
    /// the mm operands zero-extended (the low 64 bits of a lane operation
    /// depend only on the low 64 bits of its operands). The operations that
    /// mix the halves get their operands arranged first. Then the x87
    /// state switches to MMX use (TOP 0, every register valid).
    fn mmx(
        &mut self,
        op: SseOp,
        dst: Operand,
        src: Operand,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        let result = match op {
            // movd, movq, movntq, movq2dq, movdq2q: the moves handle mm.
            SseOp::MovToXmm { .. } | SseOp::MovFromXmm { .. } => {
                self.sse_ops(op, dst, src, next, mem)
            }
            SseOp::MmxConvert(conversion) => self.mmx_convert(conversion, dst, src, next, mem),
            _ => self.mmx_lanes(op, dst, src, next, mem),
        };
        self.mm_scratch = [0; 2];
        result?;
        // MMX use begins with an mm register operand ⚠️ (cvtpi2ps with a
        // memory source does not touch the x87 state here).
        let is_mm = |o: Operand| matches!(o, Operand::Reg(Reg::Mm(n)) if n < 8);
        if is_mm(dst) || is_mm(src) {
            self.x87.enter_mmx();
        }
        Ok(())
    }

    fn mmx_lanes(
        &mut self,
        op: SseOp,
        dst: Operand,
        src: Operand,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        const A: Operand = Operand::Reg(Reg::Mm(8));
        const B: Operand = Operand::Reg(Reg::Mm(9));
        let a = match dst {
            Operand::Reg(Reg::Mm(n)) => self.mm(n),
            _ => 0,
        };
        // pinsrw reads a word from memory or a register: it stays as is.
        let (b, src) = match src {
            Operand::Reg(Reg::Mm(n)) => (self.mm(n), B),
            Operand::Mem(_) if !matches!(op, SseOp::Pinsrw(_)) => {
                (u128::from(self.read(src, Size::B64, next, mem)?), B)
            }
            other => (0, other),
        };
        let (op, a, b) = match op {
            // Both operands in one register: the result's low half.
            SseOp::Pack { .. } | SseOp::Int(IntOp::Horizontal { .. }) => (op, a | (b << 64), 0),
            // The high halves of 64-bit registers are bytes 4–7.
            SseOp::Unpack { high: true, lane } => {
                (SseOp::Unpack { high: false, lane }, a >> 32, b >> 32)
            }
            // Indices of 3 bits (and the zeroing bit 7).
            SseOp::Int(IntOp::Pshufb) => (op, a, b & 0x8787_8787_8787_8787),
            // dst:src shifted right: src in the low half, dst above.
            SseOp::Int(IntOp::Palignr(_)) => (op, 0, b | (a << 64)),
            _ => (op, a, b),
        };
        self.mm_scratch = [a, b];
        let target = match dst {
            Operand::Reg(Reg::Mm(_)) => A,
            other => other,
        };
        self.sse_ops(op, target, src, next, mem)?;
        if let Operand::Reg(Reg::Mm(n)) = dst {
            self.set_mm(n, self.mm_scratch[0] & LOW64);
        }
        Ok(())
    }

    /// cvtpi2ps, cvtpi2pd, cvt(t)ps2pi, cvt(t)pd2pi: the packed SSE
    /// conversions on the two lanes involved.
    fn mmx_convert(
        &mut self,
        conversion: MmxConversion,
        dst: Operand,
        src: Operand,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        const B: Operand = Operand::Reg(Reg::Mm(9));
        // The 64-bit source of all but cvtpd2pi, in the scratch register.
        let low_source = |cpu: &mut Self| -> Exec<()> {
            let value = match src {
                Operand::Reg(Reg::Mm(n)) => cpu.mm(n),
                Operand::Reg(Reg::Xmm(n)) => cpu.xmm[usize::from(n)] & LOW64,
                other => u128::from(cpu.read(other, Size::B64, next, mem)?),
            };
            cpu.mm_scratch[1] = value;
            Ok(())
        };
        match conversion {
            MmxConversion::Pi2Ps => {
                low_source(self)?;
                let (value, flags) = self.convert(Conversion::Dq2Ps, B, next, mem)?;
                self.raise(flags)?;
                let old = self.xmm_of(dst);
                self.set_xmm(dst, (old & !LOW64) | (value & LOW64));
            }
            MmxConversion::Pi2Pd => {
                low_source(self)?;
                let (value, flags) = self.convert(Conversion::Dq2Pd, B, next, mem)?;
                self.raise(flags)?;
                self.set_xmm(dst, value);
            }
            MmxConversion::Ps2Pi { truncate } => {
                low_source(self)?;
                let (value, flags) = self.convert(Conversion::Ps2Dq { truncate }, B, next, mem)?;
                self.raise(flags)?;
                self.set_xmm(dst, value & LOW64);
            }
            MmxConversion::Pd2Pi { truncate } => {
                let (value, flags) =
                    self.convert(Conversion::Pd2Dq { truncate }, src, next, mem)?;
                self.raise(flags)?;
                self.set_xmm(dst, value & LOW64);
            }
        }
        Ok(())
    }

    fn sse_ops(
        &mut self,
        op: SseOp,
        dst: Operand,
        src: Operand,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        match op {
            SseOp::Emms | SseOp::MmxConvert(_) => unreachable!("handled by `sse`"),
            SseOp::Mov128 { aligned } => {
                let value = self.read128(src, aligned, next, mem)?;
                self.write128(dst, value, aligned, next, mem)?;
            }
            SseOp::MovToXmm { bytes } => {
                let size = gpr_size(bytes);
                let value = match src {
                    Operand::Reg(Reg::Xmm(n)) => self.xmm[usize::from(n)] as u64 & size.mask(),
                    other => self.read(other, size, next, mem)?,
                };
                self.set_xmm(dst, u128::from(value));
            }
            SseOp::MovFromXmm { bytes } => {
                let size = gpr_size(bytes);
                let value = self.xmm_of(src) as u64 & size.mask();
                match dst {
                    // movq xmm, xmm (66 0F D6): zeroes the upper half.
                    Operand::Reg(Reg::Xmm(n)) => self.xmm[usize::from(n)] = u128::from(value),
                    other => self.write(other, size, value, next, mem)?,
                }
            }
            SseOp::MovHalf { high } => {
                let shift = if high { 64 } else { 0 };
                match (dst, src) {
                    (Operand::Reg(Reg::Xmm(n)), Operand::Mem(_)) => {
                        let value = u128::from(self.read(src, Size::B64, next, mem)?);
                        let r = &mut self.xmm[usize::from(n)];
                        *r = (*r & !(LOW64 << shift)) | (value << shift);
                    }
                    (Operand::Mem(_), Operand::Reg(Reg::Xmm(n))) => {
                        let value = (self.xmm[usize::from(n)] >> shift) as u64;
                        self.write(dst, Size::B64, value, next, mem)?;
                    }
                    _ => unreachable!("the decoder only produces load and store forms"),
                }
            }
            SseOp::MovHlps => {
                let high = self.xmm_of(src) >> 64;
                let d = self.xmm_of(dst);
                self.set_xmm(dst, (d & !LOW64) | high);
            }
            SseOp::MovLhps => {
                let low = self.xmm_of(src) & LOW64;
                let d = self.xmm_of(dst);
                self.set_xmm(dst, (d & LOW64) | (low << 64));
            }
            SseOp::Lane { op, lane } => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                self.set_xmm(dst, lanes(a, b, lane, |x, y| lane_op(op, lane, x, y)));
            }
            SseOp::Logic(op) => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let value = match op {
                    LogicOp::And => a & b,
                    LogicOp::AndNot => !a & b,
                    LogicOp::Or => a | b,
                    LogicOp::Xor => a ^ b,
                };
                self.set_xmm(dst, value);
            }
            SseOp::Pmuludq => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let product = |shift: u32| {
                    let x = (a >> shift) as u32;
                    let y = (b >> shift) as u32;
                    u128::from(u64::from(x) * u64::from(y))
                };
                self.set_xmm(dst, product(0) | (product(64) << 64));
            }
            SseOp::Pmovmskb => {
                let value = self.xmm_of(src);
                let mask = (0..16).fold(0u64, |mask, i| {
                    mask | ((((value >> (i * 8 + 7)) & 1) as u64) << i)
                });
                if let Operand::Reg(reg) = dst {
                    self.write_reg(reg, Size::B32, mask);
                }
            }
            SseOp::Pshufd(order) => {
                let b = self.read128(src, true, next, mem)?;
                let value = (0..4).fold(0u128, |acc, i| {
                    let pick = u32::from((order >> (2 * i)) & 3);
                    let dword = (b >> (pick * 32)) & u128::from(u32::MAX);
                    acc | (dword << (i * 32))
                });
                self.set_xmm(dst, value);
            }
            SseOp::Pextrw(index) => {
                let word = (self.xmm_of(src) >> (16 * u32::from(index))) as u64 & 0xffff;
                let Operand::Reg(reg) = dst else {
                    unreachable!("the decoder puts a GPR here")
                };
                self.write_reg(reg, Size::B32, word);
            }
            SseOp::Pinsrw(index) => {
                let word = match src {
                    Operand::Reg(reg) => self.read_reg(reg, Size::B16),
                    other => self.read(other, Size::B16, next, mem)?,
                };
                let shift = 16 * u32::from(index);
                let old = self.xmm_of(dst) & !(0xffffu128 << shift);
                self.set_xmm(dst, old | (u128::from(word) << shift));
            }
            SseOp::Shuffle { double, order } => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let value = if double {
                    let lane = |v: u128, i: u8| (v >> (64 * u32::from(i & 1))) as u64;
                    u128::from(lane(a, order)) | (u128::from(lane(b, order >> 1)) << 64)
                } else {
                    let lane = |v: u128, i: u8| (v >> (32 * u32::from(i & 3))) as u32;
                    u128::from(lane(a, order))
                        | (u128::from(lane(a, order >> 2)) << 32)
                        | (u128::from(lane(b, order >> 4)) << 64)
                        | (u128::from(lane(b, order >> 6)) << 96)
                };
                self.set_xmm(dst, value);
            }
            SseOp::PshufWords { high, order } => {
                let b = self.read128(src, true, next, mem)?;
                let shift = if high { 64 } else { 0 };
                let half = (b >> shift) as u64;
                let shuffled = (0..4).fold(0u64, |acc, i| {
                    let pick = u64::from((order >> (2 * i)) & 3);
                    acc | (((half >> (pick * 16)) & 0xffff) << (i * 16))
                });
                let keep = u128::from(u64::MAX) << (64 - shift);
                self.set_xmm(dst, (b & keep) | (u128::from(shuffled) << shift));
            }
            SseOp::FloatCmpMask { double, predicate } => {
                let bytes = if double { 8 } else { 4 };
                let b = self.read_scalar(src, bytes, next, mem)?;
                let a = self.xmm_of(dst) as u64 & low_mask(bytes);
                let (mask, flags) = float::compare_mask(double, predicate, a, b, self.float_env());
                self.raise(flags)?;
                self.set_low(dst, mask, bytes);
            }
            SseOp::PackedCmpMask { double, predicate } => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let lane = if double { Size::B64 } else { Size::B32 };
                let env = self.float_env();
                let mut flags = 0;
                let value = lanes(a, b, lane, |x, y| {
                    let (mask, f) = float::compare_mask(double, predicate, x, y, env);
                    flags |= f;
                    mask
                });
                self.raise(flags)?;
                self.set_xmm(dst, value);
            }
            SseOp::PackedArith { op, double } => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let env = self.float_env();
                let lane = if double { Size::B64 } else { Size::B32 };
                let mut flags = 0;
                let value = lanes(a, b, lane, |x, y| {
                    let (result, f) = float::arith(op, double, x, y, env);
                    flags |= f;
                    result
                });
                self.raise(flags)?;
                self.set_xmm(dst, value);
            }
            SseOp::Int(op) => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                self.set_xmm(dst, crate::simd::int_op(op, a, b, self.xmm[0]));
            }
            SseOp::PcmpStr {
                explicit,
                mask,
                wide,
                imm,
            } => {
                use crate::simd::{explicit_length, implicit_length, pcmpstr};
                // Memory need not be aligned.
                let b = self.read128(src, false, next, mem)?;
                let a = self.xmm_of(dst);
                let (la, lb) = if explicit {
                    let size = if wide { Size::B64 } else { Size::B32 };
                    let length = |r: usize| {
                        crate::alu::sign_extend(self.read_reg(Reg::Gpr(r as u8), size), size) as i64
                    };
                    (
                        explicit_length(length(crate::RAX), imm),
                        explicit_length(length(crate::RDX), imm),
                    )
                } else {
                    (implicit_length(a, imm), implicit_length(b, imm))
                };
                let result = pcmpstr(imm, a, b, la, lb);
                let n = result.elements as usize;
                if mask {
                    self.xmm[0] = result.xmm0(imm);
                } else {
                    let index = u64::from(result.index(imm));
                    self.write_reg(Reg::Gpr(crate::RCX as u8), Size::B32, index);
                }
                let flag = |set: bool, bit: u64| if set { bit } else { 0 };
                self.rflags = (self.rflags & !STATUS_FLAGS)
                    | flag(result.mask != 0, FLAG_CF)
                    | flag(lb < n, FLAG_ZF)
                    | flag(la < n, crate::FLAG_SF)
                    | flag(result.mask & 1 != 0, crate::FLAG_OF);
            }
            SseOp::Ptest => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let flag = |set: bool, bit: u64| if set { bit } else { 0 };
                self.rflags = (self.rflags & !STATUS_FLAGS)
                    | flag(a & b == 0, FLAG_ZF)
                    | flag(!a & b == 0, FLAG_CF);
            }
            SseOp::Pmovx { signed, from, to } => {
                let count = 128 / to.bits();
                let bytes = count * from.bits() / 8;
                let low = match src {
                    Operand::Mem(_) => {
                        let size = match bytes {
                            2 => Size::B16,
                            4 => Size::B32,
                            _ => Size::B64,
                        };
                        self.read(src, size, next, mem)?
                    }
                    _ => self.xmm_of(src) as u64,
                };
                let value = (0..count).fold(0u128, |acc, i| {
                    let x = low >> (i * from.bits()) & from.mask();
                    let x = if signed {
                        crate::alu::sign_extend(x, from) & to.mask()
                    } else {
                        x
                    };
                    acc | u128::from(x) << (i * to.bits())
                });
                self.set_xmm(dst, value);
            }
            SseOp::Extract { lane, index } => {
                let v = self.xmm_of(src);
                let value = (v >> (u32::from(index) * lane.bits())) as u64 & lane.mask();
                match dst {
                    // Zero-extended into the whole register.
                    Operand::Reg(reg) => self.write_reg(reg, Size::B64, value),
                    _ => self.write(dst, lane, value, next, mem)?,
                }
            }
            SseOp::Insert { lane, index } => {
                let value = match src {
                    Operand::Reg(reg) => self.read_reg(reg, Size::B64) & lane.mask(),
                    _ => self.read(src, lane, next, mem)?,
                };
                let shift = u32::from(index) * lane.bits();
                let keep = !(u128::from(lane.mask()) << shift);
                let a = self.xmm_of(dst);
                self.set_xmm(dst, a & keep | u128::from(value) << shift);
            }
            SseOp::Insertps(imm) => {
                let element = match src {
                    Operand::Mem(_) => self.read(src, Size::B32, next, mem)? as u32,
                    _ => (self.xmm_of(src) >> (32 * u32::from(imm >> 6))) as u32,
                };
                let shift = 32 * u32::from(imm >> 4 & 3);
                let a =
                    self.xmm_of(dst) & !(0xffff_ffffu128 << shift) | u128::from(element) << shift;
                let value = (0..4).fold(a, |acc, i| {
                    if imm >> i & 1 != 0 {
                        acc & !(0xffff_ffffu128 << (32 * i))
                    } else {
                        acc
                    }
                });
                self.set_xmm(dst, value);
            }
            SseOp::Round {
                double,
                scalar,
                imm,
            } => {
                let env = self.float_env();
                let rc = if imm & 4 != 0 {
                    env.rc
                } else {
                    crate::f80::Rounding::from_control_word(u16::from(imm & 3) << 10)
                };
                let exact = imm & 8 != 0;
                let lane = if double { Size::B64 } else { Size::B32 };
                let (b, keep) = if scalar {
                    let bytes = if double { 8 } else { 4 };
                    let b = u128::from(self.read_scalar(src, bytes, next, mem)?);
                    (b, self.xmm_of(dst) & !u128::from(lane.mask()))
                } else {
                    (self.read128(src, true, next, mem)?, 0)
                };
                let count = if scalar { 1 } else { 128 / lane.bits() };
                let mut flags = 0;
                let value = (0..count).fold(keep, |acc, i| {
                    let x = (b >> (i * lane.bits())) as u64 & lane.mask();
                    let (r, f) = float::round(double, x, rc, exact, env);
                    flags |= f;
                    acc | u128::from(r) << (i * lane.bits())
                });
                self.raise(flags)?;
                self.set_xmm(dst, value);
            }
            SseOp::Dp { double, imm } => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let env = self.float_env();
                let lane = if double { Size::B64 } else { Size::B32 };
                let count = 128 / lane.bits();
                let at = |v: u128, i: u32| (v >> (i * lane.bits())) as u64 & lane.mask();
                let mut flags = 0;
                let mut op = |op: FloatOp, x: u64, y: u64| {
                    let (r, f) = float::arith(op, double, x, y, env);
                    flags |= f;
                    r
                };
                // Products of the selected lanes (+0 for the others),
                // added pairwise.
                let products: Vec<u64> = (0..count)
                    .map(|i| {
                        if imm >> (4 + i) & 1 != 0 {
                            op(FloatOp::Mul, at(a, i), at(b, i))
                        } else {
                            0
                        }
                    })
                    .collect();
                let sum = if double {
                    op(FloatOp::Add, products[0], products[1])
                } else {
                    let low = op(FloatOp::Add, products[0], products[1]);
                    let high = op(FloatOp::Add, products[2], products[3]);
                    op(FloatOp::Add, low, high)
                };
                let value = (0..count).fold(0u128, |acc, i| {
                    if imm >> i & 1 != 0 {
                        acc | u128::from(sum) << (i * lane.bits())
                    } else {
                        acc
                    }
                });
                self.raise(flags)?;
                self.set_xmm(dst, value);
            }
            SseOp::AddSub { double } => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let env = self.float_env();
                let lane = if double { Size::B64 } else { Size::B32 };
                let bits = lane.bits();
                let mask = u128::from(lane.mask());
                let mut flags = 0;
                let value = (0..128 / bits).fold(0u128, |acc, i| {
                    let x = ((a >> (i * bits)) & mask) as u64;
                    let y = ((b >> (i * bits)) & mask) as u64;
                    let op = if i % 2 == 0 {
                        FloatOp::Sub
                    } else {
                        FloatOp::Add
                    };
                    let (result, f) = float::arith(op, double, x, y, env);
                    flags |= f;
                    acc | (u128::from(result) & mask) << (i * bits)
                });
                self.raise(flags)?;
                self.set_xmm(dst, value);
            }
            SseOp::Horizontal { sub, double } => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let env = self.float_env();
                let lane = if double { Size::B64 } else { Size::B32 };
                let bits = lane.bits();
                let mask = u128::from(lane.mask());
                let op = if sub { FloatOp::Sub } else { FloatOp::Add };
                let per_source = 64 / bits;
                let mut flags = 0;
                // Result lanes: pairs of a, then pairs of b.
                let value = (0..128 / bits).fold(0u128, |acc, i| {
                    let (v, pair) = if i < per_source {
                        (a, i)
                    } else {
                        (b, i - per_source)
                    };
                    let x = ((v >> (2 * pair * bits)) & mask) as u64;
                    let y = ((v >> ((2 * pair + 1) * bits)) & mask) as u64;
                    let (result, f) = float::arith(op, double, x, y, env);
                    flags |= f;
                    acc | (u128::from(result) & mask) << (i * bits)
                });
                self.raise(flags)?;
                self.set_xmm(dst, value);
            }
            SseOp::MovDup(dup) => {
                let dword = |v: u128, i: u32| (v >> (32 * i)) as u32;
                let lanes32 =
                    |l: [u32; 4]| (0..4).fold(0u128, |acc, i| acc | u128::from(l[i]) << (32 * i));
                let value = match dup {
                    Dup::Low64 => {
                        let low = u128::from(self.read_scalar(src, 8, next, mem)?);
                        low | low << 64
                    }
                    Dup::OddSingles => {
                        let v = self.read128(src, true, next, mem)?;
                        lanes32([dword(v, 1), dword(v, 1), dword(v, 3), dword(v, 3)])
                    }
                    Dup::EvenSingles => {
                        let v = self.read128(src, true, next, mem)?;
                        lanes32([dword(v, 0), dword(v, 0), dword(v, 2), dword(v, 2)])
                    }
                };
                self.set_xmm(dst, value);
            }
            SseOp::PackedConvert(conversion) => {
                let (value, flags) = self.convert(conversion, src, next, mem)?;
                self.raise(flags)?;
                self.set_xmm(dst, value);
            }
            SseOp::ShiftXmm { kind, lane } => {
                let count = self.read128(src, true, next, mem)? as u64;
                let a = self.xmm_of(dst);
                self.set_xmm(dst, shift_lanes(a, kind, lane, count));
            }
            SseOp::Pmaddwd => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let word = |v: u128, i: u32| i32::from((v >> (16 * i)) as u16 as i16);
                let value = (0..4).fold(0u128, |acc, i| {
                    let sum = (word(a, 2 * i) * word(b, 2 * i))
                        .wrapping_add(word(a, 2 * i + 1) * word(b, 2 * i + 1));
                    acc | (u128::from(sum as u32) << (32 * i))
                });
                self.set_xmm(dst, value);
            }
            SseOp::Psadbw => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let byte = |v: u128, i: u32| (v >> (8 * i)) as u8;
                let value = (0..2).fold(0u128, |acc, half| {
                    let sum: u32 = (0..8)
                        .map(|i| u32::from(byte(a, 8 * half + i).abs_diff(byte(b, 8 * half + i))))
                        .sum();
                    acc | (u128::from(sum) << (64 * half))
                });
                self.set_xmm(dst, value);
            }
            SseOp::Maskmovdqu => {
                // The bytes of dst selected by the top bits of src's bytes,
                // at [rdi], one by one.
                let (data, mask) = (self.xmm_of(dst), self.xmm_of(src));
                let addr = self.regs[crate::RDI];
                for i in 0..16 {
                    if (mask >> (8 * i + 7)) & 1 != 0 {
                        mem.write(addr.wrapping_add(i), &[(data >> (8 * i)) as u8])?;
                    }
                }
            }
            SseOp::ShiftImm { kind, lane, count } => {
                let a = self.xmm_of(dst);
                self.set_xmm(dst, shift_lanes(a, kind, lane, u64::from(count)));
            }
            SseOp::ByteShift { left, count } => {
                let a = self.xmm_of(dst);
                let value = if count > 15 {
                    0
                } else if left {
                    a << (u32::from(count) * 8)
                } else {
                    a >> (u32::from(count) * 8)
                };
                self.set_xmm(dst, value);
            }
            SseOp::MovScalar { double } => {
                let bytes = if double { 8 } else { 4 };
                let value = self.read_scalar(src, bytes, next, mem)?;
                match (dst, src) {
                    // Register to register: only the low lane changes.
                    (Operand::Reg(Reg::Xmm(_)), Operand::Reg(Reg::Xmm(_))) => {
                        self.set_low(dst, value, bytes);
                    }
                    // Load: the rest of the register is zeroed.
                    (Operand::Reg(Reg::Xmm(_)), _) => self.set_xmm(dst, u128::from(value)),
                    (other, _) => self.write(other, scalar_size(bytes), value, next, mem)?,
                }
            }
            SseOp::FloatArith { op, double } => {
                let bytes = if double { 8 } else { 4 };
                let b = self.read_scalar(src, bytes, next, mem)?;
                let a = self.xmm_of(dst) as u64 & low_mask(bytes);
                let (result, flags) = float::arith(op, double, a, b, self.float_env());
                self.raise(flags)?;
                self.set_low(dst, result, bytes);
            }
            SseOp::FloatCompare { double, signaling } => {
                let bytes = if double { 8 } else { 4 };
                let b = self.read_scalar(src, bytes, next, mem)?;
                let a = self.xmm_of(dst) as u64 & low_mask(bytes);
                let ((zf, pf, cf), flags) =
                    float::compare(double, a, b, signaling, self.float_env());
                self.raise(flags)?;
                let flag = |set: bool, bit: u64| if set { bit } else { 0 };
                self.rflags = (self.rflags & !STATUS_FLAGS)
                    | flag(zf, FLAG_ZF)
                    | flag(pf, FLAG_PF)
                    | flag(cf, FLAG_CF);
            }
            SseOp::IntToFloat { double, int_bytes } => {
                let size = gpr_size(int_bytes);
                let raw = self.read(src, size, next, mem)?;
                let value = crate::alu::sign_extend(raw, size) as i64;
                let bytes = if double { 8 } else { 4 };
                let (result, flags) = float::from_int(value, double, self.float_env());
                self.raise(flags)?;
                self.set_low(dst, result, bytes);
            }
            SseOp::FloatToInt {
                double,
                int_bytes,
                truncate,
            } => {
                let bytes = if double { 8 } else { 4 };
                let a = self.read_scalar(src, bytes, next, mem)?;
                let bits = u32::from(int_bytes) * 8;
                let (value, flags) = float::to_int(double, a, bits, truncate, self.float_env());
                self.raise(flags)?;
                let Operand::Reg(reg) = dst else {
                    unreachable!("the decoder puts a GPR here")
                };
                self.write_reg(reg, gpr_size(int_bytes), value);
            }
            SseOp::FloatToFloat { to_double } => {
                if to_double {
                    let a = self.read_scalar(src, 4, next, mem)?;
                    let (result, flags) = float::widen(a as u32, self.float_env());
                    self.raise(flags)?;
                    self.set_low(dst, result, 8);
                } else {
                    let a = self.read_scalar(src, 8, next, mem)?;
                    let (result, flags) = float::narrow(a, self.float_env());
                    self.raise(flags)?;
                    self.set_low(dst, u64::from(result), 4);
                }
            }
            SseOp::FloatSignMask { double } => {
                let value = self.xmm_of(src);
                let lane_bits = if double { 64 } else { 32 };
                let mask = (0..128 / lane_bits).fold(0u64, |acc, i| {
                    acc | (((value >> (i * lane_bits + lane_bits - 1)) & 1) as u64) << i
                });
                let Operand::Reg(reg) = dst else {
                    unreachable!("the decoder puts a GPR here")
                };
                self.write_reg(reg, Size::B32, mask);
            }
            SseOp::Pack { from, signed } => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let bits = from.bits();
                let half = bits / 2;
                let count = 128 / bits;
                let (low, high) = if signed {
                    (-(1i64 << (half - 1)), (1i64 << (half - 1)) - 1)
                } else {
                    (0, (1i64 << half) - 1)
                };
                let narrow = |v: u128, i: u32| {
                    let lane = ((v >> (i * bits)) & u128::from(from.mask())) as u64;
                    let value = crate::alu::sign_extend(lane, from) as i64;
                    (value.clamp(low, high) as u64 & ((1u64 << half) - 1)) as u128
                };
                let value = (0..count).fold(0u128, |acc, i| {
                    acc | (narrow(a, i) << (i * half)) | (narrow(b, i) << ((i + count) * half))
                });
                self.set_xmm(dst, value);
            }
            SseOp::Unpack { high, lane } => {
                let b = self.read128(src, true, next, mem)?;
                let a = self.xmm_of(dst);
                let bits = lane.bits();
                let per_half = 64 / bits;
                let base = if high { per_half } else { 0 };
                let get = |v: u128, i: u32| (v >> (i * bits)) & u128::from(lane.mask());
                let value = (0..per_half).fold(0u128, |acc, i| {
                    acc | (get(a, base + i) << (2 * i * bits))
                        | (get(b, base + i) << ((2 * i + 1) * bits))
                });
                self.set_xmm(dst, value);
            }
        }
        Ok(())
    }

    /// A packed conversion of `src` (an xmm register, m128 or, for the
    /// forms that read two 32-bit lanes, m64), with its MXCSR flags.
    fn convert(
        &self,
        conversion: Conversion,
        src: Operand,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<(u128, u16)> {
        let env = self.float_env();
        let dword = |v: u128, i: u32| (v >> (32 * i)) as u32;
        let qword = |v: u128, i: u32| (v >> (64 * i)) as u64;
        let mut flags = 0;
        let mut keep = |(value, f): (u64, u16)| {
            flags |= f;
            value
        };
        let value = match conversion {
            Conversion::Ps2Pd | Conversion::Dq2Pd => {
                let low = u128::from(self.read_scalar(src, 8, next, mem)?);
                let mut lane = |i| {
                    let x = dword(low, i);
                    u128::from(keep(if conversion == Conversion::Ps2Pd {
                        float::widen(x, env)
                    } else {
                        float::from_int(i64::from(x as i32), true, env)
                    }))
                };
                lane(0) | (lane(1) << 64)
            }
            Conversion::Pd2Ps => {
                let v = self.read128(src, true, next, mem)?;
                let mut lane = |i| {
                    let (result, f) = float::narrow(qword(v, i), env);
                    u128::from(keep((u64::from(result), f)))
                };
                lane(0) | (lane(1) << 32)
            }
            Conversion::Pd2Dq { truncate } => {
                let v = self.read128(src, true, next, mem)?;
                let mut lane =
                    |i| u128::from(keep(float::to_int(true, qword(v, i), 32, truncate, env)));
                lane(0) | (lane(1) << 32)
            }
            Conversion::Dq2Ps => {
                let v = self.read128(src, true, next, mem)?;
                lanes(v, 0, Size::B32, |x, _| {
                    keep(float::from_int(i64::from(x as u32 as i32), false, env))
                })
            }
            Conversion::Ps2Dq { truncate } => {
                let v = self.read128(src, true, next, mem)?;
                lanes(v, 0, Size::B32, |x, _| {
                    keep(float::to_int(false, x, 32, truncate, env))
                })
            }
        };
        Ok((value, flags))
    }

    /// The low `bytes` (4 or 8) of an xmm register, or a memory operand of
    /// that size (no alignment requirement).
    fn read_scalar(
        &self,
        operand: Operand,
        bytes: u8,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<u64> {
        match operand {
            Operand::Reg(Reg::Xmm(n)) => Ok(self.xmm[usize::from(n)] as u64 & low_mask(bytes)),
            other => self.read(other, scalar_size(bytes), next, mem),
        }
    }

    /// Replaces the low `bytes` of xmm `operand`, keeping the rest.
    fn set_low(&mut self, operand: Operand, value: u64, bytes: u8) {
        let mask = u128::from(low_mask(bytes));
        let old = self.xmm_of(operand);
        self.set_xmm(operand, (old & !mask) | (u128::from(value) & mask));
    }

    fn xmm_of(&self, operand: Operand) -> u128 {
        match operand {
            Operand::Reg(Reg::Xmm(n)) => self.xmm[usize::from(n)],
            Operand::Reg(Reg::Mm(n)) => self.mm(n),
            _ => unreachable!("the decoder puts an xmm register here"),
        }
    }

    fn set_xmm(&mut self, operand: Operand, value: u128) {
        match operand {
            Operand::Reg(Reg::Xmm(n)) => self.xmm[usize::from(n)] = value,
            Operand::Reg(Reg::Mm(n)) => self.set_mm(n, value),
            _ => unreachable!("the decoder puts an xmm register here"),
        }
    }

    fn read128(
        &self,
        operand: Operand,
        aligned: bool,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<u128> {
        match operand {
            Operand::Reg(Reg::Xmm(n)) => Ok(self.xmm[usize::from(n)]),
            Operand::Reg(Reg::Mm(n)) => Ok(self.mm(n)),
            Operand::Mem(m) => {
                let addr = self.address(&m, next);
                check_alignment(addr, aligned)?;
                let mut bytes = [0u8; 16];
                mem.read(addr, &mut bytes)?;
                Ok(u128::from_le_bytes(bytes))
            }
            _ => unreachable!("128-bit operands are xmm registers or memory"),
        }
    }

    fn write128(
        &mut self,
        operand: Operand,
        value: u128,
        aligned: bool,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        match operand {
            Operand::Reg(Reg::Xmm(n)) => self.xmm[usize::from(n)] = value,
            Operand::Mem(m) => {
                let addr = self.address(&m, next);
                check_alignment(addr, aligned)?;
                mem.write(addr, &value.to_le_bytes())?;
            }
            _ => unreachable!("128-bit operands are xmm registers or memory"),
        }
        Ok(())
    }
}

fn check_alignment(addr: u64, aligned: bool) -> Exec<()> {
    if aligned && !addr.is_multiple_of(16) {
        Err(Trap::Exception(Exception::GeneralProtection))
    } else {
        Ok(())
    }
}

fn scalar_size(bytes: u8) -> Size {
    if bytes == 8 { Size::B64 } else { Size::B32 }
}

fn low_mask(bytes: u8) -> u64 {
    if bytes == 8 { u64::MAX } else { 0xffff_ffff }
}

fn gpr_size(bytes: u8) -> Size {
    if bytes == 8 { Size::B64 } else { Size::B32 }
}

/// Applies `f` to each pair of `lane`-sized lanes of `a` and `b`.
fn lanes(a: u128, b: u128, lane: Size, mut f: impl FnMut(u64, u64) -> u64) -> u128 {
    let bits = lane.bits();
    let mask = u128::from(lane.mask());
    (0..128 / bits).fold(0u128, |acc, i| {
        let x = ((a >> (i * bits)) & mask) as u64;
        let y = ((b >> (i * bits)) & mask) as u64;
        acc | ((u128::from(f(x, y)) & mask) << (i * bits))
    })
}

/// psll / psrl / psra of every `lane` of `a` by `count`: logical shifts
/// past the lane width give 0, arithmetic ones the sign.
fn shift_lanes(a: u128, kind: ShiftKind, lane: Size, count: u64) -> u128 {
    let bits = u64::from(lane.bits());
    lanes(a, 0, lane, |x, _| match kind {
        ShiftKind::Left if count < bits => (x << count) & lane.mask(),
        ShiftKind::RightLogical if count < bits => x >> count,
        ShiftKind::RightArithmetic => {
            let signed = crate::alu::sign_extend(x, lane) as i64;
            (signed >> count.min(bits - 1)) as u64 & lane.mask()
        }
        _ => 0,
    })
}

fn lane_op(op: LaneOp, lane: Size, x: u64, y: u64) -> u64 {
    let signed = |v: u64| crate::alu::sign_extend(v, lane) as i64;
    let all_ones = lane.mask();
    match op {
        LaneOp::Add => x.wrapping_add(y),
        LaneOp::Sub => x.wrapping_sub(y),
        LaneOp::CmpEq => {
            if x == y {
                all_ones
            } else {
                0
            }
        }
        LaneOp::CmpGt => {
            if signed(x) > signed(y) {
                all_ones
            } else {
                0
            }
        }
        LaneOp::MinU => x.min(y),
        LaneOp::MaxU => x.max(y),
        LaneOp::MinS => {
            if signed(x) <= signed(y) {
                x
            } else {
                y
            }
        }
        LaneOp::MaxS => {
            if signed(x) >= signed(y) {
                x
            } else {
                y
            }
        }
        LaneOp::AddSat { signed: true } | LaneOp::SubSat { signed: true } => {
            let (min, max) = (
                -(1i64 << (lane.bits() - 1)),
                (1i64 << (lane.bits() - 1)) - 1,
            );
            let value = if matches!(op, LaneOp::AddSat { .. }) {
                signed(x) + signed(y)
            } else {
                signed(x) - signed(y)
            };
            value.clamp(min, max) as u64 & all_ones
        }
        LaneOp::AddSat { signed: false } => (x + y).min(all_ones),
        LaneOp::SubSat { signed: false } => x.saturating_sub(y),
        LaneOp::Avg => (x + y + 1) >> 1,
        LaneOp::MulLow => x.wrapping_mul(y) & all_ones,
        LaneOp::MulHigh { signed: true } => {
            ((signed(x) * signed(y)) >> lane.bits()) as u64 & all_ones
        }
        LaneOp::MulHigh { signed: false } => (x * y) >> lane.bits(),
    }
}
