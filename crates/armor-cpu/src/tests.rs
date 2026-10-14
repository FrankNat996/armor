use super::*;

/// Flat guest memory for tests: one readable, writable and executable
/// region starting at `BASE`.
struct FlatMemory {
    bytes: std::cell::RefCell<Vec<u8>>,
}

const BASE: u64 = 0x40_0000;
const STACK_TOP: u64 = BASE + 0x1000;

impl FlatMemory {
    fn with_code(code: &[u8]) -> FlatMemory {
        let mut bytes = vec![0u8; 0x1000];
        bytes[..code.len()].copy_from_slice(code);
        FlatMemory {
            bytes: std::cell::RefCell::new(bytes),
        }
    }

    fn range(&self, addr: u64, len: usize, access: Access) -> Result<usize, MemFault> {
        let start = addr.checked_sub(BASE).ok_or(MemFault { addr, access })? as usize;
        let size = self.bytes.borrow().len();
        if start + len > size {
            let addr = BASE + size as u64;
            return Err(MemFault { addr, access });
        }
        Ok(start)
    }
}

impl GuestMemory for FlatMemory {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemFault> {
        let start = self.range(addr, buf.len(), Access::Read)?;
        buf.copy_from_slice(&self.bytes.borrow()[start..start + buf.len()]);
        Ok(())
    }

    fn write(&self, addr: u64, data: &[u8]) -> Result<(), MemFault> {
        let start = self.range(addr, data.len(), Access::Write)?;
        self.bytes.borrow_mut()[start..start + data.len()].copy_from_slice(data);
        Ok(())
    }

    fn fetch(&self, addr: u64, buf: &mut [u8]) -> usize {
        let Some(start) = addr.checked_sub(BASE).map(|s| s as usize) else {
            return 0;
        };
        let bytes = self.bytes.borrow();
        let n = buf.len().min(bytes.len().saturating_sub(start));
        buf[..n].copy_from_slice(&bytes[start..start + n]);
        n
    }
}

/// Runs `code` from `BASE` for `steps` instructions.
fn run(code: &[u8], steps: usize) -> (Cpu, FlatMemory) {
    let mem = FlatMemory::with_code(code);
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    for _ in 0..steps {
        cpu.step(&mem).unwrap();
    }
    (cpu, mem)
}

fn status(cpu: &Cpu) -> u64 {
    cpu.rflags & STATUS_FLAGS
}

#[test]
fn initial_state_matches_process_entry() {
    let cpu = Cpu::new(0x401000, 0x7000);
    assert_eq!(cpu.rip, 0x401000);
    assert_eq!(cpu.regs[RSP], 0x7000);
    assert_eq!(cpu.rflags, 0x202);
    assert!(
        cpu.regs
            .iter()
            .enumerate()
            .all(|(i, &r)| i == RSP || r == 0)
    );
}

#[test]
fn hello_sequence_reaches_syscall_with_arguments() {
    // mov eax,1 ; mov edi,1 ; lea rsi,[rip+0xfef] ; mov edx,6 ; syscall
    let code = [
        0xb8, 1, 0, 0, 0, 0xbf, 1, 0, 0, 0, 0x48, 0x8d, 0x35, 0xef, 0x0f, 0, 0, 0xba, 6, 0, 0, 0,
        0x0f, 0x05,
    ];
    let mem = FlatMemory::with_code(&code);
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    cpu.run_until_syscall(&mem).unwrap();
    assert_eq!(cpu.regs[RAX], 1);
    assert_eq!(cpu.regs[RDI], 1);
    // lea is at BASE+10 and is 7 bytes long.
    assert_eq!(cpu.regs[RSI], BASE + 17 + 0xfef);
    assert_eq!(cpu.regs[RDX], 6);
    assert_eq!(cpu.rip, BASE + code.len() as u64);
    assert_eq!(cpu.regs[RCX], cpu.rip);
    assert_eq!(cpu.regs[R11], INITIAL_RFLAGS);
}

