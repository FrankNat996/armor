//! Message queues (ADR-0044): System V (`msgget`, `msgsnd`, `msgrcv`,
//! `msgctl`) and POSIX (`mq_open` and its five companions). The queues are
//! the host's, shared with every other process. `struct msqid64_ds` (120
//! bytes), `struct msginfo` (32), `struct mq_attr` (64) and `struct
//! sigevent` (64) have the same layout on x86-64 and AArch64 (✅ `sizeof`
//! and `offsetof` under both compilers, 2026-10-15); `struct msgbuf` is a
//! `long` type followed by the text.

use armor_mem::{AddressSpace, GuestMemory};

use super::{SysResult, copy_in, copy_out, err, host_errno, open_flags_to_host, read_path};

pub mod nr {
    pub const MSGGET: u64 = 68;
    pub const MSGSND: u64 = 69;
    pub const MSGRCV: u64 = 70;
    pub const MSGCTL: u64 = 71;
    pub const MQ_OPEN: u64 = 240;
    pub const MQ_UNLINK: u64 = 241;
    pub const MQ_TIMEDSEND: u64 = 242;
    pub const MQ_TIMEDRECEIVE: u64 = 243;
    pub const MQ_NOTIFY: u64 = 244;
    pub const MQ_GETSETATTR: u64 = 245;
}

pub const HANDLED: &[u64] = &[
    nr::MSGGET,
    nr::MSGSND,
    nr::MSGRCV,
    nr::MSGCTL,
    nr::MQ_OPEN,
    nr::MQ_UNLINK,
    nr::MQ_TIMEDSEND,
    nr::MQ_TIMEDRECEIVE,
    nr::MQ_NOTIFY,
    nr::MQ_GETSETATTR,
];

/// The largest message armor moves in one call; the host refuses larger
/// ones itself (`msgmax`, `msgsize_max` are far smaller by default).
const MESSAGE_MAX: u64 = 1 << 20;

pub(super) fn dispatch(
    mem: &AddressSpace,
    number: u64,
    a: [u64; 6],
) -> Option<Result<SysResult, String>> {
    let result = match number {
        nr::MSGGET => raw(libc::SYS_msgget, &[a[0], a[1]]),
        nr::MSGSND => msgsnd(mem, a[0], a[1], a[2], a[3]),
        nr::MSGRCV => msgrcv(mem, a[0], a[1], a[2], a[3], a[4]),
        nr::MSGCTL => return msgctl(mem, a[0], a[1], a[2]).map(Ok),
        nr::MQ_OPEN => mq_open(mem, a[0], a[1], a[2], a[3]),
        nr::MQ_UNLINK => {
            read_path(mem, a[0]).and_then(|name| raw(libc::SYS_mq_unlink, &[name.as_ptr() as u64]))
        }
        nr::MQ_TIMEDSEND => mq_timedsend(mem, a),
        nr::MQ_TIMEDRECEIVE => mq_timedreceive(mem, a),
        nr::MQ_NOTIFY => mq_notify(mem, a[0], a[1]),
        nr::MQ_GETSETATTR => mq_getsetattr(mem, a[0], a[1], a[2]),
        _ => return None,
    };
    Some(Ok(result))
}

