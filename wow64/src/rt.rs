//! The little runtime a DLL without a C runtime needs: the allocator (the
//! process heap, through ntdll), the panic handler, and the memory
//! functions the compiler calls.

use core::alloc::{GlobalAlloc, Layout};
use core::ffi::c_void;

use crate::nt;

/// The process heap, from the PEB (TEB + 0x60, PEB + 0x30).
fn process_heap() -> *mut c_void {
    // SAFETY: x18 is the TEB on ARM64 Windows, and the PEB and its heap
    // live as long as the process.
    unsafe {
        let peb = *((nt::teb() + 0x60) as *const u64);
        *((peb + 0x30) as *const *mut c_void)
    }
}

/// `RtlAllocateHeap` aligns to 16 bytes; larger alignments take more and
/// keep the block's start just before the aligned pointer.
struct Heap;

const HEAP_ALIGN: usize = 16;

// SAFETY: RtlAllocateHeap returns memory valid until RtlFreeHeap, or null.
unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the process heap is a valid heap handle.
        unsafe {
            if layout.align() <= HEAP_ALIGN {
                return nt::RtlAllocateHeap(process_heap(), 0, layout.size()).cast();
            }
            let raw: *mut u8 =
                nt::RtlAllocateHeap(process_heap(), 0, layout.size() + layout.align()).cast();
            if raw.is_null() {
                return raw;
            }
            let aligned = (raw as usize + layout.align()) & !(layout.align() - 1);
            *((aligned - 8) as *mut usize) = raw as usize;
            aligned as *mut u8
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc` with the same layout.
        unsafe {
            let raw = if layout.align() <= HEAP_ALIGN {
                ptr
            } else {
                *((ptr as usize - 8) as *const usize) as *mut u8
            };
            nt::RtlFreeHeap(process_heap(), 0, raw.cast());
        }
    }
}

#[global_allocator]
static HEAP: Heap = Heap;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    crate::log(format_args!("armor-wow64: panic: {info}"));
    // SAFETY: ends the process.
    unsafe { nt::NtTerminateProcess(-1isize as *mut c_void, 0xC000_0409u32 as i32) };
    loop {
        core::hint::spin_loop();
    }
}

// The MSVC target marks programs that use floating point with this.
#[unsafe(no_mangle)]
static _fltused: i32 = 0;

// The memory functions the compiler emits calls to. Plain byte loops, kept
// from being turned back into calls to themselves.

/// # Safety
/// As C's memcpy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    // SAFETY: the caller's contract.
    unsafe { memmove(dst, src, n) }
}

/// # Safety
/// As C's memmove.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let (dst, src) = (dst.cast::<u8>(), src.cast::<u8>());
    // SAFETY: the caller's contract; volatile accesses are not merged into
    // a call to memmove.
    unsafe {
        if (dst as usize) < (src as usize) {
            for i in 0..n {
                core::ptr::write_volatile(dst.add(i), core::ptr::read_volatile(src.add(i)));
            }
        } else {
            for i in (0..n).rev() {
                core::ptr::write_volatile(dst.add(i), core::ptr::read_volatile(src.add(i)));
            }
        }
    }
    dst.cast()
}

/// # Safety
/// As C's memset.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dst: *mut c_void, value: i32, n: usize) -> *mut c_void {
    let dst = dst.cast::<u8>();
    // SAFETY: the caller's contract.
    unsafe {
        for i in 0..n {
            core::ptr::write_volatile(dst.add(i), value as u8);
        }
    }
    dst.cast()
}

/// # Safety
/// As C's memcmp.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    let (a, b) = (a.cast::<u8>(), b.cast::<u8>());
    // SAFETY: the caller's contract.
    unsafe {
        for i in 0..n {
            let (x, y) = (
                core::ptr::read_volatile(a.add(i)),
                core::ptr::read_volatile(b.add(i)),
            );
            if x != y {
                return i32::from(x) - i32::from(y);
            }
        }
    }
    0
}

/// # Safety
/// As memcmp.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    // SAFETY: the caller's contract.
    unsafe { memcmp(a, b, n) }
}

/// # Safety
/// As C's strlen.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(s: *const core::ffi::c_char) -> usize {
    let s = s.cast::<u8>();
    let mut n = 0;
    // SAFETY: the caller's contract: a NUL-terminated string.
    unsafe {
        while core::ptr::read_volatile(s.add(n)) != 0 {
            n += 1;
        }
    }
    n
}

/// The unwinding personality the precompiled `alloc` refers to: with
/// `panic = "abort"` nothing unwinds through Rust frames, so it is never
/// called; if it were, the process ends.
#[unsafe(no_mangle)]
pub extern "C" fn __CxxFrameHandler3() -> ! {
    // SAFETY: ends the process.
    unsafe { nt::NtTerminateProcess(-1isize as *mut c_void, 0xC000_0409u32 as i32) };
    loop {
        core::hint::spin_loop();
    }
}
