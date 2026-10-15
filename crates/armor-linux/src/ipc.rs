//! System V semaphores for the guest: x86_64 (`semget`, `semop`,
//! `semctl`, `semtimedop`) and i386 (the `ipc` multiplexer and the direct
//! calls). The constants, `struct sembuf` (6 bytes) and the commands are
//! the same on x86_64, i386 and AArch64 (✅ measured with `sizeof` and the
//! headers' values on all three, 2026-10-14, docs/tech/syscalls.md).
//! Semaphore sets are the host's: other processes, under armor or not,
//! share them.
//!
//! The commands that carry a `struct semid_ds` or `seminfo` (`IPC_STAT`,
//! `IPC_SET`, `IPC_INFO`, `SEM_INFO`, `SEM_STAT`), whose layouts differ,
//! are not supported yet.
//!
//! System V shared memory (x86_64): `shmget`, `shmat` (the host's segment
//! attached into the guest's address space, `AddressSpace::attach_sysv`),
//! `shmdt`, `shmctl`. X11's MIT-SHM extension passes images to the X
//! server this way. `struct shmid_ds` is the same 112 bytes on x86_64 and
//! AArch64 (✅ measured with `sizeof`/`offsetof`, 2026-10-14).

use std::collections::HashMap;
use std::sync::Mutex;

use armor_mem::{AddressSpace, GUEST_PAGE_SIZE, GuestMemory, MapError};

use crate::net::Word;
use crate::syscall::{SysResult, copy_in, copy_out, err, host_errno};

const IPC_RMID: u64 = 0;
const GETPID: u64 = 11;
const GETVAL: u64 = 12;
const GETALL: u64 = 13;
const GETNCNT: u64 = 14;
const GETZCNT: u64 = 15;
const SETVAL: u64 = 16;
const SETALL: u64 = 17;
const IPC_STAT: i32 = 2;
/// The flag of the "new" structure layouts in a command (`IPC_64`); none
/// of the commands handled here has a structure, so it is ignored.
const IPC_64: u64 = 0x100;

/// `struct sembuf`: sem_num (u16), sem_op (i16), sem_flg (i16).
const SEMBUF_SIZE: u64 = 6;
/// The most operations copied in one call (Linux's limit, `SEMOPM`, is
/// 500 by default; the host applies its own).
const MAX_SOPS: u64 = 1 << 16;
/// The most semaphores in a set (Linux's `SEMMSL` is 32000 by default).
const MAX_SEMS: usize = 1 << 16;

fn check(result: libc::c_long) -> SysResult {
    if result < 0 {
        Err(host_errno())
    } else {
        Ok(result)
    }
}

/// semget(2): integers only.
pub(crate) fn semget(key: u64, nsems: u64, flags: u64) -> SysResult {
    // SAFETY: integer arguments only.
    check(unsafe { libc::syscall(libc::SYS_semget, key as i32, nsems as i32, flags as i32) })
}

/// The number of semaphores in set `id`, from the host's IPC_STAT.
fn set_size(id: i32) -> Result<usize, i64> {
    // SAFETY: an all-zero semid_ds is a valid value of this plain C struct.
    let mut ds: libc::semid_ds = unsafe { std::mem::zeroed() };
    // SAFETY: IPC_STAT writes one semid_ds, which `ds` is.
    let r = unsafe { libc::syscall(libc::SYS_semctl, id, 0, IPC_STAT, &mut ds) };
    if r < 0 {
        return Err(host_errno());
    }
    Ok((ds.sem_nsems as usize).min(MAX_SEMS))
}

/// semctl(2) with the `union semun` argument `arg` as a value (an `int`
/// or a guest pointer). `None` for a command with a structure.
pub(crate) fn semctl(
    mem: &AddressSpace,
    id: u64,
    num: u64,
    cmd: u64,
    arg: u64,
) -> Option<SysResult> {
    let (id, num) = (id as i32, num as i32);
    Some(match cmd & !IPC_64 {
        IPC_RMID | GETPID | GETVAL | GETNCNT | GETZCNT => {
            // SAFETY: these commands ignore the fourth argument.
            check(unsafe { libc::syscall(libc::SYS_semctl, id, num, (cmd & !IPC_64) as i32, 0) })
        }
        SETVAL => {
            // SAFETY: SETVAL takes the value itself, an int.
            check(unsafe { libc::syscall(libc::SYS_semctl, id, num, SETVAL as i32, arg as i32) })
        }
        GETALL => get_all(mem, id, arg),
        SETALL => set_all(mem, id, arg),
        _ => return None,
    })
}

/// GETALL: one u16 per semaphore, written to the guest's array.
fn get_all(mem: &AddressSpace, id: i32, array: u64) -> SysResult {
    let mut values = vec![0u16; set_size(id)?];
    // SAFETY: GETALL writes one u16 per semaphore of the set, which
    // `values` holds.
    check(unsafe { libc::syscall(libc::SYS_semctl, id, 0, GETALL as i32, values.as_mut_ptr()) })?;
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    copy_out(mem, array, &bytes)?;
    Ok(0)
}

