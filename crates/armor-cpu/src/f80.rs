//! x87 80-bit extended-precision arithmetic in software (ADR-0016).
//!
//! An `F80` is the raw register format: sign, 15-bit biased exponent and a
//! 64-bit significand with an explicit integer bit. Every operation is
//! computed exactly on integers and rounded once by `round`, which is
//! parametrized by the precision (64 bits for registers, 53 or 24 for
//! stores to double or single) and the exponent range of the destination.
//!
//! x87 rules implemented here (Intel SDM vol. 1, chapter 8; verified by the
//! `c/x87` differential test against qemu, docs/tech/floating-point.md):
//! - non-canonical encodings (unnormals, pseudo-infinities, pseudo-NaNs)
//!   are invalid operands; pseudo-denormals are accepted as denormals;
//! - NaN propagation: a QNaN wins over an SNaN, otherwise the larger
//!   significand; the result is always quiet;
//! - invalid operations return the negative "indefinite" QNaN;
//! - status flags as in the status word: IE, DE, ZE, OE, UE, PE; the
//!   underflow flag (with the exception masked) needs a tiny and inexact
//!   result, tininess being detected before rounding.

use core::cmp::Ordering;

mod transcendental;
pub use transcendental::{Trig, atan2, cos, exp2m1, sin, tan, ylog2x, ylog2xp1};

pub const FLAG_INVALID: u16 = 0x01;
pub const FLAG_DENORMAL: u16 = 0x02;
pub const FLAG_ZERO_DIVIDE: u16 = 0x04;
pub const FLAG_OVERFLOW: u16 = 0x08;
pub const FLAG_UNDERFLOW: u16 = 0x10;
pub const FLAG_INEXACT: u16 = 0x20;

const BIAS: i32 = 16383;
const EXP_MAX: u16 = 0x7fff;
const INTEGER_BIT: u64 = 1 << 63;
const QUIET_BIT: u64 = 1 << 62;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct F80 {
    pub sign: bool,
    /// Biased exponent, 15 bits.
    pub exp: u16,
    pub mant: u64,
}

/// Rounding control (RC field of the control word).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounding {
    Nearest,
    Down,
    Up,
    Zero,
}

impl Rounding {
    pub fn from_control_word(cw: u16) -> Rounding {
        match (cw >> 10) & 3 {
            0 => Rounding::Nearest,
            1 => Rounding::Down,
            2 => Rounding::Up,
            _ => Rounding::Zero,
        }
    }
}

/// Rounding control plus precision control (PC field: 24, 53 or 64
/// significand bits). Only the arithmetic operations (add, sub, mul, div,
/// sqrt) round to the precision; the exponent range stays the 15-bit one.
/// `target` says where the result goes: an x87 register, or an SSE single
/// or double (ADR-0024), whose exponent range and rules apply instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub rc: Rounding,
    pub precision: u32,
    pub target: Target,
}

/// The destination of an arithmetic result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// An x87 register: the 15-bit exponent range, `Mode::precision`
    /// significand bits, tininess detected before rounding (as qemu does,
    /// `c/x87`).
    Register,
    /// An SSE single or double: its precision and exponent range,
    /// tininess detected after rounding (✅ Xeon X5675, ADR-0024), and with
    /// `ftz` (MXCSR.FZ) a tiny result becomes a zero with UE and PE. The
    /// result is still an `F80`, holding the rounded value exactly.
    Float { double: bool, ftz: bool },
}

impl Mode {
    /// An x87 register at `precision` bits.
    pub fn register(rc: Rounding, precision: u32) -> Mode {
        Mode {
            rc,
            precision,
            target: Target::Register,
        }
    }

    /// An SSE single or double result.
    pub fn float(double: bool, rc: Rounding, ftz: bool) -> Mode {
        Mode {
            rc,
            precision: if double { 53 } else { 24 },
            target: Target::Float { double, ftz },
        }
    }
}

impl From<Rounding> for Mode {
    /// 64-bit precision, the Linux default.
    fn from(rc: Rounding) -> Mode {
        Mode::register(rc, 64)
    }
}

/// A result and the status flags it raised. `rounded_up` is the C1
/// condition code: the magnitude was increased by rounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Out<T> {
    pub value: T,
    pub flags: u16,
    pub rounded_up: bool,
}

fn out<T>(value: T, flags: u16) -> Out<T> {
    Out {
        value,
        flags,
        rounded_up: false,
    }
}

impl F80 {
    pub const ZERO: F80 = F80 {
        sign: false,
        exp: 0,
        mant: 0,
    };
    pub const ONE: F80 = F80 {
        sign: false,
        exp: 0x3fff,
        mant: INTEGER_BIT,
    };
    /// The "real indefinite" QNaN.
    pub const INDEFINITE: F80 = F80 {
        sign: true,
        exp: EXP_MAX,
        mant: 0xc000_0000_0000_0000,
    };

    pub fn from_bytes(bytes: [u8; 10]) -> F80 {
        let mant = u64::from_le_bytes(bytes[..8].try_into().expect("8 bytes"));
        let high = u16::from_le_bytes([bytes[8], bytes[9]]);
        F80 {
            sign: high & 0x8000 != 0,
            exp: high & 0x7fff,
            mant,
        }
    }

    pub fn to_bytes(self) -> [u8; 10] {
        let mut bytes = [0u8; 10];
        bytes[..8].copy_from_slice(&self.mant.to_le_bytes());
        let high = (u16::from(self.sign) << 15) | self.exp;
        bytes[8..].copy_from_slice(&high.to_le_bytes());
        bytes
    }

    pub fn negate(self) -> F80 {
        F80 {
            sign: !self.sign,
            ..self
        }
    }

    pub fn abs(self) -> F80 {
        F80 {
            sign: false,
            ..self
        }
    }

    fn class(self) -> Class {
        match (self.exp, self.mant) {
            (0, 0) => Class::Zero,
            (0, _) => Class::Denormal,
            (EXP_MAX, m) if m & INTEGER_BIT == 0 => Class::Invalid,
            (EXP_MAX, m) if m << 1 == 0 => Class::Infinity,
            (EXP_MAX, m) if m & QUIET_BIT != 0 => Class::QuietNan,
            (EXP_MAX, _) => Class::SignalingNan,
            (_, m) if m & INTEGER_BIT == 0 => Class::Invalid,
            _ => Class::Normal,
        }
    }

