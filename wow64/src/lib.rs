//! armor's engine as Wine's WoW64 CPU backend (ADR-0047), stage 1: the
//! interpreter. Wine's `wow64.dll` loads this DLL (with Hangover:
//! `HODLL=armor-wow64.dll`) in the 64-bit side of a process that runs a
//! 32-bit x86 Windows program, and calls its `BTCpu*` functions: the
//! guest's code runs in armor's interpreter (`armor-cpu`, compatibility
//! mode), and its system calls go to `Wow64SystemServiceEx`.
//!
//! The protocol, from Wine's `dlls/wow64/syscall.c`:
//! - the guest's 32-bit ntdll enters the system through
//!   `__wine_syscall_dispatcher`, which Wine points at the address
//!   `BTCpuGetBopCode` returns; the unix calls through the address
//!   `__wine_get_unix_opcode` returns. Both are `int 0x2e` in a page this
//!   DLL allocates below 4 GiB, recognized by their address;
//! - the guest's registers live in the thread's WOW64 CPU area
//!   (`RtlWow64GetCurrentCpuArea`, an x86 `CONTEXT`): `BTCpuSimulate`
//!   loads them, runs, and stores them back around every system call,
//!   since Wine reads and changes them there (thread start, exceptions,
//!   `NtContinue`).

#![no_std]

extern crate alloc;

mod jit;
mod nt;
mod rt;

use alloc::boxed::Box;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use armor_cpu::{Cpu, CpuError, Exception, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP, Step};
use armor_guest::{GuestMemory, MemFault};

/// `__wine_dbg_output` of a formatted line.
pub(crate) fn log(args: core::fmt::Arguments<'_>) {
    let mut line = alloc::format!("{args}\n");
    line.push('\0');
    // SAFETY: a NUL-terminated string.
    unsafe { nt::__wine_dbg_output(line.as_ptr()) };
}

/// The page with the two `int 0x2e` stubs: the system call at its start,
/// the unix call 2 bytes further (set by `BTCpuProcessInit`).
static BOP: AtomicU64 = AtomicU64::new(0);

/// Changes when guest code may have changed (`BTCpuFlushInstructionCache2`
/// and the memory notifications): the interpreter's decoded instructions
/// follow it.
static CODE_GENERATION: AtomicU64 = AtomicU64::new(0);

// WOW64_CONTEXT (the i386 CONTEXT, `winnt.h`), packed to 4.
const CONTEXT_SIZE: usize = 0x2cc;
const CTX_EDI: usize = 0x9c;
const CTX_ESI: usize = 0xa0;
const CTX_EBX: usize = 0xa4;
const CTX_EDX: usize = 0xa8;
const CTX_ECX: usize = 0xac;
const CTX_EAX: usize = 0xb0;
const CTX_EBP: usize = 0xb4;
const CTX_EIP: usize = 0xb8;
const CTX_EFLAGS: usize = 0xc0;
const CTX_ESP: usize = 0xc4;
/// The FXSAVE image of the x87 and SSE state.
const CTX_EXTENDED: usize = 0xcc;

/// The TEB's TlsSlots, and the slots used: Wine's CPU-reserved one (1),
/// and this backend's per-thread state (18).
const TEB_TLS_SLOTS: u64 = 0x1480;
const TLS_CPU_RESERVED: u64 = 1;
const TLS_ARMOR: u64 = 18;
/// TEB64 + this offset's value = TEB32 (`WowTebOffset`).
const TEB_WOW_TEB_OFFSET: u64 = 0x180c;

/// The guest memory of the process, as it is: Wine manages it, and the
/// host's protections are the guest's.
struct ProcessMemory;

impl GuestMemory for ProcessMemory {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemFault> {
        // SAFETY: the guest's own memory; an access it may not make faults
        // as it would on Windows (stage 1: no recovery yet).
        unsafe { core::ptr::copy_nonoverlapping(addr as *const u8, buf.as_mut_ptr(), buf.len()) };
        Ok(())
    }

    fn write(&self, addr: u64, data: &[u8]) -> Result<(), MemFault> {
        // SAFETY: as `read`.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), addr as *mut u8, data.len()) };
        Ok(())
    }

