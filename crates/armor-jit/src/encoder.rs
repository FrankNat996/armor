//! An AArch64 encoder for the instructions the JIT emits (ADR-0021: our
//! own, no dependency). Encodings from the Arm Architecture Reference
//! Manual, "A64 Instruction Set Encoding"; every one is checked against
//! the GNU assembler by `cargo xtask encoder-check` (`samples` below).
//!
//! Register 31 means SP or the zero register depending on the
//! instruction, as in the architecture; the methods document which.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// A general-purpose register number, 0..=31.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reg(u8);

impl Reg {
    pub const fn new(n: u8) -> Reg {
        assert!(n < 32, "AArch64 has 32 register numbers");
        Reg(n)
    }

    pub fn bits(self) -> u32 {
        u32::from(self.0)
    }
}

/// Register 31 as the zero register (XZR/WZR).
pub const ZR: Reg = Reg(31);
/// Register 31 as the stack pointer.
pub const SP: Reg = Reg(31);
/// The link register.
pub const LR: Reg = Reg(30);

/// Operand width: 32-bit (W registers) or 64-bit (X registers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    W,
    X,
}

impl Width {
    fn sf(self) -> u32 {
        match self {
            Width::W => 0,
            Width::X => 1 << 31,
        }
    }

    fn bits(self) -> u32 {
        match self {
            Width::W => 32,
            Width::X => 64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shift {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
    Ror = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    Eq = 0,
    Ne = 1,
    Hs = 2,
    Lo = 3,
    Mi = 4,
    Pl = 5,
    Vs = 6,
    Vc = 7,
    Hi = 8,
    Ls = 9,
    Ge = 10,
    Lt = 11,
    Gt = 12,
    Le = 13,
    Al = 14,
}

impl Cond {
    pub fn invert(self) -> Cond {
        const ALL: [Cond; 15] = [
            Cond::Eq,
            Cond::Ne,
            Cond::Hs,
            Cond::Lo,
            Cond::Mi,
            Cond::Pl,
            Cond::Vs,
            Cond::Vc,
            Cond::Hi,
            Cond::Ls,
            Cond::Ge,
            Cond::Lt,
            Cond::Gt,
            Cond::Le,
            Cond::Al,
        ];
        assert!(self != Cond::Al, "AL has no inverse");
        ALL[(self as usize) ^ 1]
    }
}

/// Load and store sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemSize {
    B8,
    B16,
    B32,
    B64,
}

impl MemSize {
    fn scale(self) -> u32 {
        match self {
            MemSize::B8 => 0,
            MemSize::B16 => 1,
            MemSize::B32 => 2,
            MemSize::B64 => 3,
        }
    }
}

/// A position in the code that branches can target before it is bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label(usize);

#[derive(Debug, Clone, Copy)]
enum FixupKind {
    /// B, BL: imm26 at bits 0..26.
    Imm26,
    /// B.cond, CBZ, CBNZ: imm19 at bits 5..24.
    Imm19,
    /// TBZ, TBNZ: imm14 at bits 5..19.
    Imm14,
    /// ADR: a byte offset of 21 bits, its low 2 bits at 29..31 and the
    /// rest at 5..24.
    Adr,
}

/// An encoding problem: an operand the instruction cannot express.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError(pub String);

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for EncodeError {}

type Result<T> = core::result::Result<T, EncodeError>;

fn fail<T>(what: impl Into<String>) -> Result<T> {
    Err(EncodeError(what.into()))
}

/// Emits instructions into a buffer of 32-bit words.
#[derive(Debug, Default)]
pub struct Assembler {
    words: Vec<u32>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Label, FixupKind)>,
}

impl Assembler {
    pub fn new() -> Assembler {
        // Room for a typical block, so that emitting does not reallocate.
        Assembler {
            words: Vec::with_capacity(1024),
            labels: Vec::with_capacity(256),
            fixups: Vec::with_capacity(256),
        }
    }

    /// The encoded words; every label used must be bound.
    pub fn finish(mut self) -> Result<Vec<u32>> {
        for &(at, label, kind) in &self.fixups {
            let Some(target) = self.labels[label.0] else {
                return fail("branch to an unbound label");
            };
            let delta = target as i64 - at as i64;
            self.words[at] |= branch_field(kind, delta)?;
        }
        Ok(self.words)
    }

    /// Words emitted so far (labels may still be unresolved).
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    fn emit(&mut self, word: u32) {
        self.words.push(word);
    }

    pub fn new_label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    /// Where `label` is bound (in words), once it is.
    pub fn position(&self, label: Label) -> Option<usize> {
        self.labels[label.0]
    }

    pub fn bind(&mut self, label: Label) {
        self.labels[label.0] = Some(self.words.len());
    }

    // --- Moves --------------------------------------------------------

    /// MOVZ: `rd = imm16 << (16 * hw)`.
    pub fn movz(&mut self, w: Width, rd: Reg, imm16: u16, hw: u32) -> Result<()> {
        self.move_wide(0x5280_0000, w, rd, imm16, hw)
    }

    /// MOVN: `rd = !(imm16 << (16 * hw))`.
    pub fn movn(&mut self, w: Width, rd: Reg, imm16: u16, hw: u32) -> Result<()> {
        self.move_wide(0x1280_0000, w, rd, imm16, hw)
    }

    /// MOVK: replaces 16 bits of `rd`.
    pub fn movk(&mut self, w: Width, rd: Reg, imm16: u16, hw: u32) -> Result<()> {
        self.move_wide(0x7280_0000, w, rd, imm16, hw)
    }

    fn move_wide(&mut self, base: u32, w: Width, rd: Reg, imm16: u16, hw: u32) -> Result<()> {
        if hw >= w.bits() / 16 {
            return fail(format!("shift {} too large for {w:?}", hw * 16));
        }
        self.emit(base | w.sf() | (hw << 21) | (u32::from(imm16) << 5) | rd.bits());
        Ok(())
    }

    /// Any 64-bit constant, in one to four instructions.
    pub fn mov_imm64(&mut self, rd: Reg, value: u64) {
        let halves: [u16; 4] = core::array::from_fn(|i| (value >> (16 * i)) as u16);
        let zeros = halves.iter().filter(|&&h| h == 0).count();
        let ones = halves.iter().filter(|&&h| h == 0xffff).count();
        let mut started = false;
        if ones > zeros {
            // Start from all ones (MOVN) and patch the other halves.
            for (i, &h) in halves.iter().enumerate() {
                if h == 0xffff {
                    continue;
                }
                if !started {
                    self.movn(Width::X, rd, !h, i as u32).expect("hw < 4");
                    started = true;
                } else {
                    self.movk(Width::X, rd, h, i as u32).expect("hw < 4");
                }
            }
            if !started {
                self.movn(Width::X, rd, 0, 0).expect("hw < 4");
            }
            return;
        }
        for (i, &h) in halves.iter().enumerate() {
            if h == 0 {
                continue;
            }
            if !started {
                self.movz(Width::X, rd, h, i as u32).expect("hw < 4");
                started = true;
            } else {
                self.movk(Width::X, rd, h, i as u32).expect("hw < 4");
            }
        }
        if !started {
            self.movz(Width::X, rd, 0, 0).expect("hw < 4");
        }
    }

