//! Processes, messages and waits added by ADR-0044: `execveat`,
//! `openat2`, `process_vm_readv`/`writev`, `process_madvise`,
//! `sched_setattr`/`getattr`, `sendmmsg`/`recvmmsg`, `futex_waitv` and the
//! futex2 calls (`futex_wake`, `futex_wait`, `futex_requeue`).
//!
//! The futex calls follow ADR-0018: the words are checked (aligned,
//! mapped writable in the guest), then the host kernel gets the guest's
//! addresses, which are armor's own identity-mapped memory.

use armor_mem::{AddressSpace, GuestMemory};

use super::{
    Process, SysResult, copy_in, copy_out, err, futex_word, host_errno, open_flags_to_host,
    read_path,
};
use crate::net::{self, Word};

pub mod nr {
    pub const SCHED_SETATTR: u64 = 314;
    pub const SCHED_GETATTR: u64 = 315;
    pub const RECVMMSG: u64 = 299;
    pub const SENDMMSG: u64 = 307;
    pub const PROCESS_VM_READV: u64 = 310;
    pub const PROCESS_VM_WRITEV: u64 = 311;
    pub const EXECVEAT: u64 = 322;
    pub const OPENAT2: u64 = 437;
    pub const PROCESS_MADVISE: u64 = 440;
    pub const FUTEX_WAITV: u64 = 449;
    pub const FUTEX_WAKE: u64 = 454;
    pub const FUTEX_WAIT: u64 = 455;
    pub const FUTEX_REQUEUE: u64 = 456;
}

pub const HANDLED: &[u64] = &[
    nr::SCHED_SETATTR,
    nr::SCHED_GETATTR,
    nr::RECVMMSG,
    nr::SENDMMSG,
    nr::PROCESS_VM_READV,
    nr::PROCESS_VM_WRITEV,
    nr::EXECVEAT,
    nr::OPENAT2,
    nr::PROCESS_MADVISE,
    nr::FUTEX_WAITV,
    nr::FUTEX_WAKE,
    nr::FUTEX_WAIT,
    nr::FUTEX_REQUEUE,
];

// Host numbers (one number on every architecture from 424 on).
const SYS_OPENAT2: libc::c_long = 437;
const SYS_PROCESS_MADVISE: libc::c_long = 440;
const SYS_FUTEX_WAITV: libc::c_long = 449;
const SYS_FUTEX_WAKE: libc::c_long = 454;
const SYS_FUTEX_WAIT: libc::c_long = 455;
const SYS_FUTEX_REQUEUE: libc::c_long = 456;

/// IOV_MAX, and the most waiters of futex_waitv.
const IOV_MAX: u64 = 1024;
const FUTEX_WAITV_MAX: u64 = 128;

pub(super) fn dispatch(
    mem: &AddressSpace,
    process: &Process,
    number: u64,
    a: [u64; 6],
) -> Option<Result<SysResult, String>> {
    let result = match number {
        nr::EXECVEAT => execveat(mem, process, a),
        nr::OPENAT2 => openat2(mem, process, a[0], a[1], a[2], a[3]),
        nr::PROCESS_VM_READV => process_vm(mem, a, false),
        nr::PROCESS_VM_WRITEV => process_vm(mem, a, true),
        nr::PROCESS_MADVISE => process_madvise(mem, a),
        nr::SCHED_SETATTR => sched_setattr(mem, a[0], a[1], a[2]),
        nr::SCHED_GETATTR => sched_getattr(mem, a[0], a[1], a[2], a[3]),
        nr::SENDMMSG => sendmmsg(mem, a[0], a[1], a[2], a[3]),
        nr::RECVMMSG => recvmmsg(mem, a[0], a[1], a[2], a[3], a[4]),
        nr::FUTEX_WAITV => futex_waitv(mem, a),
        nr::FUTEX_WAKE => futex2_word(mem, a[0], a[3])
            .and_then(|()| raw(SYS_FUTEX_WAKE, &[a[0], a[1], a[2], a[3]])),
        nr::FUTEX_WAIT => futex_wait(mem, a),
        nr::FUTEX_REQUEUE => futex_requeue(mem, a),
        _ => return None,
    };
    Some(Ok(result))
}

