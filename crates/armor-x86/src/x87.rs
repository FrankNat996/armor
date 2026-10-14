//! x87 opcodes D8..DF (Intel SDM vol. 2, appendix A.4, "x87 FPU
//! instructions"). Every form has a ModRM byte and no immediate, so the
//! length is always right; the forms armor does not implement yet are
//! reported as unsupported with their name.

use crate::decoder::{Decoder, Rm};
use crate::{
    Cond, DecodeError, Op, Size, X87Arith, X87Constant, X87Format, X87Op, X87Operand,
    X87Transcendental,
};

/// Arithmetic of the /digit (0, 1, 4..7) of D8/DC/DA/DE memory forms and
/// of the D8 register forms, where st0 is the destination.
const ARITH: [Option<X87Arith>; 8] = [
    Some(X87Arith::Add),
    Some(X87Arith::Mul),
    None,
    None,
    Some(X87Arith::Sub),
    Some(X87Arith::SubR),
    Some(X87Arith::Div),
    Some(X87Arith::DivR),
];

/// DC and DE register forms write st(i): their sub and div encodings are
/// swapped with respect to D8 (`DC E0+i` is fsubr st(i), st0).
const ARITH_TO_ST: [Option<X87Arith>; 8] = [
    Some(X87Arith::Add),
    Some(X87Arith::Mul),
    None,
    None,
    Some(X87Arith::SubR),
    Some(X87Arith::Sub),
    Some(X87Arith::DivR),
    Some(X87Arith::Div),
];

pub(crate) fn decode(d: &mut Decoder, opcode: u8) -> Result<Op, DecodeError> {
    let modrm = d.peek_modrm()?;
    let (reg, rm) = d.modrm()?;
    let digit = reg & 7;
    let op = match rm {
        Rm::Mem(m) => {
            let op = memory_form(opcode, digit, m)?;
            let environment = matches!(
                op,
                X87Op::StoreEnvironment(_)
                    | X87Op::LoadEnvironment(_)
                    | X87Op::Save(_)
                    | X87Op::Restore(_)
            );
            if environment && d.operand_size() == Size::B16 {
                return Err(DecodeError::Unsupported("x87 16-bit environment format"));
            }
            op
        }
        Rm::Reg(n) => register_form(opcode, digit, n & 7)?,
    };
    Ok(Op::X87 {
        op,
        opcode: (u16::from(opcode & 7) << 8) | u16::from(modrm),
    })
}

fn unsupported(name: &'static str) -> Result<X87Op, DecodeError> {
    Err(DecodeError::Unsupported(name))
}

fn memory_form(opcode: u8, digit: u8, m: crate::Mem) -> Result<X87Op, DecodeError> {
    use X87Format::*;
    let mem = |format| X87Operand::Mem(m, format);
    // D8 m32fp, DC m64fp, DA m32int, DE m16int: arithmetic and compares.
    let arith_format = match opcode {
        0xd8 => Some(F32),
        0xdc => Some(F64),
        0xda => Some(I32),
        0xde => Some(I16),
        _ => None,
    };
    if let Some(format) = arith_format {
        return Ok(match ARITH[usize::from(digit)] {
            Some(op) => X87Op::Arith {
                op,
                src: mem(format),
                to_st: false,
                pop: false,
            },
            None => X87Op::Compare {
                src: mem(format),
                unordered: false,
                pops: u8::from(digit == 3),
            },
        });
    }
    let store = |format, pop| X87Op::Store {
        dst: mem(format),
        pop,
    };
    Ok(match (opcode, digit) {
        (0xd9, 0) => X87Op::Load(mem(F32)),
        (0xd9, 2) => store(F32, false),
        (0xd9, 3) => store(F32, true),
        (0xd9, 5) => X87Op::LoadControlWord(m),
        (0xd9, 7) => X87Op::StoreControlWord(m),
        (0xd9, 4) => X87Op::LoadEnvironment(m),
        (0xd9, 6) => X87Op::StoreEnvironment(m),
        (0xdb, 0) => X87Op::Load(mem(I32)),
        (0xdb, 2) => store(I32, false),
        (0xdb, 3) => store(I32, true),
        (0xdb, 5) => X87Op::Load(mem(F80)),
        (0xdb, 7) => store(F80, true),
        (0xdb, 1) => X87Op::StoreTruncate(mem(I32)),
        (0xdd, 1) => X87Op::StoreTruncate(mem(I64)),
        (0xdf, 1) => X87Op::StoreTruncate(mem(I16)),
        (0xdd, 0) => X87Op::Load(mem(F64)),
        (0xdd, 2) => store(F64, false),
        (0xdd, 3) => store(F64, true),
        (0xdd, 7) => X87Op::StoreStatusWord(Some(m)),
        (0xdd, 4) => X87Op::Restore(m),
        (0xdd, 6) => X87Op::Save(m),
        (0xdf, 0) => X87Op::Load(mem(I16)),
        (0xdf, 2) => store(I16, false),
        (0xdf, 3) => store(I16, true),
        (0xdf, 5) => X87Op::Load(mem(I64)),
        (0xdf, 7) => store(I64, true),
        (0xdf, 4 | 6) => return unsupported("x87 BCD (fbld, fbstp)"),
        _ => return Err(DecodeError::Invalid),
    })
}

