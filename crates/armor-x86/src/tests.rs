//! Expected encodings come from `x86_64-linux-gnu-objdump -d -M intel`
//! (binutils 2.44) on the listed assembly, the decoder oracle of ADR-0004.

use super::*;

fn ok(bytes: &[u8]) -> Instruction {
    decode(bytes).unwrap_or_else(|e| panic!("{bytes:02x?}: {e}"))
}

fn gpr(n: u8) -> Operand {
    Operand::Reg(Reg::Gpr(n))
}

fn mem(base: Base, index: Option<u8>, scale: u8, disp: i64) -> Operand {
    Operand::Mem(Mem {
        segment: Segment::Default,
        base,
        index,
        scale,
        disp,
        addr32: false,
    })
}

const RAX: u8 = 0;
const RCX: u8 = 1;
const RDX: u8 = 2;
const RBX: u8 = 3;
const RSP: u8 = 4;
const RBP: u8 = 5;
const RSI: u8 = 6;
const RDI: u8 = 7;
const R8: u8 = 8;
const R9: u8 = 9;
const R10: u8 = 10;
const R12: u8 = 12;
const R13: u8 = 13;
const R15: u8 = 15;

#[test]
fn lengths_match_objdump() {
    let cases: &[(&str, &[u8])] = &[
        ("mov eax,0x1", &[0xb8, 0x01, 0, 0, 0]),
        (
            "movabs rax,0x1122334455667788",
            &[0x48, 0xb8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
        ),
        ("mov r9d,0xffffffff", &[0x41, 0xb9, 0xff, 0xff, 0xff, 0xff]),
        ("mov ax,0x1234", &[0x66, 0xb8, 0x34, 0x12]),
        ("mov al,0x12", &[0xb0, 0x12]),
        ("mov ah,0x34", &[0xb4, 0x34]),
        ("mov r12b,0x5", &[0x41, 0xb4, 0x05]),
        ("mov rbx,rcx", &[0x48, 0x89, 0xcb]),
        ("mov ecx,r8d", &[0x44, 0x89, 0xc1]),
        ("mov [rip+0x100],rax", &[0x48, 0x89, 0x05, 0x00, 0x01, 0, 0]),
        ("mov rdx,[rbx+rcx*8+0x10]", &[0x48, 0x8b, 0x54, 0xcb, 0x10]),
        (
            "mov DWORD PTR [rsp+0x4],0x7",
            &[0xc7, 0x44, 0x24, 0x04, 0x07, 0, 0, 0],
        ),
        (
            "mov QWORD PTR [r12],-2",
            &[0x49, 0xc7, 0x04, 0x24, 0xfe, 0xff, 0xff, 0xff],
        ),
        ("mov BYTE PTR [rbp-0x1],0x80", &[0xc6, 0x45, 0xff, 0x80]),
        ("mov sil,dil", &[0x40, 0x88, 0xfe]),
        ("mov rax,[r13+0x0]", &[0x49, 0x8b, 0x45, 0x00]),
        ("lea rsi,[rip+0xfef]", &[0x48, 0x8d, 0x35, 0xef, 0x0f, 0, 0]),
        ("lea rax,[rbx+rcx*4]", &[0x48, 0x8d, 0x04, 0x8b]),
        ("lea eax,[rax+0x1]", &[0x8d, 0x40, 0x01]),
        ("xor edi,edi", &[0x31, 0xff]),
        ("add rax,rbx", &[0x48, 0x01, 0xd8]),
        ("add eax,0x1", &[0x83, 0xc0, 0x01]),
        ("add rax,-1", &[0x48, 0x83, 0xc0, 0xff]),
        ("add al,0x7f", &[0x04, 0x7f]),
        ("add rax,0x12345678", &[0x48, 0x05, 0x78, 0x56, 0x34, 0x12]),
        ("sub r10,[rsp+0x8]", &[0x4c, 0x2b, 0x54, 0x24, 0x08]),
        ("cmp eax,ebx", &[0x39, 0xd8]),
        ("cmp BYTE PTR [rdi],0x0", &[0x80, 0x3f, 0x00]),
        ("and ecx,0xff", &[0x81, 0xe1, 0xff, 0, 0, 0]),
        ("or rax,0x1", &[0x48, 0x83, 0xc8, 0x01]),
        ("adc rdx,0x0", &[0x48, 0x83, 0xd2, 0x00]),
        ("sbb ax,bx", &[0x66, 0x19, 0xd8]),
        ("test eax,eax", &[0x85, 0xc0]),
        ("test al,0x1", &[0xa8, 0x01]),
        ("test rdi,rsi", &[0x48, 0x85, 0xf7]),
        ("test DWORD PTR [rbx],0x10", &[0xf7, 0x03, 0x10, 0, 0, 0]),
        ("push rbx", &[0x53]),
        ("push r15", &[0x41, 0x57]),
        ("push 0x1", &[0x6a, 0x01]),
        ("push 0x12345", &[0x68, 0x45, 0x23, 0x01, 0x00]),
        ("pop rax", &[0x58]),
        ("pop r8", &[0x41, 0x58]),
        ("pushf", &[0x9c]),
        ("syscall", &[0x0f, 0x05]),
        ("ret", &[0xc3]),
        (
            "mov rax,QWORD PTR fs:0x28",
            &[0x64, 0x48, 0x8b, 0x04, 0x25, 0x28, 0, 0, 0],
        ),
        ("mov ebx,DWORD PTR gs:[rax]", &[0x65, 0x8b, 0x18]),
        ("jmp rel8", &[0xeb, 0xfe]),
        ("jmp rel32", &[0xe9, 0xc8, 0, 0, 0]),
        ("je rel32", &[0x0f, 0x84, 0x2b, 0xff, 0xff, 0xff]),
        ("jne rel8", &[0x75, 0x05]),
        ("call rel32", &[0xe8, 0, 0, 0, 0]),
        ("jl rel8", &[0x7c, 0xfe]),
    ];
    for (text, bytes) in cases {
        // Trailing bytes must not be consumed.
        let mut padded = bytes.to_vec();
        padded.extend_from_slice(&[0x90, 0x90]);
        assert_eq!(ok(&padded).len, bytes.len(), "{text}");
    }
}

#[test]
fn immediates_follow_operand_size_rules() {
    assert_eq!(
        ok(&[0x48, 0xb8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]).op,
        Op::Mov {
            dst: gpr(RAX),
            src: Operand::Imm(0x1122334455667788)
        }
    );
    let mov_r9d = ok(&[0x41, 0xb9, 0xff, 0xff, 0xff, 0xff]);
    assert_eq!(mov_r9d.size, Size::B32);
    assert_eq!(
        mov_r9d.op,
        Op::Mov {
            dst: gpr(R9),
            src: Operand::Imm(-1)
        }
    );
    let mov_ax = ok(&[0x66, 0xb8, 0x34, 0x12]);
    assert_eq!(mov_ax.size, Size::B16);
    assert_eq!(
        mov_ax.op,
        Op::Mov {
            dst: gpr(RAX),
            src: Operand::Imm(0x1234)
        }
    );
    assert_eq!(
        ok(&[0x48, 0x83, 0xc0, 0xff]).op,
        Op::Alu {
            op: AluOp::Add,
            dst: gpr(RAX),
            src: Operand::Imm(-1)
        }
    );
    assert_eq!(
        ok(&[0x68, 0x45, 0x23, 0x01, 0x00]).op,
        Op::Push(Operand::Imm(0x12345))
    );
}

#[test]
fn byte_registers_depend_on_rex() {
    assert_eq!(
        ok(&[0xb4, 0x34]).op,
        Op::Mov {
            dst: Operand::Reg(Reg::HighByte(0)),
            src: Operand::Imm(0x34)
        }
    );
    assert_eq!(
        ok(&[0x41, 0xb4, 0x05]).op,
        Op::Mov {
            dst: gpr(R12),
            src: Operand::Imm(5)
        }
    );
    // mov sil, dil: REX 40 turns 6/7 into SIL/DIL instead of DH/BH.
    assert_eq!(
        ok(&[0x40, 0x88, 0xfe]).op,
        Op::Mov {
            dst: gpr(RSI),
            src: gpr(RDI)
        }
    );
}

#[test]
fn modrm_and_sib_special_cases() {
    // RIP-relative.
    assert_eq!(
        ok(&[0x48, 0x8d, 0x35, 0xef, 0x0f, 0, 0]).op,
        Op::Lea {
            dst: Reg::Gpr(RSI),
            src: Mem {
                segment: Segment::Default,
                base: Base::Rip,
                index: None,
                scale: 1,
                disp: 0xfef,
                addr32: false,
            }
        }
    );
    // [rbx + rcx*8 + 0x10].
    assert_eq!(
        ok(&[0x48, 0x8b, 0x54, 0xcb, 0x10]).op,
        Op::Mov {
            dst: gpr(RDX),
            src: mem(Base::Gpr(RBX), Some(RCX), 8, 0x10)
        }
    );
    // [rsp + 4] needs a SIB with "no index".
    assert_eq!(
        ok(&[0xc7, 0x44, 0x24, 0x04, 0x07, 0, 0, 0]).op,
        Op::Mov {
            dst: mem(Base::Gpr(RSP), None, 1, 4),
            src: Operand::Imm(7)
        }
    );
    // [r12] also needs a SIB.
    assert_eq!(
        ok(&[0x49, 0xc7, 0x04, 0x24, 0xfe, 0xff, 0xff, 0xff]).op,
        Op::Mov {
            dst: mem(Base::Gpr(R12), None, 1, 0),
            src: Operand::Imm(-2)
        }
    );
    // [r13] is encoded as [r13 + disp8 0], not as RIP-relative.
    assert_eq!(
        ok(&[0x49, 0x8b, 0x45, 0x00]).op,
        Op::Mov {
            dst: gpr(RAX),
            src: mem(Base::Gpr(R13), None, 1, 0)
        }
    );
    // [rbp - 1].
    assert_eq!(
        ok(&[0xc6, 0x45, 0xff, 0x80]).op,
        Op::Mov {
            dst: mem(Base::Gpr(RBP), None, 1, -1),
            src: Operand::Imm(-128)
        }
    );
    // REX.R selects r8-r15 in the reg field.
    assert_eq!(
        ok(&[0x4c, 0x2b, 0x54, 0x24, 0x08]).op,
        Op::Alu {
            op: AluOp::Sub,
            dst: gpr(R10),
            src: mem(Base::Gpr(RSP), None, 1, 8)
        }
    );
    // REX.R on the source of a store.
    assert_eq!(
        ok(&[0x44, 0x89, 0xc1]).op,
        Op::Mov {
            dst: gpr(RCX),
            src: gpr(R8)
        }
    );
}

#[test]
fn segment_overrides_and_absolute_addresses() {
    assert_eq!(
        ok(&[0x64, 0x48, 0x8b, 0x04, 0x25, 0x28, 0, 0, 0]).op,
        Op::Mov {
            dst: gpr(RAX),
            src: Operand::Mem(Mem {
                segment: Segment::Fs,
                base: Base::None,
                index: None,
                scale: 1,
                disp: 0x28,
                addr32: false,
            })
        }
    );
    assert_eq!(
        ok(&[0x65, 0x8b, 0x18]).op,
        Op::Mov {
            dst: gpr(RBX),
            src: Operand::Mem(Mem {
                segment: Segment::Gs,
                base: Base::Gpr(RAX),
                index: None,
                scale: 1,
                disp: 0,
                addr32: false,
            })
        }
    );
}

#[test]
fn alu_forms() {
    assert_eq!(
        ok(&[0x31, 0xff]).op,
        Op::Alu {
            op: AluOp::Xor,
            dst: gpr(RDI),
            src: gpr(RDI)
        }
    );
    assert_eq!(
        ok(&[0x80, 0x3f, 0x00]),
        Instruction {
            op: Op::Alu {
                op: AluOp::Cmp,
                dst: mem(Base::Gpr(RDI), None, 1, 0),
                src: Operand::Imm(0)
            },
            size: Size::B8,
            len: 3,
            lock: false,
            addr32: false
        }
    );
    let sbb = ok(&[0x66, 0x19, 0xd8]);
    assert_eq!(sbb.size, Size::B16);
    assert_eq!(
        sbb.op,
        Op::Alu {
            op: AluOp::Sbb,
            dst: gpr(RAX),
            src: gpr(RBX)
        }
    );
    assert_eq!(
        ok(&[0x04, 0x7f]),
        Instruction {
            op: Op::Alu {
                op: AluOp::Add,
                dst: gpr(RAX),
                src: Operand::Imm(0x7f)
            },
            size: Size::B8,
            len: 2,
            lock: false,
            addr32: false
        }
    );
}

#[test]
fn branches_are_relative_to_the_next_instruction() {
    assert_eq!(ok(&[0xeb, 0xfe]).op, Op::Jmp { rel: -2 });
    assert_eq!(
        ok(&[0x0f, 0x84, 0x2b, 0xff, 0xff, 0xff]).op,
        Op::Jcc {
            cond: Cond::E,
            rel: -0xd5
        }
    );
    assert_eq!(
        ok(&[0x7c, 0xfe]).op,
        Op::Jcc {
            cond: Cond::L,
            rel: -2
        }
    );
    assert_eq!(ok(&[0xe8, 0, 0, 0, 0]).op, Op::Call { rel: 0 });
    assert_eq!(ok(&[0x41, 0x57]).op, Op::Push(gpr(R15)));
    assert_eq!(ok(&[0x41, 0x58]).op, Op::Pop(gpr(R8)));
}

#[test]
fn rex_before_a_legacy_prefix_is_ignored() {
    // REX.W then 66: the REX does not immediately precede the opcode.
    let insn = ok(&[0x48, 0x66, 0xb8, 0x34, 0x12]);
    assert_eq!(insn.size, Size::B16);
    assert_eq!(insn.len, 5);
}

#[test]
fn errors() {
    assert_eq!(decode(&[]), Err(DecodeError::Truncated));
    assert_eq!(decode(&[0x48, 0xb8, 0x01]), Err(DecodeError::Truncated));
    assert_eq!(decode(&[0x27]), Err(DecodeError::Invalid));
    // INTO, AAM, AAD are "Invalid" in 64-bit mode per the SDM, although
    // qemu-x86_64 executes them (docs/testing.md); SALC is undocumented.
    for opcode in [0xce, 0xd4, 0xd5, 0xd6] {
        assert_eq!(
            decode(&[opcode, 0x0a]),
            Err(DecodeError::Invalid),
            "{opcode:02x}"
        );
    }
    assert_eq!(decode(&[0x8d, 0xc0]), Err(DecodeError::Invalid));
    // LOCK on a register destination raises #UD.
    assert_eq!(decode(&[0xf0, 0x01, 0xd8]), Err(DecodeError::Invalid));
    // ud2.
    assert_eq!(decode(&[0x0f, 0x0b]), Err(DecodeError::Invalid));
    // fbld and fbstp (x87 packed BCD).
    assert!(matches!(
        decode(&[0xdf, 0x20]).unwrap().op,
        Op::X87 {
            op: X87Op::Load(X87Operand::Mem(_, X87Format::Bcd)),
            ..
        }
    ));
    assert!(matches!(
        decode(&[0xdf, 0x30]).unwrap().op,
        Op::X87 {
            op: X87Op::Store {
                dst: X87Operand::Mem(_, X87Format::Bcd),
                pop: true
            },
            ..
        }
    ));
    // The transcendentals (ADR-0030).
    assert_eq!(
        decode(&[0xd9, 0xfe]).unwrap().op,
        Op::X87 {
            op: X87Op::Transcendental(X87Transcendental::Sin),
            opcode: 0x1fe,
        }
    );
    // Register forms of 0F AE other than the fences (rdfsbase, ...).
    assert!(matches!(
        decode(&[0x0f, 0xae, 0xc0]),
        Err(DecodeError::Unsupported(_))
    ));
    // XSAVE: xgetbv, xsetbv, xsave [rsp+0x40] and xrstor64 [rdi] (objdump).
    assert_eq!(decode(&[0x0f, 0x01, 0xd0]).unwrap().op, Op::Xgetbv);
    assert_eq!(decode(&[0x0f, 0x01, 0xd1]).unwrap().op, Op::Xsetbv);
    let xsave = decode(&[0x0f, 0xae, 0x64, 0x24, 0x40]).unwrap();
    assert!(matches!(
        xsave.op,
        Op::State {
            op: StateOp::Xsave,
            mem: Mem { disp: 0x40, .. }
        }
    ));
    assert_eq!(xsave.len, 5);
    assert!(matches!(
        decode(&[0x48, 0x0f, 0xae, 0x2f]).unwrap().op,
        Op::State {
            op: StateOp::Xrstor,
            ..
        }
    ));
    // xsaveopt, and clwb (66 0F AE /6), the rest of 0F 01: not executed.
    for bytes in [
        &[0x0f, 0xae, 0x37][..],
        &[0x66, 0x0f, 0xae, 0x37],
        &[0x0f, 0x01, 0xf9],
    ] {
        assert!(
            matches!(decode(bytes), Err(DecodeError::Unsupported(_))),
            "{bytes:02x?}"
        );
    }
    // Operand-size prefix on branches and stack operations: not supported.
    for bytes in [
        &[0x66, 0x74, 0x00][..],
        &[0x66, 0xeb, 0x00],
        &[0x66, 0xe9, 0, 0],
        &[0x66, 0x9c],
    ] {
        assert!(
            matches!(decode(bytes), Err(DecodeError::Unsupported(_))),
            "{bytes:02x?}"
        );
    }
    // push and pop with the prefix move 2 bytes (Spotify's obfuscated code).
    let push = ok(&[0x66, 0x50]);
    assert_eq!((push.op, push.size), (Op::Push(gpr(RAX)), Size::B16));
    let push = ok(&[0x66, 0x41, 0xff, 0x37]);
    assert_eq!(push.size, Size::B16);
    let push = ok(&[0x66, 0x68, 0x34, 0x12]);
    assert_eq!(
        (push.op, push.size, push.len),
        (Op::Push(Operand::Imm(0x1234)), Size::B16, 4)
    );
    // With REX.W the prefix is overridden: a 64-bit push.
    assert_eq!(ok(&[0x66, 0x48, 0x50]).size, Size::B64);
    // 14 operand-size prefixes + opcode + immediate exceed 15 bytes.
    let mut long = vec![0x66; 14];
    long.extend_from_slice(&[0xb8, 0x34, 0x12]);
    assert_eq!(decode(&long), Err(DecodeError::TooLong));
}

// M2 forms. Encodings from objdump on the listed assembly.

fn plain_mem(base: Base, index: Option<u8>, scale: u8, disp: i64) -> Mem {
    Mem {
        segment: Segment::Default,
        base,
        index,
        scale,
        disp,
        addr32: false,
    }
}

#[test]
fn address_size_prefix_marks_32_bit_addressing() {
    // mov rdx, QWORD PTR [eax]
    let insn = ok(&[0x67, 0x48, 0x8b, 0x10]);
    assert_eq!(
        insn.op,
        Op::Mov {
            dst: gpr(RDX),
            src: Operand::Mem(Mem {
                addr32: true,
                ..plain_mem(Base::Gpr(RAX), None, 1, 0)
            })
        }
    );
}

#[test]
fn tzcnt_encoding_runs_as_bsf() {
    // tzcnt rdx, rcx (F3 0F BC): bsf without BMI1 (`counts_zeros`).
    assert_eq!(
        ok(&[0xf3, 0x48, 0x0f, 0xbc, 0xd1]).op,
        Op::BitScan {
            reverse: false,
            dst: Reg::Gpr(RDX),
            src: gpr(RCX),
            counts_zeros: true,
        }
    );
    // bsr rax, rcx: no F3.
    assert!(matches!(
        ok(&[0x48, 0x0f, 0xbd, 0xc1]).op,
        Op::BitScan {
            reverse: true,
            counts_zeros: false,
            ..
        }
    ));
}

/// The VEX forms of SSE instructions (AVX), bytes from the GNU assembler
/// and objdump.
#[test]
fn vex_sse_forms() {
    let xmm = |n| Operand::Reg(Reg::Xmm(n));
    let vex = |bytes: &[u8], op, dst, src1, src, wide| {
        let insn = ok(bytes);
        assert_eq!(
            (insn.op, insn.len),
            (
                Op::Vex {
                    op,
                    dst,
                    src1,
                    src,
                    wide,
                    avx2: false,
                },
                bytes.len()
            ),
            "{bytes:02x?}"
        );
    };
    let add = SseOp::PackedArith {
        op: FloatOp::Add,
        double: false,
    };
    // vaddps xmm1, xmm2, xmm3; vaddps ymm1, ymm2, ymm3
    vex(
        &[0xc5, 0xe8, 0x58, 0xcb],
        add,
        xmm(1),
        Some(2),
        xmm(3),
        false,
    );
    vex(
        &[0xc5, 0xec, 0x58, 0xcb],
        add,
        xmm(1),
        Some(2),
        xmm(3),
        true,
    );
    // vaddss xmm9, xmm10, [rax]
    let adds = SseOp::FloatArith {
        op: FloatOp::Add,
        double: false,
    };
    vex(
        &[0xc5, 0x2a, 0x58, 0x08],
        adds,
        xmm(9),
        Some(10),
        mem(Base::Gpr(RAX), None, 1, 0),
        false,
    );
    // vpsllw xmm5, xmm6, 3: VEX.vvvv is the destination.
    let shift = SseOp::ShiftImm {
        kind: ShiftKind::Left,
        lane: Size::B16,
        count: 3,
    };
    vex(
        &[0xc5, 0xd1, 0x71, 0xf6, 0x03],
        shift,
        xmm(5),
        Some(6),
        Operand::Imm(3),
        false,
    );
    // vmovdqu xmm0, [rdi]; vpshufd xmm1, xmm2, 0x1b: no VEX.vvvv.
    let movdqu = SseOp::Mov128 { aligned: false };
    vex(
        &[0xc5, 0xfa, 0x6f, 0x07],
        movdqu,
        xmm(0),
        None,
        mem(Base::Gpr(RDI), None, 1, 0),
        false,
    );
    vex(
        &[0xc5, 0xf9, 0x70, 0xca, 0x1b],
        SseOp::Pshufd(0x1b),
        xmm(1),
        None,
        xmm(2),
        false,
    );
    // vcmpps xmm1, xmm2, xmm3, 17 (lt_oq): VEX predicates up to 31.
    let cmp = SseOp::PackedCmpMask {
        double: false,
        predicate: 17,
    };
    vex(
        &[0xc5, 0xe8, 0xc2, 0xcb, 0x11],
        cmp,
        xmm(1),
        Some(2),
        xmm(3),
        false,
    );
    // vmovss xmm1, xmm2, xmm3
    let movss = SseOp::MovScalar { double: false };
    vex(
        &[0xc5, 0xea, 0x10, 0xcb],
        movss,
        xmm(1),
        Some(2),
        xmm(3),
        false,
    );
    // vpinsrq xmm1, xmm2, rax, 1
    let insert = SseOp::Insert {
        lane: Size::B64,
        index: 1,
    };
    vex(
        &[0xc4, 0xe3, 0xe9, 0x22, 0xc8, 0x01],
        insert,
        xmm(1),
        Some(2),
        gpr(RAX),
        false,
    );
    // vstmxcsr [rsp] (glibc's libm)
    assert!(matches!(
        ok(&[0xc5, 0xf8, 0xae, 0x1c, 0x24]).op,
        Op::VexMxcsr { store: true, .. }
    ));
    // vpinsrq with VEX.L = 1: no 256-bit form.
    assert_eq!(
        decode(&[0xc4, 0xe3, 0xed, 0x22, 0xc8, 0x01]),
        Err(DecodeError::Invalid)
    );
    // vzeroupper, vzeroall
    assert_eq!(ok(&[0xc5, 0xf8, 0x77]).op, Op::Vzero { all: false });
    assert_eq!(ok(&[0xc5, 0xfc, 0x77]).op, Op::Vzero { all: true });
    // vmovdqu with VEX.vvvv in use, an MMX form (VEX.pp none on 0F FE) and
    // the legacy pblendvb opcode: #UD.
    for bytes in [
        &[0xc5, 0xea, 0x6f, 0x07][..],
        &[0xc5, 0xe8, 0xfe, 0xcb],
        &[0xc4, 0xe2, 0x69, 0x10, 0xcb],
    ] {
        assert_eq!(decode(bytes), Err(DecodeError::Invalid), "{bytes:02x?}");
    }
    // Legacy cmpps still refuses predicates above 7.
    assert!(matches!(
        decode(&[0x0f, 0xc2, 0xcb, 0x11]),
        Err(DecodeError::Unsupported(_))
    ));
}

/// BMI1, BMI2 (VEX) and MOVBE, bytes from the GNU assembler and objdump.
#[test]
fn bmi_and_movbe() {
    let bmi = |bytes: &[u8], op, dst, src, vvvv, size| {
        let insn = ok(bytes);
        assert_eq!(
            (insn.op, insn.size, insn.len),
            (
                Op::Bmi {
                    op,
                    dst: Reg::Gpr(dst),
                    src,
                    vvvv: Reg::Gpr(vvvv),
                },
                size,
                bytes.len()
            ),
            "{bytes:02x?}"
        );
    };
    let at = |base, disp| mem(Base::Gpr(base), None, 1, disp);
    // andn rax, rbx, rcx; andn r9d, r10d, [r11+8]
    bmi(
        &[0xc4, 0xe2, 0xe0, 0xf2, 0xc1],
        BmiOp::Andn,
        RAX,
        gpr(RCX),
        RBX,
        Size::B64,
    );
    bmi(
        &[0xc4, 0x42, 0x28, 0xf2, 0x4b, 0x08],
        BmiOp::Andn,
        R9,
        at(11, 8),
        R10,
        Size::B32,
    );
    // bextr eax, [rsi], edx
    bmi(
        &[0xc4, 0xe2, 0x68, 0xf7, 0x06],
        BmiOp::Bextr,
        RAX,
        at(RSI, 0),
        RDX,
        Size::B32,
    );
    // blsi r12, rax; blsmsk eax, ecx; blsr rbx, [rdi]: the destination
    // is VEX.vvvv.
    bmi(
        &[0xc4, 0xe2, 0x98, 0xf3, 0xd8],
        BmiOp::Blsi,
        R12,
        gpr(RAX),
        R12,
        Size::B64,
    );
    bmi(
        &[0xc4, 0xe2, 0x78, 0xf3, 0xd1],
        BmiOp::Blsmsk,
        RAX,
        gpr(RCX),
        RAX,
        Size::B32,
    );
    bmi(
        &[0xc4, 0xe2, 0xe0, 0xf3, 0x0f],
        BmiOp::Blsr,
        RBX,
        at(RDI, 0),
        RBX,
        Size::B64,
    );
    // bzhi rax, rbx, rcx; pdep eax, ebx, ecx; pext r8, r9, [rsp]
    bmi(
        &[0xc4, 0xe2, 0xf0, 0xf5, 0xc3],
        BmiOp::Bzhi,
        RAX,
        gpr(RBX),
        RCX,
        Size::B64,
    );
    bmi(
        &[0xc4, 0xe2, 0x63, 0xf5, 0xc1],
        BmiOp::Pdep,
        RAX,
        gpr(RCX),
        RBX,
        Size::B32,
    );
    bmi(
        &[0xc4, 0x62, 0xb2, 0xf5, 0x04, 0x24],
        BmiOp::Pext,
        R8,
        at(RSP, 0),
        R9,
        Size::B64,
    );
    // mulx r10, r11, rax: high half r10, low half r11.
    bmi(
        &[0xc4, 0x62, 0xa3, 0xf6, 0xd0],
        BmiOp::Mulx,
        R10,
        gpr(RAX),
        11,
        Size::B64,
    );
    // rorx eax, ebx, 7
    bmi(
        &[0xc4, 0xe3, 0x7b, 0xf0, 0xc3, 0x07],
        BmiOp::Rorx(7),
        RAX,
        gpr(RBX),
        0,
        Size::B32,
    );
    // sarx eax, ebx, ecx; shlx r15, [rax], r14; shrx rax, rbx, rcx
    bmi(
        &[0xc4, 0xe2, 0x72, 0xf7, 0xc3],
        BmiOp::Sarx,
        RAX,
        gpr(RBX),
        RCX,
        Size::B32,
    );
    bmi(
        &[0xc4, 0x62, 0x89, 0xf7, 0x38],
        BmiOp::Shlx,
        R15,
        at(RAX, 0),
        14,
        Size::B64,
    );
    bmi(
        &[0xc4, 0xe2, 0xf3, 0xf7, 0xc3],
        BmiOp::Shrx,
        RAX,
        gpr(RBX),
        RCX,
        Size::B64,
    );
    // VEX.L = 1, rorx with VEX.vvvv in use, group 17 /0, and a REX or 66
    // before VEX: #UD.
    for bytes in [
        &[0xc4, 0xe2, 0xe4, 0xf2, 0xc1][..],
        &[0xc4, 0xe3, 0x73, 0xf0, 0xc3, 0x07],
        &[0xc4, 0xe2, 0x78, 0xf3, 0xc1],
        &[0x48, 0xc4, 0xe2, 0xe0, 0xf2, 0xc1],
        &[0x66, 0xc4, 0xe2, 0xe0, 0xf2, 0xc1],
    ] {
        assert_eq!(decode(bytes), Err(DecodeError::Invalid), "{bytes:02x?}");
    }
    // An AVX-only instruction is not decoded yet: vbroadcastss xmm0, [rdi].
    assert!(matches!(
        decode(&[0xc4, 0xe2, 0x79, 0x18, 0x07]),
        Err(DecodeError::Unsupported(_))
    ));
    // In compatibility mode: andn eax, ebx, ecx (VEX.W ignored).
    assert!(matches!(
        decode_in(&[0xc4, 0xe2, 0xe0, 0xf2, 0xc1], Mode::Compat32).unwrap(),
        Instruction {
            op: Op::Bmi {
                op: BmiOp::Andn,
                ..
            },
            size: Size::B32,
            ..
        }
    ));

    // movbe eax, [rcx]; movbe [rdx], r8; movbe ax, [rcx]
    let movbe = |bytes: &[u8], dst, src, size| {
        let insn = ok(bytes);
        assert_eq!((insn.op, insn.size), (Op::Movbe { dst, src }, size));
    };
    movbe(&[0x0f, 0x38, 0xf0, 0x01], gpr(RAX), at(RCX, 0), Size::B32);
    movbe(
        &[0x4c, 0x0f, 0x38, 0xf1, 0x02],
        at(RDX, 0),
        gpr(R8),
        Size::B64,
    );
    movbe(
        &[0x66, 0x0f, 0x38, 0xf0, 0x01],
        gpr(RAX),
        at(RCX, 0),
        Size::B16,
    );
    // A register operand is #UD.
    assert_eq!(decode(&[0x0f, 0x38, 0xf0, 0xc1]), Err(DecodeError::Invalid));
}

#[test]
fn bit_tests() {
    assert_eq!(
        ok(&[0x48, 0x0f, 0xab, 0x0b]).op,
        Op::BitTest {
            op: BitTestOp::Bts,
            dst: Operand::Mem(plain_mem(Base::Gpr(RBX), None, 1, 0)),
            offset: gpr(RCX)
        }
    );
    let bt = ok(&[0x0f, 0xba, 0xe0, 0x05]);
    assert_eq!(bt.size, Size::B32);
    assert_eq!(
        bt.op,
        Op::BitTest {
            op: BitTestOp::Bt,
            dst: gpr(RAX),
            offset: Operand::Imm(5)
        }
    );
}

#[test]
fn sse_mandatory_prefixes_select_the_instruction() {
    let xmm = |n| Operand::Reg(Reg::Xmm(n));
    assert_eq!(
        ok(&[0x66, 0x0f, 0x6f, 0x0d, 0x10, 0, 0, 0]).op,
        Op::Sse {
            op: SseOp::Mov128 { aligned: true },
            dst: xmm(1),
            src: Operand::Mem(plain_mem(Base::Rip, None, 1, 0x10))
        }
    );
    assert_eq!(
        ok(&[0xf3, 0x44, 0x0f, 0x6f, 0x08]).op,
        Op::Sse {
            op: SseOp::Mov128 { aligned: false },
            dst: xmm(9),
            src: Operand::Mem(plain_mem(Base::Gpr(RAX), None, 1, 0))
        }
    );
    assert_eq!(
        ok(&[0x66, 0x0f, 0x74, 0xc1]).op,
        Op::Sse {
            op: SseOp::Lane {
                op: LaneOp::CmpEq,
                lane: Size::B8
            },
            dst: xmm(0),
            src: xmm(1)
        }
    );
    assert_eq!(
        ok(&[0x66, 0x48, 0x0f, 0x6e, 0xf0]).op,
        Op::Sse {
            op: SseOp::MovToXmm { bytes: 8 },
            dst: xmm(6),
            src: gpr(RAX)
        }
    );
    assert_eq!(
        ok(&[0x66, 0x0f, 0x73, 0xda, 0x04]).op,
        Op::Sse {
            op: SseOp::ByteShift {
                left: false,
                count: 4
            },
            dst: xmm(2),
            src: Operand::Imm(4)
        }
    );
    // Without a prefix, 0F 74 is MMX: pcmpeqb mm0, mm1 (REX does not
    // extend mm registers).
    let mm = |n| Operand::Reg(Reg::Mm(n));
    assert_eq!(
        ok(&[0x41, 0x0f, 0x74, 0xc1]).op,
        Op::Sse {
            op: SseOp::Lane {
                op: LaneOp::CmpEq,
                lane: Size::B8
            },
            dst: mm(0),
            src: mm(1)
        }
    );
    // movq mm3, [rax]; emms; pextrw eax, mm2, 6 (two index bits).
    assert_eq!(
        ok(&[0x0f, 0x6f, 0x18]).op,
        Op::Sse {
            op: SseOp::MovToXmm { bytes: 8 },
            dst: mm(3),
            src: Operand::Mem(Mem {
                segment: Segment::Default,
                base: Base::Gpr(0),
                index: None,
                scale: 1,
                disp: 0,
                addr32: false,
            })
        }
    );
    assert!(matches!(
        ok(&[0x0f, 0x77]).op,
        Op::Sse {
            op: SseOp::Emms,
            ..
        }
    ));
    assert!(matches!(
        ok(&[0x0f, 0xc5, 0xc2, 6]).op,
        Op::Sse {
            op: SseOp::Pextrw(2),
            ..
        }
    ));
    // The whole-register byte shifts have no MMX form; nor has 0F D6.
    assert_eq!(decode(&[0x0f, 0x73, 0xf9, 1]), Err(DecodeError::Invalid));
    assert_eq!(decode(&[0x0f, 0xd6, 0xc0]), Err(DecodeError::Invalid));
}

#[test]
fn lock_is_accepted_only_on_memory_read_modify_write() {
    let insn = ok(&[0xf0, 0x48, 0x0f, 0xb1, 0x1a]);
    assert!(insn.lock);
    assert_eq!(
        insn.op,
        Op::Cmpxchg {
            dst: Operand::Mem(plain_mem(Base::Gpr(RDX), None, 1, 0)),
            src: Reg::Gpr(RBX)
        }
    );
    // lock mov is invalid.
    assert_eq!(decode(&[0xf0, 0x89, 0x18]), Err(DecodeError::Invalid));
}

#[test]
fn integer_forms() {
    assert_eq!(
        ok(&[0x48, 0xd3, 0xe0]).op,
        Op::Shift {
            op: ShiftOp::Shl,
            dst: gpr(RAX),
            count: ShiftCount::Cl
        }
    );
    assert_eq!(
        ok(&[0x6b, 0xc1, 0x64]).op,
        Op::Imul {
            dst: Reg::Gpr(RAX),
            src: gpr(RCX),
            factor: Some(100)
        }
    );
    assert_eq!(
        ok(&[0x0f, 0x45, 0xd3]).op,
        Op::Cmov {
            cond: Cond::Ne,
            dst: Reg::Gpr(RDX),
            src: gpr(RBX)
        }
    );
    assert_eq!(
        ok(&[0x0f, 0x94, 0x43, 0x02]).op,
        Op::Setcc {
            cond: Cond::E,
            dst: Operand::Mem(plain_mem(Base::Gpr(RBX), None, 1, 2))
        }
    );
    assert_eq!(
        ok(&[0x0f, 0xb6, 0x06]).op,
        Op::Extend {
            signed: false,
            dst: Reg::Gpr(RAX),
            src: Operand::Mem(plain_mem(Base::Gpr(RSI), None, 1, 0)),
            src_size: Size::B8
        }
    );
    assert_eq!(
        ok(&[0xff, 0x24, 0xce]).op,
        Op::JmpIndirect(Operand::Mem(plain_mem(Base::Gpr(RSI), Some(RCX), 8, 0)))
    );
}

#[test]
fn string_instructions_carry_their_rep_prefix() {
    let movsq = ok(&[0xf3, 0x48, 0xa5]);
    assert_eq!(movsq.size, Size::B64);
    assert_eq!(
        movsq.op,
        Op::String {
            op: StringOp::Movs,
            rep: Some(Rep::Rep)
        }
    );
    let scasb = ok(&[0xf2, 0xae]);
    assert_eq!(scasb.size, Size::B8);
    assert_eq!(
        scasb.op,
        Op::String {
            op: StringOp::Scas,
            rep: Some(Rep::Repne)
        }
    );
}

#[test]
fn legacy_integer_forms() {
    // mov rax, [moffs64]: opcode, REX.W and an 8-byte address.
    let insn = ok(&[0x48, 0xa1, 8, 7, 6, 5, 4, 3, 2, 1]);
    assert_eq!(insn.len, 10);
    assert_eq!(
        insn.op,
        Op::Mov {
            dst: gpr(RAX),
            src: Operand::Mem(plain_mem(Base::None, None, 1, 0x0102030405060708)),
        }
    );
    // 67 A2: mov [moffs32], al with a 4-byte address, zero-extended.
    let insn = ok(&[0x67, 0xa2, 0, 0, 0, 0x80]);
    assert_eq!((insn.len, insn.size), (6, Size::B8));
    let Op::Mov {
        dst: Operand::Mem(m),
        ..
    } = insn.op
    else {
        panic!("{insn:?}")
    };
    assert_eq!((m.disp, m.addr32), (0x8000_0000, true));
    // enter 0x1234, 3: two immediates, 4 bytes.
    let insn = ok(&[0xc8, 0x34, 0x12, 3]);
    assert_eq!(
        (insn.len, insn.op),
        (
            4,
            Op::Enter {
                alloc: 0x1234,
                level: 3
            }
        )
    );
    assert_eq!(ok(&[0x9e]).op, Op::Sahf);
    assert_eq!(ok(&[0x9f]).op, Op::Lahf);
    assert_eq!(ok(&[0x9d]).op, Op::Popf);
    assert_eq!(ok(&[0x64, 0xd7]).op, Op::Xlat(Segment::Fs));
    // cmpxchg8b / cmpxchg16b: memory only, LOCK allowed.
    let insn = ok(&[0xf0, 0x48, 0x0f, 0xc7, 0x0e]);
    assert!(insn.lock);
    assert_eq!(
        (insn.size, insn.op),
        (
            Size::B64,
            Op::CmpxchgWide(plain_mem(Base::Gpr(RSI), None, 1, 0))
        )
    );
    assert_eq!(ok(&[0x0f, 0xc7, 0x0e]).size, Size::B32);
    assert_eq!(decode(&[0x0f, 0xc7, 0xce]), Err(DecodeError::Invalid));
    // LOCK on lahf is invalid.
    assert_eq!(decode(&[0xf0, 0x9f]), Err(DecodeError::Invalid));
}

#[test]
fn privileged_instructions_decode_with_their_length() {
    for (bytes, len) in [
        (&[0xe4, 0x60][..], 2), // in al, 0x60
        (&[0xe7, 0x80], 2),     // out 0x80, eax
        (&[0xec], 1),           // in al, dx
        (&[0xf3, 0x6c], 2),     // rep insb
        (&[0xfa], 1),           // cli
        (&[0xcd, 0x10], 2),     // int 0x10
    ] {
        let insn = ok(bytes);
        assert_eq!((insn.op, insn.len), (Op::Privileged, len), "{bytes:02x?}");
    }
    assert_eq!(ok(&[0xcd, 0x03]).op, Op::Int3);
    assert_eq!(ok(&[0xcd, 0x03]).len, 2);
    assert_eq!(ok(&[0xf1]).op, Op::Int1);
    assert!(matches!(
        decode(&[0xcd, 0x80]),
        Err(DecodeError::Unsupported(_))
    ));
    // mov [rax], ss: 16 bits whatever the operand size.
    let insn = ok(&[0x8c, 0x10]);
    assert_eq!(insn.size, Size::B16);
    assert_eq!(
        insn.op,
        Op::Mov {
            dst: Operand::Mem(plain_mem(Base::Gpr(RAX), None, 1, 0)),
            src: Operand::Imm(0x2b)
        }
    );
    // mov rax, cs: zero-extended to 64 bits.
    assert_eq!(ok(&[0x48, 0x8c, 0xc8]).size, Size::B64);
}

/// xorshift64: reproducible random numbers without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }
}