fn raw(number: libc::c_long, args: &[u64]) -> SysResult {
    let arg = |i: usize| args.get(i).copied().unwrap_or(0);
    // SAFETY: callers pass integers, pointers to local copies, or guest
    // futex addresses checked as ADR-0018 requires.
    let r = unsafe { libc::syscall(number, arg(0), arg(1), arg(2), arg(3), arg(4), arg(5)) };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

/// execveat(2): dirfd, path, argv, envp, flags. With AT_EMPTY_PATH and an
/// empty path, or a relative path from a directory descriptor, the
/// program is named through `/proc/self/fd/`, as the kernel does; a
/// descriptor that would close at exec (fexecve's usual case) is first
/// duplicated without O_CLOEXEC, so that the armor started for the program
/// can still open it.
fn execveat(mem: &AddressSpace, process: &Process, a: [u64; 6]) -> SysResult {
    const AT_EMPTY_PATH: u64 = 0x1000;
    const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
    let (dirfd, flags) = (a[0] as i32, a[4]);
    if flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0 {
        return Err(err(libc::EINVAL));
    }
    let path = read_path(mem, a[1])?.into_bytes();
    let guest_path = if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            return Err(err(libc::ENOENT));
        }
        // SAFETY: integers.
        let open = unsafe { libc::fcntl(dirfd, libc::F_GETFD) };
        if open < 0 {
            return Err(host_errno());
        }
        let fd = if open & libc::FD_CLOEXEC != 0 {
            // SAFETY: integers.
            let copy = unsafe { libc::fcntl(dirfd, libc::F_DUPFD, 0) };
            if copy < 0 {
                return Err(host_errno());
            }
            copy
        } else {
            dirfd
        };
        format!("/proc/self/fd/{fd}").into_bytes()
    } else if path[0] == b'/' || dirfd == libc::AT_FDCWD {
        path
    } else {
        let mut base = format!("/proc/self/fd/{dirfd}/").into_bytes();
        base.extend(path);
        base
    };
    if flags & AT_SYMLINK_NOFOLLOW != 0 {
        let host = process.root.resolve(&guest_path);
        if std::fs::symlink_metadata(std::ffi::OsStr::new(
            std::str::from_utf8(&host).unwrap_or_default(),
        ))
        .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(err(libc::ELOOP));
        }
    }
    crate::process::execve_path(mem, process, guest_path, a[2], a[3], 8)
}

/// openat2(2): `struct open_how` (flags, mode, resolve: three u64) of at
/// least 24 bytes; larger structures must be zero past the known fields
/// (E2BIG). The flags are translated, an absolute path resolved in the
/// guest root; RESOLVE_* flags are the same on both.
fn openat2(
    mem: &AddressSpace,
    process: &Process,
    dirfd: u64,
    path: u64,
    how: u64,
    size: u64,
) -> SysResult {
    const VER0: u64 = 24;
    if !(VER0..=4096).contains(&size) {
        return Err(err(if size < VER0 {
            libc::EINVAL
        } else {
            libc::E2BIG
        }));
    }
    let mut bytes = vec![0u8; size as usize];
    mem.read(how, &mut bytes).map_err(|_| err(libc::EFAULT))?;
    if bytes[VER0 as usize..].iter().any(|&b| b != 0) {
        return Err(err(libc::E2BIG));
    }
    let field = |i: usize| u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
    let host_how = [open_flags_to_host(field(0)), field(1), field(2)];
    let path = process.root.resolve_c(read_path(mem, path)?);
    raw(
        SYS_OPENAT2,
        &[dirfd, path.as_ptr() as u64, host_how.as_ptr() as u64, VER0],
    )
}

/// The guest's iovecs at `iov` (16 bytes each, the same on both): base
/// and length, checked as Linux does.
fn iovecs(mem: &AddressSpace, iov: u64, count: u64) -> Result<Vec<(u64, u64)>, i64> {
    if count > IOV_MAX {
        return Err(err(libc::EINVAL));
    }
    let mut raw = vec![0u8; count as usize * 16];
    mem.read(iov, &mut raw).map_err(|_| err(libc::EFAULT))?;
    raw.chunks(16)
        .map(|e| {
            let base = u64::from_le_bytes(e[..8].try_into().expect("8 bytes"));
            let len = u64::from_le_bytes(e[8..].try_into().expect("8 bytes"));
            if (len as i64) < 0 {
                Err(err(libc::EINVAL))
            } else {
                Ok((base, len))
            }
        })
        .collect()
}

