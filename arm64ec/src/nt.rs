//! What the backend uses from ntdll (import library from `ntdll.def`), and
//! the layouts of the structures Wine's ARM64EC side shares with it.

#![allow(non_snake_case)]

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type NtStatus = i32;

pub const STATUS_SUCCESS: NtStatus = 0;
pub const STATUS_NOT_IMPLEMENTED: NtStatus = 0xC000_0002u32 as i32;
pub const STATUS_ILLEGAL_INSTRUCTION: NtStatus = 0xC000_001Du32 as i32;

pub const MEM_COMMIT: u32 = 0x1000;
pub const MEM_RESERVE: u32 = 0x2000;
pub const PAGE_EXECUTE_READWRITE: u32 = 0x40;

/// The TEB's pointer to the thread's `CHPE_V2_CPU_AREA_INFO`.
pub const TEB_CHPE_CPU_AREA: u64 = 0x1788;
/// The TEB's PEB, and the PEB's bitmap of ARM64EC code pages.
pub const TEB_PEB: u64 = 0x60;
pub const PEB_EC_CODE_BITMAP: u64 = 0x368;

/// `CHPE_V2_CPU_AREA_INFO` (Wine's `winternl.h`).
#[repr(C)]
pub struct CpuArea {
    pub in_simulation: u8,
    pub in_syscall_callback: u8,
    pub emulator_stack_base: u64,
    pub emulator_stack_limit: u64,
    /// The thread's x86-64 CONTEXT (`ARM64EC_NT_CONTEXT`).
    pub context_amd64: *mut u8,
    pub suspend_doorbell: *mut u32,
    pub loading_module_modflag: u64,
    /// The emulator's own: [0] its thread state, [2] where entries from
    /// ARM64EC code go, [3] where `BeginSimulation` goes.
    pub emulator_data: [u64; 4],
    pub emulator_data_inline: u64,
}

// The x86-64 CONTEXT (`winnt.h`), 0x4d0 bytes.
pub const CTX_FLAGS: usize = 0x30;
pub const CTX_MXCSR: usize = 0x34;
pub const CTX_EFLAGS: usize = 0x44;
pub const CTX_RAX: usize = 0x78;
pub const CTX_RIP: usize = 0xf8;
/// The FXSAVE image of the x87 and SSE state (`FltSave`).
pub const CTX_FLTSAVE: usize = 0x100;

pub const CONTEXT_AMD64: u32 = 0x0010_0000;
pub const CONTEXT_CONTROL: u32 = CONTEXT_AMD64 | 1;
pub const CONTEXT_INTEGER: u32 = CONTEXT_AMD64 | 2;
pub const CONTEXT_FLOATING_POINT: u32 = CONTEXT_AMD64 | 8;

#[link(name = "ntdll")]
unsafe extern "system" {
    pub fn NtAllocateVirtualMemory(
        process: Handle,
        base: *mut *mut c_void,
        zero_bits: usize,
        size: *mut usize,
        kind: u32,
        protect: u32,
    ) -> NtStatus;
    pub fn NtTerminateProcess(process: Handle, status: NtStatus) -> NtStatus;
    pub fn NtReadVirtualMemory(
        process: Handle,
        addr: *const c_void,
        buf: *mut c_void,
        len: usize,
        read: *mut usize,
    ) -> NtStatus;
    pub fn RtlAllocateHeap(heap: *mut c_void, flags: u32, size: usize) -> *mut c_void;
    pub fn RtlFreeHeap(heap: *mut c_void, flags: u32, ptr: *mut c_void) -> u8;
    pub fn __wine_dbg_output(text: *const u8) -> i32;
}

/// The TEB (x18 on ARM64 Windows, ARM64EC code included).
pub fn teb() -> u64 {
    let teb: u64;
    // SAFETY: reads x18, which holds the TEB.
    unsafe { core::arch::asm!("mov {}, x18", out(reg) teb, options(nomem, nostack)) };
    teb
}

/// This thread's CPU area, which Wine allocates with the thread.
pub fn cpu_area() -> *mut CpuArea {
    // SAFETY: the TEB's field, set by Wine for every thread of an ARM64EC
    // process.
    unsafe { *((teb() + TEB_CHPE_CPU_AREA) as *const *mut CpuArea) }
}

/// Whether `addr` is in a page of ARM64EC code (the PEB's bitmap, one bit
/// per 4 KiB page).
pub fn is_ec_code(addr: u64) -> bool {
    // SAFETY: the PEB and its bitmap live as long as the process; the
    // bitmap covers the user address space.
    unsafe {
        let peb = *((teb() + TEB_PEB) as *const u64);
        let bitmap = *((peb + PEB_EC_CODE_BITMAP) as *const *const u64);
        if bitmap.is_null() || addr >> 47 != 0 {
            return false;
        }
        (*bitmap.add((addr >> 18) as usize) >> ((addr >> 12) & 63)) & 1 != 0
    }
}
