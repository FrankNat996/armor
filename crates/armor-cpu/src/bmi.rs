//! BMI1, BMI2 and the zero counts (tzcnt, lzcnt): pure functions on
//! general-register values (Intel SDM vol. 2; docs/tech/avx.md).
//!
//! The flags an operation leaves undefined get values chosen here, as for
//! the other instructions (docs/tech/x86-64-flags.md): PF (and SF where it
//! is undefined) from the result, AF 0. The differential tests mask them.

use armor_x86::{BmiOp, Size};

use crate::alu::{result_flags, sign_extend};
use crate::{FLAG_CF, FLAG_ZF};

/// The result of `op` with `src` the r/m operand and `vvvv` the value of
/// the VEX.vvvv register, and the status flags, or `None` for the
/// operations that leave them unchanged. Not for mulx (`Cpu::execute`).
pub(crate) fn bmi(op: BmiOp, size: Size, src: u64, vvvv: u64) -> (u64, Option<u64>) {
    let bits = size.bits();
    let mask = size.mask();
    let (src, vvvv) = (src & mask, vvvv & mask);
    let carry = |on: bool| if on { FLAG_CF } else { 0 };
    // SF, ZF, PF from the result; CF, OF, AF cleared.
    let logic = |r: u64| result_flags(r, size);
    match op {
        BmiOp::Andn => {
            let r = !vvvv & src & mask;
            (r, Some(logic(r)))
        }
        BmiOp::Bextr => {
            let (start, len) = (vvvv & 0xff, (vvvv >> 8) & 0xff);
            let shifted = if start >= u64::from(bits) {
                0
            } else {
                src >> start
            };
            let r = if len >= u64::from(bits) {
                shifted
            } else {
                shifted & ((1 << len) - 1)
            };
            (r, Some(logic(r)))
        }
        BmiOp::Blsi => {
            let r = src.isolate_lowest_one() & mask;
            (r, Some(logic(r) | carry(src != 0)))
        }
        BmiOp::Blsmsk => {
            let r = (src.wrapping_sub(1) ^ src) & mask;
            (r, Some(logic(r) & !FLAG_ZF | carry(src == 0)))
        }
        BmiOp::Blsr => {
            let r = src.wrapping_sub(1) & src & mask;
            (r, Some(logic(r) | carry(src == 0)))
        }
        BmiOp::Bzhi => {
            let index = vvvv & 0xff;
            let r = if index >= u64::from(bits) {
                src
            } else {
                src & ((1 << index) - 1)
            };
            (r, Some(logic(r) | carry(index >= u64::from(bits))))
        }
        BmiOp::Pdep => (deposit(vvvv, src), None),
        BmiOp::Pext => (extract(vvvv, src), None),
        BmiOp::Rorx(count) => {
            let count = u32::from(count) & (bits - 1);
            let r = if count == 0 {
                src
            } else {
                (src >> count | src << (bits - count)) & mask
            };
            (r, None)
        }
        BmiOp::Shlx => (src << (vvvv & u64::from(bits - 1)) & mask, None),
        BmiOp::Shrx => (src >> (vvvv & u64::from(bits - 1)), None),
        BmiOp::Sarx => {
            let r = (sign_extend(src, size) as i64) >> (vvvv & u64::from(bits - 1));
            (r as u64 & mask, None)
        }
        BmiOp::Mulx => unreachable!("mulx writes two registers: Cpu::execute"),
    }
}

/// pdep: the low bits of `value`, one by one, at the bits set in `mask`.
fn deposit(value: u64, mask: u64) -> u64 {
    let (mut result, mut mask, mut next) = (0, mask, 0);
    while mask != 0 {
        let lowest = mask.isolate_lowest_one();
        if value >> next & 1 == 1 {
            result |= lowest;
        }
        next += 1;
        mask &= mask - 1;
    }
    result
}

/// pext: the bits of `value` at the bits set in `mask`, packed from bit 0.
fn extract(value: u64, mask: u64) -> u64 {
    let (mut result, mut mask, mut next) = (0, mask, 0);
    while mask != 0 {
        let lowest = mask.isolate_lowest_one();
        if value & lowest != 0 {
            result |= 1 << next;
        }
        next += 1;
        mask &= mask - 1;
    }
    result
}

/// tzcnt (`leading` false) and lzcnt: the number of zero bits below the
/// lowest set bit (above the highest one), the operand size for 0; CF if
/// `src` is 0, ZF if the count is 0. The other flags are undefined and
/// stay as they were.
pub(crate) fn count_zeros(leading: bool, size: Size, src: u64) -> (u64, u64) {
    let bits = size.bits();
    let src = src & size.mask();
    let count = if src == 0 {
        bits
    } else if leading {
        src.leading_zeros() - (64 - bits)
    } else {
        src.trailing_zeros()
    };
    let flags = if src == 0 { FLAG_CF } else { 0 } | if count == 0 { FLAG_ZF } else { 0 };
    (u64::from(count), flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_manipulation_examples() {
        // Values worked out by hand from the SDM's operation sections.
        let b32 = Size::B32;
        assert_eq!(bmi(BmiOp::Andn, b32, 0b1100, 0b1010).0, 0b0100);
        assert_eq!(bmi(BmiOp::Bextr, b32, 0xabcd_ef12, 0x0804).0, 0xf1);
        assert_eq!(bmi(BmiOp::Bextr, b32, 0xabcd_ef12, 0xff00).0, 0xabcd_ef12);
        assert_eq!(bmi(BmiOp::Bextr, b32, 0xabcd_ef12, 0x0820).0, 0);
        assert_eq!(bmi(BmiOp::Blsi, b32, 0b1011000, 0), (0b1000, Some(FLAG_CF)));
        assert_eq!(bmi(BmiOp::Blsmsk, b32, 0b1011000, 0).0, 0b1111);
        assert_eq!(bmi(BmiOp::Blsmsk, b32, 0, 0).0, 0xffff_ffff);
        assert_eq!(bmi(BmiOp::Blsr, b32, 0b1011000, 0).0, 0b1010000);
        assert_eq!(bmi(BmiOp::Bzhi, b32, 0xffff_ffff, 4).0, 0xf);
        assert_eq!(
            bmi(BmiOp::Bzhi, b32, 0xffff_ffff, 40).1.unwrap() & FLAG_CF,
            FLAG_CF
        );
        assert_eq!(bmi(BmiOp::Pdep, b32, 0b1010_0101, 0b101).0, 0b0010_0001);
        assert_eq!(bmi(BmiOp::Pext, b32, 0b1010_0101, 0b1110_0001).0, 0b1101);
        assert_eq!(bmi(BmiOp::Rorx(4), b32, 0x1234_5678, 0).0, 0x8123_4567);
        assert_eq!(bmi(BmiOp::Rorx(36), b32, 0x1234_5678, 0).0, 0x8123_4567);
        assert_eq!(bmi(BmiOp::Shlx, b32, 1, 33).0, 2);
        assert_eq!(bmi(BmiOp::Sarx, b32, 0x8000_0000, 4).0, 0xf800_0000);
        assert_eq!(bmi(BmiOp::Shrx, Size::B64, 1 << 63, 63).0, 1);
        assert_eq!(count_zeros(false, b32, 0), (32, FLAG_CF));
        assert_eq!(count_zeros(true, Size::B16, 1), (15, 0));
        assert_eq!(count_zeros(true, Size::B64, 1 << 63), (0, FLAG_ZF));
    }
}
