//! SSE floating point with x86 semantics, in software (ADR-0024).
//!
//! Every operation works on raw bits and returns its result with the MXCSR
//! exception flags it raised (bits 0..5: IE, DE, ZE, OE, UE, PE, the same
//! positions as the x87 status word). The arithmetic is the exact integer
//! engine of `f80.rs`, rounded once to single or double (`Mode::float`):
//! every rounding mode, flush-to-zero, and tininess detected after
//! rounding. No host floating-point instruction runs here.
//!
//! x86 rules (docs/tech/floating-point.md; ✅ against qemu, and against an
//! Intel Xeon X5675 where qemu differs, docs/testing.md):
//! - a NaN operand propagates as the first source operand that is a NaN,
//!   made quiet; IE only for a signalling NaN, and no DE then;
//! - an invalid operation without NaN operands (inf - inf, 0 * inf, 0 / 0,
//!   sqrt of a negative number, ...) returns the negative "QNaN
//!   indefinite" with IE;
//! - IE and ZE leave out DE (`denormal / 0` raises ZE alone); DE coexists
//!   with OE, UE and PE;
//! - min and max return the second operand when either is a NaN (IE, also
//!   for a quiet one) or both are zeros;
//! - comis and the ordered compare predicates raise IE for any NaN, ucomis
//!   and the others for signalling NaNs only;
//! - an out-of-range or NaN conversion to an integer returns the "integer
//!   indefinite" with IE; conversions to integers never raise DE;
//! - rcp and rsqrt raise no flag at all.
//!
//! `Env::daz` (denormals are zero) replaces denormal operands by zeros
//! before anything else, without DE. `Env::ftz` (flush to zero) applies
//! only while UE is masked, as on x86.
//!
//! With OE or UE unmasked the flags change (`unmasked_post`); whether the
//! instruction then raises #XM is the caller's business (`sse.rs`).

use armor_x86::FloatOp;

use crate::f80::{
    self, F80, FLAG_DENORMAL, FLAG_INEXACT, FLAG_INVALID, FLAG_OVERFLOW, FLAG_UNDERFLOW,
    FLAG_ZERO_DIVIDE, Mode, Out, Rounding,
};

/// The MXCSR control bits the operations need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Env {
    pub rc: Rounding,
    /// FZ: tiny results become zeros (with UE and PE).
    pub ftz: bool,
    /// DAZ: denormal operands are zeros.
    pub daz: bool,
    /// The exception flags whose mask bit is clear.
    pub unmasked: u16,
}

impl Env {
    pub(crate) fn from_mxcsr(mxcsr: u32) -> Env {
        // The masks (bits 7..12) are in the order of the flags.
        let unmasked = (!(mxcsr >> 7) & 0x3f) as u16;
        Env {
            // RC (bits 13, 14) has the x87 RC encoding.
            rc: Rounding::from_control_word((mxcsr >> 3) as u16),
            // FZ works only with UE masked (SDM vol. 1, §10.2.3.3).
            ftz: mxcsr & (1 << 15) != 0 && unmasked & FLAG_UNDERFLOW == 0,
            daz: mxcsr & (1 << 6) != 0,
            unmasked,
        }
    }
}

/// A raw single (`double` false) or double.
#[derive(Clone, Copy)]
struct Bits {
    raw: u64,
    double: bool,
}

impl Bits {
    fn new(raw: u64, double: bool) -> Bits {
        Bits { raw, double }
    }
    fn frac_bits(self) -> u32 {
        if self.double { 52 } else { 23 }
    }
    fn exp_bits(self) -> u32 {
        if self.double { 11 } else { 8 }
    }
    fn exponent(self) -> u64 {
        (self.raw >> self.frac_bits()) & ((1 << self.exp_bits()) - 1)
    }
    fn fraction(self) -> u64 {
        self.raw & ((1 << self.frac_bits()) - 1)
    }
    fn sign_bit(self) -> u64 {
        1 << (self.frac_bits() + self.exp_bits())
    }
    fn quiet_bit(self) -> u64 {
        1 << (self.frac_bits() - 1)
    }
    fn is_nan(self) -> bool {
        self.exponent() == (1 << self.exp_bits()) - 1 && self.fraction() != 0
    }
    fn is_snan(self) -> bool {
        self.is_nan() && self.raw & self.quiet_bit() == 0
    }
    fn is_denormal(self) -> bool {
        self.exponent() == 0 && self.fraction() != 0
    }
    fn quiet(self) -> u64 {
        self.raw | self.quiet_bit()
    }
    /// The negative QNaN "indefinite".
    fn indefinite(self) -> u64 {
        self.sign_bit() | (((1 << self.exp_bits()) - 1) << self.frac_bits()) | self.quiet_bit()
    }
}

