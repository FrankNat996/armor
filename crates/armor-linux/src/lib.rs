//! Linux side of armor: loading x86_64 executables, translating x86_64
//! syscalls to the AArch64 host, and process termination.
//!
//! See docs/architecture.md and docs/tech/syscalls.md.

mod events;
mod files;
pub mod host;
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

/// The core dump filter armor started with (`limit_core_dumps`).
static CORE_FILTER: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Leaves the mappings out of armor's core dumps, for every way the guest
/// can die with one (a fault, its own `abort`, a signal from elsewhere).
/// A core dump of armor says nothing about the guest, and armor's address
/// space is large (the JIT's 32 GiB permission table, the guest's
/// memory): where the kernel pipes cores to a handler (Ubuntu's apport,
/// GitHub's runners) dumping it took minutes. The exit status still says
/// the core was dumped. `restore_core_dumps` undoes it before a native
/// program replaces armor.
pub fn limit_core_dumps() {
    if let Ok(filter) = std::fs::read_to_string("/proc/self/coredump_filter") {
        let _ = CORE_FILTER.set(filter.trim().to_string());
        let _ = std::fs::write("/proc/self/coredump_filter", "0");
    }
}

/// The core dump filter armor started with, for a native program that
/// replaces it.
pub fn restore_core_dumps() {
    if let Some(filter) = CORE_FILTER.get() {
        let _ = std::fs::write("/proc/self/coredump_filter", filter);
    }
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
