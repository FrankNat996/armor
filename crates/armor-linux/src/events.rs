//! Waiting for file descriptors and timers (docs/tech/syscalls.md):
//! select, poll, epoll, eventfd, inotify, alarm. AArch64 lacks `select`,
//! `poll`,
//! `epoll_create`, `epoll_wait`, `eventfd`, `inotify_init` and `alarm`,
//! and its `struct epoll_event` is 16 bytes where x86-64's is 12 (packed),
//! so every epoll call that carries events converts them.

use armor_mem::{AddressSpace, GuestMemory};

use crate::net::Word;
use crate::root::GuestRoot;
use crate::syscall::{SysResult, copy_in, copy_out, err, host_errno, read_path};

/// x86-64's packed `struct epoll_event`: u32 events, u64 data.
const GUEST_EPOLL_EVENT: usize = 12;

/// The kernel's limit on maxevents: INT_MAX / sizeof(struct epoll_event).
const EP_MAX_EVENTS: u64 = i32::MAX as u64 / GUEST_EPOLL_EVENT as u64;

/// Events returned by one epoll wait at most (the kernel may return fewer
/// than asked for): bounds the host buffer.
const HOST_EVENTS: usize = 1024;

/// The most file descriptors a select set may describe here: bounds the
/// copies (the kernel clips nfds to the descriptor table's size).
const MAX_SELECT_FDS: u64 = 1 << 20;

const EPOLL_CTL_DEL: u64 = 2;

fn check(result: libc::c_long) -> SysResult {
    if result < 0 {
        Err(host_errno())
    } else {
        Ok(result)
    }
}

/// How a waiting call gives its timeout: the structure, and the width of
/// its two fields (`long`s, or 64-bit in the time64 calls of i386).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Timeout {
    /// select and _newselect: a `struct timeval`, normalized as Linux
    /// does (microseconds over a million carry into seconds).
    Timeval(Word),
    /// pselect6 and ppoll: a `struct timespec`.
    Timespec(Word),
}

/// A pair of signed time fields (seconds, then micro- or nanoseconds) of
/// width `word` at `addr`.
fn read_time(mem: &AddressSpace, addr: u64, word: Word) -> Result<(i64, i64), i64> {
    Ok(match word {
        Word::W64 => {
            let t = copy_in::<16>(mem, addr)?;
            (
                i64::from_le_bytes(t[..8].try_into().unwrap()),
                i64::from_le_bytes(t[8..].try_into().unwrap()),
            )
        }
        Word::W32 => {
            let t = copy_in::<8>(mem, addr)?;
            (
                i64::from(i32::from_le_bytes(t[..4].try_into().unwrap())),
                i64::from(i32::from_le_bytes(t[4..].try_into().unwrap())),
            )
        }
    })
}

fn write_time(
    mem: &AddressSpace,
    addr: u64,
    word: Word,
    (sec, frac): (i64, i64),
) -> Result<(), i64> {
    match word {
        Word::W64 => copy_out(mem, addr, &[sec, frac].map(i64::to_le_bytes).concat()),
        Word::W32 => copy_out(
            mem,
            addr,
            &[sec as i32, frac as i32].map(i32::to_le_bytes).concat(),
        ),
    }
}

/// The host timespec of a guest timeout (`None` for a null pointer).
fn read_timeout(
    mem: &AddressSpace,
    addr: u64,
    kind: Timeout,
) -> Result<Option<libc::timespec>, i64> {
    if addr == 0 {
        return Ok(None);
    }
    Ok(Some(match kind {
        Timeout::Timeval(word) => {
            let (sec, usec) = read_time(mem, addr, word)?;
            // A negative result is EINVAL from the host's pselect6, as
            // from Linux's select.
            libc::timespec {
                tv_sec: sec.wrapping_add(usec / 1_000_000),
                tv_nsec: (usec % 1_000_000) * 1000,
            }
        }
        // Checked by the host as by Linux: negative, or nanoseconds over a
        // billion, is EINVAL, and the timespec is left as it was.
        Timeout::Timespec(word) => {
            let (sec, nsec) = read_time(mem, addr, word)?;
            libc::timespec {
                tv_sec: sec,
                tv_nsec: nsec,
            }
        }
    }))
}

