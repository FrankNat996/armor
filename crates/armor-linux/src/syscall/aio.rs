//! Linux AIO (ADR-0044): `io_setup`, `io_destroy`, `io_submit`,
//! `io_cancel`, `io_getevents`, `io_pgetevents`. `struct iocb` (64 bytes)
//! and `struct io_event` (32) are the same on both architectures.
//!
//! The kernel reads the guest's iocbs and later moves the data to and
//! from the guest's buffers by itself, after `io_submit` returns, so the
//! host gets the guest's addresses, as for futex (ADR-0018): armor's own
//! identity-mapped memory, checked first to be mapped with the access each
//! operation needs. As with DMA, data the kernel writes into translated
//! code does not invalidate the translations.

use armor_mem::{AddressSpace, GuestMemory, Prot};

use super::{SysResult, Thread, copy_in, copy_out, err, host_errno};

pub mod nr {
    pub const IO_SETUP: u64 = 206;
    pub const IO_DESTROY: u64 = 207;
    pub const IO_GETEVENTS: u64 = 208;
    pub const IO_SUBMIT: u64 = 209;
    pub const IO_CANCEL: u64 = 210;
    pub const IO_PGETEVENTS: u64 = 333;
}

pub const HANDLED: &[u64] = &[
    nr::IO_SETUP,
    nr::IO_DESTROY,
    nr::IO_GETEVENTS,
    nr::IO_SUBMIT,
    nr::IO_CANCEL,
    nr::IO_PGETEVENTS,
];

// iocb opcodes.
const IOCB_CMD_PREAD: u16 = 0;
const IOCB_CMD_PWRITE: u16 = 1;
const IOCB_CMD_PREADV: u16 = 7;
const IOCB_CMD_PWRITEV: u16 = 8;

/// AArch64's io_pgetevents (asm-generic/unistd.h; not in the libc crate).
const SYS_IO_PGETEVENTS: libc::c_long = 292;

/// The most events moved in one io_getevents.
const EVENTS_MAX: u64 = 4096;

pub(super) fn dispatch(
    mem: &AddressSpace,
    thread: &mut Thread,
    number: u64,
    a: [u64; 6],
) -> Option<Result<SysResult, String>> {
    let result = match number {
        nr::IO_SETUP => io_setup(mem, a[0], a[1]),
        nr::IO_DESTROY => raw(libc::SYS_io_destroy, &[a[0]]),
        nr::IO_SUBMIT => io_submit(mem, a[0], a[1], a[2]),
        nr::IO_CANCEL => io_cancel(mem, a[0], a[1], a[2]),
        nr::IO_GETEVENTS => io_getevents(mem, a[0], a[1], a[2], a[3], a[4], None),
        nr::IO_PGETEVENTS => io_pgetevents(mem, thread, a),
        _ => return None,
    };
    Some(Ok(result))
}

fn raw(number: libc::c_long, args: &[u64]) -> SysResult {
    let arg = |i: usize| args.get(i).copied().unwrap_or(0);
    // SAFETY: callers pass integers, pointers to local copies, or guest
    // addresses checked as the module's description says.
    let r = unsafe { libc::syscall(number, arg(0), arg(1), arg(2), arg(3), arg(4), arg(5)) };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

fn io_setup(mem: &AddressSpace, events: u64, ctx: u64) -> SysResult {
    // The kernel refuses a context id that is not zero on entry.
    if u64::from_le_bytes(copy_in::<8>(mem, ctx)?) != 0 {
        return Err(err(libc::EINVAL));
    }
    let mut id = 0u64;
    raw(libc::SYS_io_setup, &[events, &raw mut id as u64])?;
    copy_out(mem, ctx, &id.to_le_bytes())?;
    Ok(0)
}

/// Whether `[start, start + len)` is mapped in the guest with `need`.
fn mapped(mem: &AddressSpace, start: u64, len: u64, need: Prot) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = start.checked_add(len) else {
        return false;
    };
    let mut page = start & !0xfff;
    while page < end {
        match mem.prot(page) {
            Some(prot) if prot.contains(need) => {}
            _ => return false,
        }
        page += 0x1000;
    }
    true
}

/// Checks an iocb at `at`: EFAULT if the guest could not read and write
/// it; then whether the guest could make its buffers' accesses itself.
fn check_iocb(mem: &AddressSpace, at: u64) -> Result<bool, i64> {
    if !mapped(mem, at, 64, Prot::READ | Prot::WRITE) {
        return Err(err(libc::EFAULT));
    }
    let iocb = copy_in::<64>(mem, at)?;
    let opcode = u16::from_le_bytes(iocb[16..18].try_into().expect("2 bytes"));
    let buf = u64::from_le_bytes(iocb[24..32].try_into().expect("8 bytes"));
    let nbytes = u64::from_le_bytes(iocb[32..40].try_into().expect("8 bytes"));
    Ok(match opcode {
        IOCB_CMD_PREAD => mapped(mem, buf, nbytes, Prot::WRITE),
        IOCB_CMD_PWRITE => mapped(mem, buf, nbytes, Prot::READ),
        IOCB_CMD_PREADV | IOCB_CMD_PWRITEV => {
            let need = if opcode == IOCB_CMD_PREADV {
                Prot::WRITE
            } else {
                Prot::READ
            };
            nbytes <= 1024
                && mapped(mem, buf, nbytes * 16, Prot::READ)
                && (0..nbytes).all(|i| {
                    let entry = copy_in::<16>(mem, buf + i * 16).unwrap_or([0xff; 16]);
                    let base = u64::from_le_bytes(entry[..8].try_into().expect("8 bytes"));
                    let len = u64::from_le_bytes(entry[8..].try_into().expect("8 bytes"));
                    mapped(mem, base, len, need)
                })
        }
        _ => true,
    })
}