#[test]
fn register_writes_follow_size_rules() {
    let mut cpu = Cpu::new(0, 0);
    cpu.regs[RAX] = 0x1122_3344_5566_7788;
    cpu.write_reg(Reg::Gpr(0), Size::B8, 0xaa);
    assert_eq!(cpu.regs[RAX], 0x1122_3344_5566_77aa);
    cpu.write_reg(Reg::HighByte(0), Size::B8, 0xbb);
    assert_eq!(cpu.regs[RAX], 0x1122_3344_5566_bbaa);
    cpu.write_reg(Reg::Gpr(0), Size::B16, 0xcccc);
    assert_eq!(cpu.regs[RAX], 0x1122_3344_5566_cccc);
    cpu.write_reg(Reg::Gpr(0), Size::B32, 0xdddd_dddd);
    assert_eq!(cpu.regs[RAX], 0x0000_0000_dddd_dddd);
    assert_eq!(cpu.read_reg(Reg::HighByte(0), Size::B8), 0xdd);
}

#[test]
fn alu_with_memory_destination() {
    // From objdump: mov QWORD PTR [rip+0x100],0x5 ; add QWORD PTR [rip+0xf8],-6
    // Both address BASE+0x10b.
    let code = [
        0x48, 0xc7, 0x05, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, //
        0x48, 0x83, 0x05, 0xf8, 0x00, 0x00, 0x00, 0xfa,
    ];
    let (cpu, mem) = run(&code, 2);
    let mut value = [0u8; 8];
    mem.read(BASE + 0x10b, &mut value).unwrap();
    assert_eq!(i64::from_le_bytes(value), -1);
    // 5 + 0xff..fa = 0xff..ff: no carry, no overflow, AF clear, even parity.
    assert_eq!(status(&cpu), FLAG_SF | FLAG_PF);
}

#[test]
fn push_pop_call_ret() {
    // 0: push 0x12 ; 2: call +3 ; 7: pop rbx (skipped) ; 8: nop-like filler
    // 10: pop rax (return address) ; 11: pop rcx (0x12)
    let code = [
        0x6a, 0x12, // push 0x12
        0xe8, 0x03, 0, 0, 0, // call BASE+10
        0x5b, 0x5b, 0x5b, // pop rbx x3 (not executed)
        0x58, // pop rax
        0x59, // pop rcx
    ];
    let (cpu, _) = run(&code, 4);
    assert_eq!(cpu.regs[RAX], BASE + 7);
    assert_eq!(cpu.regs[RCX], 0x12);
    assert_eq!(cpu.regs[RSP], STACK_TOP);
    assert_eq!(cpu.regs[RBX], 0);
}

#[test]
fn conditional_jumps_follow_flags() {
    // xor eax,eax ; je +2 ; mov al,1 (skipped) ; cmp eax,1 ; jl -> taken
    let code = [
        0x31, 0xc0, // xor eax,eax (ZF=1)
        0x74, 0x02, // je +2
        0xb0, 0x01, // mov al,1 (skipped)
        0x83, 0xf8, 0x01, // cmp eax,1
        0x7c, 0x10, // jl +0x10
    ];
    let (cpu, _) = run(&code, 4);
    assert_eq!(cpu.regs[RAX], 0);
    assert_eq!(cpu.rip, BASE + 11 + 0x10);
}

#[test]
fn faulting_instruction_changes_nothing() {
    // mov rax, 1 ; mov [rax], rbx  (address 1 is outside the flat memory)
    let code = [0x48, 0xc7, 0xc0, 1, 0, 0, 0, 0x48, 0x89, 0x18];
    let mem = FlatMemory::with_code(&code);
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    cpu.step(&mem).unwrap();
    let before = cpu.clone();
    let err = cpu.step(&mem).unwrap_err();
    assert_eq!(
        err,
        CpuError::Fault {
            rip: BASE + 7,
            fault: MemFault {
                addr: 1,
                access: Access::Write
            }
        }
    );
    assert_eq!(cpu, before);
}

