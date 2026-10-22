//! Samples for `cargo xtask encoder-check`: each pairs the words the
//! encoder emits with the same instruction written as assembly text, which
//! the GNU assembler turns into the reference words. The text is built
//! from the operands, not from the encoding, so a wrong bit shows up.

use crate::encoder::{
    Assembler, AtomicOp, Barrier, Cond, Data1, FpOp, Lanes, Logic, MemSize, PairMode, Reg, SP,
    Select, Shift, VReg, VShift, Vec3, VecFp, VecUnary, Width, ZR,
};

pub struct Sample {
    pub text: String,
    pub words: Vec<u32>,
}

const REGS: [u8; 5] = [0, 1, 15, 30, 31];

/// Register 31 as the zero register or as SP.
#[derive(Clone, Copy)]
enum R31 {
    Zero,
    Sp,
}

fn name(w: Width, n: u8, r31: R31) -> String {
    match (w, n, r31) {
        (Width::X, 31, R31::Zero) => "xzr".into(),
        (Width::W, 31, R31::Zero) => "wzr".into(),
        (Width::X, 31, R31::Sp) => "sp".into(),
        (Width::W, 31, R31::Sp) => "wsp".into(),
        (Width::X, n, _) => format!("x{n}"),
        (Width::W, n, _) => format!("w{n}"),
    }
}

fn shift_name(s: Shift) -> &'static str {
    match s {
        Shift::Lsl => "lsl",
        Shift::Lsr => "lsr",
        Shift::Asr => "asr",
        Shift::Ror => "ror",
    }
}

fn cond_name(c: Cond) -> &'static str {
    [
        "eq", "ne", "hs", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "al",
    ][c as usize]
}

const CONDS: [Cond; 14] = [
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
];

struct Samples(Vec<Sample>);

impl Samples {
    fn add(&mut self, text: String, build: impl FnOnce(&mut Assembler)) {
        let mut a = Assembler::new();
        build(&mut a);
        let words = a.finish().expect("samples only use valid operands");
        self.0.push(Sample { text, words });
    }
}

