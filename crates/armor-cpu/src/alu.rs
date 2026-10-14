//! Integer results and status flags. Flags that the architecture leaves
//! undefined get a fixed, documented value here; the differential tests
//! mask them out (docs/tech/x86-64-flags.md).

use armor_x86::{AluOp, ShiftOp, Size};

pub const FLAG_CF: u64 = 1 << 0;
pub const FLAG_PF: u64 = 1 << 2;
pub const FLAG_AF: u64 = 1 << 4;
pub const FLAG_ZF: u64 = 1 << 6;
pub const FLAG_SF: u64 = 1 << 7;
pub const FLAG_DF: u64 = 1 << 10;
pub const FLAG_OF: u64 = 1 << 11;
/// The six arithmetic status flags.
pub const STATUS_FLAGS: u64 = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;

fn sign(value: u64, size: Size) -> bool {
    (value >> (size.bits() - 1)) & 1 != 0
}

pub fn sign_extend(value: u64, size: Size) -> u64 {
    let shift = 64 - size.bits();
    (((value << shift) as i64) >> shift) as u64
}

fn flag(on: bool, flag: u64) -> u64 {
    if on { flag } else { 0 }
}

/// ZF, SF and PF of a result.
pub fn result_flags(result: u64, size: Size) -> u64 {
    flag(result & size.mask() == 0, FLAG_ZF)
        | flag(sign(result, size), FLAG_SF)
        | flag((result as u8).count_ones().is_multiple_of(2), FLAG_PF)
}

/// Result and status flags of an ALU operation on `size`-bit operands.
/// `carry` is the incoming CF, used by ADC and SBB. AF is cleared by the
/// logical operations, where the architecture leaves it undefined.
pub fn alu(op: AluOp, size: Size, a: u64, b: u64, carry: bool) -> (u64, u64) {
    let bits = size.bits();
    let mask = size.mask();
    let (a, b) = (a & mask, b & mask);
    let carry_in = u128::from(carry);

    let (result, cf, of, af) = match op {
        AluOp::Add | AluOp::Adc => {
            let c = if op == AluOp::Adc { carry_in } else { 0 };
            let wide = u128::from(a) + u128::from(b) + c;
            let r = wide as u64 & mask;
            let of = sign((a ^ r) & (b ^ r), size);
            (r, (wide >> bits) & 1 != 0, of, (a ^ b ^ r) & 0x10 != 0)
        }
        AluOp::Sub | AluOp::Sbb | AluOp::Cmp => {
            let c = if op == AluOp::Sbb { carry_in } else { 0 };
            let subtrahend = u128::from(b) + c;
            let r = (u128::from(a).wrapping_sub(subtrahend)) as u64 & mask;
            let of = sign((a ^ b) & (a ^ r), size);
            (r, u128::from(a) < subtrahend, of, (a ^ b ^ r) & 0x10 != 0)
        }
        AluOp::And => (a & b, false, false, false),
        AluOp::Or => (a | b, false, false, false),
        AluOp::Xor => (a ^ b, false, false, false),
    };

    let flags =
        flag(cf, FLAG_CF) | flag(af, FLAG_AF) | flag(of, FLAG_OF) | result_flags(result, size);
    (result, flags)
}

/// Outcome of a shift or rotate: the new value and the new status flags.
/// `None` when the masked count is zero: nothing changes, flags included.
pub struct Shifted {
    pub result: u64,
    pub flags: u64,
}

