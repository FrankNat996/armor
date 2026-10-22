//! The JIT's engine in a Wine process (ADR-0047): the translator of
//! `armor-jit` (compatibility mode for the WoW64 backend, 64-bit mode for
//! the ARM64EC one; reads and writes checked by the host's protections,
//! which Wine keeps as the guest's) around a code buffer from
//! `NtAllocateVirtualMemory`, as `armor-jit`'s Linux engine does around
//! its own: a block cache, chained exits, the indirect-jump table, the
//! interpreter for what the translator does not handle. A fault in
//! translated code resumes at the instruction's slow path
//! (`slow_path`, from the backend's exception hook).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_void;

use armor_cpu::{Cpu, CpuError, INITIAL_MXCSR, MXCSR_CONTROL, Step};
use armor_guest::GuestMemory;
use armor_jit::encoder::branch_word;
use armor_jit::translator::{
    BlockEnd, CHAINED_ENTRY, Context, EXIT_CHAIN, EXIT_INTERPRET, EXIT_SYSCALL, FLAG_TABLE,
    GuestInstruction, HostFeatures, MAX_BLOCK_INSTRUCTIONS, Options, TARGET_ENTRIES,
    UNCHECKED_ENTRY, ends_block, enter_routine, flags_routine, leave_routine, supported,
    supported_concurrent, target_slot, translate_block, uses_mxcsr_control,
};
use armor_x86::{MAX_INSTRUCTION_LEN, Mode, decode_in};

use crate::nt;

/// The code buffer per thread: reserved and committed at once (Wine
/// commits lazily on Linux).
const BUFFER_SIZE: usize = 64 << 20;
const NO_RIP: u64 = u64::MAX;

#[derive(Clone, Copy)]
enum Entry {
    Code(*const u8),
    Interpret,
}

/// `enter_routine`: `enter(cpu, page_table, context, block)`.
#[cfg(not(target_arch = "arm64ec"))]
type EnterFn = unsafe extern "C" fn(*mut Cpu, *const u8, *mut Context, *const u8) -> u64;

pub struct Engine {
    base: *mut u8,
    used: usize,
    blocks: BTreeMap<u64, Entry>,
    targets: Box<[[u64; 2]]>,
    /// (executable address of a guest access, of its slow path), sorted.
    faults: Vec<(u64, u64)>,
    context: Context,
    /// `enter_routine` in the buffer.
    enter: *const u8,
    generation: u64,
    epoch: u64,
    default_mxcsr: bool,
    concurrent: bool,
    /// 32-bit code (WoW64) rather than 64-bit code (ARM64EC).
    compat: bool,
    features: HostFeatures,
}

/// Calls `enter_routine` for the block at `code`.
///
/// # Safety
/// `enter` and `code` are this epoch's routine and block.
#[cfg(not(target_arch = "arm64ec"))]
unsafe fn enter(enter: *const u8, cpu: *mut Cpu, context: *mut Context, code: *const u8) -> u64 {
    // SAFETY: the caller's contract; the routine follows the C ABI.
    unsafe {
        let enter: EnterFn = core::mem::transmute(enter);
        enter(cpu, core::ptr::null(), context, code)
    }
}

/// ARM64EC: a call through a pointer goes through the loader's call
/// checker, which would take the code buffer (not ARM64EC code in the
/// PEB's bitmap) for x86-64 code; the routine is called directly. It
/// follows the C ABI (x19–x30 saved), and uses only registers the C ABI
/// lets a callee change besides those, and x18: in 64-bit mode r10 lives
/// there while translated code runs, and the TEB is put back after.
///
/// # Safety
/// As above.
#[cfg(target_arch = "arm64ec")]
unsafe fn enter(enter: *const u8, cpu: *mut Cpu, context: *mut Context, code: *const u8) -> u64 {
    let status: u64;
    // SAFETY: the caller's contract; the TEB waits on the stack.
    unsafe {
        core::arch::asm!(
            "str x18, [sp, #-16]!",
            "blr {enter}",
            "ldr x18, [sp], #16",
            enter = in(reg) enter,
            inlateout("x0") cpu => status,
            in("x1") 0u64,
            in("x2") context,
            in("x3") code,
            clobber_abi("C"),
        )
    };
    status
}