/// Writes back the time left, as Linux does for select, pselect and ppoll
/// (even after EINTR).
fn write_timeout(
    mem: &AddressSpace,
    addr: u64,
    kind: Timeout,
    left: &libc::timespec,
) -> Result<(), i64> {
    match kind {
        Timeout::Timeval(word) => write_time(mem, addr, word, (left.tv_sec, left.tv_nsec / 1000)),
        Timeout::Timespec(word) => write_time(mem, addr, word, (left.tv_sec, left.tv_nsec)),
    }
}

/// select(2), _newselect and pselect6 through the host's pselect6. The
/// fd_set bitmaps are arrays of `long`s (`word`), little-endian: bit n is
/// in byte n / 8 at both widths, and an i386 set is a multiple of 4 bytes
/// where the host's is a multiple of 8.
#[allow(clippy::too_many_arguments)]
pub(crate) fn select(
    mem: &AddressSpace,
    word: Word,
    nfds: u64,
    read: u64,
    write: u64,
    except: u64,
    timeout: u64,
    kind: Timeout,
) -> SysResult {
    let nfds = nfds as i32;
    if nfds < 0 {
        return Err(err(libc::EINVAL));
    }
    let nfds = u64::from(nfds as u32).min(MAX_SELECT_FDS);
    let long_bits = word.bytes() as u64 * 8;
    let guest_bytes = (nfds.div_ceil(long_bits) * word.bytes() as u64) as usize;
    let host_bytes = (nfds.div_ceil(64) * 8) as usize;
    let mut sets = Vec::new();
    for addr in [read, write, except] {
        sets.push(if addr == 0 {
            None
        } else {
            let mut bits = vec![0u8; host_bytes];
            mem.read(addr, &mut bits[..guest_bytes])
                .map_err(|_| err(libc::EFAULT))?;
            Some(bits)
        });
    }
    let mut spec = read_timeout(mem, timeout, kind)?;
    let pointer = |set: &mut Option<Vec<u8>>| {
        set.as_mut()
            .map_or(std::ptr::null_mut(), |bits| bits.as_mut_ptr())
    };
    let [r, w, e] = [0, 1, 2].map(|i| pointer(&mut sets[i]));
    let spec_ptr = spec
        .as_mut()
        .map_or(std::ptr::null_mut(), |s| s as *mut libc::timespec);
    // SAFETY: each set pointer is null or `host_bytes` writable bytes,
    // enough for `nfds` bits; spec_ptr is null or a local timespec; no
    // signal mask.
    let n = unsafe {
        libc::syscall(
            libc::SYS_pselect6,
            nfds as i32,
            r,
            w,
            e,
            spec_ptr,
            std::ptr::null::<u8>(),
        )
    };
    if let Some(spec) = spec {
        write_timeout(mem, timeout, kind, &spec)?;
    }
    if n < 0 {
        return Err(host_errno());
    }
    for (addr, set) in [read, write, except].into_iter().zip(&sets) {
        if let Some(bits) = set {
            copy_out(mem, addr, &bits[..guest_bytes])?;
        }
    }
    Ok(n)
}

/// pselect6 and i386's pselect6_time64 (`time`: the timespec's width).
/// The sixth argument points to a pair (sigset pointer, size) of `long`s;
/// glibc passes the pair even without a mask. Without a signal mask the
/// call is select with a timespec; with one, `None` (not supported: the
/// guest's mask is armor's, not the host's).
pub(crate) fn pselect6(
    mem: &AddressSpace,
    word: Word,
    time: Word,
    a: [u64; 6],
) -> Option<SysResult> {
    if a[5] != 0 {
        let mask = match word {
            Word::W64 => copy_in::<8>(mem, a[5]).map(u64::from_le_bytes),
            Word::W32 => copy_in::<4>(mem, a[5]).map(|b| u64::from(u32::from_le_bytes(b))),
        };
        match mask {
            Ok(0) => {}
            Ok(_) => return None,
            Err(e) => return Some(Err(e)),
        }
    }
    Some(select(
        mem,
        word,
        a[0],
        a[1],
        a[2],
        a[3],
        a[4],
        Timeout::Timespec(time),
    ))
}