    fn fetch(&self, addr: u64, buf: &mut [u8]) -> usize {
        // The bytes up to the end of the page directly; the next page's
        // only if it can be read (an instruction at the end of the code).
        let in_page = (4096 - (addr & 0xfff)) as usize;
        let first = in_page.min(buf.len());
        if self.read(addr, &mut buf[..first]).is_err() {
            return 0;
        }
        if first == buf.len() {
            return first;
        }
        let mut read = 0usize;
        // SAFETY: NtReadVirtualMemory on the current process fails instead
        // of faulting.
        let status = unsafe {
            nt::NtReadVirtualMemory(
                -1isize as nt::Handle,
                (addr + first as u64) as *const c_void,
                buf[first..].as_mut_ptr().cast(),
                buf.len() - first,
                &mut read,
            )
        };
        if status == nt::STATUS_SUCCESS {
            first + read
        } else {
            first
        }
    }

    fn code_generation(&self) -> u64 {
        CODE_GENERATION.load(Ordering::Acquire)
    }

    fn time_stamp_counter(&self) -> u64 {
        let count: u64;
        // SAFETY: reads the virtual counter, readable from user space.
        unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) count, options(nomem, nostack)) };
        count
    }
}

fn tls_slot(index: u64) -> *mut u64 {
    (nt::teb() + TEB_TLS_SLOTS + 8 * index) as *mut u64
}

/// A thread's state in the backend.
struct Thread {
    cpu: Cpu,
    /// The host context at `BTCpuSimulate`'s entry: an exception in the
    /// guest's code is raised from there, so that unwinding never walks
    /// the interpreter's frames.
    entry: nt::Context64,
    /// The guest instruction being run, while `running`.
    rip: u64,
    running: bool,
    /// The JIT, when its code buffer could be allocated.
    engine: Option<jit::Engine>,
}

/// Threads that ran guest code: from the second one on, the JIT orders
/// its accesses (concurrent mode, ADR-0023).
static THREADS: AtomicU32 = AtomicU32::new(0);

/// This thread's state, made on first use.
fn thread() -> *mut Thread {
    let slot = tls_slot(TLS_ARMOR);
    // SAFETY: the TLS slot is this backend's, per thread.
    unsafe {
        if *slot == 0 {
            *slot = Box::into_raw(Box::new(Thread {
                cpu: Cpu::new_compat(0, 0),
                entry: nt::Context64([0; 0x390]),
                rip: 0,
                running: false,
                engine: jit::Engine::new(),
            })) as u64;
            THREADS.fetch_add(1, Ordering::AcqRel);
        }
        *slot as *mut Thread
    }
}

/// Raises `code` for the guest at its instruction `rip` (the context
/// already saved), from `BTCpuSimulate`'s entry context: Wine passes it to
/// the guest (`Wow64PassExceptionToGuest`) and runs `BTCpuSimulate` again
/// at the guest's handler.
fn raise_to_guest(thread: *mut Thread, code: nt::NtStatus, rip: u64, info: &[u64]) -> ! {
    let mut record = [0u8; 152];
    let put = |record: &mut [u8; 152], at: usize, value: u64, size: usize| {
        record[at..at + size].copy_from_slice(&value.to_le_bytes()[..size]);
    };
    put(&mut record, nt::RECORD_CODE, code as u32 as u64, 4);
    put(&mut record, nt::RECORD_ADDRESS, rip, 8);
    put(&mut record, nt::RECORD_PARAMETERS, info.len() as u64, 4);
    for (i, &value) in info.iter().enumerate() {
        put(&mut record, nt::RECORD_INFORMATION + 8 * i, value, 8);
    }
    // SAFETY: a valid EXCEPTION_RECORD and the entry context.
    unsafe {
        (*thread).running = false;
        nt::NtRaiseException(
            record.as_mut_ptr().cast(),
            (&raw mut (*thread).entry).cast(),
            1,
        );
    }
    // NtRaiseException does not return when it dispatches.
    // SAFETY: ends the process.
    unsafe { nt::NtTerminateProcess(-1isize as nt::Handle, code) };
    loop {
        core::hint::spin_loop();
    }
}

/// The thread's x86 CONTEXT in its CPU area.
fn context() -> *mut u8 {
    let mut ctx: *mut c_void = core::ptr::null_mut();
    // SAFETY: Wine keeps the CPU area for the thread's life.
    unsafe {
        nt::RtlWow64GetCurrentCpuArea(core::ptr::null_mut(), &mut ctx, core::ptr::null_mut())
    };
    ctx.cast()
}

fn ctx_u32(ctx: *mut u8, offset: usize) -> u64 {
    // SAFETY: inside the CONTEXT (packed: unaligned reads).
    u64::from(unsafe { core::ptr::read_unaligned(ctx.add(offset).cast::<u32>()) })
}

fn set_ctx_u32(ctx: *mut u8, offset: usize, value: u64) {
    // SAFETY: as `ctx_u32`.
    unsafe { core::ptr::write_unaligned(ctx.add(offset).cast::<u32>(), value as u32) };
}

