//! The JIT's run loop (ADR-0021, docs/tech/jit.md): translated blocks
//! where the translator handles the code, the interpreter for everything
//! else, one instruction at a time.
//!
//! Each guest thread has its own `Jit`: its blocks and its code buffer.
//! Translated code is valid for one "code generation" of the address
//! space (ADR-0020), like the interpreter's decoded instructions: when the
//! generation changes (a write to an executable page, or a mapping change
//! that touches one), every block is dropped.
//!
//! The code buffer is a shared mapping (`buffer`), which a host `fork`
//! (ADR-0019) would share between parent and child: the child notices
//! that `FORKS` changed and starts over with a buffer of its own. It used
//! to compare the pid, but `getpid` is a real syscall, and it ran at every
//! `run`: once per guest syscall.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use armor_cpu::{Cpu, CpuError, INITIAL_MXCSR, MXCSR_CONTROL, Step};
use armor_mem::{GuestMemory, MemView};
use armor_x86::{MAX_INSTRUCTION_LEN, Mode, decode_in};

use crate::buffer::CodeBuffer;
use crate::cache;
use crate::encoder::branch_word;
use crate::translator::{
    BlockEnd, CHAINED_ENTRY, Context, EXIT_CHAIN, EXIT_INTERPRET, EXIT_SYSCALL, FLAG_TABLE,
    GuestInstruction, HostFeatures, MAX_BLOCK_INSTRUCTIONS, Options, TARGET_ENTRIES,
    UNCHECKED_ENTRY, ends_block, supported, supported_concurrent, target_slot, translate_block,
    uses_mxcsr_control,
};
use crate::translator::{enter_routine, flags_routine, leave_routine};

/// Clears FPSR, the host's cumulative floating-point exception flags, so
/// that after a block it holds the flags of the guest's SSE floating point
/// alone (ADR-0024): the translated code computes it with host
/// instructions, which set them.
fn clear_fpsr() {
    // SAFETY: writes FPSR only; Rust code assumes nothing about its
    // cumulative exception bits.
    unsafe { std::arch::asm!("msr fpsr, xzr", options(nomem, nostack)) };
}

fn read_fpsr() -> u64 {
    let fpsr: u64;
    // SAFETY: reads FPSR only.
    unsafe { std::arch::asm!("mrs {}, fpsr", out(reg) fpsr, options(nomem, nostack)) };
    fpsr
}

/// The MXCSR exception flags of FPSR's cumulative flags: IOC, DZC, OFC,
/// UFC, IXC (bits 0..4) are IE, ZE, OE, UE, PE (MXCSR bits 0, 2..5). x86
/// has DE where ARM has nothing (with FPCR.FZ off IDC is never set): the
/// translated code leaves denormal operands to the interpreter. QC (NEON
/// saturation) is not a floating-point flag.
fn mxcsr_flags(fpsr: u64) -> u32 {
    let fpsr = fpsr as u32;
    (fpsr & 1) | ((fpsr & 0x1e) << 1)
}

/// Host forks this process has been through, counted in the child by a
/// `pthread_atfork` handler: armor forks with `libc::fork` (ADR-0019).
static FORKS: AtomicU64 = AtomicU64::new(0);

extern "C" fn count_fork() {
    FORKS.fetch_add(1, Ordering::Relaxed);
}

/// The fork count, registering the handler on first use.
fn forks() -> u64 {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: registers a handler that only adds to an atomic, which is
        // async-signal-safe as the child side of fork requires.
        let registered = unsafe { libc::pthread_atfork(None, None, Some(count_fork)) };
        assert_eq!(registered, 0, "armor: pthread_atfork failed");
    });
    FORKS.load(Ordering::Relaxed)
}

thread_local! {
    /// The fault table of the `Jit` that runs translated code on this
    /// thread (`Jit::faults`), for `on_fault`.
    static FAULTS: std::cell::Cell<*const Vec<(u64, u64)>> =
        const { std::cell::Cell::new(std::ptr::null()) };
}