/// Shifts and rotates. `flags` is the current RFLAGS (rotates only modify
/// CF and OF; RCL/RCR also read CF).
pub fn shift(op: ShiftOp, size: Size, value: u64, count: u64, flags: u64) -> Option<Shifted> {
    let bits = u64::from(size.bits());
    let mask = size.mask();
    let a = value & mask;
    let count = count & if size == Size::B64 { 0x3f } else { 0x1f };
    if count == 0 {
        return None;
    }
    let msb = |v: u64| sign(v, size);
    let cf_in = flags & FLAG_CF != 0;

    let (result, cf, of) = match op {
        ShiftOp::Shl => {
            let result = if count < bits { (a << count) & mask } else { 0 };
            // Count above the size (8/16-bit only): CF is undefined.
            let cf = count <= bits && (a >> (bits - count)) & 1 != 0;
            (result, cf, msb(result) ^ cf)
        }
        ShiftOp::Shr => {
            let result = if count < bits { a >> count } else { 0 };
            let cf = count <= bits && (a >> (count - 1)) & 1 != 0;
            (result, cf, msb(a))
        }
        ShiftOp::Sar => {
            let signed = sign_extend(a, size) as i64;
            let result = (signed >> count.min(bits - 1)) as u64 & mask;
            let cf = (signed >> (count - 1).min(bits - 1)) & 1 != 0;
            (result, cf, false)
        }
        ShiftOp::Rol | ShiftOp::Ror => {
            let n = count % bits;
            let result = if n == 0 {
                a
            } else if op == ShiftOp::Rol {
                ((a << n) | (a >> (bits - n))) & mask
            } else {
                ((a >> n) | (a << (bits - n))) & mask
            };
            let (cf, of) = if op == ShiftOp::Rol {
                let cf = result & 1 != 0;
                (cf, msb(result) ^ cf)
            } else {
                (msb(result), msb(result) ^ sign(result << 1, size))
            };
            let kept = flags & STATUS_FLAGS & !(FLAG_CF | FLAG_OF);
            return Some(Shifted {
                result,
                flags: kept | flag(cf, FLAG_CF) | flag(of, FLAG_OF),
            });
        }
        ShiftOp::Rcl | ShiftOp::Rcr => {
            // Rotate the (bits + 1)-bit value CF:a.
            let width = bits + 1;
            let n = count % width;
            let full = (u128::from(cf_in) << bits) | u128::from(a);
            let wide_mask = (1u128 << width) - 1;
            let rotated = if n == 0 {
                full
            } else if op == ShiftOp::Rcl {
                ((full << n) | (full >> (width - n))) & wide_mask
            } else {
                ((full >> n) | (full << (width - n))) & wide_mask
            };
            let result = rotated as u64 & mask;
            let cf = (rotated >> bits) & 1 != 0;
            let of = if op == ShiftOp::Rcl {
                msb(result) ^ cf
            } else {
                msb(a) ^ cf_in
            };
            let kept = flags & STATUS_FLAGS & !(FLAG_CF | FLAG_OF);
            return Some(Shifted {
                result,
                flags: kept | flag(cf, FLAG_CF) | flag(of, FLAG_OF),
            });
        }
    };
    // Shifts: OF is only defined for count 1 and AF is undefined (cleared).
    Some(Shifted {
        result,
        flags: flag(cf, FLAG_CF) | flag(of, FLAG_OF) | result_flags(result, size),
    })
}

