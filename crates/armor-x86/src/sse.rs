//! SSE/SSE2/SSE3 opcodes of the two-byte map. The mandatory prefix (none, 66,
//! F3, F2) selects the instruction. Without a prefix the integer opcodes are
//! the MMX forms: the same operations on the 64-bit mm registers, decoded
//! as their 66 form with the xmm operands turned into mm (`to_mmx`).

use crate::decoder::{Decoder, Rm};
use crate::{
    Conversion, DecodeError, Dup, FloatOp, LaneOp, LogicOp, MmxConversion, Op, Operand, Reg,
    ShiftKind, Size, SseOp,
};

pub(crate) fn decode(d: &mut Decoder, opcode: u8) -> Result<(Op, Size), DecodeError> {
    let prefix = d.sse_prefix();
    let (op, dst, src) = if prefix == 0 && mmx_as_66(opcode) {
        to_mmx(decode_with(d, 0x66, opcode)?)?
    } else {
        decode_with(d, prefix, opcode)?
    };
    Ok((Op::Sse { op, dst, src }, Size::B64))
}

/// The integer opcodes whose MMX form (no prefix) is their 66 form on mm
/// registers.
fn mmx_as_66(opcode: u8) -> bool {
    matches!(
        opcode,
        0x60..=0x6b
            | 0x6e
            | 0x71..=0x76
            | 0x7e
            | 0xc4
            | 0xc5
            | 0xd1..=0xd5
            | 0xd7..=0xe5
            | 0xe8..=0xef
            | 0xf1..=0xfe
    )
}

/// A 66-form decoding turned into the MMX form: xmm operands become mm
/// (REX does not extend them), word indices have two bits, and the byte
/// shifts of the whole register do not exist.
pub(crate) fn to_mmx((op, dst, src): Decoded) -> Result<Decoded, DecodeError> {
    let mm = |operand| match operand {
        Operand::Reg(Reg::Xmm(n)) => Operand::Reg(Reg::Mm(n & 7)),
        other => other,
    };
    let op = match op {
        SseOp::ByteShift { .. } => return Err(DecodeError::Invalid),
        SseOp::Pinsrw(index) => SseOp::Pinsrw(index & 3),
        SseOp::Pextrw(index) => SseOp::Pextrw(index & 3),
        other => other,
    };
    Ok((op, mm(dst), mm(src)))
}