/// ADR-0046: SIGSEGV and SIGBUS from a guest load of translated code that
/// the host checks resume at that instruction's slow path, where the
/// interpreter runs it again (and raises the guest's signal, or does the
/// access); the guest state is exactly as before the instruction, since
/// translated code changes nothing before its loads. Any other fault is
/// armor's own: the default action, as without the handler.
extern "C" fn on_fault(
    signal: libc::c_int,
    _info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    let table = FAULTS.with(|faults| faults.get());
    // SAFETY: the kernel passes the interrupted context; `table` is the
    // running `Jit`'s fault table, which nothing changes while translated
    // code runs on this thread (the engine is not running).
    unsafe {
        let context = &mut *context.cast::<libc::ucontext_t>();
        let pc = context.uc_mcontext.pc;
        if !table.is_null() {
            let faults = &*table;
            if let Ok(i) = faults.binary_search_by_key(&pc, |&(load, _)| load) {
                context.uc_mcontext.pc = faults[i].1;
                return;
            }
        }
        // Not translated code: the fault happens again, with the default
        // action.
        libc::signal(signal, libc::SIG_DFL);
    }
}

/// Installs `on_fault` for SIGSEGV and SIGBUS, once per process.
fn install_fault_handler() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        for signal in [libc::SIGSEGV, libc::SIGBUS] {
            // SAFETY: an all-zero sigaction is valid; the handler only
            // reads the thread's fault table and edits the context.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_fault as *const () as usize;
                action.sa_flags = libc::SA_SIGINFO;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
    });
}

/// Code buffer per thread: address space reserved, memory used as filled.
const BUFFER_SIZE: usize = 64 << 20;

#[derive(Clone, Copy)]
pub(crate) enum Entry {
    /// Translated code for the block.
    Code(*const u8),
    /// The first instruction is not translated: the interpreter runs it.
    Interpret,
    /// Not translated yet: the interpreter ran the block this many times
    /// (`Jit::warm_up`).
    Cold(u32),
}

/// Hashes guest addresses: one multiplication instead of SipHash, which
/// cost more than the rest of the lookup (the engine looks up every
/// interpreted instruction). Keys come from the guest, but a guest that
/// makes its own lookups slow only slows itself.
#[derive(Default)]
struct RipHasher(u64);

impl Hasher for RipHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u64(&mut self, rip: u64) {
        // Fibonacci hashing; the shift brings the well-mixed high bits
        // down to the low bits the table indexes with.
        let h = (self.0 ^ rip).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = h ^ (h >> 32);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Entries of the direct-mapped cache in front of `Jit::blocks`.
const RECENT_ENTRIES: usize = 1 << 14;
/// Marks an empty slot of the cache: no guest address is above 2^47.
const NO_RIP: u64 = u64::MAX;

pub struct Jit {
    buffer: CodeBuffer,
    blocks: HashMap<u64, Entry, BuildHasherDefault<RipHasher>>,
    /// The recently used entries of `blocks`, by address: one comparison
    /// where the hash table takes a probe (measured: the table lookup was
    /// a third of the time on c/malloc, where every SSE instruction is
    /// interpreted and looked up).
    recent: Box<[(u64, Entry)]>,
    /// The code generation the blocks were translated in.
    generation: u64,
    /// MXCSR's control bits had their default value (`INITIAL_MXCSR`)
    /// when the blocks were translated: only then does the translator
    /// handle SSE floating point (ADR-0024; `sync_mxcsr`).
    default_mxcsr: bool,
    /// `forks()` when `buffer` was made (see the module comment on fork).
    forks: u64,
    /// Lockstep mode (`lockstep.rs`): one instruction per block, each
    /// checked against the interpreter.
    lockstep: bool,
    /// Counts `flush`es: a chainable exit is only patched if the buffer
    /// was not emptied since its block ran.
    epoch: u64,
    context: Context,
    /// `enter_routine` in this buffer.
    enter: *const u8,
    /// Blocks called by the engine (chained ones not counted): tests use
    /// it to check that chaining happens.
    pub(crate) calls: u64,
    /// The indirect-jump table that generated code reads
    /// (`Context::targets`): (guest address, chained entry) by
    /// `target_slot`. Never moves; emptied with the buffer.
    targets: Box<[[u64; 2]]>,
    /// The guest's mode (ADR-0029): the blocks are decoded and translated
    /// for it. A process has one; a change empties the translations.
    mode: Mode,
    /// The host CPU's extensions the translations may use (ADR-0032).
    features: HostFeatures,
    /// ADR-0046: the guest loads of the translated code that the host
    /// checks, as (executable address of the load, of its slow path),
    /// sorted (the buffer only grows within an epoch). The fault handler
    /// reads it (`on_fault`, through `FAULTS`).
    faults: Vec<(u64, u64)>,
    /// How many times the interpreter runs a block before it is
    /// translated: code that runs only a few times (a program's startup)
    /// costs less interpreted than translated. 0 translates at once.
    warm_up: u32,
}

/// A translated block: `extern "C" fn(cpu, page_table, context)
/// -> status`.
/// `enter_routine`: `enter(cpu, page_table, context, block)`.
type EnterFn = unsafe extern "C" fn(*mut Cpu, *const u8, *mut Context, *const u8) -> u64;

impl Jit {
    pub fn new() -> io::Result<Jit> {
        Jit::with_mode(false)
    }