/// Iterations and seed of the fuzz tests: `ARMOR_FUZZ_ITERATIONS` and
/// `ARMOR_FUZZ_SEED` (`cargo xtask fuzz` sets both), else a quick run with
/// a fixed seed.
fn fuzz_config(default_iterations: u64) -> (u64, u64) {
    let var = |name: &str| std::env::var(name).ok().and_then(|v| v.parse().ok());
    let iterations = var("ARMOR_FUZZ_ITERATIONS").unwrap_or(default_iterations);
    let seed = var("ARMOR_FUZZ_SEED").unwrap_or(0x9e37_79b9_7f4a_7c15) | 1;
    (iterations, seed)
}

/// Bytes shaped like an instruction: prefixes, an escape or map selector,
/// an opcode, ModRM/SIB, displacement and immediate bytes, all random.
fn instruction_like(rng: &mut Rng) -> Vec<u8> {
    const PREFIXES: [u8; 12] = [
        0x66, 0x67, 0xf0, 0xf2, 0xf3, 0x2e, 0x3e, 0x26, 0x36, 0x64, 0x65, 0x40,
    ];
    let mut bytes = Vec::new();
    for _ in 0..rng.below(5) {
        let prefix = PREFIXES[rng.below(PREFIXES.len() as u64) as usize];
        // 0x40 stands for any REX byte.
        bytes.push(if prefix == 0x40 {
            0x40 | rng.byte() & 0xf
        } else {
            prefix
        });
    }
    match rng.below(8) {
        0 => bytes.push(0x0f),
        1 => bytes.extend([0x0f, if rng.below(2) == 0 { 0x38 } else { 0x3a }]),
        2 => bytes.push([0xc4, 0xc5, 0x62, 0xd8 + (rng.byte() & 7)][rng.below(4) as usize]),
        _ => {}
    }
    let tail = 1 + rng.below(16) as usize;
    bytes.extend((0..tail).map(|_| rng.byte()));
    bytes
}

