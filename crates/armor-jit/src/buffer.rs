//! Executable memory for generated code (ADR-0021, docs/tech/jit.md).
//!
//! The same memory is mapped twice: read-write, where the JIT writes, and
//! read-execute, where the host runs it. It is a shared anonymous mapping,
//! and the second view comes from `mremap` with an old size of 0, which
//! maps the same pages again. (Until 2026-10-13 it was a `memfd`, but
//! sizing it with `ftruncate` fell under `RLIMIT_FSIZE`: with
//! `--limit-file-size`, SIGXFSZ killed armor as the JIT started.) No page is ever
//! writable and executable at once (W^X), and publishing new code needs no
//! `mprotect`. After writing, the instruction cache must be made coherent
//! (`publish`): `dc cvau` on the written lines unless `CTR_EL0.IDC` says it
//! is not needed, then `ic ivau` on the executable lines unless
//! `CTR_EL0.DIC` says so, with the barriers in between (Arm ARM, "Concurrent
//! modification and execution of instructions"). On the Raspberry Pi 5's
//! Cortex-A76, `CTR_EL0` = 0x9444c004: 64-byte lines, IDC 1, DIC 0 (read on
//! 2026-10-12).

use std::io;
use std::ptr::NonNull;

/// A fixed-size region of code, filled from the start.
pub struct CodeBuffer {
    write: NonNull<u8>,
    exec: NonNull<u8>,
    size: usize,
    used: usize,
}

