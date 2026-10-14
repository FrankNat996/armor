//! Path-based file and directory syscalls (docs/tech/syscalls.md).
//!
//! The x86-64 calls AArch64 does not have (`mkdir`, `rename`, `link`,
//! `chmod`, `utime`, `getdents`, ...) become their `*at` forms with
//! `AT_FDCWD`; the `*at` forms run on the host with the path resolved in
//! the guest root, like every other path. `AT_*` flags, mode bits and
//! device numbers have the same values on both architectures.

use std::ffi::CString;

use armor_mem::AddressSpace;

use crate::root::GuestRoot;
use crate::syscall::{SysResult, copy_in, copy_out, err, host_errno, read_path};

/// `AT_FDCWD` as a syscall argument.
pub const AT_FDCWD: u64 = libc::AT_FDCWD as u64;

/// The host's fchmodat2 (asm-generic/unistd.h; the libc crate does not
/// have it for AArch64 yet).
const SYS_FCHMODAT2: libc::c_long = 452;

/// Guest bytes are read from the host in chunks of at most this size.
const CHUNK: usize = 64 * 1024;

fn resolved(mem: &AddressSpace, root: &GuestRoot, path: u64) -> Result<CString, i64> {
    Ok(root.resolve_c(read_path(mem, path)?))
}

fn check(result: libc::c_long) -> SysResult {
    if result < 0 {
        Err(host_errno())
    } else {
        Ok(result)
    }
}

pub fn mkdirat(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    mode: u64,
) -> SysResult {
    let path = resolved(mem, root, path)?;
    // SAFETY: path is NUL-terminated; the other arguments are integers.
    check(unsafe { libc::syscall(libc::SYS_mkdirat, dirfd as i32, path.as_ptr(), mode as u32) })
}

pub fn mknodat(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    mode: u64,
    dev: u64,
) -> SysResult {
    let path = resolved(mem, root, path)?;
    // SAFETY: path is NUL-terminated; the other arguments are integers.
    check(unsafe {
        libc::syscall(
            libc::SYS_mknodat,
            dirfd as i32,
            path.as_ptr(),
            mode as u32,
            dev as u32,
        )
    })
}

/// rename, renameat and renameat2: both paths are resolved in the root
/// (a destination that does not exist there is a host path, as with
/// every other path).
pub fn renameat2(
    mem: &AddressSpace,
    root: &GuestRoot,
    old_dir: u64,
    old: u64,
    new_dir: u64,
    new: u64,
    flags: u64,
) -> SysResult {
    let (old, new) = (resolved(mem, root, old)?, resolved(mem, root, new)?);
    // SAFETY: both paths are NUL-terminated; the other arguments are
    // integers.
    check(unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            old_dir as i32,
            old.as_ptr(),
            new_dir as i32,
            new.as_ptr(),
            flags as u32,
        )
    })
}

pub fn linkat(
    mem: &AddressSpace,
    root: &GuestRoot,
    old_dir: u64,
    old: u64,
    new_dir: u64,
    new: u64,
    flags: u64,
) -> SysResult {
    let (old, new) = (resolved(mem, root, old)?, resolved(mem, root, new)?);
    // SAFETY: both paths are NUL-terminated; the other arguments are
    // integers.
    check(unsafe {
        libc::syscall(
            libc::SYS_linkat,
            old_dir as i32,
            old.as_ptr(),
            new_dir as i32,
            new.as_ptr(),
            flags as i32,
        )
    })
}

/// symlink and symlinkat: the target is the link's content, stored as the
/// guest wrote it; only the new link's path is resolved.
pub fn symlinkat(
    mem: &AddressSpace,
    root: &GuestRoot,
    target: u64,
    dirfd: u64,
    path: u64,
) -> SysResult {
    let target = read_path(mem, target)?;
    let path = resolved(mem, root, path)?;
    // SAFETY: both strings are NUL-terminated; dirfd is an integer.
    check(unsafe {
        libc::syscall(
            libc::SYS_symlinkat,
            target.as_ptr(),
            dirfd as i32,
            path.as_ptr(),
        )
    })
}

/// chmod, fchmodat and fchmodat2 (`flags`: AT_SYMLINK_NOFOLLOW, AT_EMPTY_PATH).
pub fn fchmodat(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    mode: u64,
    flags: u64,
) -> SysResult {
    let path = resolved(mem, root, path)?;
    // fchmodat2 only exists since Linux 6.6: without flags, fchmodat.
    let number = if flags == 0 {
        libc::SYS_fchmodat
    } else {
        SYS_FCHMODAT2
    };
    // SAFETY: path is NUL-terminated; the other arguments are integers.
    check(unsafe {
        libc::syscall(
            number,
            dirfd as i32,
            path.as_ptr(),
            mode as u32,
            flags as u32,
        )
    })
}

/// chown, lchown and fchownat.
pub fn fchownat(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    owner: u64,
    group: u64,
    flags: u64,
) -> SysResult {
    let path = resolved(mem, root, path)?;
    // SAFETY: path is NUL-terminated; the other arguments are integers.
    check(unsafe {
        libc::syscall(
            libc::SYS_fchownat,
            dirfd as i32,
            path.as_ptr(),
            owner as u32,
            group as u32,
            flags as i32,
        )
    })
}

/// utimensat with `struct timespec[2]` (the same on both) or null times,
/// and a null path (the file is `dirfd` itself).
pub fn utimensat(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    times: u64,
    flags: u64,
) -> SysResult {
    let times = if times == 0 {
        None
    } else {
        Some(copy_in::<32>(mem, times)?)
    };
    utimens_host(mem, root, dirfd, path, times, flags)
}

