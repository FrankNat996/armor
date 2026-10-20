//! The JIT against the interpreter: the same code from the same state must
//! end in the same registers, flags, memory and errors (ADR-0021 §9; the
//! interpreter is the oracle, itself checked against qemu by `difftest`).

use std::sync::atomic::{AtomicU64, Ordering};

use armor_cpu::{Cpu, CpuError, RBX, RSP, STATUS_FLAGS, Step};
use armor_mem::{AddressSpace, GuestMemory, Prot};
use armor_x86::Mode;

use crate::Jit;

/// Tests run in parallel in one process: each gets its own addresses.
static NEXT_SLOT: AtomicU64 = AtomicU64::new(0);

/// Code, data and stack of one test.
struct Guest {
    mem: AddressSpace,
    code: u64,
    data: u64,
    stack_top: u64,
    /// The host extensions the JIT may use (`None`: the host's).
    features: Option<crate::translator::HostFeatures>,
}

const CODE_SIZE: u64 = 0x4000;
/// Two pages: accesses can cross from the first into the second.
const DATA_SIZE: u64 = 0x2000;
const STACK_SIZE: u64 = 0x2000;

impl Guest {
    fn new() -> Guest {
        let slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
        // Below 2^39: kernels with 4K pages often give processes 39-bit
        // addresses (the Raspberry Pi's 4K kernel does), 16K ones 47-bit.
        Guest::at(0x30_0000_0000 + slot * 0x100_0000)
    }

    /// A guest below 4 GiB, for code in compatibility mode (ADR-0029).
    fn new_low() -> Guest {
        let slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
        Guest::at(0x6000_0000 + slot * 0x100_0000)
    }

    fn at(base: u64) -> Guest {
        Guest::in_space(AddressSpace::new(), base)
    }

    fn in_space(mem: AddressSpace, base: u64) -> Guest {
        let (code, data, stack) = (base, base + 0x10_0000, base + 0x20_0000);
        mem.map_fixed(code, CODE_SIZE, Prot::READ | Prot::EXEC)
            .unwrap();
        mem.map_fixed(data, DATA_SIZE, Prot::READ | Prot::WRITE)
            .unwrap();
        // A guard page: mapped, but no access at all; then a read-only page.
        mem.map_fixed(data + DATA_SIZE, 0x1000, Prot::NONE).unwrap();
        mem.map_fixed(data + DATA_SIZE + 0x1000, 0x1000, Prot::READ)
            .unwrap();
        mem.write_privileged(data + DATA_SIZE + 0x1000, &[0x5a; 0x1000])
            .unwrap();
        mem.map_fixed(stack, STACK_SIZE, Prot::READ | Prot::WRITE)
            .unwrap();
        Guest {
            mem,
            code,
            data,
            stack_top: stack + STACK_SIZE / 2,
            features: None,
        }
    }

    fn memory(&self) -> Vec<u8> {
        let mut bytes = vec![0u8; (DATA_SIZE + STACK_SIZE) as usize];
        let (data, stack) = bytes.split_at_mut(DATA_SIZE as usize);
        self.mem.read(self.data, data).unwrap();
        self.mem
            .read(self.stack_top - STACK_SIZE / 2, stack)
            .unwrap();
        bytes
    }

    fn fill_memory(&self, seed: u64) {
        let mut rng = Rng(seed | 1);
        let data: Vec<u8> = (0..DATA_SIZE).map(|_| rng.next() as u8).collect();
        self.mem.write_privileged(self.data, &data).unwrap();
        let stack = vec![0u8; STACK_SIZE as usize];
        self.mem
            .write_privileged(self.stack_top - STACK_SIZE / 2, &stack)
            .unwrap();
    }

    /// Runs `code` (followed by a `syscall`, or `int 0x80` in
    /// compatibility mode) from `cpu` under both engines and checks that
    /// they agree.
    fn compare(&self, code: &[u8], cpu: &Cpu, seed: u64) {
        let mut bytes = code.to_vec();
        bytes.extend_from_slice(match cpu.mode {
            Mode::Long => &[0x0f, 0x05],
            Mode::Compat32 => &[0xcd, 0x80],
        });
        assert!(bytes.len() as u64 <= CODE_SIZE);

        // The code too may be written (writes_to_code_...): reset it.
        self.mem.write_privileged(self.code, &bytes).unwrap();
        let mut interpreted = cpu.clone();
        self.fill_memory(seed);
        let expected = interpreted.run(&self.mem.view(), 100_000);
        let expected_memory = self.memory();

        self.mem.write_privileged(self.code, &bytes).unwrap();
        let mut jitted = cpu.clone();
        self.fill_memory(seed);
        let mut jit = Jit::new().unwrap();
        if let Some(features) = self.features {
            jit.set_features(features);
        }
        let got = jit.run(&mut jitted, &self.mem.view(), 100_000);
        let jitted_memory = self.memory();

        // Lockstep panics at the first instruction that differs.
        self.mem.write_privileged(self.code, &bytes).unwrap();
        let mut stepped = cpu.clone();
        self.fill_memory(seed);
        let mut lockstep = Jit::lockstep().unwrap();
        if let Some(features) = self.features {
            lockstep.set_features(features);
        }
        let checked = lockstep.run(&mut stepped, &self.mem.view(), 100_000);
        assert_eq!(checked, expected, "lockstep result");
        assert_eq!(stepped.regs, interpreted.regs, "lockstep registers");
        assert_eq!(stepped.xmm, interpreted.xmm, "lockstep xmm");
        assert_eq!(stepped.mxcsr, interpreted.mxcsr, "lockstep mxcsr");
        assert_eq!(stepped.fpu_state(), interpreted.fpu_state(), "lockstep x87");
        assert!(self.memory() == expected_memory, "lockstep memory");
        let context = || format!("code {code:02x?}\ninitial {:x?}", (cpu.regs, cpu.rflags));
        assert_ne!(expected, Ok(Step::Continue), "{}", context());
        assert_eq!(got, expected, "result; {}", context());
        assert_eq!(jitted.regs, interpreted.regs, "registers; {}", context());
        assert_eq!(jitted.rip, interpreted.rip, "rip; {}", context());
        assert_eq!(jitted.rflags, interpreted.rflags, "rflags; {}", context());
        assert_eq!(jitted.xmm, interpreted.xmm, "xmm; {}", context());
        assert_eq!(jitted.mxcsr, interpreted.mxcsr, "mxcsr; {}", context());
        assert_eq!(
            jitted.fpu_state(),
            interpreted.fpu_state(),
            "x87 and MMX; {}",
            context()
        );
        assert!(jitted_memory == expected_memory, "memory; {}", context());
    }

    fn cpu(&self, rng: &mut Rng) -> Cpu {
        let mut cpu = Cpu::new(self.code, self.stack_top);
        for (n, reg) in cpu.regs.iter_mut().enumerate() {
            *reg = match n {
                RSP => self.stack_top,
                RBX => self.data,
                _ => rng.value(),
            };
        }
        cpu.rflags = 0x202 | (rng.next() & STATUS_FLAGS);
        for x in cpu.xmm.iter_mut() {
            *x = u128::from(rng.next()) << 64 | u128::from(rng.next());
        }
        cpu
    }
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

    /// Values that exercise carries, overflows and signs.
    fn value(&mut self) -> u64 {
        match self.below(6) {
            0 => 0,
            1 => self.below(4),
            2 => u64::MAX - self.below(4),
            3 => 0x7fff_ffff + self.below(3),
            4 => 0x8000_0000_0000_0000 - self.below(3),
            _ => self.next(),
        }
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize]
    }
}