// SAFETY: the buffer owns its mappings, and only its owner writes to it:
// each guest thread has its own `Jit` and buffer (engine.rs).
unsafe impl Send for CodeBuffer {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

impl CodeBuffer {
    /// Maps `size` bytes (rounded up to the host page size) twice.
    pub fn new(size: usize) -> io::Result<CodeBuffer> {
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let size = size.div_ceil(page) * page;
        // SAFETY: a shared anonymous mapping at an address the kernel
        // chooses.
        let write = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if write == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: with an old size of 0, mremap leaves `write` alone and
        // maps the same shared pages again, where the kernel chooses.
        let exec = unsafe { libc::mremap(write, 0, size, libc::MREMAP_MAYMOVE) };
        let exec = if exec == libc::MAP_FAILED {
            Err(io::Error::last_os_error())
        // SAFETY: `exec` is the view just made, `size` bytes long.
        } else if unsafe { libc::mprotect(exec, size, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
            let error = io::Error::last_os_error();
            // SAFETY: unmapping the view just made.
            unsafe { libc::munmap(exec, size) };
            Err(error)
        } else {
            Ok(exec)
        };
        let exec = match exec {
            Ok(exec) => exec,
            Err(error) => {
                // SAFETY: unmapping the mapping made above.
                unsafe { libc::munmap(write, size) };
                return Err(error);
            }
        };
        let write = NonNull::new(write.cast::<u8>()).expect("mmap does not return null");
        let exec = NonNull::new(exec.cast::<u8>()).expect("mremap does not return null");
        Ok(CodeBuffer {
            write,
            exec,
            size,
            used: 0,
        })
    }

    /// Copies `words` into the buffer, makes them executable and returns
    /// their executable address. The words must be position-independent or
    /// already relocated for that address (`next_address`).
    pub fn publish(&mut self, words: &[u32]) -> Result<*const u8, Full> {
        let bytes = words.len() * 4;
        if self.size - self.used < bytes {
            return Err(Full);
        }
        let offset = self.used;
        // SAFETY: [offset, offset + bytes) is inside the writable mapping
        // (checked above), and the words are plain data.
        unsafe {
            std::ptr::copy_nonoverlapping(
                words.as_ptr().cast::<u8>(),
                self.write.as_ptr().add(offset),
                bytes,
            );
        }
        self.used += bytes;
        // SAFETY: both ranges are inside this buffer's mappings.
        unsafe {
            sync_instruction_cache(
                self.write.as_ptr().add(offset),
                self.exec.as_ptr().add(offset),
                bytes,
            )
        };
        // SAFETY: offset is inside the executable mapping.
        Ok(unsafe { self.exec.as_ptr().add(offset) })
    }

    /// Rewrites code already published at `exec` (for example to patch a
    /// branch) and makes the change visible to the instruction stream.
    ///
    /// # Safety
    /// `exec..exec + 4 * words.len()` must lie in this buffer's published
    /// code, and no thread may be executing it.
    pub unsafe fn patch(&mut self, exec: *const u8, words: &[u32]) {
        let offset = exec as usize - self.exec.as_ptr() as usize;
        let bytes = words.len() * 4;
        assert!(
            offset + bytes <= self.used,
            "patch outside the published code"
        );
        // SAFETY: inside the writable mapping (asserted above).
        unsafe {
            std::ptr::copy_nonoverlapping(
                words.as_ptr().cast::<u8>(),
                self.write.as_ptr().add(offset),
                bytes,
            );
            sync_instruction_cache(
                self.write.as_ptr().add(offset),
                self.exec.as_ptr().add(offset),
                bytes,
            );
        }
    }

    /// Empties the buffer: the next `publish` starts from the beginning.
    /// Addresses returned before become invalid; the caller must drop them
    /// (calling one would run whatever is published there later).
    pub fn reset(&mut self) {
        self.used = 0;
    }

    /// The executable address the next `publish` will return.
    pub fn next_address(&self) -> *const u8 {
        // SAFETY: `used` never exceeds the mapping's size.
        unsafe { self.exec.as_ptr().add(self.used) }
    }

    /// Whether `exec` points into the code published so far.
    pub fn contains(&self, exec: *const u8) -> bool {
        let start = self.exec.as_ptr() as usize;
        (start..start + self.used).contains(&(exec as usize))
    }

    pub fn remaining(&self) -> usize {
        self.size - self.used
    }
}

impl Drop for CodeBuffer {
    fn drop(&mut self) {
        // SAFETY: unmapping the two mappings made in `new`.
        unsafe {
            libc::munmap(self.write.as_ptr().cast(), self.size);
            libc::munmap(self.exec.as_ptr().cast(), self.size);
        }
    }
}

/// Makes `len` bytes written at `write` visible to instruction fetches at
/// `exec` (the same memory).
///
/// # Safety
/// Both ranges must be mapped.
#[cfg(target_arch = "aarch64")]
unsafe fn sync_instruction_cache(write: *const u8, exec: *const u8, len: usize) {
    let ctr: u64;
    // SAFETY: CTR_EL0 is readable at EL0 on Linux.
    unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let data_line = 4usize << ((ctr >> 16) & 0xf);
    let instruction_line = 4usize << (ctr & 0xf);
    let idc = (ctr >> 28) & 1 == 1;
    let dic = (ctr >> 29) & 1 == 1;
    if !idc {
        let mut at = write as usize & !(data_line - 1);
        while at < write as usize + len {
            // SAFETY: the line is mapped (caller's contract).
            unsafe { std::arch::asm!("dc cvau, {}", in(reg) at, options(nostack)) };
            at += data_line;
        }
    }
    // SAFETY: barriers have no memory-safety preconditions.
    unsafe { std::arch::asm!("dsb ish", options(nostack)) };
    if !dic {
        let mut at = exec as usize & !(instruction_line - 1);
        while at < exec as usize + len {
            // SAFETY: the line is mapped (caller's contract).
            unsafe { std::arch::asm!("ic ivau, {}", in(reg) at, options(nostack)) };
            at += instruction_line;
        }
        // SAFETY: as above.
        unsafe { std::arch::asm!("dsb ish", options(nostack)) };
    }
    // SAFETY: as above.
    unsafe { std::arch::asm!("isb", options(nostack)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{Assembler, Cond, Reg, Shift, Width};

    /// Calls generated code as `extern "C" fn(u64, u64) -> u64`.
    fn call(code: *const u8, a: u64, b: u64) -> u64 {
        // SAFETY: the tests only publish complete functions that follow
        // the AArch64 procedure call standard and use no stack.
        let f: extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(code) };
        f(a, b)
    }

    fn function(build: impl FnOnce(&mut Assembler)) -> Vec<u32> {
        let mut a = Assembler::new();
        build(&mut a);
        a.ret();
        a.finish().unwrap()
    }

    #[test]
    fn generated_code_runs() {
        let mut buffer = CodeBuffer::new(1 << 16).unwrap();
        let x = Reg::new;
        let add = buffer
            .publish(&function(|a| {
                a.add(Width::X, x(0), x(0), x(1), Shift::Lsl, 0).unwrap()
            }))
            .unwrap();
        assert_eq!(call(add, 40, 2), 42);
        // A loop: sum 1..=n with a conditional branch back.
        let sum = buffer
            .publish(&function(|a| {
                a.mov(Width::X, x(2), crate::encoder::ZR);
                let top = a.new_label();
                let done = a.new_label();
                a.bind(top);
                a.cbz(false, Width::X, x(0), done);
                a.add(Width::X, x(2), x(2), x(0), Shift::Lsl, 0).unwrap();
                a.sub_imm(Width::X, x(0), x(0), 1).unwrap();
                a.b(top);
                a.bind(done);
                a.mov(Width::X, x(0), x(2));
            }))
            .unwrap();
        assert_eq!(call(sum, 100, 0), 5050);
        // Flags: a < b as an unsigned compare.
        let below = buffer
            .publish(&function(|a| {
                a.subs(Width::X, crate::encoder::ZR, x(0), x(1), Shift::Lsl, 0)
                    .unwrap();
                a.cset(Width::X, x(0), Cond::Lo);
            }))
            .unwrap();
        assert_eq!(call(below, 1, 2), 1);
        assert_eq!(call(below, 2, 1), 0);
        // A constant built in four moves.
        let constant = buffer
            .publish(&function(|a| a.mov_imm64(x(0), 0x1234_5678_9abc_def0)))
            .unwrap();
        assert_eq!(call(constant, 0, 0), 0x1234_5678_9abc_def0);
    }

    #[test]
    fn patched_code_is_seen_by_the_instruction_stream() {
        let mut buffer = CodeBuffer::new(1 << 16).unwrap();
        let x = Reg::new;
        let first = function(|a| a.movz(Width::X, x(0), 1, 0).unwrap());
        let second = function(|a| a.movz(Width::X, x(0), 2, 0).unwrap());
        let code = buffer.publish(&first).unwrap();
        assert_eq!(call(code, 0, 0), 1);
        // SAFETY: the code was published above and is not running.
        unsafe { buffer.patch(code, &second) };
        assert_eq!(call(code, 0, 0), 2);
    }

    #[test]
    fn a_full_buffer_says_so() {
        let mut buffer = CodeBuffer::new(1).unwrap();
        let words = vec![0xd503_201f; buffer.remaining() / 4 + 1];
        assert_eq!(buffer.publish(&words), Err(Full));
    }
}