fn decode_with(d: &mut Decoder, prefix: u8, opcode: u8) -> Result<Decoded, DecodeError> {
    Ok(match (prefix, opcode) {
        // movups / movupd (10, 11), movaps / movapd (28, 29).
        (0 | 0x66, 0x10 | 0x28) => load(
            d,
            SseOp::Mov128 {
                aligned: opcode == 0x28,
            },
        )?,
        (0 | 0x66, 0x11 | 0x29) => store(
            d,
            SseOp::Mov128 {
                aligned: opcode == 0x29,
            },
        )?,
        // movss (F3) / movsd (F2).
        (0xf3 | 0xf2, 0x10) => load(
            d,
            SseOp::MovScalar {
                double: prefix == 0xf2,
            },
        )?,
        (0xf3 | 0xf2, 0x11) => store(
            d,
            SseOp::MovScalar {
                double: prefix == 0xf2,
            },
        )?,
        // cvtsi2ss / cvtsi2sd xmm, r/m32 or r/m64.
        (0xf3 | 0xf2, 0x2a) => {
            let int_bytes = if d.rex_w() { 8 } else { 4 };
            let (reg, rm) = d.modrm()?;
            let src = d.rm_operand(rm, gpr_size(int_bytes));
            (
                SseOp::IntToFloat {
                    double: prefix == 0xf2,
                    int_bytes,
                },
                xmm(reg),
                src,
            )
        }
        // cvttss2si / cvttsd2si (2C), cvtss2si / cvtsd2si (2D) r32/r64, xmm/m.
        (0xf3 | 0xf2, 0x2c | 0x2d) => {
            let int_bytes = if d.rex_w() { 8 } else { 4 };
            let (reg, rm) = d.modrm()?;
            (
                SseOp::FloatToInt {
                    double: prefix == 0xf2,
                    int_bytes,
                    truncate: opcode == 0x2c,
                },
                Operand::Reg(Reg::Gpr(reg)),
                xmm_rm(rm),
            )
        }
        // ucomiss / ucomisd (2E), comiss / comisd (2F).
        (0 | 0x66, 0x2e | 0x2f) => load(
            d,
            SseOp::FloatCompare {
                double: prefix == 0x66,
                signaling: opcode == 0x2f,
            },
        )?,
        // movmskps / movmskpd r32, xmm.
        (0 | 0x66, 0x50) => {
            let (reg, rm) = d.modrm()?;
            let Rm::Reg(n) = rm else {
                return Err(DecodeError::Invalid);
            };
            (
                SseOp::FloatSignMask {
                    double: prefix == 0x66,
                },
                Operand::Reg(Reg::Gpr(reg)),
                xmm(n),
            )
        }
        // Scalar arithmetic: sqrt, add, mul, sub, min, div, max (ss / sd).
        (0xf3 | 0xf2, 0x51 | 0x58 | 0x59 | 0x5c..=0x5f) => load(
            d,
            SseOp::FloatArith {
                op: float_op(opcode),
                double: prefix == 0xf2,
            },
        )?,
        // Packed arithmetic: sqrt, add, mul, sub, min, div, max (ps / pd).
        (0 | 0x66, 0x51 | 0x58 | 0x59 | 0x5c..=0x5f) => load(
            d,
            SseOp::PackedArith {
                op: float_op(opcode),
                double: prefix == 0x66,
            },
        )?,
        // rsqrtps / rcpps, rsqrtss / rcpss (single precision only).
        (0 | 0xf3, 0x52 | 0x53) => {
            let op = if opcode == 0x52 {
                FloatOp::Rsqrt
            } else {
                FloatOp::Rcp
            };
            let op = if prefix == 0 {
                SseOp::PackedArith { op, double: false }
            } else {
                SseOp::FloatArith { op, double: false }
            };
            load(d, op)?
        }
        // Packed conversions.
        (0, 0x5a) => load(d, SseOp::PackedConvert(Conversion::Ps2Pd))?,
        (0x66, 0x5a) => load(d, SseOp::PackedConvert(Conversion::Pd2Ps))?,
        (0, 0x5b) => load(d, SseOp::PackedConvert(Conversion::Dq2Ps))?,
        (0x66 | 0xf3, 0x5b) => load(
            d,
            SseOp::PackedConvert(Conversion::Ps2Dq {
                truncate: prefix == 0xf3,
            }),
        )?,
        (0xf3, 0xe6) => load(d, SseOp::PackedConvert(Conversion::Dq2Pd))?,
        (0x66 | 0xf2, 0xe6) => load(
            d,
            SseOp::PackedConvert(Conversion::Pd2Dq {
                truncate: prefix == 0x66,
            }),
        )?,
        // cmpps / cmppd xmm, xmm/m128, imm8.
        (0 | 0x66, 0xc2) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            let predicate = d.u8()?;
            if predicate > 7 {
                return Err(DecodeError::Unsupported("AVX compare predicates"));
            }
            (
                SseOp::PackedCmpMask {
                    double: prefix == 0x66,
                    predicate,
                },
                xmm(reg),
                src,
            )
        }
        // cvtss2sd (F3) / cvtsd2ss (F2).
        (0xf3 | 0xf2, 0x5a) => load(
            d,
            SseOp::FloatToFloat {
                to_double: prefix == 0xf3,
            },
        )?,
        // SSE3: addsubpd (66) / addsubps (F2).
        (0x66 | 0xf2, 0xd0) => load(
            d,
            SseOp::AddSub {
                double: prefix == 0x66,
            },
        )?,
        // SSE3: haddpd / haddps (7C), hsubpd / hsubps (7D).
        (0x66 | 0xf2, 0x7c | 0x7d) => load(
            d,
            SseOp::Horizontal {
                sub: opcode == 0x7d,
                double: prefix == 0x66,
            },
        )?,
        // SSE3: movddup (F2 12, xmm or m64), movsldup (F3 12), movshdup
        // (F3 16).
        (0xf2, 0x12) => load(d, SseOp::MovDup(Dup::Low64))?,
        (0xf3, 0x12) => load(d, SseOp::MovDup(Dup::EvenSingles))?,
        (0xf3, 0x16) => load(d, SseOp::MovDup(Dup::OddSingles))?,
        // SSE3: lddqu xmm, m128 (unaligned load).
        (0xf2, 0xf0) => load_mem(d, SseOp::Mov128 { aligned: false })?,
        // movntps / movntpd: aligned non-temporal stores.
        (0 | 0x66, 0x2b) => store_mem(d, SseOp::Mov128 { aligned: true })?,

        // movlps / movhps loads, or movhlps / movlhps between registers.
        (0, 0x12 | 0x16) => {
            let high = opcode == 0x16;
            let (reg, rm) = d.modrm()?;
            match rm {
                Rm::Reg(n) => (
                    if high { SseOp::MovLhps } else { SseOp::MovHlps },
                    xmm(reg),
                    xmm(n),
                ),
                Rm::Mem(m) => (SseOp::MovHalf { high }, xmm(reg), Operand::Mem(m)),
            }
        }
        // movlpd / movhpd loads: memory source only.
        (0x66, 0x12 | 0x16) => load_mem(
            d,
            SseOp::MovHalf {
                high: opcode == 0x16,
            },
        )?,
        // movlps / movlpd / movhps / movhpd stores.
        (0 | 0x66, 0x13 | 0x17) => store_mem(
            d,
            SseOp::MovHalf {
                high: opcode == 0x17,
            },
        )?,

        // unpcklps / unpckhps (32-bit lanes), unpcklpd / unpckhpd (64-bit).
        (0 | 0x66, 0x14 | 0x15) => {
            let lane = if prefix == 0x66 { Size::B64 } else { Size::B32 };
            load(
                d,
                SseOp::Unpack {
                    high: opcode == 0x15,
                    lane,
                },
            )?
        }

        // andps/andnps/orps/xorps and the pd forms: bitwise on 128 bits.
        (0 | 0x66, 0x54..=0x57) => load(d, SseOp::Logic(logic(opcode - 0x54)))?,

        (0x66, 0x60..=0x62) => load(
            d,
            SseOp::Unpack {
                high: false,
                lane: lane(opcode - 0x60),
            },
        )?,
        (0x66, 0x68..=0x6a) => load(
            d,
            SseOp::Unpack {
                high: true,
                lane: lane(opcode - 0x68),
            },
        )?,
        (0x66, 0x6c | 0x6d) => load(
            d,
            SseOp::Unpack {
                high: opcode == 0x6d,
                lane: Size::B64,
            },
        )?,
        (0x66, 0x64..=0x66) => load(
            d,
            SseOp::Lane {
                op: LaneOp::CmpGt,
                lane: lane(opcode - 0x64),
            },
        )?,
        (0x66, 0x74..=0x76) => load(
            d,
            SseOp::Lane {
                op: LaneOp::CmpEq,
                lane: lane(opcode - 0x74),
            },
        )?,

        // movd / movq xmm, r/m32 / r/m64.
        (0x66, 0x6e) => {
            let bytes = if d.rex_w() { 8 } else { 4 };
            let (reg, rm) = d.modrm()?;
            let src = d.rm_operand(rm, gpr_size(bytes));
            (SseOp::MovToXmm { bytes }, xmm(reg), src)
        }
        // movd / movq r/m32 / r/m64, xmm.
        (0x66, 0x7e) => {
            let bytes = if d.rex_w() { 8 } else { 4 };
            let (reg, rm) = d.modrm()?;
            let dst = d.rm_operand(rm, gpr_size(bytes));
            (SseOp::MovFromXmm { bytes }, dst, xmm(reg))
        }
        // movq xmm, xmm/m64.
        (0xf3, 0x7e) => load(d, SseOp::MovToXmm { bytes: 8 })?,
        // movq xmm/m64, xmm.
        (0x66, 0xd6) => store(d, SseOp::MovFromXmm { bytes: 8 })?,

        // movdqa / movdqu.
        (0x66 | 0xf3, 0x6f) => load(
            d,
            SseOp::Mov128 {
                aligned: prefix == 0x66,
            },
        )?,
        (0x66 | 0xf3, 0x7f) => store(
            d,
            SseOp::Mov128 {
                aligned: prefix == 0x66,
            },
        )?,
        // movntdq.
        (0x66, 0xe7) => store_mem(d, SseOp::Mov128 { aligned: true })?,

        (0x66, 0x70) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            let order = d.u8()?;
            (SseOp::Pshufd(order), xmm(reg), src)
        }
        (0xf2 | 0xf3, 0x70) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            let order = d.u8()?;
            (
                SseOp::PshufWords {
                    high: prefix == 0xf3,
                    order,
                },
                xmm(reg),
                src,
            )
        }
        (0x66, 0xc4) => {
            // pinsrw xmm, r32/m16, imm8.
            let (reg, rm) = d.modrm()?;
            let src = d.rm_operand(rm, Size::B16);
            let index = d.u8()?;
            (SseOp::Pinsrw(index & 7), xmm(reg), src)
        }
        (0x66, 0xc5) => {
            // pextrw r32, xmm, imm8 (register source only).
            let (reg, rm) = d.modrm()?;
            let Rm::Reg(n) = rm else {
                return Err(DecodeError::Invalid);
            };
            let index = d.u8()?;
            (
                SseOp::Pextrw(index & 7),
                Operand::Reg(Reg::Gpr(reg)),
                xmm(n),
            )
        }
        (0 | 0x66, 0xc6) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            let order = d.u8()?;
            (
                SseOp::Shuffle {
                    double: prefix == 0x66,
                    order,
                },
                xmm(reg),
                src,
            )
        }
        // cmpss (F3) / cmpsd (F2) xmm, xmm/m, imm8.
        (0xf3 | 0xf2, 0xc2) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            let predicate = d.u8()?;
            if predicate > 7 {
                return Err(DecodeError::Unsupported("AVX compare predicates"));
            }
            (
                SseOp::FloatCmpMask {
                    double: prefix == 0xf2,
                    predicate,
                },
                xmm(reg),
                src,
            )
        }

        // Shifts by immediate: 71 words, 72 dwords, 73 qwords and the
        // whole-register byte shifts. Register operand only.
        (0x66, 0x71..=0x73) => {
            let (digit, rm) = d.modrm()?;
            let Rm::Reg(n) = rm else {
                return Err(DecodeError::Invalid);
            };
            let count = d.u8()?;
            let lane = match opcode {
                0x71 => Size::B16,
                0x72 => Size::B32,
                _ => Size::B64,
            };
            let op = match (opcode, digit & 7) {
                (_, 2) => SseOp::ShiftImm {
                    kind: ShiftKind::RightLogical,
                    lane,
                    count,
                },
                (0x71 | 0x72, 4) => SseOp::ShiftImm {
                    kind: ShiftKind::RightArithmetic,
                    lane,
                    count,
                },
                (_, 6) => SseOp::ShiftImm {
                    kind: ShiftKind::Left,
                    lane,
                    count,
                },
                (0x73, 3) => SseOp::ByteShift { left: false, count },
                (0x73, 7) => SseOp::ByteShift { left: true, count },
                _ => return Err(DecodeError::Invalid),
            };
            (op, xmm(n), Operand::Imm(i64::from(count)))
        }

        (0x66, 0x63) => load(
            d,
            SseOp::Pack {
                from: Size::B16,
                signed: true,
            },
        )?,
        (0x66, 0x67) => load(
            d,
            SseOp::Pack {
                from: Size::B16,
                signed: false,
            },
        )?,
        (0x66, 0x6b) => load(
            d,
            SseOp::Pack {
                from: Size::B32,
                signed: true,
            },
        )?,
        (0x66, 0xd4) => load(
            d,
            SseOp::Lane {
                op: LaneOp::Add,
                lane: Size::B64,
            },
        )?,
        (0x66, 0xd7) => {
            // pmovmskb r32, xmm.
            let (reg, rm) = d.modrm()?;
            let Rm::Reg(n) = rm else {
                return Err(DecodeError::Invalid);
            };
            (SseOp::Pmovmskb, Operand::Reg(Reg::Gpr(reg)), xmm(n))
        }
        (0x66, 0xda) => load(d, lane_op(LaneOp::MinU, Size::B8))?,
        (0x66, 0xde) => load(d, lane_op(LaneOp::MaxU, Size::B8))?,
        (0x66, 0xea) => load(d, lane_op(LaneOp::MinS, Size::B16))?,
        (0x66, 0xee) => load(d, lane_op(LaneOp::MaxS, Size::B16))?,
        (0x66, 0xdb) => load(d, SseOp::Logic(LogicOp::And))?,
        (0x66, 0xdf) => load(d, SseOp::Logic(LogicOp::AndNot))?,
        (0x66, 0xeb) => load(d, SseOp::Logic(LogicOp::Or))?,
        (0x66, 0xef) => load(d, SseOp::Logic(LogicOp::Xor))?,
        (0x66, 0xf4) => load(d, SseOp::Pmuludq)?,
        (0x66, 0xd5) => load(d, lane_op(LaneOp::MulLow, Size::B16))?,
        (0x66, 0xe5) => load(d, lane_op(LaneOp::MulHigh { signed: true }, Size::B16))?,
        (0x66, 0xe4) => load(d, lane_op(LaneOp::MulHigh { signed: false }, Size::B16))?,
        (0x66, 0xe0) => load(d, lane_op(LaneOp::Avg, Size::B8))?,
        (0x66, 0xe3) => load(d, lane_op(LaneOp::Avg, Size::B16))?,
        (0x66, 0xd8 | 0xd9) => load(
            d,
            lane_op(LaneOp::SubSat { signed: false }, lane(opcode - 0xd8)),
        )?,
        (0x66, 0xdc | 0xdd) => load(
            d,
            lane_op(LaneOp::AddSat { signed: false }, lane(opcode - 0xdc)),
        )?,
        (0x66, 0xe8 | 0xe9) => load(
            d,
            lane_op(LaneOp::SubSat { signed: true }, lane(opcode - 0xe8)),
        )?,
        (0x66, 0xec | 0xed) => load(
            d,
            lane_op(LaneOp::AddSat { signed: true }, lane(opcode - 0xec)),
        )?,
        (0x66, 0xf5) => load(d, SseOp::Pmaddwd)?,
        (0x66, 0xf6) => load(d, SseOp::Psadbw)?,
        // Shifts by the count in an xmm register or m128.
        (0x66, 0xd1..=0xd3) => load(
            d,
            SseOp::ShiftXmm {
                kind: ShiftKind::RightLogical,
                lane: lane(opcode - 0xd0),
            },
        )?,
        (0x66, 0xe1 | 0xe2) => load(
            d,
            SseOp::ShiftXmm {
                kind: ShiftKind::RightArithmetic,
                lane: lane(opcode - 0xe0),
            },
        )?,
        (0x66, 0xf1..=0xf3) => load(
            d,
            SseOp::ShiftXmm {
                kind: ShiftKind::Left,
                lane: lane(opcode - 0xf0),
            },
        )?,
        // maskmovdqu xmm, xmm: register operands only.
        (0x66, 0xf7) => {
            let (reg, rm) = d.modrm()?;
            let Rm::Reg(n) = rm else {
                return Err(DecodeError::Invalid);
            };
            (SseOp::Maskmovdqu, xmm(reg), xmm(n))
        }
        (0x66, 0xf8..=0xfb) => load(d, lane_op(LaneOp::Sub, lane(opcode - 0xf8)))?,
        (0x66, 0xfc..=0xfe) => load(d, lane_op(LaneOp::Add, lane(opcode - 0xfc)))?,

        // MMX: movq mm, mm/m64 and mm/m64, mm; movntq m64, mm.
        (0, 0x6f) => {
            let (reg, rm) = d.modrm()?;
            (SseOp::MovToXmm { bytes: 8 }, mm(reg), mm_rm(rm))
        }
        (0, 0x7f) => {
            let (reg, rm) = d.modrm()?;
            (SseOp::MovFromXmm { bytes: 8 }, mm_rm(rm), mm(reg))
        }
        (0, 0xe7) => {
            let (reg, rm) = d.modrm()?;
            match rm {
                Rm::Mem(m) => (SseOp::MovFromXmm { bytes: 8 }, Operand::Mem(m), mm(reg)),
                Rm::Reg(_) => return Err(DecodeError::Invalid),
            }
        }
        // pshufw mm, mm/m64, imm8: the low-word shuffle of pshuflw.
        (0, 0x70) => {
            let (reg, rm) = d.modrm()?;
            let src = mm_rm(rm);
            let order = d.u8()?;
            (SseOp::PshufWords { high: false, order }, mm(reg), src)
        }
        // emms: no operands.
        (0, 0x77) => (SseOp::Emms, Operand::Imm(0), Operand::Imm(0)),
        // movq2dq xmm, mm (F3 D6) and movdq2q mm, xmm (F2 D6): registers.
        (0xf3 | 0xf2, 0xd6) => {
            let (reg, rm) = d.modrm()?;
            let Rm::Reg(n) = rm else {
                return Err(DecodeError::Invalid);
            };
            if prefix == 0xf3 {
                (SseOp::MovToXmm { bytes: 8 }, xmm(reg), mm(n))
            } else {
                (SseOp::MovFromXmm { bytes: 8 }, mm(reg), xmm(n))
            }
        }
        // cvtpi2ps (no prefix) / cvtpi2pd (66) xmm, mm/m64.
        (0 | 0x66, 0x2a) => {
            let (reg, rm) = d.modrm()?;
            let conversion = if prefix == 0 {
                MmxConversion::Pi2Ps
            } else {
                MmxConversion::Pi2Pd
            };
            (SseOp::MmxConvert(conversion), xmm(reg), mm_rm(rm))
        }
        // cvttps2pi / cvtps2pi mm, xmm/m64; cvttpd2pi / cvtpd2pi mm,
        // xmm/m128.
        (0 | 0x66, 0x2c | 0x2d) => {
            let (reg, rm) = d.modrm()?;
            let truncate = opcode == 0x2c;
            let conversion = if prefix == 0 {
                MmxConversion::Ps2Pi { truncate }
            } else {
                MmxConversion::Pd2Pi { truncate }
            };
            (SseOp::MmxConvert(conversion), mm(reg), xmm_rm(rm))
        }
        // No MMX form: #UD (0F FF is ud0).
        (0, 0xd6 | 0xe6 | 0xf0 | 0xff) => return Err(DecodeError::Invalid),
        _ => return Err(DecodeError::Unsupported("SSE instruction")),
    })
}