    /// MOV (register): ORR rd, ZR, rm. Register 31 is the zero register.
    pub fn mov(&mut self, w: Width, rd: Reg, rm: Reg) {
        self.logical_reg(0x2A00_0000, w, rd, ZR, rm, Shift::Lsl, 0)
            .expect("no shift");
    }

    /// MOV to or from SP: ADD rd, rn, #0. Register 31 is SP.
    pub fn mov_sp(&mut self, rd: Reg, rn: Reg) {
        self.add_imm(Width::X, rd, rn, 0).expect("0 fits");
    }

    // --- Arithmetic ---------------------------------------------------

    /// ADD (immediate); register 31 is SP. `imm` is 12 bits, or 12 bits
    /// shifted left by 12.
    pub fn add_imm(&mut self, w: Width, rd: Reg, rn: Reg, imm: u32) -> Result<()> {
        self.add_sub_imm(0x1100_0000, w, rd, rn, imm)
    }

    /// ADDS (immediate): flags set; `rd` 31 is the zero register (CMN).
    pub fn adds_imm(&mut self, w: Width, rd: Reg, rn: Reg, imm: u32) -> Result<()> {
        self.add_sub_imm(0x3100_0000, w, rd, rn, imm)
    }

    pub fn sub_imm(&mut self, w: Width, rd: Reg, rn: Reg, imm: u32) -> Result<()> {
        self.add_sub_imm(0x5100_0000, w, rd, rn, imm)
    }

    /// SUBS (immediate): `rd` 31 is the zero register (CMP).
    pub fn subs_imm(&mut self, w: Width, rd: Reg, rn: Reg, imm: u32) -> Result<()> {
        self.add_sub_imm(0x7100_0000, w, rd, rn, imm)
    }

    fn add_sub_imm(&mut self, base: u32, w: Width, rd: Reg, rn: Reg, imm: u32) -> Result<()> {
        let (imm12, shifted) = if imm < 1 << 12 {
            (imm, 0)
        } else if imm & 0xfff == 0 && imm < 1 << 24 {
            (imm >> 12, 1)
        } else {
            return fail(format!(
                "immediate {imm:#x} is not 12 bits (optionally << 12)"
            ));
        };
        self.emit(base | w.sf() | (shifted << 22) | (imm12 << 10) | (rn.bits() << 5) | rd.bits());
        Ok(())
    }

    /// ADD (shifted register); register 31 is the zero register.
    pub fn add(
        &mut self,
        w: Width,
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        amount: u32,
    ) -> Result<()> {
        self.add_sub_reg(0x0B00_0000, w, rd, rn, rm, shift, amount)
    }

    pub fn adds(
        &mut self,
        w: Width,
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        amount: u32,
    ) -> Result<()> {
        self.add_sub_reg(0x2B00_0000, w, rd, rn, rm, shift, amount)
    }

    pub fn sub(
        &mut self,
        w: Width,
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        amount: u32,
    ) -> Result<()> {
        self.add_sub_reg(0x4B00_0000, w, rd, rn, rm, shift, amount)
    }

    pub fn subs(
        &mut self,
        w: Width,
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        amount: u32,
    ) -> Result<()> {
        self.add_sub_reg(0x6B00_0000, w, rd, rn, rm, shift, amount)
    }

    #[allow(clippy::too_many_arguments)]
    fn add_sub_reg(
        &mut self,
        base: u32,
        w: Width,
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        amount: u32,
    ) -> Result<()> {
        if shift == Shift::Ror {
            return fail("add/sub do not take ROR");
        }
        if amount >= w.bits() {
            return fail(format!("shift amount {amount} too large"));
        }
        self.emit(
            base | w.sf()
                | ((shift as u32) << 22)
                | (rm.bits() << 16)
                | (amount << 10)
                | (rn.bits() << 5)
                | rd.bits(),
        );
        Ok(())
    }

    /// ADC, SBC and their flag-setting forms: rd = rn ± rm ± carry.
    pub fn adc(&mut self, w: Width, rd: Reg, rn: Reg, rm: Reg, set_flags: bool) {
        self.carry_op(0x1A00_0000, w, rd, rn, rm, set_flags);
    }

    pub fn sbc(&mut self, w: Width, rd: Reg, rn: Reg, rm: Reg, set_flags: bool) {
        self.carry_op(0x5A00_0000, w, rd, rn, rm, set_flags);
    }

    fn carry_op(&mut self, base: u32, w: Width, rd: Reg, rn: Reg, rm: Reg, set_flags: bool) {
        let s = if set_flags { 1 << 29 } else { 0 };
        self.emit(base | w.sf() | s | (rm.bits() << 16) | (rn.bits() << 5) | rd.bits());
    }

    /// MADD: rd = ra + rn * rm (MUL with ra = ZR).
    pub fn madd(&mut self, w: Width, rd: Reg, rn: Reg, rm: Reg, ra: Reg) {
        self.emit(
            0x1B00_0000
                | w.sf()
                | (rm.bits() << 16)
                | (ra.bits() << 10)
                | (rn.bits() << 5)
                | rd.bits(),
        );
    }

    /// MSUB: rd = ra - rn * rm.
    pub fn msub(&mut self, w: Width, rd: Reg, rn: Reg, rm: Reg, ra: Reg) {
        self.emit(
            0x1B00_8000
                | w.sf()
                | (rm.bits() << 16)
                | (ra.bits() << 10)
                | (rn.bits() << 5)
                | rd.bits(),
        );
    }

    /// SMULH / UMULH: the high 64 bits of the 128-bit product.
    pub fn mulh(&mut self, signed: bool, rd: Reg, rn: Reg, rm: Reg) {
        let base = if signed { 0x9B40_7C00 } else { 0x9BC0_7C00 };
        self.emit(base | (rm.bits() << 16) | (rn.bits() << 5) | rd.bits());
    }

    /// SDIV / UDIV.
    pub fn div(&mut self, signed: bool, w: Width, rd: Reg, rn: Reg, rm: Reg) {
        let base = if signed { 0x1AC0_0C00 } else { 0x1AC0_0800 };
        self.emit(base | w.sf() | (rm.bits() << 16) | (rn.bits() << 5) | rd.bits());
    }

    // --- Logic and shifts ---------------------------------------------

    /// AND, ORR, EOR, ANDS (shifted register), and with `invert` BIC, ORN,
    /// EON, BICS. Register 31 is the zero register.
    #[allow(clippy::too_many_arguments)]
    pub fn logic(
        &mut self,
        op: Logic,
        invert: bool,
        w: Width,
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        amount: u32,
    ) -> Result<()> {
        let n = if invert { 1 << 21 } else { 0 };
        self.logical_reg(op.base_reg() | n, w, rd, rn, rm, shift, amount)
    }

    #[allow(clippy::too_many_arguments)]
    fn logical_reg(
        &mut self,
        base: u32,
        w: Width,
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        amount: u32,
    ) -> Result<()> {
        if amount >= w.bits() {
            return fail(format!("shift amount {amount} too large"));
        }
        self.emit(
            base | w.sf()
                | ((shift as u32) << 22)
                | (rm.bits() << 16)
                | (amount << 10)
                | (rn.bits() << 5)
                | rd.bits(),
        );
        Ok(())
    }