/// The decoder reads bytes the guest controls. Whatever they are, it must
/// not panic, and a successful decode must be self-consistent: a length of
/// 1 to 15 bytes, the same instruction from exactly those bytes, and
/// "truncated" from any shorter prefix of them.
#[test]
fn fuzz_decoder() {
    let (iterations, seed) = fuzz_config(200_000);
    let mut rng = Rng(seed);
    for i in 0..iterations {
        let bytes = if rng.below(4) == 0 {
            (0..rng.below(20)).map(|_| rng.byte()).collect()
        } else {
            instruction_like(&mut rng)
        };
        let context = || format!("iteration {i}, seed {seed:#x}, bytes {bytes:02x?}");
        // The length-only decoder agrees with every successful decode.
        let measured = crate::length(&bytes);
        if let Ok(insn) = decode(&bytes) {
            assert_eq!(
                measured.map(|l| l.len),
                Ok(insn.len),
                "length: {}",
                context()
            );
        }
        if let Ok(l) = measured {
            assert!((1..=MAX_INSTRUCTION_LEN).contains(&l.len) && l.len <= bytes.len());
        }
        match decode(&bytes) {
            Ok(insn) => {
                assert!(
                    (1..=MAX_INSTRUCTION_LEN).contains(&insn.len) && insn.len <= bytes.len(),
                    "length {}: {}",
                    insn.len,
                    context()
                );
                assert_eq!(decode(&bytes[..insn.len]), Ok(insn), "{}", context());
                for short in 0..insn.len {
                    assert_eq!(
                        decode(&bytes[..short]),
                        Err(DecodeError::Truncated),
                        "prefix of {short} bytes: {}",
                        context()
                    );
                }
            }
            Err(DecodeError::Truncated) => {
                assert!(bytes.len() < MAX_INSTRUCTION_LEN, "{}", context());
            }
            Err(_) => {}
        }
    }
}