/// SETALL: one u16 per semaphore, read from the guest's array.
fn set_all(mem: &AddressSpace, id: i32, array: u64) -> SysResult {
    let mut bytes = vec![0u8; 2 * set_size(id)?];
    mem.read(array, &mut bytes).map_err(|_| err(libc::EFAULT))?;
    let values: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| u16::from_le_bytes(b))
        .collect();
    // SAFETY: SETALL reads one u16 per semaphore of the set.
    check(unsafe { libc::syscall(libc::SYS_semctl, id, 0, SETALL as i32, values.as_ptr()) })
}

/// semop(2) and semtimedop(2): `nsops` operations at `sops`, and a
/// timeout of width `time` (null: wait without limit).
pub(crate) fn semtimedop(
    mem: &AddressSpace,
    id: u64,
    sops: u64,
    nsops: u64,
    timeout: u64,
    time: Word,
) -> SysResult {
    if nsops == 0 {
        return Err(err(libc::EINVAL));
    }
    if nsops > MAX_SOPS {
        return Err(err(libc::E2BIG));
    }
    let mut ops = vec![0u8; (nsops * SEMBUF_SIZE) as usize];
    mem.read(sops, &mut ops).map_err(|_| err(libc::EFAULT))?;
    let spec = if timeout == 0 {
        None
    } else {
        Some(match time {
            Word::W64 => {
                let t = copy_in::<16>(mem, timeout)?;
                libc::timespec {
                    tv_sec: i64::from_le_bytes(t[..8].try_into().unwrap()),
                    tv_nsec: i64::from_le_bytes(t[8..].try_into().unwrap()),
                }
            }
            Word::W32 => {
                let t = copy_in::<8>(mem, timeout)?;
                libc::timespec {
                    tv_sec: i64::from(i32::from_le_bytes(t[..4].try_into().unwrap())),
                    tv_nsec: i64::from(i32::from_le_bytes(t[4..].try_into().unwrap())),
                }
            }
        })
    };
    let spec_ptr = spec
        .as_ref()
        .map_or(std::ptr::null(), |s| s as *const libc::timespec);
    // SAFETY: ops holds `nsops` sembuf records (same layout on the host);
    // spec_ptr is null or a local timespec, checked by the host as by
    // Linux.
    check(unsafe {
        libc::syscall(
            libc::SYS_semtimedop,
            id as i32,
            ops.as_ptr(),
            nsops as usize,
            spec_ptr,
        )
    })
}

/// i386's `ipc` multiplexer for the semaphore calls (low 16 bits of
/// `call`; the high ones are a version). `None` for the message queue and
/// shared memory calls, and the semctl commands with a structure.
pub(crate) fn ipc(mem: &AddressSpace, a: [u64; 6]) -> Option<SysResult> {
    let (call, first, second, third, ptr, fifth) = (a[0] & 0xffff, a[1], a[2], a[3], a[4], a[5]);
    Some(match call {
        // SEMOP: semtimedop without a timeout.
        1 => semtimedop(mem, first, ptr, second, 0, Word::W32),
        2 => semget(first, second, third),
        // SEMCTL: `ptr` points to the union's 32-bit value.
        3 => {
            if ptr == 0 {
                return Some(Err(err(libc::EINVAL)));
            }
            match copy_in::<4>(mem, ptr) {
                Ok(value) => semctl(
                    mem,
                    first,
                    second,
                    third,
                    u64::from(u32::from_le_bytes(value)),
                )?,
                Err(e) => Err(e),
            }
        }
        // SEMTIMEDOP: the timeout is a 32-bit timespec.
        4 => semtimedop(mem, first, ptr, second, fifth, Word::W32),
        _ => return None,
    })
}

const SHM_RDONLY: u64 = 0o10000;
const SHM_RND: u64 = 0o20000;
const SHM_REMAP: u64 = 0o40000;
const SHM_EXEC: u64 = 0o100000;
/// x86's SHMLBA: one 4K page.
const SHMLBA: u64 = 4096;
const SHMID_DS_SIZE: usize = 112;
/// shmctl commands with a `struct shmid_ds`: IPC_SET, IPC_STAT, SHM_STAT,
/// SHM_STAT_ANY; and the ones with integers only.
const SHM_IPC_SET: u64 = 1;
const SHM_IPC_STAT: u64 = 2;
const SHM_LOCK: u64 = 11;
const SHM_UNLOCK: u64 = 12;
const SHM_STAT: u64 = 13;
const SHM_STAT_ANY: u64 = 15;
/// shmctl commands that report the system's limits and usage: IPC_INFO
/// with a `struct shminfo` (nine longs), SHM_INFO with a `struct shm_info`
/// (an int, padding, five longs). Both layouts are the same on x86-64 and
/// AArch64 (LP64, the generic kernel structures).
const SHM_IPC_INFO: u64 = 3;
const SHM_INFO: u64 = 14;
const SHMINFO_SIZE: usize = 72;
const SHM_INFO_SIZE: usize = 48;