    /// AND, ORR, EOR, ANDS with a bitmask immediate. `rd` 31 is SP for
    /// AND, ORR, EOR and the zero register for ANDS (TST).
    pub fn logic_imm(&mut self, op: Logic, w: Width, rd: Reg, rn: Reg, imm: u64) -> Result<()> {
        let Some((n, immr, imms)) = encode_bitmask(imm, w.bits()) else {
            return fail(format!("{imm:#x} is not a bitmask immediate"));
        };
        self.emit(
            op.base_imm()
                | w.sf()
                | (n << 22)
                | (immr << 16)
                | (imms << 10)
                | (rn.bits() << 5)
                | rd.bits(),
        );
        Ok(())
    }

    /// LSLV, LSRV, ASRV, RORV: shift by a register (modulo the width).
    pub fn shift_reg(&mut self, shift: Shift, w: Width, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(
            0x1AC0_2000
                | w.sf()
                | ((shift as u32) << 10)
                | (rm.bits() << 16)
                | (rn.bits() << 5)
                | rd.bits(),
        );
    }

    /// UBFM / SBFM, the base of the immediate shifts and extensions.
    #[allow(clippy::too_many_arguments)]
    pub fn bfm(
        &mut self,
        signed: bool,
        w: Width,
        rd: Reg,
        rn: Reg,
        immr: u32,
        imms: u32,
    ) -> Result<()> {
        if immr >= w.bits() || imms >= w.bits() {
            return fail("bitfield position out of range");
        }
        let base = match (signed, w) {
            (false, Width::W) => 0x5300_0000,
            (false, Width::X) => 0xD340_0000,
            (true, Width::W) => 0x1300_0000,
            (true, Width::X) => 0x9340_0000,
        };
        self.emit(base | (immr << 16) | (imms << 10) | (rn.bits() << 5) | rd.bits());
        Ok(())
    }

    /// RBIT, REV (bytes of the whole register), CLZ.
    pub fn data1(&mut self, op: Data1, w: Width, rd: Reg, rn: Reg) {
        let opcode = match (op, w) {
            (Data1::Rbit, _) => 0b000000,
            (Data1::Rev, Width::W) => 0b000010,
            (Data1::Rev, Width::X) => 0b000011,
            (Data1::Clz, _) => 0b000100,
        };
        self.emit(0x5AC0_0000 | w.sf() | (opcode << 10) | (rn.bits() << 5) | rd.bits());
    }

    /// BFM: inserts a bitfield of `rn` into `rd`, keeping the other bits
    /// (BFI rd, rn, #lsb, #width is immr = -lsb mod size, imms = width - 1).
    pub fn bfm_insert(&mut self, w: Width, rd: Reg, rn: Reg, immr: u32, imms: u32) -> Result<()> {
        if immr >= w.bits() || imms >= w.bits() {
            return fail("bitfield position out of range");
        }
        let base = match w {
            Width::W => 0x3300_0000,
            Width::X => 0xB340_0000,
        };
        self.emit(base | (immr << 16) | (imms << 10) | (rn.bits() << 5) | rd.bits());
        Ok(())
    }

    /// LSL, LSR, ASR by an immediate (aliases of UBFM and SBFM).
    pub fn shift_imm(
        &mut self,
        shift: Shift,
        w: Width,
        rd: Reg,
        rn: Reg,
        amount: u32,
    ) -> Result<()> {
        let bits = w.bits();
        if amount >= bits {
            return fail(format!("shift amount {amount} too large"));
        }
        match shift {
            Shift::Lsl => self.bfm(false, w, rd, rn, (bits - amount) % bits, bits - 1 - amount),
            Shift::Lsr => self.bfm(false, w, rd, rn, amount, bits - 1),
            Shift::Asr => self.bfm(true, w, rd, rn, amount, bits - 1),
            Shift::Ror => {
                // EXTR rd, rn, rn, #amount.
                let base = match w {
                    Width::W => 0x1380_0000,
                    Width::X => 0x93C0_0000,
                };
                self.emit(base | (rn.bits() << 16) | (amount << 10) | (rn.bits() << 5) | rd.bits());
                Ok(())
            }
        }
    }

    // --- Conditional select -------------------------------------------

    /// CSEL, CSINC, CSINV, CSNEG.
    pub fn csel(&mut self, op: Select, w: Width, rd: Reg, rn: Reg, rm: Reg, cond: Cond) {
        let base = match op {
            Select::Csel => 0x1A80_0000,
            Select::Csinc => 0x1A80_0400,
            Select::Csinv => 0x5A80_0000,
            Select::Csneg => 0x5A80_0400,
        };
        self.emit(
            base | w.sf()
                | (rm.bits() << 16)
                | ((cond as u32) << 12)
                | (rn.bits() << 5)
                | rd.bits(),
        );
    }

    /// CSET: rd = cond ? 1 : 0.
    pub fn cset(&mut self, w: Width, rd: Reg, cond: Cond) {
        self.csel(Select::Csinc, w, rd, ZR, ZR, cond.invert());
    }

    // --- Loads and stores ---------------------------------------------

    /// LDR/LDRB/LDRH (zero-extending) or STR/STRB/STRH with an unsigned
    /// offset, a multiple of the size. Register 31 as base is SP.
    pub fn ldr(&mut self, size: MemSize, rt: Reg, rn: Reg, offset: u32) -> Result<()> {
        self.load_store_imm(size, 0b01, rt, rn, offset)
    }

    pub fn str(&mut self, size: MemSize, rt: Reg, rn: Reg, offset: u32) -> Result<()> {
        self.load_store_imm(size, 0b00, rt, rn, offset)
    }

    /// LDRSB/LDRSH/LDRSW into a 64-bit register.
    pub fn ldrs(&mut self, size: MemSize, rt: Reg, rn: Reg, offset: u32) -> Result<()> {
        if size == MemSize::B64 {
            return fail("no sign-extending 64-bit load");
        }
        self.load_store_imm(size, 0b10, rt, rn, offset)
    }

    fn load_store_imm(
        &mut self,
        size: MemSize,
        opc: u32,
        rt: Reg,
        rn: Reg,
        offset: u32,
    ) -> Result<()> {
        let scale = size.scale();
        if !offset.is_multiple_of(1 << scale) || offset >> scale >= 1 << 12 {
            return fail(format!("offset {offset} not encodable for {size:?}"));
        }
        self.emit(
            (scale << 30)
                | 0x3900_0000
                | (opc << 22)
                | ((offset >> scale) << 10)
                | (rn.bits() << 5)
                | rt.bits(),
        );
        Ok(())
    }

    /// LDR/STR with a register offset (`[rn, rm]` or `[rn, rm, lsl #size]`).
    pub fn load_store_reg(
        &mut self,
        load: bool,
        size: MemSize,
        rt: Reg,
        rn: Reg,
        rm: Reg,
        scaled: bool,
    ) {
        let opc = if load { 0b01 } else { 0b00 };
        let s = u32::from(scaled);
        // option = 011 (LSL / UXTX).
        self.emit(
            (size.scale() << 30)
                | 0x3820_0800
                | (opc << 22)
                | (rm.bits() << 16)
                | (0b011 << 13)
                | (s << 12)
                | (rn.bits() << 5)
                | rt.bits(),
        );
    }

