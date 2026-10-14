//! Linux side of armor: loading x86_64 executables, translating x86_64
//! syscalls to the AArch64 host, and process termination.
//!
//! See docs/architecture.md and docs/tech/syscalls.md.

mod events;
mod files;
mod ipc;
pub mod loader;
mod net;
pub mod process;
pub mod root;
pub mod sandbox;
pub mod signal;
pub mod syscall;

pub use libc::{
    EAGAIN, EINTR, ENOSYS, SIGABRT, SIGALRM, SIGBUS, SIGFPE, SIGHUP, SIGILL, SIGINT, SIGKILL,
    SIGPIPE, SIGQUIT, SIGSEGV, SIGTERM, SIGTRAP,
};

/// The kernel thread id of the calling host thread, which is also the
/// guest thread's tid (ADR-0018).
pub fn current_tid() -> u64 {
    // SAFETY: gettid has no preconditions.
    u64::from(unsafe { libc::gettid() } as u32)
}

/// Gives armor the signal dispositions a freshly exec'd process has, which
/// the guest expects. The Rust runtime ignores SIGPIPE in armor; a new
/// process has it at its default, so that writing to a closed pipe kills
/// it (e.g. `armor yes | head -1`).
pub fn reset_signal_dispositions() {
    // SAFETY: SIG_DFL installs no handler code.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
}

/// Ends armor the way the kernel would end the guest for `signal` (e.g. a
/// fault becomes SIGSEGV), so the exit status matches a native run.
pub fn terminate_by_signal(signal: i32) -> ! {
    // SAFETY: restoring the default disposition and raising a signal have no
    // memory-safety preconditions. The Rust runtime installs its own SIGSEGV
    // handler, so the default must be restored first.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
    // Only reached if the signal does not terminate the process.
    std::process::abort()
}