/// An operand as an exact `F80` (not a NaN), after DAZ, with DE for a
/// denormal one.
fn input(b: Bits, env: Env) -> (F80, u16) {
    if env.daz && b.is_denormal() {
        let zero = F80 {
            sign: b.raw & b.sign_bit() != 0,
            exp: 0,
            mant: 0,
        };
        return (zero, 0);
    }
    let converted = f80::from_float(b.raw, b.double);
    (converted.value, converted.flags)
}

/// A result held exactly by `value` (`Mode::float` produced it, or it is
/// special) as raw bits.
fn output(value: F80, double: bool) -> u64 {
    f80::to_float(value, double, Rounding::Nearest).value
}

/// The x86 priority: IE and ZE leave out DE (✅ Xeon X5675).
fn priority(flags: u16) -> u16 {
    if flags & (FLAG_INVALID | FLAG_ZERO_DIVIDE) != 0 {
        flags & !FLAG_DENORMAL
    } else {
        flags
    }
}

/// The NaN result of a two-operand operation with a NaN operand: the
/// first NaN, made quiet; IE for a signalling one.
fn nan_result(a: Bits, b: Bits) -> Option<(u64, u16)> {
    let flags = if a.is_snan() || b.is_snan() {
        FLAG_INVALID
    } else {
        0
    };
    if a.is_nan() {
        Some((a.quiet(), flags))
    } else if b.is_nan() {
        Some((b.quiet(), flags))
    } else {
        None
    }
}

/// `op a, b` (`a` is the destination and first source; sqrt, rcp and
/// rsqrt use `b` only), with its flags.
pub(crate) fn arith(op: FloatOp, double: bool, a: u64, b: u64, env: Env) -> (u64, u16) {
    let (x, y) = (Bits::new(a, double), Bits::new(b, double));
    let mode = Mode::float(double, env.rc, env.ftz);
    match op {
        FloatOp::Min | FloatOp::Max => {
            if x.is_nan() || y.is_nan() {
                return (b, FLAG_INVALID);
            }
            let ((xa, fa), (xb, fb)) = (input(x, env), input(y, env));
            let order = f80::compare(xa, xb, true).value;
            let first = match op {
                FloatOp::Min => order == f80::Compare::Less,
                _ => order == f80::Compare::Greater,
            };
            // With DAZ, a denormal operand is returned as the zero it was
            // taken for.
            let pick = |bits: Bits, value: F80| {
                if env.daz && bits.is_denormal() {
                    output(value, double)
                } else {
                    bits.raw
                }
            };
            let result = if first { pick(x, xa) } else { pick(y, xb) };
            return (result, fa | fb);
        }
        FloatOp::Rcp | FloatOp::Rsqrt => {
            // No flags, whatever the operand; to nearest (✅ Xeon X5675 for
            // the flags; the value is exact, as under qemu).
            let quiet = Env {
                rc: Rounding::Nearest,
                ftz: false,
                daz: env.daz,
                unmasked: 0,
            };
            let one = if double {
                0x3ff0_0000_0000_0000
            } else {
                0x3f80_0000
            };
            let divisor = if op == FloatOp::Rsqrt {
                arith(FloatOp::Sqrt, double, 0, b, quiet).0
            } else {
                b
            };
            return (arith(FloatOp::Div, double, one, divisor, quiet).0, 0);
        }
        FloatOp::Sqrt => {
            if y.is_nan() {
                let flags = if y.is_snan() { FLAG_INVALID } else { 0 };
                return (y.quiet(), flags);
            }
            let (v, f) = input(y, env);
            return finish(f80::sqrt(v, mode), f, y);
        }
        _ => {}
    }
    if let Some(nan) = nan_result(x, y) {
        return nan;
    }
    let ((va, fa), (vb, fb)) = (input(x, env), input(y, env));
    let compute = |mode: Mode| match op {
        FloatOp::Add => f80::add(va, vb, mode),
        FloatOp::Sub => f80::sub(va, vb, mode),
        FloatOp::Mul => f80::mul(va, vb, mode),
        _ => f80::div(va, vb, mode),
    };
    let (value, flags) = finish(compute(mode), fa | fb, x);
    let flags = unmasked_post(flags, double, env, || {
        compute(Mode::register(env.rc, precision(double)))
    });
    (value, flags)
}

