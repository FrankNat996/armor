//! What the emulator needs from Wine's ntdll beyond its imports: system
//! calls made directly (ntdll's own ARM64EC functions take x86-64
//! contexts, and their converted contexts lose the registers the JIT
//! uses), and the native `KiUserExceptionDispatcher`.
//!
//! - A system call goes through `__wine_syscall_dispatcher` as Wine's own
//!   ARM64 stubs make it: the number in x8, the return address in x9,
//!   `blr x16`. The numbers are read from ntdll's x86-64 stubs (`mov r10,
//!   rcx; mov eax, <number>`), which share them.
//! - ntdll is an ARM64X image: in an ARM64EC process its exports are the
//!   x86-64 entry points, and the CHPE metadata's redirections map them to
//!   their ARM64EC implementations.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::nt;

/// `__wine_syscall_dispatcher`'s value.
static DISPATCHER: AtomicU64 = AtomicU64::new(0);

static NT_CONTINUE: AtomicU64 = AtomicU64::new(u64::MAX);
static NT_RAISE_EXCEPTION: AtomicU64 = AtomicU64::new(u64::MAX);
/// The native `KiUserExceptionDispatcher`.
static KI_USER_EXCEPTION_DISPATCHER: AtomicU64 = AtomicU64::new(0);

/// A system call through Wine's dispatcher, as its ARM64 stubs make it
/// (in assembly here: a call through a pointer would go through the
/// ARM64EC call checker). It comes back after the `blr`, whether the
/// dispatcher returns to x9 or to lr.
///
/// # Safety
/// The call's own contract.
unsafe fn syscall(a0: u64, a1: u64, a2: u64, number: u64) -> nt::NtStatus {
    let status: u64;
    // SAFETY: the caller's contract; the dispatcher follows the C ABI.
    unsafe {
        core::arch::asm!(
            "adr x9, 2f",
            "blr x16",
            "2:",
            in("x16") DISPATCHER.load(Ordering::Acquire),
            in("x8") number,
            inlateout("x0") a0 => status,
            in("x1") a1,
            in("x2") a2,
            clobber_abi("C"),
        )
    };
    status as nt::NtStatus
}

/// Jumps to `pc` with the stack at `sp`.
///
/// # Safety
/// What runs at `pc` never returns here, and `sp` is a stack.
unsafe fn jump_with_stack(pc: u64, sp: u64) -> ! {
    // SAFETY: the caller's contract.
    unsafe {
        core::arch::asm!(
            "mov sp, {sp}",
            "br {pc}",
            sp = in(reg) sp,
            pc = in(reg) pc,
            options(noreturn),
        )
    }
}

fn read<T: Copy>(addr: u64) -> T {
    // SAFETY: inside ntdll's image, mapped for the process's life.
    unsafe { core::ptr::read_unaligned(addr as *const T) }
}

/// ntdll's image: export `name`'s address.
fn export(base: u64, name: &[u8]) -> Option<u64> {
    let pe = base + u64::from(read::<u32>(base + 0x3c));
    // The optional header (PE32+), its export directory entry.
    let exports = base + u64::from(read::<u32>(pe + 0x18 + 0x70));
    let count = read::<u32>(exports + 0x18);
    let functions = base + u64::from(read::<u32>(exports + 0x1c));
    let names = base + u64::from(read::<u32>(exports + 0x20));
    let ordinals = base + u64::from(read::<u32>(exports + 0x24));
    (0..u64::from(count)).find_map(|i| {
        let at = base + u64::from(read::<u32>(names + 4 * i));
        let matches = name
            .iter()
            .enumerate()
            .all(|(j, &c)| read::<u8>(at + j as u64) == c)
            && read::<u8>(at + name.len() as u64) == 0;
        matches.then(|| {
            let ordinal = u64::from(read::<u16>(ordinals + 2 * i));
            base + u64::from(read::<u32>(functions + 4 * ordinal))
        })
    })
}

/// The system call number in ntdll's x86-64 stub at `stub`.
fn syscall_number(stub: u64) -> Option<u64> {
    let head: [u8; 4] = read(stub);
    (head == [0x4c, 0x8b, 0xd1, 0xb8]).then(|| u64::from(read::<u32>(stub + 4)))
}