/// Lengths and extensions of instructions armor does not execute, with
/// bytes and lengths from objdump (`decoder-check` compares all of them on
/// the guest root: 11 million instructions).
#[test]
fn length_only_decoder() {
    use crate::{Extension, length};
    let cases: [(&[u8], Extension); 19] = [
        // vpcmpeqb ymm1, ymm0, [rdi]
        (&[0xc5, 0xfd, 0x74, 0x0f], Extension::Vex),
        // vzeroupper: no ModRM.
        (&[0xc5, 0xf8, 0x77], Extension::Vex),
        // vpalignr ymm0, ymm1, ymm2, 4 (map 3: an immediate).
        (&[0xc4, 0xe3, 0x75, 0x0f, 0xc2, 0x04], Extension::Vex),
        // shlx eax, [rsi+0x10], ecx (BMI2, map 2).
        (&[0xc4, 0xe2, 0x71, 0xf7, 0x46, 0x10], Extension::Vex),
        // vmovdqu64 zmm0, [rdi+0x40] (EVEX, compressed disp8).
        (&[0x62, 0xf1, 0xfe, 0x48, 0x6f, 0x47, 0x01], Extension::Evex),
        // vpcmpub k1, zmm0, [rdi], 1 (EVEX map 3: an immediate).
        (&[0x62, 0xf3, 0x7d, 0x48, 0x3e, 0x0f, 0x01], Extension::Evex),
        // movq mm0, [rdi]
        (&[0x0f, 0x6f, 0x07], Extension::Mmx),
        // pfadd mm0, mm1 (3DNow!: trailing operation byte).
        (&[0x0f, 0x0f, 0xc1, 0x9e], Extension::Amd3dNow),
        // aesenc xmm0, xmm1
        (&[0x66, 0x0f, 0x38, 0xdc, 0xc1], Extension::Aes),
        // pclmulqdq xmm0, xmm1, 0
        (&[0x66, 0x0f, 0x3a, 0x44, 0xc1, 0x00], Extension::Pclmul),
        // sha256rnds2 xmm1, xmm2 (xmm0 implicit)
        (&[0x0f, 0x38, 0xcb, 0xca], Extension::Sha),
        // vprotd xmm0, xmm1, 3 (XOP map 8: an immediate).
        (&[0x8f, 0xe8, 0x78, 0xc2, 0xc1, 0x03], Extension::Xop),
        // xsha256 (VIA PadLock)
        (&[0xf3, 0x0f, 0xa6, 0xd0], Extension::Padlock),
        // xgetbv
        (&[0x0f, 0x01, 0xd0], Extension::Xsave),
        // xsaveopt [rsp+0x40]
        (&[0x0f, 0xae, 0x74, 0x24, 0x40], Extension::Xsave),
        // xsavec [rsp+0x40]
        (&[0x0f, 0xc7, 0x64, 0x24, 0x40], Extension::Xsave),
        // movbe eax, [rcx]
        (&[0x0f, 0x38, 0xf0, 0x01], Extension::Movbe),
        // rdrand eax (0F C7 /6, a register): not XSAVE.
        (&[0x0f, 0xc7, 0xf0], Extension::Base),
        // mov rax, imm64
        (&[0x48, 0xb8, 1, 2, 3, 4, 5, 6, 7, 8], Extension::Base),
    ];
    for (bytes, extension) in cases {
        let mut padded = bytes.to_vec();
        padded.extend([0x90; 8]);
        assert_eq!(
            length(&padded),
            Ok(crate::Length {
                len: bytes.len(),
                extension
            }),
            "{bytes:02x?}"
        );
        assert_eq!(
            length(&bytes[..bytes.len() - 1]),
            Err(DecodeError::Truncated),
            "{bytes:02x?}"
        );
    }
    // VEX after a 66 prefix, and removed one-byte opcodes: #UD.
    assert_eq!(length(&[0x66, 0xc5, 0xf8, 0x77]), Err(DecodeError::Invalid));
    assert_eq!(length(&[0x06]), Err(DecodeError::Invalid));
    // 15 prefixes and an opcode: too long.
    assert_eq!(length(&[0x66; 16]), Err(DecodeError::TooLong));
}