#[test]
fn unsupported_and_invalid_instructions_are_reported() {
    let mem = FlatMemory::with_code(&[0x27]);
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    assert!(matches!(
        cpu.step(&mem),
        Err(CpuError::Decode {
            error: DecodeError::Invalid,
            ..
        })
    ));
}

#[test]
fn running_off_executable_memory_is_an_execute_fault() {
    let mem = FlatMemory::with_code(&[]);
    // rip at the first byte past the flat memory: nothing can be fetched.
    let mut cpu = Cpu::new(BASE + 0x1000, STACK_TOP);
    assert_eq!(
        cpu.step(&mem),
        Err(CpuError::Fault {
            rip: BASE + 0x1000,
            fault: MemFault {
                addr: BASE + 0x1000,
                access: Access::Execute
            }
        })
    );
}

#[test]
fn ldmxcsr_with_a_reserved_bit_is_a_general_protection_fault() {
    // ldmxcsr [0x400800], with 0x10000 (bit 16, reserved) stored there.
    // qemu accepts it; the Intel SDM specifies #GP.
    let mem = FlatMemory::with_code(&[0x0f, 0xae, 0x14, 0x25, 0x00, 0x08, 0x40, 0x00]);
    mem.write(BASE + 0x800, &0x1_0000u32.to_le_bytes()).unwrap();
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    assert!(matches!(
        cpu.step(&mem),
        Err(CpuError::Exception {
            exception: Exception::GeneralProtection,
            ..
        })
    ));
    assert_eq!(cpu.mxcsr, 0x1f80);
}

#[test]
fn rdtsc_never_goes_backwards() {
    // rdtsc ; mov rbx, rax ; mov rcx, rdx ; rdtsc
    let code = [0x0f, 0x31, 0x48, 0x89, 0xc3, 0x48, 0x89, 0xd1, 0x0f, 0x31];
    let (cpu, _) = run(&code, 4);
    let first = cpu.regs[RCX] << 32 | cpu.regs[RBX];
    let second = cpu.regs[RDX] << 32 | cpu.regs[RAX];
    assert!(second >= first);
    assert!(cpu.regs[RAX] <= 0xffff_ffff && cpu.regs[RDX] <= 0xffff_ffff);
}

/// x87 stack faults, which qemu does not implement (docs/testing.md): the
/// masked responses of the Intel SDM, vol. 1, 8.5.1.1.
#[test]
fn x87_stack_underflow_and_overflow_give_the_indefinite() {
    // fadd st0, st1 on an empty stack.
    let (cpu, _) = run(&[0xd8, 0xc1], 1);
    assert_eq!(cpu.x87.stack_register(0), f80::F80::INDEFINITE);
    // IE and SF, C1 = 0 (underflow), TOP unchanged.
    assert_eq!(cpu.x87.status_word(), 0x0041);
    // Nine fld1: the ninth overflows (C1 = 1) and pushes the indefinite.
    let (cpu, _) = run(&[0xd9, 0xe8].repeat(9), 9);
    assert_eq!(cpu.x87.stack_register(0), f80::F80::INDEFINITE);
    assert_eq!(cpu.x87.status_word() & 0x3fff, 0x0241 | (7 << 11));
}

#[test]
fn x87_control_words_with_unmasked_exceptions_or_reserved_precision_are_refused() {
    // fldcw [0x400800] with 0x037e (invalid-operation exception unmasked).
    let mem = FlatMemory::with_code(&[0xd9, 0x2c, 0x25, 0x00, 0x08, 0x40, 0x00]);
    mem.write(BASE + 0x800, &0x037eu16.to_le_bytes()).unwrap();
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    assert!(matches!(cpu.step(&mem), Err(CpuError::Unsupported { .. })));
    // 0x017f: the reserved precision-control value.
    mem.write(BASE + 0x800, &0x017fu16.to_le_bytes()).unwrap();
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    assert!(matches!(cpu.step(&mem), Err(CpuError::Unsupported { .. })));
    // 0x027f: 53-bit precision, as python sets it.
    mem.write(BASE + 0x800, &0x027fu16.to_le_bytes()).unwrap();
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    cpu.step(&mem).unwrap();
    assert_eq!(cpu.x87.control_word(), 0x027f);
    // Rounding control is supported: 0x0f7f (truncate) is accepted.
    mem.write(BASE + 0x800, &0x0f7fu16.to_le_bytes()).unwrap();
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    cpu.step(&mem).unwrap();
    assert_eq!(cpu.x87.control_word(), 0x0f7f);
}

