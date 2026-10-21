//! A length-only decoder for all of x86-64 (roadmap M4): the length of any
//! instruction, and the instruction-set extension it belongs to, whether
//! or not armor executes it. It reads prefixes, the opcode map (one byte,
//! 0F, 0F 38, 0F 3A, VEX, EVEX, XOP, 3DNow!), ModRM, SIB, displacement
//! and immediate, from the opcode maps of the Intel SDM (vol. 2, appendix
//! A) and AMD's APM (XOP, 3DNow!). `decode` stays the decoder armor
//! executes with; `cargo xtask decoder-check` compares both with objdump,
//! and `armor inspect` lists the extensions a program uses.

use core::fmt;

use crate::{DecodeError, MAX_INSTRUCTION_LEN, Mode};

/// The instruction-set extension of an instruction (as far as the
/// encoding tells: VEX and EVEX are not split into AVX, AVX2, FMA, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Extension {
    /// The integer instruction set (and system instructions).
    Base,
    X87,
    Mmx,
    /// SSE and SSE2, part of every x86-64 CPU.
    Sse,
    Sse3,
    Ssse3,
    Sse41,
    Sse42,
    Popcnt,
    /// XSAVE: xgetbv, xsetbv, xsave, xrstor, xsaveopt, xsavec, xsaves,
    /// xrstors.
    Xsave,
    Aes,
    Pclmul,
    Sha,
    Amd3dNow,
    /// VEX-encoded: AVX, AVX2, FMA, F16C, BMI1/2, …
    Vex,
    /// EVEX-encoded: AVX-512, AVX10.
    Evex,
    /// AMD XOP.
    Xop,
    /// VIA PadLock (xstore, xcrypt*, xsha*, montmul).
    Padlock,
}

impl Extension {
    /// Whether armor executes instructions of this extension (some
    /// instructions excepted: `docs/tech/x86-64-encoding.md`).
    pub fn implemented(self) -> bool {
        matches!(
            self,
            Extension::Base
                | Extension::X87
                | Extension::Mmx
                | Extension::Sse
                | Extension::Sse3
                | Extension::Ssse3
                | Extension::Sse41
                | Extension::Sse42
                | Extension::Popcnt
                | Extension::Xsave
        )
    }
}

impl fmt::Display for Extension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Extension::Base => "x86-64 base",
            Extension::X87 => "x87",
            Extension::Mmx => "MMX",
            Extension::Sse => "SSE/SSE2",
            Extension::Sse3 => "SSE3",
            Extension::Ssse3 => "SSSE3",
            Extension::Sse41 => "SSE4.1",
            Extension::Sse42 => "SSE4.2",
            Extension::Popcnt => "POPCNT",
            Extension::Xsave => "XSAVE",
            Extension::Aes => "AES-NI",
            Extension::Pclmul => "PCLMULQDQ",
            Extension::Sha => "SHA",
            Extension::Amd3dNow => "3DNow!",
            Extension::Vex => "VEX (AVX, AVX2, FMA, BMI…)",
            Extension::Evex => "EVEX (AVX-512)",
            Extension::Xop => "XOP",
            Extension::Padlock => "VIA PadLock",
        })
    }
}

/// The length of an instruction and its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Length {
    pub len: usize,
    pub extension: Extension,
}

/// The length and extension of the instruction at the start of `bytes`.
/// `Invalid` for encodings that raise #UD in 64-bit mode whatever the CPU
/// (removed one-byte opcodes, VEX after a 66/F2/F3/F0/REX prefix);
/// `Truncated` and `TooLong` as for `decode`.
pub fn length(bytes: &[u8]) -> Result<Length, DecodeError> {
    length_in(bytes, Mode::Long)
}