    fn quiet(self) -> F80 {
        F80 {
            mant: self.mant | QUIET_BIT,
            ..self
        }
    }

    fn infinity(sign: bool) -> F80 {
        F80 {
            sign,
            exp: EXP_MAX,
            mant: INTEGER_BIT,
        }
    }

    fn zero(sign: bool) -> F80 {
        F80 {
            sign,
            exp: 0,
            mant: 0,
        }
    }

    /// A finite nonzero value as (exponent of the integer bit, normalized
    /// significand with bit 63 set).
    fn unpack(self) -> (i32, u64) {
        if self.exp == 0 {
            // Denormals and pseudo-denormals have the exponent of 1.
            let shift = self.mant.leading_zeros();
            (1 - BIAS - shift as i32, self.mant << shift)
        } else {
            (i32::from(self.exp) - BIAS, self.mant)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Zero,
    Denormal,
    Normal,
    Infinity,
    QuietNan,
    SignalingNan,
    /// Unnormals, pseudo-infinities and pseudo-NaNs.
    Invalid,
}

/// A destination format: precision in bits (with the integer bit) and the
/// exponent range of its normal numbers.
#[derive(Debug, Clone, Copy)]
struct Format {
    precision: u32,
    emin: i32,
    emax: i32,
}

const EXTENDED: Format = Format {
    precision: 64,
    emin: 1 - BIAS,
    emax: BIAS,
};
const DOUBLE: Format = Format {
    precision: 53,
    emin: -1022,
    emax: 1023,
};
const SINGLE: Format = Format {
    precision: 24,
    emin: -126,
    emax: 127,
};
const HALF: Format = Format {
    precision: 11,
    emin: -14,
    emax: 15,
};

/// A rounded finite result: `mant` has `precision` bits, its top bit being
/// the integer bit (clear for denormals and zero), and `exp` is the
/// exponent of that integer bit; or an infinity.
enum Rounded {
    Finite { exp: i32, mant: u64 },
    Infinity,
}

/// Whether the exact value is tiny: below the smallest normal of `format`,
/// before rounding, or (`after_rounding`) once rounded to the format's
/// precision with an unbounded exponent. The two differ only for a value
/// just below the smallest normal that rounds up to it.
fn is_tiny(sign: bool, exp: i32, sig: u128, format: Format, rc: Rounding, after: bool) -> bool {
    if exp >= format.emin {
        return false;
    }
    if !after || exp != format.emin - 1 {
        return true;
    }
    let p = format.precision;
    let kept = sig >> (128 - p);
    let rest = sig << p;
    let half = 1u128 << 127;
    let increment = match rc {
        Rounding::Nearest => rest > half || (rest == half && kept & 1 != 0),
        Rounding::Zero => false,
        Rounding::Up => !sign && rest != 0,
        Rounding::Down => sign && rest != 0,
    };
    !(increment && kept + 1 == 1u128 << p)
}

/// Rounds `sig` (normalized: bit 127 set; value = sig * 2^(exp - 127)) to
/// `format`. Returns the result, flags and whether the magnitude grew.
/// Tininess, for the underflow flag, is detected before rounding or
/// (`after`) after it (`is_tiny`).
fn round(
    sign: bool,
    exp: i32,
    sig: u128,
    format: Format,
    rc: Rounding,
    after: bool,
) -> Out<Rounded> {
    let mut flags = 0;
    let tiny = is_tiny(sign, exp, sig, format, rc, after);
    let (mut exp, mut sig) = (exp, sig);
    if exp < format.emin {
        sig = shift_right_jam(sig, (format.emin - exp) as u32);
        exp = format.emin;
    }
    let p = format.precision;
    let mut kept = sig >> (128 - p);
    let rest = sig << p;
    let half = 1u128 << 127;
    let inexact = rest != 0;
    let increment = match rc {
        Rounding::Nearest => rest > half || (rest == half && kept & 1 != 0),
        Rounding::Zero => false,
        Rounding::Up => !sign && inexact,
        Rounding::Down => sign && inexact,
    };
    if increment {
        kept += 1;
        if kept == 1u128 << p {
            kept = 1 << (p - 1);
            exp += 1;
        }
    }
    if inexact {
        flags |= FLAG_INEXACT;
        if tiny {
            flags |= FLAG_UNDERFLOW;
        }
    }
    if exp > format.emax {
        flags |= FLAG_OVERFLOW | FLAG_INEXACT;
        let to_infinity = match rc {
            Rounding::Nearest => true,
            Rounding::Zero => false,
            Rounding::Up => !sign,
            Rounding::Down => sign,
        };
        let value = if to_infinity {
            Rounded::Infinity
        } else {
            Rounded::Finite {
                exp: format.emax,
                mant: mask(p),
            }
        };
        return Out {
            value,
            flags,
            rounded_up: to_infinity,
        };
    }
    Out {
        value: Rounded::Finite {
            exp,
            mant: kept as u64,
        },
        flags,
        rounded_up: increment,
    }
}

fn mask(bits: u32) -> u64 {
    if bits == 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    }
}

/// `value >> shift`, with any bit shifted out ORed into bit 0.
fn shift_right_jam(value: u128, shift: u32) -> u128 {
    if shift == 0 {
        value
    } else if shift >= 128 {
        u128::from(value != 0)
    } else {
        (value >> shift) | u128::from(value << (128 - shift) != 0)
    }
}

/// Rounds an exact nonzero result to the 80-bit register format.
/// `sig` may be any nonzero value; `exp` is the exponent of its bit 127.
/// Rounds to `mode.precision` bits (the result keeps the 64-bit register
/// layout, low bits zero).
fn pack(sign: bool, exp: i32, sig: u128, mode: Mode) -> Out<F80> {
    let shift = sig.leading_zeros();
    let (exp, sig) = (exp - shift as i32, sig << shift);
    if let Target::Float { double, ftz } = mode.target {
        return pack_float(sign, exp, sig, double, ftz, mode.rc);
    }
    let format = Format {
        precision: mode.precision,
        ..EXTENDED
    };
    let rounded = round(sign, exp, sig, format, mode.rc, false);
    let low_zeros = 64 - mode.precision;
    let value = match rounded.value {
        Rounded::Infinity => F80::infinity(sign),
        Rounded::Finite { exp, mant } => {
            let mant = mant << low_zeros;
            F80 {
                sign,
                exp: if mant & INTEGER_BIT != 0 {
                    (exp + BIAS) as u16
                } else {
                    0
                },
                mant,
            }
        }
    };
    Out {
        value,
        flags: rounded.flags,
        rounded_up: rounded.rounded_up,
    }
}

/// `pack` for an SSE single or double: rounded once to that format, the
/// value returned as an `F80` (which holds it exactly). With `ftz`, a tiny
/// result is a zero with UE and PE (SDM vol. 1, 10.2.3.3).
fn pack_float(sign: bool, exp: i32, sig: u128, double: bool, ftz: bool, rc: Rounding) -> Out<F80> {
    let format = if double { DOUBLE } else { SINGLE };
    if ftz && is_tiny(sign, exp, sig, format, rc, true) {
        return out(F80::zero(sign), FLAG_UNDERFLOW | FLAG_INEXACT);
    }
    let rounded = round(sign, exp, sig, format, rc, true);
    let value = match rounded.value {
        Rounded::Infinity => F80::infinity(sign),
        Rounded::Finite { mant: 0, .. } => F80::zero(sign),
        Rounded::Finite { exp, mant } => {
            // mant has `precision` bits, the top one weighing 2^exp.
            let m = mant << (64 - format.precision);
            let s = m.leading_zeros();
            F80 {
                sign,
                exp: (exp - s as i32 + BIAS) as u16,
                mant: m << s,
            }
        }
    };
    Out {
        value,
        flags: rounded.flags,
        rounded_up: rounded.rounded_up,
    }
}

/// The NaN result of a two-operand operation with at least one NaN
/// operand, or `None`. Raises IE for signalling NaNs.
fn propagate(a: F80, b: F80) -> Option<Out<F80>> {
    let (ca, cb) = (a.class(), b.class());
    let a_nan = matches!(ca, Class::QuietNan | Class::SignalingNan);
    let b_nan = matches!(cb, Class::QuietNan | Class::SignalingNan);
    if !a_nan && !b_nan {
        return None;
    }
    let flags = if ca == Class::SignalingNan || cb == Class::SignalingNan {
        FLAG_INVALID
    } else {
        0
    };
    let value = match (a_nan, b_nan) {
        (true, false) => a,
        (false, true) => b,
        _ if (ca == Class::SignalingNan) != (cb == Class::SignalingNan) => {
            if ca == Class::QuietNan { a } else { b }
        }
        _ => match (a.mant | QUIET_BIT).cmp(&(b.mant | QUIET_BIT)) {
            Ordering::Greater => a,
            Ordering::Less => b,
            Ordering::Equal => {
                if !a.sign && b.sign {
                    a
                } else {
                    b
                }
            }
        },
    };
    Some(out(value.quiet(), flags))
}

fn invalid() -> Out<F80> {
    out(F80::INDEFINITE, FLAG_INVALID)
}

/// The checks shared by the two-operand operations: invalid encodings,
/// NaNs and the denormal flag.
fn prepare(a: F80, b: F80) -> Result<u16, Out<F80>> {
    if a.class() == Class::Invalid || b.class() == Class::Invalid {
        return Err(invalid());
    }
    if let Some(nan) = propagate(a, b) {
        return Err(nan);
    }
    Ok(
        if a.class() == Class::Denormal || b.class() == Class::Denormal {
            FLAG_DENORMAL
        } else {
            0
        },
    )
}

fn with_flags(mut result: Out<F80>, flags: u16) -> Out<F80> {
    result.flags |= flags;
    result
}

pub fn add(a: F80, b: F80, mode: impl Into<Mode>) -> Out<F80> {
    add_signed(a, b, b.sign, mode.into())
}

pub fn sub(a: F80, b: F80, mode: impl Into<Mode>) -> Out<F80> {
    add_signed(a, b, !b.sign, mode.into())
}

/// a + (b with sign `b_sign`).
fn add_signed(a: F80, b: F80, b_sign: bool, mode: Mode) -> Out<F80> {
    let rc = mode.rc;
    let denormal = match prepare(a, b) {
        Ok(flags) => flags,
        Err(result) => return result,
    };
    let (ca, cb) = (a.class(), b.class());
    match (ca == Class::Infinity, cb == Class::Infinity) {
        (true, true) if a.sign != b_sign => return invalid(),
        (true, _) => return out(a, 0),
        (_, true) => return out(F80::infinity(b_sign), 0),
        _ => {}
    }
    let zero_sum_sign =
        |both_negative: bool| both_negative || (rc == Rounding::Down && a.sign != b_sign);
    match (ca == Class::Zero, cb == Class::Zero) {
        (true, true) => return out(F80::zero(zero_sum_sign(a.sign && b_sign)), denormal),
        (true, false) => {
            let b = F80 { sign: b_sign, ..b };
            return with_flags(normalize_register(b, mode), denormal);
        }
        (false, true) => return with_flags(normalize_register(a, mode), denormal),
        _ => {}
    }
    let (ea, ma) = a.unpack();
    let (eb, mb) = b.unpack();
    // Significands at bits 125..62, leaving two bits of headroom.
    let (big, small, e, big_sign, small_sign) = if (ea, ma) >= (eb, mb) {
        (ma, mb, ea, a.sign, b_sign)
    } else {
        (mb, ma, eb, b_sign, a.sign)
    };
    let shift = (e - if (ea, ma) >= (eb, mb) { eb } else { ea }) as u32;
    let x = u128::from(big) << 62;
    let y = shift_right_jam(u128::from(small) << 62, shift);
    let (sum, sign) = if big_sign == small_sign {
        (x + y, big_sign)
    } else {
        (x - y, big_sign)
    };
    if sum == 0 {
        return out(F80::zero(rc == Rounding::Down), denormal);
    }
    // Bit 125 of x has the exponent e, so bit 127 has e + 2.
    with_flags(pack(sign, e + 2, sum, mode), denormal)
}

/// A single finite operand as an arithmetic result (x + 0): denormals
/// keep their encoding when exact (pseudo-denormals are normalized), and a
/// reduced precision rounds the significand.
fn normalize_register(a: F80, mode: Mode) -> Out<F80> {
    if a.class() == Class::Denormal || mode.precision < 64 {
        let (e, m) = a.unpack();
        return pack(a.sign, e, u128::from(m) << 64, mode);
    }
    out(a, 0)
}

pub fn mul(a: F80, b: F80, mode: impl Into<Mode>) -> Out<F80> {
    let mode = mode.into();
    let denormal = match prepare(a, b) {
        Ok(flags) => flags,
        Err(result) => return result,
    };
    let sign = a.sign != b.sign;
    let (ca, cb) = (a.class(), b.class());
    let zero = ca == Class::Zero || cb == Class::Zero;
    let infinite = ca == Class::Infinity || cb == Class::Infinity;
    if zero && infinite {
        return invalid();
    }
    if infinite {
        return out(F80::infinity(sign), denormal);
    }
    if zero {
        return out(F80::zero(sign), denormal);
    }
    let (ea, ma) = a.unpack();
    let (eb, mb) = b.unpack();
    // The product of two 1.63 fixed-point numbers is 2.126: bit 127 has
    // the exponent ea + eb + 1.
    let product = u128::from(ma) * u128::from(mb);
    with_flags(pack(sign, ea + eb + 1, product, mode), denormal)
}

/// `a * b + c` rounded once (fused multiply-add, FMA3). The operands are
/// not NaN: the SSE layer chooses the NaN result first (`float::fma`).
pub fn fma(a: F80, b: F80, c: F80, mode: impl Into<Mode>) -> Out<F80> {
    let mode = mode.into();
    let (ca, cb, cc) = (a.class(), b.class(), c.class());
    let sign = a.sign != b.sign;
    let infinite = ca == Class::Infinity || cb == Class::Infinity;
    let zero = ca == Class::Zero || cb == Class::Zero;
    if infinite && zero {
        return invalid();
    }
    if infinite {
        if cc == Class::Infinity && c.sign != sign {
            return invalid();
        }
        return out(F80::infinity(sign), 0);
    }
    if cc == Class::Infinity {
        return out(c, 0);
    }
    // An exact zero sum: the common sign, or with opposite signs +0 (-0
    // when rounding down).
    let zero_sum = |signs_differ: bool, negative: bool| {
        let negative = if signs_differ {
            mode.rc == Rounding::Down
        } else {
            negative
        };
        out(F80::zero(negative), 0)
    };
    if zero {
        if cc == Class::Zero {
            return zero_sum(sign != c.sign, sign);
        }
        return normalize_register(c, mode);
    }
    let (ea, ma) = a.unpack();
    let (eb, mb) = b.unpack();
    // The exact product, two bits of headroom: bit 127 has the exponent
    // ea + eb + 3.
    let product = shift_right_jam(u128::from(ma) * u128::from(mb), 2);
    let ep = ea + eb + 3;
    if cc == Class::Zero {
        return pack(sign, ep, product, mode);
    }
    // The addend at bits 125..62: bit 127 has the exponent ec + 2.
    let (ec, mc) = c.unpack();
    let (addend, eq) = (u128::from(mc) << 62, ec + 2);
    let e = ep.max(eq);
    let x = shift_right_jam(product, (e - ep) as u32);
    let y = shift_right_jam(addend, (e - eq) as u32);
    let (sum, sum_sign) = if sign == c.sign {
        (x + y, sign)
    } else if x >= y {
        (x - y, sign)
    } else {
        (y - x, c.sign)
    };
    if sum == 0 {
        return zero_sum(true, false);
    }
    pack(sum_sign, e, sum, mode)
}

pub fn div(a: F80, b: F80, mode: impl Into<Mode>) -> Out<F80> {
    let mode = mode.into();
    let denormal = match prepare(a, b) {
        Ok(flags) => flags,
        Err(result) => return result,
    };
    let sign = a.sign != b.sign;
    let (ca, cb) = (a.class(), b.class());
    match (ca, cb) {
        (Class::Infinity, Class::Infinity) | (Class::Zero, Class::Zero) => return invalid(),
        (Class::Infinity, _) => return out(F80::infinity(sign), denormal),
        (_, Class::Infinity) | (Class::Zero, _) => return out(F80::zero(sign), denormal),
        (_, Class::Zero) => return out(F80::infinity(sign), FLAG_ZERO_DIVIDE | denormal),
        _ => {}
    }
    let (ea, ma) = a.unpack();
    let (eb, mb) = b.unpack();
    // 128 quotient bits of ma / mb, the first one weighing 2^0, plus a
    // sticky bit for the remainder.
    let (first, mut rem) = if ma >= mb { (1u128, ma - mb) } else { (0, ma) };
    let high = (u128::from(rem) << 64) / u128::from(mb);
    rem = ((u128::from(rem) << 64) % u128::from(mb)) as u64;
    let low = (u128::from(rem) << 63) / u128::from(mb);
    rem = ((u128::from(rem) << 63) % u128::from(mb)) as u64;
    let quotient = (first << 127) | (high << 63) | low | u128::from(rem != 0);
    with_flags(pack(sign, ea - eb, quotient, mode), denormal)
}

pub fn sqrt(a: F80, mode: impl Into<Mode>) -> Out<F80> {
    let mode = mode.into();
    match a.class() {
        Class::Invalid => return invalid(),
        Class::QuietNan => return out(a, 0),
        Class::SignalingNan => return out(a.quiet(), FLAG_INVALID),
        Class::Zero => return out(a, 0),
        _ if a.sign => return invalid(),
        Class::Infinity => return out(a, 0),
        _ => {}
    }
    let denormal = if a.class() == Class::Denormal {
        FLAG_DENORMAL
    } else {
        0
    };
    let (e, m) = a.unpack();
    // value = m * 2^(e - 63) = x * 2^(e - 63 - s) with x = m << s and
    // e - 63 - s even, so that sqrt(value) = sqrt(x) * 2^((e - 63 - s) / 2).
    let k = e - 63;
    let s = if k.rem_euclid(2) == 0 { 64 } else { 63 };
    let x = u128::from(m) << s;
    let root = x.isqrt();
    let rem = x - root * root;
    // The root has 64 bits; one more bit is set iff sqrt(x) >= root + 1/2,
    // that is rem > root (a tie is impossible), and any remainder is sticky.
    let sig = (root << 64) | (u128::from(rem > root) << 63) | u128::from(rem != 0);
    let exp = (k - s) / 2 + 63;
    with_flags(pack(false, exp, sig, mode), denormal)
}

/// `a` rounded to `mode`'s precision and target: the SSE conversion
/// between formats (cvtsd2ss). Zeros, infinities and NaNs are returned as
/// they are.
pub fn round_to(a: F80, mode: Mode) -> Out<F80> {
    match a.class() {
        Class::Normal | Class::Denormal => {
            let (e, m) = a.unpack();
            pack(a.sign, e, u128::from(m) << 64, mode)
        }
        _ => out(a, 0),
    }
}

/// x87 comparison result for the condition codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    Less,
    Equal,
    Greater,
    Unordered,
}