/// Compatibility mode (ADR-0029), bytes and decodings from objdump on
/// i386 binaries (`decoder-check` compares all of glibc i386 and the
/// Steam client).
#[test]
fn compatibility_mode_decoding() {
    use crate::{Mode, decode_in, length_in};
    let d = |bytes: &[u8]| decode_in(bytes, Mode::Compat32).unwrap();
    // 40–4F: inc and dec, not REX.
    let inc = d(&[0x40]);
    assert_eq!(
        inc.op,
        Op::Unary {
            op: UnaryOp::Inc,
            dst: gpr(RAX)
        }
    );
    assert_eq!((inc.size, inc.len), (Size::B32, 1));
    assert_eq!(
        d(&[0x4b]).op,
        Op::Unary {
            op: UnaryOp::Dec,
            dst: gpr(RBX)
        }
    );
    // push and call are 32 bits wide; addresses are 32 bits.
    let push = d(&[0x55]);
    assert_eq!((push.op, push.size), (Op::Push(gpr(RBP)), Size::B32));
    assert!(push.addr32);
    assert_eq!(d(&[0xe8, 0, 0, 0, 0]).size, Size::B32);
    // mod 00 rm 101: an absolute address, not RIP-relative.
    let load = d(&[0x8b, 0x05, 0x78, 0x56, 0x34, 0x12]);
    assert_eq!(
        load.op,
        Op::Mov {
            dst: gpr(RAX),
            src: Operand::Mem(Mem {
                segment: Segment::Default,
                base: Base::None,
                index: None,
                scale: 1,
                disp: 0x1234_5678,
                addr32: true,
            })
        }
    );
    // int 0x80 is the syscall; mov gs, ax loads a TLS selector.
    assert_eq!(d(&[0xcd, 0x80]).op, Op::Int80);
    assert_eq!(
        d(&[0x8e, 0xe8]).op,
        Op::LoadSegment {
            segment: Segment::Gs,
            src: Operand::Reg(Reg::Gpr(RAX))
        }
    );
    // mov eax, gs reads the selector the program loaded.
    assert_eq!(
        d(&[0x8c, 0xe8]).op,
        Op::ReadSegment {
            segment: Segment::Gs,
            dst: Operand::Reg(Reg::Gpr(RAX))
        }
    );
    // moffs: a 4-byte address.
    assert_eq!(d(&[0xa1, 1, 2, 3, 4]).len, 5);
    // syscall does not exist there.
    assert_eq!(
        decode_in(&[0x0f, 0x05], Mode::Compat32),
        Err(DecodeError::Invalid)
    );
    // Lengths: les (ModRM), far call (pointer), aam (byte), 16-bit
    // addressing with 67 (disp16 for mod 00 rm 110).
    let len = |bytes: &[u8]| length_in(bytes, Mode::Compat32).map(|l| l.len);
    assert_eq!(len(&[0xc4, 0x06]), Ok(2));
    assert_eq!(len(&[0x9a, 1, 2, 3, 4, 5, 6]), Ok(7));
    assert_eq!(len(&[0xd4, 0x0a]), Ok(2));
    assert_eq!(len(&[0x67, 0x8b, 0x06, 0x34, 0x12]), Ok(5));
    // But c4 with mod 11 is VEX, as in 64-bit mode.
    assert_eq!(
        length_in(&[0xc5, 0xf8, 0x77], Mode::Compat32).map(|l| l.extension),
        Ok(crate::Extension::Vex)
    );
}
