//! VEX-encoded instructions (Intel SDM vol. 2, 2.3), after the prefix
//! (`Decoder::vex_prefix`): BMI1 and BMI2 on general registers, and the
//! VEX forms of the SSE instructions (AVX), decoded by the legacy tables
//! of `sse.rs` and `maps.rs` with VEX.pp as the mandatory prefix, then
//! given their VEX.vvvv operand (docs/tech/avx.md).

use crate::decoder::{Decoder, Rm, Vex};
use crate::sse::{xmm, xmm_rm};
use crate::{AvxOp, BmiOp, DecodeError, IntOp, Op, Operand, Reg, ShiftKind, Size, SseOp};

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
        (2 | 3, _) if vex.prefix == 0x66 && new_instruction(vex, opcode).is_some() => {
            d.u8()?;
            avx(d, vex, opcode)
        }
        // One of them with a VEX.W it does not have.
        (
            2,
            0x0c..=0x0f
            | 0x16
            | 0x18..=0x1a
            | 0x2c..=0x2f
            | 0x36
            | 0x46
            | 0x58..=0x5a
            | 0x78
            | 0x79,
        )
        | (3, 0x00..=0x02 | 0x04..=0x06 | 0x18 | 0x19 | 0x38 | 0x39 | 0x46 | 0x4a..=0x4c)
            if vex.prefix == 0x66 =>
        {
            Err(DecodeError::Invalid)
        }
        (1, _) => {
            d.u8()?;
            sse(vex, opcode, crate::sse::decode(d, opcode)?)
        }
        (2, _) => sse(vex, opcode, crate::maps::decode_38(d)?),
        _ => sse(vex, opcode, crate::maps::decode_3a(d)?),
    }
}

/// What decoding an instruction AVX or AVX2 adds needs (`new_instruction`).
struct New {
    op: AvxOp,
    form: Form,
    length: Length,
    /// AVX2 (for some, only in their 256-bit or register forms: `avx`).
    avx2: bool,
    /// An immediate byte follows.
    imm: bool,
}

/// The vector lengths an instruction has.
#[derive(PartialEq)]
enum Length {
    Both,
    Only256,
}

/// The instructions of maps 2 and 3 (VEX.66) that AVX and AVX2 add, or
/// `None` for the others (and for a VEX.W they do not have).
fn new_instruction(vex: Vex, opcode: u8) -> Option<New> {
    use AvxOp::*;
    use Form::{Source, Unused};
    use Length::{Both, Only256};
    let lane = |w: bool| if w { Size::B64 } else { Size::B32 };
    let new = |op, form, length, avx2| New {
        op,
        form,
        length,
        avx2,
        imm: vex.map == 3,
    };
    let w = vex.w;
    Some(match (vex.map, opcode) {
        (2, 0x0c | 0x0d) if !w => new(
            Permil {
                double: opcode == 0x0d,
                imm: None,
            },
            Source,
            Both,
            false,
        ),
        (2, 0x0e | 0x0f) if !w => new(
            Test {
                double: opcode == 0x0f,
            },
            Unused,
            Both,
            false,
        ),
        (2, 0x16 | 0x36) if !w => new(Permd, Source, Only256, true),
        (2, 0x18) if !w => new(Broadcast(Size::B32), Unused, Both, false),
        (2, 0x19) if !w => new(Broadcast(Size::B64), Unused, Only256, false),
        (2, 0x1a) if !w => new(Broadcast128, Unused, Only256, false),
        (2, 0x5a) if !w => new(Broadcast128, Unused, Only256, true),
        (2, 0x58) if !w => new(Broadcast(Size::B32), Unused, Both, true),
        (2, 0x59) if !w => new(Broadcast(Size::B64), Unused, Both, true),
        (2, 0x78) if !w => new(Broadcast(Size::B8), Unused, Both, true),
        (2, 0x79) if !w => new(Broadcast(Size::B16), Unused, Both, true),
        (2, 0x2c..=0x2f) if !w => new(
            MaskMov {
                lane: lane(opcode & 1 == 1),
                store: opcode >= 0x2e,
            },
            Source,
            Both,
            false,
        ),
        (2, 0x8c | 0x8e) => new(
            MaskMov {
                lane: lane(w),
                store: opcode == 0x8e,
            },
            Source,
            Both,
            true,
        ),
        (2, 0x45) => new(
            ShiftVar {
                kind: ShiftKind::RightLogical,
                lane: lane(w),
            },
            Source,
            Both,
            true,
        ),
        (2, 0x46) if !w => new(
            ShiftVar {
                kind: ShiftKind::RightArithmetic,
                lane: Size::B32,
            },
            Source,
            Both,
            true,
        ),
        (2, 0x47) => new(
            ShiftVar {
                kind: ShiftKind::Left,
                lane: lane(w),
            },
            Source,
            Both,
            true,
        ),
        (3, 0x00 | 0x01) if w => new(Permq(0), Unused, Only256, true),
        (3, 0x02) if !w => new(Blendd(0), Source, Both, true),
        (3, 0x04 | 0x05) if !w => new(
            Permil {
                double: opcode == 0x05,
                imm: Some(0),
            },
            Unused,
            Both,
            false,
        ),
        (3, 0x06 | 0x46) if !w => new(Perm2x128(0), Source, Only256, opcode == 0x46),
        (3, 0x18 | 0x38) if !w => new(Insert128(0), Source, Only256, opcode == 0x38),
        (3, 0x19 | 0x39) if !w => new(Extract128(0), Unused, Only256, opcode == 0x39),
        (3, 0x4a..=0x4c) if !w => new(
            BlendV {
                lane: match opcode {
                    0x4a => Size::B32,
                    0x4b => Size::B64,
                    _ => Size::B8,
                },
                mask: 0,
            },
            Source,
            Both,
            false,
        ),
        _ => return None,
    })
}

