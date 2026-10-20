//! Integer SIMD instructions of SSSE3 and SSE4 (maps 0F 38 and 0F 3A) as
//! pure functions on 128-bit values: `dst` is the destination's old value
//! (also the first source), `src` the second source. Lanes are numbered
//! from the least significant.

use alloc::vec::Vec;
use armor_x86::{IntOp, Size};

/// The value of `op dst, src`; `xmm0` is the mask of the variable blends.
pub(crate) fn int_op(op: IntOp, dst: u128, src: u128, xmm0: u128) -> u128 {
    match op {
        IntOp::Pshufb => pshufb(dst, src),
        IntOp::Horizontal {
            sub,
            saturate,
            lane,
        } => horizontal(dst, src, sub, saturate, lane),
        IntOp::Pmaddubsw => pmaddubsw(dst, src),
        IntOp::Psign(lane) => map2(dst, src, lane, |a, b| {
            if b == 0 {
                0
            } else if b < 0 {
                a.wrapping_neg()
            } else {
                a
            }
        }),
        IntOp::Pmulhrsw => map2(dst, src, Size::B16, |a, b| {
            ((a * b) >> 14).wrapping_add(1) >> 1
        }),
        IntOp::Pabs(lane) => map2(dst, src, lane, |_, b| b.wrapping_abs()),
        IntOp::Palignr(shift) => palignr(dst, src, shift),
        IntOp::Blend { lane, imm } => {
            let bits = lane.bits();
            (0..128 / bits).fold(0, |acc, i| {
                let from = if imm >> i & 1 != 0 { src } else { dst };
                acc | from & (u128::from(lane.mask()) << (i * bits))
            })
        }
        IntOp::BlendV(lane) => {
            let bits = lane.bits();
            (0..128 / bits).fold(0, |acc, i| {
                let top = xmm0 >> (i * bits + bits - 1) & 1 != 0;
                let from = if top { src } else { dst };
                acc | from & (u128::from(lane.mask()) << (i * bits))
            })
        }
        IntOp::Pmuldq => {
            let product = |i: u32| {
                let a = i64::from((dst >> (64 * i)) as u32 as i32);
                let b = i64::from((src >> (64 * i)) as u32 as i32);
                u128::from((a * b) as u64)
            };
            product(0) | product(1) << 64
        }
        IntOp::Pmulld => map2(dst, src, Size::B32, |a, b| a.wrapping_mul(b)),
        IntOp::Pcmpq { greater } => map2(dst, src, Size::B64, |a, b| {
            if if greater { a > b } else { a == b } {
                -1
            } else {
                0
            }
        }),
        IntOp::Packusdw => pack(
            lanes(dst, Size::B32)
                .chain(lanes(src, Size::B32))
                .map(|x| x.clamp(0, 0xffff)),
            Size::B16,
        ),
        IntOp::MinMax { max, signed, lane } => {
            let value = |x: i64| {
                if signed { x } else { x & lane.mask() as i64 }
            };
            map2(dst, src, lane, |a, b| {
                let (va, vb) = (value(a), value(b));
                if (va > vb) == max { a } else { b }
            })
        }
        IntOp::Phminposuw => {
            let words = (0..8).map(|i| (src >> (16 * i)) as u16);
            let (index, min) = words
                .enumerate()
                .min_by_key(|&(i, w)| (w, i))
                .expect("eight words");
            u128::from(min) | (index as u128) << 16
        }
        IntOp::Mpsadbw(imm) => mpsadbw(dst, src, imm),
    }
}

/// The result of pcmpestri / pcmpestrm / pcmpistri / pcmpistrm before
/// it becomes an index or a mask: `mask` has one bit per element
/// (IntRes2 in the SDM), `elements` is 16 for bytes and 8 for words.
pub(crate) struct StrCompare {
    pub mask: u16,
    pub elements: u32,
}

impl StrCompare {
    /// ecx for the "index" forms: the least (`imm` bit 6 clear) or most
    /// significant set bit, `elements` if none.
    pub fn index(&self, imm: u8) -> u32 {
        if self.mask == 0 {
            self.elements
        } else if imm & 0x40 != 0 {
            15 - self.mask.leading_zeros()
        } else {
            self.mask.trailing_zeros()
        }
    }

