//! Prefixes, ModRM/SIB and the one- and two-byte opcode maps.
//! SSE opcodes of the two-byte map are decoded in `sse.rs`.

use crate::{
    AluOp, Base, BitTestOp, Cond, DecodeError, FlagOp, Instruction, LoopKind, MAX_INSTRUCTION_LEN,
    Mem, Mode, Op, Operand, Reg, Rep, Segment, ShiftCount, ShiftOp, Size, StateOp, StringOp,
    UnaryOp,
};

const REX_W: u8 = 8;
const REX_R: u8 = 4;
const REX_X: u8 = 2;
const REX_B: u8 = 1;

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<Instruction, DecodeError> {
    let compat = mode == Mode::Compat32;
    let mut d = Decoder {
        bytes,
        pos: 0,
        rex: 0,
        has_rex: false,
        opsize16: false,
        // In compatibility mode addresses are 32 bits by default.
        addr32: compat,
        addr16: false,
        compat,
        lock: false,
        rep: None,
        segment: Segment::Default,
    };
    d.prefixes()?;
    let (op, size) = d.instruction()?;
    if d.lock && !lockable(&op) {
        return Err(DecodeError::Invalid);
    }
    Ok(Instruction {
        op,
        size,
        len: d.pos,
        lock: d.lock,
        addr32: d.addr32,
    })
}

/// LOCK is only valid on read-modify-write instructions with a memory
/// destination; anywhere else the CPU raises #UD.
fn lockable(op: &Op) -> bool {
    match op {
        Op::Alu { op, dst, .. } => *op != AluOp::Cmp && dst.is_mem(),
        Op::Unary { dst, .. } | Op::Cmpxchg { dst, .. } | Op::Xadd { dst, .. } => dst.is_mem(),
        Op::CmpxchgWide(_) => true,
        Op::Xchg { a, b } => a.is_mem() || b.is_mem(),
        Op::BitTest { op, dst, .. } => *op != BitTestOp::Bt && dst.is_mem(),
        _ => false,
    }
}

pub(crate) struct Decoder<'a> {
    bytes: &'a [u8],
    pos: usize,
    rex: u8,
    has_rex: bool,
    opsize16: bool,
    addr32: bool,
    /// The address-size prefix in compatibility mode: 16-bit addresses.
    addr16: bool,
    /// Compatibility mode (32-bit code, ADR-0029).
    compat: bool,
    lock: bool,
    /// The last F2 or F3 prefix.
    rep: Option<u8>,
    segment: Segment,
}

/// The r/m half of a ModRM byte.
pub(crate) enum Rm {
    Reg(u8),
    Mem(Mem),
}