/// fcom (`quiet` false: any NaN raises IE) or fucom (only SNaNs do).
pub fn compare(a: F80, b: F80, quiet: bool) -> Out<Compare> {
    let (ca, cb) = (a.class(), b.class());
    if ca == Class::Invalid || cb == Class::Invalid {
        return out(Compare::Unordered, FLAG_INVALID);
    }
    let nan = |c| matches!(c, Class::QuietNan | Class::SignalingNan);
    if nan(ca) || nan(cb) {
        let signaling = ca == Class::SignalingNan || cb == Class::SignalingNan;
        let flags = if signaling || !quiet { FLAG_INVALID } else { 0 };
        return out(Compare::Unordered, flags);
    }
    let denormal = if ca == Class::Denormal || cb == Class::Denormal {
        FLAG_DENORMAL
    } else {
        0
    };
    let key = |x: F80| -> (i8, i32, u64) {
        match x.class() {
            Class::Zero => (0, 0, 0),
            Class::Infinity => (1, i32::MAX, 0),
            _ => {
                let (e, m) = x.unpack();
                (1, e, m)
            }
        }
    };
    let (ka, kb) = (key(a), key(b));
    let magnitude = ka.cmp(&kb);
    let both_zero = ca == Class::Zero && cb == Class::Zero;
    let order = if both_zero {
        Ordering::Equal
    } else {
        match (a.sign, b.sign) {
            (false, false) => magnitude,
            (true, true) => magnitude.reverse(),
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
        }
    };
    let result = match order {
        Ordering::Less => Compare::Less,
        Ordering::Equal => Compare::Equal,
        Ordering::Greater => Compare::Greater,
    };
    out(result, denormal)
}

