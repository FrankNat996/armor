//! x86-64 syscalls added by ADR-0044 so that every syscall of the kernel
//! headers gets an answer: those the host kernel does for the guest once
//! the arguments are converted, and those answered as a real kernel
//! configuration answers them (docs/tech/syscalls.md, "Every syscall").

use std::sync::atomic::{AtomicU64, Ordering};

use armor_mem::{AddressSpace, GuestMemory};

use super::Process;
use super::{SysResult, copy_in, err, host, host_errno};

/// The x86-64 numbers handled here.
pub mod nr {
    pub const PTRACE: u64 = 101;
    pub const SETREUID: u64 = 113;
    pub const SETREGID: u64 = 114;
    pub const SETGROUPS: u64 = 116;
    pub const SETRESUID: u64 = 117;
    pub const SETRESGID: u64 = 119;
    pub const SETFSUID: u64 = 122;
    pub const SETFSGID: u64 = 123;
    pub const PERSONALITY: u64 = 135;
    pub const VHANGUP: u64 = 153;
    pub const NFSSERVCTL: u64 = 180;
    pub const READAHEAD: u64 = 187;
    pub const LOOKUP_DCOOKIE: u64 = 212;
    pub const REMAP_FILE_PAGES: u64 = 216;
    pub const RESTART_SYSCALL: u64 = 219;
    pub const UNSHARE: u64 = 272;
    pub const GET_ROBUST_LIST: u64 = 274;
    pub const SIGNALFD: u64 = 282;
    pub const SIGNALFD4: u64 = 289;
    pub const SETNS: u64 = 308;
    pub const KCMP: u64 = 312;
    pub const USERFAULTFD: u64 = 323;
    pub const MLOCK2: u64 = 325;
    pub const PIDFD_SEND_SIGNAL: u64 = 424;
    pub const IO_URING_SETUP: u64 = 425;
    pub const IO_URING_ENTER: u64 = 426;
    pub const IO_URING_REGISTER: u64 = 427;
    pub const PIDFD_OPEN: u64 = 434;
    pub const PIDFD_GETFD: u64 = 438;
    pub const MEMFD_SECRET: u64 = 447;
    pub const PROCESS_MRELEASE: u64 = 448;
    pub const MAP_SHADOW_STACK: u64 = 453;
    pub const MSEAL: u64 = 462;
}

/// Every number `dispatch` answers.
pub const HANDLED: &[u64] = &[
    nr::PTRACE,
    nr::SETREUID,
    nr::SETREGID,
    nr::SETGROUPS,
    nr::SETRESUID,
    nr::SETRESGID,
    nr::SETFSUID,
    nr::SETFSGID,
    nr::PERSONALITY,
    nr::VHANGUP,
    nr::NFSSERVCTL,
    nr::READAHEAD,
    nr::LOOKUP_DCOOKIE,
    nr::REMAP_FILE_PAGES,
    nr::RESTART_SYSCALL,
    nr::UNSHARE,
    nr::GET_ROBUST_LIST,
    nr::SIGNALFD,
    nr::SIGNALFD4,
    nr::SETNS,
    nr::KCMP,
    nr::USERFAULTFD,
    nr::MLOCK2,
    nr::PIDFD_SEND_SIGNAL,
    nr::IO_URING_SETUP,
    nr::IO_URING_ENTER,
    nr::IO_URING_REGISTER,
    nr::PIDFD_OPEN,
    nr::PIDFD_GETFD,
    nr::MEMFD_SECRET,
    nr::PROCESS_MRELEASE,
    nr::MAP_SHADOW_STACK,
    nr::MSEAL,
];

// AArch64 numbers the libc crate does not name (the calls from 424 on have
// one number on every architecture).
const SYS_PIDFD_SEND_SIGNAL: libc::c_long = 424;
const SYS_PIDFD_OPEN: libc::c_long = 434;
const SYS_PIDFD_GETFD: libc::c_long = 438;
const SYS_PROCESS_MRELEASE: libc::c_long = 448;

