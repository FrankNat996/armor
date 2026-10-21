//! AVX and AVX2: the VEX-encoded forms of the SSE operations (`Op::Vex`),
//! run by the legacy operations of `sse.rs` on the architectural
//! registers (docs/tech/avx.md).
//!
//! A VEX.128 operation `dst = op(src1, src)` is the legacy `op dst, src`
//! with dst first set to src1 (VEX.vvvv), then bits 255:128 of dst zeroed.
//! A VEX.256 one runs the legacy operation on each 128-bit half (the low
//! halves in `xmm`, the high ones in `ymm_hi`), with the adjustments of
//! the operations whose halves are not independent. Memory operands need
//! no alignment, except for the moves that require it (vmovaps, vmovdqa,
//! the non-temporal ones: 16 bytes, 32 for the 256-bit forms).

use armor_guest::GuestMemory;
use armor_x86::{Conversion, IntOp, Mem, Op, Operand, Reg, Size, SseOp};

use crate::{Cpu, Exception, Exec, FLAG_CF, FLAG_ZF, STATUS_FLAGS, Trap};

/// The second source of one half: an operand as it is (memory, an
/// immediate, a general register), or a 128-bit value, which goes into a
/// borrowed xmm register for the legacy operation.
#[derive(Clone, Copy)]
enum Second {
    Operand(Operand),
    Value(u128),
}

impl Cpu {
    /// `Op::Vex`.
    pub(crate) fn vex(&mut self, insn: &Op, next: u64, mem: &impl GuestMemory) -> Exec<()> {
        let Op::Vex {
            op,
            dst,
            src1,
            src,
            wide,
            ..
        } = *insn
        else {
            unreachable!("called for Op::Vex")
        };
        self.vex_unaligned = !matches!(op, SseOp::Mov128 { aligned: true });
        let result = if wide {
            self.vex256(op, dst, src1, src, next, mem)
        } else {
            self.vex128(op, dst, src1, src, next, mem)
        };
        self.vex_unaligned = false;
        result
    }

    fn vex128(
        &mut self,
        op: SseOp,
        dst: Operand,
        src1: Option<u8>,
        src: Operand,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        let Operand::Reg(Reg::Xmm(d)) = dst else {
            // A store, or a general register written: no xmm changes.
            return self.sse_ops(op, dst, src, next, mem);
        };
        if !writes_destination(op) {
            self.sse_ops(op, dst, src, next, mem)?;
            // pcmpistrm and pcmpestrm write xmm0.
            if let SseOp::PcmpStr { mask: true, .. } = op {
                self.ymm_hi[0] = 0;
            }
            return Ok(());
        }
        let d = usize::from(d);
        let first = src1.map_or(self.xmm[d], |s| self.xmm[usize::from(s)]);
        let value = self.half(op, d, first, self.low(src), next, mem)?;
        self.xmm[d] = value;
        self.ymm_hi[d] = 0;
        Ok(())
    }