/// fist / fistp to a `bits`-bit integer, as raw bits of that width.
/// Out of range, NaN, infinity or invalid: IE and the integer indefinite.
pub fn to_int(a: F80, bits: u32, rc: Rounding) -> Out<u64> {
    let indefinite = 1u64 << (bits - 1);
    match a.class() {
        Class::Zero => return out(0, 0),
        Class::Normal | Class::Denormal => {}
        _ => return out(indefinite, FLAG_INVALID),
    }
    // No DE: FIST/FISTP list only #IS, #IA and #P (felixcloutier).
    let (e, m) = a.unpack();
    if e >= 64 {
        return out(indefinite, FLAG_INVALID);
    }
    // The integer part and the fraction bits, as a 64.64 fixed-point value.
    let fixed = shift_right_jam(u128::from(m) << 64, (63 - e).clamp(0, 127) as u32);
    let mut int = (fixed >> 64) as u64;
    let fraction = fixed as u64;
    let half = 1u64 << 63;
    let increment = match rc {
        Rounding::Nearest => fraction > half || (fraction == half && int & 1 != 0),
        Rounding::Zero => false,
        Rounding::Up => !a.sign && fraction != 0,
        Rounding::Down => a.sign && fraction != 0,
    };
    if increment {
        int = match int.checked_add(1) {
            Some(v) => v,
            None => return out(indefinite, FLAG_INVALID),
        };
    }
    let limit = if a.sign { indefinite } else { indefinite - 1 };
    if int > limit {
        return out(indefinite, FLAG_INVALID);
    }
    let value = if a.sign { int.wrapping_neg() } else { int } & mask(bits);
    let flags = if fraction != 0 { FLAG_INEXACT } else { 0 };
    Out {
        value,
        flags,
        rounded_up: increment,
    }
}

