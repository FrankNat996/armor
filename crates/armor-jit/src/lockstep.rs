//! Lockstep mode (ADR-0021 §9.2, `armor --lockstep`): every instruction
//! the translator handles runs twice from the same state, first in the
//! interpreter (the oracle) and then translated on its own, and the two
//! results must agree. The first difference stops armor with a report
//! naming the instruction.
//!
//! For each translated instruction:
//! 1. registers, rip and flags are saved;
//! 2. the interpreter runs it through `Recorder`, which keeps the old and
//!    new bytes of every write;
//! 3. the writes are undone and the state restored;
//! 4. the instruction's translation (a block of one instruction) runs;
//! 5. registers, rip, flags, MXCSR, the x87 and MMX state and the bytes
//!    the interpreter wrote
//!    are compared. If the translation took its slow path, the state must
//!    be exactly as saved, and the interpreter then runs the instruction
//!    for real (that is also how a fault is reported). The one exception
//!    is MXCSR: a host operation may already have raised exception flags
//!    before the slow path, and that is right if the interpreter raises
//!    them too (`sse_float` in the translator).
//!
//! Limits: a translated store to an address the interpreter did not write
//! is only seen if it changes bytes the interpreter wrote; state the
//! translator never touches (x87, segment bases) is not compared.

use std::cell::{Cell, RefCell};

use armor_cpu::{Cpu, CpuError, FpuState, STATUS_FLAGS, Step};
use armor_mem::{GuestMemory, MemFault, MemView};
use armor_x86::{MAX_INSTRUCTION_LEN, decode};

use crate::engine::{Entry, Jit};
use crate::translator::EXIT_INTERPRET;

/// What the translated code may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct State {
    regs: [u64; 16],
    rip: u64,
    rflags: u64,
    xmm: [u128; 16],
    mxcsr: u32,
    /// x87 and MMX: translated MMX instructions change it.
    fpu: FpuState,
}

impl State {
    fn of(cpu: &Cpu) -> State {
        State {
            regs: cpu.regs,
            rip: cpu.rip,
            rflags: cpu.rflags,
            xmm: cpu.xmm,
            mxcsr: cpu.mxcsr,
            fpu: cpu.fpu_state(),
        }
    }

    fn restore(&self, cpu: &mut Cpu) {
        cpu.regs = self.regs;
        cpu.rip = self.rip;
        cpu.rflags = self.rflags;
        cpu.xmm = self.xmm;
        cpu.mxcsr = self.mxcsr;
        cpu.set_fpu_state(&self.fpu);
    }
}

struct Write {
    addr: u64,
    old: Vec<u8>,
    new: Vec<u8>,
}

/// Guest memory that remembers what it overwrote.
///
/// Memory shared with other processes (ADR-0022) cannot be compared this
/// way: they may change it between the two runs, and undoing a write
/// could overwrite theirs. An access to it sets `shared` (a write is not
/// made), and `run` then executes the instruction once, interpreted,
/// without a comparison.
struct Recorder<'m, 'a> {
    mem: &'m MemView<'a>,
    writes: RefCell<Vec<Write>>,
    shared: Cell<bool>,
}

impl GuestMemory for Recorder<'_, '_> {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemFault> {
        if self.mem.in_shared(addr, buf.len()) {
            self.shared.set(true);
        }
        self.mem.read(addr, buf)
    }

    fn write(&self, addr: u64, data: &[u8]) -> Result<(), MemFault> {
        if self.mem.in_shared(addr, data.len()) {
            self.shared.set(true);
            return Ok(());
        }
        let mut old = vec![0; data.len()];
        if self.mem.read(addr, &mut old).is_err() {
            // Not readable, hence not writable: the write reports the
            // fault, at the address a write reports.
            return self.mem.write(addr, data);
        }
        self.mem.write(addr, data)?;
        self.writes.borrow_mut().push(Write {
            addr,
            old,
            new: data.to_vec(),
        });
        Ok(())
    }

    fn fetch(&self, addr: u64, buf: &mut [u8]) -> usize {
        self.mem.fetch(addr, buf)
    }

    fn code_generation(&self) -> u64 {
        self.mem.code_generation()
    }
}