    /// LDP/STP of X registers: signed offset, pre-index or post-index.
    pub fn pair(
        &mut self,
        load: bool,
        mode: PairMode,
        rt: Reg,
        rt2: Reg,
        rn: Reg,
        offset: i32,
    ) -> Result<()> {
        if offset % 8 != 0 || !(-512..=504).contains(&offset) {
            return fail(format!("pair offset {offset} not encodable"));
        }
        let mode_bits = match mode {
            PairMode::PostIndex => 0b001,
            PairMode::Offset => 0b010,
            PairMode::PreIndex => 0b011,
        };
        let imm7 = ((offset / 8) as u32) & 0x7f;
        self.emit(
            0xA800_0000
                | (mode_bits << 23)
                | (u32::from(load) << 22)
                | (imm7 << 15)
                | (rt2.bits() << 10)
                | (rn.bits() << 5)
                | rt.bits(),
        );
        Ok(())
    }

    // --- Branches -----------------------------------------------------

    pub fn b(&mut self, target: Label) {
        self.branch(0x1400_0000, target, FixupKind::Imm26);
    }

    pub fn bl(&mut self, target: Label) {
        self.branch(0x9400_0000, target, FixupKind::Imm26);
    }

    pub fn b_cond(&mut self, cond: Cond, target: Label) {
        self.branch(0x5400_0000 | cond as u32, target, FixupKind::Imm19);
    }

    /// CBZ / CBNZ.
    pub fn cbz(&mut self, nonzero: bool, w: Width, rt: Reg, target: Label) {
        let base = if nonzero { 0x3500_0000 } else { 0x3400_0000 };
        self.branch(base | w.sf() | rt.bits(), target, FixupKind::Imm19);
    }

    /// TBZ / TBNZ on bit `bit` of `rt`.
    pub fn tbz(&mut self, nonzero: bool, rt: Reg, bit: u32, target: Label) {
        assert!(bit < 64, "bit {bit} out of range");
        let base = if nonzero { 0x3700_0000 } else { 0x3600_0000 };
        self.branch(
            base | ((bit >> 5) << 31) | ((bit & 31) << 19) | rt.bits(),
            target,
            FixupKind::Imm14,
        );
    }

    fn branch(&mut self, word: u32, target: Label, kind: FixupKind) {
        self.fixups.push((self.words.len(), target, kind));
        self.emit(word);
    }

    /// ADR: rd = the address of `target`.
    pub fn adr(&mut self, rd: Reg, target: Label) {
        self.branch(0x1000_0000 | rd.bits(), target, FixupKind::Adr);
    }

    pub fn br(&mut self, rn: Reg) {
        self.emit(0xD61F_0000 | (rn.bits() << 5));
    }

    pub fn blr(&mut self, rn: Reg) {
        self.emit(0xD63F_0000 | (rn.bits() << 5));
    }

    pub fn ret(&mut self) {
        self.emit(0xD65F_0000 | (LR.bits() << 5));
    }

    // --- System -------------------------------------------------------

    /// MRS rt, NZCV.
    pub fn read_flags(&mut self, rt: Reg) {
        self.emit(0xD53B_4200 | rt.bits());
    }

    /// MSR NZCV, rt.
    pub fn write_flags(&mut self, rt: Reg) {
        self.emit(0xD51B_4200 | rt.bits());
    }

    /// MRS rt, FPSR: the cumulative floating-point exception flags.
    pub fn read_fpsr(&mut self, rt: Reg) {
        self.emit(0xD53B_4420 | rt.bits());
    }

    /// MSR FPSR, rt.
    pub fn write_fpsr(&mut self, rt: Reg) {
        self.emit(0xD51B_4420 | rt.bits());
    }

    pub fn nop(&mut self) {
        self.emit(0xD503_201F);
    }

    /// An instruction word encoded elsewhere (`branch_word`).
    pub fn raw(&mut self, word: u32) {
        self.emit(word);
    }

    pub fn brk(&mut self, imm16: u16) {
        self.emit(0xD420_0000 | (u32::from(imm16) << 5));
    }
}

/// A SIMD&FP register number, v0..=v31.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VReg(u8);

impl VReg {
    pub const fn new(n: u8) -> VReg {
        assert!(n < 32, "AArch64 has 32 vector registers");
        VReg(n)
    }

    fn bits(self) -> u32 {
        u32::from(self.0)
    }
}

/// Lane size of a 128-bit vector operation: 16B, 8H, 4S or 2D.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lanes {
    B,
    H,
    S,
    D,
}

impl Lanes {
    fn size(self) -> u32 {
        match self {
            Lanes::B => 0,
            Lanes::H => 1,
            Lanes::S => 2,
            Lanes::D => 3,
        }
    }

    pub fn bits(self) -> u32 {
        8 << self.size()
    }
}

/// Advanced SIMD "three same" operations on 128-bit vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vec3 {
    Add,
    Sub,
    /// All ones where equal.
    Cmeq,
    /// All ones where signed greater.
    Cmgt,
    Smax,
    Smin,
    Umax,
    Umin,
    /// Bitwise: the lane size is ignored.
    And,
    /// `vn & !vm`.
    Bic,
    Orr,
    Eor,
    /// Interleave the low halves (vn[0], vm[0], vn[1], vm[1], ...).
    Zip1,
    /// Interleave the high halves.
    Zip2,
}

/// Shifts of every lane by an immediate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VShift {
    /// Left, 0..lane bits - 1.
    Shl,
    /// Logical right, 1..=lane bits.
    Ushr,
    /// Arithmetic right, 1..=lane bits.
    Sshr,
}

/// The memory barriers the translator uses (DMB, inner shareable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Barrier {
    /// DMB ISH: every access before against every access after.
    Ish,
    /// DMB ISHLD: loads before against loads and stores after.
    IshLd,
    /// DMB ISHST: stores before against stores after.
    IshSt,
}

/// The LSE atomic read-modify-writes (FEAT_LSE) the translator uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicOp {
    /// LDADDAL: memory + operand.
    Add,
    /// LDCLRAL: memory AND NOT operand.
    Clr,
    /// LDEORAL: memory XOR operand.
    Eor,
    /// LDSETAL: memory OR operand.
    Set,
    /// SWPAL: the operand.
    Swp,
}

impl Assembler {
    // --- Ordered accesses (ADR-0023) ------------------------------------

    /// LDAPR (FEAT_LRCPC): a load-acquire, RCpc. With STLR stores it gives
    /// x86's TSO. Base register only, and the address must be aligned to
    /// the size: without FEAT_LSE2 (the Raspberry Pi 5's Cortex-A76) an
    /// unaligned one faults.
    pub fn ldapr(&mut self, size: MemSize, rt: Reg, rn: Reg) {
        let base = match size {
            MemSize::B8 => 0x38BF_C000,
            MemSize::B16 => 0x78BF_C000,
            MemSize::B32 => 0xB8BF_C000,
            MemSize::B64 => 0xF8BF_C000,
        };
        self.emit(base | (rn.bits() << 5) | rt.bits());
    }

    /// STLR: a store-release. Base register only, aligned address.
    pub fn stlr(&mut self, size: MemSize, rt: Reg, rn: Reg) {
        let base = match size {
            MemSize::B8 => 0x089F_FC00,
            MemSize::B16 => 0x489F_FC00,
            MemSize::B32 => 0x889F_FC00,
            MemSize::B64 => 0xC89F_FC00,
        };
        self.emit(base | (rn.bits() << 5) | rt.bits());
    }

