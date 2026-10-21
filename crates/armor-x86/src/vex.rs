//! VEX-encoded instructions (Intel SDM vol. 2, 2.3), after the prefix
//! (`Decoder::vex_prefix`): BMI1 and BMI2 on general registers, and the
//! VEX forms of the SSE instructions (AVX), decoded by the legacy tables
//! of `sse.rs` and `maps.rs` with VEX.pp as the mandatory prefix, then
//! given their VEX.vvvv operand (docs/tech/avx.md).

use crate::decoder::{Decoder, Rm, Vex};
use crate::{BmiOp, DecodeError, IntOp, Op, Operand, Reg, Size, SseOp};

pub(crate) fn decode(d: &mut Decoder) -> Result<(Op, Size), DecodeError> {
    let vex = d.vex();
    let opcode = d.peek_modrm()?;
    match (vex.map, opcode) {
        (2, 0xf2 | 0xf3 | 0xf5..=0xf7) | (3, 0xf0) => {
            d.u8()?;
            bmi(d, opcode)
        }
        // vzeroupper (VEX.L 0) and vzeroall (VEX.L 1): no ModRM, VEX.vvvv
        // 1111.
        (1, 0x77) if vex.prefix == 0 => {
            d.u8()?;
            if vex.vvvv != 0 {
                return Err(DecodeError::Invalid);
            }
            Ok((Op::Vzero { all: vex.l }, Size::B64))
        }
        // vldmxcsr, vstmxcsr: memory, VEX.L 0, VEX.vvvv 1111.
        (1, 0xae) if vex.prefix == 0 => {
            d.u8()?;
            let (digit, rm) = d.modrm()?;
            match (digit & 7, rm) {
                (2 | 3, Rm::Mem(mem)) if !vex.l && vex.vvvv == 0 => Ok((
                    Op::VexMxcsr {
                        store: digit & 7 == 3,
                        mem,
                    },
                    Size::B32,
                )),
                _ => Err(DecodeError::Invalid),
            }
        }
        // crc32 and movbe have no VEX form.
        (2, 0xf0 | 0xf1) => Err(DecodeError::Invalid),
        (1, _) => {
            d.u8()?;
            sse(vex, opcode, crate::sse::decode(d, opcode)?)
        }
        (2, _) => sse(vex, opcode, crate::maps::decode_38(d)?),
        _ => sse(vex, opcode, crate::maps::decode_3a(d)?),
    }
}

/// How an SSE operation's VEX form uses VEX.vvvv.
enum Form {
    /// The first source; the destination is ModRM.reg (NDS).
    Source,
    /// None: VEX.vvvv must be 1111.
    Unused,
    /// The destination; ModRM.rm is the source (NDD: the shifts by an
    /// immediate).
    Destination,
}

/// The VEX form of a legacy decoding: `#UD` for the MMX forms and the
/// operations that have no VEX form.
fn sse(vex: Vex, opcode: u8, decoded: (Op, Size)) -> Result<(Op, Size), DecodeError> {
    let (Op::Sse { op, dst, src }, _) = decoded else {
        return Err(DecodeError::Invalid);
    };
    let mm = |o: &Operand| matches!(o, Operand::Reg(Reg::Mm(_)));
    if mm(&dst)
        || mm(&src)
        || matches!(
            op,
            SseOp::Emms | SseOp::MmxConvert(_) | SseOp::Int(IntOp::BlendV(_))
        )
    {
        return Err(DecodeError::Invalid);
    }
    let both_xmm = matches!(
        (dst, src),
        (Operand::Reg(Reg::Xmm(_)), Operand::Reg(Reg::Xmm(_)))
    );
    let form = match op {
        SseOp::MovScalar { .. } if both_xmm => Form::Source,
        SseOp::MovHalf { .. } if !dst.is_mem() => Form::Source,
        SseOp::PackedArith {
            op: crate::FloatOp::Sqrt | crate::FloatOp::Rcp | crate::FloatOp::Rsqrt,
            ..
        } => Form::Unused,
        SseOp::Round { scalar, .. } => {
            if scalar {
                Form::Source
            } else {
                Form::Unused
            }
        }
        SseOp::Int(IntOp::Pabs(_) | IntOp::Phminposuw) => Form::Unused,
        SseOp::ShiftImm { .. } | SseOp::ByteShift { .. } => Form::Destination,
        SseOp::MovHlps
        | SseOp::MovLhps
        | SseOp::Lane { .. }
        | SseOp::Logic(_)
        | SseOp::Pmuludq
        | SseOp::Pinsrw(_)
        | SseOp::Shuffle { .. }
        | SseOp::FloatCmpMask { .. }
        | SseOp::Pack { .. }
        | SseOp::FloatArith { .. }
        | SseOp::IntToFloat { .. }
        | SseOp::FloatToFloat { .. }
        | SseOp::Unpack { .. }
        | SseOp::PackedArith { .. }
        | SseOp::Int(_)
        | SseOp::Insert { .. }
        | SseOp::Insertps(_)
        | SseOp::Dp { .. }
        | SseOp::AddSub { .. }
        | SseOp::Horizontal { .. }
        | SseOp::PackedCmpMask { .. }
        | SseOp::ShiftXmm { .. }
        | SseOp::Pmaddwd
        | SseOp::Psadbw => Form::Source,
        _ => Form::Unused,
    };
    // The scalar operations ignore VEX.L (LIG).
    let scalar = matches!(
        op,
        SseOp::MovScalar { .. }
            | SseOp::FloatArith { .. }
            | SseOp::FloatCmpMask { .. }
            | SseOp::FloatCompare { .. }
            | SseOp::IntToFloat { .. }
            | SseOp::FloatToInt { .. }
            | SseOp::FloatToFloat { .. }
            | SseOp::Round { scalar: true, .. }
    );
    let wide = vex.l && !scalar;
    if wide && !has_256_bit_form(op) {
        return Err(DecodeError::Invalid);
    }
    let avx2 = wide && integer(vex, opcode);
    let op = match form {
        Form::Source => Op::Vex {
            op,
            dst,
            src1: Some(vex.vvvv),
            src,
            wide,
            avx2,
        },
        Form::Unused if vex.vvvv != 0 => return Err(DecodeError::Invalid),
        Form::Unused => Op::Vex {
            op,
            dst,
            src1: None,
            src,
            wide,
            avx2,
        },
        Form::Destination => {
            let Operand::Reg(Reg::Xmm(n)) = dst else {
                unreachable!("the shifts by an immediate have a register operand")
            };
            Op::Vex {
                op,
                dst: Operand::Reg(Reg::Xmm(vex.vvvv)),
                src1: Some(n),
                src,
                wide,
                avx2,
            }
        }
    };
    Ok((op, Size::B64))
}