/// A partial fprem (exponents 64 or more apart) sets C2 and clears C0, C1
/// and C3, as an Intel Xeon X5675 does (the hardware oracle,
/// docs/testing.md). fxam of a negative normal first sets C1.
#[test]
fn x87_partial_fprem_clears_the_other_condition_codes() {
    let mem = FlatMemory::with_code(&[
        0xdb, 0x2c, 0x25, 0x00, 0x08, 0x40, 0x00, // fld tword [0x400800] (1)
        0xdb, 0x2c, 0x25, 0x10, 0x08, 0x40, 0x00, // fld tword [0x400810] (-2^100)
        0xd9, 0xe5, // fxam
        0xd9, 0xf8, // fprem
    ]);
    mem.write(BASE + 0x800, &f80::F80::ONE.to_bytes()).unwrap();
    let huge = f80::F80 {
        sign: true,
        exp: 0x3fff + 100,
        mant: 1 << 63,
    };
    mem.write(BASE + 0x810, &huge.to_bytes()).unwrap();
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    for _ in 0..3 {
        cpu.step(&mem).unwrap();
    }
    // fxam: negative normal, C1 and C2.
    assert_eq!(cpu.x87.status_word() & 0x4700, 0x0600);
    cpu.step(&mem).unwrap();
    assert_eq!(cpu.x87.status_word() & 0x4700, 0x0400);
}

/// fxsave writes zeros in the 6 reserved bytes after each 80-bit register,
/// as an Intel Xeon X5675 does (the hardware oracle; qemu leaves them as
/// they were, docs/testing.md), and leaves 416..512 alone.
#[test]
fn fxsave_zeroes_the_register_padding() {
    // fxsave [0x400800]
    let mem = FlatMemory::with_code(&[0x0f, 0xae, 0x04, 0x25, 0x00, 0x08, 0x40, 0x00]);
    mem.write(BASE + 0x800, &[0xa5; 512]).unwrap();
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    cpu.step(&mem).unwrap();
    let mut area = [0u8; 512];
    mem.read(BASE + 0x800, &mut area).unwrap();
    for i in 0..8 {
        let at = 32 + 16 * i;
        assert_eq!(area[at + 10..at + 16], [0; 6], "register {i}");
    }
    assert!(area[416..].iter().all(|&b| b == 0xa5));
}

#[test]
fn x87_denormal_operands_set_de() {
    // fld qword [0x400800] with the smallest double denormal: DE.
    let mem = FlatMemory::with_code(&[0xdd, 0x04, 0x25, 0x00, 0x08, 0x40, 0x00]);
    mem.write(BASE + 0x800, &1u64.to_le_bytes()).unwrap();
    let mut cpu = Cpu::new(BASE, STACK_TOP);
    cpu.step(&mem).unwrap();
    assert_eq!(cpu.x87.status_word() & 0x3f, f80::FLAG_DENORMAL);
}