fn clear_fpsr() {
    // SAFETY: writes FPSR only.
    unsafe { core::arch::asm!("msr fpsr, xzr", options(nomem, nostack)) };
}

fn read_fpsr() -> u64 {
    let fpsr: u64;
    // SAFETY: reads FPSR only.
    unsafe { core::arch::asm!("mrs {}, fpsr", out(reg) fpsr, options(nomem, nostack)) };
    fpsr
}

/// MXCSR's exception flags from FPSR's (as `armor-jit`'s engine).
fn mxcsr_flags(fpsr: u64) -> u32 {
    let fpsr = fpsr as u32;
    (fpsr & 1) | ((fpsr & 0x1e) << 1)
}

/// The instruction cache sees `len` bytes written at `at`.
fn sync_caches(at: *const u8, len: usize) {
    let (start, end) = (at as usize & !63, at as usize + len);
    let mut line = start;
    while line < end {
        // SAFETY: cache maintenance of the buffer's own lines.
        unsafe { core::arch::asm!("dc cvau, {}", in(reg) line, options(nostack)) };
        line += 64;
    }
    // SAFETY: barriers and invalidation, as the architecture requires
    // after writing code.
    unsafe { core::arch::asm!("dsb ish", options(nostack)) };
    let mut line = start;
    while line < end {
        // SAFETY: as above.
        unsafe { core::arch::asm!("ic ivau, {}", in(reg) line, options(nostack)) };
        line += 64;
    }
    // SAFETY: as above.
    unsafe { core::arch::asm!("dsb ish", "isb", options(nostack)) };
}

impl Engine {
    pub fn new(compat: bool) -> Option<Engine> {
        let mut base: *mut c_void = core::ptr::null_mut();
        let mut size = BUFFER_SIZE;
        // SAFETY: a new region for generated code.
        #[cfg(not(target_arch = "arm64ec"))]
        let status = unsafe {
            nt::NtAllocateVirtualMemory(
                -1isize as nt::Handle,
                &mut base,
                0,
                &mut size,
                nt::MEM_RESERVE | nt::MEM_COMMIT,
                nt::PAGE_EXECUTE_READWRITE,
            )
        };
        // ARM64EC: marked as ARM64EC code (the PEB's bitmap). Otherwise the
        // system takes the buffer for x86-64 code: `NtContinue` into a
        // slow path, after a fault, would run the emulator there.
        // SAFETY: as above.
        #[cfg(target_arch = "arm64ec")]
        let status = unsafe {
            let mut ec = nt::MemExtendedParameter {
                kind: nt::MEM_EXTENDED_PARAMETER_ATTRIBUTE_FLAGS,
                value: nt::MEM_EXTENDED_PARAMETER_EC_CODE,
            };
            nt::NtAllocateVirtualMemoryEx(
                -1isize as nt::Handle,
                &mut base,
                &mut size,
                nt::MEM_RESERVE | nt::MEM_COMMIT,
                nt::PAGE_EXECUTE_READWRITE,
                &mut ec,
                1,
            )
        };
        if status != nt::STATUS_SUCCESS {
            return None;
        }
        let mut engine = Engine {
            base: base.cast(),
            used: 0,
            blocks: BTreeMap::new(),
            targets: vec![[NO_RIP, 0]; TARGET_ENTRIES].into_boxed_slice(),
            faults: Vec::new(),
            context: Context::default(),
            enter: core::ptr::null(),
            generation: 0,
            epoch: 0,
            default_mxcsr: true,
            concurrent: false,
            compat,
            features: HostFeatures::from_id_registers(),
        };
        engine.context.targets = engine.targets.as_ptr() as u64;
        engine.context.flag_table = FLAG_TABLE.as_ptr() as u64;
        engine.flush();
        Some(engine)
    }