/// mm0–mm7: REX does not extend the MMX registers.
fn mm(n: u8) -> Operand {
    Operand::Reg(Reg::Mm(n & 7))
}

fn mm_rm(rm: Rm) -> Operand {
    match rm {
        Rm::Reg(n) => mm(n),
        Rm::Mem(m) => Operand::Mem(m),
    }
}

pub(crate) type Decoded = (SseOp, Operand, Operand);

/// `op xmm, xmm/m`.
pub(crate) fn load(d: &mut Decoder, op: SseOp) -> Result<Decoded, DecodeError> {
    let (reg, rm) = d.modrm()?;
    Ok((op, xmm(reg), xmm_rm(rm)))
}

/// `op xmm, m`: memory source only.
pub(crate) fn load_mem(d: &mut Decoder, op: SseOp) -> Result<Decoded, DecodeError> {
    let (reg, rm) = d.modrm()?;
    match rm {
        Rm::Mem(m) => Ok((op, xmm(reg), Operand::Mem(m))),
        Rm::Reg(_) => Err(DecodeError::Invalid),
    }
}

/// `op xmm/m, xmm`.
fn store(d: &mut Decoder, op: SseOp) -> Result<Decoded, DecodeError> {
    let (reg, rm) = d.modrm()?;
    Ok((op, xmm_rm(rm), xmm(reg)))
}