/// Every sample: a few thousand instructions.
pub fn samples() -> Vec<Sample> {
    let mut s = Samples(Vec::new());
    let r = Reg::new;
    let widths = [Width::W, Width::X];

    for w in widths {
        let hws: &[u32] = if w == Width::X {
            &[0, 1, 2, 3]
        } else {
            &[0, 1]
        };
        for &rd in &REGS {
            for imm in [0u16, 1, 0x1234, 0xffff] {
                for &hw in hws {
                    for (op, mnemonic) in [(0, "movz"), (1, "movn"), (2, "movk")] {
                        s.add(
                            format!(
                                "{mnemonic} {}, #{imm:#x}, lsl #{}",
                                name(w, rd, R31::Zero),
                                hw * 16
                            ),
                            |a| {
                                let f = match op {
                                    0 => Assembler::movz,
                                    1 => Assembler::movn,
                                    _ => Assembler::movk,
                                };
                                f(a, w, r(rd), imm, hw).unwrap();
                            },
                        );
                    }
                }
            }
        }
    }

    for w in widths {
        for &rd in &REGS {
            for &rn in &REGS {
                for imm in [0u32, 1, 0xfff, 0x1000, 0xfff000] {
                    let imm_text = if imm >= 0x1000 {
                        format!("#{:#x}, lsl #12", imm >> 12)
                    } else {
                        format!("#{imm:#x}")
                    };
                    s.add(
                        format!(
                            "add {}, {}, {imm_text}",
                            name(w, rd, R31::Sp),
                            name(w, rn, R31::Sp)
                        ),
                        |a| a.add_imm(w, r(rd), r(rn), imm).unwrap(),
                    );
                    s.add(
                        format!(
                            "sub {}, {}, {imm_text}",
                            name(w, rd, R31::Sp),
                            name(w, rn, R31::Sp)
                        ),
                        |a| a.sub_imm(w, r(rd), r(rn), imm).unwrap(),
                    );
                    s.add(
                        format!(
                            "adds {}, {}, {imm_text}",
                            name(w, rd, R31::Zero),
                            name(w, rn, R31::Sp)
                        ),
                        |a| a.adds_imm(w, r(rd), r(rn), imm).unwrap(),
                    );
                    s.add(
                        format!(
                            "subs {}, {}, {imm_text}",
                            name(w, rd, R31::Zero),
                            name(w, rn, R31::Sp)
                        ),
                        |a| a.subs_imm(w, r(rd), r(rn), imm).unwrap(),
                    );
                }
            }
        }
    }

    for w in widths {
        let bits = if w == Width::X { 64 } else { 32 };
        for &rd in &REGS {
            for &(rn, rm) in &[(0u8, 1u8), (15, 30), (31, 31), (30, 0)] {
                for shift in [Shift::Lsl, Shift::Lsr, Shift::Asr] {
                    for amount in [0, 1, bits - 1] {
                        let ops = |m: &str| {
                            format!(
                                "{m} {}, {}, {}, {} #{amount}",
                                name(w, rd, R31::Zero),
                                name(w, rn, R31::Zero),
                                name(w, rm, R31::Zero),
                                shift_name(shift)
                            )
                        };
                        s.add(ops("add"), |a| {
                            a.add(w, r(rd), r(rn), r(rm), shift, amount).unwrap()
                        });
                        s.add(ops("adds"), |a| {
                            a.adds(w, r(rd), r(rn), r(rm), shift, amount).unwrap()
                        });
                        s.add(ops("sub"), |a| {
                            a.sub(w, r(rd), r(rn), r(rm), shift, amount).unwrap()
                        });
                        s.add(ops("subs"), |a| {
                            a.subs(w, r(rd), r(rn), r(rm), shift, amount).unwrap()
                        });
                    }
                }
                for shift in [Shift::Lsl, Shift::Lsr, Shift::Asr, Shift::Ror] {
                    for amount in [0, 7, bits - 1] {
                        for (op, plain, inverted) in [
                            (Logic::And, "and", "bic"),
                            (Logic::Orr, "orr", "orn"),
                            (Logic::Eor, "eor", "eon"),
                            (Logic::Ands, "ands", "bics"),
                        ] {
                            for invert in [false, true] {
                                let m = if invert { inverted } else { plain };
                                s.add(
                                    format!(
                                        "{m} {}, {}, {}, {} #{amount}",
                                        name(w, rd, R31::Zero),
                                        name(w, rn, R31::Zero),
                                        name(w, rm, R31::Zero),
                                        shift_name(shift)
                                    ),
                                    |a| {
                                        a.logic(op, invert, w, r(rd), r(rn), r(rm), shift, amount)
                                            .unwrap()
                                    },
                                );
                            }
                        }
                    }
                }
                let three = |m: &str| {
                    format!(
                        "{m} {}, {}, {}",
                        name(w, rd, R31::Zero),
                        name(w, rn, R31::Zero),
                        name(w, rm, R31::Zero)
                    )
                };
                for flags in [false, true] {
                    let suffix = if flags { "s" } else { "" };
                    s.add(three(&format!("adc{suffix}")), |a| {
                        a.adc(w, r(rd), r(rn), r(rm), flags)
                    });
                    s.add(three(&format!("sbc{suffix}")), |a| {
                        a.sbc(w, r(rd), r(rn), r(rm), flags)
                    });
                }
                s.add(three("udiv"), |a| a.div(false, w, r(rd), r(rn), r(rm)));
                s.add(three("sdiv"), |a| a.div(true, w, r(rd), r(rn), r(rm)));
                for (shift, m) in [
                    (Shift::Lsl, "lslv"),
                    (Shift::Lsr, "lsrv"),
                    (Shift::Asr, "asrv"),
                    (Shift::Ror, "rorv"),
                ] {
                    s.add(three(m), |a| a.shift_reg(shift, w, r(rd), r(rn), r(rm)));
                }
                for &ra in &[0u8, 15, 31] {
                    let four = |m: &str| format!("{}, {}", three(m), name(w, ra, R31::Zero));
                    s.add(four("madd"), |a| a.madd(w, r(rd), r(rn), r(rm), r(ra)));
                    s.add(four("msub"), |a| a.msub(w, r(rd), r(rn), r(rm), r(ra)));
                }
                for cond in CONDS {
                    let sel = |m: &str| format!("{}, {}", three(m), cond_name(cond));
                    s.add(sel("csel"), |a| {
                        a.csel(Select::Csel, w, r(rd), r(rn), r(rm), cond)
                    });
                    s.add(sel("csinc"), |a| {
                        a.csel(Select::Csinc, w, r(rd), r(rn), r(rm), cond)
                    });
                    s.add(sel("csinv"), |a| {
                        a.csel(Select::Csinv, w, r(rd), r(rn), r(rm), cond)
                    });
                    s.add(sel("csneg"), |a| {
                        a.csel(Select::Csneg, w, r(rd), r(rn), r(rm), cond)
                    });
                }
            }
            for cond in CONDS {
                s.add(
                    format!("cset {}, {}", name(w, rd, R31::Zero), cond_name(cond)),
                    |a| a.cset(w, r(rd), cond),
                );
            }
            for &rn in &[0u8, 15, 31] {
                for amount in [1, 7, bits - 1] {
                    for (shift, m) in [
                        (Shift::Lsl, "lsl"),
                        (Shift::Lsr, "lsr"),
                        (Shift::Asr, "asr"),
                        (Shift::Ror, "ror"),
                    ] {
                        s.add(
                            format!(
                                "{m} {}, {}, #{amount}",
                                name(w, rd, R31::Zero),
                                name(w, rn, R31::Zero)
                            ),
                            |a| a.shift_imm(shift, w, r(rd), r(rn), amount).unwrap(),
                        );
                    }
                }
                for (immr, imms) in [(0, 7), (4, 15), (bits - 1, bits - 1)] {
                    for (signed, m) in [(false, "ubfm"), (true, "sbfm")] {
                        s.add(
                            format!(
                                "{m} {}, {}, #{immr}, #{imms}",
                                name(w, rd, R31::Zero),
                                name(w, rn, R31::Zero)
                            ),
                            |a| a.bfm(signed, w, r(rd), r(rn), immr, imms).unwrap(),
                        );
                    }
                }
                for (immr, imms) in [
                    (0, 7),
                    (0, 15),
                    (bits - 8, 7),
                    (4, 15),
                    (bits - 1, bits - 1),
                ] {
                    s.add(
                        format!(
                            "bfm {}, {}, #{immr}, #{imms}",
                            name(w, rd, R31::Zero),
                            name(w, rn, R31::Zero)
                        ),
                        |a| a.bfm_insert(w, r(rd), r(rn), immr, imms).unwrap(),
                    );
                }
            }
        }
    }

    for &rd in &REGS {
        for &(rn, rm) in &[(0u8, 1u8), (31, 30)] {
            let ops = format!(
                "{}, {}, {}",
                name(Width::X, rd, R31::Zero),
                name(Width::X, rn, R31::Zero),
                name(Width::X, rm, R31::Zero)
            );
            s.add(format!("smulh {ops}"), |a| {
                a.mulh(true, r(rd), r(rn), r(rm))
            });
            s.add(format!("umulh {ops}"), |a| {
                a.mulh(false, r(rd), r(rn), r(rm))
            });
        }
    }

    let masks64 = [
        0xffu64,
        0x1,
        0x8000_0000_0000_0000,
        0x5555_5555_5555_5555,
        0x0f0f_0f0f_0f0f_0f0f,
        0xffff_0000_ffff_0000,
        0x7fff_ffff_ffff_ffff,
        0x00ff_ff00_0000_0000,
        0xfffe,
    ];
    let masks32 = [
        0xffu64,
        0x1,
        0x8000_0000,
        0x5555_5555,
        0xffff_0000,
        0x7fff_ffff,
        0x0ff0,
    ];
    for w in widths {
        let masks: &[u64] = if w == Width::X { &masks64 } else { &masks32 };
        for &rd in &REGS {
            for &rn in &[0u8, 15, 31] {
                for &imm in masks {
                    for (op, m) in [
                        (Logic::And, "and"),
                        (Logic::Orr, "orr"),
                        (Logic::Eor, "eor"),
                        (Logic::Ands, "ands"),
                    ] {
                        let rd_kind = if op == Logic::Ands {
                            R31::Zero
                        } else {
                            R31::Sp
                        };
                        s.add(
                            format!(
                                "{m} {}, {}, #{imm:#x}",
                                name(w, rd, rd_kind),
                                name(w, rn, R31::Zero)
                            ),
                            |a| a.logic_imm(op, w, r(rd), r(rn), imm).unwrap(),
                        );
                    }
                }
            }
        }
    }

    let sizes = [
        (MemSize::B8, "b", 1u32),
        (MemSize::B16, "h", 2),
        (MemSize::B32, "", 4),
        (MemSize::B64, "", 8),
    ];
    for &(size, suffix, bytes) in &sizes {
        let reg_w = if size == MemSize::B64 {
            Width::X
        } else {
            Width::W
        };
        for &rt in &[0u8, 15, 31] {
            for &rn in &[0u8, 15, 31] {
                for offset in [0, bytes, bytes * 4095] {
                    let addr = format!("[{}, #{offset}]", name(Width::X, rn, R31::Sp));
                    let t = name(reg_w, rt, R31::Zero);
                    s.add(format!("ldr{suffix} {t}, {addr}"), |a| {
                        a.ldr(size, r(rt), r(rn), offset).unwrap()
                    });
                    s.add(format!("str{suffix} {t}, {addr}"), |a| {
                        a.str(size, r(rt), r(rn), offset).unwrap()
                    });
                    if size != MemSize::B64 {
                        let signed_suffix = if size == MemSize::B32 { "w" } else { suffix };
                        s.add(
                            format!(
                                "ldrs{signed_suffix} {}, {addr}",
                                name(Width::X, rt, R31::Zero)
                            ),
                            |a| a.ldrs(size, r(rt), r(rn), offset).unwrap(),
                        );
                    }
                }
                for &rm in &[1u8, 30] {
                    for scaled in [false, true] {
                        let shift = if scaled {
                            format!(", lsl #{}", bytes.trailing_zeros())
                        } else {
                            String::new()
                        };
                        let addr = format!("[{}, x{rm}{shift}]", name(Width::X, rn, R31::Sp));
                        let t = name(reg_w, rt, R31::Zero);
                        s.add(format!("ldr{suffix} {t}, {addr}"), |a| {
                            a.load_store_reg(true, size, r(rt), r(rn), r(rm), scaled)
                        });
                        s.add(format!("str{suffix} {t}, {addr}"), |a| {
                            a.load_store_reg(false, size, r(rt), r(rn), r(rm), scaled)
                        });
                    }
                }
            }
        }
    }

    for &(rt, rt2) in &[(0u8, 1u8), (19, 20), (29, 30)] {
        for &rn in &[0u8, 31] {
            let base = name(Width::X, rn, R31::Sp);
            for offset in [-512, -16, 0, 8, 504] {
                for load in [false, true] {
                    let m = if load { "ldp" } else { "stp" };
                    s.add(format!("{m} x{rt}, x{rt2}, [{base}, #{offset}]"), |a| {
                        a.pair(load, PairMode::Offset, r(rt), r(rt2), r(rn), offset)
                            .unwrap()
                    });
                    s.add(format!("{m} x{rt}, x{rt2}, [{base}, #{offset}]!"), |a| {
                        a.pair(load, PairMode::PreIndex, r(rt), r(rt2), r(rn), offset)
                            .unwrap()
                    });
                    s.add(format!("{m} x{rt}, x{rt2}, [{base}], #{offset}"), |a| {
                        a.pair(load, PairMode::PostIndex, r(rt), r(rt2), r(rn), offset)
                            .unwrap()
                    });
                }
            }
        }
    }

    // Branches, forward over two nops and backward over one.
    s.add("b .+12\nnop\nnop".into(), |a| {
        let l = a.new_label();
        a.b(l);
        a.nop();
        a.nop();
        a.bind(l);
    });
    s.add("nop\nbl .-4".into(), |a| {
        let l = a.new_label();
        a.bind(l);
        a.nop();
        a.bl(l);
    });
    for cond in CONDS {
        s.add(format!("b.{} .+8\nnop", cond_name(cond)), |a| {
            let l = a.new_label();
            a.b_cond(cond, l);
            a.nop();
            a.bind(l);
        });
    }
    for w in widths {
        for &rt in &[0u8, 30] {
            for (nonzero, m) in [(false, "cbz"), (true, "cbnz")] {
                s.add(format!("nop\n{m} {}, .-4", name(w, rt, R31::Zero)), |a| {
                    let l = a.new_label();
                    a.bind(l);
                    a.nop();
                    a.cbz(nonzero, w, r(rt), l);
                });
            }
        }
    }
    for bit in [0u32, 5, 31, 32, 63] {
        for (nonzero, m) in [(false, "tbz"), (true, "tbnz")] {
            let reg = if bit < 32 { "w7" } else { "x7" };
            s.add(format!("{m} {reg}, #{bit}, .+8\nnop"), |a| {
                let l = a.new_label();
                a.tbz(nonzero, r(7), bit, l);
                a.nop();
                a.bind(l);
            });
        }
    }
    for &rd in &[0u8, 10, 30] {
        s.add(format!("adr x{rd}, .+12\nnop\nnop"), |a| {
            let l = a.new_label();
            a.adr(r(rd), l);
            a.nop();
            a.nop();
            a.bind(l);
        });
        s.add(format!("nop\nnop\nadr x{rd}, .-8"), |a| {
            let l = a.new_label();
            a.bind(l);
            a.nop();
            a.nop();
            a.adr(r(rd), l);
        });
    }
    // Patched branches, by distance in bytes, up to the limits of B.
    for delta in [0i64, 4, -4, 0x1000, -0x10_0000, 0x7ff_fffc, -0x800_0000] {
        let sign = if delta < 0 { '-' } else { '+' };
        s.add(format!("b .{sign}{}", delta.unsigned_abs()), |a| {
            let word = crate::encoder::branch_word(0x1000_0000, (0x1000_0000 + delta) as usize);
            a.raw(word.unwrap());
        });
    }
    for w in widths {
        for &(rd, rn) in &[(0u8, 1u8), (30, 17), (31, 31)] {
            for (op, m) in [
                (Data1::Rbit, "rbit"),
                (Data1::Rev, "rev"),
                (Data1::Clz, "clz"),
            ] {
                s.add(
                    format!("{m} {}, {}", name(w, rd, R31::Zero), name(w, rn, R31::Zero)),
                    |a| a.data1(op, w, r(rd), r(rn)),
                );
            }
        }
    }

    // Ordered accesses and barriers (ADR-0023).
    for &(size, suffix) in &[
        (MemSize::B8, "b"),
        (MemSize::B16, "h"),
        (MemSize::B32, ""),
        (MemSize::B64, ""),
    ] {
        let w = if size == MemSize::B64 {
            Width::X
        } else {
            Width::W
        };
        for &(rt, rn) in &[(0u8, 1u8), (15, 7), (30, 31)] {
            let t = name(w, rt, R31::Zero);
            let addr = format!("[{}]", name(Width::X, rn, R31::Sp));
            s.add(format!("ldapr{suffix} {t}, {addr}"), |a| {
                a.ldapr(size, r(rt), r(rn))
            });
            s.add(format!("stlr{suffix} {t}, {addr}"), |a| {
                a.stlr(size, r(rt), r(rn))
            });
        }
    }
    // LSE atomics (ADR-0023 step 4).
    for &(size, suffix) in &[
        (MemSize::B8, "b"),
        (MemSize::B16, "h"),
        (MemSize::B32, ""),
        (MemSize::B64, ""),
    ] {
        let w = if size == MemSize::B64 {
            Width::X
        } else {
            Width::W
        };
        for &(rs, rt, rn) in &[(0u8, 1u8, 2u8), (15, 7, 30), (29, 30, 31)] {
            let (s_, t) = (name(w, rs, R31::Zero), name(w, rt, R31::Zero));
            let addr = format!("[{}]", name(Width::X, rn, R31::Sp));
            for (op, m) in [
                (AtomicOp::Add, "ldaddal"),
                (AtomicOp::Clr, "ldclral"),
                (AtomicOp::Eor, "ldeoral"),
                (AtomicOp::Set, "ldsetal"),
                (AtomicOp::Swp, "swpal"),
            ] {
                s.add(format!("{m}{suffix} {s_}, {t}, {addr}"), |a| {
                    a.atomic(op, size, r(rs), r(rt), r(rn))
                });
            }
            s.add(format!("casal{suffix} {s_}, {t}, {addr}"), |a| {
                a.casal(size, r(rs), r(rt), r(rn))
            });
        }
    }
    for (barrier, text) in [
        (Barrier::Ish, "dmb ish"),
        (Barrier::IshLd, "dmb ishld"),
        (Barrier::IshSt, "dmb ishst"),
    ] {
        s.add(text.to_string(), |a| a.dmb(barrier));
    }

    // Scalar floating point.
    let v = VReg::new;
    for (double, p) in [(false, 's'), (true, 'd')] {
        let max = if double { 32760u32 } else { 16380 };
        for &(vt, rn, offset) in &[(0u8, 0u8, 0u32), (2, 7, 160), (31, 30, max)] {
            s.add(format!("ldr {p}{vt}, [x{rn}, #{offset}]"), |a| {
                a.load_store_fp(true, double, v(vt), r(rn), offset).unwrap()
            });
            s.add(format!("str {p}{vt}, [x{rn}, #{offset}]"), |a| {
                a.load_store_fp(false, double, v(vt), r(rn), offset)
                    .unwrap()
            });
        }
        for (op, m) in [
            (FpOp::Add, "fadd"),
            (FpOp::Sub, "fsub"),
            (FpOp::Mul, "fmul"),
            (FpOp::Div, "fdiv"),
        ] {
            s.add(format!("{m} {p}2, {p}0, {p}1"), |a| {
                a.fp_arith(op, double, v(2), v(0), v(1))
            });
            s.add(format!("{m} {p}31, {p}17, {p}9"), |a| {
                a.fp_arith(op, double, v(31), v(17), v(9))
            });
        }
        s.add(format!("fsqrt {p}2, {p}1"), |a| a.fsqrt(double, v(2), v(1)));
        s.add(format!("fcmp {p}0, {p}1"), |a| a.fcmp(double, v(0), v(1)));
        s.add(format!("fcmp {p}30, {p}7"), |a| a.fcmp(double, v(30), v(7)));
        s.add(format!("fcmpe {p}0, {p}1"), |a| a.fcmpe(double, v(0), v(1)));
        s.add(format!("fcmpe {p}30, {p}7"), |a| {
            a.fcmpe(double, v(30), v(7))
        });
        s.add(format!("fmov {p}16, {p}2"), |a| {
            a.fmov_reg(double, v(16), v(2))
        });
        s.add(format!("fmov {p}31, {p}0"), |a| {
            a.fmov_reg(double, v(31), v(0))
        });
        let g = if double { "x" } else { "w" };
        s.add(format!("fmov {g}5, {p}1"), |a| {
            a.fmov_to_gpr(double, r(5), v(1))
        });
        s.add(format!("fmov {g}30, {p}17"), |a| {
            a.fmov_to_gpr(double, r(30), v(17))
        });
        for cond in [Cond::Mi, Cond::Gt, Cond::Eq] {
            s.add(
                format!("fcsel {p}2, {p}0, {p}1, {}", cond_name(cond)),
                |a| a.fcsel(double, v(2), v(0), v(1), cond),
            );
        }
        for w in widths {
            let g = |n: u8| name(w, n, R31::Zero);
            s.add(format!("scvtf {p}2, {}", g(5)), |a| {
                a.scvtf(w, double, v(2), r(5))
            });
            s.add(format!("fcvtzs {}, {p}1", g(5)), |a| {
                a.fcvt_to_int(true, w, double, r(5), v(1))
            });
            s.add(format!("fcvtns {}, {p}1", g(30)), |a| {
                a.fcvt_to_int(false, w, double, r(30), v(1))
            });
        }
    }
    s.add("fcvt d2, s1".into(), |a| a.fcvt(true, v(2), v(1)));
    s.add("fcvt s2, d1".into(), |a| a.fcvt(false, v(2), v(1)));

    // Advanced SIMD.
    let v = VReg::new;
    let lane_names = [
        (Lanes::B, "16b"),
        (Lanes::H, "8h"),
        (Lanes::S, "4s"),
        (Lanes::D, "2d"),
    ];
    for &(vt, rn, offset) in &[
        (0u8, 0u8, 0u32),
        (1, 7, 160),
        (31, 30, 65520),
        (17, 31, 400),
    ] {
        let base = if rn == 31 {
            "sp".to_string()
        } else {
            format!("x{rn}")
        };
        s.add(format!("ldr q{vt}, [{base}, #{offset}]"), |a| {
            a.ldr_q(v(vt), r(rn), offset).unwrap()
        });
        s.add(format!("str q{vt}, [{base}, #{offset}]"), |a| {
            a.str_q(v(vt), r(rn), offset).unwrap()
        });
    }
    let ops = [
        (Vec3::Add, "add"),
        (Vec3::Sub, "sub"),
        (Vec3::Cmeq, "cmeq"),
        (Vec3::Cmgt, "cmgt"),
        (Vec3::Smax, "smax"),
        (Vec3::Smin, "smin"),
        (Vec3::Umax, "umax"),
        (Vec3::Umin, "umin"),
        (Vec3::Zip1, "zip1"),
        (Vec3::Zip2, "zip2"),
    ];
    for (op, m) in ops {
        for (lanes, t) in lane_names {
            let min_max = matches!(op, Vec3::Smax | Vec3::Smin | Vec3::Umax | Vec3::Umin);
            if min_max && lanes == Lanes::D {
                continue;
            }
            for &(d, n, mm) in &[(0u8, 1u8, 2u8), (31, 15, 7)] {
                s.add(format!("{m} v{d}.{t}, v{n}.{t}, v{mm}.{t}"), |a| {
                    a.vec3(op, lanes, v(d), v(n), v(mm)).unwrap()
                });
            }
        }
    }
    for (op, m) in [
        (Vec3::And, "and"),
        (Vec3::Bic, "bic"),
        (Vec3::Orr, "orr"),
        (Vec3::Eor, "eor"),
    ] {
        for &(d, n, mm) in &[(0u8, 1u8, 2u8), (31, 15, 7)] {
            s.add(format!("{m} v{d}.16b, v{n}.16b, v{mm}.16b"), |a| {
                a.vec3(op, Lanes::B, v(d), v(n), v(mm)).unwrap()
            });
        }
    }
    for (lanes, t) in lane_names {
        let bits = lanes.bits();
        for amount in [0, 1, bits / 2, bits - 1, bits] {
            for (op, m) in [
                (VShift::Shl, "shl"),
                (VShift::Ushr, "ushr"),
                (VShift::Sshr, "sshr"),
            ] {
                let valid = match op {
                    VShift::Shl => amount < bits,
                    _ => amount >= 1,
                };
                if valid {
                    s.add(format!("{m} v3.{t}, v29.{t}, #{amount}"), |a| {
                        a.vshift(op, lanes, v(3), v(29), amount).unwrap()
                    });
                }
            }
        }
    }
    for index in [0u32, 1, 8, 15] {
        s.add(format!("ext v0.16b, v1.16b, v31.16b, #{index}"), |a| {
            a.ext(v(0), v(1), v(31), index).unwrap()
        });
    }
    for (load, m) in [(true, "ldp"), (false, "stp")] {
        for (t1, t2, n, offset) in [
            (16u8, 17u8, 0u8, 160i32),
            (30, 31, 0, 400),
            (0, 1, 7, -1024),
            (2, 3, 30, 1008),
        ] {
            s.add(format!("{m} q{t1}, q{t2}, [x{n}, #{offset}]"), |a| {
                a.pair_q(load, v(t1), v(t2), r(n), offset).unwrap()
            });
        }
    }
    for (lanes, t, g, top) in [
        (Lanes::B, "b", "w", 15u32),
        (Lanes::H, "h", "w", 7),
        (Lanes::S, "s", "w", 3),
        (Lanes::D, "d", "x", 1),
    ] {
        for (d, i, n, j) in [(16u8, 0u32, 2u8, top), (31, top, 0, 0), (5, 1, 17, 1)] {
            s.add(format!("mov v{d}.{t}[{i}], v{n}.{t}[{j}]"), |a| {
                a.ins_element(lanes, v(d), i, v(n), j).unwrap()
            });
            s.add(format!("mov v{d}.{t}[{i}], {g}{n}"), |a| {
                a.ins_gpr(lanes, v(d), i, r(n)).unwrap()
            });
            s.add(format!("umov {g}{n}, v{d}.{t}[{i}]"), |a| {
                a.umov(lanes, r(n), v(d), i).unwrap()
            });
        }
    }
    for (double, p, g) in [(false, "s", "w"), (true, "d", "x")] {
        s.add(format!("fmov {p}16, {g}5"), |a| {
            a.fmov_from_gpr(double, v(16), r(5))
        });
        s.add(format!("fmov {p}0, {g}30"), |a| {
            a.fmov_from_gpr(double, v(0), r(30))
        });
    }
    for (d, n) in [(16u8, 2u8), (0, 31)] {
        s.add(format!("mov v{d}.16b, v{n}.16b"), |a| {
            a.mov_vector(v(d), v(n))
        });
    }
    for (double, t) in [(false, "4s"), (true, "2d")] {
        for (op, m) in [
            (VecFp::Add, "fadd"),
            (VecFp::Sub, "fsub"),
            (VecFp::Mul, "fmul"),
            (VecFp::Div, "fdiv"),
            (VecFp::Cmeq, "fcmeq"),
            (VecFp::Cmge, "fcmge"),
            (VecFp::Cmgt, "fcmgt"),
        ] {
            for (d, n, mm) in [(2u8, 16u8, 1u8), (31, 0, 30)] {
                s.add(format!("{m} v{d}.{t}, v{n}.{t}, v{mm}.{t}"), |a| {
                    a.vec_fp(op, double, v(d), v(n), v(mm))
                });
            }
        }
        for (op, m) in [
            (VecUnary::Fsqrt { double }, "fsqrt"),
            (VecUnary::Scvtf { double }, "scvtf"),
            (VecUnary::Fcvtzs { double }, "fcvtzs"),
            (VecUnary::Fcvtns { double }, "fcvtns"),
        ] {
            s.add(format!("{m} v2.{t}, v17.{t}"), |a| {
                a.vec_unary(op, v(2), v(17))
            });
        }
        let x = if double { "2d" } else { "4s" };
        s.add(format!("cmhi v3.{x}, v4.{x}, v20.{x}"), |a| {
            a.cmhi(double, v(3), v(4), v(20))
        });
        let g = if double { "x" } else { "w" };
        s.add(format!("dup v5.{x}, {g}10"), |a| a.dup(double, v(5), r(10)));
    }
    for (op, text) in [
        (VecUnary::Fcvtl, "fcvtl v2.2d, v17.2s"),
        (VecUnary::Fcvtn, "fcvtn v2.2s, v17.2d"),
        (VecUnary::Sxtl, "sshll v2.2d, v17.2s, #0"),
        (VecUnary::Xtn, "xtn v2.2s, v17.2d"),
        (VecUnary::Not, "mvn v2.16b, v17.16b"),
        (VecUnary::Uminv, "uminv s2, v17.4s"),
        (VecUnary::Umaxv, "umaxv s2, v17.4s"),
    ] {
        s.add(text.into(), |a| a.vec_unary(op, v(2), v(17)));
    }
    s.add("bsl v3.16b, v16.16b, v1.16b".into(), |a| {
        a.bsl(v(3), v(16), v(1))
    });
    for (load, m) in [(false, "str"), (true, "ldr")] {
        for (size, g) in [(MemSize::B64, "x"), (MemSize::B32, "w")] {
            for (t, n, offset) in [(17u8, 26u8, -8i32), (4, 26, 8), (30, 1, 255), (0, 30, -256)] {
                s.add(format!("{m} {g}{t}, [x{n}, #{offset}]!"), |a| {
                    a.load_store_indexed(load, size, r(t), r(n), offset, true)
                        .unwrap()
                });
                s.add(format!("{m} {g}{t}, [x{n}], #{offset}"), |a| {
                    a.load_store_indexed(load, size, r(t), r(n), offset, false)
                        .unwrap()
                });
            }
        }
    }
    s.add("movi v0.2d, #0".into(), |a| a.movi_zero(v(0)));
    s.add("movi v31.2d, #0".into(), |a| a.movi_zero(v(31)));
    for (size, suffix, reg) in [
        (MemSize::B8, "b", "w"),
        (MemSize::B16, "h", "w"),
        (MemSize::B32, "", "w"),
        (MemSize::B64, "", "x"),
    ] {
        for (t, n) in [(0u8, 1u8), (30, 7)] {
            s.add(format!("ldar{suffix} {reg}{t}, [x{n}]"), |a| {
                a.ldar(size, r(t), r(n))
            });
            s.add(format!("ldaxr{suffix} {reg}{t}, [x{n}]"), |a| {
                a.ldaxr(size, r(t), r(n))
            });
            s.add(format!("stlxr{suffix} w15, {reg}{t}, [x{n}]"), |a| {
                a.stlxr(size, r(15), r(t), r(n))
            });
        }
    }
    s.add("clrex".into(), |a| a.clrex());
    for (d, imm) in [(0u8, 0u8), (2, 0x8f), (31, 0xff), (17, 0x20)] {
        s.add(format!("movi v{d}.16b, #{imm:#x}"), |a| {
            a.movi_bytes(v(d), imm)
        });
    }
    for (d, n, mm) in [(0u8, 0u8, 1u8), (31, 7, 30), (5, 16, 2)] {
        s.add(format!("tbl v{d}.16b, {{v{n}.16b}}, v{mm}.16b"), |a| {
            a.tbl(v(d), v(n), v(mm))
        });
    }
    for (from, low, high, wide) in [
        (Lanes::H, "8b", "16b", "8h"),
        (Lanes::S, "4h", "8h", "4s"),
        (Lanes::D, "2s", "4s", "2d"),
    ] {
        for (unsigned, m) in [(false, "sqxtn"), (true, "sqxtun")] {
            s.add(format!("{m} v2.{low}, v9.{wide}"), |a| {
                a.narrow(unsigned, false, from, v(2), v(9)).unwrap()
            });
            s.add(format!("{m}2 v2.{high}, v9.{wide}"), |a| {
                a.narrow(unsigned, true, from, v(2), v(9)).unwrap()
            });
        }
    }

    for &rn in &[0u8, 16, 30] {
        s.add(format!("br x{rn}"), |a| a.br(r(rn)));
        s.add(format!("blr x{rn}"), |a| a.blr(r(rn)));
    }
    s.add("ret".into(), Assembler::ret);
    for &rt in &[0u8, 17, 31] {
        let t = name(Width::X, rt, R31::Zero);
        s.add(format!("mrs {t}, nzcv"), |a| a.read_flags(r(rt)));
        s.add(format!("msr nzcv, {t}"), |a| a.write_flags(r(rt)));
        s.add(format!("mrs {t}, fpsr"), |a| a.read_fpsr(r(rt)));
        s.add(format!("msr fpsr, {t}"), |a| a.write_fpsr(r(rt)));
    }
    s.add("nop".into(), Assembler::nop);
    s.add("brk #0x1234".into(), |a| a.brk(0x1234));
    s.add("mov x3, x4".into(), |a| a.mov(Width::X, r(3), r(4)));
    s.add("mov w3, wzr".into(), |a| a.mov(Width::W, r(3), ZR));
    s.add("mov sp, x5".into(), |a| a.mov_sp(SP, r(5)));
    s.add("mov x6, sp".into(), |a| a.mov_sp(r(6), SP));
    s.0
}