/// process_vm_readv/writev(2): pid, local iov, count, remote iov, count,
/// flags. The local side is this guest's memory, moved through one host
/// buffer; the remote side is the other process's addresses, given to the
/// host as they are (under armor they are that process's guest memory).
fn process_vm(mem: &AddressSpace, a: [u64; 6], write: bool) -> SysResult {
    if a[5] != 0 {
        return Err(err(libc::EINVAL));
    }
    let local = iovecs(mem, a[1], a[2])?;
    let remote = iovecs(mem, a[3], a[4])?;
    let total: u64 = local.iter().map(|(_, l)| l).sum();
    let mut buffer = vec![0u8; total.min(16 << 20) as usize];
    if write {
        let mut at = 0usize;
        for &(base, len) in &local {
            let end = (at + len as usize).min(buffer.len());
            mem.read(base, &mut buffer[at..end])
                .map_err(|_| err(libc::EFAULT))?;
            at = end;
        }
    }
    let host_local = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    };
    let host_remote: Vec<libc::iovec> = remote
        .iter()
        .map(|&(base, len)| libc::iovec {
            iov_base: base as *mut libc::c_void,
            iov_len: len as usize,
        })
        .collect();
    let number = if write {
        libc::SYS_process_vm_writev
    } else {
        libc::SYS_process_vm_readv
    };
    // SAFETY: one local iovec over `buffer`; the remote iovecs name the
    // other process's memory, which the host checks.
    let n = unsafe {
        libc::syscall(
            number,
            a[0],
            &host_local,
            1,
            host_remote.as_ptr(),
            host_remote.len(),
            0,
        )
    };
    if n < 0 {
        return Err(host_errno());
    }
    if !write {
        let mut at = 0usize;
        for &(base, len) in &local {
            if at >= n as usize {
                break;
            }
            let end = (at + len as usize).min(n as usize);
            copy_out(mem, base, &buffer[at..end])?;
            at = end;
        }
    }
    Ok(n)
}

/// process_madvise(2): pidfd, iovecs of the other process's addresses,
/// count, advice, flags.
fn process_madvise(mem: &AddressSpace, a: [u64; 6]) -> SysResult {
    let ranges: Vec<libc::iovec> = iovecs(mem, a[1], a[2])?
        .into_iter()
        .map(|(base, len)| libc::iovec {
            iov_base: base as *mut libc::c_void,
            iov_len: len as usize,
        })
        .collect();
    raw(
        SYS_PROCESS_MADVISE,
        &[
            a[0],
            ranges.as_ptr() as u64,
            ranges.len() as u64,
            a[3],
            a[4],
        ],
    )
}

/// sched_setattr(2): `struct sched_attr` starts with its size (48 bytes
/// in version 0, 56 with utilization clamps), the same layout on both.
fn sched_setattr(mem: &AddressSpace, pid: u64, attr: u64, flags: u64) -> SysResult {
    let size = u32::from_le_bytes(copy_in::<4>(mem, attr)?);
    let size = if size == 0 { 48 } else { size };
    let mut bytes = vec![0u8; (size as usize).clamp(48, 4096)];
    mem.read(attr, &mut bytes).map_err(|_| err(libc::EFAULT))?;
    raw(
        libc::SYS_sched_setattr,
        &[pid, bytes.as_ptr() as u64, flags],
    )
}

/// sched_getattr(2): pid, attr, size, flags.
fn sched_getattr(mem: &AddressSpace, pid: u64, attr: u64, size: u64, flags: u64) -> SysResult {
    if !(48..=4096).contains(&size) {
        return Err(err(libc::EINVAL));
    }
    let mut bytes = vec![0u8; size as usize];
    raw(
        libc::SYS_sched_getattr,
        &[pid, bytes.as_mut_ptr() as u64, size, flags],
    )?;
    let written = u32::from_le_bytes(bytes[..4].try_into().expect("4 bytes")) as usize;
    copy_out(mem, attr, &bytes[..written.clamp(48, size as usize)])?;
    Ok(0)
}

/// `struct mmsghdr`: a `struct msghdr` (56 bytes) and the length sent or
/// received, padded to 64.
const MMSGHDR: u64 = 64;

/// sendmmsg(2): each message through armor's sendmsg; the count sent, or
/// the error of the first.
fn sendmmsg(mem: &AddressSpace, fd: u64, vec: u64, count: u64, flags: u64) -> SysResult {
    let count = count.min(IOV_MAX);
    let mut sent = 0;
    while sent < count {
        let entry = vec + sent * MMSGHDR;
        match net::sendmsg(mem, fd, entry, flags, Word::W64) {
            Ok(n) => copy_out(mem, entry + 56, &(n as u32).to_le_bytes())?,
            Err(e) if sent == 0 => return Err(e),
            Err(_) => break,
        }
        sent += 1;
    }
    Ok(sent as i64)
}