    /// Whether `pc` is in the code buffer.
    pub fn contains(&self, pc: u64) -> bool {
        let base = self.base as u64;
        (base..base + BUFFER_SIZE as u64).contains(&pc)
    }

    /// The slow path of the guest access at `pc`, if it is one.
    pub fn slow_path(&self, pc: u64) -> Option<u64> {
        let i = self.faults.binary_search_by_key(&pc, |&(at, _)| at).ok()?;
        Some(self.faults[i].1)
    }

    /// Copies `words` into the buffer; `None` when it is full.
    fn publish(&mut self, words: &[u32]) -> Option<*const u8> {
        let len = words.len() * 4;
        let at = (self.used + 15) & !15;
        if at + len > BUFFER_SIZE {
            return None;
        }
        // SAFETY: inside the buffer, which nothing runs past `used`.
        let code = unsafe { self.base.add(at) };
        // SAFETY: as above.
        unsafe { core::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), code, len) };
        sync_caches(code, len);
        self.used = at + len;
        Some(code)
    }

    fn flush(&mut self) {
        self.epoch += 1;
        self.blocks.clear();
        self.targets.fill([NO_RIP, 0]);
        self.faults.clear();
        self.used = 0;
        // The shared routines.
        let flags = flags_routine(false).expect("the flags routine encodes");
        let enter = enter_routine(self.compat, false).expect("the entry routine encodes");
        let leave = leave_routine(self.compat, false).expect("the exit routine encodes");
        let flags = self.publish(&flags).expect("fits in an empty buffer");
        self.enter = self.publish(&enter).expect("fits in an empty buffer");
        let leave = self.publish(&leave).expect("fits in an empty buffer");
        self.context.flags_routine = flags as u64;
        self.context.leave = leave as u64;
    }

    /// Concurrent mode from the second thread on (ADR-0023): ordered
    /// accesses and LSE atomics.
    pub fn set_concurrent(&mut self, concurrent: bool) {
        if concurrent != self.concurrent {
            self.concurrent = concurrent;
            self.flush();
        }
    }

    fn sync_mxcsr(&mut self, cpu: &Cpu) {
        let default = cpu.mxcsr & MXCSR_CONTROL == INITIAL_MXCSR;
        if default != self.default_mxcsr {
            self.flush();
            self.default_mxcsr = default;
        }
    }

    /// Runs about `budget` instructions; stops at what the interpreter
    /// reports (`int 0x2e` at the stubs, an exception), the instruction's
    /// address in `rip`, at a `syscall` (64-bit mode), and before an
    /// address `outside` names (ARM64EC code, which the backend runs
    /// natively).
    pub fn run(
        &mut self,
        cpu: &mut Cpu,
        mem: &impl GuestMemory,
        budget: i64,
        rip: *mut u64,
        running: *mut bool,
        outside: impl Fn(u64) -> bool,
    ) -> Result<Step, CpuError> {
        let generation = mem.code_generation();
        if generation != self.generation {
            self.flush();
            self.generation = generation;
        }
        self.sync_mxcsr(cpu);
        let mut budget = budget;
        while budget > 0 {
            if outside(cpu.rip) {
                break;
            }
            let interpret = match self.entry(cpu.rip, mem) {
                Entry::Interpret => true,
                Entry::Code(code) => {
                    let epoch = self.epoch;
                    self.context.budget = budget;
                    // SAFETY: a complete block of this epoch's buffer; it
                    // touches the `Cpu`, the `Context`, the flag table and
                    // guest memory, whose faults resume at slow paths.
                    let status = unsafe {
                        clear_fpsr();
                        enter(self.enter, cpu, &mut self.context, code)
                    };
                    cpu.mxcsr |= mxcsr_flags(read_fpsr());
                    budget = self.context.budget;
                    if status == EXIT_CHAIN && !outside(cpu.rip) {
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
                // Volatile: an exception handler reads them while the
                // interpreter runs (`BTCpuResetToConsistentState`).
                // SAFETY: the caller's thread state.
                unsafe {
                    core::ptr::write_volatile(rip, cpu.rip);
                    core::ptr::write_volatile(running, true);
                }
                let step = cpu.step(mem);
                // SAFETY: as above.
                unsafe { core::ptr::write_volatile(running, false) };
                self.sync_mxcsr(cpu);
                match step {
                    Ok(Step::Continue) => {}
                    other => return other,
                }
            }
        }
        Ok(Step::Continue)
    }

    fn chain(&mut self, site: u64, epoch: u64, target: u64, mem: &impl GuestMemory) {
        let Entry::Code(code) = self.entry(target, mem) else {
            return;
        };
        if self.epoch != epoch {
            return;
        }
        // Bit 0: a jump forward, past the target's budget check.
        let offset = if site & 1 != 0 {
            UNCHECKED_ENTRY
        } else {
            CHAINED_ENTRY
        };
        let site = site & !1;
        let entry = code.wrapping_add(offset);
        let word = branch_word(site as usize, entry as usize).expect("within the buffer");
        // SAFETY: the exit's `b`, in this epoch's buffer; no guest code
        // runs while the engine does.
        unsafe { core::ptr::write_volatile(site as *mut u32, word) };
        sync_caches(site as *const u8, 4);
    }

    fn entry(&mut self, rip: u64, mem: &impl GuestMemory) -> Entry {
        let entry = match self.blocks.get(&rip) {
            Some(&entry) => entry,
            None => {
                let entry = self.translate(rip, mem);
                self.blocks.insert(rip, entry);
                entry
            }
        };
        if let Entry::Code(code) = entry {
            self.targets[target_slot(rip)] = [rip, code.wrapping_add(CHAINED_ENTRY) as u64];
        }
        entry
    }

    fn translate(&mut self, rip: u64, mem: &impl GuestMemory) -> Entry {
        let mut block = Vec::with_capacity(MAX_BLOCK_INSTRUCTIONS);
        let mut at = rip;
        let end = loop {
            if block.len() == MAX_BLOCK_INSTRUCTIONS {
                break BlockEnd::Continue(at);
            }
            let mut window = [0u8; MAX_INSTRUCTION_LEN];
            let available = mem.fetch(at, &mut window);
            let mode = if self.compat {
                Mode::Compat32
            } else {
                Mode::Long
            };
            match decode_in(&window[..available], mode) {
                Ok(insn)
                    if (if self.concurrent {
                        supported_concurrent(&insn)
                    } else {
                        supported(&insn)
                    }) && (self.default_mxcsr || !uses_mxcsr_control(&insn)) =>
                {
                    block.push(GuestInstruction { rip: at, insn });
                    at += insn.len as u64;
                    if self.compat {
                        at &= 0xffff_ffff;
                    }
                    if ends_block(&insn) {
                        break BlockEnd::Continue(at);
                    }
                }
                _ => break BlockEnd::Interpret(at),
            }
        };
        if block.is_empty() {
            return Entry::Interpret;
        }
        let options = Options {
            features: self.features,
            chain: true,
            concurrent: self.concurrent,
            compat: self.compat,
            // The host's protections are the guest's. In compatibility
            // mode addresses wrap at 4 GiB: every one passes the region
            // test; in 64-bit mode those above the user half (2^47) go to
            // the interpreter, which faults as the guest.
            host_reads: Some(if self.compat { 32 } else { 47 }),
            page_table: false,
            kind_register: false,
            fast_syscalls: false,
            entry_kind: core::mem::take(&mut self.context.exit_kind),
        };
        let Ok(translation) = translate_block(&block, end, options) else {
            return Entry::Interpret;
        };
        let code = match self.publish(&translation.words) {
            Some(code) => code,
            None => {
                self.flush();
                match self.publish(&translation.words) {
                    Some(code) => code,
                    None => return Entry::Interpret,
                }
            }
        };
        let at = |offset: u32| code as u64 + 4 * u64::from(offset);
        self.faults.extend(
            translation
                .faults
                .iter()
                .map(|&(load, slow)| (at(load), at(slow))),
        );
        Entry::Code(code)
    }
}
