//! The x87 transcendental instructions, correctly rounded (ADR-0030):
//! f2xm1, fyl2x, fyl2xp1, fpatan, fsin, fcos, fsincos, fptan.
//!
//! Each result is computed in multi-precision binary arithmetic (`Big`,
//! integers only, as the rest of `f80`) with a known bound on its relative
//! error, then rounded once to the 64-bit register format by `pack`. If the
//! two ends of the error interval round differently, the computation is
//! repeated with twice the precision (Ziv's strategy), so the result is the
//! exact value rounded, in every rounding mode. The precision control does
//! not apply: the SDM limits it to add, sub, mul, div and sqrt.
//!
//! Real hardware does not round these correctly (and reduces the
//! arguments of fsin, fcos and fptan with a 66-bit pi): results can differ
//! from an Intel CPU in the last bit, more for large trigonometric
//! arguments (docs/tech/floating-point.md).

use std::cmp::Ordering;

use super::{
    Class, F80, FLAG_DENORMAL, FLAG_INEXACT, FLAG_INVALID, FLAG_ZERO_DIVIDE, Mode, Out, Rounding,
    invalid, out, pack, propagate,
};

/// A binary floating-point value of `limbs.len()` 64-bit limbs:
/// (-1)^neg × M × 2^exp, M the little-endian integer of `limbs` with its
/// top bit set; zero when `limbs` is empty. Operations truncate their
/// result to the precision of their first operand (an error below one
/// unit in the last place).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Big {
    neg: bool,
    exp: i64,
    limbs: Vec<u64>,
}

/// Bits of precision lost by a whole computation, at most: each truncated
/// operation errs by less than one unit in the last place, and no
/// computation here chains more than 2^20 of them (series of up to a few
/// thousand terms), with the relative error growing by small factors only.
const MARGIN: i64 = 40;
/// Precisions tried, in limbs: 192 bits first, doubling up to 3072, far
/// beyond the hardest cases of a 64-bit result.
const PRECISIONS: [usize; 5] = [3, 6, 12, 24, 48];
/// Below 2^-34 the second term of sin, tan, cos - 1 and atan is under a
/// sixteenth of the last place: the result is the first term, moved by an
/// amount smaller than any precision can show (`nudged`).
const TINY_EXP: i32 = -35;

impl Big {
    fn zero() -> Big {
        Big {
            neg: false,
            exp: 0,
            limbs: Vec::new(),
        }
    }

    fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    /// The value of a little-endian magnitude `mag` × 2^exp, normalized to
    /// `n` limbs (truncated).
    fn from_mag(neg: bool, exp: i64, mut mag: Vec<u64>, n: usize) -> Big {
        while mag.last() == Some(&0) {
            mag.pop();
        }
        if mag.is_empty() {
            return Big::zero();
        }
        let bits = 64 * mag.len() as i64 - i64::from(mag.last().unwrap().leading_zeros());
        let shift = 64 * n as i64 - bits;
        let mag = if shift >= 0 {
            shl(&mag, shift as u64, n)
        } else {
            shr(&mag, (-shift) as u64, n)
        };
        Big {
            neg,
            exp: exp - shift,
            limbs: mag,
        }
    }

    fn from_u64(value: u64, n: usize) -> Big {
        Big::from_mag(false, 0, vec![value], n)
    }

    fn one(n: usize) -> Big {
        Big::from_u64(1, n)
    }

    /// A finite F80 exactly (it needs one limb).
    fn from_f80(a: F80, n: usize) -> Big {
        if a.exp == 0 && a.mant == 0 {
            return Big::zero();
        }
        let (e, m) = a.unpack();
        let mut big = Big::from_mag(a.sign, i64::from(e) - 63, vec![m], n);
        big.neg = a.sign;
        big
    }

    fn n(&self) -> usize {
        self.limbs.len()
    }

    /// The value is in [2^(top - 1), 2^top).
    fn top(&self) -> i64 {
        self.exp + 64 * self.n() as i64
    }

    fn negate(mut self) -> Big {
        if !self.is_zero() {
            self.neg = !self.neg;
        }
        self
    }

    fn abs(mut self) -> Big {
        self.neg = false;
        self
    }

    fn scale(mut self, by: i64) -> Big {
        if !self.is_zero() {
            self.exp += by;
        }
        self
    }

