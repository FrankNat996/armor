//! The instructions AVX and AVX2 add, with no legacy SSE form (`Op::Avx`,
//! `AvxOp`): broadcasts, the moves and permutations across the 128-bit
//! halves, the variable blends, permutations and shifts, the masked
//! moves and vtestps/vtestpd (Intel SDM vol. 2; docs/tech/avx.md). They
//! work on whole vectors, the low half in `xmm` and the high one in
//! `ymm_hi`; a 128-bit form zeroes the high half of its destination.

use armor_guest::GuestMemory;
use armor_x86::{AvxOp, Op, Operand, Reg, ShiftKind};

use crate::{Cpu, Exec, FLAG_CF, FLAG_ZF, STATUS_FLAGS};

/// A 256-bit vector as 32 bytes, lowest first.
type Bytes = [u8; 32];

impl Cpu {
    /// `Op::Avx`.
    pub(crate) fn avx(&mut self, insn: &Op, next: u64, mem: &impl GuestMemory) -> Exec<()> {
        let Op::Avx {
            op,
            dst,
            src1,
            src,
            wide,
            ..
        } = *insn
        else {
            unreachable!("called for Op::Avx")
        };
        let size = if wide { 32 } else { 16 };
        let first = || src1.map_or([0; 32], |n| self.vector(n));
        let result = match op {
            AvxOp::Broadcast(lane) => {
                let bytes = lane.bytes();
                let element = match src {
                    Operand::Reg(Reg::Xmm(n)) => self.vector(n),
                    other => self.read_bytes(other, bytes, next, mem)?,
                };
                core::array::from_fn(|i| if i < size { element[i % bytes] } else { 0 })
            }
            AvxOp::Broadcast128 => {
                let half = self.read_bytes(src, 16, next, mem)?;
                core::array::from_fn(|i| half[i % 16])
            }
            AvxOp::Insert128(index) => {
                let mut result = first();
                let half = self.read_bytes(src, 16, next, mem)?;
                let at = usize::from(index) * 16;
                result[at..at + 16].copy_from_slice(&half[..16]);
                result
            }
            AvxOp::Extract128(index) => {
                let Operand::Reg(Reg::Xmm(s)) = src else {
                    unreachable!("the source is a ymm register")
                };
                let at = usize::from(index) * 16;
                let mut half = [0u8; 32];
                half[..16].copy_from_slice(&self.vector(s)[at..at + 16]);
                if let Operand::Mem(m) = dst {
                    mem.write(self.address(&m, next), &half[..16])?;
                    return Ok(());
                }
                half
            }
            AvxOp::Perm2x128(imm) => {
                let (a, b) = (first(), self.read_bytes(src, 32, next, mem)?);
                let mut result = [0u8; 32];
                for k in 0..2 {
                    let control = imm >> (4 * k);
                    if control & 8 == 0 {
                        let from = if control & 2 == 0 { &a } else { &b };
                        let at = usize::from(control & 1) * 16;
                        result[k * 16..k * 16 + 16].copy_from_slice(&from[at..at + 16]);
                    }
                }
                result
            }
            AvxOp::Permq(imm) => {
                let data = self.read_bytes(src, 32, next, mem)?;
                permute(&data, 8, |i| usize::from(imm >> (2 * i)) & 3)
            }
            AvxOp::Permd => {
                let (index, data) = (first(), self.read_bytes(src, 32, next, mem)?);
                permute(&data, 4, |i| (get(&index, 4, i) & 7) as usize)
            }
            AvxOp::Permil { double, imm } => {
                // The selector of element i, within its 128-bit half.
                let lane = if double { 8 } else { 4 };
                let per_half = 16 / lane;
                let (data, select): (Bytes, Bytes) = match imm {
                    Some(_) => (self.read_bytes(src, size, next, mem)?, [0; 32]),
                    None => (first(), self.read_bytes(src, size, next, mem)?),
                };
                let mut result = permute(&data, lane, |i| {
                    let base = i / per_half * per_half;
                    let choice = match (imm, double) {
                        (Some(imm), false) => usize::from(imm >> (2 * (i % 4))) & 3,
                        (Some(imm), true) => usize::from(imm >> i) & 1,
                        (None, false) => (get(&select, 4, i) & 3) as usize,
                        (None, true) => ((get(&select, 8, i) >> 1) & 1) as usize,
                    };
                    base + choice
                });
                result[size..].fill(0);
                result
            }
            AvxOp::BlendV { lane, mask } => {
                let (a, b) = (first(), self.read_bytes(src, size, next, mem)?);
                let m = self.vector(mask);
                let bytes = lane.bytes();
                let mut result = a;
                for i in 0..size / bytes {
                    if m[i * bytes + bytes - 1] & 0x80 != 0 {
                        result[i * bytes..(i + 1) * bytes]
                            .copy_from_slice(&b[i * bytes..(i + 1) * bytes]);
                    }
                }
                result[size..].fill(0);
                result
            }
            AvxOp::Blendd(imm) => {
                let (a, b) = (first(), self.read_bytes(src, size, next, mem)?);
                let mut result = a;
                for i in 0..size / 4 {
                    if imm >> i & 1 == 1 {
                        result[i * 4..i * 4 + 4].copy_from_slice(&b[i * 4..i * 4 + 4]);
                    }
                }
                result[size..].fill(0);
                result
            }
            AvxOp::ShiftVar { kind, lane } => {
                let (a, counts) = (first(), self.read_bytes(src, size, next, mem)?);
                let (bytes, bits) = (lane.bytes(), u64::from(lane.bits()));
                let mut result = [0u8; 32];
                for i in 0..size / bytes {
                    let (x, count) = (get(&a, bytes, i), get(&counts, bytes, i));
                    let value = match kind {
                        ShiftKind::Left if count < bits => x << count,
                        ShiftKind::RightLogical if count < bits => x >> count,
                        ShiftKind::RightArithmetic => {
                            let x = crate::alu::sign_extend(x, lane) as i64;
                            (x >> count.min(bits - 1)) as u64
                        }
                        _ => 0,
                    };
                    set(&mut result, bytes, i, value);
                }
                result
            }
            AvxOp::MaskMov { lane, store } => {
                let mask = first();
                let bytes = lane.bytes();
                let enabled = |i: usize| mask[i * bytes + bytes - 1] & 0x80 != 0;
                let Operand::Mem(m) = (if store { dst } else { src }) else {
                    unreachable!("masked moves access memory")
                };
                let base = self.address(&m, next);
                if store {
                    let Operand::Reg(Reg::Xmm(s)) = src else {
                        unreachable!("masked stores come from a register")
                    };
                    let data = self.vector(s);
                    // Lane by lane: a masked-off lane is not accessed ⚠️ (a
                    // fault on a later lane leaves the earlier ones written).
                    for i in (0..size / bytes).filter(|&i| enabled(i)) {
                        let addr = base.wrapping_add((i * bytes) as u64);
                        mem.write(addr, &data[i * bytes..(i + 1) * bytes])?;
                    }
                    return Ok(());
                }
                let mut result = [0u8; 32];
                for i in (0..size / bytes).filter(|&i| enabled(i)) {
                    let addr = base.wrapping_add((i * bytes) as u64);
                    mem.read(addr, &mut result[i * bytes..(i + 1) * bytes])?;
                }
                result
            }
            AvxOp::Test { double } => {
                let Operand::Reg(Reg::Xmm(d)) = dst else {
                    unreachable!("vtestps reads a register")
                };
                let (a, b) = (self.vector(d), self.read_bytes(src, size, next, mem)?);
                let lane = if double { 8 } else { 4 };
                let sign = |i: usize, x: u8| x & 0x80 != 0 && i % lane == lane - 1;
                let zf = (0..size).all(|i| !sign(i, a[i] & b[i]));
                let cf = (0..size).all(|i| !sign(i, !a[i] & b[i]));
                self.rflags = (self.rflags & !STATUS_FLAGS)
                    | if zf { FLAG_ZF } else { 0 }
                    | if cf { FLAG_CF } else { 0 };
                return Ok(());
            }
        };
        let Operand::Reg(Reg::Xmm(d)) = dst else {
            unreachable!("the other forms write a register")
        };
        self.set_vector(d, &result, wide);
        Ok(())
    }