fn precision(double: bool) -> u32 {
    if double { 53 } else { 24 }
}

/// The flags of a result with OE or UE unmasked (`flags` are the masked
/// ones). x86 then judges the result rounded to the format's precision
/// with an unbounded exponent (`wide`): OE if it is too large, UE if it is
/// tiny, exact or not, and PE with them only if that rounding was inexact
/// (✅ Xeon X5675, `c/sse_exceptions`: `DBL_MAX × 2` gives OE alone,
/// `DBL_MIN / 2` UE alone, `DBL_MIN / 3` UE and PE). The instruction then
/// raises #XM and writes nothing, so the result itself does not matter.
fn unmasked_post(flags: u16, double: bool, env: Env, wide: impl FnOnce() -> Out<F80>) -> u16 {
    let watched = env.unmasked & (FLAG_OVERFLOW | FLAG_UNDERFLOW);
    if watched == 0 || flags & (FLAG_INVALID | FLAG_ZERO_DIVIDE) != 0 {
        return flags;
    }
    let wide = wide();
    let v = wide.value;
    // Zero, infinity (from an infinite operand), NaN: no OE or UE.
    if v.mant == 0 || v.exp == 0x7fff {
        return flags;
    }
    let exponent = i32::from(v.exp) - 16383;
    let (emax, emin) = if double { (1023, -1022) } else { (127, -126) };
    let raised = if watched & FLAG_OVERFLOW != 0 && exponent > emax {
        FLAG_OVERFLOW
    } else if watched & FLAG_UNDERFLOW != 0 && exponent < emin {
        FLAG_UNDERFLOW
    } else {
        return flags;
    };
    flags & FLAG_DENORMAL | raised | wide.flags & FLAG_INEXACT
}

/// Bits and flags of an arithmetic result: f80's invalid result (its
/// indefinite) becomes the SSE indefinite of `like`'s format.
fn finish(result: Out<F80>, input_flags: u16, like: Bits) -> (u64, u16) {
    let flags = priority(result.flags | input_flags);
    let value = if result.flags & FLAG_INVALID != 0 {
        like.indefinite()
    } else {
        output(result.value, like.double)
    };
    (value, flags)
}

/// comis (`signaling`) / ucomis: (ZF, PF, CF) and the flags; also the
/// base of the cmp predicates.
pub(crate) fn compare(
    double: bool,
    a: u64,
    b: u64,
    signaling: bool,
    env: Env,
) -> ((bool, bool, bool), u16) {
    let (x, y) = (Bits::new(a, double), Bits::new(b, double));
    if x.is_nan() || y.is_nan() {
        let invalid = signaling || x.is_snan() || y.is_snan();
        return ((true, true, true), if invalid { FLAG_INVALID } else { 0 });
    }
    let ((va, fa), (vb, fb)) = (input(x, env), input(y, env));
    let codes = match f80::compare(va, vb, true).value {
        f80::Compare::Less => (false, false, true),
        f80::Compare::Greater => (false, false, false),
        _ => (true, false, false),
    };
    (codes, fa | fb)
}

/// The mask of a cmpss / cmpsd / cmpps / cmppd lane: predicate 0..7 is eq,
/// lt, le, unord, neq, nlt, nle, ord; lt, le, nlt and nle signal on QNaNs.
pub(crate) fn compare_mask(double: bool, predicate: u8, a: u64, b: u64, env: Env) -> (u64, u16) {
    let signaling = matches!(predicate, 1 | 2 | 5 | 6);
    let ((zf, pf, cf), flags) = compare(double, a, b, signaling, env);
    let unordered = pf;
    let equal = zf && !unordered;
    let less = cf && !unordered;
    let result = match predicate {
        0 => equal,
        1 => less,
        2 => less || equal,
        3 => unordered,
        4 => !equal,
        5 => !less,
        6 => !(less || equal),
        _ => !unordered,
    };
    let all_ones = if double { u64::MAX } else { 0xffff_ffff };
    (if result { all_ones } else { 0 }, flags)
}