    /// Compares magnitudes.
    fn cmp_mag(&self, other: &Big) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        match self.top().cmp(&other.top()) {
            Ordering::Equal => {}
            unequal => return unequal,
        }
        // Same top: compare the significands aligned at the top.
        let n = self.n().max(other.n());
        let a = shl(&self.limbs, 64 * (n - self.n()) as u64, n);
        let b = shl(&other.limbs, 64 * (n - other.n()) as u64, n);
        cmp_limbs(&a, &b)
    }

    /// `self + other`, at the precision of `self`.
    fn add(&self, other: &Big) -> Big {
        let n = self.n().max(other.n());
        if other.is_zero() {
            return Big::from_mag(self.neg, self.exp, self.limbs.clone(), n);
        }
        if self.is_zero() {
            return Big::from_mag(other.neg, other.exp, other.limbs.clone(), n);
        }
        // Far apart: the smaller one is below the last place (truncation).
        let gap = (self.top() - other.top()).abs();
        if gap > 64 * n as i64 + 2 {
            let larger = if self.top() > other.top() {
                self
            } else {
                other
            };
            return Big::from_mag(larger.neg, larger.exp, larger.limbs.clone(), n);
        }
        // Exactly, at the smaller exponent, then normalized.
        let low = self.exp.min(other.exp);
        let size = n * 2 + (gap as usize / 64) + 2;
        let a = shl(&self.limbs, (self.exp - low) as u64, size);
        let b = shl(&other.limbs, (other.exp - low) as u64, size);
        let (neg, mag) = if self.neg == other.neg {
            (self.neg, add_limbs(&a, &b))
        } else {
            match cmp_limbs(&a, &b) {
                Ordering::Equal => return Big::zero(),
                Ordering::Greater => (self.neg, sub_limbs(&a, &b)),
                Ordering::Less => (other.neg, sub_limbs(&b, &a)),
            }
        };
        Big::from_mag(neg, low, mag, n)
    }

    fn sub(&self, other: &Big) -> Big {
        self.add(&other.clone().negate())
    }

    fn mul(&self, other: &Big) -> Big {
        let n = self.n().max(other.n());
        if self.is_zero() || other.is_zero() {
            return Big::zero();
        }
        let mut product = vec![0u64; self.n() + other.n()];
        for (i, &a) in self.limbs.iter().enumerate() {
            let mut carry = 0u128;
            for (j, &b) in other.limbs.iter().enumerate() {
                let t = u128::from(a) * u128::from(b) + u128::from(product[i + j]) + carry;
                product[i + j] = t as u64;
                carry = t >> 64;
            }
            product[i + other.n()] = carry as u64;
        }
        Big::from_mag(self.neg != other.neg, self.exp + other.exp, product, n)
    }

    /// `self / other` (`other` nonzero), by long division to 64n + 2
    /// quotient bits.
    fn div(&self, other: &Big) -> Big {
        let n = self.n().max(other.n());
        if self.is_zero() {
            return Big::zero();
        }
        let a = shl(&self.limbs, 64 * (n - self.n()) as u64, n);
        let b = shl(&other.limbs, 64 * (n - other.n()) as u64, n);
        let a_exp = self.exp - 64 * (n - self.n()) as i64;
        let b_exp = other.exp - 64 * (n - other.n()) as i64;
        // floor(a × 2^k / b), k = 64n + 2: at least 64n + 2 bits.
        let k = 64 * n as u64 + 2;
        let mut remainder = vec![0u64; n + 1];
        let mut quotient = vec![0u64; (64 * n as u64 + k).div_ceil(64) as usize + 1];
        let total = 64 * n as u64 + k;
        for i in (0..total).rev() {
            // remainder = 2 × remainder + the next dividend bit.
            let bit = if i >= k {
                let j = i - k;
                (a[(j / 64) as usize] >> (j % 64)) & 1
            } else {
                0
            };
            let mut carry = bit;
            for limb in remainder.iter_mut() {
                let next = *limb >> 63;
                *limb = (*limb << 1) | carry;
                carry = next;
            }
            if cmp_limbs(&remainder, &b) != Ordering::Less {
                remainder = sub_limbs(&remainder, &b);
                quotient[(i / 64) as usize] |= 1 << (i % 64);
            }
        }
        Big::from_mag(self.neg != other.neg, a_exp - b_exp - k as i64, quotient, n)
    }

    fn mul_small(&self, m: u64) -> Big {
        self.mul(&Big::from_u64(m, self.n()))
    }

    fn div_small(&self, d: u64) -> Big {
        if self.is_zero() {
            return Big::zero();
        }
        // One extra limb below, for the bits the division brings down.
        let mut extended = vec![0u64];
        extended.extend_from_slice(&self.limbs);
        let mut rem = 0u128;
        for limb in extended.iter_mut().rev() {
            let cur = (rem << 64) | u128::from(*limb);
            *limb = (cur / u128::from(d)) as u64;
            rem = cur % u128::from(d);
        }
        Big::from_mag(self.neg, self.exp - 64, extended, self.n())
    }

    /// The nearest integer, as (value, its absolute value fits 64 bits).
    fn round_to_int(&self) -> i64 {
        if self.is_zero() || self.top() < -1 {
            return 0;
        }
        // |self| < 2^63 for every caller.
        let half = Big::from_mag(false, -1, vec![1], self.n());
        let shifted = self.clone().abs().add(&half);
        let shift = -shifted.exp;
        let mag = if shift >= 0 {
            shr(&shifted.limbs, shift as u64, 1)
        } else {
            shl(&shifted.limbs, (-shift) as u64, 1)
        };
        let value = mag.first().copied().unwrap_or(0) as i64;
        if self.neg { -value } else { value }
    }

    /// Rounded to the x87 register format by `rc`.
    fn round(&self, rc: Rounding) -> Out<F80> {
        if self.is_zero() {
            return out(F80::zero(self.neg), 0);
        }
        // The top 128 bits, the rest folded into a sticky bit.
        let n = self.n();
        let mut sig = (u128::from(self.limbs[n - 1]) << 64) | u128::from(self.limbs[n - 2]);
        if self.limbs[..n - 2].iter().any(|&l| l != 0) {
            sig |= 1;
        }
        let top = self.top() - 1;
        // Beyond any F80 exponent: clamp, keeping overflow and underflow.
        let exp = top.clamp(-40000, 40000) as i32;
        pack(self.neg, exp, sig, Mode::register(rc, 64))
    }
}

fn cmp_limbs(a: &[u64], b: &[u64]) -> Ordering {
    let n = a.len().max(b.len());
    for i in (0..n).rev() {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        match x.cmp(&y) {
            Ordering::Equal => {}
            unequal => return unequal,
        }
    }
    Ordering::Equal
}

fn add_limbs(a: &[u64], b: &[u64]) -> Vec<u64> {
    let n = a.len().max(b.len());
    let mut sum = Vec::with_capacity(n + 1);
    let mut carry = 0u128;
    for i in 0..n {
        let t = u128::from(a.get(i).copied().unwrap_or(0))
            + u128::from(b.get(i).copied().unwrap_or(0))
            + carry;
        sum.push(t as u64);
        carry = t >> 64;
    }
    sum.push(carry as u64);
    sum
}

/// `a - b`, with a >= b.
fn sub_limbs(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut diff = Vec::with_capacity(a.len());
    let mut borrow = 0u64;
    for (i, &limb) in a.iter().enumerate() {
        let (d, b1) = limb.overflowing_sub(b.get(i).copied().unwrap_or(0));
        let (d, b2) = d.overflowing_sub(borrow);
        diff.push(d);
        borrow = u64::from(b1 || b2);
    }
    diff
}