/// io_submit(2): ctx, nr, iocbpp. The guest's array of iocb pointers is
/// copied; the iocbs are the guest's own. Up to the first iocb the guest
/// could not read or write, like the kernel (its EFAULT only if it is the
/// first). An iocb whose buffers the guest could not access gets the
/// address 0 in its `aio_buf` for the duration of the call (the kernel
/// reads the iocb only then), so the kernel itself reports the fault, at
/// submission or in the event, as on x86; the guest's value is put back.
fn io_submit(mem: &AddressSpace, ctx: u64, count: u64, list: u64) -> SysResult {
    if (count as i64) < 0 {
        return Err(err(libc::EINVAL));
    }
    let count = count.min(EVENTS_MAX);
    let mut pointers = vec![0u8; count as usize * 8];
    mem.read(list, &mut pointers)
        .map_err(|_| err(libc::EFAULT))?;
    let pointers: Vec<u64> = pointers
        .chunks(8)
        .map(|p| u64::from_le_bytes(p.try_into().expect("8 bytes")))
        .collect();
    let mut patched = Vec::new();
    let mut good = 0;
    for &at in &pointers {
        match check_iocb(mem, at) {
            Ok(true) => {}
            Ok(false) => {
                let buf = copy_in::<8>(mem, at + 24)?;
                copy_out(mem, at + 24, &0u64.to_le_bytes())?;
                patched.push((at, buf));
            }
            Err(e) if good == 0 => return Err(e),
            Err(_) => break,
        }
        good += 1;
    }
    let result = raw(
        libc::SYS_io_submit,
        &[ctx, good as u64, pointers.as_ptr() as u64],
    );
    for (at, buf) in patched {
        copy_out(mem, at + 24, &buf)?;
    }
    result
}

/// io_cancel(2): ctx, iocb (the guest's, checked), result (an io_event).
fn io_cancel(mem: &AddressSpace, ctx: u64, iocb: u64, result: u64) -> SysResult {
    check_iocb(mem, iocb)?;
    let mut event = [0u8; 32];
    let r = raw(libc::SYS_io_cancel, &[ctx, iocb, event.as_mut_ptr() as u64])?;
    copy_out(mem, result, &event)?;
    Ok(r)
}

/// io_getevents(2), and io_pgetevents with the host mask for the wait.
fn io_getevents(
    mem: &AddressSpace,
    ctx: u64,
    min: u64,
    count: u64,
    events: u64,
    timeout: u64,
    mask: Option<&libc::sigset_t>,
) -> SysResult {
    if (count as i64) < 0 || (min as i64) < 0 || min > count {
        return Err(err(libc::EINVAL));
    }
    let count = count.min(EVENTS_MAX);
    let mut out = vec![0u8; count as usize * 32];
    let spec = if timeout != 0 {
        Some(copy_in::<16>(mem, timeout)?)
    } else {
        None
    };
    let spec_ptr = spec.as_ref().map_or(0, |t| t.as_ptr() as u64);
    let n = match mask {
        None => raw(
            libc::SYS_io_getevents,
            &[ctx, min, count, out.as_mut_ptr() as u64, spec_ptr],
        )?,
        Some(set) => {
            let usig = [set as *const libc::sigset_t as u64, 8u64];
            raw(
                SYS_IO_PGETEVENTS,
                &[
                    ctx,
                    min,
                    count,
                    out.as_mut_ptr() as u64,
                    spec_ptr,
                    usig.as_ptr() as u64,
                ],
            )?
        }
    };
    copy_out(mem, events, &out[..n as usize * 32])?;
    Ok(n)
}

/// io_pgetevents(2): as io_getevents, with `struct __aio_sigset` (mask
/// pointer, size) for the wait, which takes the guest's mask the way ppoll
/// does.
fn io_pgetevents(mem: &AddressSpace, thread: &mut Thread, a: [u64; 6]) -> SysResult {
    if a[5] == 0 {
        return io_getevents(mem, a[0], a[1], a[2], a[3], a[4], None);
    }
    let usig = copy_in::<16>(mem, a[5])?;
    let set = u64::from_le_bytes(usig[..8].try_into().expect("8 bytes"));
    let size = u64::from_le_bytes(usig[8..].try_into().expect("8 bytes"));
    if set == 0 {
        return io_getevents(mem, a[0], a[1], a[2], a[3], a[4], None);
    }
    if size != 8 {
        return Err(err(libc::EINVAL));
    }
    let mask = u64::from_le_bytes(copy_in::<8>(mem, set)?);
    match thread.signals.begin_masked_wait(mask) {
        Some(host) => io_getevents(mem, a[0], a[1], a[2], a[3], a[4], Some(&host)),
        None => Err(err(libc::EINTR)),
    }
}
