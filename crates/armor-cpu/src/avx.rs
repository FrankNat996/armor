//! AVX: the VEX-encoded forms of the SSE operations (`Op::Vex`), run by
//! the legacy operations of `sse.rs` on the architectural registers
//! (docs/tech/avx.md).
//!
//! A VEX.128 operation `dst = op(src1, src)` is the legacy `op dst, src`
//! with dst first set to src1 (VEX.vvvv), then bits 255:128 of dst zeroed.
//! Memory operands need no alignment, except for the moves that require
//! it (vmovaps, vmovdqa, the non-temporal ones).

use armor_guest::GuestMemory;
use armor_x86::{Op, Operand, Reg, SseOp};

use crate::{Cpu, Exec, Trap};

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
        if wide {
            return Err(Trap::Unsupported("256-bit AVX instructions"));
        }
        self.vex_unaligned = !matches!(op, SseOp::Mov128 { aligned: true });
        let result = self.vex128(op, dst, src1, src, next, mem);
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
        let old = self.xmm[d];
        let first = src1.map_or(old, |s| self.xmm[usize::from(s)]);
        // dst is also the second source and src1 is another register:
        // the second source's value goes into a register borrowed for the
        // instruction (not one the operation reads, nor xmm0, which some
        // operations read implicitly) and given back afterwards.
        let borrowed = match src {
            Operand::Reg(Reg::Xmm(s)) if usize::from(s) == d && first != old => {
                let used = |t: usize| t == 0 || t == d || src1.is_some_and(|s| usize::from(s) == t);
                let t = (1..16)
                    .find(|&t| !used(t))
                    .expect("16 registers, at most 3 used");
                let saved = self.xmm[t];
                self.xmm[t] = old;
                Some((t, saved))
            }
            _ => None,
        };
        let src = match borrowed {
            Some((t, _)) => Operand::Reg(Reg::Xmm(t as u8)),
            None => src,
        };
        self.xmm[d] = first;
        let result = self.sse_ops(op, dst, src, next, mem);
        if let Some((t, saved)) = borrowed {
            self.xmm[t] = saved;
        }
        match result {
            Ok(()) => {
                self.ymm_hi[d] = 0;
                Ok(())
            }
            Err(trap) => {
                self.xmm[d] = old;
                Err(trap)
            }
        }
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

/// Whether `op` writes its xmm operand `dst`: not the compares to flags
/// (comiss, ptest), the string compares (`dst` is their first source), or
/// maskmovdqu (which stores).
fn writes_destination(op: SseOp) -> bool {
    !matches!(
        op,
        SseOp::FloatCompare { .. } | SseOp::Ptest | SseOp::PcmpStr { .. } | SseOp::Maskmovdqu
    )
}