/// `mag << shift`, in `n` limbs (the caller guarantees the room).
fn shl(mag: &[u64], shift: u64, n: usize) -> Vec<u64> {
    let mut out = vec![0u64; n];
    let (limbs, bits) = ((shift / 64) as usize, (shift % 64) as u32);
    for (i, &limb) in mag.iter().enumerate() {
        if i + limbs < n {
            out[i + limbs] |= limb << bits;
        }
        if bits != 0 && i + limbs + 1 < n {
            out[i + limbs + 1] |= limb >> (64 - bits);
        }
    }
    out
}

/// `mag >> shift`, truncated, in `n` limbs.
fn shr(mag: &[u64], shift: u64, n: usize) -> Vec<u64> {
    let mut out = vec![0u64; n];
    let (limbs, bits) = ((shift / 64) as usize, (shift % 64) as u32);
    for (i, slot) in out.iter_mut().enumerate() {
        let lo = mag.get(i + limbs).copied().unwrap_or(0);
        let hi = mag.get(i + limbs + 1).copied().unwrap_or(0);
        *slot = if bits == 0 {
            lo
        } else {
            (lo >> bits) | (hi << (64 - bits))
        };
    }
    out
}

/// Whether `term` no longer matters for a sum near `sum`: below its last
/// place by 8 bits.
fn negligible(term: &Big, sum: &Big) -> bool {
    term.is_zero() || (!sum.is_zero() && term.top() < sum.top() - 64 * sum.n() as i64 - 8)
}

/// pi/4 = atan(1), by Euler's series: (1/2) × sum t_k, t_0 = 1,
/// t_k = t_(k-1) × 2k / (2k + 1) / 2.
fn quarter_pi(n: usize) -> Big {
    let mut term = Big::one(n);
    let mut sum = Big::one(n);
    let mut k = 1u64;
    loop {
        term = term.mul_small(2 * k).div_small(2 * k + 1).scale(-1);
        if negligible(&term, &sum) {
            break;
        }
        sum = sum.add(&term);
        k += 1;
    }
    sum.scale(-1)
}

/// ln 2 = 2 atanh(1/3).
fn ln2(n: usize) -> Big {
    atanh(&Big::one(n).div_small(3)).scale(1)
}

/// atanh(u) = sum u^(2k+1) / (2k+1), for |u| <= 1/5.
fn atanh(u: &Big) -> Big {
    let n = u.n();
    let square = u.mul(u);
    let mut power = u.clone();
    let mut sum = u.clone();
    let mut k = 1u64;
    loop {
        power = power.mul(&square);
        let term = power.div_small(2 * k + 1);
        if negligible(&term, &sum) {
            break;
        }
        sum = sum.add(&term);
        k += 1;
    }
    Big::from_mag(sum.neg, sum.exp, sum.limbs, n)
}

/// atan(z) for 0 <= z <= 1, by Euler's series:
/// atan(z) = z / (1 + z²) × sum t_k, t_0 = 1,
/// t_k = t_(k-1) × w × 2k / (2k + 1), w = z² / (1 + z²) <= 1/2.
fn atan(z: &Big) -> Big {
    let n = z.n();
    if z.is_zero() {
        return Big::zero();
    }
    let square = z.mul(z);
    let one_plus = Big::one(n).add(&square);
    let w = square.div(&one_plus);
    let mut term = Big::one(n);
    let mut sum = Big::one(n);
    let mut k = 1u64;
    loop {
        term = term.mul(&w).mul_small(2 * k).div_small(2 * k + 1);
        if negligible(&term, &sum) {
            break;
        }
        sum = sum.add(&term);
        k += 1;
    }
    z.div(&one_plus).mul(&sum)
}

/// e^t - 1, for |t| <= 1: sum t^k / k!.
fn expm1(t: &Big) -> Big {
    let n = t.n();
    let mut term = t.clone();
    let mut sum = t.clone();
    let mut k = 2u64;
    loop {
        term = term.mul(t).div_small(k);
        if negligible(&term, &sum) {
            break;
        }
        sum = sum.add(&term);
        k += 1;
    }
    Big::from_mag(sum.neg, sum.exp, sum.limbs, n)
}

/// sin(r) and cos(r), for |r| <= 1, by their Taylor series.
fn sin_cos(r: &Big) -> (Big, Big) {
    let n = r.n();
    let square = r.mul(r);
    // sin: r - r³/3! + …; cos: 1 - r²/2! + ….
    let series = |first: Big, k0: u64| {
        let mut term = first.clone();
        let mut sum = first;
        let mut k = k0;
        loop {
            term = term.mul(&square).div_small(k * (k + 1)).negate();
            if negligible(&term, &sum) {
                break;
            }
            sum = sum.add(&term);
            k += 2;
        }
        sum
    };
    if r.is_zero() {
        return (Big::zero(), Big::one(n));
    }
    (series(r.clone(), 2), series(Big::one(n), 1))
}

/// ln(m) for m in [1/2, 2), with m - 1 exact: 2 atanh((m - 1) / (m + 1)),
/// |(m - 1) / (m + 1)| <= 1/3. Precise relative to the result.
fn ln_near_one(m_minus_1: &Big, n: usize) -> Big {
    if m_minus_1.is_zero() {
        return Big::zero();
    }
    let two = Big::from_u64(2, n);
    let u = m_minus_1.div(&two.add(m_minus_1));
    atanh(&u).scale(1)
}

/// log2(x) for a finite x > 0: e + ln(m) / ln 2 with x = m × 2^e and
/// m in [sqrt(1/2), sqrt(2)), so that e and the logarithm of m never
/// cancel.
fn log2(x: F80, n: usize) -> Big {
    let (mut e, m) = x.unpack();
    // m × 2^-63 in [1, 2); halve it above sqrt(2) (0xb504f333f9de6484…).
    let mut mm = Big::from_mag(false, -63, vec![m], n);
    if m > 0xb504_f333_f9de_6484 {
        mm = mm.scale(-1);
        e += 1;
    }
    let ln_m = ln_near_one(&mm.sub(&Big::one(n)), n);
    ln_m.div(&ln2(n)).add(&Big::from_mag(
        e < 0,
        0,
        vec![u64::from(e.unsigned_abs())],
        n,
    ))
}