    /// LD<op>AL or SWPAL (FEAT_LSE): atomically replaces the `size` bytes
    /// at `[rn]` with `op` applied to them and `rs`, and loads their old
    /// value, zero-extended, into `rt`. Acquire and release: ordered
    /// against every access before and after (ADR-0023). Base register
    /// only; the address must be aligned to the size (no FEAT_LSE2).
    pub fn atomic(&mut self, op: AtomicOp, size: MemSize, rs: Reg, rt: Reg, rn: Reg) {
        let (o3, opc) = match op {
            AtomicOp::Add => (0, 0),
            AtomicOp::Clr => (0, 1),
            AtomicOp::Eor => (0, 2),
            AtomicOp::Set => (0, 3),
            AtomicOp::Swp => (1, 0),
        };
        self.emit(
            0x38E0_0000
                | size.scale() << 30
                | rs.bits() << 16
                | o3 << 15
                | opc << 12
                | rn.bits() << 5
                | rt.bits(),
        );
    }

    /// CASAL (FEAT_LSE): if the `size` bytes at `[rn]` equal `rs`, they
    /// become `rt`; either way `rs` gets their old value, zero-extended.
    /// Acquire and release, aligned address, as `atomic`.
    pub fn casal(&mut self, size: MemSize, rs: Reg, rt: Reg, rn: Reg) {
        self.emit(0x08E0_FC00 | size.scale() << 30 | rs.bits() << 16 | rn.bits() << 5 | rt.bits());
    }

    // ARMv8.0 replacements (ADR-0032, CPUs without FEAT_LSE or
    // FEAT_LRCPC: Cortex-A53, A72, …).

    /// LDAR: a load-acquire, RCsc: stronger than LDAPR (it is also ordered
    /// after earlier STLRs), so it serves as its replacement. Base
    /// register only, aligned address.
    pub fn ldar(&mut self, size: MemSize, rt: Reg, rn: Reg) {
        self.emit(0x08DF_FC00 | size.scale() << 30 | rn.bits() << 5 | rt.bits());
    }

    /// LDAXR: a load-acquire exclusive, zero-extended into `rt`.
    pub fn ldaxr(&mut self, size: MemSize, rt: Reg, rn: Reg) {
        self.emit(0x085F_FC00 | size.scale() << 30 | rn.bits() << 5 | rt.bits());
    }

    /// STLXR: a store-release exclusive of the low `size` bytes of `rt`;
    /// `ws` gets 0 if it was stored, 1 if the exclusive monitor was lost.
    pub fn stlxr(&mut self, size: MemSize, ws: Reg, rt: Reg, rn: Reg) {
        self.emit(0x0800_FC00 | size.scale() << 30 | ws.bits() << 16 | rn.bits() << 5 | rt.bits());
    }

    /// CLREX: drops the exclusive monitor.
    pub fn clrex(&mut self) {
        self.emit(0xD503_3F5F);
    }

    pub fn dmb(&mut self, barrier: Barrier) {
        self.emit(match barrier {
            Barrier::Ish => 0xD503_3BBF,
            Barrier::IshLd => 0xD503_39BF,
            Barrier::IshSt => 0xD503_3ABF,
        });
    }

    // --- Advanced SIMD (128-bit vectors) ------------------------------

    /// LDR Qt / STR Qt with an unsigned offset, a multiple of 16.
    pub fn ldr_q(&mut self, vt: VReg, rn: Reg, offset: u32) -> Result<()> {
        self.load_store_q(0x3DC0_0000, vt, rn, offset)
    }

    pub fn str_q(&mut self, vt: VReg, rn: Reg, offset: u32) -> Result<()> {
        self.load_store_q(0x3D80_0000, vt, rn, offset)
    }

    fn load_store_q(&mut self, base: u32, vt: VReg, rn: Reg, offset: u32) -> Result<()> {
        if !offset.is_multiple_of(16) || offset / 16 >= 1 << 12 {
            return fail(format!("offset {offset} not encodable for a Q register"));
        }
        self.emit(base | ((offset / 16) << 10) | (rn.bits() << 5) | vt.bits());
        Ok(())
    }

    /// LDR / STR (immediate) of 32 or 64 bits with writeback: pre-index
    /// (`[rn, #offset]!`, the address is rn + offset) or post-index
    /// (`[rn], #offset`, the address is rn); rn += offset either way.
    /// `offset` from -256 to 255; rt must not be rn.
    pub fn load_store_indexed(
        &mut self,
        load: bool,
        size: MemSize,
        rt: Reg,
        rn: Reg,
        offset: i32,
        pre: bool,
    ) -> Result<()> {
        if !(-256..=255).contains(&offset) {
            return fail(format!("offset {offset} not encodable with writeback"));
        }
        if rt == rn {
            return fail("writeback to the transferred register");
        }
        let base = match (size, load) {
            (MemSize::B64, false) => 0xF800_0000,
            (MemSize::B64, true) => 0xF840_0000,
            (MemSize::B32, false) => 0xB800_0000,
            (MemSize::B32, true) => 0xB840_0000,
            _ => return fail("writeback forms of 32 and 64 bits only"),
        };
        let mode = if pre { 0xC00 } else { 0x400 };
        let imm9 = (offset as u32) & 0x1ff;
        self.emit(base | mode | (imm9 << 12) | (rn.bits() << 5) | rt.bits());
        Ok(())
    }

    /// LDP Qt1, Qt2 / STP Qt1, Qt2 with a signed offset, a multiple of 16
    /// from -1024 to 1008.
    pub fn pair_q(&mut self, load: bool, vt1: VReg, vt2: VReg, rn: Reg, offset: i32) -> Result<()> {
        if offset % 16 != 0 || !(-1024..=1008).contains(&offset) {
            return fail(format!("offset {offset} not encodable for a Q pair"));
        }
        let imm7 = ((offset / 16) as u32) & 0x7f;
        let base = if load { 0xAD40_0000 } else { 0xAD00_0000 };
        self.emit(base | (imm7 << 15) | (vt2.bits() << 10) | (rn.bits() << 5) | vt1.bits());
        Ok(())
    }

    /// The imm5 field of the element instructions: the lane size and
    /// `index`.
    fn element(lanes: Lanes, index: u32) -> Result<u32> {
        let size = lanes.size();
        if index >= 16 >> size {
            return fail(format!("lane {index} out of range"));
        }
        Ok((index << (size + 1)) | (1 << size))
    }

    /// INS vd.T[dst], vn.T[src] (MOV element): one lane of vn into one
    /// lane of vd; the other lanes stay.
    pub fn ins_element(
        &mut self,
        lanes: Lanes,
        vd: VReg,
        dst: u32,
        vn: VReg,
        src: u32,
    ) -> Result<()> {
        let imm5 = Self::element(lanes, dst)?;
        Self::element(lanes, src)?;
        let imm4 = src << lanes.size();
        self.emit(0x6E00_0400 | (imm5 << 16) | (imm4 << 11) | (vn.bits() << 5) | vd.bits());
        Ok(())
    }

    /// INS vd.T[index], rn (MOV from a general register): the low bits of
    /// rn (W, X for D lanes) into one lane; the other lanes stay.
    pub fn ins_gpr(&mut self, lanes: Lanes, vd: VReg, index: u32, rn: Reg) -> Result<()> {
        let imm5 = Self::element(lanes, index)?;
        self.emit(0x4E00_1C00 | (imm5 << 16) | (rn.bits() << 5) | vd.bits());
        Ok(())
    }