/// Registers the random code may write: not rsp (the stack) nor rbx (the
/// base of every memory operand).
const FREE: [u8; 14] = [0, 1, 2, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// One random instruction for the translator's subset (and a few it
/// leaves to the interpreter, to exercise the fallback).
fn random_instruction(rng: &mut Rng, out: &mut Vec<u8>) {
    let w = rng.below(2) as u8;
    let reg = rng.pick(&FREE);
    let rm = rng.pick(&FREE);
    let rex = |w: u8, r: u8, b: u8| 0x40 | (w << 3) | ((r >> 3) << 2) | (b >> 3);
    // [rbx + disp]: mostly inside the first data page; sometimes crossing
    // into the second; sometimes crossing out of the mapping or beyond it
    // (a fault, although the host page may still be mapped).
    let mem = |rng: &mut Rng, reg: u8, out: &mut Vec<u8>| {
        let disp: u32 = match rng.below(13) {
            // The read-only page: reads work, writes fault.
            12 => 0x3000 + rng.below(0x100) as u32,
            0 => 0xffc + rng.below(4) as u32,
            1 => 0x1ff9 + rng.below(7) as u32,
            2 => 0x2000 + rng.below(16) as u32,
            _ => rng.below(0x100) as u32,
        };
        out.push(0x83 | ((reg & 7) << 3)); // mod 10, rm 011 (rbx)
        out.extend_from_slice(&disp.to_le_bytes());
    };
    // [rbx + disp32], 16-byte aligned except now and then.
    let aligned_mem = |rng: &mut Rng, reg: u8, out: &mut Vec<u8>| {
        let disp = if rng.below(8) == 0 {
            rng.below(0x100) as u32
        } else {
            16 * rng.below(0x100 / 16) as u32
        };
        out.push(0x83 | ((reg & 7) << 3));
        out.extend_from_slice(&disp.to_le_bytes());
    };
    const ALU_RM_R: [u8; 10] = [0x01, 0x09, 0x11, 0x19, 0x21, 0x29, 0x31, 0x39, 0x85, 0x89];
    const ALU_R_RM: [u8; 9] = [0x03, 0x0b, 0x13, 0x1b, 0x23, 0x2b, 0x33, 0x3b, 0x8b];
    match rng.below(40) {
        0..=2 => {
            out.extend([rex(w, reg, rm), rng.pick(&ALU_RM_R)]);
            out.push(0xc0 | ((reg & 7) << 3) | (rm & 7));
        }
        3 => {
            out.extend([rex(w, reg, 0), rng.pick(&ALU_R_RM)]);
            mem(rng, reg, out);
        }
        4 => {
            out.extend([rex(w, reg, 0), rng.pick(&ALU_RM_R)]);
            mem(rng, reg, out);
        }
        5 => {
            // 81 /op id, 83 /op ib, c7 /0 id: register or memory.
            let (opcode, digit) = match rng.below(3) {
                0 => (0x81, rng.below(8) as u8),
                1 => (0x83, rng.below(8) as u8),
                _ => (0xc7, 0),
            };
            let in_memory = rng.below(3) == 0;
            out.extend([rex(w, 0, if in_memory { 0 } else { rm }), opcode]);
            if in_memory {
                mem(rng, digit, out);
            } else {
                out.push(0xc0 | (digit << 3) | (rm & 7));
            }
            let imm = rng.value();
            if opcode == 0x83 {
                out.push(imm as u8);
            } else {
                out.extend_from_slice(&(imm as u32).to_le_bytes());
            }
        }
        6 => {
            // inc, dec (ff /0 /1), not, neg (f7 /2 /3).
            let (opcode, digit) = rng.pick(&[(0xff, 0), (0xff, 1), (0xf7, 2), (0xf7, 3)]);
            if rng.below(3) == 0 {
                out.extend([rex(w, 0, 0), opcode]);
                mem(rng, digit, out);
            } else {
                out.extend([rex(w, 0, rm), opcode, 0xc0 | (digit << 3) | (rm & 7)]);
            }
        }
        7 => {
            // push r / pop r, push imm8, push [mem], pop [mem] (not
            // translated).
            match rng.below(5) {
                0 => out.extend([rex(0, 0, rm), 0x50 | (rm & 7)]),
                1 => out.extend([rex(0, 0, rm), 0x58 | (rm & 7)]),
                2 => out.extend([0x6a, rng.next() as u8]),
                3 => {
                    out.push(0xff);
                    mem(rng, 6, out);
                }
                _ => {
                    out.push(0x8f);
                    mem(rng, 0, out);
                }
            }
        }
        8 => {
            // lea reg, [rbx + index * scale + disp8], [index * scale +
            // disp32] without a base, or [r9 + index * scale + disp8] (a
            // base kept in the `Cpu`).
            let index = rng.pick(&[0u8, 1, 2, 5, 6, 7, 8, 9, 12, 13, 14, 15]);
            let form = rng.below(3);
            let base_r9 = u8::from(form == 2);
            out.extend([
                0x40 | (w << 3) | ((reg >> 3) << 2) | ((index >> 3) << 1) | base_r9,
                0x8d,
            ]);
            let scale = (rng.below(4) as u8) << 6;
            if form == 1 {
                out.push(0x04 | ((reg & 7) << 3));
                out.push(scale | ((index & 7) << 3) | 5);
                out.extend_from_slice(&(rng.next() as u32).to_le_bytes());
            } else {
                out.push(0x44 | ((reg & 7) << 3));
                out.push(scale | ((index & 7) << 3) | if form == 2 { 1 } else { 3 });
                out.push(rng.next() as u8);
            }
        }
        9 => {
            // movzx, movsx from a register or memory; movsxd.
            let opcode = rng.pick(&[0xb6u8, 0xb7, 0xbe, 0xbf, 0x63]);
            if opcode == 0x63 {
                out.extend([rex(1, reg, rm), 0x63, 0xc0 | ((reg & 7) << 3) | (rm & 7)]);
            } else if rng.below(2) == 0 {
                out.extend([rex(w, reg, 0), 0x0f, opcode]);
                mem(rng, reg, out);
            } else {
                out.extend([rex(w, reg, rm), 0x0f, opcode]);
                out.push(0xc0 | ((reg & 7) << 3) | (rm & 7));
            }
        }
        10 => {
            // jcc over an `inc rax`.
            out.extend([0x70 | rng.below(16) as u8, 0x03, 0x48, 0xff, 0xc0]);
        }
        11 => {
            // mov r64, imm64.
            out.extend([rex(1, 0, rm), 0xb8 | (rm & 7)]);
            out.extend_from_slice(&rng.value().to_le_bytes());
        }
        12 => {
            // call to a ret, then a jmp over it.
            out.extend([0xe8, 0x02, 0, 0, 0, 0xeb, 0x01, 0xc3]);
        }
        13 => {
            // lea rax, [rip + 3]; jmp rax or call rax, over a nop.
            let call = rng.below(2) == 0;
            out.extend([0x48, 0x8d, 0x05, 0x03, 0, 0, 0, 0xff]);
            out.extend([if call { 0xd0 } else { 0xe0 }, 0x90]);
        }
        14 => {
            // 8-bit ALU, mov and test: without REX, registers 4-6 are AH,
            // CH, DH (never BL or BH: rbx is the memory base); with REX,
            // the low bytes (never SPL).
            const OPS8: [u8; 10] = [0x00, 0x08, 0x10, 0x18, 0x20, 0x28, 0x30, 0x38, 0x84, 0x88];
            let opcode = rng.pick(&OPS8);
            if rng.below(2) == 0 {
                let r8 = rng.pick(&[0u8, 1, 2, 4, 5, 6]);
                let m8 = rng.pick(&[0u8, 1, 2, 4, 5, 6]);
                match rng.below(3) {
                    0 => out.extend([opcode, 0xc0 | (r8 << 3) | m8]),
                    1 => {
                        out.push(opcode);
                        mem(rng, r8, out);
                    }
                    _ => {
                        // 80 /op ib (or c6 /0 ib), register or memory.
                        let digit = rng.below(8) as u8;
                        let (op, digit) = if rng.below(4) == 0 {
                            (0xc6, 0)
                        } else {
                            (0x80, digit)
                        };
                        out.extend([op, 0xc0 | (digit << 3) | m8, rng.next() as u8]);
                    }
                }
            } else {
                let r8 = rng.pick(&[0u8, 1, 2, 5, 6, 7, 8, 9, 12, 15]);
                let m8 = rng.pick(&[0u8, 1, 2, 5, 6, 7, 10, 11, 13, 14]);
                out.extend([rex(0, r8, m8), opcode, 0xc0 | ((r8 & 7) << 3) | (m8 & 7)]);
            }
        }
        15 => {
            // 16-bit ALU, mov and test (66), register, memory, immediate.
            out.push(0x66);
            match rng.below(3) {
                0 => {
                    out.extend([rex(0, reg, rm), rng.pick(&ALU_RM_R)]);
                    out.push(0xc0 | ((reg & 7) << 3) | (rm & 7));
                }
                1 => {
                    out.extend([rex(0, reg, 0), rng.pick(&ALU_R_RM)]);
                    mem(rng, reg, out);
                }
                _ => {
                    let digit = rng.below(8) as u8;
                    let wide = rng.below(2) == 0;
                    out.extend([rex(0, 0, rm), if wide { 0x81 } else { 0x83 }]);
                    out.push(0xc0 | (digit << 3) | (rm & 7));
                    let imm = rng.next();
                    if wide {
                        out.extend_from_slice(&(imm as u16).to_le_bytes());
                    } else {
                        out.push(imm as u8);
                    }
                }
            }
        }
        16 => {
            // 8- and 16-bit inc, dec, not, neg (fe/f6, 66 ff/f7).
            let digit = rng.below(4) as u8;
            let narrow = rng.below(2) == 0;
            let opcode = match (narrow, digit < 2) {
                (true, true) => 0xfe,
                (true, false) => 0xf6,
                (false, true) => 0xff,
                (false, false) => 0xf7,
            };
            if !narrow {
                out.push(0x66);
            }
            if rng.below(3) == 0 {
                out.push(opcode);
                mem(rng, digit, out);
            } else {
                out.extend([rex(0, 0, rm), opcode, 0xc0 | (digit << 3) | (rm & 7)]);
            }
        }
        17 => {
            // Shifts and rotates of a register: by an immediate (c1), by
            // 1 (d1), by cl (d3); rcl/rcr and 8-bit shifts (c0) go to the
            // interpreter. The count is sometimes 0 or above the size.
            let digit = rng.pick(&[0u8, 1, 4, 5, 7, 2]);
            let r = rng.pick(&[0u8, 2, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
            match rng.below(4) {
                0 => out.extend([rex(w, 0, r), 0xd1, 0xc0 | (digit << 3) | (r & 7)]),
                1 => out.extend([rex(w, 0, r), 0xd3, 0xc0 | (digit << 3) | (r & 7)]),
                2 => {
                    let count = rng.pick(&[0u8, 1, 7, 31, 32, 33, 63, 64, 200]);
                    out.extend([rex(w, 0, r), 0xc1, 0xc0 | (digit << 3) | (r & 7), count]);
                }
                _ => out.extend([
                    rex(0, 0, r),
                    0xc0,
                    0xc0 | (digit << 3) | (r & 7),
                    rng.below(9) as u8,
                ]),
            }
        }
        18 => {
            // imul r, r/m (0f af); imul r, r/m, imm8 (6b) or imm32 (69).
            match rng.below(3) {
                0 => {
                    out.extend([
                        rex(w, reg, rm),
                        0x0f,
                        0xaf,
                        0xc0 | ((reg & 7) << 3) | (rm & 7),
                    ]);
                }
                1 => {
                    out.extend([rex(w, reg, 0), 0x0f, 0xaf]);
                    mem(rng, reg, out);
                }
                _ => {
                    let wide = rng.below(2) == 0;
                    out.extend([rex(w, reg, rm), if wide { 0x69 } else { 0x6b }]);
                    out.push(0xc0 | ((reg & 7) << 3) | (rm & 7));
                    let imm = rng.value();
                    if wide {
                        out.extend_from_slice(&(imm as u32).to_le_bytes());
                    } else {
                        out.push(imm as u8);
                    }
                }
            }
        }
        19 => {
            // cmovcc r, r/m.
            let cc = rng.below(16) as u8;
            if rng.below(3) == 0 {
                out.extend([rex(w, reg, 0), 0x0f, 0x40 | cc]);
                mem(rng, reg, out);
            } else {
                out.extend([
                    rex(w, reg, rm),
                    0x0f,
                    0x40 | cc,
                    0xc0 | ((reg & 7) << 3) | (rm & 7),
                ]);
            }
        }
        20 => {
            // setcc r/m8: AH-DH without REX, low bytes with it, memory.
            let cc = rng.below(16) as u8;
            match rng.below(3) {
                0 => out.extend([0x0f, 0x90 | cc, 0xc0 | rng.pick(&[0u8, 1, 2, 4, 5, 6])]),
                1 => out.extend([rex(0, 0, rm), 0x0f, 0x90 | cc, 0xc0 | (rm & 7)]),
                _ => {
                    out.extend([0x0f, 0x90 | cc]);
                    mem(rng, 0, out);
                }
            }
        }
        21 => {
            // cbw/cwde/cdqe (98), cwd/cdq/cqo (99), movzx/movsx from AH-DH.
            match rng.below(3) {
                0 => out.extend([rng.pick(&[0x66u8, 0x40, 0x48]), rng.pick(&[0x98u8, 0x99])]),
                1 => out.extend([0x98 | rng.below(2) as u8]),
                _ => {
                    let opcode = rng.pick(&[0xb6u8, 0xbe]);
                    let r = rng.pick(&[0u8, 1, 2, 5, 6]);
                    out.extend([0x0f, opcode, 0xc0 | (r << 3) | (4 + rng.below(3) as u8)]);
                }
            }
        }
        22 | 23 => {
            // movaps/movups (0f 28/10, 29/11), movdqa/movdqu (66/f3 0f
            // 6f/7f), between registers or with memory (aligned or not).
            let x = rng.below(16) as u8;
            let y = rng.below(16) as u8;
            let (prefix, load, store) = rng.pick(&[
                (None, 0x28u8, 0x29u8),
                (None, 0x10, 0x11),
                (Some(0x66u8), 0x6f, 0x7f),
                (Some(0xf3), 0x6f, 0x7f),
            ]);
            out.extend(prefix);
            match rng.below(3) {
                0 => out.extend([rex(0, x, y), 0x0f, load, 0xc0 | ((x & 7) << 3) | (y & 7)]),
                1 => {
                    out.extend([rex(0, x, 0), 0x0f, load]);
                    mem(rng, x, out);
                }
                _ => {
                    out.extend([rex(0, x, 0), 0x0f, store]);
                    mem(rng, x, out);
                }
            }
        }
        24 => {
            // movd/movq: 66 0f 6e (xmm <- r/m), 66 0f 7e (r/m <- xmm),
            // f3 0f 7e (movq xmm <- xmm/m64), 66 0f d6 (movq xmm/m64 <- xmm).
            let x = rng.below(16) as u8;
            let (prefix, opcode, w) = rng.pick(&[
                (0x66u8, 0x6eu8, w),
                (0x66, 0x7e, w),
                (0xf3, 0x7e, 0),
                (0x66, 0xd6, 0),
            ]);
            out.push(prefix);
            if rng.below(3) == 0 {
                out.extend([rex(w, x, 0), 0x0f, opcode]);
                mem(rng, x, out);
            } else {
                let other = if opcode == 0x6e || (opcode == 0x7e && prefix == 0x66) {
                    rm
                } else {
                    rng.below(16) as u8
                };
                out.extend([
                    rex(w, x, other),
                    0x0f,
                    opcode,
                    0xc0 | ((x & 7) << 3) | (other & 7),
                ]);
            }
        }
        26..=28 => {
            // SSE2 integer operations, xmm or memory source: lane-wise
            // arithmetic and compares, logic, unpack, pack, pmuludq.
            const OPS: [u8; 33] = [
                0xfc, 0xfd, 0xfe, 0xd4, 0xf8, 0xf9, 0xfa, 0xfb, 0x74, 0x75, 0x76, 0x64, 0x65, 0x66,
                0xda, 0xde, 0xea, 0xee, 0xdb, 0xdf, 0xeb, 0xef, 0x60, 0x61, 0x62, 0x6c, 0x68, 0x69,
                0x6a, 0x6d, 0x63, 0x6b, 0x67,
            ];
            let x = rng.below(16) as u8;
            let y = rng.below(16) as u8;
            let opcode = if rng.below(8) == 0 {
                0xf4
            } else {
                rng.pick(&OPS)
            };
            // Also the unprefixed float logic (andps..xorps) and unpacks.
            let (prefix, opcode) = if rng.below(8) == 0 {
                (None, rng.pick(&[0x54u8, 0x55, 0x56, 0x57, 0x14, 0x15]))
            } else {
                (Some(0x66u8), opcode)
            };
            out.extend(prefix);
            if rng.below(3) == 0 {
                out.extend([rex(0, x, 0), 0x0f, opcode]);
                aligned_mem(rng, x, out);
            } else {
                out.extend([rex(0, x, y), 0x0f, opcode, 0xc0 | ((x & 7) << 3) | (y & 7)]);
            }
        }
        29 => {
            // Shuffles with an immediate: pshufd (66), pshuflw (f2),
            // pshufhw (f3), shufps, shufpd (66 0f c6).
            let x = rng.below(16) as u8;
            let y = rng.below(16) as u8;
            let (prefix, opcode) = rng.pick(&[
                (Some(0x66u8), 0x70u8),
                (Some(0xf2), 0x70),
                (Some(0xf3), 0x70),
                (None, 0xc6),
                (Some(0x66), 0xc6),
            ]);
            out.extend(prefix);
            if rng.below(3) == 0 {
                out.extend([rex(0, x, 0), 0x0f, opcode]);
                aligned_mem(rng, x, out);
            } else {
                out.extend([rex(0, x, y), 0x0f, opcode, 0xc0 | ((x & 7) << 3) | (y & 7)]);
            }
            out.push(rng.next() as u8);
        }
        30 => {
            // Shifts by an immediate: psrlw/psraw/psllw (71), d (72),
            // q and the byte shifts psrldq/pslldq (73).
            let x = rng.below(16) as u8;
            let (opcode, digit) = rng.pick(&[
                (0x71u8, 2u8),
                (0x71, 4),
                (0x71, 6),
                (0x72, 2),
                (0x72, 4),
                (0x72, 6),
                (0x73, 2),
                (0x73, 6),
                (0x73, 3),
                (0x73, 7),
            ]);
            let count = rng.pick(&[0u8, 1, 3, 7, 8, 15, 16, 17, 31, 32, 63, 64, 200]);
            out.extend([
                0x66,
                rex(0, 0, x),
                0x0f,
                opcode,
                0xc0 | (digit << 3) | (x & 7),
                count,
            ]);
        }
        25 => {
            // A mov to memory, then a read back.
            out.extend([rex(w, reg, 0), 0x89]);
            mem(rng, reg, out);
            out.extend([rex(w, rm, 0), 0x8b]);
            mem(rng, rm, out);
        }
        32 => {
            // 8- and 16-bit shifts and rotates: register (by immediate, 1
            // or cl) or memory (by immediate or 1).
            let digit = rng.pick(&[0u8, 1, 4, 5, 7, 2]);
            let wide16 = rng.below(2) == 0;
            if wide16 {
                out.push(0x66);
            }
            let (by_imm, by_one, by_cl) = if wide16 {
                (0xc1u8, 0xd1u8, 0xd3u8)
            } else {
                (0xc0, 0xd0, 0xd2)
            };
            let count = rng.pick(&[0u8, 1, 3, 7, 8, 9, 15, 16, 17, 31, 33]);
            match rng.below(5) {
                0 => out.extend([rex(0, 0, rm), by_imm, 0xc0 | (digit << 3) | (rm & 7), count]),
                1 => out.extend([rex(0, 0, rm), by_one, 0xc0 | (digit << 3) | (rm & 7)]),
                2 => out.extend([rex(0, 0, rm), by_cl, 0xc0 | (digit << 3) | (rm & 7)]),
                3 => {
                    out.push(by_imm);
                    mem(rng, digit, out);
                    out.push(count);
                }
                _ => {
                    out.push(by_one);
                    mem(rng, digit, out);
                }
            }
        }
        33 => {
            // div/idiv r/m32 or r/m64 (f7 /6, /7), with rdx set first:
            // zero, the sign of rax, or anything.
            match rng.below(3) {
                0 => out.extend([0x31, 0xd2]), // xor edx, edx
                1 => out.extend([0x48, 0x99]), // cqo
                _ => {}
            }
            let digit = 6 + rng.below(2) as u8;
            let divisor = rng.pick(&[1u8, 2, 5, 6, 8, 9, 10, 12, 13, 14, 15]);
            if rng.below(4) == 0 {
                out.extend([rex(w, 0, 0), 0xf7]);
                mem(rng, digit, out);
            } else {
                out.extend([
                    rex(w, 0, divisor),
                    0xf7,
                    0xc0 | (digit << 3) | (divisor & 7),
                ]);
            }
        }
        34 => {
            // bsf/bsr (0f bc/bd), sometimes of a zeroed register; bt,
            // bts, btr, btc by register (0f a3/ab/b3/bb) or immediate
            // (0f ba /4-7), on a register or memory; bswap (0f c8+r).
            match rng.below(4) {
                0 => {
                    if rng.below(3) == 0 {
                        out.extend([rex(1, rm, rm), 0x31, 0xc0 | ((rm & 7) << 3) | (rm & 7)]);
                    }
                    let opcode = rng.pick(&[0xbcu8, 0xbd]);
                    out.extend([
                        rex(w, reg, rm),
                        0x0f,
                        opcode,
                        0xc0 | ((reg & 7) << 3) | (rm & 7),
                    ]);
                }
                1 => {
                    let opcode = rng.pick(&[0xa3u8, 0xab, 0xb3, 0xbb]);
                    out.extend([
                        rex(w, reg, rm),
                        0x0f,
                        opcode,
                        0xc0 | ((reg & 7) << 3) | (rm & 7),
                    ]);
                }
                2 => {
                    let digit = 4 + rng.below(4) as u8;
                    if rng.below(2) == 0 {
                        out.extend([rex(w, 0, 0), 0x0f, 0xba]);
                        mem(rng, digit, out);
                    } else {
                        out.extend([rex(w, 0, rm), 0x0f, 0xba, 0xc0 | (digit << 3) | (rm & 7)]);
                    }
                    out.push(rng.next() as u8);
                }
                _ => out.extend([rex(w, 0, rm), 0x0f, 0xc8 | (rm & 7)]),
            }
        }
        38 => {
            // MMX: the moves (movd/movq with r/m, mm and m64, movntq,
            // movq2dq, movdq2q), lane operations, logic, unpacks, pmuludq,
            // packs and high unpacks (interpreted), shifts by an
            // immediate, pinsrw, pshufw and emms.
            const OPS: [u8; 27] = [
                0xfc, 0xfd, 0xfe, 0xd4, 0xf8, 0xf9, 0xfa, 0xfb, 0x74, 0x75, 0x76, 0x64, 0x65, 0x66,
                0xda, 0xde, 0xea, 0xee, 0xdb, 0xdf, 0xeb, 0xef, 0x60, 0x61, 0x62, 0xf4, 0x68,
            ];
            let x = rng.below(8) as u8;
            let y = rng.below(8) as u8;
            let modrm = |x: u8, y: u8| 0xc0 | (x << 3) | y;
            match rng.below(9) {
                0 | 1 => {
                    let opcode = if rng.below(8) == 0 {
                        rng.pick(&[0x63u8, 0x6b, 0x69])
                    } else {
                        rng.pick(&OPS)
                    };
                    out.extend([0x0f, opcode]);
                    if rng.below(3) == 0 {
                        mem(rng, x, out);
                    } else {
                        out.push(modrm(x, y));
                    }
                }
                2 => {
                    // movd/movq mm, r/m (6e) and r/m, mm (7e).
                    let opcode = rng.pick(&[0x6eu8, 0x7e]);
                    if rng.below(3) == 0 {
                        out.extend([0x0f, opcode]);
                        mem(rng, x, out);
                    } else {
                        out.extend([rex(w, 0, rm), 0x0f, opcode, modrm(x, rm & 7)]);
                    }
                }
                3 => {
                    // movq mm, mm/m64 (6f); mm/m64, mm (7f); movntq (e7).
                    let opcode = rng.pick(&[0x6fu8, 0x7f, 0xe7]);
                    out.extend([0x0f, opcode]);
                    if opcode == 0xe7 || rng.below(2) == 0 {
                        mem(rng, x, out);
                    } else {
                        out.push(modrm(x, y));
                    }
                }
                4 => {
                    // movq2dq xmm, mm (f3) and movdq2q mm, xmm (f2).
                    let prefix = rng.pick(&[0xf3u8, 0xf2]);
                    let xmm = rng.below(16) as u8;
                    if prefix == 0xf3 {
                        out.extend([prefix, rex(0, xmm, 0), 0x0f, 0xd6, modrm(xmm & 7, y)]);
                    } else {
                        out.extend([prefix, rex(0, 0, xmm), 0x0f, 0xd6, modrm(x, xmm & 7)]);
                    }
                }
                5 => {
                    let (opcode, digit) = rng.pick(&[
                        (0x71u8, 2u8),
                        (0x71, 4),
                        (0x71, 6),
                        (0x72, 2),
                        (0x72, 4),
                        (0x72, 6),
                        (0x73, 2),
                        (0x73, 6),
                    ]);
                    let count = rng.pick(&[0u8, 1, 3, 7, 8, 15, 16, 17, 31, 32, 63, 64, 200]);
                    out.extend([0x0f, opcode, 0xc0 | (digit << 3) | y, count]);
                }
                6 => {
                    // pinsrw mm, r32/m16, imm.
                    if rng.below(2) == 0 {
                        out.extend([rex(0, 0, rm), 0x0f, 0xc4, modrm(x, rm & 7)]);
                    } else {
                        out.extend([0x0f, 0xc4]);
                        mem(rng, x, out);
                    }
                    out.push(rng.next() as u8);
                }
                7 => {
                    // pshufw mm, mm/m64, imm.
                    out.extend([0x0f, 0x70]);
                    if rng.below(3) == 0 {
                        mem(rng, x, out);
                    } else {
                        out.push(modrm(x, y));
                    }
                    out.push(rng.next() as u8);
                }
                _ => out.extend([0x0f, 0x77]),
            }
        }
        39 => {
            // pshufb xmm, xmm/m128 (66 0f 38 00); palignr xmm, xmm/m128,
            // imm (66 0f 3a 0f).
            let x = rng.below(16) as u8;
            let y = rng.below(16) as u8;
            let palignr = rng.below(2) == 0;
            let opcode: &[u8] = if palignr {
                &[0x3a, 0x0f]
            } else {
                &[0x38, 0x00]
            };
            if rng.below(3) == 0 {
                out.extend([0x66, rex(0, x, 0), 0x0f]);
                out.extend(opcode);
                aligned_mem(rng, x, out);
            } else {
                out.extend([0x66, rex(0, x, y), 0x0f]);
                out.extend(opcode);
                out.push(0xc0 | ((x & 7) << 3) | (y & 7));
            }
            if palignr {
                out.push(rng.pick(&[0u8, 1, 4, 8, 15, 16, 17, 24, 31, 32, 200]));
            }
        }
        35 => {
            // xchg (87 /r, 90+r), cmpxchg (0f b1), xadd (0f c1), with or
            // without lock (f0) on memory; lock ALU, inc/dec/not/neg on
            // memory at 8 to 64 bits; mul/imul r/m (f7 /4 /5). Memory
            // operands are often aligned: in concurrent mode only those
            // take the LSE atomics (ADR-0023).
            let lock = rng.below(2) == 0;
            let any_mem = |rng: &mut Rng, reg: u8, out: &mut Vec<u8>| {
                if rng.below(2) == 0 {
                    aligned_mem(rng, reg, out)
                } else {
                    mem(rng, reg, out)
                }
            };
            match rng.below(6) {
                0 => {
                    if rng.below(2) == 0 {
                        out.extend([rex(w, reg, rm), 0x87, 0xc0 | ((reg & 7) << 3) | (rm & 7)]);
                    } else {
                        out.extend([rex(w, 0, rm), 0x90 | (rm & 7)]);
                    }
                }
                1 => {
                    if lock {
                        out.push(0xf0);
                    }
                    out.extend([rex(w, reg, 0), 0x87]);
                    any_mem(rng, reg, out);
                }
                2 | 3 => {
                    // Sometimes rax equals the destination: mov rax, rm
                    // first (register form).
                    let opcode = if rng.below(2) == 0 { 0xb1u8 } else { 0xc1 };
                    if rng.below(2) == 0 {
                        if lock {
                            out.push(0xf0);
                        }
                        out.extend([rex(w, reg, 0), 0x0f, opcode]);
                        any_mem(rng, reg, out);
                    } else {
                        if rng.below(2) == 0 {
                            out.extend([rex(1, rm, 0), 0x89, 0xc0 | ((rm & 7) << 3)]);
                        }
                        out.extend([
                            rex(w, reg, rm),
                            0x0f,
                            opcode,
                            0xc0 | ((reg & 7) << 3) | (rm & 7),
                        ]);
                    }
                }
                4 => {
                    // 0: 8 bits, 1: 16 bits, otherwise 32 or 64.
                    let narrow = rng.below(4);
                    if narrow == 1 {
                        out.push(0x66);
                    }
                    out.push(0xf0);
                    let (w, byte) = if narrow < 2 {
                        (0, narrow == 0)
                    } else {
                        (w, false)
                    };
                    let wide = u8::from(!byte);
                    match rng.below(3) {
                        0 => {
                            // add, or, adc, sbb, and, sub, xor r/m, r.
                            let op = rng.pick(&[0x00u8, 0x08, 0x10, 0x18, 0x20, 0x28, 0x30]);
                            out.extend([rex(w, reg, 0), op | wide]);
                            any_mem(rng, reg, out);
                        }
                        1 => {
                            // 80 /op ib, 83 /op ib (not cmp, /7).
                            let digit = rng.below(7) as u8;
                            out.extend([rex(w, 0, 0), if byte { 0x80 } else { 0x83 }]);
                            any_mem(rng, digit, out);
                            out.push(rng.next() as u8);
                        }
                        _ => {
                            // inc, dec (fe/ff /0 /1), not, neg (f6/f7 /2 /3).
                            let digit = rng.below(4) as u8;
                            let opcode = if digit < 2 { 0xfe } else { 0xf6 };
                            out.extend([rex(w, 0, 0), opcode | wide]);
                            any_mem(rng, digit, out);
                        }
                    }
                }
                _ => {
                    let digit = 4 + rng.below(2) as u8;
                    if rng.below(3) == 0 {
                        out.extend([rex(w, 0, 0), 0xf7]);
                        mem(rng, digit, out);
                    } else {
                        out.extend([rex(w, 0, rm), 0xf7, 0xc0 | (digit << 3) | (rm & 7)]);
                    }
                }
            }
        }
        36 | 37 => {
            // Scalar floating point: first a special value into the low
            // lane of an xmm (mov rax, imm64; movq xmm, rax), then an
            // operation on it.
            const F32: [u32; 14] = [
                0,
                0x8000_0000,
                0x3f80_0000,
                0xbf80_0000,
                0x7f80_0000,
                0xff80_0000,
                0x7fc0_0000,
                0xffc0_0001,
                0x7f80_0001,
                0x0000_0001,
                0x4f00_0000,
                0xcf00_0000,
                0x5f00_0000,
                0x7f7f_ffff,
            ];
            const F64: [u64; 14] = [
                0,
                0x8000_0000_0000_0000,
                0x3ff0_0000_0000_0000,
                0xbff0_0000_0000_0000,
                0x7ff0_0000_0000_0000,
                0xfff0_0000_0000_0000,
                0x7ff8_0000_0000_0000,
                0xfff8_0000_0000_0001,
                0x7ff0_0000_0000_0001,
                0x0000_0000_0000_0001,
                0x41e0_0000_0000_0000,
                0xc1e0_0000_0000_0000,
                0x43e0_0000_0000_0000,
                0x7fef_ffff_ffff_ffff,
            ];
            let double = rng.below(2) == 0;
            let x = rng.below(16) as u8;
            let y = rng.below(16) as u8;
            if rng.below(4) != 0 {
                let value = if double {
                    rng.pick(&F64)
                } else {
                    u64::from(rng.pick(&F32))
                };
                out.extend([0x48, 0xb8]);
                out.extend_from_slice(&value.to_le_bytes());
                // movq xmm(y), rax
                out.extend([0x66, rex(1, y, 0), 0x0f, 0x6e, 0xc0 | ((y & 7) << 3)]);
            }
            let prefix = if double { 0xf2u8 } else { 0xf3 };
            let modrm_rr = 0xc0 | ((x & 7) << 3) | (y & 7);
            match rng.below(7) {
                0 | 1 => {
                    // add, mul, sub, min, div, max, sqrt
                    let opcode = rng.pick(&[0x58u8, 0x59, 0x5c, 0x5d, 0x5e, 0x5f, 0x51]);
                    out.push(prefix);
                    if rng.below(4) == 0 {
                        out.extend([rex(0, x, 0), 0x0f, opcode]);
                        mem(rng, x, out);
                    } else {
                        out.extend([rex(0, x, y), 0x0f, opcode, modrm_rr]);
                    }
                }
                2 => {
                    // comiss/ucomiss (66 for sd), then setcc al or a jcc.
                    if double {
                        out.push(0x66);
                    }
                    out.extend([rex(0, x, y), 0x0f, rng.pick(&[0x2eu8, 0x2f]), modrm_rr]);
                    if rng.below(2) == 0 {
                        out.extend([0x0f, 0x90 | rng.below(16) as u8, 0xc0]);
                    } else {
                        out.extend([0x70 | rng.below(16) as u8, 0x03, 0x48, 0xff, 0xc2]);
                    }
                }
                3 => {
                    // cmpss/cmpsd with a predicate
                    out.extend([
                        prefix,
                        rex(0, x, y),
                        0x0f,
                        0xc2,
                        modrm_rr,
                        rng.below(8) as u8,
                    ]);
                }
                4 => {
                    // cvtsi2ss/sd from a register or memory, 32 or 64 bits
                    out.push(prefix);
                    if rng.below(3) == 0 {
                        out.extend([rex(w, x, 0), 0x0f, 0x2a]);
                        mem(rng, x, out);
                    } else {
                        out.extend([rex(w, x, rm), 0x0f, 0x2a, 0xc0 | ((x & 7) << 3) | (rm & 7)]);
                    }
                }
                5 => {
                    // cvt(t)ss2si / cvt(t)sd2si into a register; or
                    // cvtss2sd / cvtsd2ss
                    if rng.below(3) == 0 {
                        out.extend([prefix, rex(0, x, y), 0x0f, 0x5a, modrm_rr]);
                    } else {
                        let opcode = rng.pick(&[0x2cu8, 0x2d]);
                        out.extend([
                            prefix,
                            rex(w, reg, y),
                            0x0f,
                            opcode,
                            0xc0 | ((reg & 7) << 3) | (y & 7),
                        ]);
                    }
                }
                _ => {
                    // movmskps / movmskpd
                    if double {
                        out.push(0x66);
                    }
                    out.extend([
                        rex(0, reg, y),
                        0x0f,
                        0x50,
                        0xc0 | ((reg & 7) << 3) | (y & 7),
                    ]);
                }
            }
        }
        _ => {
            // pmovmskb, pextrw, pinsrw; movhps/movlps/movhlps/movlhps;
            // movss/movsd.
            let x = rng.below(16) as u8;
            let y = rng.below(16) as u8;
            match rng.below(6) {
                0 => out.extend([
                    0x66,
                    rex(0, reg, x),
                    0x0f,
                    0xd7,
                    0xc0 | ((reg & 7) << 3) | (x & 7),
                ]),
                1 => out.extend([
                    0x66,
                    rex(0, reg, x),
                    0x0f,
                    0xc5,
                    0xc0 | ((reg & 7) << 3) | (x & 7),
                    rng.next() as u8,
                ]),
                2 => {
                    out.extend([0x66, rex(0, x, rm), 0x0f, 0xc4]);
                    if rng.below(2) == 0 {
                        out.push(0xc0 | ((x & 7) << 3) | (rm & 7));
                    } else {
                        out.pop();
                        out.pop();
                        out.pop();
                        out.extend([rex(0, x, 0), 0x0f, 0xc4]);
                        mem(rng, x, out);
                    }
                    out.push(rng.next() as u8);
                }
                3 => {
                    // 0f 12/16 between registers (movhlps/movlhps) or as
                    // loads, 0f 13/17 as stores.
                    let opcode = rng.pick(&[0x12u8, 0x16, 0x13, 0x17]);
                    if opcode & 1 == 0 && rng.below(2) == 0 {
                        out.extend([rex(0, x, y), 0x0f, opcode, 0xc0 | ((x & 7) << 3) | (y & 7)]);
                    } else {
                        out.extend([rex(0, x, 0), 0x0f, opcode]);
                        mem(rng, x, out);
                    }
                }
                _ => {
                    // movss (f3) / movsd (f2): 10 load or register move,
                    // 11 store.
                    out.push(rng.pick(&[0xf2u8, 0xf3]));
                    let opcode = rng.pick(&[0x10u8, 0x11]);
                    if opcode == 0x10 && rng.below(2) == 0 {
                        out.extend([rex(0, x, y), 0x0f, 0x10, 0xc0 | ((x & 7) << 3) | (y & 7)]);
                    } else {
                        out.extend([rex(0, x, 0), 0x0f, opcode]);
                        mem(rng, x, out);
                    }
                }
            }
        }
    }
}

/// The same random bytes run as 32-bit code (ADR-0029): REX prefixes
/// become inc and dec, [rbx + disp] becomes [ebx + disp], and the stack and
/// the branches are 32-bit.
#[test]
fn random_code_matches_the_interpreter_in_compatibility_mode() {
    let guest = Guest::new_low();
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for case in 0..3000 {
        let mut code = Vec::new();
        for _ in 0..1 + rng.below(24) {
            random_instruction(&mut rng, &mut code);
        }
        let mut cpu = guest.cpu(&mut rng);
        cpu.mode = Mode::Compat32;
        for reg in cpu.regs.iter_mut() {
            *reg &= 0xffff_ffff;
        }
        guest.compare(&code, &cpu, case + 1);
    }
}

#[test]
fn random_code_matches_the_interpreter() {
    let guest = Guest::new();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for case in 0..3000 {
        let mut code = Vec::new();
        for _ in 0..1 + rng.below(24) {
            random_instruction(&mut rng, &mut code);
        }
        let cpu = guest.cpu(&mut rng);
        guest.compare(&code, &cpu, case + 1);
    }
}

/// ADR-0023: in concurrent mode the translations order their accesses
/// (LDAPR, STLR, barriers, unaligned addresses out of line); they must
/// still compute what the interpreter computes.
#[test]
fn random_code_matches_the_interpreter_in_concurrent_mode() {
    let guest = Guest::new();
    guest.mem.enter_concurrent_mode();
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for case in 0..1500 {
        let mut code = Vec::new();
        for _ in 0..1 + rng.below(24) {
            random_instruction(&mut rng, &mut code);
        }
        let cpu = guest.cpu(&mut rng);
        guest.compare(&code, &cpu, case + 1);
    }
}

/// ADR-0046: the random programs with host protection, where the host
/// checks the JIT's reads and a fault resumes at the slow path; plain and
/// in concurrent mode (unaligned ordered loads out of line). The guest's
/// region must be free, so the test runs again in a process of its own.
#[test]
fn random_code_matches_the_interpreter_with_host_protection() {
    const CHILD: &str = "ARMOR_HOST_PROTECTION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::random_code_matches_the_interpreter_with_host_protection",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success(), "the test in its own process failed");
        return;
    }
    let mut mem = AddressSpace::new();
    let protected = mem.enable_host_protection();
    assert_eq!(protected, armor_mem::host_page_size() == 4096);
    let guest = Guest::in_space(mem, 0x30_0000_0000);
    let mut rng = Rng(0x6a09_e667_f3bc_c908);
    for case in 0..6000 {
        if case == 3000 {
            guest.mem.enter_concurrent_mode();
        }
        let mut code = Vec::new();
        for _ in 0..1 + rng.below(24) {
            random_instruction(&mut rng, &mut code);
        }
        let cpu = guest.cpu(&mut rng);
        guest.compare(&code, &cpu, case + 1);
    }
    // The stack with writeback (`push`, `pop`, `ret`): faults into the
    // guard page (no access) and the read-only page after the data, and a
    // misaligned rsp; a fault leaves rsp as it was.
    let guard = guest.data + DATA_SIZE;
    let read_only = guard + 0x1000;
    for (rsp, ops) in [
        (read_only + 0x10, &[0x50u8][..]),
        (guard + 0x10, &[0x50]),
        (guard + 0x10, &[0x5b]),
        (guard + 0x10, &[0xc3]),
        (read_only + 0x10, &[0x5b, 0x5b, 0x50]),
        (guest.data + 0x101, &[0x50, 0x5b]),
        (guest.data + 0x1004, &[0x50, 0x50, 0x5b]),
        (guest.data + 0x1ffc, &[0x5b]),
        (guest.data + 0x2000 - 4, &[0x50]),
        (guard + 4, &[0x50]),
    ] {
        let mut code = vec![0x48, 0xbc];
        code.extend_from_slice(&rsp.to_le_bytes());
        code.extend_from_slice(ops);
        let cpu = guest.cpu(&mut rng);
        guest.compare(&code, &cpu, 7);
    }
    // An access near one whose region test passed needs none, until its
    // base changes: here to host memory above the region (armor's own),
    // which the next access must not read.
    let host = Box::new([0x1234_5678u64; 4]);
    let host_address = host.as_ptr() as u64;
    let end = guest.mem.host_protection().expect("host protection");
    if host_address >= end {
        for second in [
            &[0x8b, 0x43, 0x08][..],   // mov eax, [rbx+8]
            &[0x89, 0x43, 0x08],       // mov [rbx+8], eax
            &[0x48, 0x03, 0x43, 0x10], // add rax, [rbx+16]
            &[0x83, 0x43, 0x08, 0x01], // add dword [rbx+8], 1
        ] {
            let mut code = vec![0x8b, 0x03, 0x48, 0x89, 0xcb]; // mov eax, [rbx]; mov rbx, rcx
            code.extend_from_slice(second);
            let mut cpu = guest.cpu(&mut rng);
            cpu.regs[3] = guest.data;
            cpu.regs[1] = host_address;
            guest.compare(&code, &cpu, 8);
        }
    }
    assert_eq!(host[1], 0x1234_5678, "the guest wrote armor's memory");
}

/// The same in concurrent mode with ARMv8.0 alone (ADR-0032): LDAR for
/// LDAPR, exclusive loops for the LSE atomics (the generator's locked
/// instructions on aligned memory take them).
#[test]
fn random_code_matches_the_interpreter_on_armv8_0() {
    let mut guest = Guest::new();
    guest.features = Some(crate::translator::HostFeatures::BASELINE);
    guest.mem.enter_concurrent_mode();
    let mut rng = Rng(0x7f4a_7c15_9e37_79b9);
    for case in 0..1500 {
        let mut code = Vec::new();
        for _ in 0..1 + rng.below(24) {
            random_instruction(&mut rng, &mut code);
        }
        let cpu = guest.cpu(&mut rng);
        guest.compare(&code, &cpu, case + 1);
    }
}

/// With host protection, an access near one whose region test passed,
/// with the same base, needs none (`Translator::checked`), also after the
/// base moved by a constant (`push`, `pop`, `add`, `sub`, `lea`); far from
/// it, or after another write of the base, it does. Counts the tests (an
/// `ands` with the region's bits, into the zero register) on the main path.
#[test]
fn region_tests_cover_nearby_accesses() {
    use crate::translator::{BlockEnd, GuestInstruction, Options, translate};
    let tests = |code: &[&[u8]]| {
        let block: Vec<GuestInstruction> = code
            .iter()
            .scan(0x1000u64, |rip, bytes| {
                let insn = armor_x86::decode(bytes).unwrap();
                let g = GuestInstruction { rip: *rip, insn };
                *rip += insn.len as u64;
                Some(g)
            })
            .collect();
        let words = translate(
            &block,
            BlockEnd::Continue(0x2000),
            Options {
                features: crate::translator::HostFeatures {
                    lse: true,
                    rcpc: true,
                },
                chain: false,
                concurrent: false,
                compat: false,
                host_reads: Some(38),
                page_table: true,
                entry_kind: 0,
            },
        )
        .unwrap();
        // ANDS (immediate, 64-bit, N = 1) into xzr, on the main path: up
        // to the first unconditional branch (the exit).
        words
            .iter()
            .take_while(|&&w| w & 0xfc00_0000 != 0x1400_0000)
            .filter(|&&w| w >> 23 == 0b1_1110_0100 && w & 0x1f == 31 && w >> 22 & 1 == 1)
            .count()
    };
    let load = [0x8b, 0x03]; // mov eax, [rbx]
    let load_8 = [0x8b, 0x43, 0x08]; // mov eax, [rbx+8]
    let store_4 = [0x89, 0x4b, 0x04]; // mov [rbx+4], ecx
    let store_12 = [0x89, 0x4b, 0x0c]; // mov [rbx+12], ecx
    // Near: one test. A read tests no alignment, so a write after it
    // tests again; a write's test covers reads and aligned writes.
    assert_eq!(tests(&[&load, &load_8]), 1);
    assert_eq!(tests(&[&load, &load_8, &store_4]), 2);
    assert_eq!(tests(&[&store_4, &load, &load_8, &store_12]), 1);
    // rbx written: the test again.
    assert_eq!(tests(&[&load, &[0x48, 0x89, 0xcb], &load_8]), 2);
    // add rbx, 0x9000, then [rbx+0x9000]: 0x12000 from the first.
    assert_eq!(
        tests(&[
            &load,
            &[0x48, 0x81, 0xc3, 0x00, 0x90, 0, 0],
            &[0x8b, 0x83, 0x00, 0x90, 0, 0]
        ]),
        2
    );
    // sub rbx, 0x9000, then [rbx+0x9000]: the first address.
    assert_eq!(
        tests(&[
            &load,
            &[0x48, 0x81, 0xeb, 0x00, 0x90, 0, 0],
            &[0x8b, 0x83, 0x00, 0x90, 0, 0]
        ]),
        1
    );
    // lea rbx, [rbx+8], then [rbx-8].
    assert_eq!(
        tests(&[&load, &[0x48, 0x8d, 0x5b, 0x08], &[0x8b, 0x43, 0xf8]]),
        1
    );
    // Pushes, then a load from the slots, and pops: one test of rsp.
    assert_eq!(
        tests(&[
            &[0x50],
            &[0x53],
            &[0x51],
            &[0x48, 0x8b, 0x44, 0x24, 0x10],
            &[0x59],
            &[0x5b]
        ]),
        1
    );
    // After a push, the rsp tested is 8 above rsp: [rsp+0xfff8] is
    // within the guard's distance of it, [rsp+0x10008] is not.
    assert_eq!(tests(&[&[0x50], &[0x8b, 0x84, 0x24, 0xf8, 0xff, 0, 0]]), 1);
    assert_eq!(tests(&[&[0x50], &[0x8b, 0x84, 0x24, 0x08, 0, 1, 0]]), 2);
    // A RIP-relative address is known: no test, unless a write to it is
    // misaligned.
    // (rip after the instruction is 0x1006: 0x1006 + 0xfa = 0x1100.)
    assert_eq!(tests(&[&[0x8b, 0x05, 0x01, 0x01, 0, 0]]), 0);
    assert_eq!(tests(&[&[0x89, 0x05, 0xfa, 0x00, 0, 0]]), 0);
    assert_eq!(tests(&[&[0x89, 0x05, 0xfb, 0x00, 0, 0]]), 1);
    // A store 4 bytes off the tested alignment of 8: tested again.
    assert_eq!(tests(&[&[0x50], &[0x89, 0x4c, 0x24, 0xfc]]), 1);
    assert_eq!(tests(&[&[0x50], &[0x48, 0x89, 0x4c, 0x24, 0xfc]]), 2);
}

/// The concurrent-mode translation of `mov rax, [rbx]; mov [rbx], rcx`
/// uses LDAPR and STLR, with the alignment test before each.
#[test]
fn concurrent_mode_emits_ordered_accesses() {
    use crate::translator::{BlockEnd, GuestInstruction, Options, translate};
    let block: Vec<GuestInstruction> = [&[0x48u8, 0x8b, 0x03][..], &[0x48, 0x89, 0x0b]]
        .iter()
        .scan(0x1000u64, |rip, bytes| {
            let insn = armor_x86::decode(bytes).unwrap();
            let g = GuestInstruction { rip: *rip, insn };
            *rip += insn.len as u64;
            Some(g)
        })
        .collect();
    let words = |concurrent| {
        translate(
            &block,
            BlockEnd::Continue(0x1006),
            Options {
                features: crate::translator::HostFeatures {
                    lse: true,
                    rcpc: true,
                },
                chain: false,
                concurrent,
                compat: false,
                host_reads: None,
                page_table: true,
                entry_kind: 0,
            },
        )
        .unwrap()
    };
    let is_ldapr_x = |w: &u32| w & 0xFFFF_FC00 == 0xF8BF_C000;
    let is_stlr_x = |w: &u32| w & 0xFFFF_FC00 == 0xC89F_FC00;
    let plain = words(false);
    assert!(!plain.iter().any(is_ldapr_x) && !plain.iter().any(is_stlr_x));
    let ordered = words(true);
    assert_eq!(ordered.iter().filter(|w| is_ldapr_x(w)).count(), 1);
    assert_eq!(ordered.iter().filter(|w| is_stlr_x(w)).count(), 1);
    // Out of line, for unaligned addresses: a load barrier and a full one.
    assert!(ordered.contains(&0xD503_39BF) && ordered.contains(&0xD503_3BBF));
}

/// ADR-0023 step 4: in concurrent mode `lock add`, `xchg` with memory,
/// `lock xadd` and `lock cmpxchg` become LDADDAL, SWPAL and CASAL; outside
/// it they stay plain read-modify-writes. `lock adc` has no LSE form and
/// is left to the interpreter in concurrent mode only.
#[test]
fn concurrent_mode_emits_lse_atomics() {
    use crate::translator::{
        BlockEnd, GuestInstruction, Options, supported, supported_concurrent, translate,
    };
    let words = |bytes: &[u8], concurrent| {
        let insn = armor_x86::decode(bytes).unwrap();
        let block = [GuestInstruction { rip: 0x1000, insn }];
        let end = BlockEnd::Continue(0x1000 + insn.len as u64);
        let options = Options {
            features: crate::translator::HostFeatures {
                lse: true,
                rcpc: true,
            },
            chain: false,
            concurrent,
            compat: false,
            host_reads: None,
            page_table: true,
            entry_kind: 0,
        };
        translate(&block, end, options).unwrap()
    };
    let count = |words: &[u32], pattern: u32| {
        words
            .iter()
            .filter(|&&w| w & 0xFFE0_FC00 == pattern)
            .count()
    };
    const LDADDAL_X: u32 = 0xF8E0_0000;
    const SWPAL_X: u32 = 0xF8E0_8000;
    const CASAL_X: u32 = 0xC8E0_FC00;
    for (bytes, pattern) in [
        (&[0xf0u8, 0x48, 0x01, 0x0b][..], LDADDAL_X), // lock add [rbx], rcx
        (&[0xf0, 0x48, 0x0f, 0xc1, 0x0b], LDADDAL_X), // lock xadd [rbx], rcx
        (&[0x48, 0x87, 0x03], SWPAL_X),               // xchg [rbx], rax
        (&[0xf0, 0x48, 0x0f, 0xb1, 0x0b], CASAL_X),   // lock cmpxchg [rbx], rcx
    ] {
        assert_eq!(count(&words(bytes, true), pattern), 1, "{bytes:02x?}");
        assert_eq!(count(&words(bytes, false), pattern), 0, "{bytes:02x?}");
    }
    let adc = armor_x86::decode(&[0xf0, 0x48, 0x11, 0x0b]).unwrap();
    assert!(supported(&adc) && !supported_concurrent(&adc));
}

/// Flags nobody reads are not computed (`live_flags`): in the hot loop
/// of `bench threads` (xor, add, imul, mov, shr, xor, cmp, jne) only
/// cmp's flags reach a reader, so no flag is read back from NZCV
/// (`mrs`). The loop still computes what the interpreter does, rflags
/// included at the exits.
#[test]
fn dead_flags_are_not_computed() {
    use crate::translator::{BlockEnd, GuestInstruction, Options, translate};
    // The loop, with a count of 5 instead of 15,000,000.
    let code = [
        0x48, 0x31, 0xd0, 0x48, 0x83, 0xc2, 0x01, 0x48, 0x0f, 0xaf, 0xc6, 0x48, 0x89, 0xc1, 0x48,
        0xc1, 0xe9, 0x1d, 0x48, 0x31, 0xc8, 0x48, 0x81, 0xfa, 0x05, 0x00, 0x00, 0x00, 0x75, 0xe2,
    ];
    let mut block = Vec::new();
    let mut at = 0;
    while at < code.len() {
        let insn = armor_x86::decode(&code[at..]).unwrap();
        block.push(GuestInstruction {
            rip: 0x1000 + at as u64,
            insn,
        });
        at += insn.len as usize;
    }
    let options = Options {
        features: crate::translator::HostFeatures {
            lse: true,
            rcpc: true,
        },
        chain: true,
        concurrent: false,
        compat: false,
        host_reads: None,
        page_table: true,
        entry_kind: 0,
    };
    let words = translate(&block, BlockEnd::Continue(0x1000 + at as u64), options).unwrap();
    let is_mrs_nzcv = |w: &u32| w & 0xFFFF_FFE0 == 0xD53B_4200;
    // Only the epilogue's flags routine path may read NZCV: none here.
    assert_eq!(words.iter().filter(|w| is_mrs_nzcv(w)).count(), 0);

    let guest = Guest::new();
    let mut rng = Rng(17);
    for _ in 0..20 {
        let mut cpu = guest.cpu(&mut rng);
        cpu.regs[armor_cpu::RDX] = 0;
        guest.compare(&code, &cpu, 1);
    }
}

#[test]
fn every_condition_in_every_flag_state() {
    let guest = Guest::new();
    let mut rng = Rng(7);
    for cc in 0..16u8 {
        for flags in 0..64u64 {
            // Spread the six bits over CF, PF, AF, ZF, SF, OF.
            let status = [0, 2, 4, 6, 7, 11]
                .iter()
                .enumerate()
                .fold(0, |acc, (i, bit)| acc | (((flags >> i) & 1) << bit));
            let mut cpu = guest.cpu(&mut rng);
            cpu.rflags = 0x202 | status;
            guest.compare(&[0x70 | cc, 0x03, 0x48, 0xff, 0xc0], &cpu, 1);
        }
    }
}

#[test]
fn a_fault_leaves_the_state_of_the_instruction_before() {
    let guest = Guest::new();
    let mut rng = Rng(11);
    // add rax, rcx; mov rdx, [rbx + 0x4000] (unmapped); add rax, rax.
    let code = [
        0x48, 0x01, 0xc8, 0x48, 0x8b, 0x93, 0x00, 0x40, 0x00, 0x00, 0x48, 0x01, 0xc0,
    ];
    let cpu = guest.cpu(&mut rng);
    guest.compare(&code, &cpu, 1);
    let mut jitted = cpu.clone();
    let mut jit = Jit::new().unwrap();
    let result = jit.run(&mut jitted, &guest.mem.view(), 100);
    assert!(
        matches!(result, Err(CpuError::Fault { rip, .. }) if rip == guest.code + 3),
        "{result:?}"
    );
}

#[test]
fn writes_to_code_go_through_the_interpreter() {
    // A writable and executable page: the JIT's write check sends the
    // store to the interpreter, which invalidates the translated code.
    let guest = Guest::new();
    guest
        .mem
        .protect(guest.code, CODE_SIZE, Prot::READ | Prot::WRITE | Prot::EXEC)
        .unwrap();
    let mut rng = Rng(13);
    let mut cpu = guest.cpu(&mut rng);
    cpu.regs[RBX] = guest.code;
    // The function at 0x20 runs, is rewritten, and runs again: its first
    // translation must not.
    let mut code = vec![
        0xe8, 0x1b, 0, 0, 0, // 0x00: call 0x20
        0x89, 0x8b, 0x20, 0, 0, 0, // 0x05: mov [rbx + 0x20], ecx
        0xe8, 0x10, 0, 0, 0, // 0x0b: call 0x20
        0xeb, 0x1e, // 0x10: jmp 0x30 (the end)
    ];
    code.resize(0x20, 0x90);
    code.extend([0x48, 0xff, 0xc0, 0xc3]); // 0x20: inc rax; ret
    code.resize(0x30, 0x90);
    cpu.regs[1] = 0xc3c8_ff48; // dec rax; ret
    let before = cpu.regs[0];
    guest.compare(&code, &cpu, 1);
    // Both engines ran inc, then dec.
    let mut jitted = cpu.clone();
    guest.mem.write_privileged(guest.code, &code).unwrap();
    guest
        .mem
        .write_privileged(guest.code + 0x30, &[0x0f, 0x05])
        .unwrap();
    let mut jit = Jit::new().unwrap();
    assert_eq!(
        jit.run(&mut jitted, &guest.mem.view(), 1000),
        Ok(Step::Syscall)
    );
    assert_eq!(jitted.regs[0], before);
    // The rewrite dropped every translation: no indirect jump may still
    // lead to one.
    assert_eq!(jit.check_targets(), Ok(()));
}

/// mov ecx, 1000; loop: add rax, rcx; call f; dec ecx; jnz loop; jmp end;
/// f: add rdx, 3; ret; end: (syscall). Every exit of the loop is taken
/// many times, so the chained branches run.
const LOOP: [u8; 26] = [
    0xb9, 0xe8, 0x03, 0x00, 0x00, // 0x00: mov ecx, 1000
    0x48, 0x01, 0xc8, // 0x05: add rax, rcx
    0xe8, 0x06, 0x00, 0x00, 0x00, // 0x08: call 0x13
    0xff, 0xc9, // 0x0d: dec ecx
    0x75, 0xf4, // 0x0f: jnz 0x05
    0xeb, 0x05, // 0x11: jmp 0x18
    0x48, 0x83, 0xc2, 0x03, // 0x13: add rdx, 3
    0xc3, // 0x17: ret
    0x90, 0x90, // 0x18: end
];

#[test]
fn chained_loops_match_the_interpreter() {
    let guest = Guest::new();
    let mut rng = Rng(17);
    let cpu = guest.cpu(&mut rng);
    guest.compare(&LOOP, &cpu, 1);
}

#[test]
fn chained_loops_stay_in_translated_code() {
    let guest = Guest::new();
    let mut code = LOOP.to_vec();
    code.extend([0x0f, 0x05]);
    guest.mem.write_privileged(guest.code, &code).unwrap();
    let mut cpu = guest.cpu(&mut Rng(23));
    let mut jit = Jit::new().unwrap();
    assert_eq!(
        jit.run(&mut cpu, &guest.mem.view(), 1_000_000),
        Ok(Step::Syscall)
    );
    // Unchained, each of the 1000 iterations would return to the engine
    // three times (loop body, f, the ret's target); with direct jumps
    // chained but not the ret, once. With the indirect-jump table, only
    // until every block is translated and linked.
    assert!(jit.calls < 20, "{} block calls", jit.calls);
}

/// The flags of `cmp` cross a `ret`, in a loop whose every jump, the
/// `ret` included, stays in generated code once linked.
const FLAGS_ACROSS_RET: [u8; 33] = [
    0xb9, 0xe8, 0x03, 0x00, 0x00, // 0x00: mov ecx, 1000
    0xe8, 0x10, 0x00, 0x00, 0x00, // 0x05: call 0x1a
    0x72, 0x05, // 0x0a: jb 0x11
    0x48, 0xff, 0xc0, // 0x0c: inc rax
    0xeb, 0x03, // 0x0f: jmp 0x14
    0x48, 0xff, 0xc2, // 0x11: inc rdx
    0xff, 0xc9, // 0x14: dec ecx
    0x75, 0xed, // 0x16: jnz 0x05
    0xeb, 0x07, // 0x18: jmp 0x21 (the end)
    0x81, 0xf9, 0xf4, 0x01, 0x00, 0x00, // 0x1a: cmp ecx, 500
    0xc3, // 0x20: ret
];

#[test]
fn indirect_jumps_keep_flags_and_budget() {
    let guest = Guest::new();
    let mut rng = Rng(29);
    let cpu = guest.cpu(&mut rng);
    guest.compare(&FLAGS_ACROSS_RET, &cpu, 1);

    // A small budget must still bring control back to the engine.
    let mut code = FLAGS_ACROSS_RET.to_vec();
    code.extend([0x0f, 0x05]);
    guest.mem.write_privileged(guest.code, &code).unwrap();
    let mut cpu = cpu.clone();
    let mut jit = Jit::new().unwrap();
    let mut returns = 0;
    while jit.run(&mut cpu, &guest.mem.view(), 50) == Ok(Step::Continue) {
        returns += 1;
        assert!(returns < 10_000, "the budget is not enforced");
    }
    assert!(returns >= 100, "only {returns} returns to the engine");
}

#[test]
fn chained_code_returns_when_the_budget_runs_out() {
    let guest = Guest::new();
    // A loop chained to itself, with no indirect jump to return to the
    // engine: mov ecx, 1000; loop: add rax, rcx; add rdx, rax; nop; nop;
    // dec ecx; jnz loop.
    let code = [
        0xb9, 0xe8, 0x03, 0x00, 0x00, 0x48, 0x01, 0xc8, 0x48, 0x01, 0xc2, 0x90, 0x90, 0xff, 0xc9,
        0x75, 0xf4, 0x0f, 0x05,
    ];
    guest.mem.write_privileged(guest.code, &code).unwrap();
    let mut rng = Rng(19);
    let mut cpu = guest.cpu(&mut rng);
    let mut expected = cpu.clone();
    expected.run_until_syscall(&guest.mem.view()).unwrap();

    // 1000 iterations of 6 instructions: with a budget of 50, the engine
    // must get control back about 120 times, chained or not.
    let mut jit = Jit::new().unwrap();
    let mut calls = 0;
    while jit.run(&mut cpu, &guest.mem.view(), 50) == Ok(Step::Continue) {
        calls += 1;
        assert!(calls < 1000, "the budget is not enforced");
    }
    assert!(calls >= 100, "only {calls} returns to the engine");
    assert_eq!(cpu.regs, expected.regs);
    assert_eq!(cpu.rip, expected.rip);
    assert_eq!(cpu.rflags, expected.rflags);
}

/// Every flag-setting operation the translator defers, at every size,
/// followed by every condition: right after it (the condition is taken
/// from the pending operation), and in the next block (through `KIND`
/// and the flags routine), also read by setcc and adc.
#[test]
fn conditions_after_every_lazy_flag_operation() {
    let guest = Guest::new();
    let mut rng = Rng(31);
    // (opcode bytes for al/ax/eax/rax op= cl/cx/ecx/rcx or op al.., byte form?)
    let ops: [(&[u8], &[u8]); 9] = [
        (&[0x01, 0xc8], &[0x00, 0xc8]), // add
        (&[0x29, 0xc8], &[0x28, 0xc8]), // sub
        (&[0x39, 0xc8], &[0x38, 0xc8]), // cmp
        (&[0x21, 0xc8], &[0x20, 0xc8]), // and
        (&[0x85, 0xc8], &[0x84, 0xc8]), // test
        (&[0x31, 0xc8], &[0x30, 0xc8]), // xor
        (&[0xff, 0xc0], &[0xfe, 0xc0]), // inc
        (&[0xff, 0xc8], &[0xfe, 0xc8]), // dec
        (&[0xf7, 0xd8], &[0xf6, 0xd8]), // neg
    ];
    let values = [
        0u64,
        1,
        0x7f,
        0x80,
        0xff,
        0x7fff,
        0x8000,
        0xffff,
        0x7fff_ffff,
        0x8000_0000,
        0xffff_ffff,
        i64::MAX as u64,
        i64::MIN as u64,
        u64::MAX,
    ];
    for (wide, narrow) in ops {
        for size in 0..4 {
            for cc in 0..16u8 {
                for form in 0..4 {
                    let mut code = Vec::new();
                    match size {
                        0 => code.extend(narrow),
                        1 => code.extend([&[0x66u8][..], wide].concat()),
                        2 => code.extend(wide),
                        _ => code.extend([&[0x48u8][..], wide].concat()),
                    }
                    match form {
                        0 => code.extend([0x70 | cc, 0x03, 0x48, 0xff, 0xc2]),
                        1 => code.extend([0xeb, 0x00, 0x70 | cc, 0x03, 0x48, 0xff, 0xc2]),
                        // setcc dl, then adc rsi, rdi, in the next block.
                        2 => code.extend([0xeb, 0x00, 0x0f, 0x90 | cc, 0xc2, 0x48, 0x11, 0xfe]),
                        // jcc over nothing, then cmovcc rdx, [rbx + 0x40]:
                        // the load's check changes the host's flags.
                        _ => code.extend([0x70 | cc, 0x00, 0x48, 0x0f, 0x40 | cc, 0x53, 0x40]),
                    }
                    let mut cpu = guest.cpu(&mut rng);
                    cpu.regs[0] = rng.pick(&values);
                    cpu.regs[1] = rng.pick(&values);
                    guest.compare(&code, &cpu, 1);
                }
            }
        }
    }
}

/// Reads and writes near address 0 fault, as under the interpreter, also
/// when nothing is known yet about the pages last checked.
#[test]
fn null_pointer_accesses_fault() {
    let guest = Guest::new();
    let mut rng = Rng(37);
    for code in [
        [0x48u8, 0x8b, 0x01], // mov rax, [rcx]
        [0x48, 0x89, 0x01],   // mov [rcx], rax
        [0x48, 0x8b, 0x02],   // mov rax, [rdx]
    ] {
        for addr in [0u64, 8, 0xff8] {
            let mut cpu = guest.cpu(&mut rng);
            cpu.regs[1] = addr;
            cpu.regs[2] = addr;
            guest.compare(&code, &cpu, 1);
        }
    }
}

/// Division edge cases: by zero, MIN / -1, quotients that do not fit,
/// dividends wider than 64 bits; at 32 and 64 bits, signed and unsigned.
#[test]
fn division_edge_cases() {
    let guest = Guest::new();
    let mut rng = Rng(41);
    let cases: [(u64, u64, u64); 8] = [
        (i64::MIN as u64, u64::MAX, u64::MAX), // MIN / -1, rdx = sign
        (i64::MIN as u64, u64::MAX, 0),
        (0x8000_0000, 0xffff_ffff, 0xffff_ffff), // 32-bit MIN / -1
        (100, 0, 0),                             // by zero
        (5, 7, 3),                               // wide dividend
        (u64::MAX, 2, 1),
        (u64::MAX, 3, 0),
        (12345, u64::MAX - 6, u64::MAX),
    ];
    for (rax, rcx, rdx) in cases {
        for code in [
            [0x48u8, 0xf7, 0xf9], // idiv rcx
            [0x48, 0xf7, 0xf1],   // div rcx
            [0x40, 0xf7, 0xf9],   // idiv ecx
            [0x40, 0xf7, 0xf1],   // div ecx
        ] {
            let mut cpu = guest.cpu(&mut rng);
            (cpu.regs[0], cpu.regs[1], cpu.regs[2]) = (rax, rcx, rdx);
            guest.compare(&code, &cpu, 1);
        }
    }
}

/// Permission checks of nearby accesses with the same base: they must
/// follow the base register's changes and rsp's moves, and fault where
/// the interpreter does, at the edges of the stack and of the mappings.
/// (Written for a grouped-checks attempt, since discarded:
/// `tech/performance.md`; it checks the current code as well.)
#[test]
fn checks_follow_base_and_stack_changes() {
    let guest = Guest::new();
    let mut rng = Rng(43);
    let stack_base = guest.stack_top - STACK_SIZE / 2;
    // (code, a register to set first and its value)
    type Case<'a> = (&'a [u8], Option<(usize, u64)>);
    let cases: [Case; 7] = [
        // mov rax, [rbx+8]; add rbx, 0x2000 (the guard page); mov rcx, [rbx+8]
        (
            &[
                0x48, 0x8b, 0x43, 0x08, 0x48, 0x81, 0xc3, 0x00, 0x20, 0x00, 0x00, 0x48, 0x8b, 0x4b,
                0x08,
            ],
            None,
        ),
        // push rax; push rcx, with rsp 8 bytes above the bottom of the stack
        (&[0x50, 0x51], Some((RSP, stack_base + 8))),
        // pop rax; pop rcx, with rsp 8 bytes below the top of the stack
        (&[0x58, 0x59], Some((RSP, stack_base + STACK_SIZE - 8))),
        // mov rax, [rsp]; sub rsp, 0x1100; mov rcx, [rsp]
        (
            &[
                0x48, 0x8b, 0x04, 0x24, 0x48, 0x81, 0xec, 0x00, 0x11, 0x00, 0x00, 0x48, 0x8b, 0x0c,
                0x24,
            ],
            None,
        ),
        // mov rax, [rbx]; mov rcx, [rbx+0x10], across two valid pages
        (
            &[0x48, 0x8b, 0x03, 0x48, 0x8b, 0x4b, 0x10],
            Some((RBX, 0xff0)),
        ),
        // mov [rbx], rax; mov [rbx+8], rcx; mov rdx, [rbx+16]
        (
            &[
                0x48, 0x89, 0x03, 0x48, 0x89, 0x4b, 0x08, 0x48, 0x8b, 0x53, 0x10,
            ],
            None,
        ),
        // A group allowed in one block (mov rax, [rbx]; mov rcx, [rbx+8];
        // jmp), then in the next a group that reaches the guard page
        // (mov rdx, [rbx+0x1ff8]; mov rsi, [rbx+0x2000]): the second
        // block's checks must stand on their own.
        (
            &[
                0x48, 0x8b, 0x03, 0x48, 0x8b, 0x4b, 0x08, 0xeb, 0x00, 0x48, 0x8b, 0x93, 0xf8, 0x1f,
                0x00, 0x00, 0x48, 0x8b, 0xb3, 0x00, 0x20, 0x00, 0x00,
            ],
            None,
        ),
    ];
    for (code, set) in cases {
        let mut cpu = guest.cpu(&mut rng);
        match set {
            Some((RBX, offset)) => cpu.regs[RBX] = guest.data + offset,
            Some((reg, value)) => cpu.regs[reg] = value,
            None => {}
        }
        guest.compare(code, &cpu, 1);
    }
}