/// recvmmsg(2): each message through armor's recvmsg. MSG_WAITFORONE
/// makes the messages after the first non-blocking; the timeout, as in
/// Linux, is checked after each message (it does not bound a blocking
/// receive), and what remains of it is written back.
fn recvmmsg(
    mem: &AddressSpace,
    fd: u64,
    vec: u64,
    count: u64,
    flags: u64,
    timeout: u64,
) -> SysResult {
    const MSG_WAITFORONE: u64 = 0x10000;
    const MSG_DONTWAIT: u64 = 0x40;
    let deadline = if timeout != 0 {
        let t = copy_in::<16>(mem, timeout)?;
        let sec = i64::from_le_bytes(t[..8].try_into().expect("8 bytes"));
        let nsec = i64::from_le_bytes(t[8..].try_into().expect("8 bytes"));
        if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
            return Err(err(libc::EINVAL));
        }
        Some(std::time::Instant::now() + std::time::Duration::new(sec as u64, nsec as u32))
    } else {
        None
    };
    let count = count.min(IOV_MAX);
    let wait_for_one = flags & MSG_WAITFORONE != 0;
    let mut flags = flags & !MSG_WAITFORONE;
    let mut received = 0;
    while received < count {
        let entry = vec + received * MMSGHDR;
        match net::recvmsg(mem, fd, entry, flags, Word::W64) {
            Ok(n) => copy_out(mem, entry + 56, &(n as u32).to_le_bytes())?,
            Err(e) if received == 0 => return Err(e),
            Err(_) => break,
        }
        received += 1;
        if wait_for_one {
            flags |= MSG_DONTWAIT;
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
    }
    if let Some(deadline) = deadline {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let mut t = [0u8; 16];
        t[..8].copy_from_slice(&(left.as_secs() as i64).to_le_bytes());
        t[8..].copy_from_slice(&i64::from(left.subsec_nanos()).to_le_bytes());
        copy_out(mem, timeout, &t)?;
    }
    Ok(received as i64)
}

/// A futex2 word at `addr` of the size in `flags` (FUTEX2_SIZE_U8..U64 in
/// the low two bits): aligned and writable in the guest.
fn futex2_word(mem: &AddressSpace, addr: u64, flags: u64) -> Result<(), i64> {
    let size = 1u64 << (flags & 3);
    if !addr.is_multiple_of(size) {
        return Err(err(libc::EINVAL));
    }
    futex_word(mem, addr & !3)
}

/// futex_waitv(2): `struct futex_waitv` (val, uaddr, flags, reserved: 24
/// bytes, the same on both), at most 128; an absolute timeout.
fn futex_waitv(mem: &AddressSpace, a: [u64; 6]) -> SysResult {
    let (count, flags) = (a[1], a[2]);
    if count == 0 || count > FUTEX_WAITV_MAX || flags != 0 {
        return Err(err(libc::EINVAL));
    }
    let mut waiters = vec![0u8; count as usize * 24];
    mem.read(a[0], &mut waiters)
        .map_err(|_| err(libc::EFAULT))?;
    for w in waiters.chunks(24) {
        let uaddr = u64::from_le_bytes(w[8..16].try_into().expect("8 bytes"));
        let wflags = u32::from_le_bytes(w[16..20].try_into().expect("4 bytes"));
        futex2_word(mem, uaddr, u64::from(wflags))?;
    }
    let timeout = if a[3] != 0 {
        Some(copy_in::<16>(mem, a[3])?)
    } else {
        None
    };
    let timeout_ptr = timeout.as_ref().map_or(0, |t| t.as_ptr() as u64);
    raw(
        SYS_FUTEX_WAITV,
        &[waiters.as_ptr() as u64, count, flags, timeout_ptr, a[4]],
    )
}

/// futex_wait(2): uaddr, val, mask, flags, timeout, clockid.
fn futex_wait(mem: &AddressSpace, a: [u64; 6]) -> SysResult {
    futex2_word(mem, a[0], a[3])?;
    let timeout = if a[4] != 0 {
        Some(copy_in::<16>(mem, a[4])?)
    } else {
        None
    };
    let timeout_ptr = timeout.as_ref().map_or(0, |t| t.as_ptr() as u64);
    raw(SYS_FUTEX_WAIT, &[a[0], a[1], a[2], a[3], timeout_ptr, a[5]])
}

/// futex_requeue(2): two `struct futex_waitv`, flags, nr_wake, nr_requeue.
fn futex_requeue(mem: &AddressSpace, a: [u64; 6]) -> SysResult {
    let mut pair = [0u8; 48];
    mem.read(a[0], &mut pair).map_err(|_| err(libc::EFAULT))?;
    for w in pair.chunks(24) {
        let uaddr = u64::from_le_bytes(w[8..16].try_into().expect("8 bytes"));
        let wflags = u32::from_le_bytes(w[16..20].try_into().expect("4 bytes"));
        futex2_word(mem, uaddr, u64::from(wflags))?;
    }
    raw(SYS_FUTEX_REQUEUE, &[pair.as_ptr() as u64, a[1], a[2], a[3]])
}