    /// UMOV rd, vn.T[index]: one lane, zero-extended (W, X for D lanes).
    pub fn umov(&mut self, lanes: Lanes, rd: Reg, vn: VReg, index: u32) -> Result<()> {
        let imm5 = Self::element(lanes, index)?;
        let q = u32::from(lanes == Lanes::D) << 30;
        self.emit(0x0E00_3C00 | q | (imm5 << 16) | (vn.bits() << 5) | rd.bits());
        Ok(())
    }

    /// FMOV Sd, Wn / Dd, Xn: the low lane of vd = rn, the rest of vd
    /// zeroed.
    pub fn fmov_from_gpr(&mut self, double: bool, vd: VReg, rn: Reg) {
        let word = if double { 0x9E67_0000 } else { 0x1E27_0000 };
        self.emit(word | (rn.bits() << 5) | vd.bits());
    }

    /// MOV vd.16B, vn.16B (ORR with itself).
    pub fn mov_vector(&mut self, vd: VReg, vn: VReg) {
        self.emit(0x4EA0_1C00 | (vn.bits() << 16) | (vn.bits() << 5) | vd.bits());
    }

    /// Floating point on every lane of 128-bit vectors (`double`: 2D,
    /// otherwise 4S): FADD, FSUB, FMUL, FDIV; FCMEQ (quiet), FCMGE and
    /// FCMGT (signaling: any NaN raises IOC) give all ones where true.
    pub fn vec_fp(&mut self, op: VecFp, double: bool, vd: VReg, vn: VReg, vm: VReg) {
        let base = match op {
            VecFp::Add => 0x4E20_D400,
            VecFp::Sub => 0x4EA0_D400,
            VecFp::Mul => 0x6E20_DC00,
            VecFp::Div => 0x6E20_FC00,
            VecFp::Cmeq => 0x4E20_E400,
            VecFp::Cmge => 0x6E20_E400,
            VecFp::Cmgt => 0x6EA0_E400,
        };
        self.emit(
            base | (u32::from(double) << 22) | (vm.bits() << 16) | (vn.bits() << 5) | vd.bits(),
        );
    }

    /// One-operand operations on every lane of a 128-bit vector.
    pub fn vec_unary(&mut self, op: VecUnary, vd: VReg, vn: VReg) {
        let word = match op {
            VecUnary::Fsqrt { double } => 0x6EA1_F800 | (u32::from(double) << 22),
            // Signed integer lanes to floating point (4S or 2D).
            VecUnary::Scvtf { double } => 0x4E21_D800 | (u32::from(double) << 22),
            // Floating point to signed integer lanes, saturated: toward
            // zero, or to nearest with ties to even.
            VecUnary::Fcvtzs { double } => 0x4EA1_B800 | (u32::from(double) << 22),
            VecUnary::Fcvtns { double } => 0x4E21_A800 | (u32::from(double) << 22),
            // The two low singles to doubles.
            VecUnary::Fcvtl => 0x0E61_7800,
            // Two doubles to the low singles, the high half zeroed.
            VecUnary::Fcvtn => 0x0E61_6800,
            // The two low int32 lanes sign-extended to int64 (SSHLL #0).
            VecUnary::Sxtl => 0x0F20_A400,
            // The two int64 lanes narrowed to the low int32 lanes, the high
            // half zeroed (XTN).
            VecUnary::Xtn => 0x0EA1_2800,
            // NOT of every byte.
            VecUnary::Not => 0x6E20_5800,
            // The unsigned minimum or maximum of the four 32-bit lanes,
            // into the low lane (UMINV, UMAXV).
            VecUnary::Uminv => 0x6EB1_A800,
            VecUnary::Umaxv => 0x6EB0_A800,
        };
        self.emit(word | (vn.bits() << 5) | vd.bits());
    }

    /// BSL: vd = (vd & vn) | (!vd & vm), the bits of vd selecting.
    pub fn bsl(&mut self, vd: VReg, vn: VReg, vm: VReg) {
        self.emit(0x6E60_1C00 | (vm.bits() << 16) | (vn.bits() << 5) | vd.bits());
    }

    /// CMHI: all ones where vn > vm, unsigned (4S or, `double`, 2D).
    pub fn cmhi(&mut self, double: bool, vd: VReg, vn: VReg, vm: VReg) {
        let size = if double { 3 } else { 2 };
        self.emit(0x6E20_3400 | (size << 22) | (vm.bits() << 16) | (vn.bits() << 5) | vd.bits());
    }

    /// DUP vd.4S (or `double`, 2D), rn: every lane = rn.
    pub fn dup(&mut self, double: bool, vd: VReg, rn: Reg) {
        let imm5 = if double { 0b01000 } else { 0b00100 };
        self.emit(0x4E00_0C00 | (imm5 << 16) | (rn.bits() << 5) | vd.bits());
    }

    /// vd = vn op vm, lane by lane.
    pub fn vec3(&mut self, op: Vec3, lanes: Lanes, vd: VReg, vn: VReg, vm: VReg) -> Result<()> {
        let size = lanes.size();
        // (U, opcode) of the "three same" group; the logical operations
        // use the size field as part of the opcode.
        let (u, opcode, size) = match op {
            Vec3::Add => (0, 0b10000, size),
            Vec3::Sub => (1, 0b10000, size),
            Vec3::Cmeq => (1, 0b10001, size),
            Vec3::Cmgt => (0, 0b00110, size),
            Vec3::Smax | Vec3::Smin | Vec3::Umax | Vec3::Umin => {
                if lanes == Lanes::D {
                    return fail("no 64-bit lanes for min/max");
                }
                match op {
                    Vec3::Smax => (0, 0b01100, size),
                    Vec3::Smin => (0, 0b01101, size),
                    Vec3::Umax => (1, 0b01100, size),
                    _ => (1, 0b01101, size),
                }
            }
            Vec3::And => (0, 0b00011, 0),
            Vec3::Bic => (0, 0b00011, 1),
            Vec3::Orr => (0, 0b00011, 2),
            Vec3::Eor => (1, 0b00011, 0),
            Vec3::Zip1 | Vec3::Zip2 => {
                let opcode = if op == Vec3::Zip1 { 0b011 } else { 0b111 };
                self.emit(
                    0x4E00_0800
                        | (size << 22)
                        | (vm.bits() << 16)
                        | (opcode << 12)
                        | (vn.bits() << 5)
                        | vd.bits(),
                );
                return Ok(());
            }
        };
        self.emit(
            0x4E20_0400
                | (u << 29)
                | (size << 22)
                | (vm.bits() << 16)
                | (opcode << 11)
                | (vn.bits() << 5)
                | vd.bits(),
        );
        Ok(())
    }

    /// Every lane of vn shifted by `amount` into vd.
    pub fn vshift(
        &mut self,
        op: VShift,
        lanes: Lanes,
        vd: VReg,
        vn: VReg,
        amount: u32,
    ) -> Result<()> {
        let bits = lanes.bits();
        let (u, opcode, field) = match op {
            VShift::Shl if amount < bits => (0, 0b01010, bits + amount),
            VShift::Ushr if (1..=bits).contains(&amount) => (1, 0b00000, 2 * bits - amount),
            VShift::Sshr if (1..=bits).contains(&amount) => (0, 0b00000, 2 * bits - amount),
            _ => {
                return fail(format!(
                    "shift of {amount} out of range for {bits}-bit lanes"
                ));
            }
        };
        self.emit(
            0x4F00_0400 | (u << 29) | (field << 16) | (opcode << 11) | (vn.bits() << 5) | vd.bits(),
        );
        Ok(())
    }

