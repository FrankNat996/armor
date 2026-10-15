//! Extended attributes and the calls that move data between descriptors
//! in the kernel (ADR-0044): `setxattr` and its eleven companions,
//! `splice`, `tee`, `vmsplice`, `copy_file_range`. Paths are resolved in
//! the guest root; names, values and offsets are copied; the XATTR_* flags
//! and SPLICE_F_* flags are the same on both architectures.

use std::ffi::CString;

use armor_mem::{AddressSpace, GuestMemory};

use super::{Process, SysResult, copy_in, copy_out, err, host, host_errno, read_path};

pub mod nr {
    pub const SETXATTR: u64 = 188;
    pub const LSETXATTR: u64 = 189;
    pub const FSETXATTR: u64 = 190;
    pub const GETXATTR: u64 = 191;
    pub const LGETXATTR: u64 = 192;
    pub const FGETXATTR: u64 = 193;
    pub const LISTXATTR: u64 = 194;
    pub const FLISTXATTR: u64 = 196;
    pub const REMOVEXATTR: u64 = 197;
    pub const LREMOVEXATTR: u64 = 198;
    pub const FREMOVEXATTR: u64 = 199;
    pub const SPLICE: u64 = 275;
    pub const TEE: u64 = 276;
    pub const VMSPLICE: u64 = 278;
    pub const COPY_FILE_RANGE: u64 = 326;
}

pub const HANDLED: &[u64] = &[
    nr::SETXATTR,
    nr::LSETXATTR,
    nr::FSETXATTR,
    nr::GETXATTR,
    nr::LGETXATTR,
    nr::FGETXATTR,
    nr::LISTXATTR,
    nr::FLISTXATTR,
    nr::REMOVEXATTR,
    nr::LREMOVEXATTR,
    nr::FREMOVEXATTR,
    nr::SPLICE,
    nr::TEE,
    nr::VMSPLICE,
    nr::COPY_FILE_RANGE,
];

/// The largest attribute value and name list (`XATTR_SIZE_MAX`,
/// `XATTR_LIST_MAX`) and name (`XATTR_NAME_MAX`).
const XATTR_SIZE_MAX: u64 = 65536;
const XATTR_NAME_MAX: usize = 255;
const SPLICE_F_NONBLOCK: u64 = 2;

pub(super) fn dispatch(
    mem: &AddressSpace,
    process: &Process,
    number: u64,
    a: [u64; 6],
) -> Option<Result<SysResult, String>> {
    let path =
        |at: u64| -> Result<CString, i64> { Ok(process.root.resolve_c(read_path(mem, at)?)) };
    let result = match number {
        nr::SETXATTR | nr::LSETXATTR | nr::FSETXATTR => {
            let target = match number {
                nr::FSETXATTR => Target::Fd(a[0] as i32),
                n => match path(a[0]) {
                    Ok(p) => Target::Path(p, n == nr::LSETXATTR),
                    Err(e) => return Some(Ok(Err(e))),
                },
            };
            setxattr(mem, target, a[1], a[2], a[3], a[4])
        }
        nr::GETXATTR | nr::LGETXATTR | nr::FGETXATTR => {
            let target = match number {
                nr::FGETXATTR => Target::Fd(a[0] as i32),
                n => match path(a[0]) {
                    Ok(p) => Target::Path(p, n == nr::LGETXATTR),
                    Err(e) => return Some(Ok(Err(e))),
                },
            };
            getxattr(mem, target, a[1], a[2], a[3])
        }
        nr::LISTXATTR | nr::FLISTXATTR => {
            let target = match number {
                nr::FLISTXATTR => Target::Fd(a[0] as i32),
                _ => match path(a[0]) {
                    Ok(p) => Target::Path(p, false),
                    Err(e) => return Some(Ok(Err(e))),
                },
            };
            listxattr(mem, target, a[1], a[2])
        }
        nr::REMOVEXATTR | nr::LREMOVEXATTR | nr::FREMOVEXATTR => {
            let target = match number {
                nr::FREMOVEXATTR => Target::Fd(a[0] as i32),
                n => match path(a[0]) {
                    Ok(p) => Target::Path(p, n == nr::LREMOVEXATTR),
                    Err(e) => return Some(Ok(Err(e))),
                },
            };
            removexattr(mem, target, a[1])
        }
        nr::SPLICE => splice(mem, a),
        nr::TEE => host(libc::SYS_tee, &[a[0], a[1], a[2], a[3]]),
        nr::VMSPLICE => vmsplice(mem, a[0], a[1], a[2], a[3]),
        nr::COPY_FILE_RANGE => copy_file_range(mem, a),
        _ => return None,
    };
    Some(Ok(result))
}