    /// A JIT that checks every translated instruction against the
    /// interpreter (`lockstep.rs`): slow, for finding translator bugs.
    pub fn lockstep() -> io::Result<Jit> {
        Jit::with_mode(true)
    }

    fn with_mode(lockstep: bool) -> io::Result<Jit> {
        Ok(Jit {
            buffer: CodeBuffer::new(BUFFER_SIZE)?,
            blocks: HashMap::default(),
            recent: vec![(NO_RIP, Entry::Interpret); RECENT_ENTRIES].into_boxed_slice(),
            generation: 0,
            default_mxcsr: true,
            forks: forks(),
            lockstep,
            epoch: 0,
            context: Context::default(),
            enter: std::ptr::null(),
            calls: 0,
            targets: vec![[NO_RIP, 0]; TARGET_ENTRIES].into_boxed_slice(),
            mode: Mode::Long,
            features: HostFeatures::detect(),
            warm_up: 0,
            faults: Vec::new(),
        })
        .map(|mut jit| {
            jit.context.targets = jit.targets.as_ptr() as u64;
            jit.context.flag_table = FLAG_TABLE.as_ptr() as u64;
            jit.publish_routines();
            jit
        })
    }

    /// Translates for a CPU with only `features` (tests: the ARMv8.0
    /// paths on any host).
    pub fn set_features(&mut self, features: HostFeatures) {
        self.features = features;
        self.flush();
    }

    /// Blocks are translated after the interpreter has run them `runs`
    /// times (0: at once, the default). Lockstep translates at once.
    pub fn set_warm_up(&mut self, runs: u32) {
        if !self.lockstep {
            self.warm_up = runs;
        }
    }

    /// Runs about `budget` guest instructions, like `Cpu::run`: stops
    /// early at a syscall (`Step::Syscall`, with rip after it) or an error.
    pub fn run(&mut self, cpu: &mut Cpu, mem: &MemView, budget: u64) -> Result<Step, CpuError> {
        if FORKS.load(Ordering::Relaxed) != self.forks {
            // A forked child: the parent still uses the old buffer.
            *self = Jit::with_mode(self.lockstep).unwrap_or_else(|e| {
                panic!("armor: cannot allocate the JIT's code buffer after fork: {e}")
            });
        }
        if cpu.mode != self.mode {
            self.mode = cpu.mode;
            self.flush();
        }
        if self.lockstep {
            return crate::lockstep::run(self, cpu, mem, budget);
        }
        self.sync_mxcsr(cpu);
        let mut budget = i64::try_from(budget).unwrap_or(i64::MAX);
        while budget > 0 {
            let interpret = match self.entry(cpu.rip, mem) {
                Entry::Interpret => true,
                Entry::Cold(_) => {
                    // The block, interpreted until control leaves its
                    // straight line.
                    for _ in 0..MAX_BLOCK_INSTRUCTIONS {
                        let rip = cpu.rip;
                        budget -= 1;
                        let step = cpu.step(mem)?;
                        self.sync_mxcsr(cpu);
                        if step == Step::Syscall {
                            return Ok(Step::Syscall);
                        }
                        let ahead = cpu.rip.wrapping_sub(rip);
                        if ahead == 0 || ahead > MAX_INSTRUCTION_LEN as u64 || budget <= 0 {
                            break;
                        }
                    }
                    continue;
                }
                Entry::Code(code) => {
                    let epoch = self.epoch;
                    let status;
                    (status, budget) = self.execute(code, cpu, mem, budget);
                    if status == EXIT_CHAIN {
                        self.chain(self.context.site, epoch, cpu.rip, mem);
                    }
                    self.context.exit_kind = 0;
                    if status == EXIT_SYSCALL {
                        return Ok(Step::Syscall);
                    }
                    status == EXIT_INTERPRET
                }
            };
            if interpret {
                budget -= 1;
                if crate::stats::counting_interpreted() {
                    let mut window = [0u8; MAX_INSTRUCTION_LEN];
                    let available = mem.fetch(cpu.rip, &mut window);
                    crate::stats::record_interpreted(
                        decode_in(&window[..available], cpu.mode).ok().as_ref(),
                    );
                }
                let step = cpu.step(mem)?;
                // ldmxcsr and fxrstor run here.
                self.sync_mxcsr(cpu);
                if step == Step::Syscall {
                    return Ok(Step::Syscall);
                }
            }
        }
        Ok(Step::Continue)
    }