    /// EXT: bytes `index..16` of vn, then bytes `0..index` of vm.
    pub fn ext(&mut self, vd: VReg, vn: VReg, vm: VReg, index: u32) -> Result<()> {
        if index > 15 {
            return fail("ext index above 15");
        }
        self.emit(0x6E00_0000 | (vm.bits() << 16) | (index << 11) | (vn.bits() << 5) | vd.bits());
        Ok(())
    }

    // --- Scalar floating point (S: single, D: double) ----------------

    /// LDR St/Dt or STR St/Dt with an unsigned offset, a multiple of the
    /// size.
    pub fn load_store_fp(
        &mut self,
        load: bool,
        double: bool,
        vt: VReg,
        rn: Reg,
        offset: u32,
    ) -> Result<()> {
        let scale = if double { 3 } else { 2 };
        if !offset.is_multiple_of(1 << scale) || offset >> scale >= 1 << 12 {
            return fail(format!("offset {offset} not encodable for a scalar"));
        }
        let base = match (load, double) {
            (true, false) => 0xBD40_0000,
            (true, true) => 0xFD40_0000,
            (false, false) => 0xBD00_0000,
            (false, true) => 0xFD00_0000,
        };
        self.emit(base | ((offset >> scale) << 10) | (rn.bits() << 5) | vt.bits());
        Ok(())
    }

    /// FADD, FSUB, FMUL, FDIV: vd = vn op vm.
    pub fn fp_arith(&mut self, op: FpOp, double: bool, vd: VReg, vn: VReg, vm: VReg) {
        let opcode = match op {
            FpOp::Mul => 0b0000,
            FpOp::Div => 0b0001,
            FpOp::Add => 0b0010,
            FpOp::Sub => 0b0011,
        };
        self.emit(
            0x1E20_0800
                | (u32::from(double) << 22)
                | (vm.bits() << 16)
                | (opcode << 12)
                | (vn.bits() << 5)
                | vd.bits(),
        );
    }

    /// FSQRT vd, vn.
    pub fn fsqrt(&mut self, double: bool, vd: VReg, vn: VReg) {
        self.emit(0x1E21_C000 | (u32::from(double) << 22) | (vn.bits() << 5) | vd.bits());
    }

    /// FCVT: single to double (`to_double`) or double to single.
    pub fn fcvt(&mut self, to_double: bool, vd: VReg, vn: VReg) {
        let word = if to_double { 0x1E22_C000 } else { 0x1E62_4000 };
        self.emit(word | (vn.bits() << 5) | vd.bits());
    }

    /// FCMP vn, vm.
    pub fn fcmp(&mut self, double: bool, vn: VReg, vm: VReg) {
        self.emit(0x1E20_2000 | (u32::from(double) << 22) | (vm.bits() << 16) | (vn.bits() << 5));
    }

    /// FCMPE vn, vm: as FCMP, but a quiet NaN operand also raises the
    /// invalid operation exception.
    pub fn fcmpe(&mut self, double: bool, vn: VReg, vm: VReg) {
        self.emit(0x1E20_2010 | (u32::from(double) << 22) | (vm.bits() << 16) | (vn.bits() << 5));
    }

    /// FMOV: rd (W for a single, X for a double) = the bits of vn.
    pub fn fmov_to_gpr(&mut self, double: bool, rd: Reg, vn: VReg) {
        let word = if double { 0x9E66_0000 } else { 0x1E26_0000 };
        self.emit(word | (vn.bits() << 5) | rd.bits());
    }

    /// FCSEL vd = cond ? vn : vm.
    pub fn fcsel(&mut self, double: bool, vd: VReg, vn: VReg, vm: VReg, cond: Cond) {
        self.emit(
            0x1E20_0C00
                | (u32::from(double) << 22)
                | (vm.bits() << 16)
                | ((cond as u32) << 12)
                | (vn.bits() << 5)
                | vd.bits(),
        );
    }

    /// SCVTF: vd = the signed integer in rn (32 or 64 bits).
    pub fn scvtf(&mut self, w: Width, double: bool, vd: VReg, rn: Reg) {
        self.emit(0x1E22_0000 | w.sf() | (u32::from(double) << 22) | (rn.bits() << 5) | vd.bits());
    }

    /// FCVTZS (`truncate`) or FCVTNS (to nearest, ties to even): rd = the
    /// signed integer of vn, saturated.
    pub fn fcvt_to_int(&mut self, truncate: bool, w: Width, double: bool, rd: Reg, vn: VReg) {
        let base = if truncate { 0x1E38_0000 } else { 0x1E20_0000 };
        self.emit(base | w.sf() | (u32::from(double) << 22) | (vn.bits() << 5) | rd.bits());
    }

    /// MOVI vd.2D, #0.
    pub fn movi_zero(&mut self, vd: VReg) {
        self.emit(0x6F00_E400 | vd.bits());
    }

    /// MOVI vd.16B, #imm8: every byte of vd.
    pub fn movi_bytes(&mut self, vd: VReg, imm8: u8) {
        let imm = u32::from(imm8);
        self.emit(0x4F00_E400 | (imm >> 5) << 16 | (imm & 0x1f) << 5 | vd.bits());
    }

    /// TBL vd.16B, {vn.16B}, vm.16B: byte i of vd is byte vm[i] of vn, or
    /// 0 when vm[i] is 16 or more.
    pub fn tbl(&mut self, vd: VReg, vn: VReg, vm: VReg) {
        self.emit(0x4E00_0000 | vm.bits() << 16 | vn.bits() << 5 | vd.bits());
    }

    /// SQXTN (signed saturation) or SQXTUN (signed to unsigned
    /// saturation): narrows the lanes of vn (`from`: H, S or D) into the
    /// low half of vd, zeroing the high half; with `upper` (the "2" forms)
    /// into the high half, keeping the low one.
    pub fn narrow(
        &mut self,
        unsigned: bool,
        upper: bool,
        from: Lanes,
        vd: VReg,
        vn: VReg,
    ) -> Result<()> {
        if from == Lanes::B {
            return fail("cannot narrow bytes");
        }
        let base = if unsigned { 0x2E21_2800 } else { 0x0E21_4800 };
        self.emit(
            base | (u32::from(upper) << 30)
                | ((from.size() - 1) << 22)
                | (vn.bits() << 5)
                | vd.bits(),
        );
        Ok(())
    }
}

/// Floating point on vector lanes (`Assembler::vec_fp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecFp {
    Add,
    Sub,
    Mul,
    Div,
    Cmeq,
    Cmge,
    Cmgt,
}

/// One-operand vector operations (`Assembler::vec_unary`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecUnary {
    Fsqrt { double: bool },
    Scvtf { double: bool },
    Fcvtzs { double: bool },
    Fcvtns { double: bool },
    Fcvtl,
    Fcvtn,
    Sxtl,
    Xtn,
    Not,
    Uminv,
    Umaxv,
}

/// Scalar floating-point arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// Data processing with one source register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Data1 {
    /// Reverse the bits.
    Rbit,
    /// Reverse the bytes.
    Rev,
    /// Count leading zeros.
    Clz,
}

/// The logical operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Logic {
    And,
    Orr,
    Eor,
    /// AND setting flags (TST with rd = ZR).
    Ands,
}