    fn vex256(
        &mut self,
        op: SseOp,
        dst: Operand,
        src1: Option<u8>,
        src: Operand,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        // The aligned 256-bit moves need 32 bytes.
        if op == (SseOp::Mov128 { aligned: true })
            && let (Operand::Mem(m), _) | (_, Operand::Mem(m)) = (dst, src)
            && !self.address(&m, next).is_multiple_of(32)
        {
            return Err(Trap::Exception(Exception::GeneralProtection));
        }
        match dst {
            // vmovups, vmovdqu, vmovntps, … m256, ymm: the low half, then
            // the high one.
            Operand::Mem(m) => {
                let Operand::Reg(Reg::Xmm(s)) = src else {
                    unreachable!("256-bit stores come from a register")
                };
                let s = usize::from(s);
                self.sse_ops(op, dst, src, next, mem)?;
                let high = self.ymm_hi[s];
                self.with_borrowed(&[s], high, |cpu, t| {
                    let dst = Operand::Mem(offset(m, 16));
                    cpu.sse_ops(op, dst, Operand::Reg(Reg::Xmm(t)), next, mem)
                })
            }
            // vmovmskps, vmovmskpd, vpmovmskb r32, ymm.
            Operand::Reg(Reg::Gpr(_)) => self.mask_256(op, dst, src),
            Operand::Reg(Reg::Xmm(d)) if op == SseOp::Ptest => self.ptest_256(d, src, next, mem),
            Operand::Reg(Reg::Xmm(d)) => {
                let d = usize::from(d);
                let (low1, high1) = match src1 {
                    Some(s) => (self.xmm[usize::from(s)], self.ymm_hi[usize::from(s)]),
                    None => (self.xmm[d], self.ymm_hi[d]),
                };
                let (low2, high2) = (self.low(src), self.high(src));
                let (low, high) = match op {
                    // ymm ← xmm/m128: each half from 64 bits of the source.
                    SseOp::PackedConvert(Conversion::Ps2Pd | Conversion::Dq2Pd) => {
                        let upper = self.part(src, 8);
                        let low = self.half(op, d, low1, low2, next, mem)?;
                        (low, self.half(op, d, low1, upper, next, mem)?)
                    }
                    // xmm ← ymm/m256: 64 bits from each half.
                    SseOp::PackedConvert(Conversion::Pd2Ps | Conversion::Pd2Dq { .. }) => {
                        let low = self.half(op, d, low1, low2, next, mem)?;
                        let high = self.half(op, d, high1, high2, next, mem)?;
                        ((low & u128::from(u64::MAX)) | (high << 64), 0)
                    }
                    // vpmovsx, vpmovzx ymm, xmm/m128: the second half of
                    // the lanes it widens.
                    SseOp::Pmovx { from, to, .. } => {
                        let upper = self.part(src, 16 * from.bits() / to.bits());
                        let low = self.half(op, d, low1, low2, next, mem)?;
                        (low, self.half(op, d, low1, upper, next, mem)?)
                    }
                    // The count of a shift by xmm is the same for both
                    // halves.
                    SseOp::ShiftXmm { .. } => {
                        let low = self.half(op, d, low1, low2, next, mem)?;
                        (low, self.half(op, d, high1, low2, next, mem)?)
                    }
                    _ => {
                        let low = self.half(op, d, low1, low2, next, mem)?;
                        (low, self.half(high_half(op), d, high1, high2, next, mem)?)
                    }
                };
                self.xmm[d] = low;
                self.ymm_hi[d] = high;
                Ok(())
            }
            _ => unreachable!("VEX destinations are registers or memory"),
        }
    }