    /// xmm0 for the "mask" forms: the bits (`imm` bit 6 clear) or each
    /// bit spread over its element.
    pub fn xmm0(&self, imm: u8) -> u128 {
        if imm & 0x40 == 0 {
            return u128::from(self.mask);
        }
        let bits = 128 / self.elements;
        (0..self.elements).fold(0, |acc, i| {
            if self.mask >> i & 1 != 0 {
                acc | ((1u128 << bits) - 1) << (i * bits)
            } else {
                acc
            }
        })
    }
}

/// The number of valid elements of a pcmpistr* operand: those before the
/// first null one.
pub(crate) fn implicit_length(v: u128, imm: u8) -> usize {
    let (bits, n) = if imm & 1 != 0 { (16, 8) } else { (8, 16) };
    let element_mask = (1u128 << bits) - 1;
    (0..n)
        .find(|&i| (v >> (i * bits)) & element_mask == 0)
        .unwrap_or(n) as usize
}

/// The number of valid elements of a pcmpestr* operand from its length
/// register: the absolute value, saturated to the element count.
pub(crate) fn explicit_length(length: i64, imm: u8) -> usize {
    let n: u64 = if imm & 1 != 0 { 8 } else { 16 };
    length.unsigned_abs().min(n) as usize
}

/// The SSE4.2 string compare of `a` (valid elements `la`) and `b` (`lb`)
/// by `imm` (Intel SDM vol. 2, 4.1): element format (bits 0–1),
/// aggregation (2–3), polarity (4–5).
pub(crate) fn pcmpstr(imm: u8, a: u128, b: u128, la: usize, lb: usize) -> StrCompare {
    let words = imm & 1 != 0;
    let signed = imm & 2 != 0;
    let (bits, n) = if words { (16u32, 8usize) } else { (8, 16) };
    let element = |v: u128, i: usize| -> i64 {
        let x = (v >> (i as u32 * bits)) as u64 & ((1u64 << bits) - 1);
        if signed {
            sign_extend(x, bits)
        } else {
            x as i64
        }
    };
    let result = |j: usize| -> bool {
        match imm >> 2 & 3 {
            // Equal any: b[j] is one of the valid elements of a.
            0 => j < lb && (0..la).any(|i| element(a, i) == element(b, j)),
            // Ranges: lo <= b[j] <= hi for a valid pair (lo, hi) of a.
            1 => {
                j < lb
                    && (0..la / 2).any(|k| {
                        element(a, 2 * k) <= element(b, j) && element(b, j) <= element(a, 2 * k + 1)
                    })
            }
            // Equal each: a[j] == b[j]; true where both are invalid.
            2 => match (j < la, j < lb) {
                (true, true) => element(a, j) == element(b, j),
                (false, false) => true,
                _ => false,
            },
            // Equal ordered: a, as far as it is valid, occurs at j in b.
            _ => (0..n - j).all(|k| {
                if k >= la {
                    true
                } else if j + k >= lb {
                    false
                } else {
                    element(a, k) == element(b, j + k)
                }
            }),
        }
    };
    let int_res1 = (0..n).fold(0u16, |acc, j| acc | u16::from(result(j)) << j);
    let all = if n == 16 { 0xffff } else { 0xff };
    let valid_b = ((1u32 << lb) - 1) as u16;
    let mask = match imm >> 4 & 3 {
        1 => !int_res1 & all,
        3 => int_res1 ^ valid_b,
        _ => int_res1,
    };
    StrCompare {
        mask,
        elements: n as u32,
    }
}

/// crc32: `crc` updated with the `bytes` low bytes of `data`, CRC-32C
/// (Castagnoli, reflected polynomial 0x82F63B78) without the initial and
/// final inversions, which the instruction leaves to software.
pub(crate) fn crc32c(mut crc: u32, data: u64, bytes: usize) -> u32 {
    for i in 0..bytes {
        crc ^= u32::from((data >> (8 * i)) as u8);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f6_3b78 & (crc & 1).wrapping_neg());
        }
    }
    crc
}