/// Scalar floating point on every pair of special values (signed zeros,
/// infinities, quiet and signaling NaNs, denormals, ...): min and max
/// return the second operand on equal or unordered values, NaNs follow
/// the x86 rules (through the interpreter), comparisons set ZF, PF, CF,
/// and MXCSR gets the x86 exception flags (ADR-0024): DE, UE with
/// tininess after rounding (a product that rounds up to the smallest
/// normal number), IE alone for an out-of-range conversion.
#[test]
fn float_operations_on_special_values() {
    let guest = Guest::new();
    let mut rng = Rng(47);
    let singles: [u64; 14] = [
        0,
        0x8000_0000,
        0x3f80_0000,
        0xbf80_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0000,
        0xffc0_0001,
        0x7f80_0001,
        0x0000_0001,
        // Smallest normal × (1 + 2^-23), and 1 - 2^-23: their product
        // rounds up to the smallest normal.
        0x0080_0001,
        0x3f7f_fffe,
        // 3e9: out of int32 range.
        0x4f32_d05e,
        0x3eaa_aaab,
    ];
    let doubles: [u64; 14] = [
        0,
        0x8000_0000_0000_0000,
        0x3ff0_0000_0000_0000,
        0xbff0_0000_0000_0000,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_0000_0000_0000,
        0xfff8_0000_0000_0001,
        0x7ff0_0000_0000_0001,
        0x0000_0000_0000_0001,
        0x0010_0000_0000_0001,
        0x3fef_ffff_ffff_fffe,
        0x41e6_5a0b_c000_0000,
        0x3fd5_5555_5555_5555,
    ];
    for (prefix, values) in [(0xf3u8, singles), (0xf2, doubles)] {
        // xmm0 op= xmm1: min, max, add, mul, div, sqrt; comiss/ucomiss (66
        // for double); cmpss with each predicate.
        let compare_prefix: &[u8] = if prefix == 0xf2 { &[0x66] } else { &[] };
        // (0x5a: cvtss2sd / cvtsd2ss)
        let mut codes: Vec<Vec<u8>> = [0x5du8, 0x5f, 0x58, 0x59, 0x5e, 0x51, 0x5a]
            .iter()
            .map(|&op| vec![prefix, 0x0f, op, 0xc1])
            .collect();
        // cvt(t)ss2si / cvt(t)sd2si eax and rax, xmm1.
        for op in [0x2cu8, 0x2d] {
            codes.push(vec![prefix, 0x0f, op, 0xc1]);
            codes.push(vec![prefix, 0x48, 0x0f, op, 0xc1]);
        }
        for op in [0x2eu8, 0x2f] {
            codes.push([compare_prefix, &[0x0f, op, 0xc1]].concat());
        }
        for predicate in 0..8 {
            codes.push(vec![prefix, 0x0f, 0xc2, 0xc1, predicate]);
        }
        for &a in &values {
            for &b in &values {
                for code in &codes {
                    let mut cpu = guest.cpu(&mut rng);
                    cpu.xmm[0] = (cpu.xmm[0] & !u128::from(u64::MAX)) | u128::from(a);
                    cpu.xmm[1] = (cpu.xmm[1] & !u128::from(u64::MAX)) | u128::from(b);
                    guest.compare(code, &cpu, 1);
                }
            }
        }
    }
}