    /// The legacy `op` on `first` (in xmm `d`) and `second`: its result,
    /// with every register as it was. A fault changes nothing.
    fn half(
        &mut self,
        op: SseOp,
        d: usize,
        first: u128,
        second: Second,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<u128> {
        let old = self.xmm[d];
        let dst = Operand::Reg(Reg::Xmm(d as u8));
        let result = match second {
            Second::Operand(src) => {
                self.xmm[d] = first;
                self.sse_ops(op, dst, src, next, mem)
            }
            Second::Value(value) => self.with_borrowed(&[d], value, |cpu, t| {
                cpu.xmm[d] = first;
                cpu.sse_ops(op, dst, Operand::Reg(Reg::Xmm(t)), next, mem)
            }),
        };
        let value = self.xmm[d];
        self.xmm[d] = old;
        result.map(|()| value)
    }

    /// Runs `f` with `value` in an xmm register borrowed for it (not xmm0,
    /// which some operations read implicitly, nor one of `busy`), and
    /// gives the register its value back.
    fn with_borrowed(
        &mut self,
        busy: &[usize],
        value: u128,
        f: impl FnOnce(&mut Self, u8) -> Exec<()>,
    ) -> Exec<()> {
        let t = (1..16)
            .find(|t| !busy.contains(t))
            .expect("16 registers, few busy");
        let saved = self.xmm[t];
        self.xmm[t] = value;
        let result = f(self, t as u8);
        self.xmm[t] = saved;
        result
    }

    /// The low half of a source.
    fn low(&self, src: Operand) -> Second {
        match src {
            Operand::Reg(Reg::Xmm(s)) => Second::Value(self.xmm[usize::from(s)]),
            other => Second::Operand(other),
        }
    }

    /// The high half of a 256-bit source: bits 255:128 of a register, the
    /// 16 bytes after the low ones in memory, an immediate as it is.
    fn high(&self, src: Operand) -> Second {
        match src {
            Operand::Reg(Reg::Xmm(s)) => Second::Value(self.ymm_hi[usize::from(s)]),
            Operand::Mem(m) => Second::Operand(Operand::Mem(offset(m, 16))),
            other => Second::Operand(other),
        }
    }

    /// A 128-bit source from its byte `bytes` on.
    fn part(&self, src: Operand, bytes: u32) -> Second {
        match src {
            Operand::Reg(Reg::Xmm(s)) => Second::Value(self.xmm[usize::from(s)] >> (8 * bytes)),
            Operand::Mem(m) => Second::Operand(Operand::Mem(offset(m, i64::from(bytes)))),
            other => Second::Operand(other),
        }
    }

    /// vmovmskps, vmovmskpd, vpmovmskb of a ymm register: the bits of the
    /// high half above those of the low one.
    fn mask_256(&mut self, op: SseOp, dst: Operand, src: Operand) -> Exec<()> {
        let (Operand::Reg(reg), Operand::Reg(Reg::Xmm(s))) = (dst, src) else {
            unreachable!("register operands")
        };
        let lanes: u32 = match op {
            SseOp::FloatSignMask { double: true } => 2,
            SseOp::FloatSignMask { double: false } => 4,
            _ => 16,
        };
        let s = usize::from(s);
        let width = 128 / lanes;
        let mask = |value: u128| {
            (0..lanes).fold(0u64, |m, i| {
                m | (((value >> (i * width + width - 1)) & 1) as u64) << i
            })
        };
        let value = mask(self.xmm[s]) | mask(self.ymm_hi[s]) << lanes;
        self.write_reg(reg, Size::B32, value);
        Ok(())
    }

    /// vptest ymm, ymm/m256: ZF if `a & b` is 0, CF if `!a & b` is 0, on
    /// 256 bits; the other status flags cleared.
    fn ptest_256(&mut self, d: u8, src: Operand, next: u64, mem: &impl GuestMemory) -> Exec<()> {
        let d = usize::from(d);
        let (low, high) = match src {
            Operand::Reg(Reg::Xmm(s)) => (self.xmm[usize::from(s)], self.ymm_hi[usize::from(s)]),
            Operand::Mem(m) => {
                let mut bytes = [0u8; 32];
                mem.read(self.address(&m, next), &mut bytes)?;
                (
                    u128::from_le_bytes(bytes[..16].try_into().expect("16 bytes")),
                    u128::from_le_bytes(bytes[16..].try_into().expect("16 bytes")),
                )
            }
            _ => unreachable!("vptest reads a register or memory"),
        };
        let (a_low, a_high) = (self.xmm[d], self.ymm_hi[d]);
        let zf = a_low & low == 0 && a_high & high == 0;
        let cf = !a_low & low == 0 && !a_high & high == 0;
        self.rflags = (self.rflags & !STATUS_FLAGS)
            | if zf { FLAG_ZF } else { 0 }
            | if cf { FLAG_CF } else { 0 };
        Ok(())
    }

    /// vzeroupper (bits 255:128 of every ymm register zeroed) and
    /// vzeroall (the whole registers).
    pub(crate) fn vzero(&mut self, all: bool) {
        self.ymm_hi = [0; 16];
        if all {
            self.xmm = [0; 16];
        }
    }
}

/// The operation on the high half of a VEX.256 instruction: the same,
/// except for the immediates that hold one field per 128-bit lane.
fn high_half(op: SseOp) -> SseOp {
    match op {
        // vshufpd: bits 3:2 for the high lane.
        SseOp::Shuffle {
            double: true,
            order,
        } => SseOp::Shuffle {
            double: true,
            order: order >> 2,
        },
        // vblendps: bits 7:4; vblendpd: bits 3:2; vpblendw: the same 8.
        SseOp::Int(IntOp::Blend { lane, imm }) => {
            let shift = match lane {
                Size::B32 => 4,
                Size::B64 => 2,
                _ => 0,
            };
            SseOp::Int(IntOp::Blend {
                lane,
                imm: imm >> shift,
            })
        }
        // vmpsadbw ymm: bits 5:3 for the high lane.
        SseOp::Int(IntOp::Mpsadbw(imm)) => SseOp::Int(IntOp::Mpsadbw(imm >> 3)),
        other => other,
    }
}

/// `m` moved by `bytes`.
fn offset(m: Mem, bytes: i64) -> Mem {
    Mem {
        disp: m.disp.wrapping_add(bytes),
        ..m
    }
}

/// Whether `op` writes its xmm operand `dst`: not the compares to flags
/// (comiss, ptest), the string compares (`dst` is their first source), or
/// maskmovdqu (which stores).
fn writes_destination(op: SseOp) -> bool {
    !matches!(
        op,
        SseOp::FloatCompare { .. } | SseOp::Ptest | SseOp::PcmpStr { .. } | SseOp::Maskmovdqu
    )
}