/// What an xattr call acts on: a path (and whether a final symlink is
/// itself the target, the `l*` calls) or a descriptor.
enum Target {
    Path(CString, bool),
    Fd(i32),
}

fn check(r: libc::ssize_t) -> SysResult {
    if r < 0 {
        Err(host_errno())
    } else {
        Ok(r as i64)
    }
}

/// An attribute name: at most XATTR_NAME_MAX bytes (ERANGE beyond, an
/// empty one too, as Linux).
fn read_name(mem: &AddressSpace, at: u64) -> Result<CString, i64> {
    let name = read_path(mem, at).map_err(|e| {
        if e == err(libc::ENAMETOOLONG) {
            err(libc::ERANGE)
        } else {
            e
        }
    })?;
    if name.as_bytes().is_empty() || name.as_bytes().len() > XATTR_NAME_MAX {
        return Err(err(libc::ERANGE));
    }
    Ok(name)
}

fn setxattr(
    mem: &AddressSpace,
    target: Target,
    name: u64,
    value: u64,
    size: u64,
    flags: u64,
) -> SysResult {
    let name = read_name(mem, name)?;
    if size > XATTR_SIZE_MAX {
        return Err(err(libc::E2BIG));
    }
    let mut bytes = vec![0u8; size as usize];
    if size > 0 {
        mem.read(value, &mut bytes).map_err(|_| err(libc::EFAULT))?;
    }
    let (ptr, len, flags) = (bytes.as_ptr().cast(), bytes.len(), flags as i32);
    // SAFETY: path and name are NUL-terminated; ptr is valid for len.
    check(unsafe {
        match &target {
            Target::Path(p, false) => {
                libc::setxattr(p.as_ptr(), name.as_ptr(), ptr, len, flags) as isize
            }
            Target::Path(p, true) => {
                libc::lsetxattr(p.as_ptr(), name.as_ptr(), ptr, len, flags) as isize
            }
            Target::Fd(fd) => libc::fsetxattr(*fd, name.as_ptr(), ptr, len, flags) as isize,
        }
    })
}

fn getxattr(mem: &AddressSpace, target: Target, name: u64, value: u64, size: u64) -> SysResult {
    let name = read_name(mem, name)?;
    let mut bytes = vec![0u8; size.min(XATTR_SIZE_MAX) as usize];
    let (ptr, len) = (bytes.as_mut_ptr().cast(), bytes.len());
    // SAFETY: path and name are NUL-terminated; ptr is writable for len (a
    // zero length only asks for the size).
    let n = check(unsafe {
        match &target {
            Target::Path(p, false) => libc::getxattr(p.as_ptr(), name.as_ptr(), ptr, len),
            Target::Path(p, true) => libc::lgetxattr(p.as_ptr(), name.as_ptr(), ptr, len),
            Target::Fd(fd) => libc::fgetxattr(*fd, name.as_ptr(), ptr, len),
        }
    })?;
    if size > 0 {
        copy_out(mem, value, &bytes[..n as usize])?;
    }
    Ok(n)
}

fn listxattr(mem: &AddressSpace, target: Target, list: u64, size: u64) -> SysResult {
    let mut bytes = vec![0u8; size.min(XATTR_SIZE_MAX) as usize];
    let (ptr, len) = (bytes.as_mut_ptr().cast(), bytes.len());
    // SAFETY: path is NUL-terminated; ptr is writable for len.
    let n = check(unsafe {
        match &target {
            Target::Path(p, _) => libc::listxattr(p.as_ptr(), ptr, len),
            Target::Fd(fd) => libc::flistxattr(*fd, ptr, len),
        }
    })?;
    if size > 0 {
        copy_out(mem, list, &bytes[..n as usize])?;
    }
    Ok(n)
}