/// mpsadbw: word i is the sum of `|dst[d + i + j] - src[s + j]|` for j in
/// 0..4, with d = 4 × `imm` bit 2 and s = 4 × `imm` bits 0–1.
fn mpsadbw(dst: u128, src: u128, imm: u8) -> u128 {
    let byte = |v: u128, n: u32| i64::from((v >> (8 * n)) as u8);
    let d = 4 * u32::from(imm >> 2 & 1);
    let s = 4 * u32::from(imm & 3);
    pack(
        (0..8).map(|i| {
            (0..4)
                .map(|j| (byte(dst, d + i + j) - byte(src, s + j)).abs())
                .sum()
        }),
        Size::B16,
    )
}

/// The lanes of `v` as signed values.
fn lanes(v: u128, lane: Size) -> impl Iterator<Item = i64> {
    let bits = lane.bits();
    (0..128 / bits).map(move |i| sign_extend((v >> (i * bits)) as u64, bits))
}

fn sign_extend(x: u64, bits: u32) -> i64 {
    ((x << (64 - bits)) as i64) >> (64 - bits)
}

/// Packs `values` (truncated to the lane width) into a 128-bit value.
fn pack(values: impl Iterator<Item = i64>, lane: Size) -> u128 {
    let bits = lane.bits();
    let mask = u128::from(lane.mask());
    values.enumerate().fold(0, |acc, (i, x)| {
        acc | ((x as u128) & mask) << (i as u32 * bits)
    })
}

/// `f(dst lane, src lane)` on every pair of signed lanes, truncated.
fn map2(dst: u128, src: u128, lane: Size, f: impl Fn(i64, i64) -> i64) -> u128 {
    pack(
        lanes(dst, lane).zip(lanes(src, lane)).map(|(a, b)| f(a, b)),
        lane,
    )
}

fn saturate(x: i64, lane: Size) -> i64 {
    let bits = lane.bits();
    x.clamp(-(1 << (bits - 1)), (1 << (bits - 1)) - 1)
}

/// pshufb: byte i is dst byte `src[i] & 15`, or 0 if bit 7 of `src[i]`
/// is set.
fn pshufb(dst: u128, src: u128) -> u128 {
    (0..16).fold(0, |acc, i| {
        let control = (src >> (8 * i)) as u8;
        let byte = if control & 0x80 != 0 {
            0
        } else {
            (dst >> (8 * u32::from(control & 15))) as u8
        };
        acc | u128::from(byte) << (8 * i)
    })
}

/// phaddw/d, phsubw/d, phaddsw, phsubsw: sums (differences) of adjacent
/// lanes, dst's in the low half, src's in the high half.
fn horizontal(dst: u128, src: u128, sub: bool, saturating: bool, lane: Size) -> u128 {
    let pairs = |v: u128| {
        let l: Vec<i64> = lanes(v, lane).collect();
        l.as_chunks::<2>()
            .0
            .iter()
            .map(|&[x, y]| if sub { x - y } else { x + y })
            .collect::<Vec<_>>()
    };
    let values = pairs(dst)
        .into_iter()
        .chain(pairs(src))
        .map(|x| if saturating { saturate(x, lane) } else { x });
    pack(values, lane)
}

/// pmaddubsw: unsigned bytes of dst times signed bytes of src, adjacent
/// products added with signed saturation into words.
fn pmaddubsw(dst: u128, src: u128) -> u128 {
    let words = (0..8).map(|i| {
        let product = |j: u32| {
            let a = i64::from((dst >> (8 * j)) as u8);
            let b = i64::from((src >> (8 * j)) as u8 as i8);
            a * b
        };
        saturate(product(2 * i) + product(2 * i + 1), Size::B16)
    });
    pack(words, Size::B16)
}