/// `op m, xmm`: memory destination only.
fn store_mem(d: &mut Decoder, op: SseOp) -> Result<Decoded, DecodeError> {
    let (reg, rm) = d.modrm()?;
    match rm {
        Rm::Mem(m) => Ok((op, Operand::Mem(m), xmm(reg))),
        Rm::Reg(_) => Err(DecodeError::Invalid),
    }
}

pub(crate) fn xmm(n: u8) -> Operand {
    Operand::Reg(Reg::Xmm(n))
}

pub(crate) fn xmm_rm(rm: Rm) -> Operand {
    match rm {
        Rm::Reg(n) => xmm(n),
        Rm::Mem(m) => Operand::Mem(m),
    }
}

fn gpr_size(bytes: u8) -> Size {
    if bytes == 8 { Size::B64 } else { Size::B32 }
}

fn lane_op(op: LaneOp, lane: Size) -> SseOp {
    SseOp::Lane { op, lane }
}

/// Lane size from the position in a b/w/d/q opcode row.
fn lane(index: u8) -> Size {
    [Size::B8, Size::B16, Size::B32, Size::B64][usize::from(index)]
}

/// The operation of the 0F 51..5F arithmetic rows.
fn float_op(opcode: u8) -> FloatOp {
    match opcode {
        0x51 => FloatOp::Sqrt,
        0x58 => FloatOp::Add,
        0x59 => FloatOp::Mul,
        0x5c => FloatOp::Sub,
        0x5d => FloatOp::Min,
        0x5e => FloatOp::Div,
        _ => FloatOp::Max,
    }
}

fn logic(index: u8) -> LogicOp {
    [LogicOp::And, LogicOp::AndNot, LogicOp::Or, LogicOp::Xor][usize::from(index)]
}