/// The ARM64EC implementation an x86-64 entry point of ntdll redirects to.
fn redirect(base: u64, entry: u64) -> Option<u64> {
    let pe = base + u64::from(read::<u32>(base + 0x3c));
    // The load configuration's directory entry (index 10).
    let config = base + u64::from(read::<u32>(pe + 0x18 + 0x70 + 8 * 10));
    let metadata: u64 = read(config + 0xc8);
    if metadata == 0 {
        return None;
    }
    let table = base + u64::from(read::<u32>(metadata + 16));
    let count = read::<u32>(metadata + 52);
    let source = (entry - base) as u32;
    (0..u64::from(count)).find_map(|i| {
        (read::<u32>(table + 8 * i) == source)
            .then(|| base + u64::from(read::<u32>(table + 8 * i + 4)))
    })
}

/// Finds what this module needs in ntdll; false when something is missing
/// (not Wine, or a different ntdll).
pub fn init() -> bool {
    // The PEB's loader data: the second module loaded is ntdll (the
    // first, the program).
    let peb: u64 = read(nt::teb() + nt::TEB_PEB);
    let ldr: u64 = read(peb + 0x18);
    let program: u64 = read(ldr + 0x10);
    let ntdll: u64 = read(program);
    let base: u64 = read(ntdll + 0x30);
    if base == 0 {
        return false;
    }
    let (Some(dispatcher), Some(cont), Some(raise), Some(ki)) = (
        export(base, b"__wine_syscall_dispatcher"),
        export(base, b"NtContinue").and_then(syscall_number),
        export(base, b"NtRaiseException").and_then(syscall_number),
        export(base, b"KiUserExceptionDispatcher").and_then(|e| redirect(base, e)),
    ) else {
        return false;
    };
    DISPATCHER.store(read::<u64>(dispatcher), Ordering::Release);
    NT_CONTINUE.store(cont, Ordering::Release);
    NT_RAISE_EXCEPTION.store(raise, Ordering::Release);
    KI_USER_EXCEPTION_DISPATCHER.store(ki, Ordering::Release);
    true
}

/// `NtContinue` with a native (ARM64) context.
pub fn continue_native(context: *const u8) -> ! {
    // SAFETY: a complete ARM64 context; the call does not return.
    unsafe {
        syscall(context as u64, 0, 0, NT_CONTINUE.load(Ordering::Acquire));
    }
    crate::fail(format_args!("armor-arm64ec: NtContinue returned"))
}

/// `NtRaiseException` with a native (ARM64) context.
pub fn raise_native(record: *const u8, context: *const u8, first_chance: bool) -> ! {
    // SAFETY: a record and a complete ARM64 context; the call does not
    // return when it dispatches.
    let status = unsafe {
        syscall(
            record as u64,
            context as u64,
            u64::from(first_chance),
            NT_RAISE_EXCEPTION.load(Ordering::Acquire),
        )
    };
    crate::fail(format_args!(
        "armor-arm64ec: NtRaiseException returned {status:#x}"
    ))
}

/// Size of the native ARM64 CONTEXT.
pub const CONTEXT_SIZE: usize = 0x390;
/// Size of a 64-bit EXCEPTION_RECORD.
pub const RECORD_SIZE: usize = 0x98;

/// What `KiUserExceptionDispatcher` finds on the stack: the context, then
/// the record (with room the dispatcher's newer versions expect between).
#[repr(C, align(16))]
pub struct DispatchFrame {
    pub context: [u8; CONTEXT_SIZE],
    pad: [u64; 4],
    pub record: [u8; RECORD_SIZE],
    align: u64,
    redzone: [u64; 2],
}

impl DispatchFrame {
    pub fn new() -> DispatchFrame {
        DispatchFrame {
            context: [0; CONTEXT_SIZE],
            pad: [0; 4],
            record: [0; RECORD_SIZE],
            align: 0,
            redzone: [0; 2],
        }
    }
}

/// Dispatches the exception in `frame` as the kernel would: the native
/// `KiUserExceptionDispatcher` with the frame on the stack.
///
/// # Safety
/// `frame` lives on the emulator's stack, below anything still in use.
pub unsafe fn dispatch(frame: *const DispatchFrame) -> ! {
    // SAFETY: the caller's contract.
    unsafe {
        jump_with_stack(
            KI_USER_EXCEPTION_DISPATCHER.load(Ordering::Acquire),
            frame as u64,
        )
    }
}