/// fild: exact.
pub fn from_int(value: i64) -> F80 {
    if value == 0 {
        return F80::ZERO;
    }
    let magnitude = value.unsigned_abs();
    let shift = magnitude.leading_zeros();
    F80 {
        sign: value < 0,
        exp: (63 - shift as i32 + BIAS) as u16,
        mant: magnitude << shift,
    }
}

/// fld m64 (`double`) or m32: exact; an SNaN raises IE and becomes quiet,
/// a denormal raises DE.
pub fn from_float(bits: u64, double: bool) -> Out<F80> {
    let (frac_bits, exp_bits) = if double { (52, 11) } else { (23, 8) };
    let sign = (bits >> (frac_bits + exp_bits)) & 1 != 0;
    let exp = ((bits >> frac_bits) & ((1 << exp_bits) - 1)) as i32;
    let frac = bits & ((1 << frac_bits) - 1);
    let exp_all_ones = (1 << exp_bits) - 1;
    let bias = (1 << (exp_bits - 1)) - 1;
    if exp == exp_all_ones {
        let mant = INTEGER_BIT | (frac << (63 - frac_bits));
        let value = F80 {
            sign,
            exp: EXP_MAX,
            mant,
        };
        return if frac != 0 && mant & QUIET_BIT == 0 {
            out(value.quiet(), FLAG_INVALID)
        } else {
            out(value, 0)
        };
    }
    if exp == 0 {
        if frac == 0 {
            return out(F80::zero(sign), 0);
        }
        let shift = frac.leading_zeros();
        let e = 1 - bias - (shift as i32 - (63 - frac_bits));
        return out(
            F80 {
                sign,
                exp: (e + BIAS) as u16,
                mant: frac << shift,
            },
            FLAG_DENORMAL,
        );
    }
    out(
        F80 {
            sign,
            exp: (exp - bias + BIAS) as u16,
            mant: INTEGER_BIT | (frac << (63 - frac_bits)),
        },
        0,
    )
}

