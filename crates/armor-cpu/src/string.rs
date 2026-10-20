//! String instructions (movs, cmps, stos, lods, scas) and their rep forms.
//!
//! A rep instruction commits every completed iteration (rcx, rsi, rdi), so a
//! fault in the middle leaves the same state as on hardware. `rep stos` and
//! `rep movs` going forward are copied in blocks when that cannot change
//! the result.
//!
//! With 32-bit addresses (compatibility mode) the registers are esi, edi
//! and ecx: they wrap at 4 GiB.
use alloc::vec;

use armor_guest::GuestMemory;
use armor_x86::{AluOp, Reg, Rep, Size, StringOp};

use crate::{Cpu, Exec, FLAG_DF, FLAG_ZF, RAX, RCX, RDI, RSI, alu};

/// Largest block moved at once by the fast path.
const BLOCK: usize = 4096;

impl Cpu {
    pub(crate) fn string(
        &mut self,
        op: StringOp,
        rep: Option<Rep>,
        size: Size,
        addr32: bool,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        let mask = if addr32 { 0xffff_ffff } else { u64::MAX };
        let Some(rep) = rep else {
            return self.string_once(op, size, mask, mem);
        };
        while self.regs[RCX] & mask != 0 {
            if self.string_block(op, size, mask, mem)? {
                continue;
            }
            self.string_once(op, size, mask, mem)?;
            self.regs[RCX] = (self.regs[RCX] & mask) - 1;
            if matches!(op, StringOp::Cmps | StringOp::Scas) {
                let zf = self.flag(FLAG_ZF);
                let stop = match rep {
                    Rep::Rep => !zf,
                    Rep::Repne => zf,
                };
                if stop {
                    break;
                }
            }
        }
        Ok(())
    }

    /// One iteration, without touching rcx. `mask` is the address width.
    fn string_once(
        &mut self,
        op: StringOp,
        size: Size,
        mask: u64,
        mem: &impl GuestMemory,
    ) -> Exec<()> {
        let n = size.bytes() as u64;
        let step = if self.flag(FLAG_DF) {
            n.wrapping_neg()
        } else {
            n
        };
        let (rsi, rdi) = (self.regs[RSI] & mask, self.regs[RDI] & mask);
        let step_rsi = |cpu: &mut Cpu| cpu.regs[RSI] = rsi.wrapping_add(step) & mask;
        let step_rdi = |cpu: &mut Cpu| cpu.regs[RDI] = rdi.wrapping_add(step) & mask;
        let accumulator = Reg::Gpr(RAX as u8);
        match op {
            StringOp::Movs => {
                let value = self.load(rsi, size, mem)?;
                mem.write(rdi, &value.to_le_bytes()[..size.bytes()])?;
                step_rsi(self);
                step_rdi(self);
            }
            StringOp::Stos => {
                let value = self.read_reg(accumulator, size);
                mem.write(rdi, &value.to_le_bytes()[..size.bytes()])?;
                step_rdi(self);
            }
            StringOp::Lods => {
                let value = self.load(rsi, size, mem)?;
                self.write_reg(accumulator, size, value);
                step_rsi(self);
            }
            StringOp::Cmps => {
                let a = self.load(rsi, size, mem)?;
                let b = self.load(rdi, size, mem)?;
                let (_, flags) = alu::alu(AluOp::Cmp, size, a, b, false);
                self.set_status(flags);
                step_rsi(self);
                step_rdi(self);
            }
            StringOp::Scas => {
                let a = self.read_reg(accumulator, size);
                let b = self.load(rdi, size, mem)?;
                let (_, flags) = alu::alu(AluOp::Cmp, size, a, b, false);
                self.set_status(flags);
                step_rdi(self);
            }
        }
        Ok(())
    }

    /// Fast path for forward `rep stos` / `rep movs`: moves up to `BLOCK`
    /// bytes in one access. Returns false (doing nothing) when the block
    /// cannot be done at once: other instructions, DF set, overlapping
    /// regions, or a fault inside the block (the element loop then stops at
    /// the exact faulting element). `mask` is the address width; a block
    /// that would wrap around it is not taken.
    fn string_block(
        &mut self,
        op: StringOp,
        size: Size,
        mask: u64,
        mem: &impl GuestMemory,
    ) -> Exec<bool> {
        if self.flag(FLAG_DF) || !matches!(op, StringOp::Stos | StringOp::Movs) {
            return Ok(false);
        }
        let n = size.bytes();
        let count = (self.regs[RCX] & mask).min((BLOCK / n) as u64) as usize;
        if count < 2 {
            return Ok(false);
        }
        let len = count * n;
        let (rsi, rdi) = (self.regs[RSI] & mask, self.regs[RDI] & mask);
        let wraps = |addr: u64| {
            addr.checked_add(len as u64)
                .is_none_or(|end| end - 1 > mask)
        };
        if wraps(rdi) || (op == StringOp::Movs && wraps(rsi)) {
            return Ok(false);
        }
        let mut block = vec![0u8; len];
        match op {
            StringOp::Stos => {
                let value = self.read_reg(Reg::Gpr(RAX as u8), size).to_le_bytes();
                for element in block.chunks_exact_mut(n) {
                    element.copy_from_slice(&value[..n]);
                }
            }
            _ => {
                // An element-by-element forward copy into a destination that
                // starts inside the source repeats the source pattern: keep
                // those exact semantics by not taking the fast path.
                if rdi > rsi && rdi - rsi < len as u64 {
                    return Ok(false);
                }
                if mem.read(rsi, &mut block).is_err() {
                    return Ok(false);
                }
            }
        }
        if mem.write(rdi, &block).is_err() {
            return Ok(false);
        }
        let advance = len as u64;
        self.regs[RDI] = rdi.wrapping_add(advance);
        if op == StringOp::Movs {
            self.regs[RSI] = rsi.wrapping_add(advance);
        }
        self.regs[RCX] = (self.regs[RCX] & mask) - count as u64;
        Ok(true)
    }
}