    /// Drops every block when MXCSR's control bits move to or from their
    /// default: translated SSE floating point assumes round to nearest,
    /// no FZ or DAZ and every exception masked, and is translated only
    /// then; otherwise the interpreter runs it (ADR-0024). Translated code
    /// never changes the control bits, so checking after the interpreter
    /// and at each `run` (signal delivery, sigreturn) is enough.
    pub(crate) fn sync_mxcsr(&mut self, cpu: &Cpu) {
        let default = cpu.mxcsr & MXCSR_CONTROL == INITIAL_MXCSR;
        if default != self.default_mxcsr {
            self.flush();
            self.default_mxcsr = default;
        }
    }

    /// Rewrites the exit at `site` (from a block that ran in `epoch`) to
    /// jump straight into the block at `target`, translating it if needed.
    fn chain(&mut self, site: u64, epoch: u64, target: u64, mem: &MemView) {
        // A block still warming up is counted when the engine runs it.
        if let Some(Entry::Cold(_)) = self.blocks.get(&target) {
            return;
        }
        let Entry::Code(code) = self.entry(target, mem) else {
            return;
        };
        if self.epoch != epoch {
            // The buffer was emptied (new generation, or full): the exit
            // is gone with it.
            return;
        }
        // Bit 0: a jump forward, past the target's budget check.
        let offset = if site & 1 != 0 {
            UNCHECKED_ENTRY
        } else {
            CHAINED_ENTRY
        };
        let site = (site & !1) as *const u8;
        let entry = code.wrapping_add(offset);
        let word = branch_word(site as usize, entry as usize)
            .expect("the code buffer is smaller than the reach of B");
        assert!(
            self.buffer.contains(site),
            "armor bug: chaining an exit outside the JIT's buffer"
        );
        // SAFETY: `site` is the `b` of a chainable exit in code published
        // in this epoch (checked above), and no guest code runs while the
        // engine does. The new branch targets the chained entry of a block
        // of the same epoch, which starts with the state every block keeps
        // in registers (translator, "Chaining").
        unsafe { self.buffer.patch(site, &[word]) };
    }

    /// The block at `rip`, translated if needed. Drops every block first
    /// if the code generation changed.
    pub(crate) fn entry(&mut self, rip: u64, mem: &MemView) -> Entry {
        let generation = mem.code_generation();
        if generation != self.generation {
            self.flush();
            self.generation = generation;
        }
        let slot = ((rip ^ (rip >> 14)) as usize) % RECENT_ENTRIES;
        let entry = match self.recent[slot] {
            (cached, entry) if cached == rip => entry,
            _ => {
                let entry = match self.blocks.get(&rip) {
                    Some(&Entry::Cold(runs)) if runs + 1 < self.warm_up => Entry::Cold(runs + 1),
                    Some(&Entry::Cold(_)) => self.translate(rip, mem),
                    Some(&entry) => entry,
                    None if self.warm_up > 0 => Entry::Cold(0),
                    None => self.translate(rip, mem),
                };
                self.blocks.insert(rip, entry);
                // Cold blocks stay out of the cache: their count is in
                // `blocks`.
                if !matches!(entry, Entry::Cold(_)) {
                    self.recent[slot] = (rip, entry);
                }
                entry
            }
        };
        if let Entry::Code(code) = entry {
            // Indirect jumps to `rip` can now go straight there.
            let chained = code.wrapping_add(CHAINED_ENTRY) as u64;
            self.targets[target_slot(rip)] = [rip, chained];
        }
        entry
    }