/// cvt(t)ss2si / cvt(t)sd2si and the packed forms to an `int_bits`-bit
/// integer, as raw bits of that width: rounding by MXCSR, or truncation.
pub(crate) fn to_int(double: bool, a: u64, int_bits: u32, truncate: bool, env: Env) -> (u64, u16) {
    let x = Bits::new(a, double);
    if x.is_nan() {
        return (1u64 << (int_bits - 1), FLAG_INVALID);
    }
    // Conversions to integers raise no DE (✅ Xeon X5675).
    let (v, _) = input(x, env);
    let rc = if truncate { Rounding::Zero } else { env.rc };
    let result = f80::to_int(v, int_bits, rc);
    (result.value, result.flags)
}

/// roundss / roundsd / roundps / roundpd: `a` rounded to an integral
/// value by `rc`; a zero keeps the sign. IE for an SNaN (made quiet), PE
/// if inexact unless `exact` (imm bit 3), never DE (SDM, ROUNDSD).
pub(crate) fn round(double: bool, a: u64, rc: Rounding, exact: bool, env: Env) -> (u64, u16) {
    let x = Bits::new(a, double);
    if x.is_nan() {
        let flags = if x.is_snan() { FLAG_INVALID } else { 0 };
        return (x.quiet(), flags);
    }
    let (v, _) = input(x, env);
    let result = f80::round_to_integral(v, rc);
    let flags = if exact {
        0
    } else {
        result.flags & FLAG_INEXACT
    };
    (output(result.value, double), flags)
}

/// cvtsi2ss / cvtsi2sd and cvtdq2ps / cvtdq2pd: rounded by MXCSR (PE).
pub(crate) fn from_int(value: i64, double: bool, env: Env) -> (u64, u16) {
    let result = f80::to_float(f80::from_int(value), double, env.rc);
    (result.value, result.flags)
}

/// cvtss2sd and cvtps2pd: exact; DE for a denormal, and a NaN keeps its
/// sign and payload and becomes quiet (IE if it was signalling).
pub(crate) fn widen(a: u32, env: Env) -> (u64, u16) {
    let x = Bits::new(u64::from(a), false);
    if x.is_nan() {
        let sign = u64::from(a >> 31) << 63;
        let payload = u64::from(a & 0x007f_ffff) << (52 - 23);
        let flags = if x.is_snan() { FLAG_INVALID } else { 0 };
        return (sign | 0x7ff8_0000_0000_0000 | payload, flags);
    }
    let (v, flags) = input(x, env);
    (output(v, true), flags)
}

