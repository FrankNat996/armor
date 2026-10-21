//! armor's engine as Wine's ARM64EC emulator (ADR-0047), stage 2: x86-64
//! Windows programs. Wine's ntdll loads this DLL in an ARM64EC process
//! (with Hangover: `HODLL64=armor-arm64ec.dll`) and calls its exports:
//! `ProcessInit` and `ThreadInit`, the four entry points of `chpe.rs`
//! whenever ARM64EC code reaches x86-64 code, and notifications of memory
//! and code changes. The guest's code runs in armor's JIT (64-bit mode,
//! the same engine as the WoW64 backend's, `jit.rs`) and interpreter until
//! it reaches ARM64EC code (the system's DLLs: Wine's ntdll, kernel32, …
//! are ARM64EC or ARM64X), which runs natively.
//!
//! A `syscall` in x86-64 code (ntdll's x86-64 stubs, which programs may
//! call or copy) is raised as `STATUS_EMULATION_SYSCALL`, which Wine runs;
//! exceptions in x86-64 code (a fault, `int3`, an unsupported instruction)
//! are raised for the guest with its state packed in ARM64EC's register
//! mapping (`pack`), and Wine dispatches them to its handlers.

#![no_std]

extern crate alloc;

mod chpe;
#[path = "../../wow64/src/jit.rs"]
mod jit;
mod native;
mod nt;
#[path = "../../wow64/src/rt.rs"]
mod rt;

use alloc::boxed::Box;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use armor_cpu::{
    Cpu, CpuError, Exception, R8, R9, R10, R11, R12, R13, R14, R15, RAX, RBP, RBX, RCX, RDI, RDX,
    RSI, RSP, Step,
};
use armor_guest::{GuestMemory, MemFault};

/// `__wine_dbg_output` of a formatted line.
pub(crate) fn log(args: core::fmt::Arguments<'_>) {
    let mut line = alloc::format!("{args}\n");
    line.push('\0');
    // SAFETY: a NUL-terminated string.
    unsafe { nt::__wine_dbg_output(line.as_ptr()) };
}

/// Ends the process after a line in the log.
fn fail(args: core::fmt::Arguments<'_>) -> ! {
    log(args);
    // SAFETY: ends the process.
    unsafe { nt::NtTerminateProcess(-1isize as nt::Handle, nt::STATUS_NOT_IMPLEMENTED) };
    loop {
        core::hint::spin_loop();
    }
}

/// Changes when guest code may have changed (the memory notifications and
/// instruction cache flushes): the JIT's translations follow it.
static CODE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Threads that ran guest code: from the second one on, the JIT orders
/// its accesses (concurrent mode, ADR-0023).
static THREADS: AtomicU32 = AtomicU32::new(0);

/// A page with an x86-64 `ret`: the return address an entry thunk gets when
/// its x86-64 caller's return address stays on the stack (`chpe.rs`).
#[unsafe(no_mangle)]
static mut ARMOR_X64_RET: u64 = 0;

/// The guest memory of the process, as it is: Wine manages it, and the
/// host's protections are the guest's (as in the WoW64 backend).
struct ProcessMemory;

