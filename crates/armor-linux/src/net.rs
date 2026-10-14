//! Sockets for the guest: x86_64 and i386 (ADR-0029).
//!
//! The constants (domains, types, `SOCK_*`, `MSG_*`, `SOL_SOCKET`, the
//! `SO_*` options, `SCM_*`) and `struct sockaddr` are the same on x86_64,
//! i386 and AArch64, and so are `struct msghdr` and `struct cmsghdr` of
//! x86_64 and AArch64 (✅ measured with `sizeof`/`offsetof` on all three,
//! 2026-10-14, docs/tech/syscalls.md). An i386 guest has its own
//! `msghdr` (28 bytes), `iovec` (8) and `cmsghdr` (a 12-byte header, data
//! aligned to 4), converted here; `Word` says which layout a call uses.
//!
//! As everywhere, guest buffers are copied to host buffers: no guest
//! pointer reaches the host kernel.

use armor_mem::{AddressSpace, GuestMemory};

use crate::syscall::{SysResult, copy_in, copy_out, err, host, host_errno};

/// The width of the guest's pointers and `long`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Word {
    W64,
    W32,
}

impl Word {
    pub(crate) fn bytes(self) -> usize {
        match self {
            Word::W64 => 8,
            Word::W32 => 4,
        }
    }
}

/// The largest socket address (`struct sockaddr_storage`).
const SOCKADDR_MAX: usize = 128;
/// The most bytes one send or receive moves here.
const MAX_TRANSFER: usize = 1 << 20;
/// The most iovecs in one message (`UIO_MAXIOV`).
const IOV_MAX: u64 = 1024;
/// The most control-message bytes handled.
const CONTROL_MAX: usize = 4096;

const SOL_SOCKET: u64 = 1;
/// The "old" timeouts, a `struct timeval` of `long`s: 8 bytes on i386.
const SO_RCVTIMEO_OLD: u64 = 20;
const SO_SNDTIMEO_OLD: u64 = 21;
const SCM_RIGHTS: i32 = 1;

/// A socket address from the guest: its bytes (at most 128).
fn read_address(mem: &AddressSpace, addr: u64, len: u64) -> Result<Vec<u8>, i64> {
    if len as usize > SOCKADDR_MAX {
        return Err(err(libc::EINVAL));
    }
    let mut bytes = vec![0u8; len as usize];
    mem.read(addr, &mut bytes).map_err(|_| err(libc::EFAULT))?;
    Ok(bytes)
}

/// Writes a socket address the host returned (`bytes`, of the host's full
/// length `full`) to the guest's buffer `addr` whose capacity is at
/// `len_ptr`, truncated as the kernel does, and the full length back.
fn write_address(
    mem: &AddressSpace,
    addr: u64,
    len_ptr: u64,
    bytes: &[u8],
    full: u32,
) -> Result<(), i64> {
    if addr == 0 || len_ptr == 0 {
        return Ok(());
    }
    let capacity = u32::from_le_bytes(copy_in::<4>(mem, len_ptr)?) as usize;
    let n = capacity.min(bytes.len()).min(full as usize);
    copy_out(mem, addr, &bytes[..n])?;
    copy_out(mem, len_ptr, &full.to_le_bytes())
}

pub(crate) fn socket(domain: u64, kind: u64, protocol: u64) -> SysResult {
    host(libc::SYS_socket, &[domain, kind, protocol])
}