/// poll(2), absent on AArch64: the host's ppoll. `struct pollfd` (fd,
/// events, revents: 8 bytes) and the POLL* bits are the same on both, and
/// on i386. A negative timeout waits forever.
pub(crate) fn poll(mem: &AddressSpace, fds: u64, count: u64, timeout: u64) -> SysResult {
    let ms = i64::from(timeout as i32);
    let mut spec = (ms >= 0).then_some(libc::timespec {
        tv_sec: ms / 1000,
        tv_nsec: ms % 1000 * 1_000_000,
    });
    poll_host(mem, fds, count, spec.as_mut(), None)
}

/// ppoll(2), i386's ppoll and ppoll_time64 (`time`: the timespec's
/// width), the time left written back. With a signal mask, `None` (not
/// supported: the guest's mask is armor's, not the host's).
pub(crate) fn ppoll(
    mem: &AddressSpace,
    time: Word,
    fds: u64,
    count: u64,
    timeout: u64,
    host_mask: Option<&libc::sigset_t>,
) -> SysResult {
    let kind = Timeout::Timespec(time);
    let mut spec = read_timeout(mem, timeout, kind)?;
    let result = poll_host(mem, fds, count, spec.as_mut(), host_mask);
    match spec {
        Some(left) => write_timeout(mem, timeout, kind, &left).and(result),
        None => result,
    }
}

fn poll_host(
    mem: &AddressSpace,
    fds: u64,
    count: u64,
    spec: Option<&mut libc::timespec>,
    host_mask: Option<&libc::sigset_t>,
) -> SysResult {
    const POLLFD_SIZE: u64 = 8;
    // The kernel's limit is RLIMIT_NOFILE; this bounds the host buffer.
    if count > 1 << 20 {
        return Err(err(libc::EINVAL));
    }
    let mut entries = vec![0u8; (count * POLLFD_SIZE) as usize];
    mem.read(fds, &mut entries).map_err(|_| err(libc::EFAULT))?;
    let spec_ptr = spec.map_or(std::ptr::null_mut(), |s| s as *mut libc::timespec);
    let mask_ptr = host_mask.map_or(std::ptr::null(), |m| m as *const libc::sigset_t);
    // SAFETY: entries holds `count` pollfd records (same layout on the
    // host), writable by ppoll; spec_ptr is null or a timespec; mask_ptr is
    // null or a sigset whose first 8 bytes are the kernel's mask.
    let n = unsafe {
        libc::syscall(
            libc::SYS_ppoll,
            entries.as_mut_ptr(),
            count as libc::nfds_t,
            spec_ptr,
            mask_ptr,
            8usize,
        )
    };
    if n < 0 {
        return Err(host_errno());
    }
    copy_out(mem, fds, &entries)?;
    Ok(n)
}

/// epoll_create(2): `size` is only checked, as in Linux.
pub fn epoll_create(size: u64) -> SysResult {
    if (size as i32) <= 0 {
        return Err(err(libc::EINVAL));
    }
    epoll_create1(0)
}

/// epoll_create1(2): EPOLL_CLOEXEC is O_CLOEXEC on both architectures.
pub fn epoll_create1(flags: u64) -> SysResult {
    // SAFETY: integer arguments only.
    check(unsafe { libc::syscall(libc::SYS_epoll_create1, flags as i32) })
}

/// epoll_ctl(2): the guest's 12-byte event becomes the host's.
pub fn epoll_ctl(mem: &AddressSpace, epfd: u64, op: u64, fd: u64, event: u64) -> SysResult {
    let mut host_event = libc::epoll_event { events: 0, u64: 0 };
    // EPOLL_CTL_DEL ignores the event, which may be null.
    if op != EPOLL_CTL_DEL {
        let bytes = copy_in::<GUEST_EPOLL_EVENT>(mem, event)?;
        host_event.events = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        host_event.u64 = u64::from_le_bytes(bytes[4..].try_into().unwrap());
    }
    // SAFETY: host_event is a valid local epoll_event.
    check(unsafe {
        libc::syscall(
            libc::SYS_epoll_ctl,
            epfd as i32,
            op as i32,
            fd as i32,
            &mut host_event as *mut libc::epoll_event,
        )
    })
}