/// i386's utimensat: `struct timespec[2]` of 32-bit fields, widened (the
/// special nanoseconds UTIME_NOW and UTIME_OMIT fit in 32 bits).
pub fn utimensat32(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    times: u64,
    flags: u64,
) -> SysResult {
    let times = if times == 0 {
        None
    } else {
        let narrow = copy_in::<16>(mem, times)?;
        let mut wide = [0u8; 32];
        for (i, field) in narrow.as_chunks::<4>().0.iter().enumerate() {
            let value = i64::from(i32::from_le_bytes(*field));
            wide[8 * i..8 * i + 8].copy_from_slice(&value.to_le_bytes());
        }
        Some(wide)
    };
    utimens_host(mem, root, dirfd, path, times, flags)
}

fn utimens_host(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    times: Option<[u8; 32]>,
    flags: u64,
) -> SysResult {
    let path = if path == 0 {
        None
    } else {
        Some(resolved(mem, root, path)?)
    };
    let path_ptr = path.as_ref().map_or(std::ptr::null(), |p| p.as_ptr());
    let times_ptr = times.as_ref().map_or(std::ptr::null(), |t| t.as_ptr());
    // SAFETY: path_ptr is null or NUL-terminated; times_ptr is null or 32
    // bytes laid out as two `struct timespec`.
    check(unsafe {
        libc::syscall(
            libc::SYS_utimensat,
            dirfd as i32,
            path_ptr,
            times_ptr,
            flags as i32,
        )
    })
}

/// utime(2): `struct utimbuf` (access and modification seconds), or null
/// for "now".
pub fn utime(mem: &AddressSpace, root: &GuestRoot, path: u64, times: u64) -> SysResult {
    let times = if times == 0 {
        None
    } else {
        let buf = copy_in::<16>(mem, times)?;
        let mut spec = [0u8; 32];
        spec[0..8].copy_from_slice(&buf[0..8]);
        spec[16..24].copy_from_slice(&buf[8..16]);
        Some(spec)
    };
    utimens_host(mem, root, AT_FDCWD, path, times, 0)
}

/// utimes and futimesat: `struct timeval[2]`, or null for "now". As in
/// Linux, microseconds outside 0..1_000_000 are EINVAL.
pub fn futimesat(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    times: u64,
) -> SysResult {
    let times = if times == 0 {
        None
    } else {
        let buf = copy_in::<32>(mem, times)?;
        let mut spec = [0u8; 32];
        for i in 0..2 {
            let field = |at: usize| i64::from_le_bytes(buf[at..at + 8].try_into().unwrap());
            let (sec, usec) = (field(i * 16), field(i * 16 + 8));
            if !(0..1_000_000).contains(&usec) {
                return Err(err(libc::EINVAL));
            }
            spec[i * 16..i * 16 + 8].copy_from_slice(&sec.to_le_bytes());
            spec[i * 16 + 8..i * 16 + 16].copy_from_slice(&(usec * 1000).to_le_bytes());
        }
        Some(spec)
    };
    utimens_host(mem, root, dirfd, path, times, 0)
}

/// truncate(2), with the path resolved in the guest root.
pub fn truncate(mem: &AddressSpace, root: &GuestRoot, path: u64, length: u64) -> SysResult {
    let path = resolved(mem, root, path)?;
    // SAFETY: path is NUL-terminated; length is an integer.
    check(unsafe { libc::syscall(libc::SYS_truncate, path.as_ptr(), length as i64) })
}

/// getdents(2), the old format, from the host's getdents64: `struct
/// linux_dirent` is d_ino, d_off, d_reclen, the name with its NUL, then
/// d_type in the record's last byte. Both formats round a record with an
/// n-byte name up to `n + 20` bytes, aligned to 8, so each record keeps
/// its size and position.
pub fn getdents(mem: &AddressSpace, fd: u64, buf: u64, count: u64) -> SysResult {
    let mut bytes = vec![0u8; (count as usize).min(CHUNK)];
    // SAFETY: bytes is writable for its length.
    let n = unsafe {
        libc::syscall(
            libc::SYS_getdents64,
            fd as i32,
            bytes.as_mut_ptr(),
            bytes.len(),
        )
    };
    if n < 0 {
        return Err(host_errno());
    }
    let n = n as usize;
    let mut out = vec![0u8; n];
    let mut at = 0;
    while at < n {
        let record = &bytes[at..];
        let reclen = usize::from(u16::from_le_bytes([record[16], record[17]]));
        let kind = record[18];
        let name = &record[19..reclen];
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
        debug_assert_eq!(reclen, (name.len() + 20).next_multiple_of(8));
        let target = &mut out[at..at + reclen];
        target[..16].copy_from_slice(&record[..16]);
        target[16..18].copy_from_slice(&record[16..18]);
        target[18..18 + name.len()].copy_from_slice(name);
        target[reclen - 1] = kind;
        at += reclen;
    }
    copy_out(mem, buf, &out)?;
    Ok(n as i64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn both_dirent_formats_have_the_same_record_size() {
        for name_len in 1..64usize {
            // linux_dirent64: 19 bytes of header, the name, its NUL.
            let new = (19 + name_len + 1).next_multiple_of(8);
            // linux_dirent: 18 bytes of header, the name, its NUL, d_type.
            let old = (18 + name_len + 2).next_multiple_of(8);
            assert_eq!(new, old, "name of {name_len} bytes");
        }
    }
}