/// `length` for code running in `mode` (ADR-0029): in compatibility mode
/// 40–4F are instructions, not REX; addresses are 32 bits (16 with 67);
/// the opcodes removed in 64-bit mode exist.
pub fn length_in(bytes: &[u8], mode: Mode) -> Result<Length, DecodeError> {
    let compat = mode == Mode::Compat32;
    let mut r = Reader {
        bytes,
        pos: 0,
        addr16: false,
    };
    let mut opsize16 = false;
    let mut addr32 = false;
    let mut rep = None;
    let mut rex = 0u8;
    // Prefixes; a REX prefix counts only right before the opcode.
    let opcode = loop {
        let b = r.byte()?;
        match b {
            0x66 => opsize16 = true,
            0x67 => addr32 = true,
            0xf2 | 0xf3 => rep = Some(b),
            0xf0 | 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 => {}
            0x40..=0x4f if !compat => {
                rex = b;
                continue;
            }
            _ => break b,
        }
        rex = 0;
    };
    let mandatory = rep.unwrap_or(if opsize16 { 0x66 } else { 0 });
    // In compatibility mode 67 selects 16-bit addresses (another ModRM).
    r.addr16 = compat && addr32;
    // Immediates of the operand size: 16 bits with 66, unless REX.W
    // overrides it (64-bit operands take a 32-bit immediate).
    let z = if opsize16 && rex & 8 == 0 { 2 } else { 4 };
    let extension = match opcode {
        // Compatibility mode: les, lds, bound when the next byte is not
        // shaped like VEX or EVEX (ModRM.mod != 11).
        0xc4 | 0xc5 | 0x62 if compat && r.peek()? >> 6 != 3 => {
            r.modrm()?;
            Extension::Base
        }
        // VEX, EVEX: no 66, F2, F3, F0 or REX before them.
        0xc4 | 0xc5 | 0x62 => {
            if opsize16 || rep.is_some() || rex != 0 || has_lock(bytes, r.pos) {
                return Err(DecodeError::Invalid);
            }
            return r.vex_or_evex(opcode);
        }
        // XOP (8F with a map field of 8 or more), otherwise pop r/m.
        0x8f if r.peek()? & 0x1f >= 8 => return r.xop(),
        0x0f => return r.two_byte(mandatory, compat && opsize16),
        // Opcodes removed in 64-bit mode, present in compatibility mode:
        // push/pop sreg, BCD, pusha/popa, into, salc, inc/dec r32; arpl
        // and the 82 alias with ModRM; aam/aad with a byte; far call/jmp
        // with a pointer.
        0x06
        | 0x07
        | 0x0e
        | 0x16
        | 0x17
        | 0x1e
        | 0x1f
        | 0x27
        | 0x2f
        | 0x37
        | 0x3f
        | 0x60
        | 0x61
        | 0xce
        | 0xd6
        | 0x40..=0x4f
            if compat =>
        {
            Extension::Base
        }
        0x63 if compat => {
            r.modrm()?;
            Extension::Base
        }
        0x82 if compat => {
            r.modrm()?;
            r.skip(1)?;
            Extension::Base
        }
        0xd4 | 0xd5 if compat => {
            r.skip(1)?;
            Extension::Base
        }
        0x9a | 0xea if compat => {
            r.skip(if opsize16 { 4 } else { 6 })?;
            Extension::Base
        }
        // Opcodes removed in 64-bit mode.
        0x06
        | 0x07
        | 0x0e
        | 0x16
        | 0x17
        | 0x1e
        | 0x1f
        | 0x27
        | 0x2f
        | 0x37
        | 0x3f
        | 0x60
        | 0x61
        | 0x82
        | 0x9a
        | 0xce
        | 0xd4..=0xd6
        | 0xea => {
            return Err(DecodeError::Invalid);
        }
        // ALU r/m forms.
        0x00..=0x3f if opcode & 7 < 4 => {
            r.modrm()?;
            Extension::Base
        }
        // ALU al/eax, imm.
        0x00..=0x3f if opcode & 7 == 4 => {
            r.skip(1)?;
            Extension::Base
        }
        0x00..=0x3f => {
            r.skip(z)?;
            Extension::Base
        }
        0x50..=0x5f | 0x6c..=0x6f | 0x90..=0x99 | 0x9c..=0x9f | 0xa4..=0xa7 | 0xaa..=0xaf => {
            Extension::Base
        }
        // fwait.
        0x9b => Extension::X87,
        0x63 | 0x84..=0x8f | 0xd0..=0xd3 | 0xfe | 0xff => {
            r.modrm()?;
            Extension::Base
        }
        0x68 => {
            r.skip(z)?;
            Extension::Base
        }
        0x69 | 0x81 | 0xc7 => {
            r.modrm()?;
            r.skip(z)?;
            Extension::Base
        }
        0x6a | 0x70..=0x7f | 0xa8 | 0xb0..=0xb7 | 0xcd | 0xe0..=0xe7 | 0xeb => {
            r.skip(1)?;
            Extension::Base
        }
        0x6b | 0x80 | 0x83 | 0xc0 | 0xc1 | 0xc6 => {
            r.modrm()?;
            r.skip(1)?;
            Extension::Base
        }
        // mov al/eax/rax, moffs and back: a 64-bit address (32 with 67).
        0xa0..=0xa3 => {
            let size = match (compat, addr32) {
                (false, false) => 8,
                (false, true) | (true, false) => 4,
                (true, true) => 2,
            };
            r.skip(size)?;
            Extension::Base
        }
        0xa9 => {
            r.skip(z)?;
            Extension::Base
        }
        // mov r, imm: 64 bits with REX.W.
        0xb8..=0xbf => {
            r.skip(if rex & 8 != 0 { 8 } else { z })?;
            Extension::Base
        }
        0xc2 | 0xca => {
            r.skip(2)?;
            Extension::Base
        }
        0xc3
        | 0xc9
        | 0xcb
        | 0xcc
        | 0xcf
        | 0xd7
        | 0xec..=0xef
        | 0xf1
        | 0xf4
        | 0xf5
        | 0xf8..=0xfd => Extension::Base,
        // enter imm16, imm8.
        0xc8 => {
            r.skip(3)?;
            Extension::Base
        }
        0xd8..=0xdf => {
            r.modrm()?;
            Extension::X87
        }
        // call/jmp rel32 (66 does not shorten them in 64-bit mode on
        // Intel CPUs).
        // (In compatibility mode 66 makes them rel16.)
        0xe8 | 0xe9 => {
            r.skip(if compat && opsize16 { 2 } else { 4 })?;
            Extension::Base
        }
        // Group 3: test r/m, imm for /0 and /1.
        0xf6 | 0xf7 => {
            let reg = r.modrm()?;
            if reg < 2 {
                r.skip(if opcode == 0xf6 { 1 } else { z })?;
            }
            Extension::Base
        }
        // Prefixes are consumed above.
        _ => unreachable!("prefix byte {opcode:#x} after the prefixes"),
    };
    r.finish(extension)
}