/// The answer to `number`, or `None` if it is not handled here. `Err` is
/// an operation armor does not support (the caller stops with it).
pub(super) fn dispatch(
    mem: &AddressSpace,
    _process: &Process,
    number: u64,
    a: [u64; 6],
) -> Option<Result<SysResult, String>> {
    let result = match number {
        // ptrace: as with Yama's ptrace_scope 3, which refuses every
        // attach and PTRACE_TRACEME: armor cannot hand a debugger the
        // guest's registers (ADR-0044).
        nr::PTRACE => Err(err(libc::EPERM)),
        // Kernel configurations: built without io_uring (as ChromeOS and
        // Android are), without userfaultfd and secret memory, before
        // mseal (6.10); calls removed from Linux; a CPU without user
        // shadow stacks (armor's CPUID reports no CET).
        nr::IO_URING_SETUP
        | nr::IO_URING_ENTER
        | nr::IO_URING_REGISTER
        | nr::USERFAULTFD
        | nr::MEMFD_SECRET
        | nr::MSEAL
        | nr::LOOKUP_DCOOKIE
        | nr::NFSSERVCTL => Err(err(libc::ENOSYS)),
        nr::MAP_SHADOW_STACK => Err(err(libc::EOPNOTSUPP)),
        // The guest's mappings are armor's: the kernel cannot rearrange
        // their pages behind armor's tables.
        nr::REMAP_FILE_PAGES => Err(err(libc::ENOSYS)),
        // Like set_robust_list (ENOSYS, as under qemu).
        nr::GET_ROBUST_LIST => Err(err(libc::ENOSYS)),
        // armor never sets up a restart block, and without one Linux
        // answers EINTR.
        nr::RESTART_SYSCALL => Err(err(libc::EINTR)),
        nr::SETREUID => host(libc::SYS_setreuid, &[a[0], a[1]]),
        nr::SETREGID => host(libc::SYS_setregid, &[a[0], a[1]]),
        nr::SETRESUID => host(libc::SYS_setresuid, &[a[0], a[1], a[2]]),
        nr::SETRESGID => host(libc::SYS_setresgid, &[a[0], a[1], a[2]]),
        nr::SETFSUID => host(libc::SYS_setfsuid, &[a[0]]),
        nr::SETFSGID => host(libc::SYS_setfsgid, &[a[0]]),
        nr::SETGROUPS => setgroups(mem, a[0], a[1]),
        nr::PERSONALITY => Ok(personality(a[0])),
        nr::VHANGUP => host(libc::SYS_vhangup, &[]),
        nr::READAHEAD => host(libc::SYS_readahead, &[a[0], a[1], a[2]]),
        nr::MLOCK2 => host(libc::SYS_mlock2, &[a[0], a[1], a[2]]),
        nr::UNSHARE => host(libc::SYS_unshare, &[a[0]]),
        nr::SETNS => host(libc::SYS_setns, &[a[0], a[1]]),
        nr::KCMP => kcmp(mem, a),
        nr::PIDFD_OPEN => host(SYS_PIDFD_OPEN, &[a[0], a[1]]),
        nr::PIDFD_GETFD => host(SYS_PIDFD_GETFD, &[a[0], a[1], a[2]]),
        nr::PROCESS_MRELEASE => host(SYS_PROCESS_MRELEASE, &[a[0], a[1]]),
        nr::PIDFD_SEND_SIGNAL => pidfd_send_signal(mem, a[0], a[1], a[2], a[3]),
        nr::SIGNALFD => signalfd(mem, a[0], a[1], a[2], 0),
        nr::SIGNALFD4 => signalfd(mem, a[0], a[1], a[2], a[3]),
        _ => return None,
    };
    Some(Ok(result))
}

/// setgroups(2): `gid_t` is 32 bits on both; at most NGROUPS_MAX.
fn setgroups(mem: &AddressSpace, size: u64, list: u64) -> SysResult {
    const NGROUPS_MAX: u64 = 65536;
    let size = size as i32;
    if size < 0 || size as u64 > NGROUPS_MAX {
        // The kernel checks the permission first; an unprivileged caller
        // gets EPERM either way.
        return Err(err(if can_set_groups() {
            libc::EINVAL
        } else {
            libc::EPERM
        }));
    }
    let mut bytes = vec![0u8; size as usize * 4];
    if size > 0 {
        mem.read(list, &mut bytes).map_err(|_| err(libc::EFAULT))?;
    }
    // SAFETY: bytes holds `size` 32-bit group ids, as the host expects.
    let r = unsafe { libc::syscall(libc::SYS_setgroups, size as usize, bytes.as_ptr()) };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

/// Whether this process may change its groups (CAP_SETGID in its
/// effective set).
fn can_set_groups() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("CapEff:"))
                .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
        })
        .is_some_and(|caps| caps & (1 << 6) != 0)
}