/// Unmasked SSE exceptions (#XM), as on an Intel Xeon X5675
/// (`c/sse_exceptions`, which only the hardware oracle compares): the
/// destination and RFLAGS are not written; with an unmasked exception
/// raised before the computation (IE, DE, ZE) MXCSR gets only those flags,
/// from every lane, otherwise all of them; a flag already set and unmasked
/// raises nothing.
#[test]
fn unmasked_sse_exceptions_follow_the_hardware() {
    const ONE: u64 = 0x3ff0_0000_0000_0000;
    const THREE: u64 = 0x4008_0000_0000_0000;
    const MAX: u64 = 0x7fef_ffff_ffff_ffff;
    const MIN: u64 = 0x0010_0000_0000_0000;
    const DEN: u64 = 1;
    const QNAN: u64 = 0x7ff8_0000_0000_0123;
    let pair = |lo: u64, hi: u64| u128::from(lo) | u128::from(hi) << 64;
    /// Code, MXCSR, xmm0, xmm1, MXCSR after, whether #XM is raised.
    type Case = (&'static [u8], u32, u128, u128, u32, bool);
    let cases: [Case; 8] = [
        // divpd [1/0, 0/0], ZE unmasked: ZE and IE.
        (
            &[0x66, 0x0f, 0x5e, 0xc1],
            0x1d80,
            pair(ONE, 0),
            0,
            0x1d85,
            true,
        ),
        // addsd 1 + den, PE unmasked: DE and PE.
        (
            &[0xf2, 0x0f, 0x58, 0xc1],
            0x0f80,
            pair(ONE, 0),
            pair(DEN, 0),
            0x0fa2,
            true,
        ),
        // addpd [1 + den, 1 + 1/3], DE unmasked: DE alone.
        (
            &[0x66, 0x0f, 0x58, 0xc1],
            0x1e80,
            pair(ONE, ONE),
            pair(DEN, 0x3fd5_5555_5555_5555),
            0x1e82,
            true,
        ),
        // mulsd max × 2, OE unmasked: OE alone (exact with an unbounded
        // exponent).
        (
            &[0xf2, 0x0f, 0x59, 0xc1],
            0x1b80,
            pair(MAX, 0),
            pair(0x4000_0000_0000_0000, 0),
            0x1b88,
            true,
        ),
        // divpd [1/3, min/2], UE unmasked: UE (exact) and PE (other lane).
        (
            &[0x66, 0x0f, 0x5e, 0xc1],
            0x1780,
            pair(ONE, MIN),
            pair(THREE, 0x4000_0000_0000_0000),
            0x17b0,
            true,
        ),
        // divsd min/3 with FZ and UE unmasked: FZ does not apply.
        (
            &[0xf2, 0x0f, 0x5e, 0xc1],
            0x9780,
            pair(MIN, 0),
            pair(THREE, 0),
            0x97b0,
            true,
        ),
        // comisd with a QNaN, IE unmasked.
        (
            &[0x66, 0x0f, 0x2f, 0xc1],
            0x1f00,
            pair(ONE, 0),
            pair(QNAN, 0),
            0x1f01,
            true,
        ),
        // addsd 1 + 1 with ZE already set and unmasked: nothing.
        (
            &[0xf2, 0x0f, 0x58, 0xc1],
            0x1d84,
            pair(ONE, 0),
            pair(ONE, 0),
            0x1d84,
            false,
        ),
    ];
    for (code, mxcsr, a, b, after, raised) in cases {
        let mem = FlatMemory::with_code(code);
        let mut cpu = Cpu::new(BASE, STACK_TOP);
        cpu.mxcsr = mxcsr;
        cpu.xmm[0] = a;
        cpu.xmm[1] = b;
        let rflags = cpu.rflags;
        let result = cpu.step(&mem);
        assert_eq!(cpu.mxcsr, after, "{code:02x?} with {mxcsr:#x}");
        if raised {
            assert!(
                matches!(
                    result,
                    Err(CpuError::Exception {
                        exception: Exception::SimdFloatingPoint,
                        rip: BASE,
                    })
                ),
                "{code:02x?}: {result:?}"
            );
            assert_eq!(cpu.xmm[0], a, "{code:02x?}: destination written");
            assert_eq!(cpu.rflags, rflags);
            assert_eq!(cpu.rip, BASE);
        } else {
            assert!(result.is_ok());
            assert_eq!(cpu.xmm[0], pair(0x4000_0000_0000_0000, 0));
        }
    }
}