/// fst m64 (`double`) or m32: rounded to the destination format.
pub fn to_float(a: F80, double: bool, rc: Rounding) -> Out<u64> {
    if double {
        to_format(a, DOUBLE, 52, 11, rc, false)
    } else {
        to_format(a, SINGLE, 23, 8, rc, false)
    }
}

/// `a` as a half-precision value (vcvtps2ph, F16C), tininess detected
/// after rounding as for the SSE results.
pub fn to_half(a: F80, rc: Rounding) -> Out<u64> {
    to_format(a, HALF, 10, 5, rc, true)
}

/// `a` in an IEEE binary format of `frac_bits` and `exp_bits`; tininess
/// detected after rounding with `after`.
fn to_format(
    a: F80,
    format: Format,
    frac_bits: u32,
    exp_bits: u32,
    rc: Rounding,
    after: bool,
) -> Out<u64> {
    let sign_bit = u64::from(a.sign) << (frac_bits + exp_bits);
    let exp_all_ones = ((1u64 << exp_bits) - 1) << frac_bits;
    let quiet = 1u64 << (frac_bits - 1);
    match a.class() {
        Class::Zero => return out(sign_bit, 0),
        Class::Infinity => return out(sign_bit | exp_all_ones, 0),
        Class::Invalid => {
            return out(
                (1 << (frac_bits + exp_bits)) | exp_all_ones | quiet,
                FLAG_INVALID,
            );
        }
        Class::QuietNan | Class::SignalingNan => {
            let payload = (a.mant << 1) >> (64 - frac_bits);
            let flags = if a.class() == Class::SignalingNan {
                FLAG_INVALID
            } else {
                0
            };
            return out(sign_bit | exp_all_ones | payload | quiet, flags);
        }
        Class::Normal | Class::Denormal => {}
    }
    let (e, m) = a.unpack();
    // x87 stores detect tininess before rounding, like the registers.
    let rounded = round(a.sign, e, u128::from(m) << 64, format, rc, after);
    let bias = (1i32 << (exp_bits - 1)) - 1;
    let value = match rounded.value {
        Rounded::Infinity => sign_bit | exp_all_ones,
        Rounded::Finite { exp, mant } => {
            let integer = 1u64 << frac_bits;
            let biased = if mant & integer != 0 {
                (exp + bias) as u64
            } else {
                0
            };
            sign_bit | (biased << frac_bits) | (mant & (integer - 1))
        }
    };
    Out {
        value,
        flags: rounded.flags,
        rounded_up: rounded.rounded_up,
    }
}

/// The constants of fldpi, fldl2e, fldl2t, fldlg2 and fldln2: the
/// exponent of the integer bit and the first 128 bits of the significand
/// (truncated), computed with Python's decimal module at 120 digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constant {
    Pi,
    Log2E,
    Log2Ten,
    Log10Two,
    LnTwo,
}

/// The constant rounded to 64 bits by `rc` (the result of rounding the
/// exact value; the SDM's "66-bit internal value" rounds the same way
/// except, perhaps, for log10(2) to nearest, where the bits after the
/// 64th are 10 then nonzero). No flags: the constants raise none.
pub fn constant(c: Constant, rc: Rounding) -> F80 {
    let (exp, sig): (i32, u128) = match c {
        Constant::Pi => (1, 0xc90f_daa2_2168_c234_c4c6_628b_80dc_1cd1),
        Constant::Log2E => (0, 0xb8aa_3b29_5c17_f0bb_be87_fed0_691d_3e88),
        Constant::Log2Ten => (1, 0xd49a_784b_cd1b_8afe_492b_f6ff_4daf_db4c),
        Constant::Log10Two => (-2, 0x9a20_9a84_fbcf_f798_8f89_59ac_0b7c_9178),
        Constant::LnTwo => (-1, 0xb172_17f7_d1cf_79ab_c9e3_b398_03f2_f6af),
    };
    // The tail beyond 128 bits is nonzero: one sticky bit makes the
    // rounding of an exact tie impossible, as for the real value.
    pack(false, exp, sig | 1, Mode::from(rc)).value
}

/// The tag of a non-empty register in the full tag word of fnstenv and
/// fnsave: 0 valid, 1 zero, 2 special (NaN, infinity, denormal,
/// unsupported encoding).
pub fn tag(a: F80) -> u16 {
    match a.class() {
        Class::Normal => 0,
        Class::Zero => 1,
        _ => 2,
    }
}

/// fxam: (C3, C2, C0) of a non-empty register, by class: NaN 001,
/// normal 010, infinity 011, zero 100, denormal 110, unsupported
/// encodings 000. C1 (the sign) is the caller's.
pub fn examine(a: F80) -> (bool, bool, bool) {
    match a.class() {
        Class::QuietNan | Class::SignalingNan => (false, false, true),
        Class::Normal => (false, true, false),
        Class::Infinity => (false, true, true),
        Class::Zero => (true, false, false),
        Class::Denormal => (true, true, false),
        Class::Invalid => (false, false, false),
    }
}

/// The result of an operation on one operand that is not a finite
/// nonzero number: a NaN made quiet (IE if signalling), the indefinite for
/// unsupported encodings; `None` for finite values.
fn special_unary(a: F80) -> Option<Out<F80>> {
    match a.class() {
        Class::Invalid => Some(invalid()),
        Class::SignalingNan => Some(out(a.quiet(), FLAG_INVALID)),
        Class::QuietNan => Some(out(a, 0)),
        _ => None,
    }
}

/// frndint: `a` rounded to an integer by `rc`, at full precision. A zero
/// result keeps the sign of `a`.
pub fn round_to_integral(a: F80, rc: Rounding) -> Out<F80> {
    if let Some(special) = special_unary(a) {
        return special;
    }
    let denormal = if a.class() == Class::Denormal {
        FLAG_DENORMAL
    } else {
        0
    };
    match a.class() {
        Class::Zero | Class::Infinity => return out(a, 0),
        _ => {}
    }
    let (e, m) = a.unpack();
    if e >= 63 {
        return out(a, denormal);
    }
    // The integer part and the fraction, as a 64.64 fixed-point value.
    let fixed = shift_right_jam(u128::from(m) << 64, (63 - e).clamp(0, 127) as u32);
    let int = (fixed >> 64) as u64;
    let fraction = fixed as u64;
    let half = 1u64 << 63;
    let increment = match rc {
        Rounding::Nearest => fraction > half || (fraction == half && int & 1 != 0),
        Rounding::Zero => false,
        Rounding::Up => !a.sign && fraction != 0,
        Rounding::Down => a.sign && fraction != 0,
    };
    // At most 2^63: fits.
    let magnitude = int + u64::from(increment);
    let value = if magnitude == 0 {
        F80::zero(a.sign)
    } else {
        let mut v = from_int(magnitude as i64);
        if magnitude == 1 << 63 {
            v = F80 {
                sign: false,
                exp: (63 + BIAS) as u16,
                mant: INTEGER_BIT,
            };
        }
        F80 { sign: a.sign, ..v }
    };
    let flags = denormal | if fraction != 0 { FLAG_INEXACT } else { 0 };
    Out {
        value,
        flags,
        rounded_up: increment,
    }
}