/// The guest's personality (u64::MAX until read from the host): kept by
/// armor, since some personas mean something else to an AArch64 kernel
/// (PER_LINUX32 asks for AArch32, which not every ARM64 CPU has). Only
/// ADDR_NO_RANDOMIZE also goes to the host, where it acts on the programs
/// armor starts for the guest.
static PERSONALITY: AtomicU64 = AtomicU64::new(u64::MAX);
const PER_MASK: u64 = 0xff;
const PER_LINUX32: u64 = 0x0008;
const ADDR_NO_RANDOMIZE: u64 = 0x0004_0000;

fn current_personality() -> u64 {
    let value = PERSONALITY.load(Ordering::Relaxed);
    if value != u64::MAX {
        return value;
    }
    // SAFETY: 0xffffffff only queries the personality.
    let host = unsafe { libc::personality(0xffff_ffff) } as u32 as u64;
    PERSONALITY.store(host, Ordering::Relaxed);
    host
}

/// personality(2): returns the previous persona; 0xffffffff only asks.
fn personality(persona: u64) -> i64 {
    let old = current_personality();
    let persona = persona as u32 as u64;
    if persona != 0xffff_ffff {
        PERSONALITY.store(persona, Ordering::Relaxed);
        // SAFETY: querying, then setting the host's ADDR_NO_RANDOMIZE bit.
        unsafe {
            let host = libc::personality(0xffff_ffff) as u32 as u64;
            let host = (host & !ADDR_NO_RANDOMIZE) | (persona & ADDR_NO_RANDOMIZE);
            libc::personality(host as libc::c_ulong);
        }
    }
    old as i64
}

/// Whether the guest asked for the PER_LINUX32 persona, under which uname
/// says i686 (as on x86-64 Linux).
pub(crate) fn linux32() -> bool {
    current_personality() & PER_MASK == PER_LINUX32
}

/// kcmp(2); KCMP_EPOLL_TFD (7) reads a `struct kcmp_epoll_slot` (three
/// u32, the same on both).
fn kcmp(mem: &AddressSpace, a: [u64; 6]) -> SysResult {
    const KCMP_EPOLL_TFD: u64 = 7;
    let slot;
    let idx2 = if a[2] == KCMP_EPOLL_TFD {
        slot = copy_in::<12>(mem, a[4])?;
        slot.as_ptr() as u64
    } else {
        a[4]
    };
    // SAFETY: integers, and for KCMP_EPOLL_TFD a pointer to a 12-byte
    // local copy.
    let r = unsafe { libc::syscall(libc::SYS_kcmp, a[0], a[1], a[2], a[3], idx2) };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

/// pidfd_send_signal(2): the optional `siginfo_t` (128 bytes, the same
/// layout for signals sent by a process) is copied.
fn pidfd_send_signal(
    mem: &AddressSpace,
    pidfd: u64,
    signal: u64,
    info: u64,
    flags: u64,
) -> SysResult {
    let copy;
    let info_ptr = if info != 0 {
        copy = copy_in::<128>(mem, info)?;
        copy.as_ptr() as u64
    } else {
        0
    };
    // SAFETY: integers and a null or local 128-byte siginfo.
    let r = unsafe { libc::syscall(SYS_PIDFD_SEND_SIGNAL, pidfd, signal, info_ptr, flags) };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

/// signalfd(2) and signalfd4(2), by the host: the guest's signal mask is
/// mirrored into the host's (`signal.rs`), so the signals the guest blocks
/// stay pending in the host kernel, where the host's signalfd reads them.
/// The mask is 8 bytes; SFD_CLOEXEC and SFD_NONBLOCK are O_CLOEXEC and
/// O_NONBLOCK, the same values on both. The signals armor keeps for
/// itself are left out of the host's mask.
fn signalfd(mem: &AddressSpace, fd: u64, mask: u64, size: u64, flags: u64) -> SysResult {
    if size != 8 {
        return Err(err(libc::EINVAL));
    }
    if flags & !((libc::O_CLOEXEC | libc::O_NONBLOCK) as u64) != 0 {
        return Err(err(libc::EINVAL));
    }
    let set = crate::signal::host_set(u64::from_le_bytes(copy_in::<8>(mem, mask)?));
    // SAFETY: set is a valid sigset_t; the flags were checked.
    let r = unsafe { libc::signalfd(fd as i32, &set, flags as i32) };
    if r < 0 {
        Err(host_errno())
    } else {
        Ok(i64::from(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handled_numbers_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        assert!(HANDLED.iter().all(|n| seen.insert(*n)));
    }
}
