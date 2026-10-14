//! The three-byte opcode maps 0F 38 and 0F 3A: SSSE3, SSE4.1, SSE4.2.
//! Without a prefix the SSSE3 opcodes are MMX forms, decoded as their 66
//! form on mm registers (`sse::to_mmx`).

use crate::decoder::{Decoder, Rm};
use crate::sse::{Decoded, load, to_mmx, xmm, xmm_rm};
use crate::{DecodeError, IntOp, Op, Operand, Reg, Size, SseOp};

/// 0F 38 xx.
pub(crate) fn decode_38(d: &mut Decoder) -> Result<(Op, Size), DecodeError> {
    let opcode = d.u8()?;
    let prefix = d.sse_prefix();
    // crc32 r32/r64, r/m8 (F0) or r/m16/32/64 (F1), SSE4.2.
    if prefix == 0xf2 && matches!(opcode, 0xf0 | 0xf1) {
        let src_size = if opcode == 0xf0 {
            Size::B8
        } else {
            d.operand_size()
        };
        let dst_size = if d.rex_w() { Size::B64 } else { Size::B32 };
        let (reg, rm) = d.modrm()?;
        let src = d.rm_operand(rm, src_size);
        let op = Op::Crc32 {
            dst: Reg::Gpr(reg),
            src,
            src_size,
        };
        return Ok((op, dst_size));
    }
    let int = |op| SseOp::Int(op);
    let mmx = prefix == 0 && matches!(opcode, 0x00..=0x0b | 0x1c..=0x1e);
    let prefix = if mmx { 0x66 } else { prefix };
    let (op, dst, src): Decoded = match (prefix, opcode) {
        (0x66, 0x00) => load(d, int(IntOp::Pshufb))?,
        // phaddw, phaddd, phaddsw (01–03); phsubw, phsubd, phsubsw (05–07).
        (0x66, 0x01..=0x03 | 0x05..=0x07) => load(
            d,
            int(IntOp::Horizontal {
                sub: opcode >= 0x05,
                saturate: opcode & 3 == 3,
                lane: if opcode & 3 == 2 {
                    Size::B32
                } else {
                    Size::B16
                },
            }),
        )?,
        (0x66, 0x04) => load(d, int(IntOp::Pmaddubsw))?,
        (0x66, 0x08..=0x0a) => load(d, int(IntOp::Psign(lane(opcode - 0x08))))?,
        (0x66, 0x0b) => load(d, int(IntOp::Pmulhrsw))?,
        (0x66, 0x1c..=0x1e) => load(d, int(IntOp::Pabs(lane(opcode - 0x1c))))?,
        // SSE4.1.
        (0x66, 0x10) => load(d, int(IntOp::BlendV(Size::B8)))?,
        (0x66, 0x14) => load(d, int(IntOp::BlendV(Size::B32)))?,
        (0x66, 0x15) => load(d, int(IntOp::BlendV(Size::B64)))?,
        (0x66, 0x17) => load(d, SseOp::Ptest)?,
        // pmovsx (20–25) / pmovzx (30–35): bw, bd, bq, wd, wq, dq.
        (0x66, 0x20..=0x25 | 0x30..=0x35) => {
            let (from, to) = match opcode & 0xf {
                0 => (Size::B8, Size::B16),
                1 => (Size::B8, Size::B32),
                2 => (Size::B8, Size::B64),
                3 => (Size::B16, Size::B32),
                4 => (Size::B16, Size::B64),
                _ => (Size::B32, Size::B64),
            };
            load(
                d,
                SseOp::Pmovx {
                    signed: opcode < 0x30,
                    from,
                    to,
                },
            )?
        }
        (0x66, 0x28) => load(d, int(IntOp::Pmuldq))?,
        (0x66, 0x29) => load(d, int(IntOp::Pcmpq { greater: false }))?,
        // movntdqa xmm, m128: an aligned load.
        (0x66, 0x2a) => crate::sse::load_mem(d, SseOp::Mov128 { aligned: true })?,
        (0x66, 0x2b) => load(d, int(IntOp::Packusdw))?,
        // pminsb, pminsd, pminuw, pminud (38–3B); pmax* (3C–3F).
        (0x66, 0x38..=0x3f) => {
            let (signed, lane) = match opcode & 3 {
                0 => (true, Size::B8),
                1 => (true, Size::B32),
                2 => (false, Size::B16),
                _ => (false, Size::B32),
            };
            load(
                d,
                int(IntOp::MinMax {
                    max: opcode >= 0x3c,
                    signed,
                    lane,
                }),
            )?
        }
        (0x66, 0x40) => load(d, int(IntOp::Pmulld))?,
        // SSE4.2.
        (0x66, 0x37) => load(d, int(IntOp::Pcmpq { greater: true }))?,
        (0x66, 0x41) => load(d, int(IntOp::Phminposuw))?,
        _ => return Err(DecodeError::Unsupported("0F 38 instruction")),
    };
    let (op, dst, src) = if mmx {
        to_mmx((op, dst, src))?
    } else {
        (op, dst, src)
    };
    Ok((Op::Sse { op, dst, src }, Size::B64))
}