/// Packed floating point (`sse_packed`) on lanes of special values: each
/// lane drawn from NaNs, infinities, denormals, zeros, values whose
/// product underflows, and values out of int32's range; every operation,
/// compare predicate and conversion, with registers and with memory.
#[test]
fn packed_float_operations_on_special_values() {
    let guest = Guest::new();
    let mut rng = Rng(53);
    let singles: [u32; 16] = [
        0,
        0x8000_0000,
        0x3f80_0000,
        0xbf80_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0000,
        0xffc0_0001,
        0x7f80_0001,
        0x0000_0001,
        0x0080_0001,
        0x3f7f_fffe,
        0x4f32_d05e,
        0xcf32_d05e,
        0x4f00_0000,
        0x3eaa_aaab,
    ];
    let doubles: [u64; 12] = [
        0,
        0x8000_0000_0000_0000,
        0x3ff0_0000_0000_0000,
        0x7ff0_0000_0000_0000,
        0x7ff8_0000_0000_0000,
        0xfff8_0000_0000_0001,
        0x7ff0_0000_0000_0001,
        0x0000_0000_0000_0001,
        0x0010_0000_0000_0001,
        0x3fef_ffff_ffff_fffe,
        0x3810_0000_0000_0001,
        0x47ef_ffff_e000_0000,
    ];
    let mut codes: Vec<Vec<u8>> = Vec::new();
    for prefix in [&[][..], &[0x66]] {
        for op in [0x58u8, 0x59, 0x5c, 0x5d, 0x5e, 0x5f, 0x51] {
            codes.push([prefix, &[0x0f, op, 0xc1]].concat());
        }
        for predicate in 0..8 {
            codes.push([prefix, &[0x0f, 0xc2, 0xc1, predicate]].concat());
        }
    }
    // cvtdq2ps, cvtps2dq, cvttps2dq, cvtps2pd, cvtpd2ps, cvtdq2pd.
    for code in [
        &[0x0f, 0x5b, 0xc1][..],
        &[0x66, 0x0f, 0x5b, 0xc1],
        &[0xf3, 0x0f, 0x5b, 0xc1],
        &[0x0f, 0x5a, 0xc1],
        &[0x66, 0x0f, 0x5a, 0xc1],
        &[0xf3, 0x0f, 0xe6, 0xc1],
    ] {
        codes.push(code.to_vec());
    }
    // The memory forms: [rbx + 0x40] (aligned) and, for the m64 sources
    // of cvtps2pd and cvtdq2pd, [rbx + 0x44].
    let with_memory: Vec<Vec<u8>> = codes
        .iter()
        .map(|code| {
            let mut code = code.clone();
            let at = code.iter().position(|&b| b == 0xc1).expect("a modrm");
            let m64 = code.ends_with(&[0x5a, 0xc1]) && code.len() == 3
                || code[0] == 0xf3 && code[2] == 0xe6;
            code[at] = 0x43;
            code.insert(at + 1, if m64 { 0x44 } else { 0x40 });
            code
        })
        .collect();
    codes.extend(with_memory);
    for round in 0..4000 {
        let mut cpu = guest.cpu(&mut rng);
        for reg in [0, 1] {
            cpu.xmm[reg] = if round % 2 == 0 {
                (0..4).fold(0u128, |v, i| v | u128::from(rng.pick(&singles)) << (32 * i))
            } else {
                u128::from(rng.pick(&doubles)) | u128::from(rng.pick(&doubles)) << 64
            };
        }
        let code = &codes[rng.below(codes.len() as u64) as usize];
        guest.compare(code, &cpu, round + 1);
    }
}