impl Decoder<'_> {
    fn instruction(&mut self) -> Result<(Op, Size), DecodeError> {
        let opcode = self.u8()?;
        let size = self.operand_size();
        match opcode {
            0x0f => self.two_byte(),

            // ALU rows: 00–05 add, 08–0D or, 10–15 adc, 18–1D sbb,
            // 20–25 and, 28–2D sub, 30–35 xor, 38–3D cmp.
            0x00..=0x3f if opcode & 7 < 6 => {
                let alu = AluOp::from_index(opcode >> 3);
                let size = if opcode & 1 == 0 { Size::B8 } else { size };
                let (dst, src) = match opcode & 7 {
                    0 | 1 => {
                        let (reg, rm) = self.modrm()?;
                        (self.rm_operand(rm, size), self.reg_operand(reg, size))
                    }
                    2 | 3 => {
                        let (reg, rm) = self.modrm()?;
                        (self.reg_operand(reg, size), self.rm_operand(rm, size))
                    }
                    _ => (Operand::Reg(Reg::Gpr(0)), self.imm_for(size)?),
                };
                Ok((Op::Alu { op: alu, dst, src }, size))
            }
            // push/pop segment registers and BCD adjust (daa, das, aaa, aas):
            // compatibility mode only.
            0x06 | 0x07 | 0x0e | 0x16 | 0x17 | 0x1e | 0x1f if self.compat => {
                Err(DecodeError::Unsupported("push/pop of a segment register"))
            }
            0x27 | 0x2f | 0x37 | 0x3f if self.compat => {
                Err(DecodeError::Unsupported("BCD adjust (daa, das, aaa, aas)"))
            }
            0x06 | 0x07 | 0x0e | 0x16 | 0x17 | 0x1e | 0x1f | 0x27 | 0x2f | 0x37 | 0x3f => {
                Err(DecodeError::Invalid)
            }
            // inc and dec of a register (compatibility mode: REX prefixes
            // in 64-bit mode).
            0x40..=0x4f => {
                let op = if opcode < 0x48 {
                    UnaryOp::Inc
                } else {
                    UnaryOp::Dec
                };
                let dst = Operand::Reg(Reg::Gpr(opcode & 7));
                Ok((Op::Unary { op, dst }, size))
            }

            0x50..=0x57 => {
                let reg = (opcode & 7) | self.rex_bit(REX_B);
                Ok((Op::Push(Operand::Reg(Reg::Gpr(reg))), self.push_size()))
            }
            0x58..=0x5f => {
                let reg = (opcode & 7) | self.rex_bit(REX_B);
                Ok((Op::Pop(Operand::Reg(Reg::Gpr(reg))), self.push_size()))
            }
            // pusha, popa.
            0x60 | 0x61 if self.compat => Err(DecodeError::Unsupported("pusha/popa")),
            0x60 | 0x61 => Err(DecodeError::Invalid),
            // bound (compatibility mode, unless the next byte makes it EVEX).
            0x62 if self.compat && self.peek_modrm()? >> 6 != 3 => {
                Err(DecodeError::Unsupported("bound"))
            }
            0x62 => Err(DecodeError::Unsupported("AVX-512 (EVEX)")),
            0x63 if self.compat => Err(DecodeError::Unsupported("arpl")),
            0x63 => {
                // movsxd: sign-extends only with REX.W; otherwise a plain move.
                let (reg, rm) = self.modrm()?;
                let src_size = if size == Size::B64 { Size::B32 } else { size };
                let src = self.rm_operand(rm, src_size);
                Ok((
                    Op::Extend {
                        signed: true,
                        dst: Reg::Gpr(reg),
                        src,
                        src_size,
                    },
                    size,
                ))
            }
            0x68 => {
                let size = self.push_size();
                let value = if size == Size::B16 {
                    self.imm_value(Size::B16)?
                } else {
                    self.i32()?
                };
                Ok((Op::Push(Operand::Imm(value)), size))
            }
            0x69 | 0x6b => {
                let (reg, rm) = self.modrm()?;
                let src = self.rm_operand(rm, size);
                let factor = if opcode == 0x69 {
                    self.imm_value(size)?
                } else {
                    self.i8()?
                };
                Ok((
                    Op::Imul {
                        dst: Reg::Gpr(reg),
                        src,
                        factor: Some(factor),
                    },
                    size,
                ))
            }
            0x6a => Ok((Op::Push(Operand::Imm(self.i8()?)), self.push_size())),
            // ins, outs: port I/O, #GP in user mode.
            0x6c..=0x6f => Ok((Op::Privileged, Size::B64)),

            0x70..=0x7f => {
                self.no_opsize16("jcc with operand-size prefix")?;
                let rel = self.i8()?;
                Ok((
                    Op::Jcc {
                        cond: Cond::from_index(opcode),
                        rel,
                    },
                    self.stack(),
                ))
            }

            // 82 is an alias of 80 in compatibility mode.
            0x80..=0x83 if opcode != 0x82 || self.compat => {
                let size = if opcode & 1 == 0 { Size::B8 } else { size };
                let (digit, rm) = self.modrm()?;
                let dst = self.rm_operand(rm, size);
                let src = if opcode == 0x81 {
                    self.imm_for(size)?
                } else {
                    Operand::Imm(self.i8()?)
                };
                let alu = AluOp::from_index(digit);
                Ok((Op::Alu { op: alu, dst, src }, size))
            }
            0x82 => Err(DecodeError::Invalid),

            0x84 | 0x85 => {
                let size = if opcode == 0x84 { Size::B8 } else { size };
                let (reg, rm) = self.modrm()?;
                let a = self.rm_operand(rm, size);
                let b = self.reg_operand(reg, size);
                Ok((Op::Test { a, b }, size))
            }
            0x86 | 0x87 => {
                let size = if opcode == 0x86 { Size::B8 } else { size };
                let (reg, rm) = self.modrm()?;
                let a = self.rm_operand(rm, size);
                let b = self.reg_operand(reg, size);
                Ok((Op::Xchg { a, b }, size))
            }

            0x88..=0x8b => {
                let size = if opcode & 1 == 0 { Size::B8 } else { size };
                let (reg, rm) = self.modrm()?;
                let (r, m) = (self.reg_operand(reg, size), self.rm_operand(rm, size));
                let (dst, src) = if opcode & 2 == 0 { (m, r) } else { (r, m) };
                Ok((Op::Mov { dst, src }, size))
            }
            // mov r/m, sreg. CS and SS hold Linux's fixed 64-bit user
            // selectors (__USER_CS, __USER_DS, measured under qemu). The
            // selector is 16 bits in memory, zero-extended in a register.
            0x8c => {
                let (reg, rm) = self.modrm()?;
                // Compatibility mode: Linux's 32-bit user CS (0x23) and its
                // data selector in DS, ES and SS (0x2b).
                let size = match rm {
                    Rm::Mem(_) => Size::B16,
                    Rm::Reg(_) => size,
                };
                let selector = match (reg & 7, self.compat) {
                    (1, false) => 0x33,
                    (1, true) => 0x23,
                    (2, _) => 0x2b,
                    (0 | 3, true) => 0x2b,
                    // 64-bit mode: es, ds, fs and gs hold the null selector
                    // (measured on the Xeon X5675; qemu shows 0x2b in es
                    // and ds). armor never loads them.
                    (0 | 3 | 4 | 5, false) => 0,
                    // fs and gs hold what the program loaded (TLS).
                    (4 | 5, true) => {
                        let segment = if reg & 7 == 4 {
                            Segment::Fs
                        } else {
                            Segment::Gs
                        };
                        let dst = self.rm_operand(rm, size);
                        return Ok((Op::ReadSegment { segment, dst }, size));
                    }
                    _ => return Err(DecodeError::Unsupported("reading es, ds, fs or gs")),
                };
                let dst = self.rm_operand(rm, size);
                Ok((
                    Op::Mov {
                        dst,
                        src: Operand::Imm(selector),
                    },
                    size,
                ))
            }
            // mov sreg, r/m16: in compatibility mode glibc loads gs with
            // the selector of its TLS entry (set_thread_area).
            0x8e if self.compat => {
                let (reg, rm) = self.modrm()?;
                // es, ss, ds have base 0 in Linux's flat model: Default.
                let segment = match reg & 7 {
                    0 | 2 | 3 => Segment::Default,
                    4 => Segment::Fs,
                    5 => Segment::Gs,
                    _ => return Err(DecodeError::Invalid),
                };
                let src = self.rm_operand(rm, Size::B16);
                Ok((Op::LoadSegment { segment, src }, Size::B16))
            }
            0x8e => Err(DecodeError::Unsupported("segment register loads")),
            0x8d => {
                let (reg, rm) = self.modrm()?;
                match rm {
                    Rm::Mem(src) => Ok((
                        Op::Lea {
                            dst: Reg::Gpr(reg),
                            src,
                        },
                        size,
                    )),
                    Rm::Reg(_) => Err(DecodeError::Invalid),
                }
            }
            0x8f => {
                let (digit, rm) = self.modrm()?;
                if digit & 7 != 0 {
                    return Err(DecodeError::Unsupported("AMD XOP"));
                }
                let size = self.push_size();
                Ok((Op::Pop(self.rm_operand(rm, size)), size))
            }

            0x90..=0x97 => {
                let reg = (opcode & 7) | self.rex_bit(REX_B);
                if reg == 0 {
                    // nop (also `pause` with F3, and `xchg rax, rax`).
                    Ok((Op::Nop, size))
                } else {
                    Ok((
                        Op::Xchg {
                            a: Operand::Reg(Reg::Gpr(0)),
                            b: Operand::Reg(Reg::Gpr(reg)),
                        },
                        size,
                    ))
                }
            }
            0x98 => Ok((Op::SignExtendAccumulator, size)),
            0x99 => Ok((Op::SignExtendIntoRdx, size)),
            0x9a | 0xea if self.compat => Err(DecodeError::Unsupported("far call/jmp")),
            0x9a | 0xea => Err(DecodeError::Invalid),
            // fwait: x87 exceptions are always masked, nothing to wait for.
            0x9b => Ok((
                Op::X87 {
                    op: crate::X87Op::Wait,
                    opcode: 0,
                },
                size,
            )),
            0x9c => {
                self.no_opsize16("pushf with operand-size prefix")?;
                Ok((Op::Pushf, self.stack()))
            }
            0x9d => {
                self.no_opsize16("popf with operand-size prefix")?;
                Ok((Op::Popf, self.stack()))
            }
            // Valid in 64-bit mode on CPUs with LAHF-LM, which armor reports.
            0x9e => Ok((Op::Sahf, Size::B8)),
            0x9f => Ok((Op::Lahf, Size::B8)),
            // mov between the accumulator and an absolute address ("moffs"):
            // 8 address bytes, 4 with the address-size prefix.
            0xa0..=0xa3 => {
                let size = if opcode & 1 == 0 { Size::B8 } else { size };
                if self.addr16 {
                    return Err(DecodeError::Unsupported("16-bit addressing"));
                }
                let disp = if self.addr32 {
                    self.i32()? & 0xffff_ffff
                } else {
                    self.i64()?
                };
                let mem = Operand::Mem(Mem {
                    segment: self.segment,
                    base: Base::None,
                    index: None,
                    scale: 1,
                    disp,
                    addr32: self.addr32,
                });
                let accumulator = Operand::Reg(Reg::Gpr(0));
                let (dst, src) = if opcode < 0xa2 {
                    (accumulator, mem)
                } else {
                    (mem, accumulator)
                };
                Ok((Op::Mov { dst, src }, size))
            }

            0xa4..=0xa7 | 0xaa..=0xaf => {
                let size = if opcode & 1 == 0 { Size::B8 } else { size };
                if self.addr32 && !self.compat || self.addr16 {
                    return Err(DecodeError::Unsupported(
                        "string instruction with address-size prefix",
                    ));
                }
                if self.segment != Segment::Default {
                    return Err(DecodeError::Unsupported(
                        "string instruction with segment override",
                    ));
                }
                let op = match opcode & !1 {
                    0xa4 => StringOp::Movs,
                    0xa6 => StringOp::Cmps,
                    0xaa => StringOp::Stos,
                    0xac => StringOp::Lods,
                    _ => StringOp::Scas,
                };
                Ok((
                    Op::String {
                        op,
                        rep: self.rep_prefix(),
                    },
                    size,
                ))
            }
            0xa8 => Ok((
                Op::Test {
                    a: Operand::Reg(Reg::Gpr(0)),
                    b: Operand::Imm(self.i8()?),
                },
                Size::B8,
            )),
            0xa9 => Ok((
                Op::Test {
                    a: Operand::Reg(Reg::Gpr(0)),
                    b: self.imm_for(size)?,
                },
                size,
            )),

            0xb0..=0xb7 => {
                let dst = self.byte_reg((opcode & 7) | self.rex_bit(REX_B));
                let src = Operand::Imm(self.i8()?);
                Ok((
                    Op::Mov {
                        dst: Operand::Reg(dst),
                        src,
                    },
                    Size::B8,
                ))
            }
            0xb8..=0xbf => {
                let dst = Operand::Reg(Reg::Gpr((opcode & 7) | self.rex_bit(REX_B)));
                // The only x86-64 instruction with a 64-bit immediate.
                let src = match size {
                    Size::B64 => Operand::Imm(self.i64()?),
                    _ => self.imm_for(size)?,
                };
                Ok((Op::Mov { dst, src }, size))
            }

            0xc0 | 0xc1 | 0xd0..=0xd3 => {
                let size = if opcode & 1 == 0 { Size::B8 } else { size };
                let (digit, rm) = self.modrm()?;
                let dst = self.rm_operand(rm, size);
                let count = match opcode {
                    0xc0 | 0xc1 => ShiftCount::Imm(self.u8()?),
                    0xd0 | 0xd1 => ShiftCount::One,
                    _ => ShiftCount::Cl,
                };
                let op = match digit & 7 {
                    0 => ShiftOp::Rol,
                    1 => ShiftOp::Ror,
                    2 => ShiftOp::Rcl,
                    3 => ShiftOp::Rcr,
                    // /6 is an undocumented alias of shl.
                    4 | 6 => ShiftOp::Shl,
                    5 => ShiftOp::Shr,
                    _ => ShiftOp::Sar,
                };
                Ok((Op::Shift { op, dst, count }, size))
            }
            0xc2 => {
                let pop = u16::from_le_bytes(self.le()?);
                Ok((Op::Ret { pop }, self.stack()))
            }
            0xc3 => Ok((Op::Ret { pop: 0 }, self.stack())),
            // les/lds in compatibility mode, unless the next byte makes
            // them VEX.
            0xc4 | 0xc5 if self.compat && self.peek_modrm()? >> 6 != 3 => {
                Err(DecodeError::Unsupported("les/lds"))
            }
            0xc4 | 0xc5 => Err(DecodeError::Unsupported("AVX (VEX)")),
            0xc6 | 0xc7 => {
                let size = if opcode == 0xc6 { Size::B8 } else { size };
                let (digit, rm) = self.modrm()?;
                if digit & 7 != 0 {
                    return Err(DecodeError::Unsupported("TSX (xabort/xbegin)"));
                }
                let dst = self.rm_operand(rm, size);
                let src = if size == Size::B8 {
                    Operand::Imm(self.i8()?)
                } else {
                    self.imm_for(size)?
                };
                Ok((Op::Mov { dst, src }, size))
            }
            0xc8 => {
                self.no_opsize16("enter with operand-size prefix")?;
                let alloc = u16::from_le_bytes(self.le()?);
                let level = self.u8()?;
                Ok((Op::Enter { alloc, level }, self.stack()))
            }
            0xc9 => Ok((Op::Leave, self.stack())),
            0xca | 0xcb | 0xcf => Err(DecodeError::Unsupported("far return / iret")),
            0xcc => Ok((Op::Int3, Size::B64)),
            0xcd => match self.u8()? {
                3 => Ok((Op::Int3, Size::B64)),
                0x80 if self.compat => Ok((Op::Int80, Size::B32)),
                0x80 => Err(DecodeError::Unsupported("int 0x80 (32-bit syscalls)")),
                // Linux gives user mode no other gates: #GP.
                _ => Ok((Op::Privileged, Size::B64)),
            },
            // into, aam, aad, salc: compatibility mode only.
            0xce if self.compat => Err(DecodeError::Unsupported("into")),
            0xd4 | 0xd5 if self.compat => {
                self.u8()?;
                Err(DecodeError::Unsupported("aam/aad"))
            }
            0xd6 if self.compat => Err(DecodeError::Unsupported("salc")),
            0xce | 0xd4 | 0xd5 | 0xd6 => Err(DecodeError::Invalid),
            0xd7 => {
                if self.addr32 && !self.compat || self.addr16 {
                    return Err(DecodeError::Unsupported("xlat with address-size prefix"));
                }
                Ok((Op::Xlat(self.segment), Size::B8))
            }
            0xd8..=0xdf => Ok((crate::x87::decode(self, opcode)?, size)),

            0xe0..=0xe3 => {
                if self.addr32 && !self.compat || self.addr16 {
                    return Err(DecodeError::Unsupported(
                        "loop/jrcxz with address-size prefix",
                    ));
                }
                let kind = match opcode {
                    0xe0 => LoopKind::Loopne,
                    0xe1 => LoopKind::Loope,
                    0xe2 => LoopKind::Loop,
                    _ => LoopKind::Jrcxz,
                };
                Ok((
                    Op::Loop {
                        kind,
                        rel: self.i8()?,
                    },
                    self.stack(),
                ))
            }
            // in, out with an immediate port, then with the port in dx:
            // #GP in user mode.
            0xe4..=0xe7 => {
                self.u8()?;
                Ok((Op::Privileged, Size::B64))
            }
            0xec..=0xef => Ok((Op::Privileged, Size::B64)),
            0xe8 => {
                self.no_opsize16("call with operand-size prefix")?;
                Ok((Op::Call { rel: self.i32()? }, self.stack()))
            }
            0xe9 => {
                self.no_opsize16("jmp with operand-size prefix")?;
                Ok((Op::Jmp { rel: self.i32()? }, self.stack()))
            }
            0xeb => {
                self.no_opsize16("jmp with operand-size prefix")?;
                Ok((Op::Jmp { rel: self.i8()? }, self.stack()))
            }

            0xf1 => Ok((Op::Int1, Size::B64)),
            0xf4 => Ok((Op::Privileged, Size::B64)),
            0xf5 => Ok((Op::Flag(FlagOp::Cmc), Size::B64)),
            0xf6 | 0xf7 => {
                let size = if opcode == 0xf6 { Size::B8 } else { size };
                let (digit, rm) = self.modrm()?;
                let operand = self.rm_operand(rm, size);
                let op = match digit & 7 {
                    // /1 is an undocumented alias of test.
                    0 | 1 => {
                        let b = if size == Size::B8 {
                            Operand::Imm(self.i8()?)
                        } else {
                            self.imm_for(size)?
                        };
                        Op::Test { a: operand, b }
                    }
                    2 => Op::Unary {
                        op: UnaryOp::Not,
                        dst: operand,
                    },
                    3 => Op::Unary {
                        op: UnaryOp::Neg,
                        dst: operand,
                    },
                    4 => Op::MulWide {
                        signed: false,
                        src: operand,
                    },
                    5 => Op::MulWide {
                        signed: true,
                        src: operand,
                    },
                    6 => Op::Div {
                        signed: false,
                        src: operand,
                    },
                    _ => Op::Div {
                        signed: true,
                        src: operand,
                    },
                };
                Ok((op, size))
            }
            0xf8 => Ok((Op::Flag(FlagOp::Clc), Size::B64)),
            0xf9 => Ok((Op::Flag(FlagOp::Stc), Size::B64)),
            // cli, sti.
            0xfa | 0xfb => Ok((Op::Privileged, Size::B64)),
            0xfc => Ok((Op::Flag(FlagOp::Cld), Size::B64)),
            0xfd => Ok((Op::Flag(FlagOp::Std), Size::B64)),
            0xfe => {
                let (digit, rm) = self.modrm()?;
                let dst = self.rm_operand(rm, Size::B8);
                let op = match digit & 7 {
                    0 => UnaryOp::Inc,
                    1 => UnaryOp::Dec,
                    _ => return Err(DecodeError::Invalid),
                };
                Ok((Op::Unary { op, dst }, Size::B8))
            }
            0xff => {
                let (digit, rm) = self.modrm()?;
                match digit & 7 {
                    0 | 1 => {
                        let op = if digit & 7 == 0 {
                            UnaryOp::Inc
                        } else {
                            UnaryOp::Dec
                        };
                        Ok((
                            Op::Unary {
                                op,
                                dst: self.rm_operand(rm, size),
                            },
                            size,
                        ))
                    }
                    2 => {
                        self.no_opsize16("call with operand-size prefix")?;
                        let stack = self.stack();
                        Ok((Op::CallIndirect(self.rm_operand(rm, stack)), stack))
                    }
                    4 => {
                        self.no_opsize16("jmp with operand-size prefix")?;
                        let stack = self.stack();
                        Ok((Op::JmpIndirect(self.rm_operand(rm, stack)), stack))
                    }
                    6 => {
                        let size = self.push_size();
                        Ok((Op::Push(self.rm_operand(rm, size)), size))
                    }
                    3 | 5 => Err(DecodeError::Unsupported("far call/jmp")),
                    _ => Err(DecodeError::Invalid),
                }
            }

            _ => Err(DecodeError::Unsupported("opcode")),
        }
    }

    fn two_byte(&mut self) -> Result<(Op, Size), DecodeError> {
        let opcode = self.u8()?;
        let size = self.operand_size();
        match opcode {
            // syscall: #UD in compatibility mode on Intel CPUs.
            0x05 if self.compat => Err(DecodeError::Invalid),
            0x05 => Ok((Op::Syscall, Size::B64)),
            0x0b => Err(DecodeError::Invalid), // ud2
            // prefetch hints (0D, 18) and the reserved hint NOPs (19–1F),
            // including nop r/m (1F /0) and endbr64 (F3 0F 1E FA).
            0x0d | 0x18..=0x1f => {
                self.modrm()?;
                Ok((Op::Nop, size))
            }
            0x38 => crate::maps::decode_38(self),
            0x3a => crate::maps::decode_3a(self),
            0x31 => Ok((Op::Rdtsc, Size::B64)),
            0x40..=0x4f => {
                let (reg, rm) = self.modrm()?;
                Ok((
                    Op::Cmov {
                        cond: Cond::from_index(opcode),
                        dst: Reg::Gpr(reg),
                        src: self.rm_operand(rm, size),
                    },
                    size,
                ))
            }
            0x80..=0x8f => {
                self.no_opsize16("jcc with operand-size prefix")?;
                let rel = self.i32()?;
                Ok((
                    Op::Jcc {
                        cond: Cond::from_index(opcode),
                        rel,
                    },
                    self.stack(),
                ))
            }
            0x90..=0x9f => {
                let (_, rm) = self.modrm()?;
                Ok((
                    Op::Setcc {
                        cond: Cond::from_index(opcode),
                        dst: self.rm_operand(rm, Size::B8),
                    },
                    Size::B8,
                ))
            }
            0xa2 => Ok((Op::Cpuid, Size::B64)),
            0xa3 | 0xab | 0xb3 | 0xbb => {
                let op = match opcode {
                    0xa3 => BitTestOp::Bt,
                    0xab => BitTestOp::Bts,
                    0xb3 => BitTestOp::Btr,
                    _ => BitTestOp::Btc,
                };
                let (reg, rm) = self.modrm()?;
                Ok((
                    Op::BitTest {
                        op,
                        dst: self.rm_operand(rm, size),
                        offset: Operand::Reg(Reg::Gpr(reg)),
                    },
                    size,
                ))
            }
            0xba => {
                let (digit, rm) = self.modrm()?;
                let op = match digit & 7 {
                    4 => BitTestOp::Bt,
                    5 => BitTestOp::Bts,
                    6 => BitTestOp::Btr,
                    7 => BitTestOp::Btc,
                    _ => return Err(DecodeError::Invalid),
                };
                let dst = self.rm_operand(rm, size);
                let offset = Operand::Imm(i64::from(self.u8()?));
                Ok((Op::BitTest { op, dst, offset }, size))
            }
            0xa4 | 0xa5 | 0xac | 0xad => {
                let (reg, rm) = self.modrm()?;
                let dst = self.rm_operand(rm, size);
                let count = if opcode & 1 == 0 {
                    ShiftCount::Imm(self.u8()?)
                } else {
                    ShiftCount::Cl
                };
                Ok((
                    Op::DoubleShift {
                        left: opcode < 0xac,
                        dst,
                        src: Reg::Gpr(reg),
                        count,
                    },
                    size,
                ))
            }
            0xae => {
                let (digit, rm) = self.modrm()?;
                match (rm, digit & 7) {
                    // lfence, mfence, sfence.
                    (Rm::Reg(_), 5..=7) => Ok((Op::Fence, size)),
                    (Rm::Mem(mem), digit @ 0..=3) => {
                        let op = [
                            StateOp::Fxsave,
                            StateOp::Fxrstor,
                            StateOp::Ldmxcsr,
                            StateOp::Stmxcsr,
                        ][usize::from(digit)];
                        Ok((Op::State { op, mem }, size))
                    }
                    (Rm::Mem(mem), 7) => Ok((
                        Op::State {
                            op: StateOp::Clflush,
                            mem,
                        },
                        size,
                    )),
                    _ => Err(DecodeError::Unsupported(
                        "0F AE group (fxsave, ldmxcsr, ...)",
                    )),
                }
            }
            0xaf => {
                let (reg, rm) = self.modrm()?;
                Ok((
                    Op::Imul {
                        dst: Reg::Gpr(reg),
                        src: self.rm_operand(rm, size),
                        factor: None,
                    },
                    size,
                ))
            }
            0xb0 | 0xb1 | 0xc0 | 0xc1 => {
                let size = if opcode & 1 == 0 { Size::B8 } else { size };
                let (reg, rm) = self.modrm()?;
                let dst = self.rm_operand(rm, size);
                let Operand::Reg(src) = self.reg_operand(reg, size) else {
                    unreachable!("reg_operand returns a register")
                };
                let op = if opcode >= 0xc0 {
                    Op::Xadd { dst, src }
                } else {
                    Op::Cmpxchg { dst, src }
                };
                Ok((op, size))
            }
            0xb6 | 0xb7 | 0xbe | 0xbf => {
                let src_size = if opcode & 1 == 0 { Size::B8 } else { Size::B16 };
                let (reg, rm) = self.modrm()?;
                Ok((
                    Op::Extend {
                        signed: opcode >= 0xbe,
                        dst: Reg::Gpr(reg),
                        src: self.rm_operand(rm, src_size),
                        src_size,
                    },
                    size,
                ))
            }
            // popcnt (F3); without F3, jmpe, which does not exist in
            // 64-bit mode.
            0xb8 => {
                if self.rep != Some(0xf3) {
                    return Err(DecodeError::Invalid);
                }
                let (reg, rm) = self.modrm()?;
                Ok((
                    Op::Popcnt {
                        dst: Reg::Gpr(reg),
                        src: self.rm_operand(rm, size),
                    },
                    size,
                ))
            }
            // bsf/bsr. With F3 these encode tzcnt/lzcnt, which run as
            // bsf/bsr on CPUs without BMI1/LZCNT, and armor reports neither.
            0xbc | 0xbd => {
                let (reg, rm) = self.modrm()?;
                Ok((
                    Op::BitScan {
                        reverse: opcode == 0xbd,
                        dst: Reg::Gpr(reg),
                        src: self.rm_operand(rm, size),
                    },
                    size,
                ))
            }
            0xc3 => {
                // movnti: non-temporal store, memory destination only.
                let (reg, rm) = self.modrm()?;
                match rm {
                    Rm::Mem(m) => Ok((
                        Op::Mov {
                            dst: Operand::Mem(m),
                            src: Operand::Reg(Reg::Gpr(reg)),
                        },
                        size,
                    )),
                    Rm::Reg(_) => Err(DecodeError::Invalid),
                }
            }
            0xc7 => {
                let (digit, rm) = self.modrm()?;
                match (digit & 7, rm) {
                    // cmpxchg16b with REX.W (CX16, which armor reports).
                    (1, Rm::Mem(mem)) => {
                        let size = if self.rex & REX_W != 0 {
                            Size::B64
                        } else {
                            Size::B32
                        };
                        Ok((Op::CmpxchgWide(mem), size))
                    }
                    (1, Rm::Reg(_)) => Err(DecodeError::Invalid),
                    _ => Err(DecodeError::Unsupported(
                        "0F C7 group (rdrand, rdseed, rdpid)",
                    )),
                }
            }
            0xc8..=0xcf => {
                if size == Size::B16 {
                    return Err(DecodeError::Unsupported("bswap with 16-bit operand"));
                }
                let reg = (opcode & 7) | self.rex_bit(REX_B);
                Ok((Op::Bswap(Reg::Gpr(reg)), size))
            }
            0x10..=0x17
            | 0x28..=0x2f
            | 0x50..=0x5f
            | 0x60..=0x77
            | 0x7c..=0x7f
            | 0xc2
            | 0xc4..=0xc6
            | 0xd0..=0xff => crate::sse::decode(self, opcode),
            _ => Err(DecodeError::Unsupported("two-byte opcode (0F xx)")),
        }
    }

    /// Legacy prefixes and REX. A REX byte only counts if it immediately
    /// precedes the opcode, so a later legacy prefix cancels it.
    fn prefixes(&mut self) -> Result<(), DecodeError> {
        loop {
            let byte = *self.bytes.get(self.pos).ok_or(DecodeError::Truncated)?;
            match byte {
                0x66 => self.opsize16 = true,
                0x67 if self.compat => self.addr16 = true,
                0x67 => self.addr32 = true,
                0x64 => self.segment = Segment::Fs,
                0x65 => self.segment = Segment::Gs,
                // CS/SS/DS/ES overrides are ignored in 64-bit mode (3E is
                // also the `notrack` prefix of indirect branches).
                0x26 | 0x2e | 0x36 | 0x3e => {}
                0xf0 => self.lock = true,
                0xf2 | 0xf3 => self.rep = Some(byte),
                // In compatibility mode 40–4F are inc and dec.
                0x40..=0x4f if !self.compat => {
                    self.u8()?;
                    self.rex = byte & 0x0f;
                    self.has_rex = true;
                    continue;
                }
                _ => return Ok(()),
            }
            self.u8()?;
            self.rex = 0;
            self.has_rex = false;
        }
    }

    /// The width of stack and branch operations: 64 bits, 32 in
    /// compatibility mode.
    fn stack(&self) -> Size {
        if self.compat { Size::B32 } else { Size::B64 }
    }

    /// The width of a push or pop: 2 bytes with the operand-size prefix
    /// (and no REX.W), else the stack's.
    fn push_size(&self) -> Size {
        if self.opsize16 && !self.rex_w() {
            Size::B16
        } else {
            self.stack()
        }
    }

    pub(crate) fn operand_size(&self) -> Size {
        if self.rex & REX_W != 0 {
            Size::B64
        } else if self.opsize16 {
            Size::B16
        } else {
            Size::B32
        }
    }

    pub(crate) fn rex_w(&self) -> bool {
        self.rex & REX_W != 0
    }

    /// Mandatory prefix of an SSE instruction: F3 or F2 if present (the last
    /// one wins), otherwise 66, otherwise none (0).
    pub(crate) fn sse_prefix(&self) -> u8 {
        match self.rep {
            Some(p) => p,
            None if self.opsize16 => 0x66,
            None => 0,
        }
    }

    fn rep_prefix(&self) -> Option<Rep> {
        match self.rep {
            Some(0xf3) => Some(Rep::Rep),
            Some(_) => Some(Rep::Repne),
            None => None,
        }
    }

    /// Rejects the operand-size prefix where armor implements only the
    /// 64-bit form; REX.W overrides it (compilers pad TLS calls as
    /// `66 66 48 e8`).
    fn no_opsize16(&self, what: &'static str) -> Result<(), DecodeError> {
        if self.opsize16 && !self.rex_w() {
            Err(DecodeError::Unsupported(what))
        } else {
            Ok(())
        }
    }

    /// 8 if `bit` is set in REX, else 0: the high bit of a register number.
    fn rex_bit(&self, bit: u8) -> u8 {
        if self.rex & bit != 0 { 8 } else { 0 }
    }

    pub(crate) fn modrm(&mut self) -> Result<(u8, Rm), DecodeError> {
        let modrm = self.u8()?;
        let md = modrm >> 6;
        if self.addr16 && md != 3 {
            return Err(DecodeError::Unsupported("16-bit addressing"));
        }
        let reg = ((modrm >> 3) & 7) | self.rex_bit(REX_R);
        let rm = modrm & 7;
        if md == 3 {
            return Ok((reg, Rm::Reg(rm | self.rex_bit(REX_B))));
        }

        let mut mem = Mem {
            segment: self.segment,
            base: Base::Gpr(rm | self.rex_bit(REX_B)),
            index: None,
            scale: 1,
            disp: 0,
            addr32: self.addr32,
        };
        if rm == 4 {
            // SIB follows, also for r12 (rm=100 with REX.B).
            let sib = self.u8()?;
            let index = ((sib >> 3) & 7) | self.rex_bit(REX_X);
            mem.scale = 1 << (sib >> 6);
            // Index 100 means "no index", unless REX.X makes it r12.
            mem.index = (index != 4).then_some(index);
            let base = sib & 7;
            if base == 5 && md == 0 {
                mem.base = Base::None;
                mem.disp = self.i32()?;
            } else {
                mem.base = Base::Gpr(base | self.rex_bit(REX_B));
            }
        } else if rm == 5 && md == 0 {
            // RIP-relative, also for r13 (rm=101 with REX.B); an absolute
            // address in compatibility mode.
            mem.base = if self.compat { Base::None } else { Base::Rip };
            mem.disp = self.i32()?;
        }
        match md {
            1 => mem.disp = self.i8()?,
            2 => mem.disp = self.i32()?,
            _ => {}
        }
        Ok((reg, Rm::Mem(mem)))
    }

    pub(crate) fn rm_operand(&self, rm: Rm, size: Size) -> Operand {
        match rm {
            Rm::Reg(n) => self.reg_operand(n, size),
            Rm::Mem(mem) => Operand::Mem(mem),
        }
    }

    fn reg_operand(&self, n: u8, size: Size) -> Operand {
        Operand::Reg(if size == Size::B8 {
            self.byte_reg(n)
        } else {
            Reg::Gpr(n)
        })
    }

    /// Byte registers 4–7 are AH/CH/DH/BH without REX, SPL/BPL/SIL/DIL with it.
    fn byte_reg(&self, n: u8) -> Reg {
        if !self.has_rex && (4..8).contains(&n) {
            Reg::HighByte(n - 4)
        } else {
            Reg::Gpr(n)
        }
    }

    /// Immediate of an `Iz` operand: 16 bits for 16-bit operands, otherwise
    /// 32 bits (sign-extended for 64-bit operands).
    fn imm_for(&mut self, size: Size) -> Result<Operand, DecodeError> {
        Ok(Operand::Imm(self.imm_value(size)?))
    }

    fn imm_value(&mut self, size: Size) -> Result<i64, DecodeError> {
        match size {
            Size::B8 => self.i8(),
            Size::B16 => self.i16(),
            Size::B32 | Size::B64 => self.i32(),
        }
    }

    /// The next byte (the ModRM byte, for the x87 FOP), not consumed.
    pub(crate) fn peek_modrm(&self) -> Result<u8, DecodeError> {
        if self.pos >= MAX_INSTRUCTION_LEN {
            return Err(DecodeError::TooLong);
        }
        self.bytes
            .get(self.pos)
            .copied()
            .ok_or(DecodeError::Truncated)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        if self.pos >= MAX_INSTRUCTION_LEN {
            return Err(DecodeError::TooLong);
        }
        let byte = *self.bytes.get(self.pos).ok_or(DecodeError::Truncated)?;
        self.pos += 1;
        Ok(byte)
    }

    fn le<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut out = [0u8; N];
        for byte in &mut out {
            *byte = self.u8()?;
        }
        Ok(out)
    }

    fn i8(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from(self.u8()? as i8))
    }
    fn i16(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from(i16::from_le_bytes(self.le()?)))
    }
    fn i32(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from(i32::from_le_bytes(self.le()?)))
    }
    fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_le_bytes(self.le()?))
    }
}