/// log2(1 + x), for x > -1 finite: through log1p for small x, so that a
/// tiny x keeps its relative precision.
fn log2_1p(x: &Big, n: usize) -> Big {
    let threshold = Big::from_mag(false, -2, vec![1], n);
    if x.cmp_mag(&threshold) == Ordering::Less {
        // ln(1 + x) = 2 atanh(x / (2 + x)).
        return ln_near_one(x, n).div(&ln2(n));
    }
    // 1 + x exactly, then as a general logarithm.
    let sum = Big::one(n + 1).add(x);
    log2_big(&sum, n)
}

/// log2 of a positive `Big` (any exponent).
fn log2_big(x: &Big, n: usize) -> Big {
    // x = m × 2^e with m in [1, 2): e = top - 1.
    let mut e = x.top() - 1;
    let mut m = x.clone().scale(-e);
    let sqrt2 = Big::from_mag(false, -63, vec![0xb504_f333_f9de_6484], n);
    if m.cmp_mag(&sqrt2) == Ordering::Greater {
        m = m.scale(-1);
        e += 1;
    }
    let ln_m = ln_near_one(&m.sub(&Big::one(n)), n);
    ln_m.div(&ln2(n))
        .add(&Big::from_mag(e < 0, 0, vec![e.unsigned_abs()], n))
}

/// The rounded result of a computation `f(n) -> (value, correct bits)`,
/// with `correct bits` the bits of `value` guaranteed (relative error below
/// 2^-bits). Ziv's strategy: retry with more precision until both ends of
/// the error interval round alike.
fn ziv(rc: Rounding, f: impl Fn(usize) -> (Big, i64)) -> Out<F80> {
    let mut last = None;
    for n in PRECISIONS {
        let (value, bits) = f(n);
        if value.is_zero() {
            return out(F80::zero(value.neg), 0);
        }
        let delta = Big::from_mag(false, value.top() - bits, vec![1], n);
        let low = value.clone().abs().sub(&delta);
        let high = value.clone().abs().add(&delta);
        let (low, high) = if value.neg {
            (low.negate(), high.negate())
        } else {
            (low, high)
        };
        let (a, b) = (low.round(rc), high.round(rc));
        let result = value.round(rc);
        if bits > 64 + 2 && a.value == b.value && a.rounded_up == b.rounded_up {
            return with_inexact(result);
        }
        last = Some(result);
    }
    with_inexact(last.expect("PRECISIONS is not empty"))
}

/// `x` (an exact value) moved toward zero (`below`) or away from it by a
/// tiny relative amount, then rounded: the result and flags of a function
/// whose true value is known to lie just there.
fn nudged(x: &Big, below: bool, rc: Rounding) -> Out<F80> {
    let x = Big::from_mag(x.neg, x.exp, x.limbs.clone(), 3);
    let tiny = x.clone().abs().scale(-80);
    let moved = if below {
        x.clone().abs().sub(&tiny)
    } else {
        x.clone().abs().add(&tiny)
    };
    with_inexact(if x.neg { moved.negate() } else { moved }.round(rc))
}

/// The results of the transcendentals are never exact here (exact cases
/// are handled before): PE is always set.
fn with_inexact(mut result: Out<F80>) -> Out<F80> {
    result.flags |= FLAG_INEXACT;
    result
}

fn add_flags(mut result: Out<F80>, flags: u16) -> Out<F80> {
    result.flags |= flags;
    result
}

fn denormal(values: &[F80]) -> u16 {
    if values.iter().any(|v| v.class() == Class::Denormal) {
        FLAG_DENORMAL
    } else {
        0
    }
}

/// A one-operand NaN or invalid encoding: its result, or None.
fn nan_or_invalid(a: F80) -> Option<Out<F80>> {
    match a.class() {
        Class::Invalid => Some(invalid()),
        Class::QuietNan => Some(out(a, 0)),
        Class::SignalingNan => Some(out(a.quiet(), FLAG_INVALID)),
        _ => None,
    }
}

/// A two-operand NaN or invalid encoding.
fn nan_or_invalid2(a: F80, b: F80) -> Option<Out<F80>> {
    if a.class() == Class::Invalid || b.class() == Class::Invalid {
        return Some(invalid());
    }
    propagate(a, b)
}