/// Not a test: a tool for reading generated code (docs/tech/performance.md).
/// Translates the block in the hex bytes of `ARMOR_DUMP_HEX`, at guest
/// address `ARMOR_DUMP_RIP` (hex), and writes the AArch64 code to
/// `ARMOR_DUMP_OUT`, for `objdump -D -b binary -m aarch64`;
/// `ARMOR_DUMP_CONCURRENT` translates for concurrent mode,
/// `ARMOR_DUMP_HOST_READS=bits` with host protection (ADR-0046).
#[test]
#[ignore]
fn dump_translation() {
    use crate::translator::{BlockEnd, GuestInstruction, Options, ends_block, translate};
    let hex = std::env::var("ARMOR_DUMP_HEX").unwrap();
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    let mut rip = u64::from_str_radix(&std::env::var("ARMOR_DUMP_RIP").unwrap(), 16).unwrap();
    let start = rip;
    let mut block = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let mode = if std::env::var_os("ARMOR_DUMP_COMPAT").is_some() {
            Mode::Compat32
        } else {
            Mode::Long
        };
        let Ok(insn) = armor_x86::decode_in(&bytes[at..], mode) else {
            break;
        };
        if !crate::translator::supported(&insn) {
            break;
        }
        block.push(GuestInstruction { rip, insn });
        at += insn.len;
        rip += insn.len as u64;
        if ends_block(&insn) {
            break;
        }
    }
    let concurrent = std::env::var("ARMOR_DUMP_CONCURRENT").is_ok();
    let words = translate(
        &block,
        BlockEnd::Continue(rip),
        Options {
            features: crate::translator::HostFeatures {
                lse: true,
                rcpc: true,
            },
            chain: true,
            concurrent,
            compat: std::env::var_os("ARMOR_DUMP_COMPAT").is_some(),
            host_reads: std::env::var("ARMOR_DUMP_HOST_READS")
                .ok()
                .map(|b| b.parse().unwrap()),
            page_table: true,
            entry_kind: 0,
        },
    )
    .unwrap();
    let out: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(std::env::var("ARMOR_DUMP_OUT").unwrap(), out).unwrap();
    eprintln!(
        "{} guest instructions from {start:#x}, {} words",
        block.len(),
        words.len()
    );
}