    /// Runs a block returned by `entry` (with no mapping change since)
    /// with `budget` guest instructions, and returns its exit status and
    /// what is left of the budget.
    pub(crate) fn execute(
        &mut self,
        code: *const u8,
        cpu: &mut Cpu,
        mem: &MemView,
        budget: i64,
    ) -> (u64, i64) {
        debug_assert!(
            self.buffer.contains(code),
            "armor bug: running code outside the JIT's buffer"
        );
        self.context.budget = budget;
        self.calls += 1;
        // For a fault in this thread's translated code (ADR-0046).
        FAULTS.with(|faults| faults.set(&self.faults));
        // SAFETY: `code` comes from `entry`, which published it in the
        // current code generation (the view keeps the mappings, hence the
        // generation, from changing), so it is a complete function
        // following the procedure call standard. It, and every block it is
        // chained to or reaches through the indirect-jump table (all from
        // the same epoch of the buffer: `chain` checks it, `flush` empties
        // the table), touches only: the `Cpu` and `Context` it is given; the page
        // table and the flag table (read-only); and guest memory, after
        // checking the page table (ADR-0021).
        let status = unsafe {
            clear_fpsr();
            let enter: EnterFn = std::mem::transmute(self.enter);
            enter(cpu, mem.page_table(), &mut self.context, code)
        };
        cpu.mxcsr |= mxcsr_flags(read_fpsr());
        (status, self.context.budget)
    }

    /// Checks that every entry of the indirect-jump table points at the
    /// current translation of its address (tests).
    #[cfg(test)]
    pub(crate) fn check_targets(&self) -> Result<(), String> {
        for &[rip, chained] in self.targets.iter() {
            if rip == NO_RIP {
                continue;
            }
            match self.blocks.get(&rip) {
                Some(Entry::Code(code)) if code.wrapping_add(CHAINED_ENTRY) as u64 == chained => {}
                _ => return Err(format!("stale indirect-jump entry for {rip:#x}")),
            }
        }
        Ok(())
    }

    fn flush(&mut self) {
        self.epoch += 1;
        self.blocks.clear();
        self.recent.fill((NO_RIP, Entry::Interpret));
        self.targets.fill([NO_RIP, 0]);
        self.faults.clear();
        self.buffer.reset();
        self.publish_routines();
    }

    /// Puts the shared routines at the start of the (empty) buffer:
    /// `flags_routine` and `leave_routine`, which blocks find through the
    /// `Context`, and `enter_routine`, through which `execute` runs them.
    fn publish_routines(&mut self) {
        let compat = self.mode == Mode::Compat32;
        let mut publish = |words: Vec<u32>| {
            self.buffer
                .publish(&words)
                .expect("the routines fit in an empty buffer")
        };
        let flags = publish(flags_routine().expect("the flags routine encodes"));
        let enter = publish(enter_routine(compat).expect("the entry routine encodes"));
        let leave = publish(leave_routine(compat).expect("the exit routine encodes"));
        self.context.flags_routine = flags as u64;
        self.context.leave = leave as u64;
        self.enter = enter;
    }