fn removexattr(mem: &AddressSpace, target: Target, name: u64) -> SysResult {
    let name = read_name(mem, name)?;
    // SAFETY: path and name are NUL-terminated.
    check(unsafe {
        match &target {
            Target::Path(p, false) => libc::removexattr(p.as_ptr(), name.as_ptr()) as isize,
            Target::Path(p, true) => libc::lremovexattr(p.as_ptr(), name.as_ptr()) as isize,
            Target::Fd(fd) => libc::fremovexattr(*fd, name.as_ptr()) as isize,
        }
    })
}

/// An optional `loff_t *` argument: read before the call, written back
/// after it.
fn offset_in(mem: &AddressSpace, at: u64) -> Result<Option<i64>, i64> {
    if at == 0 {
        return Ok(None);
    }
    Ok(Some(i64::from_le_bytes(copy_in::<8>(mem, at)?)))
}

fn offset_out(mem: &AddressSpace, at: u64, value: Option<i64>) -> Result<(), i64> {
    match value {
        Some(v) => copy_out(mem, at, &v.to_le_bytes()),
        None => Ok(()),
    }
}

fn ptr(offset: &mut Option<i64>) -> *mut i64 {
    offset
        .as_mut()
        .map_or(std::ptr::null_mut(), |o| o as *mut i64)
}

/// splice(2): fd_in, off_in, fd_out, off_out, len, flags.
fn splice(mem: &AddressSpace, a: [u64; 6]) -> SysResult {
    let mut off_in = offset_in(mem, a[1])?;
    let mut off_out = offset_in(mem, a[3])?;
    // SAFETY: the offsets are null or local i64s; the rest are integers.
    let n = unsafe {
        libc::syscall(
            libc::SYS_splice,
            a[0],
            ptr(&mut off_in),
            a[2],
            ptr(&mut off_out),
            a[4],
            a[5],
        )
    };
    if n < 0 {
        return Err(host_errno());
    }
    offset_out(mem, a[1], off_in)?;
    offset_out(mem, a[3], off_out)?;
    Ok(n)
}

/// copy_file_range(2): fd_in, off_in, fd_out, off_out, len, flags.
fn copy_file_range(mem: &AddressSpace, a: [u64; 6]) -> SysResult {
    let mut off_in = offset_in(mem, a[1])?;
    let mut off_out = offset_in(mem, a[3])?;
    // SAFETY: as for splice.
    let n = unsafe {
        libc::syscall(
            libc::SYS_copy_file_range,
            a[0],
            ptr(&mut off_in),
            a[2],
            ptr(&mut off_out),
            a[4],
            a[5],
        )
    };
    if n < 0 {
        return Err(host_errno());
    }
    offset_out(mem, a[1], off_in)?;
    offset_out(mem, a[3], off_out)?;
    Ok(n)
}

/// vmsplice(2) between guest memory and a pipe. The host's vmsplice would
/// leave the pipe referring to armor's pages, so the data is copied: into
/// the write end as `writev` does, out of the read end as `readv` does.
/// SPLICE_F_NONBLOCK makes that one transfer non-blocking; SPLICE_F_GIFT
/// and SPLICE_F_MOVE are hints the kernel may ignore.
fn vmsplice(mem: &AddressSpace, fd: u64, iov: u64, count: u64, flags: u64) -> SysResult {
    let fd = fd as i32;
    // SAFETY: an all-zero stat is valid; fstat fills it.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: st is writable.
    if unsafe { libc::fstat(fd, &mut st) } < 0 {
        return Err(host_errno());
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFIFO {
        return Err(err(libc::EBADF));
    }
    // SAFETY: F_GETFL takes no pointer.
    let status = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    let write = match status & libc::O_ACCMODE {
        libc::O_WRONLY => true,
        libc::O_RDONLY => false,
        _ => return Err(err(libc::EBADF)),
    };
    let toggle = flags & SPLICE_F_NONBLOCK != 0 && status & libc::O_NONBLOCK == 0;
    if toggle {
        // SAFETY: integers.
        unsafe { libc::fcntl(fd, libc::F_SETFL, status | libc::O_NONBLOCK) };
    }
    let result = super::vector_io(mem, fd as u64, iov, count, None, 0, write);
    if toggle {
        // SAFETY: integers.
        unsafe { libc::fcntl(fd, libc::F_SETFL, status) };
    }
    result
}