/// cvtsd2ss and cvtpd2ps: rounded by MXCSR (OE, UE, PE, FZ); a NaN keeps
/// its sign and the high bits of its payload and becomes quiet.
pub(crate) fn narrow(a: u64, env: Env) -> (u32, u16) {
    let x = Bits::new(a, true);
    if x.is_nan() {
        let sign = ((a >> 63) as u32) << 31;
        let payload = ((a & 0x000f_ffff_ffff_ffff) >> (52 - 23)) as u32;
        let flags = if x.is_snan() { FLAG_INVALID } else { 0 };
        return (sign | 0x7fc0_0000 | payload, flags);
    }
    let (v, input_flags) = input(x, env);
    let result = f80::round_to(v, Mode::float(false, env.rc, env.ftz));
    let flags = unmasked_post(input_flags | result.flags, false, env, || {
        f80::round_to(v, Mode::register(env.rc, precision(false)))
    });
    (output(result.value, false) as u32, flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: u64 = 0x3ff0_0000_0000_0000;
    const TWO: u64 = 0x4000_0000_0000_0000;
    const INF: u64 = 0x7ff0_0000_0000_0000;
    const QNAN_A: u64 = 0x7ff8_0000_0000_0001;
    const SNAN_B: u64 = 0x7ff0_0000_0000_0002;
    const DENORMAL: u64 = 1;
    const QUIET: u64 = 1 << 51;
    const INDEFINITE: u64 = 0xfff8_0000_0000_0000;
    const IE: u16 = 0x01;
    const DE: u16 = 0x02;
    const ZE: u16 = 0x04;
    const OE: u16 = 0x08;
    const UE: u16 = 0x10;
    const PE: u16 = 0x20;
    const ENV: Env = Env {
        rc: Rounding::Nearest,
        ftz: false,
        daz: false,
        unmasked: 0,
    };

    fn d(op: FloatOp, a: u64, b: u64) -> (u64, u16) {
        arith(op, true, a, b, ENV)
    }

    #[test]
    fn arithmetic_rounds_like_ieee() {
        assert_eq!(d(FloatOp::Add, ONE, TWO), (3.0f64.to_bits(), 0));
        assert_eq!(
            d(FloatOp::Div, ONE, 3.0f64.to_bits()),
            ((1.0f64 / 3.0).to_bits(), PE)
        );
        assert_eq!(d(FloatOp::Sqrt, 0, 4.0f64.to_bits()), (TWO, 0));
        let third = f32::from_bits(0x3eaa_aaab);
        assert_eq!(
            arith(FloatOp::Mul, false, 0x4040_0000, 0x3eaa_aaab, ENV),
            (u64::from((3.0f32 * third).to_bits()), PE)
        );
    }

    #[test]
    fn nans_follow_the_x86_rules() {
        // The first NaN operand, made quiet.
        assert_eq!(d(FloatOp::Add, QNAN_A, SNAN_B), (QNAN_A, IE));
        // Also when the first is signalling and the second quiet, where
        // qemu wrongly applies the x87 rule and returns the second.
        assert_eq!(d(FloatOp::Add, SNAN_B, QNAN_A), (SNAN_B | QUIET, IE));
        assert_eq!(d(FloatOp::Mul, ONE, SNAN_B), (SNAN_B | QUIET, IE));
        assert_eq!(d(FloatOp::Mul, ONE, QNAN_A), (QNAN_A, 0));
        // Invalid operations give the negative "indefinite" NaN.
        assert_eq!(d(FloatOp::Sub, INF, INF), (INDEFINITE, IE));
        assert_eq!(d(FloatOp::Div, 0, 0), (INDEFINITE, IE));
        assert_eq!(d(FloatOp::Sqrt, 0, (-1.0f64).to_bits()), (INDEFINITE, IE));
        assert_eq!(
            arith(FloatOp::Mul, false, 0, 0x7f80_0000, ENV),
            (0xffc0_0000, IE)
        );
    }

    /// The flags an Intel Xeon X5675 raises for the same operands
    /// (docs/testing.md, hardware oracle).
    #[test]
    fn flags_follow_the_hardware() {
        assert_eq!(d(FloatOp::Div, ONE, 0).1, ZE);
        assert_eq!(d(FloatOp::Div, DENORMAL, 0).1, ZE);
        assert_eq!(d(FloatOp::Add, DENORMAL, QNAN_A).1, 0);
        assert_eq!(d(FloatOp::Add, DENORMAL, SNAN_B).1, IE);
        assert_eq!(d(FloatOp::Mul, DENORMAL, 0).1, DE);
        // sqrt(2^-1074) = 2^-537 is exact, sqrt(2^-1073) is not.
        assert_eq!(d(FloatOp::Sqrt, 0, DENORMAL).1, DE);
        assert_eq!(d(FloatOp::Sqrt, 0, 2).1, DE | PE);
        assert_eq!(d(FloatOp::Min, DENORMAL, ONE).1, DE);
        assert_eq!(d(FloatOp::Min, QNAN_A, ONE).1, IE);
        assert_eq!(d(FloatOp::Mul, f64::MAX.to_bits(), TWO).1, OE | PE);
        // Tininess after rounding: DBL_MIN·(1+2^-52) × (1−2^-52) rounds to
        // DBL_MIN, inexact but not tiny on x86.
        assert_eq!(
            d(FloatOp::Mul, 0x0010_0000_0000_0001, 0x3fef_ffff_ffff_fffe),
            (f64::MIN_POSITIVE.to_bits(), PE)
        );
        assert_eq!(
            d(FloatOp::Div, f64::MIN_POSITIVE.to_bits(), 3.0f64.to_bits()).1,
            UE | PE
        );
        assert_eq!(
            compare(true, DENORMAL, QNAN_A, true, ENV),
            ((true, true, true), IE)
        );
        assert_eq!(compare(true, DENORMAL, QNAN_A, false, ENV).1, 0);
        assert_eq!(compare(true, DENORMAL, ONE, false, ENV).1, DE);
        assert_eq!(to_int(true, DENORMAL, 64, false, ENV), (0, PE));
        assert_eq!(narrow(DENORMAL, ENV), (0, DE | UE | PE));
        assert_eq!(narrow(0x3800_0000_0000_0000, ENV), (0x0040_0000, 0));
        assert_eq!(widen(1, ENV).1, DE);
        for b in [DENORMAL, 0, (-1.0f64).to_bits(), SNAN_B] {
            assert_eq!(d(FloatOp::Rcp, 0, b).1, 0);
            assert_eq!(d(FloatOp::Rsqrt, 0, b).1, 0);
        }
    }

    #[test]
    fn min_and_max_return_the_second_operand_on_ties_and_nans() {
        let neg_zero = 0x8000_0000_0000_0000;
        assert_eq!(d(FloatOp::Min, 0, neg_zero).0, neg_zero);
        assert_eq!(d(FloatOp::Max, neg_zero, 0).0, 0);
        assert_eq!(d(FloatOp::Min, QNAN_A, ONE).0, ONE);
        assert_eq!(d(FloatOp::Max, ONE, QNAN_A).0, QNAN_A);
        assert_eq!(d(FloatOp::Min, ONE, TWO).0, ONE);
    }

    #[test]
    fn compare_sets_unordered_on_nan() {
        let c = |a, b| compare(true, a, b, false, ENV).0;
        assert_eq!(c(QNAN_A, ONE), (true, true, true));
        assert_eq!(c(ONE, TWO), (false, false, true));
        assert_eq!(c(TWO, ONE), (false, false, false));
        assert_eq!(c(0, 0x8000_0000_0000_0000), (true, false, false));
    }

    #[test]
    fn integer_conversions() {
        let i = |x: f64, bits, truncate| to_int(true, x.to_bits(), bits, truncate, ENV).0;
        // Rounding to nearest-even (cvtsd2si) and truncation (cvttsd2si).
        assert_eq!(i(2.5, 32, false), 2);
        assert_eq!(i(3.5, 32, false), 4);
        assert_eq!(i(3.5, 32, true), 3);
        assert_eq!(i(-2.5, 32, false), 0xffff_fffe);
        assert_eq!(i(-2.7, 32, true), 0xffff_fffe);
        assert_eq!(
            to_int(true, 3e9f64.to_bits(), 32, false, ENV),
            (0x8000_0000, IE)
        );
        assert_eq!(
            to_int(true, QNAN_A, 64, false, ENV),
            (0x8000_0000_0000_0000, IE)
        );
        assert_eq!(i(-2147483648.9, 32, false), 0x8000_0000);
        assert_eq!(from_int(-1, true, ENV), ((-1.0f64).to_bits(), 0));
        assert_eq!(
            from_int(i64::MAX, false, ENV),
            (u64::from(9.223372e18f32.to_bits()), PE)
        );
    }

    #[test]
    fn float_to_float_keeps_nan_payloads() {
        assert_eq!(widen(1.5f32.to_bits(), ENV), (1.5f64.to_bits(), 0));
        assert_eq!(widen(0xff80_0001, ENV), (0xfff8_0000_2000_0000, IE));
        assert_eq!(narrow(0xfff8_0000_2000_0000, ENV), (0xffc0_0001, 0));
        assert_eq!(
            narrow(1e300f64.to_bits(), ENV),
            (f32::INFINITY.to_bits(), OE | PE)
        );
    }

    /// Rounding modes, FZ and DAZ (ADR-0024).
    #[test]
    fn rounding_modes_flush_and_denormals_are_zero() {
        let third = |rc| {
            let env = Env { rc, ..ENV };
            arith(FloatOp::Div, true, ONE, 3.0f64.to_bits(), env).0
        };
        let nearest = third(Rounding::Nearest);
        assert_eq!(third(Rounding::Down), nearest);
        assert_eq!(third(Rounding::Zero), nearest);
        assert_eq!(third(Rounding::Up), nearest + 1);
        let ftz = Env { ftz: true, ..ENV };
        assert_eq!(
            arith(FloatOp::Div, true, f64::MIN_POSITIVE.to_bits(), TWO, ftz),
            (0, UE | PE)
        );
        let daz = Env { daz: true, ..ENV };
        assert_eq!(arith(FloatOp::Add, true, DENORMAL, ONE, daz), (ONE, 0));
    }
}