/// fscale: `a * 2^trunc(b)`, rounded by `rc` at full precision (the
/// precision control does not apply).
pub fn scale(a: F80, b: F80, rc: Rounding) -> Out<F80> {
    let flags = match prepare(a, b) {
        Ok(flags) => flags,
        Err(result) => return result,
    };
    let (ca, cb) = (a.class(), b.class());
    if cb == Class::Infinity {
        return match (ca, b.sign) {
            (Class::Zero, false) | (Class::Infinity, true) => with_flags(invalid(), flags),
            (Class::Zero, true) | (Class::Infinity, false) => out(a, flags),
            (_, false) => out(F80::infinity(a.sign), flags),
            (_, true) => out(F80::zero(a.sign), flags),
        };
    }
    if matches!(ca, Class::Zero | Class::Infinity) {
        return out(a, flags);
    }
    // trunc(b), clamped far beyond any exponent a result can reach.
    let n: i64 = if b.class() == Class::Zero {
        0
    } else {
        let (e, m) = b.unpack();
        let magnitude = if e < 0 {
            0
        } else if e > 30 {
            1 << 31
        } else {
            (m >> (63 - e)) as i64
        };
        if b.sign { -magnitude } else { magnitude }
    };
    let (e, m) = a.unpack();
    let exp = (i64::from(e) + n).clamp(-(1 << 20), 1 << 20) as i32;
    with_flags(
        pack(a.sign, exp, u128::from(m) << 64, Mode::from(rc)),
        flags,
    )
}

/// fxtract: (exponent, significand) of `a`. A zero gives -infinity and
/// ZE; an infinity +infinity and itself; a NaN itself twice.
pub fn extract(a: F80) -> Out<(F80, F80)> {
    if let Some(special) = special_unary(a) {
        return out((special.value, special.value), special.flags);
    }
    match a.class() {
        Class::Zero => {
            return out((F80::infinity(true), a), FLAG_ZERO_DIVIDE);
        }
        Class::Infinity => return out((F80::infinity(false), a), 0),
        _ => {}
    }
    let flags = if a.class() == Class::Denormal {
        FLAG_DENORMAL
    } else {
        0
    };
    let (e, m) = a.unpack();
    let significand = F80 {
        sign: a.sign,
        exp: BIAS as u16,
        mant: m,
    };
    out((from_int(i64::from(e)), significand), flags)
}

/// The result of fprem / fprem1: the (partial) remainder, the low three
/// bits of the quotient and whether the reduction is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Remainder {
    pub value: F80,
    pub quotient: u8,
    pub complete: bool,
}