/// epoll_wait, epoll_pwait and epoll_pwait2, without a signal mask (the
/// guest's mask is armor's, not the host's: `None`, not supported). With
/// `timespec`, `timeout` is the address of a `struct timespec` (pwait2),
/// else milliseconds.
pub fn epoll_wait(
    mem: &AddressSpace,
    epfd: u64,
    events: u64,
    max: u64,
    timeout: u64,
    timespec: bool,
    sigmask: u64,
) -> Option<SysResult> {
    if sigmask != 0 {
        return None;
    }
    Some(epoll_wait_host(mem, epfd, events, max, timeout, timespec))
}

fn epoll_wait_host(
    mem: &AddressSpace,
    epfd: u64,
    events: u64,
    max: u64,
    timeout: u64,
    timespec: bool,
) -> SysResult {
    let max = max as i32;
    if max <= 0 || max as u64 > EP_MAX_EVENTS {
        return Err(err(libc::EINVAL));
    }
    let mut host_events =
        vec![libc::epoll_event { events: 0, u64: 0 }; (max as usize).min(HOST_EVENTS)];
    let n = if timespec {
        let spec = if timeout == 0 {
            None
        } else {
            Some(copy_in::<16>(mem, timeout)?)
        };
        let spec_ptr = spec.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
        // SAFETY: host_events is writable for its length; spec_ptr is null
        // or a `struct timespec` (same layout on both); no signal mask.
        unsafe {
            libc::syscall(
                libc::SYS_epoll_pwait2,
                epfd as i32,
                host_events.as_mut_ptr(),
                host_events.len() as i32,
                spec_ptr,
                std::ptr::null::<u8>(),
                0usize,
            )
        }
    } else {
        // SAFETY: host_events is writable for its length; no signal mask.
        unsafe {
            libc::syscall(
                libc::SYS_epoll_pwait,
                epfd as i32,
                host_events.as_mut_ptr(),
                host_events.len() as i32,
                timeout as i32,
                std::ptr::null::<u8>(),
                0usize,
            )
        }
    };
    if n < 0 {
        return Err(host_errno());
    }
    let mut out = Vec::with_capacity(n as usize * GUEST_EPOLL_EVENT);
    for event in &host_events[..n as usize] {
        let (kinds, data) = (event.events, event.u64);
        out.extend_from_slice(&kinds.to_le_bytes());
        out.extend_from_slice(&data.to_le_bytes());
    }
    copy_out(mem, events, &out)?;
    Ok(n)
}

/// eventfd and eventfd2: EFD_CLOEXEC, EFD_NONBLOCK and EFD_SEMAPHORE have
/// the same values on both.
pub fn eventfd2(count: u64, flags: u64) -> SysResult {
    // SAFETY: integer arguments only.
    check(unsafe { libc::syscall(libc::SYS_eventfd2, count as u32, flags as i32) })
}

/// inotify_init and inotify_init1: IN_CLOEXEC and IN_NONBLOCK are the
/// O_* values, the same on both. Events (`struct inotify_event`) are read
/// with read(2) and have the same layout.
pub fn inotify_init1(flags: u64) -> SysResult {
    // SAFETY: integer arguments only.
    check(unsafe { libc::syscall(libc::SYS_inotify_init1, flags as i32) })
}

/// inotify_add_watch(2), with the path resolved in the guest root.
pub fn inotify_add_watch(
    mem: &AddressSpace,
    root: &GuestRoot,
    fd: u64,
    path: u64,
    mask: u64,
) -> SysResult {
    let path = root.resolve_c(read_path(mem, path)?);
    // SAFETY: path is NUL-terminated; the other arguments are integers.
    check(unsafe {
        libc::syscall(
            libc::SYS_inotify_add_watch,
            fd as i32,
            path.as_ptr(),
            mask as u32,
        )
    })
}

/// alarm(2), absent on AArch64: glibc's alarm, which uses setitimer and
/// rounds the time left like Linux's alarm (to the nearest second, and at
/// least 1 if any time was left). SIGALRM comes from the host's timer.
pub fn alarm(seconds: u64) -> SysResult {
    // SAFETY: alarm has no memory arguments.
    Ok(i64::from(unsafe { libc::alarm(seconds as u32) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoll_limits_match_the_x86_64_kernel() {
        assert_eq!(GUEST_EPOLL_EVENT, 12);
        assert_eq!(EP_MAX_EVENTS, 178_956_970);
    }
}