impl GuestMemory for ProcessMemory {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemFault> {
        // SAFETY: the guest's own memory; an access it may not make faults
        // as it would on Windows.
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

/// The x86-64 state in ARM64EC's registers, as `chpe.rs` saves and loads
/// it: the general registers in x86 order, rip, xmm0–15.
#[repr(C, align(16))]
struct Registers {
    gpr: [u64; 16],
    rip: u64,
    _pad: u64,
    xmm: [u128; 16],
}

/// A thread's state; `chpe.rs` finds it in the CPU area (EmulatorData[0])
/// and reads `registers` at its start.
#[repr(C)]
struct Thread {
    registers: Registers,
    cpu: Cpu,
    /// The JIT, when its code buffer could be allocated.
    engine: Option<jit::Engine>,
    /// The guest instruction being run, while `running`.
    rip: u64,
    running: bool,
    /// Wine's `SuspendDoorbell`.
    doorbell: u32,
}

/// The emulator's own stack, per thread.
const EMULATOR_STACK: usize = 1 << 20;

/// EFLAGS bits an ARM64EC context carries (NZCV and the single-step bit):
/// the others are the emulator's (as FEX's backend merges them).
const EC_EFLAGS: u64 = (1 << 0) | (1 << 6) | (1 << 7) | (1 << 8) | (1 << 11);

fn ctx_u64(ctx: *const u8, offset: usize) -> u64 {
    // SAFETY: inside the CONTEXT.
    unsafe { core::ptr::read_unaligned(ctx.add(offset).cast::<u64>()) }
}

fn ctx_u32(ctx: *const u8, offset: usize) -> u32 {
    // SAFETY: inside the CONTEXT.
    unsafe { core::ptr::read_unaligned(ctx.add(offset).cast::<u32>()) }
}

/// The guest state from an x86-64 CONTEXT (`BeginSimulation`).
fn load_context(cpu: &mut Cpu, ctx: *const u8) {
    let flags = ctx_u32(ctx, nt::CTX_FLAGS);
    if flags & nt::CONTEXT_INTEGER == nt::CONTEXT_INTEGER {
        for (i, reg) in cpu.regs.iter_mut().enumerate() {
            // rsp and rbp are CONTEXT_CONTROL's.
            if i != 4 && i != 5 {
                *reg = ctx_u64(ctx, nt::CTX_RAX + 8 * i);
            }
        }
    }
    if flags & nt::CONTEXT_CONTROL == nt::CONTEXT_CONTROL {
        cpu.regs[4] = ctx_u64(ctx, nt::CTX_RAX + 8 * 4);
        cpu.regs[5] = ctx_u64(ctx, nt::CTX_RAX + 8 * 5);
        cpu.rip = ctx_u64(ctx, nt::CTX_RIP);
        let eflags = u64::from(ctx_u32(ctx, nt::CTX_EFLAGS));
        cpu.rflags = (eflags & EC_EFLAGS) | (cpu.rflags & !EC_EFLAGS) | 2;
    }
    if flags & nt::CONTEXT_FLOATING_POINT == nt::CONTEXT_FLOATING_POINT {
        let mut saved = [0u8; 512];
        // SAFETY: the 512 bytes of FltSave.
        unsafe {
            core::ptr::copy_nonoverlapping(ctx.add(nt::CTX_FLTSAVE), saved.as_mut_ptr(), 512)
        };
        // What an ARM64EC context carries (`pack`): MXCSR, the x87 and the
        // xmm registers. The x87 control, status and tag words stay the
        // emulator's (an ARM64EC context has no room for them), as the
        // flags outside NZCV do.
        let mut image = cpu.fx_image();
        image[24..28].copy_from_slice(&saved[24..28]);
        image[32..416].copy_from_slice(&saved[32..416]);
        if let Err(why) = cpu.load_fx_image(&image) {
            log(format_args!("armor-arm64ec: FPU state not loaded: {why}"));
        }
    }
}

/// The guest state from ARM64EC's registers (`chpe.rs`): flags are not
/// carried across a call or a return, and the direction flag is clear
/// there (the x64 calling convention).
fn load_registers(cpu: &mut Cpu, registers: &Registers) {
    cpu.regs = registers.gpr;
    cpu.rip = registers.rip;
    cpu.xmm = registers.xmm;
    cpu.rflags &= !(1 << 10);
}

fn store_registers(cpu: &Cpu, registers: &mut Registers) {
    registers.gpr = cpu.regs;
    registers.rip = cpu.rip;
    registers.xmm = cpu.xmm;
}

/// Runs the guest until it reaches ARM64EC code; `chpe.rs` calls it on
/// the emulator's stack, with the guest's state in the thread's registers
/// (`from_context` 0) or in the thread's x86-64 CONTEXT (1).
///
/// # Safety
/// `thread` is this thread's state.
#[unsafe(no_mangle)]
unsafe extern "C" fn armor_dispatch(thread: *mut Thread, from_context: u32) -> *mut Thread {
    let area = nt::cpu_area();
    // SAFETY: this thread's state and CPU area.
    let t = unsafe { &mut *thread };
    unsafe {
        (*area).in_simulation = 1;
        if from_context != 0 {
            load_context(&mut t.cpu, (*area).context_amd64);
        } else {
            load_registers(&mut t.cpu, &t.registers);
        }
    }
    t.cpu.gs_base = nt::teb();
    t.cpu.fs_base = 0;
    loop {
        if nt::is_ec_code(t.cpu.rip) {
            break;
        }
        let result = match &mut t.engine {
            Some(engine) => {
                engine.set_concurrent(THREADS.load(Ordering::Acquire) > 1);
                engine.run(
                    &mut t.cpu,
                    &ProcessMemory,
                    1 << 20,
                    &raw mut t.rip,
                    &raw mut t.running,
                    nt::is_ec_code,
                )
            }
            None => {
                // Volatile: the exception hook reads them while the
                // interpreter runs (`ResetToConsistentState`).
                // SAFETY: this thread's state.
                unsafe {
                    core::ptr::write_volatile(&raw mut t.rip, t.cpu.rip);
                    core::ptr::write_volatile(&raw mut t.running, true);
                }
                let result = t.cpu.step(&ProcessMemory);
                // SAFETY: as above.
                unsafe { core::ptr::write_volatile(&raw mut t.running, false) };
                result
            }
        };
        match result {
            Ok(Step::Continue) => {}
            // ntdll's x86-64 stubs (`mov r10, rcx; mov eax, n; syscall`):
            // Wine runs the call from the context and resumes the guest
            // after it (`BeginSimulation`).
            Ok(Step::Syscall) => emulation_syscall(&t.cpu),
            // `int 0x2e`, the other way ntdll's x86-64 stubs enter the
            // system (when KUSER_SHARED_DATA's SystemCall says so).
            Err(CpuError::Exception {
                rip,
                exception: Exception::GeneralProtection,
            }) if is_int_2e(rip) => {
                t.cpu.rip = rip + 2;
                emulation_syscall(&t.cpu)
            }
            Err(error) => {
                let rip = t.rip;
                let (code, next) = match error {
                    CpuError::Exception { exception, .. } => match exception {
                        Exception::DivideError => (STATUS_INTEGER_DIVIDE_BY_ZERO, rip),
                        Exception::GeneralProtection => (STATUS_PRIVILEGED_INSTRUCTION, rip),
                        // The context's rip is the int3's, as on Windows.
                        Exception::Breakpoint(_) => (STATUS_BREAKPOINT, rip),
                        Exception::Debug => (STATUS_SINGLE_STEP, rip + 1),
                        Exception::SimdFloatingPoint => (STATUS_FLOAT_MULTIPLE_TRAPS, rip),
                    },
                    ref other => {
                        log(format_args!("armor-arm64ec: {other}"));
                        (nt::STATUS_ILLEGAL_INSTRUCTION, rip)
                    }
                };
                t.cpu.rip = next;
                raise_to_guest(&t.cpu, code, rip, &[]);
            }
        }
    }
    store_registers(&t.cpu, &mut t.registers);
    thread
}

const STATUS_INTEGER_DIVIDE_BY_ZERO: nt::NtStatus = 0xC000_0094u32 as i32;
const STATUS_PRIVILEGED_INSTRUCTION: nt::NtStatus = 0xC000_0096u32 as i32;
const STATUS_BREAKPOINT: nt::NtStatus = 0x8000_0003u32 as i32;
const STATUS_SINGLE_STEP: nt::NtStatus = 0x8000_0004u32 as i32;
const STATUS_FLOAT_MULTIPLE_TRAPS: nt::NtStatus = 0xC000_02B5u32 as i32;
const STATUS_EMULATION_SYSCALL: nt::NtStatus = 0x4000_0039;

/// The guest's state as a native context in ARM64EC's register mapping,
/// which Wine converts back into the x86-64 CONTEXT: the x87 registers in
/// the ARM64 registers ARM64EC leaves free (FltSave.FloatRegisters'
/// mantissas in lr, x6, x7, x9, x10, x11, x12, x15, their exponents in x16
/// and x17), the arithmetic flags and TF in NZCV and SS, MXCSR in FPCR
/// and FPSR.
fn pack(cpu: &Cpu, ctx: &mut [u8; native::CONTEXT_SIZE]) {
    let mut put = |at: usize, value: u64| ctx[at..at + 8].copy_from_slice(&value.to_le_bytes());
    let x = |n: usize| 8 + 8 * n;
    // CONTEXT_ARM64 | CONTROL | INTEGER | FLOATING_POINT.
    put(0, 0x0040_0007);
    for (reg, n) in [
        (RAX, 8),
        (RCX, 0),
        (RDX, 1),
        (RBX, 27),
        (RSI, 25),
        (RDI, 26),
        (R8, 2),
        (R9, 3),
        (R10, 4),
        (R11, 5),
        (R12, 19),
        (R13, 20),
        (R14, 21),
        (R15, 22),
    ] {
        put(x(n), cpu.regs[reg]);
    }
    put(0xf0, cpu.regs[RBP]);
    put(0x100, cpu.regs[RSP]);
    put(0x108, cpu.rip);
    let image = cpu.fx_image();
    let mantissa =
        |i: usize| u64::from_le_bytes(image[32 + 16 * i..40 + 16 * i].try_into().expect("8 bytes"));
    let exponent =
        |i: usize| u64::from(u16::from_le_bytes([image[40 + 16 * i], image[41 + 16 * i]]));
    for (i, at) in [0xf8, x(6), x(7), x(9), x(10), x(11), x(12), x(15)]
        .into_iter()
        .enumerate()
    {
        put(at, mantissa(i));
    }
    put(
        x(16),
        exponent(3) << 48 | exponent(2) << 32 | exponent(1) << 16 | exponent(0),
    );
    put(
        x(17),
        exponent(7) << 48 | exponent(6) << 32 | exponent(5) << 16 | exponent(4),
    );
    for (i, value) in cpu.xmm.iter().enumerate() {
        ctx[0x110 + 16 * i..0x120 + 16 * i].copy_from_slice(&value.to_le_bytes());
    }
    let flag = |bit: u32| (cpu.rflags >> bit) & 1;
    let cpsr = flag(7) << 31 | flag(6) << 30 | flag(0) << 29 | flag(11) << 28 | flag(8) << 21;
    ctx[4..8].copy_from_slice(&(cpsr as u32).to_le_bytes());
    let (fpcr, fpsr) = fp_control(cpu.mxcsr);
    ctx[0x310..0x314].copy_from_slice(&fpcr.to_le_bytes());
    ctx[0x314..0x318].copy_from_slice(&fpsr.to_le_bytes());
}

/// MXCSR as FPCR (rounding, flush to zero) and FPSR (the exception
/// flags).
fn fp_control(mxcsr: u32) -> (u32, u32) {
    let bit = |n: u32| (mxcsr >> n) & 1;
    // x86's rounding: nearest, down, up, zero; ARM's: nearest, up, down,
    // zero.
    let rounding = match (mxcsr >> 13) & 3 {
        1 => 2,
        2 => 1,
        r => r,
    };
    let fpcr = rounding << 22 | bit(15) << 24;
    let fpsr = bit(0) | bit(2) << 1 | bit(3) << 2 | bit(4) << 3 | bit(5) << 4 | bit(1) << 7;
    (fpcr, fpsr)
}

/// A 64-bit EXCEPTION_RECORD for the guest.
fn record(code: nt::NtStatus, address: u64, info: &[u64]) -> [u8; native::RECORD_SIZE] {
    let mut record = [0u8; native::RECORD_SIZE];
    record[0..4].copy_from_slice(&(code as u32).to_le_bytes());
    record[16..24].copy_from_slice(&address.to_le_bytes());
    record[24..28].copy_from_slice(&(info.len() as u32).to_le_bytes());
    for (i, value) in info.iter().take(15).enumerate() {
        record[32 + 8 * i..40 + 8 * i].copy_from_slice(&value.to_le_bytes());
    }
    record
}

/// Raises `code` for the guest, at its instruction `address`, with its
/// state in `cpu` (rip where it resumes): Wine dispatches it to the
/// guest's handlers, and resumes the guest through `BeginSimulation`.
fn raise_to_guest(cpu: &Cpu, code: nt::NtStatus, address: u64, info: &[u64]) -> ! {
    let mut context = [0u8; native::CONTEXT_SIZE];
    pack(cpu, &mut context);
    let record = record(code, address, info);
    native::raise_native(record.as_ptr(), context.as_ptr(), true)
}

fn is_int_2e(rip: u64) -> bool {
    let mut bytes = [0u8; 2];
    ProcessMemory.fetch(rip, &mut bytes) == 2 && bytes == [0xcd, 0x2e]
}

/// Hands the guest's `syscall` (rip after it) to Wine, as a kernel would
/// raise `STATUS_EMULATION_SYSCALL`.
fn emulation_syscall(cpu: &Cpu) -> ! {
    let mut frame = native::DispatchFrame::new();
    pack(cpu, &mut frame.context);
    frame.record = record(STATUS_EMULATION_SYSCALL, cpu.rip, &[]);
    // SAFETY: the frame is on the emulator's stack, and nothing above it
    // runs again (BeginSimulation starts from the stack's base).
    unsafe { native::dispatch(&frame) }
}

/// # Safety
/// Called by Wine once, at process start.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ProcessInit() -> nt::NtStatus {
    let mut base: *mut c_void = core::ptr::null_mut();
    let mut size = 4096usize;
    // SAFETY: a new page, outside ARM64EC code.
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
    if status != nt::STATUS_SUCCESS {
        return status;
    }
    // SAFETY: the page just allocated: x86-64's `ret`.
    unsafe {
        *(base as *mut u8) = 0xc3;
        ARMOR_X64_RET = base as u64;
    }
    if !native::init() {
        log(format_args!(
            "armor-arm64ec: ntdll's system call dispatcher or KiUserExceptionDispatcher not found"
        ));
        return nt::STATUS_NOT_IMPLEMENTED;
    }
    log(format_args!(
        "armor-arm64ec {}: started",
        env!("CARGO_PKG_VERSION")
    ));
    nt::STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn ProcessTerm(_process: nt::Handle, _after: i32, _status: nt::NtStatus) {}

/// Puts the thread's state into its CPU area: the emulator's stack, where
/// entries go, and the x86-64 CONTEXT's defaults.
///
/// # Safety
/// Called by Wine on the new thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ThreadInit() -> nt::NtStatus {
    let mut stack: *mut c_void = core::ptr::null_mut();
    let mut size = EMULATOR_STACK;
    // SAFETY: a new region for the emulator's stack.
    let status = unsafe {
        nt::NtAllocateVirtualMemory(
            -1isize as nt::Handle,
            &mut stack,
            0,
            &mut size,
            nt::MEM_RESERVE | nt::MEM_COMMIT,
            0x04, // PAGE_READWRITE
        )
    };
    if status != nt::STATUS_SUCCESS {
        return status;
    }
    let thread = Box::into_raw(Box::new(Thread {
        registers: Registers {
            gpr: [0; 16],
            rip: 0,
            _pad: 0,
            xmm: [0; 16],
        },
        cpu: Cpu::new(0, 0),
        engine: jit::Engine::new(false),
        rip: 0,
        running: false,
        doorbell: 0,
    }));
    THREADS.fetch_add(1, Ordering::AcqRel);
    let area = nt::cpu_area();
    // SAFETY: Wine's CPU area for this thread, and its CONTEXT.
    unsafe {
        (*area).emulator_stack_limit = stack as u64;
        (*area).emulator_stack_base = stack as u64 + EMULATOR_STACK as u64;
        (*area).emulator_data[0] = thread as u64;
        (*area).emulator_data[2] = chpe::ARMOR_ENTER_EC;
        (*area).suspend_doorbell = &raw mut (*thread).doorbell;
        let ctx = (*area).context_amd64;
        core::ptr::write_bytes(ctx, 0, 0x4d0);
        let put16 = |at: usize, v: u16| core::ptr::write_unaligned(ctx.add(at).cast::<u16>(), v);
        let put32 = |at: usize, v: u32| core::ptr::write_unaligned(ctx.add(at).cast::<u32>(), v);
        put32(
            nt::CTX_FLAGS,
            nt::CONTEXT_CONTROL | nt::CONTEXT_INTEGER | nt::CONTEXT_FLOATING_POINT | 4,
        );
        put32(nt::CTX_MXCSR, 0x1f80);
        // cs, ds, es, fs, gs, ss.
        for (i, selector) in [0x33u16, 0x2b, 0x2b, 0x53, 0x2b, 0x2b]
            .into_iter()
            .enumerate()
        {
            put16(0x38 + 2 * i, selector);
        }
        put32(nt::CTX_EFLAGS, 0x202);
        // FltSave: ControlWord, MxCsr.
        put16(nt::CTX_FLTSAVE, 0x27f);
        put32(nt::CTX_FLTSAVE + 0x18, 0x1f80);
        load_context(&mut (*thread).cpu, ctx);
        // Windows starts threads with the x87 at 53 bits of precision.
        let mut image = (*thread).cpu.fx_image();
        image[0..2].copy_from_slice(&0x27fu16.to_le_bytes());
        let _ = (*thread).cpu.load_fx_image(&image);
    }
    nt::STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn ThreadTerm(_thread: nt::Handle, _code: i32) -> nt::NtStatus {
    nt::STATUS_SUCCESS
}

/// Called for every exception before it is dispatched. One raised by a
/// guest access of translated code resumes at the instruction's slow
/// path, where the interpreter runs it; one raised while the interpreter
/// runs a guest instruction (an access the host refuses) becomes the
/// guest's: its state is the instruction's start (armor's interpreter
/// changes nothing before an access that can fault), raised again with the
/// guest's address, out of the emulator's frames.
///
/// # Safety
/// Wine passes the record and the two contexts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ResetToConsistentState(
    record: *mut u8,
    _guest: *mut u8,
    native: *mut u8,
) -> nt::NtStatus {
    let area = nt::cpu_area();
    // SAFETY: Wine's record and native (ARM64) context; the CPU area and
    // this thread's state.
    unsafe {
        let thread = (*area).emulator_data[0] as *mut Thread;
        if !thread.is_null() {
            let pc = ctx_u64(native, 0x108);
            if let Some(engine) = &(*thread).engine
                && engine.contains(pc)
            {
                let Some(slow) = engine.slow_path(pc) else {
                    fail(format_args!(
                        "armor-arm64ec: exception {:#x} in translated code at {pc:#x}",
                        ctx_u32(record, 0)
                    ));
                };
                core::ptr::write_unaligned(native.add(0x108).cast::<u64>(), slow);
                native::continue_native(native);
            }
            if core::ptr::read_volatile(&raw const (*thread).running) {
                (*thread).running = false;
                let rip = (*thread).rip;
                let count = (ctx_u32(record, 24) as usize).min(15);
                let mut info = [0u64; 15];
                for (i, value) in info.iter_mut().take(count).enumerate() {
                    *value = ctx_u64(record, 32 + 8 * i);
                }
                let cpu = &mut (*thread).cpu;
                cpu.rip = rip;
                raise_to_guest(cpu, ctx_u32(record, 0) as i32, rip, &info[..count]);
            }
        }
        (*area).in_simulation = 0;
        (*area).in_syscall_callback = 0;
    }
    nt::STATUS_SUCCESS
}

fn code_changed() {
    CODE_GENERATION.fetch_add(1, Ordering::AcqRel);
}

#[unsafe(no_mangle)]
pub extern "C" fn NotifyMemoryAlloc(
    _addr: *mut c_void,
    _size: usize,
    _kind: u32,
    _prot: u32,
    _after: i32,
    _status: nt::NtStatus,
) {
}

#[unsafe(no_mangle)]
pub extern "C" fn NotifyMemoryFree(
    _addr: *mut c_void,
    _size: usize,
    _kind: u32,
    _after: i32,
    _status: nt::NtStatus,
) {
    code_changed();
}

#[unsafe(no_mangle)]
pub extern "C" fn NotifyMemoryProtect(
    _addr: *mut c_void,
    _size: usize,
    _prot: u32,
    _after: i32,
    _status: nt::NtStatus,
) {
    code_changed();
}

#[unsafe(no_mangle)]
pub extern "C" fn NotifyMapViewOfSection(
    _unknown1: *mut c_void,
    _addr: *mut c_void,
    _unknown2: *mut c_void,
    _size: usize,
    _kind: u32,
    _prot: u32,
) -> nt::NtStatus {
    code_changed();
    nt::STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn NotifyUnmapViewOfSection(_addr: *mut c_void, _after: i32, _status: nt::NtStatus) {
    code_changed();
}

#[unsafe(no_mangle)]
pub extern "C" fn FlushInstructionCacheHeavy(_addr: *const c_void, _size: usize) {
    code_changed();
}

#[unsafe(no_mangle)]
pub extern "C" fn BTCpu64FlushInstructionCache(_addr: *const c_void, _size: usize) {
    code_changed();
}

#[unsafe(no_mangle)]
pub extern "C" fn BTCpu64NotifyMemoryDirty(_addr: *mut c_void, _size: usize) {
    code_changed();
}

#[unsafe(no_mangle)]
pub extern "C" fn BTCpu64NotifyReadFile(
    _file: nt::Handle,
    _addr: *mut c_void,
    _size: usize,
    after: i32,
    _status: nt::NtStatus,
) {
    if after != 0 {
        code_changed();
    }
}

// PF_* (`winnt.h`), as armor's CPUID model declares them (`tech/cpuid.md`).
const PF_COMPARE_EXCHANGE_DOUBLE: u32 = 2;
const PF_MMX_INSTRUCTIONS_AVAILABLE: u32 = 3;
const PF_XMMI_INSTRUCTIONS_AVAILABLE: u32 = 6;
const PF_RDTSC_INSTRUCTION_AVAILABLE: u32 = 8;
const PF_PAE_ENABLED: u32 = 9;
const PF_XMMI64_INSTRUCTIONS_AVAILABLE: u32 = 10;
const PF_NX_ENABLED: u32 = 12;
const PF_SSE3_INSTRUCTIONS_AVAILABLE: u32 = 13;
const PF_COMPARE_EXCHANGE128: u32 = 14;
const PF_SSSE3_INSTRUCTIONS_AVAILABLE: u32 = 36;
const PF_SSE4_1_INSTRUCTIONS_AVAILABLE: u32 = 37;
const PF_SSE4_2_INSTRUCTIONS_AVAILABLE: u32 = 38;

fn feature_present(feature: u32) -> bool {
    let [_, _, ecx, edx] = armor_cpu::cpuid::query(1, 0);
    let bit = |reg: u32, n: u32| reg >> n & 1 != 0;
    match feature {
        PF_COMPARE_EXCHANGE_DOUBLE => bit(edx, 8),
        PF_MMX_INSTRUCTIONS_AVAILABLE => bit(edx, 23),
        PF_XMMI_INSTRUCTIONS_AVAILABLE => bit(edx, 25),
        PF_RDTSC_INSTRUCTION_AVAILABLE => bit(edx, 4),
        PF_PAE_ENABLED => bit(edx, 6),
        PF_XMMI64_INSTRUCTIONS_AVAILABLE => bit(edx, 26),
        PF_NX_ENABLED => armor_cpu::cpuid::query(0x8000_0001, 0)[3] >> 20 & 1 != 0,
        PF_SSE3_INSTRUCTIONS_AVAILABLE => bit(ecx, 0),
        PF_COMPARE_EXCHANGE128 => bit(ecx, 13),
        PF_SSSE3_INSTRUCTIONS_AVAILABLE => bit(ecx, 9),
        PF_SSE4_1_INSTRUCTIONS_AVAILABLE => bit(ecx, 19),
        PF_SSE4_2_INSTRUCTIONS_AVAILABLE => bit(ecx, 20),
        _ => false,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn BTCpu64IsProcessorFeaturePresent(feature: u32) -> u8 {
    u8::from(feature_present(feature))
}

/// The x86-64 processor Wine reports (`SYSTEM_CPU_INFORMATION`): the
/// architecture, and armor's CPUID model's family, model and stepping.
/// The feature bits stay Wine's: `IsProcessorFeaturePresent` asks
/// `BTCpu64IsProcessorFeaturePresent`.
///
/// # Safety
/// Wine passes a SYSTEM_CPU_INFORMATION.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn UpdateProcessorInformation(info: *mut u8) {
    let [eax, ..] = armor_cpu::cpuid::query(1, 0);
    let family = (eax >> 8 & 0xf) + (eax >> 20 & 0xff);
    let model = (eax >> 4 & 0xf) | (eax >> 12 & 0xf0);
    let stepping = eax & 0xf;
    // SAFETY: Wine's structure: architecture, level, revision (u16 each).
    unsafe {
        core::ptr::write_unaligned(info.cast::<u16>(), 9); // PROCESSOR_ARCHITECTURE_AMD64
        core::ptr::write_unaligned(info.add(2).cast::<u16>(), family as u16);
        core::ptr::write_unaligned(info.add(4).cast::<u16>(), (model << 8 | stepping) as u16);
    }
}

// The guest's stack pointer is RSP in both orders.
const _: () = assert!(RSP == 4);