/// fprem (`ieee` false: the quotient truncated) or fprem1 (rounded to
/// nearest-even): `a - q * b`, exact. When the exponents differ by 64 or
/// more the reduction is partial (SDM: by an implementation-dependent
/// 32..63 bits); armor reduces the difference to `32 + (d - 64) % 32`
/// ⚠️ (docs/tech/floating-point.md).
pub fn remainder(a: F80, b: F80, ieee: bool) -> Out<Remainder> {
    let done = |value: F80, flags: u16| {
        out(
            Remainder {
                value,
                quotient: 0,
                complete: true,
            },
            flags,
        )
    };
    let flags = match prepare(a, b) {
        Ok(flags) => flags,
        Err(result) => return done(result.value, result.flags),
    };
    let (ca, cb) = (a.class(), b.class());
    if ca == Class::Infinity || cb == Class::Zero {
        return done(F80::INDEFINITE, flags | FLAG_INVALID);
    }
    if ca == Class::Zero || cb == Class::Infinity {
        return done(a, flags);
    }
    let (ea, ma) = a.unpack();
    let (mut eb, mb) = b.unpack();
    let mut d = ea - eb;
    let complete = d < 64;
    if !complete {
        let n = 32 + (d - 64) % 32;
        eb += d - n;
        d = n;
    }
    if d < -1 {
        return done(a, flags);
    }
    // In units of 2^(eb - 64): |a| = ma << (d + 1), |b| = mb << 1.
    let big_a = u128::from(ma) << (d + 1);
    let big_b = u128::from(mb) << 1;
    let mut q = big_a / big_b;
    let mut r = big_a % big_b;
    let mut sign = a.sign;
    if ieee && complete && (2 * r > big_b || (2 * r == big_b && q & 1 != 0)) {
        q += 1;
        r = big_b - r;
        sign = !sign;
    }
    let value = if r == 0 {
        F80::zero(a.sign)
    } else {
        // Exact: r has at most 65 bits and its unit is a representable
        // power of two (that of b's lowest bit, or half of it).
        pack(sign, eb - 64 + 127, r, Mode::from(Rounding::Nearest)).value
    };
    out(
        Remainder {
            value,
            quotient: (q & 7) as u8,
            complete,
        },
        flags,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(x: f64) -> F80 {
        from_float(x.to_bits(), true).value
    }

    fn d(x: F80) -> f64 {
        f64::from_bits(to_float(x, true, Rounding::Nearest).value)
    }

    /// fprem1 reports the quotient rounded to nearest: -2.5 rem 3 is 0.5
    /// with |Q| = 1, so Q0 is set (the SDM; qemu leaves the quotient bits
    /// clear, docs/testing.md).
    #[test]
    fn fprem1_reports_the_rounded_quotient() {
        let r = remainder(f(-2.5), f(3.0), true);
        assert_eq!(d(r.value.value), 0.5);
        assert_eq!(r.value.quotient, 1);
        assert!(r.value.complete);
        let r = remainder(f(-2.5), f(3.0), false);
        assert_eq!(d(r.value.value), -2.5);
        assert_eq!(r.value.quotient, 0);
    }

    /// Exponents 64 or more apart: a partial reduction, still exact.
    #[test]
    fn fprem_far_apart_is_partial() {
        let a = F80 {
            sign: false,
            exp: (BIAS + 100) as u16,
            mant: INTEGER_BIT | 0xabcdef,
        };
        let r = remainder(a, F80::ONE, false);
        assert!(!r.value.complete);
        let (e, _) = r.value.value.unpack();
        assert!((100 - 63..100).contains(&e), "exponent {e}");
    }

    /// fscale with two QNaNs: the x87 rule, larger significand (qemu
    /// returns ST(1)'s NaN, docs/testing.md).
    #[test]
    fn fscale_with_two_nans_follows_the_x87_rule() {
        let nan = |mant: u64| F80 {
            sign: false,
            exp: EXP_MAX,
            mant: INTEGER_BIT | QUIET_BIT | mant,
        };
        assert_eq!(scale(nan(5), nan(0), Rounding::Nearest).value, nan(5));
        assert_eq!(scale(nan(0), nan(5), Rounding::Nearest).value, nan(5));
    }

    #[test]
    fn bytes_round_trip() {
        let one = F80::ONE.to_bytes();
        assert_eq!(one, [0, 0, 0, 0, 0, 0, 0, 0x80, 0xff, 0x3f]);
        assert_eq!(F80::from_bytes(one), F80::ONE);
    }

    #[test]
    fn exact_arithmetic_matches_doubles() {
        let n = Rounding::Nearest;
        assert_eq!(d(add(f(1.5), f(2.25), n).value), 3.75);
        assert_eq!(d(sub(f(1.5), f(2.25), n).value), -0.75);
        assert_eq!(d(mul(f(1.5), f(-4.0), n).value), -6.0);
        assert_eq!(d(div(f(1.0), f(8.0), n).value), 0.125);
        assert_eq!(d(sqrt(f(2.25), n).value), 1.5);
        assert_eq!(from_int(-5), f(-5.0));
    }

    #[test]
    fn one_third_rounds_in_every_mode() {
        let third = |rc| div(F80::ONE, f(3.0), rc);
        let nearest = third(Rounding::Nearest);
        assert_eq!(nearest.value.mant, 0xaaaa_aaaa_aaaa_aaab);
        assert_eq!(nearest.flags, FLAG_INEXACT);
        assert!(nearest.rounded_up);
        assert_eq!(third(Rounding::Down).value.mant, 0xaaaa_aaaa_aaaa_aaaa);
        assert_eq!(third(Rounding::Zero).value.mant, 0xaaaa_aaaa_aaaa_aaaa);
        assert_eq!(third(Rounding::Up).value.mant, 0xaaaa_aaaa_aaaa_aaab);
    }

    #[test]
    fn special_cases() {
        let n = Rounding::Nearest;
        let inf = F80::infinity(false);
        assert_eq!(sub(inf, inf, n), invalid());
        assert_eq!(div(F80::ZERO, F80::ZERO, n), invalid());
        assert_eq!(div(F80::ONE, F80::ZERO, n).flags, FLAG_ZERO_DIVIDE);
        assert_eq!(sqrt(f(-1.0), n), invalid());
        assert_eq!(
            sub(F80::ONE, F80::ONE, Rounding::Down).value,
            F80::zero(true)
        );
        assert_eq!(sub(F80::ONE, F80::ONE, n).value, F80::zero(false));
        // An unnormal is an invalid operand.
        let unnormal = F80 {
            sign: false,
            exp: 0x3fff,
            mant: 1,
        };
        assert_eq!(add(unnormal, F80::ONE, n), invalid());
    }

    #[test]
    fn nans_follow_the_x87_rules() {
        let qnan = |m: u64| F80 {
            sign: false,
            exp: EXP_MAX,
            mant: 0xc000_0000_0000_0000 | m,
        };
        let snan = |m: u64| F80 {
            sign: false,
            exp: EXP_MAX,
            mant: 0x8000_0000_0000_0000 | m,
        };
        let n = Rounding::Nearest;
        // A QNaN wins over an SNaN, whatever the order.
        assert_eq!(add(snan(5), qnan(1), n).value, qnan(1));
        assert_eq!(add(snan(5), qnan(1), n).flags, FLAG_INVALID);
        // Otherwise the larger significand.
        assert_eq!(add(qnan(1), qnan(2), n).value, qnan(2));
        assert_eq!(mul(snan(3), snan(2), n).value, qnan(3));
    }

    #[test]
    fn integer_conversion_rounds_and_saturates_to_indefinite() {
        assert_eq!(to_int(f(2.5), 32, Rounding::Nearest).value, 2);
        assert_eq!(to_int(f(3.5), 32, Rounding::Nearest).value, 4);
        assert_eq!(to_int(f(-2.5), 32, Rounding::Zero).value, 0xffff_fffe);
        assert_eq!(to_int(f(-2.5), 16, Rounding::Down).value, 0xfffd);
        assert_eq!(to_int(f(40000.0), 16, Rounding::Nearest), invalid_int(16));
        assert_eq!(to_int(f(-32768.0), 16, Rounding::Nearest).value, 0x8000);
        assert_eq!(to_int(f(9.3e18), 64, Rounding::Nearest), invalid_int(64));
    }

    fn invalid_int(bits: u32) -> Out<u64> {
        out(1 << (bits - 1), FLAG_INVALID)
    }

    #[test]
    fn stores_to_double_and_single_round() {
        let third = div(F80::ONE, f(3.0), Rounding::Nearest).value;
        assert_eq!(d(third), 1.0 / 3.0);
        assert_eq!(
            to_float(third, false, Rounding::Nearest).value,
            u64::from((1.0f32 / 3.0).to_bits())
        );
        let huge = F80 {
            sign: false,
            exp: 0x7ffe,
            mant: INTEGER_BIT,
        };
        assert_eq!(d(huge), f64::INFINITY);
        assert_eq!(
            to_float(huge, true, Rounding::Zero).value,
            f64::MAX.to_bits()
        );
    }
}
