//! x86-64 instruction decoder: bytes to `Instruction`.
//!
//! Pure code, shared by the interpreter and the future JIT. Instructions
//! armor does not implement yet are reported as `Unsupported` rather than
//! guessed. Covered instructions and verification status:
//! docs/tech/x86-64-encoding.md.

#![forbid(unsafe_code)]
// No standard library: the Windows backend (ADR-0047) uses it too.
#![cfg_attr(not(test), no_std)]

mod decoder;
mod length;
mod maps;

pub use length::{Extension, Length, length, length_in};
mod sse;
mod vex;
mod x87;

use core::fmt;

/// Architectural limit: longer instructions raise #GP.
pub const MAX_INSTRUCTION_LEN: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    B8,
    B16,
    B32,
    B64,
}

impl Size {
    pub fn bits(self) -> u32 {
        match self {
            Size::B8 => 8,
            Size::B16 => 16,
            Size::B32 => 32,
            Size::B64 => 64,
        }
    }

    pub fn bytes(self) -> usize {
        self.bits() as usize / 8
    }

    pub fn mask(self) -> u64 {
        u64::MAX >> (64 - self.bits())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg {
    /// General-purpose register 0–15 (rax, rcx, rdx, rbx, rsp, rbp, rsi,
    /// rdi, r8–r15), accessed at the instruction's operand size.
    Gpr(u8),
    /// AH, CH, DH, BH: bits 8–15 of register 0–3. Only encodable without REX.
    HighByte(u8),
    /// xmm0–xmm15.
    Xmm(u8),
    /// mm0–mm7 (MMX): the significands of the x87 registers R0–R7. The
    /// executor also uses 8 and 9 internally, as 128-bit scratch registers
    /// for one instruction.
    Mm(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    /// No override: CS/DS/ES/SS have base 0 in 64-bit mode.
    Default,
    Fs,
    Gs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    None,
    Gpr(u8),
    /// Relative to the address of the *next* instruction.
    Rip,
}

/// Memory operand: `segment:[base + index * scale + disp]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mem {
    pub segment: Segment,
    pub base: Base,
    pub index: Option<u8>,
    pub scale: u8,
    pub disp: i64,
    /// Address-size prefix (67): the address is truncated to 32 bits.
    pub addr32: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    Reg(Reg),
    Mem(Mem),
    /// Immediate, already sign-extended to 64 bits as the encoding requires;
    /// the executor truncates it to the operand size.
    Imm(i64),
}

impl Operand {
    pub fn is_mem(&self) -> bool {
        matches!(self, Operand::Mem(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluOp {
    Add,
    Or,
    Adc,
    Sbb,
    And,
    Sub,
    Xor,
    Cmp,
}

impl AluOp {
    /// Order of the opcode rows 00–3F and of the `/digit` of opcodes 80–83.
    fn from_index(index: u8) -> AluOp {
        [
            AluOp::Add,
            AluOp::Or,
            AluOp::Adc,
            AluOp::Sbb,
            AluOp::And,
            AluOp::Sub,
            AluOp::Xor,
            AluOp::Cmp,
        ][usize::from(index & 7)]
    }
}

/// Condition codes in encoding order (low nibble of `70+cc`, `0F 80+cc`,
/// `0F 40+cc`, `0F 90+cc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    O,
    No,
    B,
    Ae,
    E,
    Ne,
    Be,
    A,
    S,
    Ns,
    P,
    Np,
    L,
    Ge,
    Le,
    G,
}

impl Cond {
    fn from_index(index: u8) -> Cond {
        use Cond::*;
        [O, No, B, Ae, E, Ne, Be, A, S, Ns, P, Np, L, Ge, Le, G][usize::from(index & 15)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Inc,
    Dec,
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftOp {
    Rol,
    Ror,
    Rcl,
    Rcr,
    Shl,
    Shr,
    Sar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftCount {
    One,
    Cl,
    Imm(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringOp {
    Movs,
    Cmps,
    Stos,
    Lods,
    Scas,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rep {
    /// F3: `rep`, or `repe` for cmps/scas.
    Rep,
    /// F2: `repne` (only meaningful for cmps/scas).
    Repne,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitTestOp {
    Bt,
    Bts,
    Btr,
    Btc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagOp {
    Clc,
    Stc,
    Cmc,
    Cld,
    Std,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopKind {
    /// `loop`: decrement rcx, jump if rcx != 0.
    Loop,
    /// `loope`: decrement rcx, jump if rcx != 0 and ZF.
    Loope,
    /// `loopne`: decrement rcx, jump if rcx != 0 and not ZF.
    Loopne,
    /// `jrcxz`: jump if rcx == 0.
    Jrcxz,
}

/// Lane-wise SSE2 integer operations on xmm registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneOp {
    Add,
    Sub,
    CmpEq,
    /// Signed greater-than.
    CmpGt,
    /// Unsigned minimum / maximum (pminub, pmaxub).
    MinU,
    MaxU,
    /// Signed minimum / maximum (pminsw, pmaxsw).
    MinS,
    MaxS,
    /// Saturating addition and subtraction (padds*, paddus*, psubs*,
    /// psubus*).
    AddSat {
        signed: bool,
    },
    SubSat {
        signed: bool,
    },
    /// Unsigned rounded average (pavgb, pavgw).
    Avg,
    /// The low half of each product (pmullw).
    MulLow,
    /// The high half of each product (pmulhw signed, pmulhuw unsigned).
    MulHigh {
        signed: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicOp {
    And,
    /// `!dst & src`.
    AndNot,
    Or,
    Xor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftKind {
    Left,
    RightLogical,
    RightArithmetic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseOp {
    /// emms: every x87 register empty, ending MMX use.
    Emms,
    /// The conversions between SSE and MMX registers (`MmxConversion`).
    MmxConvert(MmxConversion),
    /// 128-bit move (movaps, movups, movdqa, movdqu, movapd, movupd, and the
    /// non-temporal stores). `aligned` memory operands must be 16-byte
    /// aligned or the CPU raises #GP.
    Mov128 {
        aligned: bool,
    },
    /// movd/movq from a GPR or memory into the low lanes of an xmm register,
    /// zeroing the rest; also `movq xmm, xmm/m64` (F3 0F 7E).
    MovToXmm {
        bytes: u8,
    },
    /// movd/movq from the low lanes of an xmm register to a GPR or memory;
    /// also `movq xmm/m64, xmm` (66 0F D6), which zeroes the upper half
    /// when the destination is a register.
    MovFromXmm {
        bytes: u8,
    },
    /// movlps/movlpd/movhps/movhpd: 64-bit load into, or store from, the
    /// low or high half of an xmm register.
    MovHalf {
        high: bool,
    },
    /// movhlps: high half of src into the low half of dst.
    MovHlps,
    /// movlhps: low half of src into the high half of dst.
    MovLhps,
    Lane {
        op: LaneOp,
        lane: Size,
    },
    Logic(LogicOp),
    /// Unsigned 32x32->64 multiply of the even dword lanes.
    Pmuludq,
    /// Byte sign mask into a 32-bit GPR.
    Pmovmskb,
    Pshufd(u8),
    /// pextrw r32, xmm, imm8: the selected word, zero-extended.
    Pextrw(u8),
    /// pinsrw xmm, r32/m16, imm8: replace the selected word.
    Pinsrw(u8),
    /// shufps (`double` false) / shufpd: the low half of the result from
    /// dst, the high half from src, lanes chosen by imm8.
    Shuffle {
        double: bool,
        order: u8,
    },
    /// pshuflw (`high` false) / pshufhw: shuffle the four words of one
    /// half, copy the other half.
    PshufWords {
        high: bool,
        order: u8,
    },
    /// cmpss / cmpsd: compare the low lanes with a predicate (0..7: eq, lt,
    /// le, unord, neq, nlt, nle, ord) and store an all-ones or zero mask.
    FloatCmpMask {
        double: bool,
        predicate: u8,
    },
    /// psll/psrl/psra by an immediate count, per lane.
    ShiftImm {
        kind: ShiftKind,
        lane: Size,
        count: u8,
    },
    /// pslldq/psrldq: whole-register byte shift.
    ByteShift {
        left: bool,
        count: u8,
    },
    /// packsswb, packssdw, packuswb: narrow the `from`-sized lanes of dst
    /// (low half of the result) and src (high half) with saturation.
    Pack {
        from: Size,
        signed: bool,
    },
    /// movss / movsd: the low 4 or 8 bytes. A load from memory zeroes the
    /// rest of the register; a register move keeps it.
    MovScalar {
        double: bool,
    },
    /// Scalar floating-point arithmetic on the low lane (ss / sd forms);
    /// the rest of dst is kept.
    FloatArith {
        op: FloatOp,
        double: bool,
    },
    /// comiss / comisd (`signaling`: IE for any NaN) / ucomiss / ucomisd
    /// (IE for signalling NaNs only): ZF, PF, CF from an unordered compare
    /// of the low lanes; OF, SF, AF cleared.
    FloatCompare {
        double: bool,
        signaling: bool,
    },
    /// cvtsi2ss / cvtsi2sd: a 32- or 64-bit integer (`int_bytes`) into the
    /// low lane.
    IntToFloat {
        double: bool,
        int_bytes: u8,
    },
    /// cvtss2si / cvtsd2si (rounding by MXCSR) and cvttss2si / cvttsd2si
    /// (truncation) into a 32- or 64-bit GPR.
    FloatToInt {
        double: bool,
        int_bytes: u8,
        truncate: bool,
    },
    /// cvtss2sd (`to_double`) / cvtsd2ss.
    FloatToFloat {
        to_double: bool,
    },
    /// movmskps / movmskpd: lane sign bits into a GPR.
    FloatSignMask {
        double: bool,
    },
    /// punpckl*/punpckh*: interleave the low or high halves.
    Unpack {
        high: bool,
        lane: Size,
    },
    /// Packed floating-point arithmetic on every lane (ps / pd forms).
    PackedArith {
        op: FloatOp,
        double: bool,
    },
    /// Integer SIMD of the 0F 38 and 0F 3A maps (SSSE3, SSE4): `dst = op
    /// (dst, src)`, src an xmm register or aligned m128.
    Int(IntOp),
    /// pcmpestri, pcmpestrm, pcmpistri, pcmpistrm (SSE4.2): string
    /// compares of dst (the first operand) and src (xmm or unaligned
    /// m128), lengths in eax/edx (`explicit`; rax/rdx if `wide`) or up to
    /// a null element; the index into ecx or (`mask`) the mask into xmm0.
    PcmpStr {
        explicit: bool,
        mask: bool,
        wide: bool,
        imm: u8,
    },
    /// ptest (SSE4.1): ZF if `dst & src` is 0, CF if `!dst & src` is 0;
    /// OF, SF, AF, PF cleared. Writes no register.
    Ptest,
    /// pmovsx / pmovzx (SSE4.1): the low lanes of src (an xmm register or
    /// memory of the size they take) extended to `to`-sized lanes.
    Pmovx {
        signed: bool,
        from: Size,
        to: Size,
    },
    /// pextrb/w/d/q, extractps (SSE4.1): lane `index` of src into a GPR
    /// (zero-extended) or memory (the lane's size).
    Extract {
        lane: Size,
        index: u8,
    },
    /// pinsrb/d/q (SSE4.1): the low `lane` bits of a GPR or memory into
    /// lane `index` of dst.
    Insert {
        lane: Size,
        index: u8,
    },
    /// insertps (SSE4.1): a single from src (lane `imm >> 6` of a
    /// register, or m32) into lane `(imm >> 4) & 3` of dst, then the
    /// lanes in `imm & 15` zeroed.
    Insertps(u8),
    /// roundps/pd/ss/sd (SSE4.1): to an integral value; `imm` bits 0–1 the
    /// rounding, bit 2 MXCSR's instead, bit 3 no precision exception.
    Round {
        double: bool,
        scalar: bool,
        imm: u8,
    },
    /// dpps / dppd (SSE4.1): products of the lanes in `imm >> 4`, summed,
    /// into the lanes in `imm & 15` (zero elsewhere).
    Dp {
        double: bool,
        imm: u8,
    },
    /// addsubps / addsubpd (SSE3): even lanes `dst - src`, odd lanes
    /// `dst + src`.
    AddSub {
        double: bool,
    },
    /// haddps / haddpd, hsubps / hsubpd (SSE3): sums (`sub`: differences)
    /// of adjacent lanes, those of dst in the low half of the result,
    /// those of src in the high half.
    Horizontal {
        sub: bool,
        double: bool,
    },
    /// movddup, movshdup, movsldup (SSE3).
    MovDup(Dup),
    /// cmpps / cmppd: `FloatCmpMask` on every lane.
    PackedCmpMask {
        double: bool,
        predicate: u8,
    },
    /// Packed conversions between single, double and 32-bit integer lanes.
    PackedConvert(Conversion),
    /// psll/psrl/psra by the count in the low 64 bits of an xmm or m128.
    ShiftXmm {
        kind: ShiftKind,
        lane: Size,
    },
    /// pmaddwd: signed 16x16 products, adjacent pairs summed into dwords.
    Pmaddwd,
    /// psadbw: the sum of the absolute byte differences of each half.
    Psadbw,
    /// maskmovdqu: stores the bytes of dst whose byte in src has its top
    /// bit set, at [rdi] (unaligned).
    Maskmovdqu,
}

/// `SseOp::Int` operations (lanes are signed where it matters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntOp {
    /// pshufb: bytes of dst chosen by the low 4 bits of src's bytes, 0
    /// where bit 7 is set.
    Pshufb,
    /// phaddw/d, phaddsw (`saturate`), phsubw/d, phsubsw: adjacent lanes
    /// added (`sub`: subtracted), dst's in the low half, src's in the
    /// high half.
    Horizontal {
        sub: bool,
        saturate: bool,
        lane: Size,
    },
    /// pmaddubsw: unsigned bytes of dst times signed bytes of src, pairs
    /// added into words with signed saturation.
    Pmaddubsw,
    /// psignb/w/d: dst negated, zeroed or kept by the sign of src.
    Psign(Size),
    /// pmulhrsw: rounded high halves of the 16-bit products, scaled.
    Pmulhrsw,
    /// pabsb/w/d: dst = |src|.
    Pabs(Size),
    /// palignr: bytes imm..imm+16 of dst:src.
    Palignr(u8),
    /// pblendw, blendps, blendpd (SSE4.1): lane i from src if bit i of
    /// `imm` is set.
    Blend { lane: Size, imm: u8 },
    /// pblendvb, blendvps, blendvpd (SSE4.1): lane from src where the
    /// lane's top bit in xmm0 is set.
    BlendV(Size),
    /// pmuldq (SSE4.1): signed products of the even 32-bit lanes.
    Pmuldq,
    /// pmulld (SSE4.1): low halves of the 32-bit products.
    Pmulld,
    /// pcmpeqq (SSE4.1) / pcmpgtq (SSE4.2): 64-bit compares.
    Pcmpq { greater: bool },
    /// packusdw (SSE4.1): signed doublewords of dst, then src, to words
    /// with unsigned saturation.
    Packusdw,
    /// pminsb/sd, pminuw/ud, pmaxsb/sd, pmaxuw/ud (SSE4.1).
    MinMax { max: bool, signed: bool, lane: Size },
    /// phminposuw (SSE4.1): the smallest unsigned word of src and its
    /// index, the rest zeroed.
    Phminposuw,
    /// mpsadbw (SSE4.1): sums of absolute byte differences, blocks chosen
    /// by `imm`.
    Mpsadbw(u8),
}

/// What `SseOp::MovDup` duplicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dup {
    /// movddup: the low 64 bits (src is an xmm register or m64) into both
    /// halves.
    Low64,
    /// movshdup: the odd 32-bit lanes into the even ones too.
    OddSingles,
    /// movsldup: the even 32-bit lanes into the odd ones too.
    EvenSingles,
}

/// Conversions between MMX and SSE registers (0F 2A, 2C, 2D).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmxConversion {
    /// cvtpi2ps xmm, mm/m64: two int32 to the low singles, the high half
    /// kept.
    Pi2Ps,
    /// cvtps2pi / cvttps2pi mm, xmm/m64: the two low singles to int32.
    Ps2Pi { truncate: bool },
    /// cvtpi2pd xmm, mm/m64: two int32 to doubles.
    Pi2Pd,
    /// cvtpd2pi / cvttpd2pi mm, xmm/m128: two doubles to int32.
    Pd2Pi { truncate: bool },
}

/// Packed conversions (0F 5A, 5B, E6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conversion {
    /// cvtps2pd: the two low singles to doubles (m64 source).
    Ps2Pd,
    /// cvtpd2ps: two doubles to the low singles, the high half zeroed.
    Pd2Ps,
    /// cvtdq2ps: four int32 to singles.
    Dq2Ps,
    /// cvtps2dq (rounding by MXCSR) / cvttps2dq (truncation).
    Ps2Dq { truncate: bool },
    /// cvtdq2pd: the two low int32 to doubles (m64 source).
    Dq2Pd,
    /// cvtpd2dq / cvttpd2dq: two doubles to the low int32, the high half
    /// zeroed.
    Pd2Dq { truncate: bool },
}

/// Floating-point operations of the 0F 51..5F row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Sqrt,
    /// rcpss / rcpps: reciprocal (single precision only).
    Rcp,
    /// rsqrtss / rsqrtps: reciprocal of the square root.
    Rsqrt,
}

/// A memory operand format of x87 loads and stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X87Format {
    F32,
    F64,
    F80,
    I16,
    I32,
    I64,
    /// Packed BCD (`fbld`, `fbstp`): 18 decimal digits, two per byte from
    /// the lowest, and the sign in bit 7 of the tenth byte.
    Bcd,
}

/// A source or destination of an x87 instruction: ST(i) or memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X87Operand {
    St(u8),
    Mem(Mem, X87Format),
}

/// Arithmetic of `dest = dest op other` (`Sub`, `Div`) or
/// `dest = other op dest` (`SubR`, `DivR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X87Arith {
    Add,
    Mul,
    Sub,
    SubR,
    Div,
    DivR,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X87Op {
    /// `st0 = st0 op src`, or with `to_st` `st(i) = st(i) op st0`, then
    /// pop if `pop`.
    Arith {
        op: X87Arith,
        src: X87Operand,
        to_st: bool,
        pop: bool,
    },
    /// fcom/fcomp/fcompp, fucom.., ficom..: st0 against `src`, setting
    /// C3 C2 C0; then `pops` pops.
    Compare {
        src: X87Operand,
        unordered: bool,
        pops: u8,
    },
    /// fcomi/fucomi(p): st0 against st(i), setting ZF PF CF.
    CompareFlags {
        index: u8,
        unordered: bool,
        pop: bool,
    },
    /// ftst: st0 against +0.0.
    Test,
    /// fld, fild: push `src`.
    Load(X87Operand),
    /// fld1, fldz.
    LoadOne,
    LoadZero,
    /// fst, fstp, fist, fistp.
    Store {
        dst: X87Operand,
        pop: bool,
    },
    Exchange(u8),
    ChangeSign,
    Abs,
    Sqrt,
    /// fcmovcc st0, st(i).
    ConditionalMove {
        cond: Cond,
        index: u8,
    },
    LoadControlWord(Mem),
    StoreControlWord(Mem),
    /// fnstsw to memory, or to AX (`None`).
    StoreStatusWord(Option<Mem>),
    Init,
    ClearExceptions,
    Free(u8),
    /// ffreep st(i) (DF C0+i): free, then pop. Documented by AMD, not by
    /// Intel; glibc's libm uses it.
    FreeAndPop(u8),
    /// fnop.
    Nop,
    /// fwait (exceptions are masked: nothing to wait for). A control
    /// instruction, unlike fnop: it leaves the last-instruction pointers.
    Wait,
    /// fldpi, fldl2e, fldl2t, fldlg2, fldln2.
    LoadConstant(X87Constant),
    /// fxam: classify st0 in C3 C2 C0, its sign in C1.
    Examine,
    /// frndint.
    RoundToInteger,
    /// fscale: st0 * 2^trunc(st1).
    Scale,
    /// fxtract: st0 becomes its exponent, then its significand is pushed.
    Extract,
    /// fprem (`ieee` false) / fprem1: partial remainder of st0 / st1.
    PartialRemainder {
        ieee: bool,
    },
    /// fincstp / fdecstp.
    IncrementTop,
    DecrementTop,
    /// fnstenv / fldenv: the 28-byte environment (32-bit format).
    StoreEnvironment(Mem),
    LoadEnvironment(Mem),
    /// fnsave / frstor: environment and the eight registers (108 bytes).
    Save(Mem),
    Restore(Mem),
    /// fisttp (SSE3): store as an integer, truncating, then pop.
    StoreTruncate(X87Operand),
    /// The transcendental instructions (ADR-0030).
    Transcendental(X87Transcendental),
}

/// The x87 transcendental instructions (D9 F0–FF).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X87Transcendental {
    /// f2xm1: st0 = 2^st0 - 1.
    Exp2m1,
    /// fyl2x: st1 = st1 * log2(st0), pop.
    Ylog2x,
    /// fyl2xp1: st1 = st1 * log2(st0 + 1), pop.
    Ylog2xp1,
    /// fptan: st0 = tan(st0), then push 1.
    Tan,
    /// fpatan: st1 = atan(st1 / st0) with the quadrant, pop.
    Atan,
    /// fsin, fcos: st0 = sin(st0), cos(st0).
    Sin,
    Cos,
    /// fsincos: st0 = sin(st0), then push cos.
    SinCos,
}

/// The constants of fldpi & co.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X87Constant {
    Pi,
    Log2E,
    Log2Ten,
    Log10Two,
    LnTwo,
}

/// The BMI1 and BMI2 operations of `Op::Bmi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmiOp {
    /// andn (BMI1): `dst = !vvvv & src`.
    Andn,
    /// bextr (BMI1): `dst` = the bits of `src` from bit `vvvv[7:0]`, at most
    /// `vvvv[15:8]` of them.
    Bextr,
    /// blsi (BMI1): `dst` (VEX.vvvv) = the lowest set bit of `src`.
    Blsi,
    /// blsmsk (BMI1): `dst` (VEX.vvvv) = the bits up to the lowest set bit of
    /// `src`.
    Blsmsk,
    /// blsr (BMI1): `dst` (VEX.vvvv) = `src` without its lowest set bit.
    Blsr,
    /// bzhi (BMI2): `dst = src` with the bits from `vvvv[7:0]` up cleared.
    Bzhi,
    /// mulx (BMI2): `dst:vvvv` = rdx (edx) times `src`, unsigned; the high
    /// half in `dst`, the low half in `vvvv`; no flags.
    Mulx,
    /// pdep (BMI2): the low bits of `vvvv` deposited at the bits set in
    /// `src`.
    Pdep,
    /// pext (BMI2): the bits of `vvvv` at the bits set in `src`, packed.
    Pext,
    /// rorx (BMI2): `dst = src` rotated right by the immediate; no flags.
    Rorx(u8),
    /// sarx, shlx, shrx (BMI2): `dst = src` shifted by `vvvv` (masked to the
    /// operand size); no flags.
    Sarx,
    Shlx,
    Shrx,
}

/// SSE/x87 state instructions of the 0F AE group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateOp {
    /// Saves the x87, MXCSR and xmm state to a 512-byte, 16-byte aligned area.
    Fxsave,
    Fxrstor,
    Ldmxcsr,
    Stmxcsr,
    /// clflush: no cache to flush here, but the byte must be readable.
    Clflush,
    /// xsave (0F AE /4) and xrstor (/5), standard form: the state
    /// components in edx:eax & XCR0 to or from a 64-byte aligned area
    /// (Intel SDM vol. 1, 13.7 and 13.8). REX.W (xsave64, xrstor64) only
    /// changes the format of the x87 instruction and data pointers,
    /// which armor stores as 0.
    Xsave,
    Xrstor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Mov {
        dst: Operand,
        src: Operand,
    },
    /// movzx, movsx, movsxd: the instruction size is the destination size.
    Extend {
        signed: bool,
        dst: Reg,
        src: Operand,
        src_size: Size,
    },
    Lea {
        dst: Reg,
        src: Mem,
    },
    Alu {
        op: AluOp,
        dst: Operand,
        src: Operand,
    },
    Test {
        a: Operand,
        b: Operand,
    },
    Unary {
        op: UnaryOp,
        dst: Operand,
    },
    Shift {
        op: ShiftOp,
        dst: Operand,
        count: ShiftCount,
    },
    /// shld/shrd: shift dst, filling the vacated bits from src.
    DoubleShift {
        left: bool,
        dst: Operand,
        src: Reg,
        count: ShiftCount,
    },
    /// One-operand mul/imul: rdx:rax (or the narrower pair) = rax * src.
    MulWide {
        signed: bool,
        src: Operand,
    },
    /// Two- and three-operand imul: dst = src * factor, truncated. Without
    /// a factor the product is `dst * src`.
    Imul {
        dst: Reg,
        src: Operand,
        factor: Option<i64>,
    },
    /// div/idiv: rdx:rax (or the narrower pair) / src.
    Div {
        signed: bool,
        src: Operand,
    },
    /// cbw, cwde, cdqe.
    SignExtendAccumulator,
    /// cwd, cdq, cqo.
    SignExtendIntoRdx,
    Cmov {
        cond: Cond,
        dst: Reg,
        src: Operand,
    },
    Setcc {
        cond: Cond,
        dst: Operand,
    },
    Xchg {
        a: Operand,
        b: Operand,
    },
    Cmpxchg {
        dst: Operand,
        src: Reg,
    },
    Xadd {
        dst: Operand,
        src: Reg,
    },
    /// bt/bts/btr/btc: CF = selected bit, then set/reset/complement it.
    /// With a memory operand and a register offset, the offset is signed
    /// and may select a bit outside the addressed word.
    BitTest {
        op: BitTestOp,
        dst: Operand,
        offset: Operand,
    },
    /// popcnt (F3 0F B8): the number of set bits; ZF if src is 0, the
    /// other status flags cleared.
    Popcnt {
        dst: Reg,
        src: Operand,
    },
    /// crc32 (SSE4.2): dst (32 bits, zero-extended in a 64-bit
    /// destination) updated with the CRC-32C of src's `src_size` bytes.
    Crc32 {
        dst: Reg,
        src: Operand,
        src_size: Size,
    },
    /// bsf (forward) / bsr (reverse). With F3 (`counts_zeros`) the
    /// encoding is tzcnt / lzcnt, which count the zeros when CPUID declares
    /// BMI1 (tzcnt) or LZCNT (lzcnt), and run as bsf / bsr on a CPU
    /// without them.
    BitScan {
        reverse: bool,
        dst: Reg,
        src: Operand,
        counts_zeros: bool,
    },
    /// movbe (0F 38 F0, F1): a load or store with the bytes reversed; one
    /// operand is memory.
    Movbe {
        dst: Operand,
        src: Operand,
    },
    /// BMI1 and BMI2 (VEX-encoded, general registers): `dst` the ModRM.reg
    /// register (VEX.vvvv for blsi, blsmsk, blsr), `src` the ModRM.rm
    /// operand, `vvvv` the register VEX.vvvv names, whose role depends on
    /// the operation (`BmiOp`).
    Bmi {
        op: BmiOp,
        dst: Reg,
        src: Operand,
        vvvv: Reg,
    },
    Bswap(Reg),
    Push(Operand),
    Pop(Operand),
    Pushf,
    /// popf: the flags a user-mode program may change (status flags, DF,
    /// TF, NT, AC, ID) from the stack.
    Popf,
    /// lahf: AH = SF ZF AF PF CF (and the fixed bit 1).
    Lahf,
    /// sahf: SF ZF AF PF CF = AH.
    Sahf,
    /// enter alloc, level: push rbp, copy `level % 32` frame pointers,
    /// rbp = the new frame, rsp -= alloc.
    Enter {
        alloc: u16,
        level: u8,
    },
    Leave,
    /// xlat: al = byte at segment:[rbx + al].
    Xlat(Segment),
    /// cmpxchg8b (size 32: edx:eax, ecx:ebx) and cmpxchg16b (size 64:
    /// rdx:rax, rcx:rbx, 16-byte aligned operand).
    CmpxchgWide(Mem),
    /// Relative branches: the target is the next instruction's address + `rel`.
    Jmp {
        rel: i64,
    },
    JmpIndirect(Operand),
    Jcc {
        cond: Cond,
        rel: i64,
    },
    Loop {
        kind: LoopKind,
        rel: i64,
    },
    Call {
        rel: i64,
    },
    CallIndirect(Operand),
    /// `ret` / `ret imm16`: `pop` extra bytes are released after the return
    /// address.
    Ret {
        pop: u16,
    },
    String {
        op: StringOp,
        rep: Option<Rep>,
    },
    Flag(FlagOp),
    Cpuid,
    /// xgetbv (0F 01 D0): the extended control register ecx (XCR0 for 0)
    /// in edx:eax.
    Xgetbv,
    /// xsetbv (0F 01 D1): privileged, #GP in user mode (with XSAVE
    /// declared; #UD without, as xgetbv).
    Xsetbv,
    /// rdtsc: the time-stamp counter in edx:eax.
    Rdtsc,
    /// An x87 FPU instruction (D8..DF, and fwait). `opcode` is the 11-bit
    /// FOP the FPU records for the last non-control instruction: the low
    /// three bits of the first opcode byte, then the ModRM byte.
    X87 {
        op: X87Op,
        opcode: u16,
    },
    /// fxsave, fxrstor, ldmxcsr, stmxcsr, xsave, xrstor (0F AE /0../5,
    /// memory only), clflush (/7).
    State {
        op: StateOp,
        mem: Mem,
    },
    Syscall,
    /// int 0x80 in compatibility mode: an i386 syscall (ADR-0029).
    Int80,
    /// mov sreg, r/m16 (compatibility mode): loads a segment register;
    /// for fs and gs, the base of the selected TLS entry.
    LoadSegment {
        segment: Segment,
        src: Operand,
    },
    /// mov r/m, fs or gs (compatibility mode): the selector the CPU holds,
    /// 16 bits in memory, zero-extended in a register (`size`).
    ReadSegment {
        segment: Segment,
        dst: Operand,
    },
    Nop,
    /// lfence, mfence, sfence: memory barriers (ADR-0023).
    Fence,
    /// Privileged in user mode, raises #GP: hlt, cli, sti, port I/O
    /// (in, out, ins, outs), and int n other than 3 and 0x80.
    Privileged,
    /// int3 (CC) and int 3 (CD 03): raises #BP, a trap.
    Int3,
    /// int1 (F1, "icebp"): raises #DB, a trap.
    Int1,
    Sse {
        op: SseOp,
        dst: Operand,
        src: Operand,
    },
    /// The VEX-encoded form of an SSE operation (AVX): `dst = op(src1,
    /// src)`, `src1` the xmm register VEX.vvvv names, or `None` when the
    /// instruction has no such operand (then `op` works on `dst` and `src`
    /// as its legacy form does). On 128 bits, the bits 255:128 of a
    /// destination register zeroed, or with `wide` on both halves of the
    /// ymm registers (VEX.L). `avx2`: a 256-bit integer form, which AVX2
    /// adds.
    Vex {
        op: SseOp,
        dst: Operand,
        src1: Option<u8>,
        src: Operand,
        wide: bool,
        avx2: bool,
    },
    /// vldmxcsr (`store` false) and vstmxcsr (VEX 0F AE /2, /3): ldmxcsr
    /// and stmxcsr, as AVX instructions.
    VexMxcsr {
        store: bool,
        mem: Mem,
    },
    /// vzeroupper (bits 255:128 of every ymm register zeroed) and
    /// vzeroall (`all`: the whole registers).
    Vzero {
        all: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction {
    pub op: Op,
    /// Operand size; for stack and branch instructions the stack width
    /// (64 bits, 32 in compatibility mode).
    pub size: Size,
    pub len: usize,
    /// LOCK prefix (only accepted where the architecture allows it).
    pub lock: bool,
    /// Addresses are 32 bits: the address-size prefix (67) in 64-bit
    /// mode, always in compatibility mode. String instructions, `loop`,
    /// `jecxz` and `xlat` then use esi, edi, ecx and ebx.
    pub addr32: bool,
}

/// The mode code runs in (ADR-0029).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// 64-bit mode.
    #[default]
    Long,
    /// Compatibility mode: 32-bit code (i386) in a 64-bit process.
    Compat32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The bytes end before the instruction does.
    Truncated,
    /// Longer than 15 bytes: the CPU raises #GP.
    TooLong,
    /// Not a valid instruction in 64-bit mode: the CPU raises #UD.
    Invalid,
    /// Valid x86-64 that armor does not implement yet.
    Unsupported(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "truncated instruction"),
            DecodeError::TooLong => write!(f, "instruction longer than 15 bytes"),
            DecodeError::Invalid => write!(f, "invalid instruction in 64-bit mode"),
            DecodeError::Unsupported(what) => write!(f, "not supported yet: {what}"),
        }
    }
}

impl core::error::Error for DecodeError {}

/// Decodes the instruction at the start of `bytes`.
pub fn decode(bytes: &[u8]) -> Result<Instruction, DecodeError> {
    decoder::decode(bytes, Mode::Long)
}

/// `decode` for code running in `mode`.
pub fn decode_in(bytes: &[u8], mode: Mode) -> Result<Instruction, DecodeError> {
    decoder::decode(bytes, mode)
}

#[cfg(test)]
mod tests;