pub(crate) fn run(
    jit: &mut Jit,
    cpu: &mut Cpu,
    mem: &MemView,
    budget: u64,
) -> Result<Step, CpuError> {
    for _ in 0..budget {
        jit.sync_mxcsr(cpu);
        let rip = cpu.rip;
        let Entry::Code(..) = jit.entry(rip, mem) else {
            // Not translated: the interpreter alone.
            if cpu.step(mem)? == Step::Syscall {
                return Ok(Step::Syscall);
            }
            continue;
        };
        let before = State::of(cpu);

        let recorder = Recorder {
            mem,
            writes: RefCell::new(Vec::new()),
            shared: Cell::new(false),
        };
        let interpreted = cpu.step(&recorder);
        let expected = State::of(cpu);
        let shared = recorder.shared.get();
        let writes = recorder.writes.into_inner();
        for write in writes.iter().rev() {
            mem.write(write.addr, &write.old)
                .expect("armor bug: cannot undo a write the interpreter made");
        }
        before.restore(cpu);
        if shared {
            // Shared memory: interpreted once, not compared.
            if cpu.step(mem)? == Step::Syscall {
                return Ok(Step::Syscall);
            }
            continue;
        }

        // Undoing a write to code changed the generation: `entry` would
        // drop `code`. Translate again from the restored bytes.
        let code = match jit.entry(rip, mem) {
            Entry::Code(code) => code,
            Entry::Interpret => unreachable!("the bytes at {rip:#x} are restored"),
        };
        // Budget enough for the one instruction: lockstep blocks are not
        // chained.
        let (status, _) = jit.execute(code, cpu, mem, 1);
        let translated = State::of(cpu);

        if status == EXIT_INTERPRET {
            // The slow path: nothing may have changed, but for MXCSR flags
            // the interpreter raises as well.
            let raised = translated.mxcsr & !before.mxcsr;
            let same = State {
                mxcsr: before.mxcsr,
                ..translated
            } == before;
            if !same || raised & !expected.mxcsr != 0 {
                mismatch(
                    mem,
                    rip,
                    &before,
                    &before,
                    &translated,
                    "slow path changed the state",
                );
            }
            if cpu.step(mem)? == Step::Syscall {
                unreachable!("the translator does not handle syscall");
            }
            continue;
        }
        if let Err(error) = interpreted {
            mismatch(
                mem,
                rip,
                &before,
                &expected,
                &translated,
                &format!("the interpreter stopped with \"{error}\", the translation went on"),
            );
        }
        if translated != expected {
            mismatch(mem, rip, &before, &expected, &translated, "different state");
        }
        for write in &writes {
            let mut now = vec![0; write.new.len()];
            if mem.read(write.addr, &mut now).is_err() || now != write.new {
                mismatch(
                    mem,
                    rip,
                    &before,
                    &expected,
                    &translated,
                    &format!(
                        "memory at {:#x}: interpreter wrote {:02x?}, translation left {:02x?}",
                        write.addr, write.new, now
                    ),
                );
            }
        }
    }
    Ok(Step::Continue)
}

/// Stops armor: the translation of the instruction at `rip` disagrees
/// with the interpreter.
fn mismatch(
    mem: &MemView,
    rip: u64,
    before: &State,
    expected: &State,
    translated: &State,
    what: &str,
) -> ! {
    let mut bytes = [0u8; MAX_INSTRUCTION_LEN];
    let available = mem.fetch(rip, &mut bytes);
    let insn = decode(&bytes[..available]);
    let len = insn.as_ref().map_or(available, |i| i.len);
    let mut report = format!(
        "armor lockstep: the translation of the instruction at {rip:#x} disagrees with \
         the interpreter: {what}\n  bytes {:02x?}\n  decoded {insn:?}\n",
        &bytes[..len]
    );
    let names = [
        "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12",
        "r13", "r14", "r15",
    ];
    for (n, name) in names.iter().enumerate() {
        let (b, e, t) = (before.regs[n], expected.regs[n], translated.regs[n]);
        if e != t || b != e {
            let mark = if e != t { "  <-- differs" } else { "" };
            report += &format!(
                "  {name:>6}: before {b:#018x}  interpreter {e:#018x}  translation {t:#018x}{mark}\n"
            );
        }
    }
    for n in 0..16 {
        let (b, e, t) = (before.xmm[n], expected.xmm[n], translated.xmm[n]);
        if e != t || b != e {
            let mark = if e != t { "  <-- differs" } else { "" };
            report += &format!(
                "  xmm{n:<2}: before {b:#034x}  interpreter {e:#034x}  translation {t:#034x}{mark}\n"
            );
        }
    }
    report += &format!(
        "  rip:    interpreter {:#x}  translation {:#x}\n",
        expected.rip, translated.rip
    );
    report += &format!(
        "  mxcsr:  before {:#x}  interpreter {:#x}  translation {:#x}\n",
        before.mxcsr, expected.mxcsr, translated.mxcsr
    );
    report += &format!(
        "  rflags: before {:#x}  interpreter {:#x}  translation {:#x} (status bits differing: {:#x})\n",
        before.rflags,
        expected.rflags,
        translated.rflags,
        (expected.rflags ^ translated.rflags) & STATUS_FLAGS
    );
    panic!("{report}");
}