/// shld (`left`) or shrd of `value` by `count`, filling from `fill`.
/// `None` when the masked count is 0 (nothing changes, not even flags).
/// OF is defined for a count of 1 only, AF is undefined (cleared); with a
/// count above a 16-bit operand's size the result is undefined.
pub fn double_shift(left: bool, size: Size, value: u64, fill: u64, count: u64) -> Option<Shifted> {
    let bits = u64::from(size.bits());
    let mask = size.mask();
    let (a, b) = (value & mask, fill & mask);
    let count = count & if size == Size::B64 { 0x3f } else { 0x1f };
    if count == 0 {
        return None;
    }
    let (result, cf) = if left {
        let joined = (u128::from(a) << bits) | u128::from(b);
        let result = ((joined << count) >> bits) as u64 & mask;
        (result, (joined >> (2 * bits - count)) & 1 != 0)
    } else {
        let joined = (u128::from(b) << bits) | u128::from(a);
        let result = (joined >> count) as u64 & mask;
        (result, (joined >> (count - 1)) & 1 != 0)
    };
    let of = sign(result, size) != sign(a, size);
    Some(Shifted {
        result,
        flags: flag(cf, FLAG_CF) | flag(of, FLAG_OF) | result_flags(result, size),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_flags() {
        let f = |size, a, b| alu(AluOp::Add, size, a, b, false);
        assert_eq!(f(Size::B8, 0x7f, 1), (0x80, FLAG_OF | FLAG_SF | FLAG_AF));
        assert_eq!(
            f(Size::B8, 0xff, 1),
            (0, FLAG_CF | FLAG_ZF | FLAG_PF | FLAG_AF)
        );
        assert_eq!(
            f(Size::B32, 0x1_ffff_ffff, 1),
            (0, FLAG_CF | FLAG_ZF | FLAG_PF | FLAG_AF)
        );
        assert_eq!(f(Size::B64, u64::MAX, 2), (1, FLAG_CF | FLAG_AF));
    }

    #[test]
    fn sub_and_cmp_flags() {
        let f = |op, size, a, b| alu(op, size, a, b, false);
        assert_eq!(
            f(AluOp::Sub, Size::B8, 0, 1),
            (0xff, FLAG_CF | FLAG_PF | FLAG_AF | FLAG_SF)
        );
        assert_eq!(f(AluOp::Cmp, Size::B8, 0x80, 1), (0x7f, FLAG_OF | FLAG_AF));
        assert_eq!(f(AluOp::Cmp, Size::B64, 5, 5), (0, FLAG_ZF | FLAG_PF));
    }

    #[test]
    fn carry_chains() {
        assert_eq!(
            alu(AluOp::Adc, Size::B8, 0xff, 0, true),
            (0, FLAG_CF | FLAG_ZF | FLAG_PF | FLAG_AF)
        );
        assert_eq!(
            alu(AluOp::Sbb, Size::B16, 0, 0, true),
            (0xffff, FLAG_CF | FLAG_PF | FLAG_AF | FLAG_SF)
        );
    }

    #[test]
    fn logic_ops_clear_cf_and_of() {
        assert_eq!(
            alu(AluOp::Xor, Size::B32, 7, 7, true),
            (0, FLAG_ZF | FLAG_PF)
        );
        assert_eq!(
            alu(AluOp::Or, Size::B8, 0x80, 0x01, false),
            (0x81, FLAG_SF | FLAG_PF)
        );
    }

    #[test]
    fn shifts() {
        let s = |op, size, v, n| shift(op, size, v, n, 0).map(|s| (s.result, s.flags));
        assert_eq!(
            s(ShiftOp::Shl, Size::B8, 0x81, 1),
            Some((0x02, FLAG_CF | FLAG_OF))
        );
        assert_eq!(
            s(ShiftOp::Shr, Size::B8, 0x81, 1),
            Some((0x40, FLAG_CF | FLAG_OF))
        );
        assert_eq!(
            s(ShiftOp::Sar, Size::B8, 0x81, 1),
            Some((0xc0, FLAG_CF | FLAG_SF | FLAG_PF))
        );
        // Count is masked to 5 bits for 32-bit operands: 33 -> 1.
        assert_eq!(
            s(ShiftOp::Shl, Size::B32, 1, 33),
            s(ShiftOp::Shl, Size::B32, 1, 1)
        );
        assert_eq!(s(ShiftOp::Shl, Size::B64, 1, 64), None);
    }

    #[test]
    fn rotates_only_touch_cf_and_of() {
        let before = FLAG_ZF | FLAG_SF;
        let r = shift(ShiftOp::Rol, Size::B8, 0x81, 1, before).unwrap();
        assert_eq!(r.result, 0x03);
        assert_eq!(r.flags, before | FLAG_CF | FLAG_OF);
        let r = shift(ShiftOp::Rcl, Size::B8, 0x80, 1, FLAG_CF).unwrap();
        assert_eq!(r.result, 0x01);
        assert_eq!(r.flags & FLAG_CF, FLAG_CF);
    }
}