/// f2xm1: 2^x - 1. The SDM defines it for -1 <= x <= 1; outside, the
/// exact value is returned too.
pub fn exp2m1(x: F80, rc: Rounding) -> Out<F80> {
    if let Some(result) = nan_or_invalid(x) {
        return result;
    }
    match x.class() {
        Class::Zero => return out(x, 0),
        Class::Infinity if x.sign => return out(F80::ONE.negate(), 0),
        Class::Infinity => return out(x, 0),
        _ => {}
    }
    let flags = denormal(&[x]);
    let (e, m) = x.unpack();
    if !x.sign && e > 20 {
        // 2^x beyond any finite F80: overflow.
        return add_flags(Big::from_mag(false, 1 << 20, vec![1], 3).round(rc), flags);
    }
    if x.sign && e >= 7 {
        // x <= -128: -1 plus a value below a quarter of the last place of
        // the numbers just above -1.
        let below_one = match rc {
            Rounding::Nearest | Rounding::Down => F80::ONE.negate(),
            Rounding::Up | Rounding::Zero => F80 {
                sign: true,
                exp: 0x3ffe,
                mant: u64::MAX,
            },
        };
        return Out {
            value: below_one,
            flags: flags | FLAG_INEXACT,
            rounded_up: below_one.exp == 0x3fff,
        };
    }
    // An integer x: 2^x - 1 is a dyadic rational, computed exactly and
    // rounded once (exact for -127 <= x <= 64).
    if e >= 0 && m << e << 1 == 0 {
        let k = (m >> (63 - e)) as i64 * if x.sign { -1 } else { 1 };
        // Above 16384, 2^k overflows: a rounding of 2^k alone says so.
        let n = (k.unsigned_abs().min(16500) as usize) / 64 + 3;
        let value = if k > 16500 {
            Big::one(3).scale(k)
        } else {
            Big::one(n).scale(k).sub(&Big::one(n))
        };
        // PE even when exact, as an Intel Xeon X5675 does.
        return add_flags(value.round(rc), flags | FLAG_INEXACT);
    }
    add_flags(
        ziv(rc, |n| {
            let x = Big::from_f80(x, n);
            if e < 0 {
                // |x| < 1: e^(x ln 2) - 1 directly.
                (expm1(&x.mul(&ln2(n))), 64 * n as i64 - MARGIN)
            } else {
                // 2^x = 2^k × 2^f with k the nearest integer, |f| <= 1/2.
                let k = x.round_to_int();
                let f = x.sub(&Big::from_mag(k < 0, 0, vec![k.unsigned_abs()], n));
                let two_f = expm1(&f.mul(&ln2(n))).add(&Big::one(n));
                let value = two_f.scale(k).sub(&Big::one(n));
                // The subtraction of 1 cancels at most when k = 0 or -1,
                // which loses no more than the bits of 2^f near 1.
                (value, 64 * n as i64 - MARGIN - 70)
            }
        }),
        flags,
    )
}

/// The sign of log2 of a finite positive x: less than, equal to or above 0.
fn log_sign(x: F80) -> Ordering {
    let (e, m) = x.unpack();
    if e == 0 && m == 1 << 63 {
        Ordering::Equal
    } else if e >= 0 {
        Ordering::Greater
    } else {
        Ordering::Less
    }
}

/// fyl2x: y × log2(x), y in ST(1) and x in ST(0).
pub fn ylog2x(y: F80, x: F80, rc: Rounding) -> Out<F80> {
    if let Some(result) = nan_or_invalid2(y, x) {
        return result;
    }
    let flags = denormal(&[x, y]);
    let (cx, cy) = (x.class(), y.class());
    if x.sign && cx != Class::Zero {
        return invalid();
    }
    let infinity = |sign| add_flags(out(F80::infinity(sign), 0), flags);
    let zero = |sign| add_flags(out(F80::zero(sign), 0), flags);
    if cx == Class::Zero {
        return match cy {
            Class::Zero => invalid(),
            Class::Infinity => infinity(!y.sign),
            _ => add_flags(out(F80::infinity(!y.sign), FLAG_ZERO_DIVIDE), flags),
        };
    }
    if cx == Class::Infinity {
        return match cy {
            Class::Zero => invalid(),
            _ => infinity(y.sign),
        };
    }
    let sign = log_sign(x);
    if cy == Class::Infinity {
        return match sign {
            Ordering::Equal => invalid(),
            Ordering::Greater => infinity(y.sign),
            Ordering::Less => infinity(!y.sign),
        };
    }
    if cy == Class::Zero || sign == Ordering::Equal {
        // ±0 × log2(x), or y × 0.
        let negative = (y.sign && sign != Ordering::Less) || (!y.sign && sign == Ordering::Less);
        return zero(if sign == Ordering::Equal {
            y.sign
        } else {
            negative
        });
    }
    // A power of two: log2 is an integer, y × e is rounded once.
    let (e, m) = x.unpack();
    if m == 1 << 63 {
        // PE even when exact, as an Intel Xeon X5675 does.
        let product = super::mul(y, super::from_int(i64::from(e)), Mode::from(rc));
        return add_flags(product, flags | FLAG_INEXACT);
    }
    add_flags(
        ziv(rc, |n| {
            (Big::from_f80(y, n).mul(&log2(x, n)), 64 * n as i64 - MARGIN)
        }),
        flags,
    )
}

/// fyl2xp1: y × log2(1 + x), y in ST(1) and x in ST(0). The SDM defines
/// it for |x| < 1 - sqrt(2)/2; it is computed for any x > -1.
pub fn ylog2xp1(y: F80, x: F80, rc: Rounding) -> Out<F80> {
    if let Some(result) = nan_or_invalid2(y, x) {
        return result;
    }
    let flags = denormal(&[x, y]);
    let (cx, cy) = (x.class(), y.class());
    // 1 + x <= 0: x = -1 is log2(0), as fyl2x of 0; below, invalid.
    if x.sign && matches!(cx, Class::Normal | Class::Infinity) {
        let (e, m) = x.unpack();
        if cx == Class::Infinity || e > 0 || (e == 0 && m != 1 << 63) {
            return invalid();
        }
        if e == 0 {
            return ylog2x(y, F80::ZERO, rc);
        }
    }
    if cx == Class::Zero {
        return match cy {
            Class::Infinity => invalid(),
            _ => add_flags(out(F80::zero(x.sign != y.sign), 0), flags),
        };
    }
    if cx == Class::Infinity {
        return match cy {
            Class::Zero => invalid(),
            _ => add_flags(out(F80::infinity(y.sign), 0), flags),
        };
    }
    // log2(1 + x) has the sign of x.
    if cy == Class::Infinity {
        return add_flags(out(F80::infinity(y.sign != x.sign), 0), flags);
    }
    if cy == Class::Zero {
        return add_flags(out(F80::zero(y.sign != x.sign), 0), flags);
    }
    // 1 + x a power of two (x = 1, 3, -1/2, …): log2 is an integer k, and
    // y × k is rounded once.
    let (e, _) = x.unpack();
    if (-64..=62).contains(&e) {
        let sum = Big::one(3).add(&Big::from_f80(x, 3));
        if sum.limbs[..2].iter().all(|&l| l == 0) && sum.limbs[2] == 1 << 63 {
            let k = sum.top() - 1;
            let product = super::mul(y, super::from_int(k), Mode::from(rc));
            return add_flags(product, flags | FLAG_INEXACT);
        }
    }
    add_flags(
        ziv(rc, |n| {
            (
                Big::from_f80(y, n).mul(&log2_1p(&Big::from_f80(x, n), n)),
                64 * n as i64 - MARGIN,
            )
        }),
        flags,
    )
}