pub(crate) fn bind(mem: &AddressSpace, fd: u64, addr: u64, len: u64) -> SysResult {
    let address = read_address(mem, addr, len)?;
    // SAFETY: address is a host buffer of `len` bytes.
    let r = unsafe { libc::syscall(libc::SYS_bind, fd as i32, address.as_ptr(), address.len()) };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

pub(crate) fn connect(mem: &AddressSpace, fd: u64, addr: u64, len: u64) -> SysResult {
    let address = read_address(mem, addr, len)?;
    // SAFETY: address is a host buffer of `len` bytes.
    let r = unsafe {
        libc::syscall(
            libc::SYS_connect,
            fd as i32,
            address.as_ptr(),
            address.len(),
        )
    };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

pub(crate) fn listen(fd: u64, backlog: u64) -> SysResult {
    host(libc::SYS_listen, &[fd, backlog])
}

pub(crate) fn shutdown(fd: u64, how: u64) -> SysResult {
    host(libc::SYS_shutdown, &[fd, how])
}

/// accept4(2) (accept: flags 0).
pub(crate) fn accept4(
    mem: &AddressSpace,
    fd: u64,
    addr: u64,
    len_ptr: u64,
    flags: u64,
) -> SysResult {
    let mut address = [0u8; SOCKADDR_MAX];
    let mut len = SOCKADDR_MAX as u32;
    let want = addr != 0 && len_ptr != 0;
    // SAFETY: address and len are host buffers, or null when not wanted.
    let r = unsafe {
        libc::syscall(
            libc::SYS_accept4,
            fd as i32,
            if want {
                address.as_mut_ptr()
            } else {
                std::ptr::null_mut()
            },
            if want {
                &raw mut len
            } else {
                std::ptr::null_mut()
            },
            flags as i32,
        )
    };
    if r < 0 {
        return Err(host_errno());
    }
    // Like the kernel: the connection is accepted even if the address
    // cannot be written back.
    let _ = write_address(mem, addr, len_ptr, &address, len);
    Ok(r)
}

/// getsockname(2) (`peer` false) and getpeername(2).
pub(crate) fn name(mem: &AddressSpace, peer: bool, fd: u64, addr: u64, len_ptr: u64) -> SysResult {
    let mut address = [0u8; SOCKADDR_MAX];
    let mut len = SOCKADDR_MAX as u32;
    let call = if peer {
        libc::SYS_getpeername
    } else {
        libc::SYS_getsockname
    };
    // SAFETY: address and len are host buffers.
    let r = unsafe { libc::syscall(call, fd as i32, address.as_mut_ptr(), &raw mut len) };
    if r < 0 {
        return Err(host_errno());
    }
    if len_ptr == 0 {
        return Err(err(libc::EFAULT));
    }
    write_address(mem, addr, len_ptr, &address, len)?;
    Ok(0)
}

pub(crate) fn socketpair(
    mem: &AddressSpace,
    domain: u64,
    kind: u64,
    protocol: u64,
    sv: u64,
) -> SysResult {
    let mut fds = [0i32; 2];
    // SAFETY: fds is writable for two descriptors.
    let r = unsafe {
        libc::syscall(
            libc::SYS_socketpair,
            domain as i32,
            kind as i32,
            protocol as i32,
            fds.as_mut_ptr(),
        )
    };
    if r < 0 {
        return Err(host_errno());
    }
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&fds[0].to_le_bytes());
    bytes[4..].copy_from_slice(&fds[1].to_le_bytes());
    if let Err(e) = copy_out(mem, sv, &bytes) {
        for fd in fds {
            // SAFETY: closing the descriptors just made.
            unsafe { libc::close(fd) };
        }
        return Err(e);
    }
    Ok(0)
}

/// sendto(2) (send: no address).
pub(crate) fn sendto(
    mem: &AddressSpace,
    fd: u64,
    buf: u64,
    len: u64,
    flags: u64,
    addr: u64,
    addr_len: u64,
) -> SysResult {
    let mut data = vec![0u8; (len as usize).min(MAX_TRANSFER)];
    mem.read(buf, &mut data).map_err(|_| err(libc::EFAULT))?;
    let address = if addr != 0 {
        Some(read_address(mem, addr, addr_len)?)
    } else {
        None
    };
    let (address_ptr, address_len) = address
        .as_ref()
        .map_or((std::ptr::null(), 0), |a| (a.as_ptr(), a.len()));
    // SAFETY: data and address are host buffers of the lengths given.
    let r = unsafe {
        libc::syscall(
            libc::SYS_sendto,
            fd as i32,
            data.as_ptr(),
            data.len(),
            flags as i32,
            address_ptr,
            address_len,
        )
    };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

/// recvfrom(2) (recv: no address).
pub(crate) fn recvfrom(
    mem: &AddressSpace,
    fd: u64,
    buf: u64,
    len: u64,
    flags: u64,
    addr: u64,
    len_ptr: u64,
) -> SysResult {
    let mut data = vec![0u8; (len as usize).min(MAX_TRANSFER)];
    let mut address = [0u8; SOCKADDR_MAX];
    let mut address_len = SOCKADDR_MAX as u32;
    // SAFETY: data, address and address_len are host buffers.
    let r = unsafe {
        libc::syscall(
            libc::SYS_recvfrom,
            fd as i32,
            data.as_mut_ptr(),
            data.len(),
            flags as i32,
            address.as_mut_ptr(),
            &raw mut address_len,
        )
    };
    if r < 0 {
        return Err(host_errno());
    }
    // MSG_TRUNC can return more than the buffer holds.
    let n = (r as usize).min(data.len());
    copy_out(mem, buf, &data[..n])?;
    write_address(mem, addr, len_ptr, &address, address_len)?;
    Ok(r)
}

/// setsockopt(2). For an i386 guest the old timeouts are 32-bit
/// `struct timeval`s, widened for the host.
pub(crate) fn setsockopt(
    mem: &AddressSpace,
    fd: u64,
    level: u64,
    name: u64,
    value: u64,
    len: u64,
    word: Word,
) -> SysResult {
    if len > 4096 {
        return Err(err(libc::EINVAL));
    }
    let mut bytes = vec![0u8; len as usize];
    mem.read(value, &mut bytes).map_err(|_| err(libc::EFAULT))?;
    if word == Word::W32 && level == SOL_SOCKET && matches!(name, SO_RCVTIMEO_OLD | SO_SNDTIMEO_OLD)
    {
        if bytes.len() < 8 {
            return Err(err(libc::EINVAL));
        }
        let field = |i: usize| {
            i64::from(i32::from_le_bytes(
                bytes[4 * i..4 * i + 4].try_into().expect("4"),
            ))
        };
        let (seconds, micros) = (field(0), field(1));
        bytes = [seconds.to_le_bytes(), micros.to_le_bytes()].concat();
    }
    // SAFETY: bytes is a host buffer of its length.
    let r = unsafe {
        libc::syscall(
            libc::SYS_setsockopt,
            fd as i32,
            level as i32,
            name as i32,
            bytes.as_ptr(),
            bytes.len(),
        )
    };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

/// getsockopt(2), with the old timeouts narrowed for an i386 guest.
pub(crate) fn getsockopt(
    mem: &AddressSpace,
    fd: u64,
    level: u64,
    name: u64,
    value: u64,
    len_ptr: u64,
    word: Word,
) -> SysResult {
    let capacity = u32::from_le_bytes(copy_in::<4>(mem, len_ptr)?);
    if capacity as i32 > 4096 || (capacity as i32) < 0 {
        return Err(err(libc::EINVAL));
    }
    let timeout = word == Word::W32
        && level == SOL_SOCKET
        && matches!(name, SO_RCVTIMEO_OLD | SO_SNDTIMEO_OLD);
    let mut bytes = vec![0u8; if timeout { 16 } else { capacity as usize }];
    let mut len = bytes.len() as u32;
    // SAFETY: bytes and len are host buffers.
    let r = unsafe {
        libc::syscall(
            libc::SYS_getsockopt,
            fd as i32,
            level as i32,
            name as i32,
            bytes.as_mut_ptr(),
            &raw mut len,
        )
    };
    if r < 0 {
        return Err(host_errno());
    }
    if timeout {
        let field = |i: usize| i64::from_le_bytes(bytes[8 * i..8 * i + 8].try_into().expect("8"));
        let narrow = [
            (field(0) as i32).to_le_bytes(),
            (field(1) as i32).to_le_bytes(),
        ]
        .concat();
        let n = (capacity as usize).min(8);
        copy_out(mem, value, &narrow[..n])?;
        copy_out(mem, len_ptr, &(n as u32).to_le_bytes())?;
        return Ok(0);
    }
    let n = (len as usize).min(capacity as usize);
    copy_out(mem, value, &bytes[..n])?;
    copy_out(mem, len_ptr, &(n as u32).to_le_bytes())?;
    Ok(0)
}

/// A guest `struct msghdr` read in the guest's layout.
struct Message {
    name: u64,
    name_len: u64,
    iov: Vec<(u64, u64)>,
    control: u64,
    control_len: u64,
    /// Where the guest's msghdr is: recvmsg writes back lengths and flags.
    at: u64,
}

fn read_message(mem: &AddressSpace, at: u64, word: Word) -> Result<Message, i64> {
    let w = word.bytes();
    let mut raw = [0u8; 56];
    let size = 7 * w;
    mem.read(at, &mut raw[..size])
        .map_err(|_| err(libc::EFAULT))?;
    let field = |i: usize| {
        let mut value = [0u8; 8];
        value[..w].copy_from_slice(&raw[i * w..i * w + w]);
        u64::from_le_bytes(value)
    };
    // msg_namelen is 32 bits in both layouts (with padding on x86_64).
    let name_len = u64::from(u32::from_le_bytes(raw[w..w + 4].try_into().expect("4")));
    let (iov_at, iov_len) = (field(2), field(3));
    if iov_len > IOV_MAX {
        return Err(err(libc::EMSGSIZE));
    }
    let mut vector = vec![0u8; iov_len as usize * 2 * w];
    mem.read(iov_at, &mut vector)
        .map_err(|_| err(libc::EFAULT))?;
    let iov: Vec<(u64, u64)> = vector
        .chunks_exact(2 * w)
        .map(|entry| {
            let get = |part: &[u8]| {
                let mut value = [0u8; 8];
                value[..w].copy_from_slice(part);
                u64::from_le_bytes(value)
            };
            (get(&entry[..w]), get(&entry[w..]))
        })
        .collect();
    // A length negative as a signed `long` is EINVAL, as in Linux.
    let sign = 1u64 << (8 * w - 1);
    if iov.iter().any(|&(_, len)| len & sign != 0) {
        return Err(err(libc::EINVAL));
    }
    Ok(Message {
        name: field(0),
        name_len,
        iov,
        control: field(4),
        control_len: field(5),
        at,
    })
}

/// Control messages from the guest's layout to the host's: for i386 the
/// headers grow from 12 to 16 bytes and the data alignment from 4 to 8.
fn control_to_host(raw: &[u8], word: Word) -> Vec<u8> {
    if word == Word::W64 {
        return raw.to_vec();
    }
    let mut out = Vec::new();
    let mut at = 0;
    while at + 12 <= raw.len() {
        let len = u32::from_le_bytes(raw[at..at + 4].try_into().expect("4")) as usize;
        if len < 12 || at + len > raw.len() {
            break;
        }
        let data = &raw[at + 12..at + len];
        out.extend_from_slice(&((16 + data.len()) as u64).to_le_bytes());
        out.extend_from_slice(&raw[at + 4..at + 12]);
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(8), 0);
        at += len.next_multiple_of(4);
    }
    out
}

/// Control messages from the host's layout to the i386 guest's, at most
/// `capacity` bytes. Returns them, whether some did not fit, and the
/// passed descriptors left out, which the caller closes: as Linux does, an
/// `SCM_RIGHTS` message that does not fit keeps as many descriptors as
/// there is room for, and the rest are released; another message that
/// does not fit is cut to the room left, its length saying so.
fn control_to_guest(raw: &[u8], capacity: usize) -> (Vec<u8>, bool, Vec<i32>) {
    let mut out = Vec::new();
    let mut truncated = false;
    let mut dropped = Vec::new();
    let mut at = 0;
    while at + 16 <= raw.len() {
        let len = u64::from_le_bytes(raw[at..at + 8].try_into().expect("8")) as usize;
        if len < 16 || at + len > raw.len() {
            break;
        }
        let level = i32::from_le_bytes(raw[at + 8..at + 12].try_into().expect("4"));
        let kind = i32::from_le_bytes(raw[at + 12..at + 16].try_into().expect("4"));
        let data = &raw[at + 16..at + len];
        let rights = level == SOL_SOCKET as i32 && kind == SCM_RIGHTS;
        let room = capacity.saturating_sub(out.len());
        // The data bytes the guest gets, if the message is written at all.
        let fit = if truncated {
            None
        } else if 12 + data.len() <= room {
            Some(data.len())
        } else {
            truncated = true;
            if rights {
                // Whole descriptors only, and at least one (Linux's
                // scm_detach_fds_compat).
                (room >= 16).then(|| (room - 12) / 4 * 4)
            } else {
                // The header with the room left as its length, and the
                // data that fits (Linux's put_cmsg_compat).
                (room >= 12).then(|| room - 12)
            }
        };
        let fit_bytes = fit.unwrap_or(0);
        if rights {
            dropped.extend(
                data[fit_bytes..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&fd| i32::from_le_bytes(fd)),
            );
        }
        if let Some(fit) = fit {
            out.extend_from_slice(&((12 + fit) as u32).to_le_bytes());
            out.extend_from_slice(&raw[at + 8..at + 16]);
            out.extend_from_slice(&data[..fit]);
            out.resize(out.len().next_multiple_of(4).min(capacity), 0);
        }
        at += len.next_multiple_of(8);
    }
    (out, truncated, dropped)
}

pub(crate) fn sendmsg(mem: &AddressSpace, fd: u64, msg: u64, flags: u64, word: Word) -> SysResult {
    let message = read_message(mem, msg, word)?;
    let mut data = Vec::new();
    for &(base, len) in &message.iov {
        let start = data.len();
        let len = (len as usize).min(MAX_TRANSFER.saturating_sub(start));
        data.resize(start + len, 0);
        mem.read(base, &mut data[start..])
            .map_err(|_| err(libc::EFAULT))?;
    }
    let name = if message.name != 0 {
        Some(read_address(mem, message.name, message.name_len)?)
    } else {
        None
    };
    let control = if message.control != 0 && message.control_len != 0 {
        let mut raw = vec![0u8; (message.control_len as usize).min(CONTROL_MAX)];
        mem.read(message.control, &mut raw)
            .map_err(|_| err(libc::EFAULT))?;
        control_to_host(&raw, word)
    } else {
        Vec::new()
    };
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: data.len(),
    };
    // SAFETY: an all-zero msghdr is a valid value of this plain C struct.
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    if let Some(name) = &name {
        header.msg_name = name.as_ptr() as *mut libc::c_void;
        header.msg_namelen = name.len() as u32;
    }
    header.msg_iov = &raw mut iov;
    header.msg_iovlen = 1;
    if !control.is_empty() {
        header.msg_control = control.as_ptr() as *mut libc::c_void;
        header.msg_controllen = control.len();
    }
    // SAFETY: every pointer in header is to a host buffer alive here.
    let r = unsafe { libc::sendmsg(fd as i32, &header, flags as i32) };
    if r < 0 {
        Err(host_errno())
    } else {
        Ok(r as i64)
    }
}

pub(crate) fn recvmsg(mem: &AddressSpace, fd: u64, msg: u64, flags: u64, word: Word) -> SysResult {
    let message = read_message(mem, msg, word)?;
    let total = message
        .iov
        .iter()
        .fold(0usize, |sum, &(_, len)| sum.saturating_add(len as usize))
        .min(MAX_TRANSFER);
    let mut data = vec![0u8; total];
    let mut name = [0u8; SOCKADDR_MAX];
    // The x86_64 control messages are the host's: the host truncates them
    // itself. The i386 ones are smaller: twice the room holds every
    // message that fits the guest's (16 + 4n host bytes for 12 + 4n).
    let control_room = match word {
        Word::W64 => message.control_len as usize,
        Word::W32 => 2 * message.control_len as usize,
    };
    let mut control = vec![0u8; control_room.min(CONTROL_MAX)];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: data.len(),
    };
    // SAFETY: an all-zero msghdr is a valid value of this plain C struct.
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_name = name.as_mut_ptr().cast();
    header.msg_namelen = SOCKADDR_MAX as u32;
    header.msg_iov = &raw mut iov;
    header.msg_iovlen = 1;
    if !control.is_empty() {
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = control.len();
    }
    // SAFETY: every pointer in header is to a host buffer alive here.
    let r = unsafe { libc::recvmsg(fd as i32, &mut header, flags as i32) };
    if r < 0 {
        return Err(host_errno());
    }
    // Scatter the bytes into the guest's buffers.
    let mut done = 0usize;
    let received = (r as usize).min(data.len());
    for &(base, len) in &message.iov {
        if done >= received {
            break;
        }
        let n = (len as usize).min(received - done);
        copy_out(mem, base, &data[done..done + n])?;
        done += n;
    }
    let w = word.bytes() as u64;
    // msg_namelen, msg_controllen and msg_flags back in the guest's msghdr.
    if message.name != 0 {
        let n = (header.msg_namelen as usize).min(message.name_len as usize);
        copy_out(mem, message.name, &name[..n])?;
    }
    copy_out(mem, message.at + w, &header.msg_namelen.to_le_bytes())?;
    let mut msg_flags = header.msg_flags;
    let control_len = if message.control != 0 {
        let raw = &control[..header.msg_controllen];
        let (bytes, truncated) = match word {
            Word::W64 => (raw.to_vec(), false),
            Word::W32 => {
                let (bytes, truncated, dropped) =
                    control_to_guest(raw, message.control_len as usize);
                for fd in dropped {
                    // SAFETY: a descriptor the host just installed for this
                    // message, which the guest never sees.
                    unsafe { libc::close(fd) };
                }
                (bytes, truncated)
            }
        };
        copy_out(mem, message.control, &bytes)?;
        if truncated {
            msg_flags |= libc::MSG_CTRUNC;
        }
        bytes.len() as u64
    } else {
        0
    };
    copy_out(
        mem,
        message.at + 5 * w,
        &control_len.to_le_bytes()[..w as usize],
    )?;
    copy_out(mem, message.at + 6 * w, &msg_flags.to_le_bytes())?;
    Ok(r as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An i386 SCM_RIGHTS message with one descriptor, both ways.
    #[test]
    fn i386_control_messages_are_converted() {
        let mut guest = Vec::new();
        guest.extend_from_slice(&16u32.to_le_bytes()); // 12 + 4
        guest.extend_from_slice(&1i32.to_le_bytes()); // SOL_SOCKET
        guest.extend_from_slice(&1i32.to_le_bytes()); // SCM_RIGHTS
        guest.extend_from_slice(&7i32.to_le_bytes());
        let host = control_to_host(&guest, Word::W32);
        assert_eq!(host.len(), 24);
        assert_eq!(u64::from_le_bytes(host[..8].try_into().unwrap()), 20);
        assert_eq!(&host[16..20], &7i32.to_le_bytes());
        let (back, truncated, dropped) = control_to_guest(&host[..20], 64);
        assert!(!truncated && dropped.is_empty());
        assert_eq!(back, guest);
        // Not enough room: truncated (MSG_CTRUNC), the descriptor left out.
        let (back, truncated, dropped) = control_to_guest(&host[..20], 12);
        assert!(truncated && back.is_empty());
        assert_eq!(dropped, [7]);
    }

    /// msghdr fields from the guest: too many iovecs, negative lengths,
    /// and lengths whose sum does not fit.
    #[test]
    fn messages_with_extreme_lengths_are_checked() {
        use armor_mem::{GUEST_PAGE_SIZE, Prot};
        let mem = AddressSpace::new();
        // Below 4 GiB, for the i386 pointers.
        mem.set_ceiling(1 << 32);
        let at = mem
            .map_anywhere(GUEST_PAGE_SIZE, Prot::READ | Prot::WRITE)
            .unwrap();
        let (iov, buf) = (at + 256, at + 1024);
        let header = |count: u64| {
            let mut h = [0u8; 56];
            h[16..24].copy_from_slice(&iov.to_le_bytes());
            h[24..32].copy_from_slice(&count.to_le_bytes());
            h
        };
        let entries = |lens: &[u64]| {
            let mut v = Vec::new();
            for len in lens {
                v.extend_from_slice(&buf.to_le_bytes());
                v.extend_from_slice(&len.to_le_bytes());
            }
            v
        };
        mem.write(at, &header(IOV_MAX + 1)).unwrap();
        assert_eq!(
            recvmsg(&mem, u64::MAX, at, 0, Word::W64),
            Err(err(libc::EMSGSIZE))
        );
        mem.write(at, &header(1)).unwrap();
        mem.write(iov, &entries(&[u64::MAX])).unwrap();
        assert_eq!(
            recvmsg(&mem, u64::MAX, at, 0, Word::W64),
            Err(err(libc::EINVAL))
        );
        // Two lengths that are not negative but overflow when added: the
        // host's EBADF (fd -1), not a panic.
        mem.write(at, &header(2)).unwrap();
        mem.write(iov, &entries(&[i64::MAX as u64, i64::MAX as u64]))
            .unwrap();
        assert_eq!(
            recvmsg(&mem, u64::MAX, at, 0, Word::W64),
            Err(err(libc::EBADF))
        );
        // An i386 length with the top bit set is negative.
        let mut h = [0u8; 28];
        h[8..12].copy_from_slice(&(iov as u32).to_le_bytes());
        h[12..16].copy_from_slice(&1u32.to_le_bytes());
        mem.write(at, &h).unwrap();
        let mut e = Vec::new();
        e.extend_from_slice(&(buf as u32).to_le_bytes());
        e.extend_from_slice(&0x8000_0000u32.to_le_bytes());
        mem.write(iov, &e).unwrap();
        assert_eq!(
            recvmsg(&mem, u64::MAX, at, 0, Word::W32),
            Err(err(libc::EINVAL))
        );
    }

    /// Two descriptors, room for one: a shorter message and one left out.
    #[test]
    fn i386_descriptors_that_do_not_fit_are_left_out() {
        let mut host = Vec::new();
        host.extend_from_slice(&24u64.to_le_bytes()); // 16 + 8
        host.extend_from_slice(&1i32.to_le_bytes());
        host.extend_from_slice(&1i32.to_le_bytes());
        host.extend_from_slice(&5i32.to_le_bytes());
        host.extend_from_slice(&6i32.to_le_bytes());
        let (back, truncated, dropped) = control_to_guest(&host, 16);
        assert!(truncated);
        assert_eq!(u32::from_le_bytes(back[..4].try_into().unwrap()), 16);
        assert_eq!(&back[12..16], &5i32.to_le_bytes());
        assert_eq!(dropped, [6]);
        // Another message is cut to the room left: SCM_CREDENTIALS (2),
        // 12 bytes of data, with room for 8.
        let mut host = Vec::new();
        host.extend_from_slice(&28u64.to_le_bytes());
        host.extend_from_slice(&1i32.to_le_bytes());
        host.extend_from_slice(&2i32.to_le_bytes());
        host.extend_from_slice(&[7; 12]);
        let (back, truncated, dropped) = control_to_guest(&host, 20);
        assert!(truncated && dropped.is_empty());
        assert_eq!(back.len(), 20);
        assert_eq!(u32::from_le_bytes(back[..4].try_into().unwrap()), 20);
        assert_eq!(&back[12..], &[7; 8]);
    }
}