    /// Decodes and translates the block at `rip`.
    fn translate(&mut self, rip: u64, mem: &MemView) -> Entry {
        let start = Instant::now();
        // In concurrent mode (ADR-0023: other threads or processes write the
        // same memory) accesses are ordered and atomic instructions use LSE
        // atomics; those without one go to the interpreter, which makes
        // them atomic on the host (`supported_concurrent`).
        let concurrent = mem.is_concurrent();
        let host_reads = mem.host_protection().map(|end| {
            install_fault_handler();
            64 - (end - 1).leading_zeros()
        });
        let options = Options {
            features: self.features,
            chain: !self.lockstep,
            concurrent,
            compat: self.mode == Mode::Compat32,
            host_reads,
            page_table: true,
            // What the exit that led here left pending (after a chain
            // exit; 0 otherwise).
            entry_kind: core::mem::take(&mut self.context.exit_kind),
        };
        // ADR-0048: a translation of the same bytes, at the same address,
        // with the same options, from an earlier run (not in lockstep).
        let key = self.cache_key(&options);
        let cached = !self.lockstep && cache::enabled();
        if cached && let Some(hit) = cache::lookup(rip, key, |at, buf| mem.fetch(at, buf)) {
            return self.publish_translation(
                rip,
                &hit.words,
                &hit.faults,
                hit.instructions as usize,
            );
        }
        let mut block = Vec::with_capacity(MAX_BLOCK_INSTRUCTIONS);
        let mut at = rip;
        let limit = if self.lockstep {
            1
        } else {
            MAX_BLOCK_INSTRUCTIONS
        };
        let end = loop {
            if block.len() == limit {
                break BlockEnd::Continue(at);
            }
            let mut window = [0u8; MAX_INSTRUCTION_LEN];
            let available = mem.fetch(at, &mut window);
            match decode_in(&window[..available], self.mode) {
                Ok(insn)
                    if (if concurrent {
                        supported_concurrent(&insn)
                    } else {
                        supported(&insn)
                    }) && (self.default_mxcsr || !uses_mxcsr_control(&insn)) =>
                {
                    block.push(GuestInstruction { rip: at, insn });
                    at = at.wrapping_add(insn.len as u64);
                    if ends_block(&insn) {
                        // Unused: the block ends with its own exits.
                        break BlockEnd::Continue(at);
                    }
                }
                // Not translated, or an error the interpreter reports.
                _ => break BlockEnd::Interpret(at),
            }
        };
        if block.is_empty() {
            return Entry::Interpret;
        }
        let translation = translate_block(&block, end, options)
            .unwrap_or_else(|e| panic!("armor bug: cannot encode the block at {rip:#x}: {e}"));
        crate::stats::record(block.len(), translation.words.len(), start.elapsed());
        if cached {
            let last = block.last().expect("a block has an instruction");
            let len = last.rip.wrapping_sub(rip) as usize + last.insn.len;
            let mut bytes = vec![0; len];
            if mem.fetch(rip, &mut bytes) == len {
                cache::insert(
                    rip,
                    key,
                    &bytes,
                    &translation.words,
                    &translation.faults,
                    block.len(),
                );
            }
        }
        self.publish_translation(rip, &translation.words, &translation.faults, block.len())
    }

    /// Publishes a translation of the block at `rip` (`instructions` guest
    /// instructions), with its fault table.
    fn publish_translation(
        &mut self,
        rip: u64,
        words: &[u32],
        faults: &[(u32, u32)],
        instructions: usize,
    ) -> Entry {
        let code = match self.buffer.publish(words) {
            Ok(code) => code,
            Err(_) => {
                // Full: start over with an empty buffer.
                self.flush();
                self.buffer
                    .publish(words)
                    .expect("one block fits in an empty buffer")
            }
        };
        let at = |offset: u32| code as u64 + 4 * u64::from(offset);
        self.faults
            .extend(faults.iter().map(|&(load, slow)| (at(load), at(slow))));
        crate::stats::perf_map(code, words.len() * 4, rip, instructions);
        Entry::Code(code)
    }

    /// What a translation depends on besides its bytes and address: the
    /// options (but the entry kind, a hint whose code is correct for any
    /// kind), MXCSR's default (which decides what is translated) and the
    /// CPUID features declared on request (which decide how some
    /// instructions run; none by default, which leaves the key as it was).
    fn cache_key(&self, options: &Options) -> u64 {
        let mut bits = [
            u8::from(options.features.lse),
            u8::from(options.features.rcpc),
            u8::from(options.chain),
            u8::from(options.concurrent),
            u8::from(options.compat),
            options.host_reads.map_or(0, |b| b as u8),
            u8::from(options.page_table),
            u8::from(self.default_mxcsr),
            0,
            0,
            0,
            0,
        ];
        let declared = armor_cpu::cpuid::declared();
        if declared == armor_cpu::cpuid::Features::NONE {
            return cache::fnv1a(&bits[..8]);
        }
        bits[8..].copy_from_slice(&declared.bits().to_le_bytes());
        cache::fnv1a(&bits)
    }
}