const REGISTERS: [(usize, usize); 8] = [
    (RAX, CTX_EAX),
    (RCX, CTX_ECX),
    (RDX, CTX_EDX),
    (RBX, CTX_EBX),
    (RSP, CTX_ESP),
    (RBP, CTX_EBP),
    (RSI, CTX_ESI),
    (RDI, CTX_EDI),
];

/// The guest state from the CONTEXT.
fn load_context(cpu: &mut Cpu, ctx: *mut u8) {
    for (reg, offset) in REGISTERS {
        cpu.regs[reg] = ctx_u32(ctx, offset);
    }
    cpu.rip = ctx_u32(ctx, CTX_EIP);
    cpu.rflags = ctx_u32(ctx, CTX_EFLAGS) | 2;
    let mut image = [0u8; 512];
    // SAFETY: the 512 bytes of ExtendedRegisters.
    unsafe { core::ptr::copy_nonoverlapping(ctx.add(CTX_EXTENDED), image.as_mut_ptr(), 512) };
    if let Err(why) = cpu.load_fx_image(&image) {
        log(format_args!("armor-wow64: FPU state not loaded: {why}"));
    }
    // fs is the 32-bit TEB; the other segments are flat.
    let teb = nt::teb();
    // SAFETY: the TEB's WowTebOffset field.
    let offset = unsafe { *((teb + TEB_WOW_TEB_OFFSET) as *const i32) };
    cpu.fs_base = teb.wrapping_add(offset as i64 as u64) & 0xffff_ffff;
}

/// The guest state into the CONTEXT.
fn save_context(cpu: &Cpu, ctx: *mut u8) {
    for (reg, offset) in REGISTERS {
        set_ctx_u32(ctx, offset, cpu.regs[reg]);
    }
    set_ctx_u32(ctx, CTX_EIP, cpu.rip);
    set_ctx_u32(ctx, CTX_EFLAGS, cpu.rflags);
    let image = cpu.fx_image();
    // SAFETY: as in `load_context`.
    unsafe { core::ptr::copy_nonoverlapping(image.as_ptr(), ctx.add(CTX_EXTENDED), 512) };
}

fn pop32(cpu: &mut Cpu) -> u64 {
    let esp = cpu.regs[RSP];
    let mut bytes = [0u8; 4];
    let _ = ProcessMemory.read(esp, &mut bytes);
    cpu.regs[RSP] = (esp + 4) & 0xffff_ffff;
    u64::from(u32::from_le_bytes(bytes))
}

/// A system call from the guest's ntdll (`int 0x2e` at the stub): eax the
/// number, the return address into the thunk on the stack, then the
/// caller's return address and the arguments.
fn system_call(cpu: *mut Cpu, ctx: *mut u8) {
    // SAFETY: this thread's state; no reference is kept across the call,
    // which may run the guest again (user callbacks nest BTCpuSimulate).
    let number = unsafe {
        let cpu = &mut *cpu;
        cpu.rip = pop32(cpu);
        save_context(cpu, ctx);
        cpu.regs[RAX] as u32
    };
    // SAFETY: Wine's CPU-reserved block: Flags first.
    unsafe {
        let reserved = *tls_slot(TLS_CPU_RESERVED) as *mut u16;
        *reserved = 0;
    }
    let args = (ctx_u32(ctx, CTX_ESP) + 4) as *mut u32;
    // SAFETY: the guest's arguments, on its stack.
    let status = unsafe { nt::Wow64SystemServiceEx(number, args) };
    // SAFETY: as above.
    unsafe {
        let cpu = &mut *cpu;
        load_context(cpu, ctx);
        cpu.regs[RAX] = u64::from(status as u32);
    }
}

/// A unix call (`int 0x2e` at the second stub): `__wine_unix_call(handle,
/// code, args)`, stdcall with a 64-bit handle.
fn unix_call(cpu: &mut Cpu) {
    let ret = pop32(cpu);
    let low = pop32(cpu);
    let high = pop32(cpu);
    let code = pop32(cpu) as u32;
    let args = pop32(cpu);
    // SAFETY: Wine's 64-bit dispatcher, with the guest's arguments.
    let status =
        unsafe { nt::__wine_unix_call_dispatcher(low | high << 32, code, args as *mut c_void) };
    cpu.regs[RAX] = u64::from(status as u32);
    cpu.rip = ret;
}