/// The x87 rounding of pi × `num` / 4 with a sign, for the exact angles of
/// fpatan's special cases (always inexact).
fn pi_times(num: u64, negative: bool, rc: Rounding) -> Out<F80> {
    ziv(rc, |n| {
        let value = quarter_pi(n).mul_small(num);
        (
            if negative { value.negate() } else { value },
            64 * n as i64 - MARGIN,
        )
    })
}

/// fpatan: the angle of (x, y), y in ST(1) and x in ST(0), in [-pi, pi].
pub fn atan2(y: F80, x: F80, rc: Rounding) -> Out<F80> {
    if let Some(result) = nan_or_invalid2(y, x) {
        return result;
    }
    let flags = denormal(&[x, y]);
    let (cx, cy) = (x.class(), y.class());
    let with = |result: Out<F80>| add_flags(result, flags);
    match (cy, cx) {
        (Class::Zero, _) if !x.sign => return with(out(y, 0)),
        (Class::Zero, _) => return with(pi_times(4, y.sign, rc)),
        (Class::Infinity, Class::Infinity) => {
            return with(pi_times(if x.sign { 3 } else { 1 }, y.sign, rc));
        }
        (Class::Infinity, _) | (_, Class::Zero) => return with(pi_times(2, y.sign, rc)),
        (_, Class::Infinity) if !x.sign => return with(out(F80::zero(y.sign), 0)),
        (_, Class::Infinity) => return with(pi_times(4, y.sign, rc)),
        _ => {}
    }
    // A tiny exact quotient |y| / |x| with x > 0: atan is just below it.
    if !x.sign && y.unpack().0 - x.unpack().0 < TINY_EXP {
        let (a, b) = (Big::from_f80(y, 3).abs(), Big::from_f80(x, 3).abs());
        let q = a.div(&b);
        if q.mul(&b).cmp_mag(&a) == Ordering::Equal && q.limbs[..2] == [0, 0] {
            let q = if y.sign { q.negate() } else { q };
            return with(nudged(&q, true, rc));
        }
    }
    with(ziv(rc, |n| {
        let (a, b) = (Big::from_f80(y, n).abs(), Big::from_f80(x, n).abs());
        // The first octant, then the others by symmetry; the angles added
        // are larger than the atan subtracted, so nothing cancels badly.
        let mut angle = if a.cmp_mag(&b) != Ordering::Greater {
            atan(&a.div(&b))
        } else {
            quarter_pi(n).scale(1).sub(&atan(&b.div(&a)))
        };
        if x.sign {
            angle = quarter_pi(n).scale(2).sub(&angle);
        }
        (
            if y.sign { angle.negate() } else { angle },
            64 * n as i64 - MARGIN,
        )
    }))
}

/// What fsin, fcos, fsincos and fptan do with an operand.
pub enum Trig<T> {
    /// |x| >= 2^63: C2 = 1 and ST(0) unchanged.
    OutOfRange,
    Done(Out<T>),
}

/// The operand checks shared by the trigonometric instructions: NaNs,
/// infinities (invalid), the range, zeros (`zero`), or None to compute.
fn trig_special<T>(
    x: F80,
    nan: impl Fn(Out<F80>) -> Out<T>,
    zero: impl Fn() -> Out<T>,
) -> Option<Trig<T>> {
    if let Some(result) = nan_or_invalid(x) {
        return Some(Trig::Done(nan(result)));
    }
    match x.class() {
        Class::Infinity => Some(Trig::Done(nan(invalid()))),
        Class::Zero => Some(Trig::Done(zero())),
        _ if x.unpack().0 >= 63 => Some(Trig::OutOfRange),
        _ => None,
    }
}

/// x reduced by the nearest multiple k of pi/2: (r, k mod 4, correct bits
/// of r). pi is taken with 128 extra bits; the cancellation of x - k pi/2
/// is measured and the bits it costs are subtracted.
fn reduce(x: F80, n: usize) -> (Big, u64, i64) {
    let wide = n + 2;
    let half_pi = quarter_pi(wide).scale(1);
    let xb = Big::from_f80(x, wide);
    let k = xb.div(&half_pi).round_to_int();
    let r = xb.sub(&half_pi.mul(&Big::from_mag(k < 0, 0, vec![k.unsigned_abs()], wide)));
    // The series for pi/4 truncates once per term, 64 wide terms at most:
    // pi/2 errs by less than 2^(16 - 64 wide); times |k| < 2^64.
    let error_exp = 64 + 16 - 64 * wide as i64 + 1;
    let bits = if r.is_zero() {
        0
    } else {
        r.top() - 1 - error_exp
    };
    let r = Big::from_mag(r.neg, r.exp, r.limbs, n);
    (r, k.rem_euclid(4) as u64, bits)
}

/// The correct bits of a series result from a reduced argument.
fn series_bits(reduced_bits: i64, n: usize) -> i64 {
    reduced_bits.min(64 * n as i64) - MARGIN
}

/// fsin.
pub fn sin(x: F80, rc: Rounding) -> Trig<F80> {
    if let Some(special) = trig_special(x, |r| r, || out(x, 0)) {
        return special;
    }
    let flags = denormal(&[x]);
    if x.unpack().0 < TINY_EXP {
        return Trig::Done(add_flags(nudged(&Big::from_f80(x, 3), true, rc), flags));
    }
    Trig::Done(add_flags(
        ziv(rc, |n| {
            let (r, q, bits) = reduce(x, n);
            let (s, c) = sin_cos(&r);
            let value = match q {
                0 => s,
                1 => c,
                2 => s.negate(),
                _ => c.negate(),
            };
            (value, series_bits(bits, n))
        }),
        flags,
    ))
}