    /// ymm register `n` as bytes.
    fn vector(&self, n: u8) -> Bytes {
        let n = usize::from(n);
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&self.xmm[n].to_le_bytes());
        bytes[16..].copy_from_slice(&self.ymm_hi[n].to_le_bytes());
        bytes
    }

    /// Writes ymm register `n`: all 32 bytes when `wide`, else the low 16
    /// and zeros above.
    fn set_vector(&mut self, n: u8, bytes: &Bytes, wide: bool) {
        let n = usize::from(n);
        self.xmm[n] = u128::from_le_bytes(bytes[..16].try_into().expect("16 bytes"));
        self.ymm_hi[n] = if wide {
            u128::from_le_bytes(bytes[16..].try_into().expect("16 bytes"))
        } else {
            0
        };
    }

    /// The first `size` bytes of a register or of memory (no alignment),
    /// the rest zero.
    fn read_bytes(
        &self,
        src: Operand,
        size: usize,
        next: u64,
        mem: &impl GuestMemory,
    ) -> Exec<Bytes> {
        let mut bytes = match src {
            Operand::Reg(Reg::Xmm(n)) => self.vector(n),
            Operand::Mem(m) => {
                let mut bytes = [0u8; 32];
                mem.read(self.address(&m, next), &mut bytes[..size])?;
                bytes
            }
            _ => unreachable!("a vector source is a register or memory"),
        };
        bytes[size..].fill(0);
        Ok(bytes)
    }
}

/// Lane `i` of `lane` bytes.
fn get(v: &Bytes, lane: usize, i: usize) -> u64 {
    let mut word = [0u8; 8];
    word[..lane].copy_from_slice(&v[i * lane..(i + 1) * lane]);
    u64::from_le_bytes(word)
}

fn set(v: &mut Bytes, lane: usize, i: usize, value: u64) {
    v[i * lane..(i + 1) * lane].copy_from_slice(&value.to_le_bytes()[..lane]);
}

/// Lane i of the result is lane `pick(i)` of `data`, for 32 bytes.
fn permute(data: &Bytes, lane: usize, pick: impl Fn(usize) -> usize) -> Bytes {
    let mut result = [0u8; 32];
    for i in 0..32 / lane {
        let from = pick(i);
        result[i * lane..(i + 1) * lane].copy_from_slice(&data[from * lane..(from + 1) * lane]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_and_permutations() {
        let data: Bytes = core::array::from_fn(|i| i as u8);
        assert_eq!(get(&data, 4, 1), 0x0706_0504);
        // vpermq with 0x1b reverses the qwords.
        let reversed = permute(&data, 8, |i| 3 - i);
        assert_eq!(get(&reversed, 8, 0), get(&data, 8, 3));
        let mut v = [0u8; 32];
        set(&mut v, 2, 3, 0xabcd);
        assert_eq!(get(&v, 2, 3), 0xabcd);
    }
}