/// palignr: bytes `shift..shift + 16` of the 32-byte value dst:src (src
/// low), zeros past the end.
fn palignr(dst: u128, src: u128, shift: u8) -> u128 {
    let byte = |n: u32| -> u8 {
        match n {
            0..=15 => (src >> (8 * n)) as u8,
            16..=31 => (dst >> (8 * (n - 16))) as u8,
            _ => 0,
        }
    };
    (0..16).fold(0, |acc, i| {
        acc | u128::from(byte(u32::from(shift) + i)) << (8 * i)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(b: [u8; 16]) -> u128 {
        u128::from_le_bytes(b)
    }

    fn words(w: [i16; 8]) -> u128 {
        pack(w.iter().map(|&x| i64::from(x)), Size::B16)
    }

    #[test]
    fn pshufb_selects_and_zeroes_bytes() {
        let dst = bytes(core::array::from_fn(|i| 0x10 + i as u8));
        let mut control = [0u8; 16];
        control[0] = 15;
        control[1] = 0x80;
        control[2] = 0x13;
        let result = pshufb(dst, bytes(control));
        assert_eq!(result as u8, 0x1f);
        assert_eq!((result >> 8) as u8, 0);
        assert_eq!((result >> 16) as u8, 0x13);
        assert_eq!((result >> 24) as u8, 0x10);
    }

    #[test]
    fn horizontal_adds_with_and_without_saturation() {
        let a = words([30000, 30000, 1, 2, -5, 3, 0, 0]);
        let b = words([7, 8, 0, 0, 0, 0, -32768, -1]);
        assert_eq!(
            horizontal(a, b, false, false, Size::B16),
            words([-5536, 3, -2, 0, 15, 0, 0, 32767])
        );
        assert_eq!(
            horizontal(a, b, false, true, Size::B16),
            words([32767, 3, -2, 0, 15, 0, 0, -32768])
        );
        assert_eq!(
            horizontal(a, b, true, true, Size::B16),
            words([0, -1, -8, 0, -1, 0, 0, -32767])
        );
    }

    #[test]
    fn pmaddubsw_saturates() {
        let a = bytes([255; 16]);
        let b = bytes([127; 16]);
        assert_eq!(pmaddubsw(a, b), words([32767; 8]));
        let b = bytes([0x80; 16]);
        assert_eq!(pmaddubsw(a, b), words([-32768; 8]));
    }

    #[test]
    fn pmulhrsw_rounds() {
        let r = int_op(
            IntOp::Pmulhrsw,
            words([16384, -32768, 3, 0, 0, 0, 0, 0]),
            words([16384, -32768, 16384, 0, 0, 0, 0, 0]),
            0,
        );
        // 0.5 * 0.5 = 0.25; -1 * -1 overflows to -32768; 3 * 0.5 rounds
        // to 2.
        assert_eq!(r, words([8192, -32768, 2, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn psign_and_pabs() {
        let a = words([5, 5, 5, -32768, 0, 0, 0, 0]);
        let b = words([1, 0, -1, -1, 0, 0, 0, 0]);
        assert_eq!(
            int_op(IntOp::Psign(Size::B16), a, b, 0),
            words([5, 0, -5, -32768, 0, 0, 0, 0])
        );
        assert_eq!(
            int_op(IntOp::Pabs(Size::B16), 0, a, 0),
            words([5, 5, 5, -32768, 0, 0, 0, 0])
        );
    }

    #[test]
    fn sse41_integer_operations() {
        let a = words([1, -2, 3, -4, 5, -6, 7, -8]);
        let b = words([10, 20, 30, 40, 50, 60, 70, 80]);
        // pblendw with 0b1010_0101.
        assert_eq!(
            int_op(
                IntOp::Blend {
                    lane: Size::B16,
                    imm: 0xa5
                },
                a,
                b,
                0
            ),
            words([10, -2, 30, -4, 5, 60, 7, 80])
        );
        // pminsw-like on dwords, signed and unsigned.
        let x = u128::from(0xffff_ffffu32) | u128::from(1u32) << 32;
        let y = u128::from(1u32) | u128::from(2u32) << 32;
        let min_signed = IntOp::MinMax {
            max: false,
            signed: true,
            lane: Size::B32,
        };
        let max_unsigned = IntOp::MinMax {
            max: true,
            signed: false,
            lane: Size::B32,
        };
        assert_eq!(
            int_op(min_signed, x, y, 0),
            u128::from(0xffff_ffffu32) | u128::from(1u32) << 32
        );
        assert_eq!(
            int_op(max_unsigned, x, y, 0),
            u128::from(0xffff_ffffu32) | u128::from(2u32) << 32
        );
        // phminposuw: the first of two equal minima.
        let w = words([9, 3, 7, 3, 8, 8, 8, 8]);
        assert_eq!(int_op(IntOp::Phminposuw, 0, w, 0), 3 | 1 << 16);
        // packusdw saturates to 0..65535.
        let d = pack([-1i64, 70000, 5, 65535].into_iter(), Size::B32);
        assert_eq!(
            int_op(IntOp::Packusdw, d, d, 0),
            pack(
                [0i64, 65535, 5, 65535, 0, 65535, 5, 65535].into_iter(),
                Size::B16
            )
        );
    }

    #[test]
    fn crc32c_matches_the_standard_check_value() {
        // CRC-32C("123456789") = 0xE3069283, with the usual inversions.
        let crc = b"123456789"
            .iter()
            .fold(!0u32, |crc, &b| crc32c(crc, u64::from(b), 1));
        assert_eq!(!crc, 0xe306_9283);
        let word = u64::from_le_bytes(*b"12345678");
        let crc = crc32c(crc32c(!0, word, 8), u64::from(b'9'), 1);
        assert_eq!(!crc, 0xe306_9283);
    }

    #[test]
    fn string_compares() {
        let text = u128::from_le_bytes(*b"hello, world\0\0\0\0");
        // pcmpistri equal any with the set "o,": first index 4.
        let set = u128::from_le_bytes(*b"o,\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
        let r = pcmpstr(
            0x00,
            set,
            text,
            implicit_length(set, 0),
            implicit_length(text, 0),
        );
        assert_eq!(r.index(0), 4);
        assert_eq!(r.index(0x40), 8);
        // Ranges "az": the letters.
        let range = u128::from_le_bytes(*b"az\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
        let r = pcmpstr(0x04, range, text, 2, 12);
        assert_eq!(r.mask, 0b1111_1001_1111);
        // Negative polarity: the first non-letter; masked negative leaves
        // the invalid tail clear.
        assert_eq!(pcmpstr(0x14, range, text, 2, 12).index(0), 5);
        assert_eq!(pcmpstr(0x34, range, text, 2, 12).mask, 0b0000_0110_0000);
        // Equal ordered: "wor" at 7.
        let needle = u128::from_le_bytes(*b"wor\0\0\0\0\0\0\0\0\0\0\0\0\0");
        assert_eq!(pcmpstr(0x0c, needle, text, 3, 12).index(0), 7);
        // Equal each, as in strcmp: differ at 1.
        let other = u128::from_le_bytes(*b"hallo, world\0\0\0\0");
        assert_eq!(pcmpstr(0x18, text, other, 12, 12).index(0), 1);
        assert_eq!(explicit_length(-3, 0), 3);
        assert_eq!(explicit_length(i64::MIN, 1), 8);
    }

    #[test]
    fn mpsadbw_blocks() {
        let dst = bytes(core::array::from_fn(|i| i as u8));
        let src = bytes([3, 2, 1, 0, 9, 9, 9, 9, 0, 0, 0, 0, 0, 0, 0, 0]);
        // Word 0: |0-3| + |1-2| + |2-1| + |3-0| = 8.
        assert_eq!(mpsadbw(dst, src, 0) as u16, 8);
        // Second block of src (9s) against dst bytes 4..: |4-9|+|5-9|+|6-9|+|7-9| = 14.
        assert_eq!(mpsadbw(dst, src, 0b101) as u16, 14);
    }

    #[test]
    fn palignr_concatenates_and_shifts() {
        let dst = bytes(core::array::from_fn(|i| 0x20 + i as u8));
        let src = bytes(core::array::from_fn(|i| i as u8));
        assert_eq!(palignr(dst, src, 0), src);
        assert_eq!(palignr(dst, src, 16), dst);
        assert_eq!(palignr(dst, src, 4) as u8, 4);
        assert_eq!((palignr(dst, src, 4) >> 120) as u8, 0x23);
        assert_eq!(palignr(dst, src, 20) >> 96, 0);
        assert_eq!(palignr(dst, src, 32), 0);
    }
}