/// # Safety
/// Called by Wine once, at process start.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn BTCpuProcessInit() -> nt::NtStatus {
    let mut base: *mut c_void = core::ptr::null_mut();
    let mut size = 4096usize;
    // SAFETY: a new page, below 2 GiB (Wine stores its address in 32 bits).
    let status = unsafe {
        nt::NtAllocateVirtualMemory(
            -1isize as nt::Handle,
            &mut base,
            (1 << 31) - 1,
            &mut size,
            nt::MEM_RESERVE | nt::MEM_COMMIT,
            nt::PAGE_EXECUTE_READWRITE,
        )
    };
    if status != nt::STATUS_SUCCESS {
        return status;
    }
    // SAFETY: the page just allocated: int 0x2e, int 0x2e.
    unsafe { *(base as *mut u32) = 0x2ecd_2ecd };
    BOP.store(base as u64, Ordering::Release);
    log(format_args!(
        "armor-wow64 {}: started, stubs at {base:p}",
        env!("CARGO_PKG_VERSION")
    ));
    nt::STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn BTCpuThreadInit() {}

#[unsafe(no_mangle)]
pub extern "C" fn BTCpuGetBopCode() -> *mut c_void {
    BOP.load(Ordering::Acquire) as *mut c_void
}

#[unsafe(no_mangle)]
pub extern "C" fn __wine_get_unix_opcode() -> *mut c_void {
    (BOP.load(Ordering::Acquire) + 2) as *mut c_void
}

/// # Safety
/// Wine passes a thread handle and a WOW64_CONTEXT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn BTCpuGetContext(
    thread: nt::Handle,
    _process: nt::Handle,
    _unknown: *mut c_void,
    ctx: *mut c_void,
) -> nt::NtStatus {
    // SAFETY: Wine's contract.
    unsafe {
        nt::NtQueryInformationThread(
            thread,
            nt::THREAD_WOW64_CONTEXT,
            ctx,
            CONTEXT_SIZE as u32,
            core::ptr::null_mut(),
        )
    }
}

/// # Safety
/// As `BTCpuGetContext`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn BTCpuSetContext(
    thread: nt::Handle,
    _process: nt::Handle,
    _unknown: *mut c_void,
    ctx: *mut c_void,
) -> nt::NtStatus {
    // SAFETY: Wine's contract.
    unsafe {
        nt::NtSetInformationThread(thread, nt::THREAD_WOW64_CONTEXT, ctx, CONTEXT_SIZE as u32)
    }
}

/// Called for every host exception before it is dispatched. One raised
/// while the interpreter runs a guest instruction (an access the host
/// refuses) becomes the guest's: its context is the instruction's start
/// (armor's interpreter changes nothing before an access that can fault),
/// the record names the guest's address, and dispatching continues from
/// `BTCpuSimulate`'s entry, out of the interpreter's frames.
///
/// # Safety
/// Wine passes EXCEPTION_POINTERS.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn BTCpuResetToConsistentState(pointers: *mut *mut u8) -> nt::NtStatus {
    let slot = tls_slot(TLS_ARMOR);
    // SAFETY: this thread's state, and Wine's EXCEPTION_POINTERS.
    unsafe {
        if *slot == 0 {
            return nt::STATUS_SUCCESS;
        }
        let thread = *slot as *mut Thread;
        let host = *pointers.add(1);
        // A guest access of translated code: to its slow path, where the
        // interpreter runs the instruction (and faults as the guest's).
        if let Some(engine) = &(*thread).engine {
            let pc = core::ptr::read_unaligned(host.add(nt::CONTEXT_PC).cast::<u64>());
            if let Some(slow) = engine.slow_path(pc) {
                core::ptr::write_unaligned(host.add(nt::CONTEXT_PC).cast::<u64>(), slow);
                nt::NtContinue(host.cast(), 0);
            }
            if engine.contains(pc) {
                log(format_args!(
                    "armor-wow64: fault in translated code at {pc:#x}"
                ));
            }
        }
        if !core::ptr::read_volatile(&raw const (*thread).running) {
            return nt::STATUS_SUCCESS;
        }
        (*thread).running = false;
        (*thread).cpu.rip = (*thread).rip;
        save_context(&(*thread).cpu, context());
        let record = *pointers;
        core::ptr::write_unaligned(record.add(nt::RECORD_ADDRESS).cast::<u64>(), (*thread).rip);
        core::ptr::copy_nonoverlapping((*thread).entry.0.as_ptr(), host, 0x390);
    }
    nt::STATUS_SUCCESS
}

