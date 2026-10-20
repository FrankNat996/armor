//! What the backend uses from ntdll and wow64.dll (import libraries from
//! `ntdll.def` and `wow64.def`).

#![allow(non_snake_case)]

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type NtStatus = i32;

pub const STATUS_SUCCESS: NtStatus = 0;
pub const STATUS_ILLEGAL_INSTRUCTION: NtStatus = 0xC000_001Du32 as i32;
pub const STATUS_INTEGER_DIVIDE_BY_ZERO: NtStatus = 0xC000_0094u32 as i32;
pub const STATUS_PRIVILEGED_INSTRUCTION: NtStatus = 0xC000_0096u32 as i32;
pub const STATUS_BREAKPOINT: NtStatus = 0x8000_0003u32 as i32;
pub const STATUS_SINGLE_STEP: NtStatus = 0x8000_0004u32 as i32;
pub const STATUS_FLOAT_MULTIPLE_TRAPS: NtStatus = 0xC000_02B5u32 as i32;

/// The ARM64 CONTEXT (`winnt.h`), 0x390 bytes, 16-aligned.
#[repr(C, align(16))]
pub struct Context64(pub [u8; 0x390]);

/// The ARM64 CONTEXT's Pc.
pub const CONTEXT_PC: usize = 0x108;

/// EXCEPTION_RECORD (64-bit) offsets.
pub const RECORD_CODE: usize = 0;
pub const RECORD_ADDRESS: usize = 16;
pub const RECORD_PARAMETERS: usize = 24;
pub const RECORD_INFORMATION: usize = 32;

pub const MEM_COMMIT: u32 = 0x1000;
pub const MEM_RESERVE: u32 = 0x2000;
pub const PAGE_EXECUTE_READWRITE: u32 = 0x40;
/// `NtQueryInformationThread`'s ThreadWow64Context.
pub const THREAD_WOW64_CONTEXT: u32 = 29;

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
    pub fn NtQueryInformationThread(
        thread: Handle,
        class: u32,
        info: *mut c_void,
        len: u32,
        returned: *mut u32,
    ) -> NtStatus;
    pub fn NtSetInformationThread(
        thread: Handle,
        class: u32,
        info: *mut c_void,
        len: u32,
    ) -> NtStatus;
    pub fn NtTerminateProcess(process: Handle, status: NtStatus) -> NtStatus;
    pub fn NtReadVirtualMemory(
        process: Handle,
        addr: *const c_void,
        buf: *mut c_void,
        len: usize,
        read: *mut usize,
    ) -> NtStatus;
    pub fn NtSuspendThread(thread: Handle, count: *mut u32) -> NtStatus;
    pub fn RtlAllocateHeap(heap: *mut c_void, flags: u32, size: usize) -> *mut c_void;
    pub fn RtlFreeHeap(heap: *mut c_void, flags: u32, ptr: *mut c_void) -> u8;
    pub fn RtlWow64GetCurrentCpuArea(
        machine: *mut u16,
        context: *mut *mut c_void,
        context_ex: *mut *mut c_void,
    ) -> NtStatus;
    pub fn RtlCaptureContext(context: *mut c_void);
    pub fn NtContinue(context: *mut c_void, alert: u8) -> NtStatus;
    pub fn NtRaiseException(
        record: *mut c_void,
        context: *mut c_void,
        first_chance: u8,
    ) -> NtStatus;
    pub fn __wine_dbg_output(text: *const u8) -> i32;
    /// A pointer to the 64-bit unix call dispatcher (data).
    pub static __wine_unix_call_dispatcher:
        unsafe extern "C" fn(handle: u64, code: u32, args: *mut c_void) -> NtStatus;
}

#[link(name = "wow64")]
unsafe extern "system" {
    pub fn Wow64SystemServiceEx(number: u32, args: *mut u32) -> NtStatus;
}

/// The 64-bit TEB (x18 on ARM64 Windows).
pub fn teb() -> u64 {
    let teb: u64;
    // SAFETY: reads x18, which holds the TEB on ARM64 Windows.
    unsafe { core::arch::asm!("mov {}, x18", out(reg) teb, options(nomem, nostack)) };
    teb
}