/// Whether a LOCK prefix is among the prefixes before `end`.
fn has_lock(bytes: &[u8], end: usize) -> bool {
    bytes[..end - 1].contains(&0xf0)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// 16-bit addressing (67 in compatibility mode).
    addr16: bool,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, DecodeError> {
        let b = self.peek()?;
        self.pos += 1;
        Ok(b)
    }

    fn peek(&self) -> Result<u8, DecodeError> {
        if self.pos >= MAX_INSTRUCTION_LEN {
            return Err(DecodeError::TooLong);
        }
        self.bytes
            .get(self.pos)
            .copied()
            .ok_or(DecodeError::Truncated)
    }

    fn skip(&mut self, n: usize) -> Result<(), DecodeError> {
        self.pos += n;
        if self.pos > MAX_INSTRUCTION_LEN {
            Err(DecodeError::TooLong)
        } else if self.pos > self.bytes.len() {
            Err(DecodeError::Truncated)
        } else {
            Ok(())
        }
    }

    fn finish(&self, extension: Extension) -> Result<Length, DecodeError> {
        Ok(Length {
            len: self.pos,
            extension,
        })
    }

    /// Skips ModRM, SIB and displacement; returns ModRM's reg field. The
    /// layout is the same with 32-bit addresses (67).
    fn modrm(&mut self) -> Result<u8, DecodeError> {
        let modrm = self.byte()?;
        let (mode, reg, rm) = (modrm >> 6, (modrm >> 3) & 7, modrm & 7);
        if self.addr16 {
            // 16-bit forms: no SIB; disp16 alone for mod 00 rm 110.
            match (mode, rm) {
                (0, 6) | (2, _) => self.skip(2)?,
                (1, _) => self.skip(1)?,
                _ => {}
            }
            return Ok(reg);
        }
        if mode != 3 {
            if rm == 4 {
                let sib = self.byte()?;
                if mode == 0 && sib & 7 == 5 {
                    self.skip(4)?;
                }
            } else if mode == 0 && rm == 5 {
                // RIP-relative.
                self.skip(4)?;
            }
            match mode {
                1 => self.skip(1)?,
                2 => self.skip(4)?,
                _ => {}
            }
        }
        Ok(reg)
    }

    /// 0F xx, and the maps behind it.
    fn two_byte(&mut self, prefix: u8, rel16: bool) -> Result<Length, DecodeError> {
        let opcode = self.byte()?;
        let extension = match opcode {
            0x38 => return self.map_38(prefix),
            0x3a => return self.map_3a(prefix),
            0x04 | 0x0a | 0x0c | 0x25 | 0x27 | 0x36 | 0x39 | 0x3b..=0x3f | 0x7a | 0x7b => {
                return Err(DecodeError::Invalid);
            }
            // mov to and from control, debug (and, on old CPUs, test)
            // registers: the mod field is ignored, always a register.
            0x20..=0x24 | 0x26 => {
                self.skip(1)?;
                Extension::Base
            }
            // VIA PadLock: a ModRM byte naming the operation.
            0xa6 | 0xa7 => {
                self.modrm()?;
                Extension::Padlock
            }
            // 3DNow!: ModRM, then the operation as a trailing byte.
            0x0f => {
                self.modrm()?;
                self.skip(1)?;
                Extension::Amd3dNow
            }
            0x0e => Extension::Amd3dNow,
            0x05..=0x09 | 0x0b | 0x30..=0x35 | 0x37 | 0xa0..=0xa2 | 0xa8..=0xaa | 0xc8..=0xcf => {
                Extension::Base
            }
            0x77 => Extension::Mmx,
            // xgetbv, xsetbv; the rest of the group is system
            // instructions.
            0x01 => {
                let modrm = self.peek()?;
                self.modrm()?;
                if prefix == 0 && matches!(modrm, 0xd0 | 0xd1) {
                    Extension::Xsave
                } else {
                    Extension::Base
                }
            }
            // xsave, xrstor, xsaveopt (no mandatory prefix, memory); 0F
            // C7: xrstors, xsavec, xsaves.
            0xae | 0xc7 => {
                let modrm = self.peek()?;
                let reg = self.modrm()?;
                let xsave = match opcode {
                    0xae => prefix == 0 && matches!(reg, 4..=6),
                    _ => matches!(reg, 3..=5),
                };
                if modrm >> 6 != 3 && xsave {
                    Extension::Xsave
                } else {
                    sse_class(prefix, opcode)
                }
            }
            // jcc rel32 (66 does not shorten it in 64-bit mode on Intel).
            0x80..=0x8f => {
                self.skip(if rel16 { 2 } else { 4 })?;
                Extension::Base
            }
            // Immediate byte after ModRM.
            0x70..=0x73 | 0xa4 | 0xac | 0xba | 0xc2 | 0xc4..=0xc6 => {
                self.modrm()?;
                self.skip(1)?;
                sse_class(prefix, opcode)
            }
            // AMD SSE4a extrq / insertq with two immediate bytes.
            0x78 if prefix == 0x66 || prefix == 0xf2 => {
                self.modrm()?;
                self.skip(2)?;
                Extension::Sse
            }
            _ => {
                self.modrm()?;
                sse_class(prefix, opcode)
            }
        };
        self.finish(extension)
    }

    /// 0F 38 xx: ModRM, no immediate.
    fn map_38(&mut self, prefix: u8) -> Result<Length, DecodeError> {
        let opcode = self.byte()?;
        self.modrm()?;
        let extension = match (prefix, opcode) {
            (0x66, 0x00..=0x0b | 0x1c..=0x1e) => Extension::Ssse3,
            (0, 0x00..=0x0b | 0x1c..=0x1e) => Extension::Mmx,
            (0x66, 0x10 | 0x14 | 0x15 | 0x17 | 0x20..=0x25 | 0x28..=0x2b | 0x30..=0x35)
            | (0x66, 0x38..=0x41) => Extension::Sse41,
            (0x66, 0x37) | (0xf2, 0xf0 | 0xf1) => Extension::Sse42,
            (0x66, 0xdb..=0xdf) => Extension::Aes,
            (0, 0xc8..=0xcd) => Extension::Sha,
            _ => Extension::Base,
        };
        self.finish(extension)
    }

    /// 0F 3A xx: ModRM and an immediate byte.
    fn map_3a(&mut self, prefix: u8) -> Result<Length, DecodeError> {
        let opcode = self.byte()?;
        self.modrm()?;
        self.skip(1)?;
        let extension = match (prefix, opcode) {
            (0x66, 0x0f) => Extension::Ssse3,
            (0, 0x0f) => Extension::Mmx,
            (0x66, 0x08..=0x0e | 0x14..=0x17 | 0x20..=0x22 | 0x40..=0x42) => Extension::Sse41,
            (0x66, 0x60..=0x63) => Extension::Sse42,
            (0x66, 0x44) => Extension::Pclmul,
            (0x66, 0xdf) => Extension::Aes,
            (0, 0xcc) => Extension::Sha,
            _ => Extension::Base,
        };
        self.finish(extension)
    }

    /// C4 (three-byte VEX), C5 (two-byte VEX), 62 (EVEX).
    fn vex_or_evex(&mut self, escape: u8) -> Result<Length, DecodeError> {
        let (map, extension) = match escape {
            0xc5 => {
                self.byte()?;
                (1, Extension::Vex)
            }
            0xc4 => {
                let map = self.byte()? & 0x1f;
                self.byte()?;
                (map, Extension::Vex)
            }
            _ => {
                let map = self.byte()? & 7;
                self.skip(2)?;
                (map, Extension::Evex)
            }
        };
        let opcode = self.byte()?;
        let valid = match extension {
            Extension::Vex => matches!(map, 1..=3),
            _ => matches!(map, 1..=3 | 5 | 6),
        };
        if !valid {
            return Err(DecodeError::Invalid);
        }
        // vzeroupper / vzeroall: no ModRM.
        if !(extension == Extension::Vex && map == 1 && opcode == 0x77) {
            self.modrm()?;
        }
        let immediate = match map {
            1 => matches!(opcode, 0x70..=0x73 | 0xc2 | 0xc4..=0xc6),
            3 => true,
            _ => false,
        };
        if immediate {
            self.skip(1)?;
        }
        self.finish(extension)
    }

    /// AMD XOP: 8F, two payload bytes, opcode, ModRM; map 8 has an
    /// immediate byte, map A four.
    fn xop(&mut self) -> Result<Length, DecodeError> {
        let map = self.byte()? & 0x1f;
        self.byte()?;
        self.byte()?;
        self.modrm()?;
        match map {
            8 => self.skip(1)?,
            9 => {}
            0xa => self.skip(4)?,
            _ => return Err(DecodeError::Invalid),
        }
        self.finish(Extension::Xop)
    }
}

/// The extension of a 0F-map instruction with ModRM (or with an immediate
/// after ModRM), from its mandatory prefix.
fn sse_class(prefix: u8, opcode: u8) -> Extension {
    match (prefix, opcode) {
        (0xf3, 0xb8) => Extension::Popcnt,
        (0xf2, 0x12) | (0xf3, 0x12 | 0x16) | (0x66 | 0xf2, 0x7c | 0x7d | 0xd0) | (0xf2, 0xf0) => {
            Extension::Sse3
        }
        // Conversions between xmm and MMX registers.
        (0 | 0x66, 0x2a | 0x2c | 0x2d) | (0xf2 | 0xf3, 0xd6) => Extension::Mmx,
        (_, 0x10..=0x17 | 0x28..=0x2f | 0x50..=0x5f | 0xc2 | 0xc3 | 0xc6) => Extension::Sse,
        (0, 0x60..=0x7f | 0xc4 | 0xc5 | 0xd0..=0xfe) => Extension::Mmx,
        (_, 0x60..=0x7f | 0xc4 | 0xc5 | 0xd0..=0xfe) => Extension::Sse,
        _ => Extension::Base,
    }
}