/// Whether `op` has a VEX.256 form (AVX or AVX2); for the others VEX.L = 1
/// is #UD (the scalar ones ignore it, `sse`).
fn has_256_bit_form(op: SseOp) -> bool {
    match op {
        SseOp::Int(IntOp::Phminposuw) => false,
        SseOp::Dp { double, .. } => !double,
        SseOp::Mov128 { .. }
        | SseOp::Logic(_)
        | SseOp::PackedArith { .. }
        | SseOp::PackedCmpMask { .. }
        | SseOp::Shuffle { .. }
        | SseOp::Unpack { .. }
        | SseOp::AddSub { .. }
        | SseOp::Horizontal { .. }
        | SseOp::MovDup(_)
        | SseOp::Round { .. }
        | SseOp::PackedConvert(_)
        | SseOp::FloatSignMask { .. }
        | SseOp::Ptest
        | SseOp::Lane { .. }
        | SseOp::Pmuludq
        | SseOp::Pmovmskb
        | SseOp::Pshufd(_)
        | SseOp::PshufWords { .. }
        | SseOp::ShiftImm { .. }
        | SseOp::ByteShift { .. }
        | SseOp::Pack { .. }
        | SseOp::Int(_)
        | SseOp::ShiftXmm { .. }
        | SseOp::Pmaddwd
        | SseOp::Psadbw
        | SseOp::Pmovx { .. } => true,
        _ => false,
    }
}

/// Whether a VEX.256 instruction of map `vex.map`, `opcode`, works on
/// integers: an AVX2 instruction (the 256-bit moves, logic of the ps and
/// pd forms, float arithmetic, conversions, rounding, blends of singles
/// and doubles, `vdpps` and `vptest` are AVX).
fn integer(vex: Vex, opcode: u8) -> bool {
    match vex.map {
        1 => match vex.prefix {
            0x66 => matches!(
                opcode,
                0x60..=0x6d | 0x70..=0x76 | 0xd1..=0xd5 | 0xd7..=0xdf | 0xe0..=0xe5 | 0xe8..=0xef | 0xf1..=0xfe
            ),
            0xf2 | 0xf3 => opcode == 0x70,
            _ => false,
        },
        2 => opcode != 0x17,
        _ => !matches!(opcode, 0x08..=0x0d | 0x40 | 0x41),
    }
}

/// BMI1 (andn, bextr, blsi, blsmsk, blsr) and BMI2 (bzhi, mulx, pdep,
/// pext, rorx, sarx, shlx, shrx). 32-bit operands, 64-bit with VEX.W in
/// 64-bit mode; VEX.L must be 0.
fn bmi(d: &mut Decoder, opcode: u8) -> Result<(Op, Size), DecodeError> {
    let vex = d.vex();
    let size = if d.rex_w() { Size::B64 } else { Size::B32 };
    let (reg, rm) = d.modrm()?;
    let op = match (vex.map, opcode, vex.prefix) {
        (2, 0xf2, 0) => BmiOp::Andn,
        // Group 17: the operation in ModRM.reg, the destination VEX.vvvv.
        (2, 0xf3, 0) => match reg & 7 {
            1 => BmiOp::Blsr,
            2 => BmiOp::Blsmsk,
            3 => BmiOp::Blsi,
            _ => return Err(DecodeError::Invalid),
        },
        (2, 0xf5, 0) => BmiOp::Bzhi,
        (2, 0xf5, 0xf3) => BmiOp::Pext,
        (2, 0xf5, 0xf2) => BmiOp::Pdep,
        (2, 0xf6, 0xf2) => BmiOp::Mulx,
        (2, 0xf7, 0) => BmiOp::Bextr,
        (2, 0xf7, 0x66) => BmiOp::Shlx,
        (2, 0xf7, 0xf3) => BmiOp::Sarx,
        (2, 0xf7, 0xf2) => BmiOp::Shrx,
        // rorx: VEX.vvvv must be 1111 (no register).
        (3, 0xf0, 0xf2) if vex.vvvv == 0 => BmiOp::Rorx(0),
        _ => return Err(DecodeError::Invalid),
    };
    if vex.l {
        return Err(DecodeError::Invalid);
    }
    let op = match op {
        BmiOp::Rorx(_) => BmiOp::Rorx(d.u8()?),
        op => op,
    };
    let src = d.rm_operand(rm, size);
    let vvvv = Reg::Gpr(vex.vvvv);
    let dst = match op {
        BmiOp::Blsi | BmiOp::Blsmsk | BmiOp::Blsr => vvvv,
        _ => Reg::Gpr(reg),
    };
    Ok((Op::Bmi { op, dst, src, vvvv }, size))
}