fn register_form(opcode: u8, digit: u8, i: u8) -> Result<X87Op, DecodeError> {
    let st = X87Operand::St(i);
    let arith = |op, to_st, pop| X87Op::Arith {
        op,
        src: st,
        to_st,
        pop,
    };
    let fcmov = |cond| X87Op::ConditionalMove { cond, index: i };
    Ok(match (opcode, digit) {
        (0xd8, _) => match ARITH[usize::from(digit)] {
            Some(op) => arith(op, false, false),
            None => X87Op::Compare {
                src: st,
                unordered: false,
                pops: u8::from(digit == 3),
            },
        },
        (0xdc | 0xde, _) => match (ARITH_TO_ST[usize::from(digit)], opcode, digit, i) {
            (Some(op), _, _, _) => arith(op, true, opcode == 0xde),
            (None, 0xde, 3, 1) => X87Op::Compare {
                src: X87Operand::St(1),
                unordered: false,
                pops: 2,
            },
            _ => return unsupported("x87 reserved alias (fcom/fcomp in DC/DE)"),
        },
        (0xd9, 0) => X87Op::Load(st),
        (0xd9, 1) => X87Op::Exchange(i),
        (0xd9, 2) if i == 0 => X87Op::Nop,
        (0xd9, 4) => match i {
            0 => X87Op::ChangeSign,
            1 => X87Op::Abs,
            4 => X87Op::Test,
            5 => X87Op::Examine,
            _ => return unsupported("x87 reserved or undocumented register form"),
        },
        (0xd9, 5) => match i {
            0 => X87Op::LoadOne,
            1 => X87Op::LoadConstant(X87Constant::Log2Ten),
            2 => X87Op::LoadConstant(X87Constant::Log2E),
            3 => X87Op::LoadConstant(X87Constant::Pi),
            4 => X87Op::LoadConstant(X87Constant::Log10Two),
            5 => X87Op::LoadConstant(X87Constant::LnTwo),
            6 => X87Op::LoadZero,
            _ => return unsupported("x87 reserved or undocumented register form"),
        },
        (0xd9, 6) => match i {
            0 => X87Op::Transcendental(X87Transcendental::Exp2m1),
            1 => X87Op::Transcendental(X87Transcendental::Ylog2x),
            2 => X87Op::Transcendental(X87Transcendental::Tan),
            3 => X87Op::Transcendental(X87Transcendental::Atan),
            4 => X87Op::Extract,
            5 => X87Op::PartialRemainder { ieee: true },
            6 => X87Op::DecrementTop,
            _ => X87Op::IncrementTop,
        },
        (0xd9, 7) => match i {
            0 => X87Op::PartialRemainder { ieee: false },
            1 => X87Op::Transcendental(X87Transcendental::Ylog2xp1),
            2 => X87Op::Sqrt,
            3 => X87Op::Transcendental(X87Transcendental::SinCos),
            4 => X87Op::RoundToInteger,
            5 => X87Op::Scale,
            6 => X87Op::Transcendental(X87Transcendental::Sin),
            _ => X87Op::Transcendental(X87Transcendental::Cos),
        },
        (0xda, 0) => fcmov(Cond::B),
        (0xda, 1) => fcmov(Cond::E),
        (0xda, 2) => fcmov(Cond::Be),
        (0xda, 3) => fcmov(Cond::P),
        (0xda, 5) if i == 1 => X87Op::Compare {
            src: X87Operand::St(1),
            unordered: true,
            pops: 2,
        },
        (0xdb, 0) => fcmov(Cond::Ae),
        (0xdb, 1) => fcmov(Cond::Ne),
        (0xdb, 2) => fcmov(Cond::A),
        (0xdb, 3) => fcmov(Cond::Np),
        (0xdb, 4) => match i {
            2 => X87Op::ClearExceptions,
            3 => X87Op::Init,
            _ => return unsupported("x87 legacy control (feni, fdisi, fsetpm)"),
        },
        (0xdb, 5) => X87Op::CompareFlags {
            index: i,
            unordered: true,
            pop: false,
        },
        (0xdb, 6) => X87Op::CompareFlags {
            index: i,
            unordered: false,
            pop: false,
        },
        (0xdd, 0) => X87Op::Free(i),
        (0xdd, 2) => X87Op::Store {
            dst: st,
            pop: false,
        },
        (0xdd, 3) => X87Op::Store { dst: st, pop: true },
        (0xdd, 4) => X87Op::Compare {
            src: st,
            unordered: true,
            pops: 0,
        },
        (0xdd, 5) => X87Op::Compare {
            src: st,
            unordered: true,
            pops: 1,
        },
        (0xdf, 0) => X87Op::FreeAndPop(i),
        (0xdf, 4) if i == 0 => X87Op::StoreStatusWord(None),
        (0xdf, 5) => X87Op::CompareFlags {
            index: i,
            unordered: true,
            pop: true,
        },
        (0xdf, 6) => X87Op::CompareFlags {
            index: i,
            unordered: false,
            pop: true,
        },
        // Reserved forms (#UD on hardware) and undocumented aliases such
        // as fstp1 or ffreep: not implemented, rather than guessed.
        _ => return unsupported("x87 reserved or undocumented register form"),
    })
}