/// # Safety
/// As `NtSuspendThread`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn BTCpuSuspendLocalThread(
    thread: nt::Handle,
    count: *mut u32,
) -> nt::NtStatus {
    // SAFETY: Wine's contract.
    unsafe { nt::NtSuspendThread(thread, count) }
}

#[unsafe(no_mangle)]
pub extern "C" fn BTCpuFlushInstructionCache2(_addr: *const c_void, _size: usize) {
    CODE_GENERATION.fetch_add(1, Ordering::AcqRel);
}

#[unsafe(no_mangle)]
pub extern "C" fn BTCpuNotifyMemoryFree(
    _addr: *mut c_void,
    _size: usize,
    _kind: u32,
    _after: i32,
    _status: i32,
) {
    CODE_GENERATION.fetch_add(1, Ordering::AcqRel);
}

#[unsafe(no_mangle)]
pub extern "C" fn BTCpuNotifyMemoryProtect(
    _addr: *mut c_void,
    _size: usize,
    _prot: u32,
    _after: i32,
    _status: i32,
) {
    CODE_GENERATION.fetch_add(1, Ordering::AcqRel);
}

#[unsafe(no_mangle)]
pub extern "C" fn BTCpuNotifyUnmapViewOfSection(_addr: *mut c_void, _after: i32, _status: i32) {
    CODE_GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Runs the guest from its CONTEXT; Wine calls it again whenever it
/// returns or is unwound (`cpu_simulate`).
#[unsafe(no_mangle)]
pub extern "C" fn BTCpuSimulate() {
    let ctx = context();
    let thread = thread();
    // SAFETY: this thread's state; no reference is kept across a system
    // call (`system_call`), which may nest BTCpuSimulate.
    unsafe {
        nt::RtlCaptureContext((&raw mut (*thread).entry).cast());
        load_context(&mut (*thread).cpu, ctx);
    }
    let cpu = unsafe { &raw mut (*thread).cpu };
    let bop = BOP.load(Ordering::Acquire);
    loop {
        // One instruction at a time, its address known to
        // `BTCpuResetToConsistentState`.
        // SAFETY: as above.
        let result = unsafe {
            match &mut (*thread).engine {
                Some(engine) => {
                    engine.set_concurrent(THREADS.load(Ordering::Acquire) > 1);
                    engine.run(
                        &mut *cpu,
                        &ProcessMemory,
                        1 << 20,
                        &raw mut (*thread).rip,
                        &raw mut (*thread).running,
                    )
                }
                None => {
                    core::ptr::write_volatile(&raw mut (*thread).rip, (*cpu).rip);
                    core::ptr::write_volatile(&raw mut (*thread).running, true);
                    let result = (*cpu).step(&ProcessMemory);
                    core::ptr::write_volatile(&raw mut (*thread).running, false);
                    result
                }
            }
        };
        // SAFETY: as above.
        let rip = unsafe { (*thread).rip };
        match result {
            Ok(Step::Continue) => {}
            // `int 0x2e` (#GP for the interpreter) at one of the two stubs.
            Err(_) if rip == bop => system_call(cpu, ctx),
            // SAFETY: as above.
            Err(_) if rip == bop + 2 => unsafe { unix_call(&mut *cpu) },
            other => {
                let (code, info): (nt::NtStatus, &[u64]) = match other {
                    Err(CpuError::Exception { exception, .. }) => match exception {
                        Exception::DivideError => (nt::STATUS_INTEGER_DIVIDE_BY_ZERO, &[]),
                        Exception::GeneralProtection => (nt::STATUS_PRIVILEGED_INSTRUCTION, &[]),
                        Exception::Breakpoint(_) => (nt::STATUS_BREAKPOINT, &[]),
                        Exception::Debug => (nt::STATUS_SINGLE_STEP, &[]),
                        Exception::SimdFloatingPoint => (nt::STATUS_FLOAT_MULTIPLE_TRAPS, &[]),
                    },
                    Ok(_) => (nt::STATUS_ILLEGAL_INSTRUCTION, &[]),
                    Err(ref error) => {
                        log(format_args!("armor-wow64: {error}"));
                        (nt::STATUS_ILLEGAL_INSTRUCTION, &[])
                    }
                };
                // A trap (int3, int1) resumes after the instruction.
                // SAFETY: as above.
                unsafe {
                    if let Err(CpuError::Exception {
                        exception: Exception::Breakpoint(len),
                        ..
                    }) = other
                    {
                        (*cpu).rip = rip + u64::from(len);
                    } else {
                        (*cpu).rip = rip;
                    }
                    save_context(&*cpu, ctx);
                }
                raise_to_guest(thread, code, rip, info);
            }
        }
    }
}