/// An instruction AVX or AVX2 adds (`new_instruction`), after its opcode.
fn avx(d: &mut Decoder, vex: Vex, opcode: u8) -> Result<(Op, Size), DecodeError> {
    let New {
        op,
        form,
        length,
        avx2,
        imm,
    } = new_instruction(vex, opcode).expect("checked by `decode`");
    if length == Length::Only256 && !vex.l {
        return Err(DecodeError::Invalid);
    }
    let (reg, rm) = d.modrm()?;
    let is_mem = matches!(rm, Rm::Mem(_));
    let (mut dst, mut src) = (xmm(reg), xmm_rm(rm));
    let imm = if imm { d.u8()? } else { 0 };
    let op = match op {
        AvxOp::Permq(_) => AvxOp::Permq(imm),
        AvxOp::Blendd(_) => AvxOp::Blendd(imm),
        AvxOp::Permil {
            double,
            imm: Some(_),
        } => AvxOp::Permil {
            double,
            imm: Some(imm),
        },
        AvxOp::Perm2x128(_) => AvxOp::Perm2x128(imm),
        AvxOp::Insert128(_) => AvxOp::Insert128(imm & 1),
        AvxOp::Extract128(_) => {
            // vextractf128 xmm/m128, ymm, imm8.
            (dst, src) = (src, dst);
            AvxOp::Extract128(imm & 1)
        }
        AvxOp::BlendV { lane, .. } => AvxOp::BlendV {
            lane,
            // The fourth register in imm8[7:4]; bit 7 is ignored outside
            // 64-bit mode.
            mask: (imm >> 4) & if d.compat() { 7 } else { 15 },
        },
        AvxOp::MaskMov { lane, store } => {
            if !is_mem {
                return Err(DecodeError::Invalid);
            }
            if store {
                (dst, src) = (src, dst);
            }
            AvxOp::MaskMov { lane, store }
        }
        op => op,
    };
    // The 128-bit broadcasts read memory only; vbroadcastss and
    // vbroadcastsd from a register are AVX2.
    let avx2 = match op {
        AvxOp::Broadcast128 if !is_mem => return Err(DecodeError::Invalid),
        AvxOp::Broadcast(Size::B32 | Size::B64) if !avx2 => !is_mem,
        // vpblendvb ymm is AVX2.
        AvxOp::BlendV { lane: Size::B8, .. } => vex.l,
        _ => avx2,
    };
    let src1 = match form {
        Form::Source => Some(vex.vvvv),
        Form::Unused if vex.vvvv != 0 => return Err(DecodeError::Invalid),
        _ => None,
    };
    Ok((
        Op::Avx {
            op,
            dst,
            src1,
            src,
            wide: vex.l,
            avx2,
        },
        Size::B64,
    ))
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