fn raw(number: libc::c_long, args: &[u64]) -> SysResult {
    let arg = |i: usize| args.get(i).copied().unwrap_or(0);
    // SAFETY: callers pass integers and pointers to local copies.
    let r = unsafe { libc::syscall(number, arg(0), arg(1), arg(2), arg(3), arg(4)) };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

fn msgsnd(mem: &AddressSpace, id: u64, msg: u64, size: u64, flags: u64) -> SysResult {
    if size > MESSAGE_MAX {
        return Err(err(libc::EINVAL));
    }
    let mut bytes = vec![0u8; 8 + size as usize];
    mem.read(msg, &mut bytes).map_err(|_| err(libc::EFAULT))?;
    raw(libc::SYS_msgsnd, &[id, bytes.as_ptr() as u64, size, flags])
}

fn msgrcv(mem: &AddressSpace, id: u64, msg: u64, size: u64, kind: u64, flags: u64) -> SysResult {
    let size = size.min(MESSAGE_MAX);
    let mut bytes = vec![0u8; 8 + size as usize];
    let n = raw(
        libc::SYS_msgrcv,
        &[id, bytes.as_mut_ptr() as u64, size, kind, flags],
    )?;
    copy_out(mem, msg, &bytes[..8 + n as usize])?;
    Ok(n)
}

/// msgctl(2): the commands without a buffer and those with a `struct
/// msqid64_ds` or `struct msginfo`.
fn msgctl(mem: &AddressSpace, id: u64, cmd: u64, buf: u64) -> Option<SysResult> {
    const IPC_RMID: u64 = 0;
    const IPC_SET: u64 = 1;
    const IPC_STAT: u64 = 2;
    const IPC_INFO: u64 = 3;
    const MSG_STAT: u64 = 11;
    const MSG_INFO: u64 = 12;
    const MSG_STAT_ANY: u64 = 13;
    let command = cmd & !0x100; // IPC_64
    Some(match command {
        IPC_RMID => raw(libc::SYS_msgctl, &[id, cmd, 0]),
        IPC_SET => copy_in::<120>(mem, buf)
            .and_then(|ds| raw(libc::SYS_msgctl, &[id, cmd, ds.as_ptr() as u64])),
        IPC_STAT | MSG_STAT | MSG_STAT_ANY | IPC_INFO | MSG_INFO => {
            let mut out = [0u8; 120];
            raw(libc::SYS_msgctl, &[id, cmd, out.as_mut_ptr() as u64]).and_then(|r| {
                let size = if matches!(command, IPC_INFO | MSG_INFO) {
                    32
                } else {
                    120
                };
                copy_out(mem, buf, &out[..size]).map(|()| r)
            })
        }
        _ => Err(err(libc::EINVAL)),
    })
}

/// mq_open(2): name, flags (translated), mode, optional `struct mq_attr`.
fn mq_open(mem: &AddressSpace, name: u64, flags: u64, mode: u64, attr: u64) -> SysResult {
    let name = read_path(mem, name)?;
    let attr = if attr != 0 {
        Some(copy_in::<64>(mem, attr)?)
    } else {
        None
    };
    let attr_ptr = attr.as_ref().map_or(0, |a| a.as_ptr() as u64);
    raw(
        libc::SYS_mq_open,
        &[
            name.as_ptr() as u64,
            open_flags_to_host(flags),
            mode,
            attr_ptr,
        ],
    )
}

/// An optional absolute `struct timespec` (16 bytes, the same on both).
fn timespec(mem: &AddressSpace, at: u64) -> Result<Option<[u8; 16]>, i64> {
    if at == 0 {
        Ok(None)
    } else {
        copy_in::<16>(mem, at).map(Some)
    }
}

/// mq_timedsend(2): mqdes, msg, len, priority, timeout.
fn mq_timedsend(mem: &AddressSpace, a: [u64; 6]) -> SysResult {
    if a[2] > MESSAGE_MAX {
        return Err(err(libc::EMSGSIZE));
    }
    let mut bytes = vec![0u8; a[2] as usize];
    mem.read(a[1], &mut bytes).map_err(|_| err(libc::EFAULT))?;
    let timeout = timespec(mem, a[4])?;
    let timeout_ptr = timeout.as_ref().map_or(0, |t| t.as_ptr() as u64);
    raw(
        libc::SYS_mq_timedsend,
        &[a[0], bytes.as_ptr() as u64, a[2], a[3], timeout_ptr],
    )
}

/// mq_timedreceive(2): mqdes, msg, len, priority (out), timeout.
fn mq_timedreceive(mem: &AddressSpace, a: [u64; 6]) -> SysResult {
    let mut bytes = vec![0u8; a[2].min(MESSAGE_MAX) as usize];
    let mut priority = 0u32;
    let timeout = timespec(mem, a[4])?;
    let timeout_ptr = timeout.as_ref().map_or(0, |t| t.as_ptr() as u64);
    let n = raw(
        libc::SYS_mq_timedreceive,
        &[
            a[0],
            bytes.as_mut_ptr() as u64,
            a[2],
            &raw mut priority as u64,
            timeout_ptr,
        ],
    )?;
    copy_out(mem, a[1], &bytes[..n as usize])?;
    if a[3] != 0 {
        copy_out(mem, a[3], &priority.to_le_bytes())?;
    }
    Ok(n)
}

/// mq_notify(2): an optional `struct sigevent`. For SIGEV_THREAD the
/// kernel reads a 32-byte cookie at `sigev_value` (glibc's netlink
/// notification), which is copied too.
fn mq_notify(mem: &AddressSpace, mqdes: u64, event: u64) -> SysResult {
    const SIGEV_THREAD: i32 = 2;
    if event == 0 {
        return raw(libc::SYS_mq_notify, &[mqdes, 0]);
    }
    let mut sigevent = copy_in::<64>(mem, event)?;
    let notify = i32::from_le_bytes(sigevent[12..16].try_into().expect("4 bytes"));
    let cookie;
    if notify == SIGEV_THREAD {
        let at = u64::from_le_bytes(sigevent[..8].try_into().expect("8 bytes"));
        cookie = copy_in::<32>(mem, at)?;
        sigevent[..8].copy_from_slice(&(cookie.as_ptr() as u64).to_le_bytes());
    }
    raw(libc::SYS_mq_notify, &[mqdes, sigevent.as_ptr() as u64])
}

/// mq_getsetattr(2): mqdes, new attributes, old attributes.
fn mq_getsetattr(mem: &AddressSpace, mqdes: u64, new: u64, old: u64) -> SysResult {
    let new = if new != 0 {
        Some(copy_in::<64>(mem, new)?)
    } else {
        None
    };
    let new_ptr = new.as_ref().map_or(0, |n| n.as_ptr() as u64);
    let mut previous = [0u8; 64];
    let old_ptr = if old != 0 {
        previous.as_mut_ptr() as u64
    } else {
        0
    };
    let r = raw(libc::SYS_mq_getsetattr, &[mqdes, new_ptr, old_ptr])?;
    if old != 0 {
        copy_out(mem, old, &previous)?;
    }
    Ok(r)
}