impl Logic {
    fn base_reg(self) -> u32 {
        match self {
            Logic::And => 0x0A00_0000,
            Logic::Orr => 0x2A00_0000,
            Logic::Eor => 0x4A00_0000,
            Logic::Ands => 0x6A00_0000,
        }
    }

    fn base_imm(self) -> u32 {
        match self {
            Logic::And => 0x1200_0000,
            Logic::Orr => 0x3200_0000,
            Logic::Eor => 0x5200_0000,
            Logic::Ands => 0x7200_0000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    Csel,
    Csinc,
    Csinv,
    Csneg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairMode {
    Offset,
    PreIndex,
    PostIndex,
}

/// The branch offset field for a `delta` in instructions.
fn branch_field(kind: FixupKind, delta: i64) -> Result<u32> {
    if let FixupKind::Adr = kind {
        let bytes = delta * 4;
        if !(-(1 << 20)..1 << 20).contains(&bytes) {
            return fail(format!("adr of {bytes} bytes out of range"));
        }
        let field = (bytes as u32) & 0x1f_ffff;
        return Ok(((field & 3) << 29) | ((field >> 2) << 5));
    }
    let bits = match kind {
        FixupKind::Imm26 => 26,
        FixupKind::Imm19 => 19,
        FixupKind::Imm14 => 14,
        FixupKind::Adr => unreachable!("handled above"),
    };
    let limit = 1i64 << (bits - 1);
    if !(-limit..limit).contains(&delta) {
        return fail(format!("branch of {delta} instructions out of range"));
    }
    let field = (delta as u32) & ((1u32 << bits) - 1);
    Ok(match kind {
        FixupKind::Imm26 => field,
        FixupKind::Imm19 | FixupKind::Imm14 => field << 5,
        FixupKind::Adr => unreachable!("handled above"),
    })
}

/// B from the instruction at address `from` to address `to`, for patching
/// code already in place (block chaining).
pub fn branch_word(from: usize, to: usize) -> Result<u32> {
    let delta = (to as i64).wrapping_sub(from as i64);
    if delta % 4 != 0 {
        return fail("branch between unaligned addresses");
    }
    Ok(0x1400_0000 | branch_field(FixupKind::Imm26, delta / 4)?)
}

/// The (N, immr, imms) fields of a logical immediate, or `None` if `value`
/// is not a repeating pattern of a rotated run of ones (the only values
/// AArch64 logical instructions accept; all zeros and all ones are not).
pub fn encode_bitmask(value: u64, width: u32) -> Option<(u32, u32, u32)> {
    let mask = if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    let value = value & mask;
    if value == 0 || value == mask {
        return None;
    }
    // The smallest element size whose pattern repeats over `width`.
    let mut size = width;
    while size > 2 {
        let half = size / 2;
        let low = value & ((1u64 << half) - 1);
        let repeated = (0..width / half).fold(0u64, |acc, i| acc | (low << (i * half)));
        if repeated & mask != value {
            break;
        }
        size = half;
    }
    let element_mask = if size == 64 {
        u64::MAX
    } else {
        (1u64 << size) - 1
    };
    let element = value & element_mask;
    let ones = element.count_ones();
    // Rotate right until the run of ones starts at bit 0.
    let rotate = (0..size).find(|&r| {
        let rotated = rotate_right(element, r, size);
        rotated == (1u64 << ones) - 1
    })?;
    let immr = (size - rotate) % size;
    let n = u32::from(size == 64);
    // imms: the size encoded in its high bits, ones - 1 in the low bits.
    let size_code = (!(size * 2 - 1)) & 0x3f;
    let imms = (size_code | (ones - 1)) & 0x3f;
    Some((n, immr, imms))
}

fn rotate_right(value: u64, amount: u32, size: u32) -> u64 {
    if amount == 0 {
        return value;
    }
    let mask = if size == 64 {
        u64::MAX
    } else {
        (1u64 << size) - 1
    };
    ((value >> amount) | (value << (size - amount))) & mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_words_from_the_gnu_assembler() {
        // Assembled with GNU as 2.44 on the host (2026-10-12).
        let mut a = Assembler::new();
        a.add_imm(Width::X, Reg::new(1), Reg::new(2), 0x10).unwrap();
        a.subs(
            Width::W,
            Reg::new(3),
            Reg::new(4),
            Reg::new(5),
            Shift::Lsl,
            2,
        )
        .unwrap();
        a.ldr(MemSize::B64, Reg::new(0), Reg::new(1), 8).unwrap();
        let skip = a.new_label();
        a.b_cond(Cond::Eq, skip);
        a.bind(skip);
        a.ret();
        a.movz(Width::X, Reg::new(9), 0x1234, 1).unwrap();
        a.logic_imm(Logic::And, Width::X, Reg::new(1), Reg::new(2), 0xff)
            .unwrap();
        assert_eq!(
            a.finish().unwrap(),
            [
                0x9100_4041,
                0x6b05_0883,
                0xf940_0420,
                0x5400_0020,
                0xd65f_03c0,
                0xd2a2_4689,
                0x9240_1c41
            ]
        );
    }

    /// Runs a MOVZ/MOVN/MOVK sequence on one register.
    fn move_value(words: &[u32]) -> u64 {
        let mut value = 0u64;
        for &word in words {
            let imm = u64::from((word >> 5) & 0xffff);
            let shift = 16 * ((word >> 21) & 3);
            value = match word & 0xff80_0000 {
                0xd280_0000 => imm << shift,
                0x9280_0000 => !(imm << shift),
                0xf280_0000 => (value & !(0xffff << shift)) | (imm << shift),
                other => panic!("not a 64-bit move: {other:#x}"),
            };
        }
        value
    }

    #[test]
    fn any_64_bit_constant_in_at_most_four_moves() {
        for value in [
            0,
            1,
            0xffff,
            0x1_0000,
            0x1234_5678_9abc_def0,
            u64::MAX,
            0xffff_ffff_ffff_0000,
            0xffff_1234_ffff_ffff,
            0x8000_0000_0000_0000,
            0x0000_ffff_0000_ffff,
        ] {
            let mut a = Assembler::new();
            a.mov_imm64(Reg::new(5), value);
            let words = a.finish().unwrap();
            assert!(words.len() <= 4, "{value:#x}: {} words", words.len());
            assert_eq!(move_value(&words), value, "{value:#x}");
        }
    }

    #[test]
    fn bitmask_immediates() {
        assert_eq!(encode_bitmask(0xff, 64), Some((1, 0, 7)));
        assert_eq!(
            encode_bitmask(0x5555_5555_5555_5555, 64),
            Some((0, 0, 0x3c))
        );
        assert_eq!(encode_bitmask(0, 64), None);
        assert_eq!(encode_bitmask(u64::MAX, 64), None);
        assert_eq!(encode_bitmask(0x1234, 64), None);
    }

    #[test]
    fn unbound_labels_and_out_of_range_operands_are_errors() {
        let mut a = Assembler::new();
        let nowhere = a.new_label();
        a.b(nowhere);
        assert!(a.finish().is_err());
        let mut a = Assembler::new();
        assert!(
            a.add_imm(Width::X, Reg::new(0), Reg::new(0), 0x1001)
                .is_err()
        );
        assert!(a.ldr(MemSize::B64, Reg::new(0), Reg::new(0), 4).is_err());
        assert!(
            a.pair(true, PairMode::Offset, Reg::new(0), Reg::new(1), SP, 512)
                .is_err()
        );
    }
}