/// fcos.
pub fn cos(x: F80, rc: Rounding) -> Trig<F80> {
    if let Some(special) = trig_special(x, |r| r, || out(F80::ONE, 0)) {
        return special;
    }
    let flags = denormal(&[x]);
    if x.unpack().0 < TINY_EXP {
        return Trig::Done(add_flags(nudged(&Big::one(3), true, rc), flags));
    }
    Trig::Done(add_flags(
        ziv(rc, |n| {
            let (r, q, bits) = reduce(x, n);
            let (s, c) = sin_cos(&r);
            let value = match q {
                0 => c,
                1 => s.negate(),
                2 => c.negate(),
                _ => s,
            };
            (value, series_bits(bits, n))
        }),
        flags,
    ))
}

/// fptan: tan(x), which the instruction follows by pushing 1.
pub fn tan(x: F80, rc: Rounding) -> Trig<F80> {
    if let Some(special) = trig_special(x, |r| r, || out(x, 0)) {
        return special;
    }
    let flags = denormal(&[x]);
    if x.unpack().0 < TINY_EXP {
        return Trig::Done(add_flags(nudged(&Big::from_f80(x, 3), false, rc), flags));
    }
    Trig::Done(add_flags(
        ziv(rc, |n| {
            let (r, q, bits) = reduce(x, n);
            let (s, c) = sin_cos(&r);
            let value = if q % 2 == 0 {
                s.div(&c)
            } else {
                c.div(&s).negate()
            };
            (value, series_bits(bits, n) - 2)
        }),
        flags,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(x: f64) -> F80 {
        super::super::from_float(x.to_bits(), true).value
    }

    fn d(x: F80) -> f64 {
        f64::from_bits(super::super::to_float(x, true, Rounding::Nearest).value)
    }

    fn done(t: Trig<F80>) -> Out<F80> {
        match t {
            Trig::Done(out) => out,
            Trig::OutOfRange => panic!("out of range"),
        }
    }

    #[test]
    fn constants_are_right() {
        // pi/4 and ln 2 to 128 bits, against the fldpi and fldln2 tables.
        let pi = quarter_pi(3).scale(2);
        assert_eq!(
            pi.round(Rounding::Nearest).value,
            super::super::constant(super::super::Constant::Pi, Rounding::Nearest)
        );
        assert_eq!(
            ln2(3).round(Rounding::Zero).value,
            super::super::constant(super::super::Constant::LnTwo, Rounding::Zero)
        );
        assert_eq!(
            ln2(3).round(Rounding::Up).value,
            super::super::constant(super::super::Constant::LnTwo, Rounding::Up)
        );
    }

    #[test]
    fn results_agree_with_the_host_libm_to_double_precision() {
        let r = Rounding::Nearest;
        let close = |got: f64, expect: f64| (got - expect).abs() <= expect.abs() * 5e-16;
        for &x in &[0.5, -0.75, 1e-10, 0.999, -1e-300] {
            let expect = (x * std::f64::consts::LN_2).exp_m1();
            assert!(close(d(exp2m1(f(x), r).value), expect), "f2xm1 {x}");
        }
        for &(y, x) in &[(1.0, 3.0), (-2.5, 0.1), (3.0, 1e-200), (1.0, 1.0000001)] {
            let expect = y * f64::log2(x);
            assert!(
                (d(ylog2x(f(y), f(x), r).value) - expect).abs() <= expect.abs() * 5e-16,
                "fyl2x {y} {x}"
            );
        }
        for &(y, x) in &[(1.0, 1e-12), (2.0, 0.25), (-1.0, -0.2)] {
            let expect = y * f64::ln_1p(x) / std::f64::consts::LN_2;
            assert!(
                (d(ylog2xp1(f(y), f(x), r).value) - expect).abs() <= expect.abs() * 5e-16,
                "fyl2xp1 {y} {x}"
            );
        }
        for &(y, x) in &[
            (1.0, 1.0),
            (-2.0, 0.5),
            (3.0, -4.0),
            (-1e-20, -1.0),
            (5.0, 1e-30),
        ] {
            let expect = f64::atan2(y, x);
            assert!(
                (d(atan2(f(y), f(x), r).value) - expect).abs() <= expect.abs() * 5e-16,
                "fpatan {y} {x}"
            );
        }
        for &x in &[0.5f64, -3.0, 100.0, 1e-8, 1e6, 7.0e15] {
            let (s, c, t) = (x.sin(), x.cos(), x.tan());
            assert!(
                (d(done(sin(f(x), r)).value) - s).abs() <= s.abs() * 5e-16,
                "fsin {x}"
            );
            assert!(
                (d(done(cos(f(x), r)).value) - c).abs() <= c.abs() * 5e-16,
                "fcos {x}"
            );
            assert!(
                (d(done(tan(f(x), r)).value) - t).abs() <= t.abs() * 5e-16,
                "fptan {x}"
            );
        }
    }

    #[test]
    fn special_cases_follow_the_sdm() {
        let r = Rounding::Nearest;
        // f2xm1: ±0, ±1, -infinity.
        assert_eq!(exp2m1(f(-0.0), r), out(f(-0.0), 0));
        assert_eq!(d(exp2m1(f(1.0), r).value), 1.0);
        assert_eq!(d(exp2m1(f(-1.0), r).value), -0.5);
        assert_eq!(d(exp2m1(f(f64::NEG_INFINITY), r).value), -1.0);
        // fyl2x: log of a negative number, zero divide, a power of two.
        assert_eq!(ylog2x(f(1.0), f(-2.0), r).flags, FLAG_INVALID);
        let z = ylog2x(f(3.0), f(0.0), r);
        assert_eq!((d(z.value), z.flags), (f64::NEG_INFINITY, FLAG_ZERO_DIVIDE));
        // PE even when exact, as the Xeon does.
        let p = ylog2x(f(3.0), f(8.0), r);
        assert_eq!((d(p.value), p.flags), (9.0, FLAG_INEXACT));
        assert_eq!(
            d(ylog2x(f(-2.0), f(1.0), r).value).to_bits(),
            (-0.0f64).to_bits()
        );
        // fpatan: signed zeros and infinities.
        assert_eq!(
            d(atan2(f(-0.0), f(1.0), r).value).to_bits(),
            (-0.0f64).to_bits()
        );
        assert_eq!(d(atan2(f(0.0), f(-0.0), r).value), std::f64::consts::PI);
        assert_eq!(
            d(atan2(f(f64::INFINITY), f(f64::NEG_INFINITY), r).value),
            3.0 * std::f64::consts::FRAC_PI_4
        );
        // fsin: out of range, infinities, zero.
        assert!(matches!(sin(f(1e19), r), Trig::OutOfRange));
        assert_eq!(done(sin(f(f64::INFINITY), r)).flags, FLAG_INVALID);
        assert_eq!(done(cos(f(-0.0), r)).value, F80::ONE);
    }

    #[test]
    fn tiny_arguments_round_from_the_right_side() {
        // sin of a denormal: x itself to nearest (rounded up: C1), UE and
        // PE (✅ Xeon X5675), DE; toward zero the next value down.
        let x = F80 {
            sign: false,
            exp: 0,
            mant: 0x6_6327_8e62,
        };
        let near = done(sin(x, Rounding::Nearest));
        assert_eq!(near.value, x);
        assert!(near.rounded_up);
        assert_eq!(
            near.flags,
            FLAG_DENORMAL | super::super::FLAG_UNDERFLOW | FLAG_INEXACT
        );
        assert_eq!(done(sin(x, Rounding::Zero)).value.mant, x.mant - 1);
        // tan goes the other way.
        assert_eq!(done(tan(x, Rounding::Up)).value.mant, x.mant + 1);
        // cos of a tiny value: 1 to nearest, just below 1 toward zero.
        assert_eq!(done(cos(f(1e-20), Rounding::Nearest)).value, F80::ONE);
        assert_eq!(done(cos(f(1e-20), Rounding::Zero)).value.mant, u64::MAX);
        // atan of an exact tiny quotient.
        let a = atan2(f(1.0), f(2.0f64.powi(60)), Rounding::Down);
        assert_eq!(a.value.mant, u64::MAX);
        assert_eq!(a.value.exp, 0x3fff - 61);
    }

    #[test]
    fn directed_roundings_bracket_the_value() {
        // The results in the four modes: down <= nearest <= up, and down
        // and up one unit apart (never exact).
        for x in [0.3, 2.0, -5.5, 1234.5678] {
            let get = |rc| done(sin(f(x), rc)).value;
            let (down, up) = (get(Rounding::Down), get(Rounding::Up));
            assert_eq!(
                up.mant.wrapping_sub(down.mant) & 0xff,
                if up.sign { 0xff } else { 1 },
                "sin {x}"
            );
            let zero = get(Rounding::Zero);
            assert!(zero == down || zero == up);
        }
    }
}

#[cfg(test)]
/// Cases for an independent check of the rounding (tests/x87/reference.py).
mod generate {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn f80(&mut self, emin: i32, emax: i32, sign: Option<bool>) -> F80 {
            let e = emin + (self.next() % (emax - emin + 1) as u64) as i32;
            let sign = sign.unwrap_or(self.next() & 1 == 1);
            F80 {
                sign,
                exp: (e + 16383) as u16,
                mant: self.next() | 1 << 63,
            }
        }
    }

    fn hex(x: F80) -> String {
        format!(
            "{}{:04x}{:016x}",
            if x.sign { "-" } else { "+" },
            x.exp,
            x.mant
        )
    }

    /// Writes 96,000 cases (every instruction, four rounding modes) to the
    /// file in `ARMOR_X87_CASES`, for tests/x87/reference.py. The seed is
    /// `ARMOR_X87_SEED` (docs/testing.md).
    #[test]
    #[ignore]
    fn write_cases() {
        let path = std::env::var("ARMOR_X87_CASES").expect("ARMOR_X87_CASES: the output file");
        let seed: u64 = std::env::var("ARMOR_X87_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0x1234_5678_9abc_def1);
        let mut out = String::new();
        let mut rng = Rng(seed | 1);
        let modes = [
            Rounding::Nearest,
            Rounding::Down,
            Rounding::Up,
            Rounding::Zero,
        ];
        let mut line = |op: &str, a: F80, b: F80, r: Out<F80>, rc: Rounding| {
            out.push_str(&format!(
                "{op} {:?} {} {} {} {:x} {}\n",
                rc,
                hex(a),
                hex(b),
                hex(r.value),
                r.flags,
                r.rounded_up as u8
            ));
        };
        for _ in 0..3000 {
            let x = rng.f80(-80, -1, None);
            let tiny = rng.f80(-3000, -100, None);
            let y = rng.f80(-20, 20, None);
            let xl = rng.f80(-2000, 2000, Some(false));
            let xp = rng.f80(-90, -3, None);
            let (ay, ax) = (rng.f80(-60, 60, None), rng.f80(-60, 60, None));
            let t = rng.f80(-34, 62, None);
            for rc in modes {
                line("f2xm1", x, F80::ZERO, exp2m1(x, rc), rc);
                line("f2xm1", tiny, F80::ZERO, exp2m1(tiny, rc), rc);
                line("fyl2x", y, xl, ylog2x(y, xl, rc), rc);
                line("fyl2xp1", y, xp, ylog2xp1(y, xp, rc), rc);
                line("fpatan", ay, ax, atan2(ay, ax, rc), rc);
                for (op, r) in [
                    ("fsin", sin(t, rc)),
                    ("fcos", cos(t, rc)),
                    ("fptan", tan(t, rc)),
                ] {
                    if let Trig::Done(r) = r {
                        line(op, t, F80::ZERO, r, rc);
                    }
                }
            }
        }
        std::fs::write(path, out).unwrap();
    }
}