/// A block entered with flags pending from blocks of different kinds
/// (an 8-bit subtraction, a 32-bit addition, a logical operation, an
/// increment that keeps CF), whose `inc` needs only CF of them: inline for
/// the kind seen when it was translated, through the flag routine for the
/// others (`carry_from_kind`). `adc` sums the carries; the first entry,
/// and so the expected kind, depends on the count.
#[test]
fn carry_from_the_kind_left_pending() {
    let guest = Guest::new();
    let mut rng = Rng(53);
    #[rustfmt::skip]
    let code = [
        0x89, 0xc8, 0x83, 0xe0, 0x03, 0x74, 0x0f, 0x83, 0xf8, 0x01, 0x74, 0x0e, // mov eax, ecx; and eax, 3; jz a; cmp eax, 1; je b
        0x83, 0xf8, 0x02, 0x74, 0x0e, 0x45, 0x38, 0xec, 0xeb, 0x0e, // cmp eax, 2; je c; cmp r12b, r13b; jmp join
        0x01, 0xf2, 0xeb, 0x0a, // a: add edx, esi; jmp join
        0x4d, 0x21, 0xda, 0xeb, 0x05, // b: and r10, r11; jmp join
        0x49, 0xff, 0xc1, 0xeb, 0x00, // c: inc r9; jmp join
        0x48, 0xff, 0xc7, 0x49, 0x83, 0xd0, 0x00, // join: inc rdi; adc r8, 0
        0x49, 0xc1, 0xc4, 0x07, 0x4d, 0x01, 0xe5, 0x44, 0x01, 0xe6, // rol r12, 7; add r13, r12; add esi, r12d
        0xff, 0xc9, 0x75, 0xc7, // dec ecx; jnz loop
    ];
    for round in 0..64u64 {
        let mut cpu = guest.cpu(&mut rng);
        cpu.regs[1] = 200 + round;
        guest.compare(&code, &cpu, round);
    }
}