/// 0F 3A xx ib.
pub(crate) fn decode_3a(d: &mut Decoder) -> Result<(Op, Size), DecodeError> {
    let opcode = d.u8()?;
    let prefix = d.sse_prefix();
    let (op, dst, src): Decoded = match (prefix, opcode) {
        (0 | 0x66, 0x0f) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            let decoded = (SseOp::Int(IntOp::Palignr(d.u8()?)), xmm(reg), src);
            if prefix == 0 {
                to_mmx(decoded)?
            } else {
                decoded
            }
        }
        // SSE4.1.
        (0x66, 0x08..=0x0b) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            let op = SseOp::Round {
                double: opcode & 1 == 1,
                scalar: opcode >= 0x0a,
                imm: d.u8()?,
            };
            (op, xmm(reg), src)
        }
        (0x66, 0x0c..=0x0e) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            let lane = match opcode {
                0x0c => Size::B32,
                0x0d => Size::B64,
                _ => Size::B16,
            };
            let imm = d.u8()?;
            (SseOp::Int(IntOp::Blend { lane, imm }), xmm(reg), src)
        }
        // pextrb (14), pextrw (15), pextrd/q (16), extractps (17) r/m, xmm.
        (0x66, 0x14..=0x17) => {
            let lane = match opcode {
                0x14 => Size::B8,
                0x15 => Size::B16,
                0x16 if d.rex_w() => Size::B64,
                _ => Size::B32,
            };
            let (reg, rm) = d.modrm()?;
            let dst = gpr_or_mem(rm);
            let index = d.u8()? & (128 / lane.bits() - 1) as u8;
            (SseOp::Extract { lane, index }, dst, xmm(reg))
        }
        // pinsrb (20), pinsrd/q (22) xmm, r32/r64 or memory.
        (0x66, 0x20 | 0x22) => {
            let lane = match opcode {
                0x20 => Size::B8,
                _ if d.rex_w() => Size::B64,
                _ => Size::B32,
            };
            let (reg, rm) = d.modrm()?;
            let src = gpr_or_mem(rm);
            let index = d.u8()? & (128 / lane.bits() - 1) as u8;
            (SseOp::Insert { lane, index }, xmm(reg), src)
        }
        (0x66, 0x21) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            (SseOp::Insertps(d.u8()?), xmm(reg), src)
        }
        (0x66, 0x40 | 0x41) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            let op = SseOp::Dp {
                double: opcode == 0x41,
                imm: d.u8()?,
            };
            (op, xmm(reg), src)
        }
        (0x66, 0x42) => {
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            (SseOp::Int(IntOp::Mpsadbw(d.u8()?)), xmm(reg), src)
        }
        // SSE4.2: pcmpestrm (60), pcmpestri (61), pcmpistrm (62),
        // pcmpistri (63).
        (0x66, 0x60..=0x63) => {
            let wide = d.rex_w();
            let (reg, rm) = d.modrm()?;
            let src = xmm_rm(rm);
            let op = SseOp::PcmpStr {
                explicit: opcode < 0x62,
                mask: opcode & 1 == 0,
                wide,
                imm: d.u8()?,
            };
            (op, xmm(reg), src)
        }
        _ => return Err(DecodeError::Unsupported("0F 3A instruction")),
    };
    Ok((Op::Sse { op, dst, src }, Size::B64))
}

/// A general-purpose register (whole, never AH–BH) or memory.
fn gpr_or_mem(rm: Rm) -> Operand {
    match rm {
        Rm::Reg(n) => Operand::Reg(Reg::Gpr(n)),
        Rm::Mem(m) => Operand::Mem(m),
    }
}

/// Lanes of 8, 16, 32 bits for index 0, 1, 2.
fn lane(index: u8) -> Size {
    match index {
        0 => Size::B8,
        1 => Size::B16,
        _ => Size::B32,
    }
}