/// The guest's attachments: address and length, for shmdt. A forked child
/// inherits them, as on Linux.
static ATTACHMENTS: Mutex<Option<HashMap<u64, u64>>> = Mutex::new(None);

/// shmget(2): integers only.
pub(crate) fn shmget(key: u64, size: u64, flags: u64) -> SysResult {
    // SAFETY: integer arguments only.
    check(unsafe { libc::syscall(libc::SYS_shmget, key as i32, size as usize, flags as i32) })
}

/// shmat(2): the size from the host's IPC_STAT, then the segment attached
/// whole. As in Linux, an address needs SHM_RND to be rounded down to
/// SHMLBA, and SHM_REMAP to replace mappings already there.
pub(crate) fn shmat(mem: &AddressSpace, id: u64, addr: u64, flags: u64) -> SysResult {
    let id = id as i32;
    // SAFETY: an all-zero shmid_ds is a valid value of this plain C struct.
    let mut ds: libc::shmid_ds = unsafe { std::mem::zeroed() };
    // SAFETY: IPC_STAT writes one shmid_ds, which `ds` is.
    if unsafe { libc::shmctl(id, libc::IPC_STAT, &mut ds) } < 0 {
        return Err(host_errno());
    }
    let len = ds.shm_segsz as u64;
    let addr = if flags & SHM_RND != 0 {
        addr - addr % SHMLBA
    } else {
        addr
    };
    if !addr.is_multiple_of(SHMLBA) {
        return Err(err(libc::EINVAL));
    }
    let fixed = (addr != 0).then_some(addr);
    if let Some(at) = fixed
        && flags & SHM_REMAP == 0
        && (0..len.div_ceil(GUEST_PAGE_SIZE)).any(|i| mem.prot(at + i * GUEST_PAGE_SIZE).is_some())
    {
        return Err(err(libc::EINVAL));
    }
    let got = mem
        .attach_sysv(
            fixed,
            len,
            id,
            flags & SHM_RDONLY != 0,
            flags & SHM_EXEC != 0,
        )
        .map_err(|e| match e {
            MapError::Host(e) => err(e.raw_os_error().unwrap_or(libc::ENOMEM)),
            MapError::SharedPlacement(_) | MapError::Unaligned { .. } => err(libc::EINVAL),
            _ => err(libc::ENOMEM),
        })?;
    ATTACHMENTS
        .lock()
        .expect("attachments lock poisoned")
        .get_or_insert_with(HashMap::new)
        .insert(got, len);
    Ok(got as i64)
}

/// shmdt(2): the attachment at `addr` (EINVAL if none starts there).
pub(crate) fn shmdt(mem: &AddressSpace, addr: u64) -> SysResult {
    let len = ATTACHMENTS
        .lock()
        .expect("attachments lock poisoned")
        .get_or_insert_with(HashMap::new)
        .remove(&addr)
        .ok_or_else(|| err(libc::EINVAL))?;
    mem.unmap(addr, len).map_err(|_| err(libc::EINVAL))?;
    Ok(0)
}

/// shmctl(2): IPC_RMID, SHM_LOCK and SHM_UNLOCK with integers; IPC_STAT,
/// SHM_STAT, SHM_STAT_ANY and IPC_SET with the 112-byte `struct shmid_ds`
/// copied as it is; IPC_INFO and SHM_INFO with their structures.
pub(crate) fn shmctl(mem: &AddressSpace, id: u64, cmd: u64, buf: u64) -> Option<SysResult> {
    let cmd = cmd & !IPC_64;
    Some(match cmd {
        IPC_RMID | SHM_LOCK | SHM_UNLOCK => {
            // SAFETY: these commands take no structure.
            check(unsafe { libc::syscall(libc::SYS_shmctl, id as i32, cmd as i32, 0usize) })
        }
        SHM_IPC_STAT | SHM_STAT | SHM_STAT_ANY | SHM_IPC_SET => (|| {
            let mut ds = [0u8; SHMID_DS_SIZE];
            if cmd == SHM_IPC_SET {
                ds = copy_in::<SHMID_DS_SIZE>(mem, buf)?;
            }
            // SAFETY: `ds` is a 112-byte struct shmid_ds, the host's layout.
            let r = check(unsafe {
                libc::syscall(libc::SYS_shmctl, id as i32, cmd as i32, ds.as_mut_ptr())
            })?;
            if cmd != SHM_IPC_SET {
                copy_out(mem, buf, &ds)?;
            }
            Ok(r)
        })(),
        SHM_IPC_INFO | SHM_INFO => (|| {
            let mut info = [0u8; SHMINFO_SIZE];
            let size = if cmd == SHM_IPC_INFO {
                SHMINFO_SIZE
            } else {
                SHM_INFO_SIZE
            };
            // SAFETY: `info` holds the larger of the two structures, the
            // host's layouts.
            let r = check(unsafe {
                libc::syscall(libc::SYS_shmctl, id as i32, cmd as i32, info.as_mut_ptr())
            })?;
            copy_out(mem, buf, &info[..size])?;
            Ok(r)
        })(),
        _ => return None,
    })
}