/// The flags that wait as operands and are computed by whoever observes
/// them (shifts, multiplications), or that pass through (counts of 0,
/// rotates, bit tests and scans, inc and dec): every condition after each,
/// in the same block, in the next one, after an earlier comparison whose
/// flags must survive, and at a fault right after (the interpreter sees
/// the exact flags).
#[test]
fn conditions_after_deferred_and_partial_flags() {
    let guest = Guest::new();
    let mut rng = Rng(47);
    let mut ops: Vec<Vec<u8>> = Vec::new();
    // Shifts and rotates of rax/eax/ax/al by cl, 1 and 5.
    for ext in [4u8, 5, 7, 0, 1] {
        let modrm = 0xc0 | ext << 3;
        for prefix in [&[0x48u8][..], &[], &[0x66], &[0xff]] {
            let (cl, one, imm) = if prefix == [0xff] {
                (0xd2, 0xd0, 0xc0)
            } else {
                (0xd3, 0xd1, 0xc1)
            };
            let p: &[u8] = if prefix == [0xff] { &[] } else { prefix };
            ops.push([p, &[cl, modrm]].concat());
            ops.push([p, &[one, modrm]].concat());
            ops.push([p, &[imm, modrm, 5]].concat());
        }
    }
    for p in [&[0x48u8][..], &[]] {
        ops.push([p, &[0x0f, 0xaf, 0xc1]].concat()); // imul rax, rcx
        ops.push([p, &[0x6b, 0xc1, 0x07]].concat()); // imul rax, rcx, 7
        ops.push([p, &[0xf7, 0xe1]].concat()); // mul rcx
        ops.push([p, &[0xf7, 0xe9]].concat()); // imul rcx
        ops.push([p, &[0x0f, 0xa3, 0xc8]].concat()); // bt rax, rcx
        ops.push([p, &[0x0f, 0xbc, 0xc1]].concat()); // bsf rax, rcx
        ops.push([p, &[0xff, 0xc0]].concat()); // inc rax
        ops.push([p, &[0xff, 0xc8]].concat()); // dec rax
    }
    let values = [
        0u64,
        1,
        5,
        31,
        32,
        63,
        64,
        0x7f,
        0x80,
        0xffff_ffff,
        0x8000_0000,
        i64::MAX as u64,
        i64::MIN as u64,
        u64::MAX,
    ];
    for op in &ops {
        for cc in 0..16u8 {
            for form in 0..8 {
                let mut code = Vec::new();
                if (3..5).contains(&form) {
                    // cmp rcx, rdx first: its flags pass through some.
                    code.extend([0x48, 0x39, 0xd1]);
                }
                code.extend(op);
                match form {
                    0 | 3 => code.extend([0x70 | cc, 0x03, 0x48, 0xff, 0xc2]),
                    1 => code.extend([0xeb, 0x00, 0x70 | cc, 0x03, 0x48, 0xff, 0xc2]),
                    // setcc dl, then adc rsi, rdi, in the next block.
                    2 => code.extend([0xeb, 0x00, 0x0f, 0x90 | cc, 0xc2, 0x48, 0x11, 0xfe]),
                    // A load from address 0: the fault sees the flags.
                    4 => code.extend([0x48, 0x8b, 0x14, 0x25, 0, 0, 0, 0]),
                    // setcc dl and cmovcc rdi, rsi in the same block,
                    // then xor esi, esi: only the condition's flags live.
                    5 => code.extend([0x0f, 0x90 | cc, 0xc2, 0x31, 0xf6]),
                    6 => code.extend([0x48, 0x0f, 0x40 | cc, 0xfe, 0x31, 0xf6]),
                    // A jcc as the block's last instruction (the syscall
                    // after it is not translated): both exits.
                    _ => code.extend([0x70 | cc, 0x00]),
                }
                let mut cpu = guest.cpu(&mut rng);
                cpu.regs[0] = rng.pick(&values);
                cpu.regs[1] = rng.pick(&values);
                cpu.regs[2] = rng.pick(&values);
                guest.compare(&code, &cpu, 1);
            }
        }
    }
}
