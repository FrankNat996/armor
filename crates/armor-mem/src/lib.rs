//! Guest address space: 4K guest pages on top of the host page size.
//!
//! Guest addresses are identity-mapped (guest address == host address).
//! The host pages backing the guest are always mapped read-write; the guest
//! permissions live in a software table with one entry per 4K guest page and
//! are enforced on every access. This is the minimal M1 strategy for running
//! 4K-page binaries on a 16K-page host (docs/tech/memory.md, ADR-0005).
//!
//! Host pointers never leave this crate: everything else works with guest
//! addresses as `u64`.
//!
//! An `AddressSpace` is shared by the guest's threads (ADR-0018): every
//! method takes `&self`, and the page tables are behind a `RwLock`. Guest
//! accesses copy bytes under the read lock, so that no thread can unmap
//! the host pages being copied; races between guest threads on the bytes
//! themselves are the guest's own.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub use armor_guest::{Access, GUEST_PAGE_SIZE, GuestMemory, MemFault, Prot};

/// The value rdtsc reads (`GuestMemory::time_stamp_counter`):
/// nanoseconds since the first read, so it only grows. Real counters tick
/// at a CPU-specific rate that CPUID does not report here (no leaf 0x15),
/// so programs cannot rely on the unit.
pub fn time_stamp_counter() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// A 16-byte compare-and-swap on host memory: LSE's CASPAL when the host
/// has FEAT_LSE, else an exclusive loop (ARMv8.0, ADR-0032). Returns the
/// old value.
///
/// # Safety
/// `addr` must be 16-byte aligned and mapped read-write in the host for
/// the whole call.
#[cfg(target_arch = "aarch64")]
unsafe fn host_compare_exchange_16(addr: u64, expected: u128, new: u128) -> u128 {
    // SAFETY: the caller's contract.
    unsafe {
        if host_has_lse() {
            compare_exchange_16_lse(addr, expected, new)
        } else {
            compare_exchange_16_exclusive(addr, expected, new)
        }
    }
}

/// Whether the host has FEAT_LSE (`HWCAP_ATOMICS` in `AT_HWCAP`), read
/// once; `ARMOR_HOST_FEATURES=baseline` says no, as for the JIT.
#[cfg(target_arch = "aarch64")]
fn host_has_lse() -> bool {
    use std::sync::OnceLock;
    static LSE: OnceLock<bool> = OnceLock::new();
    *LSE.get_or_init(|| {
        const HWCAP_ATOMICS: u64 = 1 << 8;
        if std::env::var_os("ARMOR_HOST_FEATURES").is_some_and(|v| v == "baseline") {
            return false;
        }
        // SAFETY: getauxval reads the process's auxiliary vector.
        unsafe { libc::getauxval(libc::AT_HWCAP) & HWCAP_ATOMICS != 0 }
    })
}

/// CASPAL (FEAT_LSE).
///
/// # Safety
/// As `host_compare_exchange_16`; the host must have FEAT_LSE.
#[cfg(target_arch = "aarch64")]
unsafe fn compare_exchange_16_lse(addr: u64, expected: u128, new: u128) -> u128 {
    let (mut low, mut high) = (expected as u64, (expected >> 64) as u64);
    // SAFETY: the caller's contract; CASPAL takes the comparison value and
    // the new value in consecutive register pairs, and the address in x4.
    unsafe {
        std::arch::asm!(
            ".arch_extension lse",
            "caspal x0, x1, x2, x3, [x4]",
            inout("x0") low,
            inout("x1") high,
            in("x2") new as u64,
            in("x3") (new >> 64) as u64,
            in("x4") addr,
            options(nostack),
        );
    }
    u128::from(low) | (u128::from(high) << 64)
}

/// The same with LDAXP/STLXP (ARMv8.0). On a mismatch it stores the old
/// value back: x86's cmpxchg16b writes in both cases, and only a
/// successful STLXP makes the 16-byte read single-copy atomic.
///
/// # Safety
/// As `host_compare_exchange_16`.
#[cfg(target_arch = "aarch64")]
unsafe fn compare_exchange_16_exclusive(addr: u64, expected: u128, new: u128) -> u128 {
    let (low, high): (u64, u64);
    // SAFETY: the caller's contract; x0/x1 the expected value, x2/x3 the
    // new one, x4 the address; x5/x6 receive the old value, w7 is the
    // store's status.
    unsafe {
        std::arch::asm!(
            "2:",
            "ldaxp x5, x6, [x4]",
            "cmp x5, x0",
            "ccmp x6, x1, #0, eq",
            "b.ne 3f",
            "stlxp w7, x2, x3, [x4]",
            "cbnz w7, 2b",
            "b 4f",
            "3:",
            "stlxp w7, x5, x6, [x4]",
            "cbnz w7, 2b",
            "4:",
            in("x0") expected as u64,
            in("x1") (expected >> 64) as u64,
            in("x2") new as u64,
            in("x3") (new >> 64) as u64,
            in("x4") addr,
            out("x5") low,
            out("x6") high,
            out("x7") _,
            options(nostack),
        );
    }
    u128::from(low) | (u128::from(high) << 64)
}

/// `atomic_update` on host memory already checked readable and writable:
/// a compare-and-swap loop when `addr` is aligned to `size` (the host's
/// atomics need it), else a plain read and write. Unaligned LOCK
/// operations ("split locks") are therefore not atomic against other
/// processes; x86 makes them atomic, and they are rare.
///
/// # Safety
/// `[addr, addr + size)` must be mapped read-write in the host for the
/// whole call.
unsafe fn host_atomic_update(addr: u64, size: usize, update: &mut dyn FnMut(u64) -> u64) -> u64 {
    use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32};
    macro_rules! cas_loop {
        ($atomic:ty, $int:ty) => {{
            // SAFETY: aligned (checked by the caller of the macro), and
            // mapped read-write (the function's contract). Other processes
            // only touch it with atomic instructions or plain accesses, as
            // with any shared memory.
            let cell = unsafe { <$atomic>::from_ptr(addr as *mut $int) };
            let mut old = cell.load(Ordering::SeqCst);
            loop {
                let new = update(u64::from(old)) as $int;
                match cell.compare_exchange_weak(old, new, Ordering::SeqCst, Ordering::SeqCst) {
                    Ok(_) => break u64::from(old),
                    Err(current) => old = current,
                }
            }
        }};
    }
    if !addr.is_multiple_of(size as u64) {
        let mut bytes = [0u8; 8];
        // SAFETY: the function's contract.
        unsafe { std::ptr::copy_nonoverlapping(addr as *const u8, bytes.as_mut_ptr(), size) };
        let old = u64::from_le_bytes(bytes);
        let new = update(old).to_le_bytes();
        // SAFETY: the function's contract.
        unsafe { std::ptr::copy_nonoverlapping(new.as_ptr(), addr as *mut u8, size) };
        return old;
    }
    match size {
        1 => cas_loop!(AtomicU8, u8),
        2 => cas_loop!(AtomicU16, u16),
        4 => cas_loop!(AtomicU32, u32),
        _ => cas_loop!(AtomicU64, u64),
    }
}

#[derive(Debug)]
pub enum MapError {
    Unaligned {
        addr: u64,
    },
    /// The host already uses part of the requested range.
    AddressInUse {
        addr: u64,
        len: u64,
    },
    /// The range does not fit in the 64-bit address space.
    OutOfRange,
    Host(io::Error),
    /// A shared mapping cannot be placed there with this host's page size,
    /// or a private one would share a host page with a shared one
    /// (ADR-0022).
    SharedPlacement(&'static str),
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::Unaligned { addr } => write!(f, "address {addr:#x} is not 4K-aligned"),
            MapError::AddressInUse { addr, len } => write!(
                f,
                "range {addr:#x}..{:#x} is already in use on the host",
                addr.wrapping_add(*len)
            ),
            MapError::OutOfRange => write!(f, "range outside the address space"),
            MapError::Host(e) => write!(f, "host mmap failed: {e}"),
            MapError::SharedPlacement(why) => write!(f, "shared mapping: {why}"),
        }
    }
}

impl std::error::Error for MapError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectError {
    Unaligned,
    /// Part of the range is not mapped (`mprotect` returns ENOMEM).
    NotMapped,
    /// Write permission on a shared mapping the host maps read-only: a
    /// file opened read-only (`mprotect` returns EACCES).
    ReadOnlyShared,
    /// Executable shared memory, not supported (ADR-0022).
    ExecutableShared,
}

/// What a shared mapping maps (ADR-0022).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedSource {
    Anonymous,
    /// A host file descriptor, and the 4K-aligned offset in the file.
    File {
        fd: i32,
        offset: u64,
    },
}

pub struct AddressSpace {
    host_page_size: u64,
    tables: RwLock<Tables>,
    /// Incremented by every mapping change and every write to an
    /// executable page (`GuestMemory::code_generation`).
    code_generation: AtomicU64,
    /// Concurrent mode (ADR-0023): set once guest code may run in
    /// parallel with other code writing the same memory, at the first
    /// writable shared mapping (ADR-0022) or the first guest thread, and
    /// never cleared. From then on guest accesses are ordered like x86
    /// (TSO) and LOCK instructions are atomic on the host
    /// (`AddressSpace::enter_concurrent_mode`).
    concurrent: AtomicBool,
    /// Threads waiting to change the page tables: `view` lets them in first.
    writers_waiting: AtomicU32,
    /// `--limit-memory` (ADR-0027): the most guest address space, in bytes
    /// of host pages, that mappings may take; 0 for no limit. Counted by
    /// armor, so not a security boundary (docs/tech/security.md).
    limit: AtomicU64,
    /// Mappings whose place armor chooses end at or below this address; 0
    /// for none (`set_ceiling`): 4 GiB for a 32-bit guest (ADR-0029), 2^47
    /// less a page on a host that places mappings above what an x86-64
    /// guest addresses.
    ceiling: AtomicU64,
    /// ADR-0046, host protection: 0, or the end of the region reserved for
    /// the guest's addresses (`enable_host_protection`). Then the host
    /// pages of guest pages that allow no access are inaccessible on the
    /// host too, so that translated code may load without checking the
    /// permission table, and armor's own memory is outside the region.
    region_end: u64,
    /// Serializes the temporary protection changes of `with_access`.
    access_lock: std::sync::Mutex<()>,
}

/// The lowest address a mapping placed below the ceiling may start at:
/// Linux's default `vm.mmap_min_addr`.
const PLACEMENT_FLOOR: u64 = 0x1_0000;

/// ADR-0046: inaccessible bytes reserved right above the guest's region.
pub const REGION_GUARD: u64 = 0x1_0000;

#[derive(Default)]
struct Tables {
    /// Guest permissions of every guest page, by page number.
    pages: PageTable,
    /// Host pages (by host page number) mapped by this address space.
    host_pages: BTreeSet<u64>,
    /// The host pages of `host_pages` that back shared mappings (ADR-0022),
    /// with whether the host maps them writable. No private guest page
    /// ever lives in one.
    shared: BTreeMap<u64, bool>,
}

/// Guest page numbers: 47-bit addresses, 4K pages.
const PAGE_TABLE_ENTRIES: u64 = 1 << (47 - 12);
/// Marks a mapped page in `PageTable`; the low bits are its `Prot`.
const MAPPED: u8 = 0x80;
pub use armor_guest::{
    PAGE_READABLE_BIT, PAGE_READABLE_SPAN_BIT, PAGE_WRITABLE_DATA_BIT, PAGE_WRITABLE_DATA_SPAN_BIT,
};

/// The permissions of every guest page in a flat array, one byte per page
/// (ADR-0021): 0 for an unmapped page, `MAPPED | prot` plus the derived
/// bits (`PAGE_READABLE_BIT`, `PAGE_WRITABLE_DATA_BIT`) otherwise. The 32
/// GiB array is reserved with MAP_NORESERVE: only the parts the guest uses
/// take memory. A lookup is one load, where a `BTreeMap` walked a tree;
/// the JIT will read the same table from generated code.
struct PageTable {
    base: NonNull<u8>,
}

// SAFETY: the table is plain bytes; `Tables` is only reached through the
// address space's RwLock, which orders every write before later reads.
unsafe impl Send for PageTable {}
// SAFETY: as above.
unsafe impl Sync for PageTable {}

impl Default for PageTable {
    fn default() -> PageTable {
        // SAFETY: an anonymous mapping at an address the kernel chooses;
        // MAP_NORESERVE makes the reservation cost no memory until touched.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE_TABLE_ENTRIES as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        assert!(
            ptr != libc::MAP_FAILED,
            "cannot reserve the 32 GiB page-permission table: {} \
             (vm.overcommit_memory must not be 2, docs/tech/jit.md)",
            io::Error::last_os_error()
        );
        PageTable {
            base: NonNull::new(ptr.cast()).expect("mmap does not return null"),
        }
    }
}

impl Drop for PageTable {
    fn drop(&mut self) {
        // SAFETY: unmapping the reservation made in `default`.
        unsafe { libc::munmap(self.base.as_ptr().cast(), PAGE_TABLE_ENTRIES as usize) };
    }
}

impl PageTable {
    fn get(&self, page: u64) -> Option<Prot> {
        if page >= PAGE_TABLE_ENTRIES {
            return None;
        }
        // SAFETY: `page` is inside the reservation.
        let byte = unsafe { *self.base.as_ptr().add(page as usize) };
        (byte & MAPPED != 0).then_some(Prot::from_bits(byte))
    }

    fn contains_key(&self, page: u64) -> bool {
        self.get(page).is_some()
    }

    fn set(&mut self, page: u64, byte: u8) -> Option<Prot> {
        let old = self.get(page);
        assert!(
            page < PAGE_TABLE_ENTRIES,
            "guest page {page:#x} beyond 47 bits"
        );
        self.write(page, byte);
        // The span bits of this page and of the one before depend on it.
        self.refresh_span(page);
        if page > 0 {
            self.refresh_span(page - 1);
        }
        old
    }

    fn read(&self, page: u64) -> u8 {
        if page >= PAGE_TABLE_ENTRIES {
            return 0;
        }
        // SAFETY: `page` is inside the reservation.
        unsafe { *self.base.as_ptr().add(page as usize) }
    }

    fn write(&mut self, page: u64, byte: u8) {
        // SAFETY: the callers pass a page inside the reservation; `&mut
        // self` means the tables are locked for writing.
        unsafe { *self.base.as_ptr().add(page as usize) = byte };
    }

    /// Recomputes `PAGE_READABLE_SPAN_BIT` and `PAGE_WRITABLE_DATA_SPAN_BIT`
    /// of `page` from its own bits and the next page's.
    fn refresh_span(&mut self, page: u64) {
        let byte = self.read(page);
        if byte & MAPPED == 0 {
            return;
        }
        let next = self.read(page + 1);
        let both = |bit: u32| (byte >> bit) & (next >> bit) & 1;
        let span = (both(PAGE_READABLE_BIT) << PAGE_READABLE_SPAN_BIT)
            | (both(PAGE_WRITABLE_DATA_BIT) << PAGE_WRITABLE_DATA_SPAN_BIT);
        let cleared = byte & !(1 << PAGE_READABLE_SPAN_BIT | 1 << PAGE_WRITABLE_DATA_SPAN_BIT);
        self.write(page, cleared | span);
    }

    fn insert(&mut self, page: u64, prot: Prot) -> Option<Prot> {
        let readable = u8::from(prot != Prot::NONE) << PAGE_READABLE_BIT;
        let data = u8::from(prot.contains(Prot::WRITE) && !prot.contains(Prot::EXEC))
            << PAGE_WRITABLE_DATA_BIT;
        self.set(page, MAPPED | prot.bits() | readable | data)
    }

    fn remove(&mut self, page: u64) -> Option<Prot> {
        if page >= PAGE_TABLE_ENTRIES {
            return None;
        }
        self.set(page, 0)
    }
}

/// The end of an x86-64 guest's user addresses: 47 bits less the last
/// page, which x86-64 Linux never gives to user space (`TASK_SIZE`), and
/// which armor's syscalls refuse as EFAULT (`USER_END`).
const GUEST_ADDRESS_LIMIT: u64 = PAGE_TABLE_ENTRIES * GUEST_PAGE_SIZE - GUEST_PAGE_SIZE;

/// Whether the host puts a mapping whose place it chooses at or above
/// `limit`: kernels with more than 47 bits of user addresses do (Ubuntu's
/// kernel with 64K pages gives 48 bits, and its first mappings land above
/// 2^47), those of the Raspberry Pi (39 bits with 4K pages, 47 with 16K)
/// do not.
fn host_places_above(limit: u64) -> bool {
    let len = host_page_size() as usize;
    // SAFETY: an anonymous inaccessible mapping, unmapped right away.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return false;
    }
    // SAFETY: the mapping just made.
    unsafe { libc::munmap(ptr, len) };
    ptr as u64 + len as u64 > limit
}

/// The host's page size, in bytes.
pub fn host_page_size() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(size).expect("host page size is positive")
}

impl AddressSpace {
    pub fn new() -> AddressSpace {
        let host_page_size = host_page_size();
        assert!(
            host_page_size >= GUEST_PAGE_SIZE && host_page_size.is_multiple_of(GUEST_PAGE_SIZE),
            "host page size {host_page_size} is not a multiple of 4K"
        );
        AddressSpace {
            host_page_size,
            tables: RwLock::new(Tables::default()),
            code_generation: AtomicU64::new(0),
            concurrent: AtomicBool::new(false),
            writers_waiting: AtomicU32::new(0),
            limit: AtomicU64::new(0),
            ceiling: AtomicU64::new(if host_places_above(GUEST_ADDRESS_LIMIT) {
                GUEST_ADDRESS_LIMIT
            } else {
                0
            }),
            region_end: 0,
            access_lock: std::sync::Mutex::new(()),
        }
    }

    /// ADR-0046: on a host with 4K pages, reserves the guest's part of the
    /// address space (below 2^47 on hosts with 48 bits or more, the lower
    /// half on smaller ones: below 2^38 with 39 bits) with an inaccessible
    /// mapping, places every guest mapping inside it, and from then on
    /// makes the host pages of guest pages that allow no access
    /// inaccessible. armor's own later mappings, placed by the host, land
    /// above the region. Call it before anything is mapped. Returns
    /// whether host protection is on: not on hosts with larger pages, nor
    /// when something of the host's is already in the region.
    pub fn enable_host_protection(&mut self) -> bool {
        let probe = 0u8;
        let stack = std::ptr::addr_of!(probe) as u64;
        let bits = 64 - stack.leading_zeros();
        let end = if bits >= 48 {
            GUEST_ADDRESS_LIMIT
        } else {
            1 << (bits - 1)
        };
        let tables = self.tables.get_mut().expect("page tables lock poisoned");
        if self.host_page_size != GUEST_PAGE_SIZE || !tables.host_pages.is_empty() {
            return false;
        }
        // With a guard above the region: an access that starts below its
        // end and runs past it faults (translated code tests only where
        // an access starts, or a base register).
        let len = end + REGION_GUARD - PLACEMENT_FLOOR;
        // SAFETY: an inaccessible reservation; MAP_FIXED_NOREPLACE fails
        // instead of replacing anything.
        let ptr = unsafe {
            libc::mmap(
                PLACEMENT_FLOOR as *mut libc::c_void,
                len as usize,
                libc::PROT_NONE,
                libc::MAP_PRIVATE
                    | libc::MAP_ANONYMOUS
                    | libc::MAP_NORESERVE
                    | libc::MAP_FIXED_NOREPLACE,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return false;
        }
        if ptr as u64 != PLACEMENT_FLOOR {
            // A kernel without MAP_FIXED_NOREPLACE took a hint.
            // SAFETY: the mapping just made.
            unsafe { libc::munmap(ptr, len as usize) };
            return false;
        }
        self.region_end = end;
        let ceiling = self.ceiling.get_mut();
        if *ceiling == 0 || *ceiling > end {
            *ceiling = end;
        }
        true
    }

    /// The end of the guest's region when host protection is on
    /// (ADR-0046): translated code may then load from any address below it
    /// without a table check, the host faulting where the guest may not.
    pub fn host_protection(&self) -> Option<u64> {
        (self.region_end != 0).then_some(self.region_end)
    }

    /// The end of the addresses guest mappings may use.
    fn guest_limit(&self) -> u64 {
        if self.region_end != 0 {
            self.region_end
        } else {
            PAGE_TABLE_ENTRIES * GUEST_PAGE_SIZE
        }
    }

    /// Before mapping host pages that nothing of this address space owns at
    /// `[addr, addr + len)`: with host protection, the reservation there
    /// goes, so that the host can map them (the callers map with
    /// MAP_FIXED_NOREPLACE or attach, as without a reservation).
    fn unreserve(&self, addr: u64, len: u64) {
        if self.region_end != 0 && addr + len <= self.region_end {
            // SAFETY: inside the guest's region, host pages this address
            // space does not own: only the reservation is there.
            unsafe { libc::munmap(addr as *mut libc::c_void, len as usize) };
        }
    }

    /// Puts the reservation back over `[addr, addr + len)` after a failed
    /// mapping (`unreserve`), replacing nothing.
    fn reserve(&self, addr: u64, len: u64) {
        if self.region_end != 0 && addr + len <= self.region_end {
            // SAFETY: an inaccessible mapping that replaces nothing.
            unsafe {
                libc::mmap(
                    addr as *mut libc::c_void,
                    len as usize,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE
                        | libc::MAP_ANONYMOUS
                        | libc::MAP_NORESERVE
                        | libc::MAP_FIXED_NOREPLACE,
                    -1,
                    0,
                )
            };
        }
    }

    /// Returns host pages of this address space to the host: inside the
    /// guest's region they become part of the reservation again (so that
    /// nothing of the host's lands there), elsewhere they are unmapped.
    fn release(&self, addr: u64, len: u64) {
        if self.region_end != 0 && addr + len <= self.region_end {
            // SAFETY: host pages this address space owned, which no guest
            // page uses any more: replacing them clobbers nothing else.
            unsafe {
                libc::mmap(
                    addr as *mut libc::c_void,
                    len as usize,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | libc::MAP_FIXED,
                    -1,
                    0,
                )
            };
        } else {
            // SAFETY: as above, outside the region.
            unsafe { libc::munmap(addr as *mut libc::c_void, len as usize) };
        }
    }

    /// With host protection, sets the host protection of the guest pages
    /// in `[start, end)` (mapped, 4K: one host page each) from their guest
    /// permissions: inaccessible for no access, read-only for a shared
    /// page the host maps read-only, read-write otherwise.
    fn apply_protection(&self, tables: &Tables, start: u64, end: u64) {
        if self.region_end == 0 {
            return;
        }
        let host_prot = |page: u64| host_prot_of(tables, page);
        let (first, last) = (start / GUEST_PAGE_SIZE, end.div_ceil(GUEST_PAGE_SIZE));
        let mut page = first;
        while page < last {
            let prot = host_prot(page);
            let mut next = page + 1;
            while next < last && host_prot(next) == prot {
                next += 1;
            }
            // SAFETY: host pages of this address space (4K: one per guest
            // page); only their protection changes.
            unsafe {
                libc::mprotect(
                    (page * GUEST_PAGE_SIZE) as *mut libc::c_void,
                    ((next - page) * GUEST_PAGE_SIZE) as usize,
                    prot,
                )
            };
            page = next;
        }
    }

    /// Runs `f` with the guest pages of `[addr, addr + len)` accessible on
    /// the host, for reading or also (`write`) for writing: privileged
    /// accesses ignore the guest's permissions, the interpreter writes
    /// executable pages (self-modifying code), and with host protection a
    /// page may be inaccessible or read-only on the host (`host_prot_of`).
    /// One thread at a time changes protections here (`access_lock`).
    fn with_access<T>(
        &self,
        tables: &Tables,
        addr: u64,
        len: usize,
        write: bool,
        f: impl FnOnce() -> T,
    ) -> T {
        let end = addr + len as u64;
        let needed = if write {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        let hidden = self.region_end != 0
            && (addr / GUEST_PAGE_SIZE..end.div_ceil(GUEST_PAGE_SIZE))
                .any(|page| host_prot_of(tables, page) & needed != needed);
        if !hidden {
            return f();
        }
        let _one_at_a_time = self.access_lock.lock().expect("access lock poisoned");
        let start = addr - addr % GUEST_PAGE_SIZE;
        let stop = round_up(end, GUEST_PAGE_SIZE).unwrap_or(end);
        // SAFETY: mapped host pages of this address space; their
        // protection is put back right after.
        unsafe {
            libc::mprotect(
                start as *mut libc::c_void,
                (stop - start) as usize,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        };
        let result = f();
        self.apply_protection(tables, start, stop);
        result
    }

    /// The guest memory with the page tables locked for reading, for a run
    /// of many accesses (a block of interpreted instructions): one lock
    /// instead of one per access. Mapping changes wait until it is dropped.
    /// Stops every other use of the page tables until the guard drops:
    /// the threads running guest code finish their block of instructions
    /// (each holds the tables for one, `view`) and wait. fork(2) of a guest
    /// with several threads takes it, so that the child's copy of armor is
    /// consistent (`armor_linux::process`).
    pub fn exclusive(&self) -> Exclusive<'_> {
        Exclusive(self.tables_mut())
    }

    pub fn view(&self) -> MemView<'_> {
        // A guest thread takes a view for every block of instructions: while
        // a thread waits to change the page tables (mprotect, mmap, a fork),
        // it waits for that first, briefly, so that a thread running guest
        // code in a loop cannot keep the tables from it.
        for _ in 0..10_000 {
            if self.writers_waiting.load(Ordering::Acquire) == 0 {
                break;
            }
            std::thread::yield_now();
        }
        MemView {
            space: self,
            tables: self.tables(),
        }
    }

    /// Concurrent mode (ADR-0023): from now on other code may write the
    /// guest's memory in parallel. The first call starts a new code
    /// generation, so that code translated without ordering or atomics is
    /// translated again; call it before the other code starts.
    pub fn enter_concurrent_mode(&self) {
        if !self.concurrent.swap(true, Ordering::AcqRel) {
            self.code_changed();
        }
    }

    /// Whether concurrent mode is on (ADR-0023).
    pub fn is_concurrent(&self) -> bool {
        self.concurrent.load(Ordering::Acquire)
    }

    fn code_changed(&self) {
        self.code_generation.fetch_add(1, Ordering::Release);
    }

    fn tables(&self) -> RwLockReadGuard<'_, Tables> {
        // A poisoned lock means another thread panicked: armor is ending.
        self.tables.read().expect("page tables lock poisoned")
    }

    fn tables_mut(&self) -> RwLockWriteGuard<'_, Tables> {
        self.writers_waiting.fetch_add(1, Ordering::AcqRel);
        let tables = self.tables.write().expect("page tables lock poisoned");
        self.writers_waiting.fetch_sub(1, Ordering::AcqRel);
        tables
    }

    pub fn host_page_size(&self) -> u64 {
        self.host_page_size
    }

    /// Limits the guest's address space to `bytes` (0: no limit): a
    /// mapping that would take more host pages fails with ENOMEM, as with
    /// `RLIMIT_AS`.
    pub fn set_limit(&self, bytes: u64) {
        self.limit.store(bytes, Ordering::Relaxed);
    }

    /// Keeps the mappings whose place armor chooses (`map_anywhere`, a
    /// shared mapping without a fixed address) below `addr`, for a guest
    /// that can only address 4 GiB (ADR-0029). They are placed top-down
    /// from there, the first free range, as the kernel lays out a 32-bit
    /// process. A new address space already has 2^47 less a page as its
    /// ceiling when the host would place mappings above it
    /// (`host_places_above`).
    pub fn set_ceiling(&self, addr: u64) {
        self.ceiling.store(addr, Ordering::Relaxed);
    }

    /// Places a host mapping of `len` bytes (whole host pages) whose
    /// address armor chooses: `map(hint, extra_flags)` makes it, at a null
    /// hint without a ceiling, otherwise at fixed addresses tried top-down
    /// below the ceiling with MAP_FIXED_NOREPLACE. Returns the address.
    fn place(
        &self,
        tables: &Tables,
        len: u64,
        map: impl Fn(u64, i32) -> io::Result<u64>,
    ) -> Result<u64, MapError> {
        let ceiling = self.ceiling.load(Ordering::Relaxed);
        if ceiling == 0 {
            return map(0, 0).map_err(MapError::Host);
        }
        let host = self.host_page_size;
        let mut end = ceiling - ceiling % host;
        while end >= PLACEMENT_FLOOR.saturating_add(len) {
            let start = end - len;
            // The highest host page of this address space in the way: the
            // next try ends below it.
            if let Some(&owned) = tables
                .host_pages
                .range(start / host..end / host)
                .next_back()
            {
                end = owned * host;
                continue;
            }
            self.unreserve(start, len);
            match map(start, libc::MAP_FIXED_NOREPLACE) {
                Ok(got) if got == start => return Ok(start),
                Ok(got) => {
                    // A kernel without MAP_FIXED_NOREPLACE took a hint.
                    // SAFETY: the mapping just made, unused.
                    unsafe { libc::munmap(got as *mut libc::c_void, len as usize) };
                    self.reserve(start, len);
                    end -= host;
                }
                // Something of the host's (not of this address space).
                Err(e) if e.raw_os_error() == Some(libc::EEXIST) => {
                    self.reserve(start, len);
                    end -= host;
                }
                Err(e) => {
                    self.reserve(start, len);
                    return Err(MapError::Host(e));
                }
            }
        }
        Err(MapError::Host(io::Error::from_raw_os_error(libc::ENOMEM)))
    }

    /// ENOMEM if `count` more host pages would exceed the limit.
    fn admit(&self, tables: &Tables, count: u64) -> Result<(), MapError> {
        let limit = self.limit.load(Ordering::Relaxed);
        let pages = tables.host_pages.len() as u64 + count;
        if limit != 0 && pages.saturating_mul(self.host_page_size) > limit {
            return Err(MapError::Host(io::Error::from_raw_os_error(libc::ENOMEM)));
        }
        Ok(())
    }

    /// Maps `[addr, addr + len)` (rounded up to whole guest pages) with guest
    /// permissions `prot`, like `mmap(MAP_FIXED)`: pages already mapped by the
    /// guest are replaced, and the whole range reads as zero afterwards.
    pub fn map_fixed(&self, addr: u64, len: u64, prot: Prot) -> Result<(), MapError> {
        if !addr.is_multiple_of(GUEST_PAGE_SIZE) {
            return Err(MapError::Unaligned { addr });
        }
        let end =
            round_up(addr.saturating_add(len), GUEST_PAGE_SIZE).ok_or(MapError::OutOfRange)?;
        if end > self.guest_limit() {
            return Err(MapError::OutOfRange);
        }
        let host_start = addr / self.host_page_size;
        let host_end = end.div_ceil(self.host_page_size);

        let mut tables = self.tables_mut();
        // A private mapping over shared host pages replaces them, if no
        // other guest page uses them (a host page is shared or private as
        // a whole).
        let shared: Vec<u64> = tables
            .shared
            .range(host_start..host_end)
            .map(|(&page, _)| page)
            .collect();
        for &host_page in &shared {
            let (from, to) = (
                host_page * self.host_page_size,
                (host_page + 1) * self.host_page_size,
            );
            if any_mapped_in(&tables, self.host_page_size, from, addr.max(from))
                || any_mapped_in(&tables, self.host_page_size, end.min(to), to)
            {
                return Err(MapError::SharedPlacement(
                    "a private mapping would share a host page with a shared one",
                ));
            }
        }
        for host_page in shared {
            self.make_private(&mut tables, host_page)?;
        }
        // Work proportional to the host pages already there, not to the
        // range: a guest may ask for terabytes (the host then refuses).
        let existing: Vec<u64> = tables
            .host_pages
            .range(host_start..host_end)
            .copied()
            .collect();
        let mut next = host_start;
        for &host_page in existing.iter().chain([host_end].iter()) {
            if host_page > next {
                self.host_map_fixed(&mut tables, next, host_page - next, prot == Prot::NONE)?;
            }
            next = host_page + 1;
        }

        if set_pages(&mut tables, addr, end, prot) {
            self.code_changed();
        }
        // Freshly mapped host memory is already zero (and stays untouched,
        // so a large sparse mapping costs nothing); host pages shared with
        // earlier guest mappings may not be.
        for host_page in existing {
            let from = (host_page * self.host_page_size).max(addr);
            let to = ((host_page + 1) * self.host_page_size).min(end);
            if self.region_end != 0 {
                // With host protection the page may be inaccessible; it gets
                // its new protection below (`apply_protection`).
                // SAFETY: a host page owned by this address space.
                unsafe {
                    libc::mprotect(
                        (host_page * self.host_page_size) as *mut libc::c_void,
                        self.host_page_size as usize,
                        libc::PROT_READ | libc::PROT_WRITE,
                    )
                };
            }
            // SAFETY: [from, to) lies in a host page owned by this address
            // space, mapped read-write (invariant of `host_pages`; with host
            // protection, made so above).
            unsafe { std::ptr::write_bytes(from as *mut u8, 0, (to - from) as usize) };
        }
        self.apply_protection(&tables, addr, end);
        Ok(())
    }

    /// Maps `len` bytes (rounded up to whole guest pages) at an address
    /// chosen by the host (below the ceiling, if set: `set_ceiling`),
    /// aligned to the host page size. Returns the guest
    /// address of the mapping. The rest of the last host page stays
    /// unmapped for the guest.
    pub fn map_anywhere(&self, len: u64, prot: Prot) -> Result<u64, MapError> {
        let guest_len = round_up(len.max(1), GUEST_PAGE_SIZE).ok_or(MapError::OutOfRange)?;
        let len = round_up(guest_len, self.host_page_size).ok_or(MapError::OutOfRange)?;
        let mut tables = self.tables_mut();
        self.admit(&tables, len / self.host_page_size)?;
        let addr = self.place(&tables, len, |hint, extra| {
            // SAFETY: anonymous mapping at an address chosen by the kernel,
            // or at a fixed one with MAP_FIXED_NOREPLACE: it cannot
            // overlap memory in use.
            let ptr = unsafe {
                libc::mmap(
                    hint as *mut libc::c_void,
                    len as usize,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE
                        | libc::MAP_ANONYMOUS
                        | extra
                        | reserve_flag(prot == Prot::NONE),
                    -1,
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                Err(io::Error::last_os_error())
            } else {
                Ok(ptr as u64)
            }
        })?;
        let first = addr / self.host_page_size;
        tables
            .host_pages
            .extend(first..first + len / self.host_page_size);
        if set_pages(&mut tables, addr, addr + guest_len, prot) {
            self.code_changed();
        }
        self.apply_protection(&tables, addr, addr + guest_len);
        Ok(addr)
    }

    /// Unmaps the guest pages in `[addr, addr + len)`, like `munmap`: pages
    /// that are not mapped are ignored. Host pages left with no guest page
    /// are returned to the host.
    pub fn unmap(&self, addr: u64, len: u64) -> Result<(), MapError> {
        if !addr.is_multiple_of(GUEST_PAGE_SIZE) {
            return Err(MapError::Unaligned { addr });
        }
        let end =
            round_up(addr.saturating_add(len), GUEST_PAGE_SIZE).ok_or(MapError::OutOfRange)?;
        let mut tables = self.tables_mut();
        self.unmap_locked(&mut tables, addr, end);
        Ok(())
    }

    /// `unmap` of the guest pages in `[addr, end)` (4K-aligned), with the
    /// tables already locked.
    fn unmap_locked(&self, tables: &mut Tables, addr: u64, end: u64) {
        let mut executable = false;
        // Guest pages only exist inside this address space's host pages:
        // visit those, not every page of a range the guest chose.
        let host_range = addr / self.host_page_size..end.div_ceil(self.host_page_size);
        let existing: Vec<u64> = tables.host_pages.range(host_range).copied().collect();
        let per_host = self.host_page_size / GUEST_PAGE_SIZE;
        for &host_page in &existing {
            let first = (host_page * per_host).max(addr / GUEST_PAGE_SIZE);
            let last = ((host_page + 1) * per_host).min(end / GUEST_PAGE_SIZE);
            for page in first..last {
                if let Some(old) = tables.pages.remove(page) {
                    executable |= old.contains(Prot::EXEC);
                }
            }
        }
        if executable {
            self.code_changed();
        }
        // Host pages left empty, unmapped in runs: one munmap for a whole
        // thread stack rather than one per page.
        let mut run: Option<(u64, u64)> = None;
        let flush = |run: (u64, u64)| {
            // The host pages were mapped by this address space and no
            // guest page refers to them any more.
            self.release(
                run.0 * self.host_page_size,
                (run.1 - run.0) * self.host_page_size,
            );
        };
        for host_page in existing {
            let first = host_page * per_host;
            let in_use = (first..first + per_host).any(|page| tables.pages.contains_key(page));
            if !in_use && tables.host_pages.remove(&host_page) {
                tables.shared.remove(&host_page);
                run = match run {
                    Some((start, end)) if end == host_page => Some((start, end + 1)),
                    other => {
                        other.map(flush);
                        Some((host_page, host_page + 1))
                    }
                };
            }
        }
        run.map(flush);
    }

    /// Turns a shared host page, none of whose guest pages stays in use,
    /// into a fresh private one: replaced in place with MAP_FIXED, never
    /// unmapped first, so that no other mmap of armor's can take the
    /// address in between.
    fn make_private(&self, tables: &mut Tables, host_page: u64) -> Result<(), MapError> {
        let per_host = self.host_page_size / GUEST_PAGE_SIZE;
        for page in host_page * per_host..(host_page + 1) * per_host {
            tables.pages.remove(page);
        }
        let addr = host_page * self.host_page_size;
        // SAFETY: the host page belongs to this address space and no guest
        // page uses it any more: replacing it clobbers nothing else.
        let ptr = unsafe {
            libc::mmap(
                addr as *mut libc::c_void,
                self.host_page_size as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(MapError::Host(io::Error::last_os_error()));
        }
        tables.shared.remove(&host_page);
        Ok(())
    }

    /// Maps `len` bytes of shared memory (ADR-0022) with guest permissions
    /// `prot`: anonymous, or a file. The host maps whole host pages shared,
    /// writable only if `writable` (the file is open for writing).
    ///
    /// Without `fixed`, the host picks the place and the guest address has
    /// the file offset's remainder modulo the host page size. With `fixed`,
    /// that remainder must already match, and the parts of the host pages
    /// outside the range must hold no guest page; guest pages inside it are
    /// replaced, as by `mmap(MAP_FIXED)`. Returns the guest address.
    pub fn map_shared(
        &self,
        fixed: Option<u64>,
        len: u64,
        prot: Prot,
        source: SharedSource,
        writable: bool,
    ) -> Result<u64, MapError> {
        let host = self.host_page_size;
        let guest_len = round_up(len.max(1), GUEST_PAGE_SIZE).ok_or(MapError::OutOfRange)?;
        let file_delta = match source {
            SharedSource::File { offset, .. } => offset % host,
            SharedSource::Anonymous => 0,
        };
        let delta = match fixed {
            Some(addr) => {
                if !addr.is_multiple_of(GUEST_PAGE_SIZE) {
                    return Err(MapError::Unaligned { addr });
                }
                if matches!(source, SharedSource::File { .. }) && addr % host != file_delta {
                    return Err(MapError::SharedPlacement(
                        "the address and the file offset differ modulo the host page size",
                    ));
                }
                addr % host
            }
            None => file_delta,
        };
        let host_len = round_up(delta + guest_len, host).ok_or(MapError::OutOfRange)?;
        let (fd, host_offset, flags) = match source {
            SharedSource::Anonymous => (-1, 0, libc::MAP_SHARED | libc::MAP_ANONYMOUS),
            SharedSource::File { fd, offset } => (fd, offset - delta, libc::MAP_SHARED),
        };
        let host_prot = libc::PROT_READ | if writable { libc::PROT_WRITE } else { 0 };
        let host_map = |at: u64, len: u64, offset: u64, extra: i32| {
            // SAFETY: the callers pass either a null hint, MAP_FIXED_NOREPLACE
            // (which never replaces anything), or MAP_FIXED over host pages
            // this address space owns and no guest page uses any more.
            let ptr = unsafe {
                libc::mmap(
                    at as *mut libc::c_void,
                    len as usize,
                    host_prot,
                    flags | extra,
                    fd,
                    offset as libc::off_t,
                )
            };
            if ptr == libc::MAP_FAILED {
                Err(io::Error::last_os_error())
            } else {
                Ok(ptr as u64)
            }
        };
        let limit = self.guest_limit();
        let mut tables = self.tables_mut();
        // Counted whole, even where it replaces pages already there.
        self.admit(&tables, host_len / self.host_page_size)?;
        let start = match fixed {
            None => {
                let start = self.place(&tables, host_len, |hint, extra| {
                    host_map(hint, host_len, host_offset, extra)
                })?;
                if start + host_len > limit {
                    // The mapping just made, unused.
                    self.release(start, host_len);
                    return Err(MapError::OutOfRange);
                }
                start
            }
            Some(addr) => {
                let start = addr - delta;
                let end = start
                    .checked_add(host_len)
                    .filter(|&end| end <= limit)
                    .ok_or(MapError::OutOfRange)?;
                if any_mapped_in(&tables, host, start, addr)
                    || any_mapped_in(&tables, host, addr + guest_len, end)
                {
                    return Err(MapError::SharedPlacement(
                        "other guest pages use the same host pages",
                    ));
                }
                // Host pages nobody owns first, without replacing anything
                // (armor's own memory may be there); then the ones this
                // address space owns, replaced. A failure in the first step
                // leaves the guest's memory as it was.
                let owned: Vec<u64> = tables
                    .host_pages
                    .range(start / host..end / host)
                    .copied()
                    .collect();
                let runs = host_runs(start / host, end / host, &owned);
                let mut made = Vec::new();
                for &(first, count, is_owned) in &runs {
                    if is_owned {
                        continue;
                    }
                    let at = first * host;
                    self.unreserve(at, count * host);
                    match host_map(
                        at,
                        count * host,
                        host_offset + (at - start),
                        libc::MAP_FIXED_NOREPLACE,
                    ) {
                        Ok(got) if got == at => made.push((at, count * host)),
                        result => {
                            match result {
                                Ok(got) => {
                                    // SAFETY: a hint taken elsewhere, unused.
                                    unsafe {
                                        libc::munmap(
                                            got as *mut libc::c_void,
                                            (count * host) as usize,
                                        )
                                    };
                                    self.reserve(at, count * host);
                                }
                                Err(_) => self.reserve(at, count * host),
                            }
                            for (at, len) in made {
                                // Mappings made just above, unused.
                                self.release(at, len);
                            }
                            return Err(MapError::AddressInUse {
                                addr: at,
                                len: count * host,
                            });
                        }
                    }
                }
                // The guest pages being replaced, all inside the range.
                let mut executable = false;
                for page in addr / GUEST_PAGE_SIZE..(addr + guest_len) / GUEST_PAGE_SIZE {
                    if let Some(old) = tables.pages.remove(page) {
                        executable |= old.contains(Prot::EXEC);
                    }
                }
                if executable {
                    self.code_changed();
                }
                for &(first, count, is_owned) in &runs {
                    if !is_owned {
                        continue;
                    }
                    let at = first * host;
                    if let Err(e) = host_map(
                        at,
                        count * host,
                        host_offset + (at - start),
                        libc::MAP_FIXED,
                    ) {
                        // The old contents are gone, as after a failed
                        // MAP_FIXED on Linux; the range is left unmapped.
                        self.unmap_locked(&mut tables, start, end);
                        return Err(MapError::Host(e));
                    }
                }
                start
            }
        };
        let first = start / host;
        for page in first..first + host_len / host {
            tables.host_pages.insert(page);
            tables.shared.insert(page, writable);
        }
        let addr = start + delta;
        if set_pages(&mut tables, addr, addr + guest_len, prot) {
            self.code_changed();
        }
        self.apply_protection(&tables, addr, addr + guest_len);
        if writable {
            self.enter_concurrent_mode();
        }
        Ok(addr)
    }

    /// Grows the shared mapping `[old, old + old_len)` (ADR-0022) to
    /// `new_len` bytes, as mremap(2) does: the host remaps its own mapping,
    /// which stays the same memory (the same file pages, or the same
    /// shared anonymous memory). The mapping's host pages are its own
    /// (ADR-0022), so on a host with pages larger than the guest's it may
    /// start inside its first host page and end inside its last one:
    /// growing within those host pages only gives the guest more of them;
    /// beyond, the host extends its mapping in place when the host pages
    /// after it are free, else (with `may_move`) moves it to a place
    /// chosen like any other mapping (below the ceiling), with the same
    /// offset inside the first host page. The range must hold only shared
    /// guest pages with one protection, and the guest pages it grows into
    /// inside its last host page must be unmapped. Returns the new address.
    pub fn remap_shared(
        &self,
        old: u64,
        old_len: u64,
        new_len: u64,
        may_move: bool,
    ) -> Result<u64, MapError> {
        let host = self.host_page_size;
        if !old.is_multiple_of(GUEST_PAGE_SIZE)
            || !old_len.is_multiple_of(GUEST_PAGE_SIZE)
            || !new_len.is_multiple_of(GUEST_PAGE_SIZE)
            || old_len == 0
            || new_len <= old_len
        {
            return Err(MapError::SharedPlacement(
                "growing a shared mapping needs guest-page boundaries",
            ));
        }
        // The mapping's host pages: from the one holding `old`, `delta`
        // bytes before it.
        let base = old - old % host;
        let delta = old - base;
        let old_host_len = round_up(delta + old_len, host).ok_or(MapError::OutOfRange)?;
        let new_host_len = round_up(delta + new_len, host).ok_or(MapError::OutOfRange)?;
        let mut tables = self.tables_mut();
        let first_page = old / GUEST_PAGE_SIZE;
        let Some(prot) = tables.pages.get(first_page) else {
            return Err(MapError::SharedPlacement("not mapped"));
        };
        let old_pages = first_page..(old + old_len) / GUEST_PAGE_SIZE;
        if old_pages
            .clone()
            .any(|page| tables.pages.get(page) != Some(prot))
        {
            return Err(MapError::SharedPlacement(
                "pages with different permissions",
            ));
        }
        let host_range = base / host..(base + old_host_len) / host;
        let Some(&writable) = tables.shared.get(&host_range.start) else {
            return Err(MapError::SharedPlacement("not a shared mapping"));
        };
        if host_range
            .clone()
            .any(|page| tables.shared.get(&page) != Some(&writable))
        {
            return Err(MapError::SharedPlacement("mixed shared pages"));
        }
        // The rest of the last host page belongs to this mapping only
        // while the guest has nothing there.
        let tail = old_pages.end..(base + old_host_len) / GUEST_PAGE_SIZE;
        if tail.clone().any(|page| tables.pages.get(page).is_some()) {
            return Err(MapError::SharedPlacement(
                "other guest pages in the mapping's last host page",
            ));
        }
        self.admit(&tables, (new_host_len - old_host_len) / host)?;
        let limit = self.guest_limit();
        let grow_end = base + new_host_len;
        // In place: within the host pages the mapping has, or the host
        // extends it only into free pages.
        let in_place = new_host_len == old_host_len
            || (grow_end <= limit
                && !tables
                    .host_pages
                    .range(host_range.end..grow_end / host)
                    .any(|_| true)
                && {
                    let grown = base + old_host_len;
                    self.unreserve(grown, new_host_len - old_host_len);
                    // SAFETY: base is this address space's shared mapping;
                    // without MREMAP_MAYMOVE the host only extends it into
                    // free pages.
                    let got = unsafe {
                        libc::mremap(
                            base as *mut libc::c_void,
                            old_host_len as usize,
                            new_host_len as usize,
                            0,
                        )
                    };
                    if got as u64 != base {
                        self.reserve(grown, new_host_len - old_host_len);
                    }
                    got as u64 == base
                });
        let new_base = if in_place {
            base
        } else {
            if !may_move {
                return Err(MapError::Host(io::Error::from_raw_os_error(libc::ENOMEM)));
            }
            // A placeholder where armor would map, then the host's mremap
            // moves the mapping over it (MREMAP_FIXED replaces it).
            let target = self.place(&tables, new_host_len, |hint, extra| {
                // SAFETY: an anonymous inaccessible mapping as a placeholder.
                let ptr = unsafe {
                    libc::mmap(
                        hint as *mut libc::c_void,
                        new_host_len as usize,
                        libc::PROT_NONE,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | extra,
                        -1,
                        0,
                    )
                };
                if ptr == libc::MAP_FAILED {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(ptr as u64)
                }
            })?;
            // SAFETY: base is this address space's shared mapping and
            // target the placeholder just made, which MREMAP_FIXED replaces.
            let got = unsafe {
                libc::mremap(
                    base as *mut libc::c_void,
                    old_host_len as usize,
                    new_host_len as usize,
                    libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED,
                    target as *mut libc::c_void,
                )
            };
            if got == libc::MAP_FAILED {
                let error = io::Error::last_os_error();
                // The placeholder, unused.
                self.release(target, new_host_len);
                return Err(MapError::Host(error));
            }
            // The old place is empty now: back into the reservation.
            self.reserve(base, old_host_len);
            target
        };
        let new = new_base + delta;
        if new_base != base {
            for page in old_pages {
                tables.pages.remove(page);
            }
            for page in host_range {
                tables.host_pages.remove(&page);
                tables.shared.remove(&page);
            }
        }
        for page in new_base / host..(new_base + new_host_len) / host {
            tables.host_pages.insert(page);
            tables.shared.insert(page, writable);
        }
        set_pages(&mut tables, new, new + new_len, prot);
        self.apply_protection(&tables, new, new + new_len);
        // Moved code, or code that grew: translations are stale either way.
        self.code_changed();
        Ok(new)
    }

    /// Attaches the host's System V shared memory segment `id` of `len`
    /// bytes, as shmat(2) does (ADR-0022 for the shared pages): the host
    /// attaches it whole, so the segment stays the same memory for the
    /// other processes (an X server reading MIT-SHM images, say). Without
    /// `fixed` armor picks the place; with it, the address must be on a
    /// host page boundary and the guest pages already there are replaced.
    /// `read_only` is SHM_RDONLY, `exec` SHM_EXEC. Detaching is `unmap`
    /// (the host's munmap of an attachment detaches it).
    pub fn attach_sysv(
        &self,
        fixed: Option<u64>,
        len: u64,
        id: i32,
        read_only: bool,
        exec: bool,
    ) -> Result<u64, MapError> {
        const SHM_RDONLY: i32 = 0o10000;
        const SHM_EXEC: i32 = 0o100000;
        let host = self.host_page_size;
        let host_len = round_up(len.max(1), host).ok_or(MapError::OutOfRange)?;
        let flags = if read_only { SHM_RDONLY } else { 0 } | if exec { SHM_EXEC } else { 0 };
        let attach = |at: u64| {
            // SAFETY: a null address lets the host choose; a given one is
            // attached without SHM_REMAP, so it never replaces a mapping.
            let ptr = unsafe { libc::shmat(id, at as *const libc::c_void, flags) };
            if ptr as isize == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(ptr as u64)
            }
        };
        let limit = self.guest_limit();
        let mut tables = self.tables_mut();
        self.admit(&tables, host_len / host)?;
        let start = match fixed {
            None => {
                let start = self.place(&tables, host_len, |hint, extra| {
                    if extra == 0 {
                        attach(0)
                    } else {
                        // Taken: EINVAL from shmat, EEXIST for `place`.
                        attach(hint).map_err(|e| {
                            if e.raw_os_error() == Some(libc::EINVAL) {
                                io::Error::from_raw_os_error(libc::EEXIST)
                            } else {
                                e
                            }
                        })
                    }
                })?;
                if start + host_len > limit {
                    // SAFETY: the attachment just made, unused.
                    unsafe { libc::shmdt(start as *const libc::c_void) };
                    return Err(MapError::OutOfRange);
                }
                start
            }
            Some(addr) => {
                if !addr.is_multiple_of(host) {
                    return Err(MapError::SharedPlacement(
                        "a System V segment starts on a host page boundary",
                    ));
                }
                let end = addr
                    .checked_add(host_len)
                    .filter(|&end| end <= limit)
                    .ok_or(MapError::OutOfRange)?;
                // The guest pages there go, as with shmat's address over
                // a mapping of the process (Linux needs SHM_REMAP for that;
                // the caller checks it).
                self.unmap_locked(&mut tables, addr, end);
                self.unreserve(addr, host_len);
                match attach(addr) {
                    Ok(got) if got == addr => addr,
                    Ok(got) => {
                        // SAFETY: the attachment just made, unused.
                        unsafe { libc::shmdt(got as *const libc::c_void) };
                        return Err(MapError::AddressInUse {
                            addr,
                            len: host_len,
                        });
                    }
                    Err(e) => {
                        self.reserve(addr, host_len);
                        return Err(MapError::Host(e));
                    }
                }
            }
        };
        for page in start / host..(start + host_len) / host {
            tables.host_pages.insert(page);
            tables.shared.insert(page, !read_only);
        }
        let mut prot = Prot::READ;
        if !read_only {
            prot = prot | Prot::WRITE;
        }
        if exec {
            prot = prot | Prot::EXEC;
        }
        let guest_end =
            start + round_up(len.max(1), GUEST_PAGE_SIZE).ok_or(MapError::OutOfRange)?;
        if set_pages(&mut tables, start, guest_end, prot) {
            self.code_changed();
        }
        self.apply_protection(&tables, start, guest_end);
        if !read_only {
            self.enter_concurrent_mode();
        }
        Ok(start)
    }

    /// Whether any guest page of `[addr, addr + len)` belongs to a shared
    /// mapping.
    pub fn any_shared(&self, addr: u64, len: u64) -> bool {
        let end = addr.saturating_add(len);
        let tables = self.tables();
        let host_range = addr / self.host_page_size..end.div_ceil(self.host_page_size);
        let per_host = self.host_page_size / GUEST_PAGE_SIZE;
        tables.shared.range(host_range).any(|(&host_page, _)| {
            let first = (host_page * per_host).max(addr / GUEST_PAGE_SIZE);
            let last = ((host_page + 1) * per_host).min(end.div_ceil(GUEST_PAGE_SIZE));
            (first..last).any(|page| tables.pages.contains_key(page))
        })
    }

    /// `msync`: every guest page of `[addr, addr + len)` must be mapped
    /// (else ENOMEM); the host's msync runs on the host pages around them.
    /// Returns an errno on failure.
    pub fn sync(&self, addr: u64, len: u64, flags: i32) -> Result<(), i32> {
        let end = addr
            .checked_add(len)
            .and_then(|end| round_up(end, GUEST_PAGE_SIZE))
            .ok_or(libc::ENOMEM)?;
        let tables = self.tables();
        let host = self.host_page_size;
        let host_range = addr / host..end.div_ceil(host);
        let spanned = host_range.end - host_range.start;
        if (tables.host_pages.range(host_range.clone()).count() as u64) < spanned
            || (addr / GUEST_PAGE_SIZE..end / GUEST_PAGE_SIZE)
                .any(|page| !tables.pages.contains_key(page))
        {
            return Err(libc::ENOMEM);
        }
        if spanned == 0 {
            return Ok(());
        }
        // SAFETY: host pages owned by this address space (checked above),
        // kept mapped by the read lock.
        let result = unsafe {
            libc::msync(
                (host_range.start * host) as *mut libc::c_void,
                (spanned * host) as usize,
                flags,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO))
        }
    }

    /// `madvise(MADV_DONTNEED)`: the mapped guest pages of `[addr, addr +
    /// len)` read as zero afterwards. Whole host pages are given back to
    /// the host kernel (which zero-fills them on the next touch); the rest
    /// is cleared. Like Linux, unmapped pages in the range are an error
    /// (`Err` with the first one) reported after the mapped ones were
    /// discarded.
    pub fn discard(&self, addr: u64, len: u64) -> Result<(), MemFault> {
        let fault = |at| MemFault {
            addr: at,
            access: Access::Write,
        };
        let end = addr
            .checked_add(len)
            .and_then(|e| round_up(e, GUEST_PAGE_SIZE))
            .ok_or(fault(addr))?;
        let tables = self.tables();
        let mut executable = false;
        let mut unmapped = None;
        let mut run: Option<u64> = None;
        // Only the guest pages inside this address space's host pages can
        // be mapped: visit those (and one page past the end, to close the
        // last run). A page skipped between them is unmapped.
        let per_host = self.host_page_size / GUEST_PAGE_SIZE;
        let (first_page, end_page) = (addr / GUEST_PAGE_SIZE, end / GUEST_PAGE_SIZE);
        let host_range = addr / self.host_page_size..end.div_ceil(self.host_page_size);
        let candidates = tables
            .host_pages
            .range(host_range)
            .flat_map(|&host_page| host_page * per_host..(host_page + 1) * per_host)
            .filter(|&page| (first_page..end_page).contains(&page))
            .chain([end_page]);
        let mut expected = first_page;
        for page in candidates {
            if page > expected {
                // The pages in between are not in any host page.
                if expected < end_page && unmapped.is_none() {
                    unmapped = Some(expected * GUEST_PAGE_SIZE);
                }
                if let Some(start) = run.take() {
                    self.discard_mapped(start, expected * GUEST_PAGE_SIZE);
                }
            }
            expected = page + 1;
            let prot = (page < end_page).then(|| tables.pages.get(page)).flatten();
            // Shared pages keep their contents: on Linux they read back the
            // file or the shared memory, which is what the guest sees.
            if prot.is_some() && tables.shared.contains_key(&(page / per_host)) {
                if let Some(start) = run.take() {
                    self.discard_mapped(start, page * GUEST_PAGE_SIZE);
                }
                continue;
            }
            match (prot, run) {
                (Some(prot), None) => {
                    executable |= prot.contains(Prot::EXEC);
                    run = Some(page * GUEST_PAGE_SIZE);
                }
                (Some(prot), Some(_)) => executable |= prot.contains(Prot::EXEC),
                (None, Some(start)) => {
                    self.discard_mapped(start, page * GUEST_PAGE_SIZE);
                    run = None;
                }
                (None, None) => {}
            }
            if prot.is_none() && page < end_page && unmapped.is_none() {
                unmapped = Some(page * GUEST_PAGE_SIZE);
            }
        }
        if executable {
            self.code_changed();
        }
        unmapped.map_or(Ok(()), |at| Err(fault(at)))
    }

    /// `discard` of `[start, end)`, every guest page of which is mapped;
    /// the caller holds the tables lock.
    fn discard_mapped(&self, start: u64, end: u64) {
        let host = self.host_page_size;
        let clear = |from: u64, to: u64| {
            // SAFETY: every guest page of the range is mapped (the
            // caller's contract), hence backed by a read-write host page of
            // this address space; the tables lock keeps it mapped.
            unsafe { std::ptr::write_bytes(from as *mut u8, 0, (to - from) as usize) };
        };
        let (whole_start, whole_end) = (start.next_multiple_of(host), end / host * host);
        if whole_start >= whole_end {
            clear(start, end);
            return;
        }
        clear(start, whole_start);
        clear(whole_end, end);
        // SAFETY: whole host pages of this address space, all of whose guest
        // pages are in the range: private anonymous memory, which reads as
        // zero after MADV_DONTNEED.
        let result = unsafe {
            libc::madvise(
                whole_start as *mut libc::c_void,
                (whole_end - whole_start) as usize,
                libc::MADV_DONTNEED,
            )
        };
        if result != 0 {
            clear(whole_start, whole_end);
        }
    }

    /// Whether any guest page of `[addr, addr + len)` is mapped (brk must
    /// not grow over a mapping). Visits only existing host pages.
    pub fn any_mapped(&self, addr: u64, len: u64) -> bool {
        any_mapped_in(
            &self.tables(),
            self.host_page_size,
            addr,
            addr.saturating_add(len),
        )
    }

    /// Changes the guest permissions of `[addr, addr + len)`, like
    /// `mprotect`. Fails without changing anything if a page in the range
    /// is not mapped.
    pub fn protect(&self, addr: u64, len: u64, prot: Prot) -> Result<(), ProtectError> {
        if !addr.is_multiple_of(GUEST_PAGE_SIZE) {
            return Err(ProtectError::Unaligned);
        }
        let end =
            round_up(addr.saturating_add(len), GUEST_PAGE_SIZE).ok_or(ProtectError::NotMapped)?;
        let pages = addr / GUEST_PAGE_SIZE..end / GUEST_PAGE_SIZE;
        let mut tables = self.tables_mut();
        // A fully mapped range has every host page it spans: count them
        // first, so that a huge unmapped range fails without visiting it.
        let host_range = addr / self.host_page_size..end.div_ceil(self.host_page_size);
        let spanned = host_range.end - host_range.start;
        if (tables.host_pages.range(host_range.clone()).count() as u64) < spanned
            || pages.clone().any(|page| !tables.pages.contains_key(page))
        {
            return Err(ProtectError::NotMapped);
        }
        for (_, &host_writable) in tables.shared.range(host_range) {
            if prot.contains(Prot::EXEC) {
                return Err(ProtectError::ExecutableShared);
            }
            if prot.contains(Prot::WRITE) && !host_writable {
                return Err(ProtectError::ReadOnlyShared);
            }
        }
        if set_pages(&mut tables, addr, end, prot) {
            self.code_changed();
        }
        self.apply_protection(&tables, addr, end);
        Ok(())
    }

    /// Writes `data` ignoring guest permissions, e.g. to load read-only
    /// segments or build the initial stack. Every byte must be mapped.
    pub fn write_privileged(&self, addr: u64, data: &[u8]) -> Result<(), MemFault> {
        let tables = self.tables();
        let executable = check(&tables, addr, data.len(), Access::Write, |_| true)?;
        // SAFETY: `check` verified that every byte is in a mapped guest page,
        // backed by a host page owned by this address space, read-write
        // inside `with_access`.
        self.with_access(&tables, addr, data.len(), true, || unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), addr as *mut u8, data.len())
        });
        if executable {
            self.code_changed();
        }
        Ok(())
    }

    /// Reads ignoring guest permissions (mremap copies a mapping whatever
    /// its protection). Every byte must be mapped.
    pub fn read_privileged(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemFault> {
        let tables = self.tables();
        check(&tables, addr, buf.len(), Access::Read, |_| true)?;
        let len = buf.len();
        // SAFETY: `check` verified that every byte is in a mapped guest page,
        // backed by a host page of this address space, accessible inside
        // `with_access`; the lock keeps it.
        self.with_access(&tables, addr, len, false, || unsafe {
            std::ptr::copy_nonoverlapping(addr as *const u8, buf.as_mut_ptr(), len)
        });
        Ok(())
    }

    pub fn prot(&self, addr: u64) -> Option<Prot> {
        self.tables().pages.get(addr / GUEST_PAGE_SIZE)
    }

    /// mmaps `count` host pages starting at host page number `first`.
    fn host_map_fixed(
        &self,
        tables: &mut Tables,
        first: u64,
        count: u64,
        noreserve: bool,
    ) -> Result<(), MapError> {
        self.admit(tables, count)?;
        let addr = first * self.host_page_size;
        let len = count * self.host_page_size;
        self.unreserve(addr, len);
        // SAFETY: MAP_FIXED_NOREPLACE never replaces existing mappings, so
        // this cannot clobber memory in use; failure is reported as EEXIST.
        let ptr = unsafe {
            libc::mmap(
                addr as *mut libc::c_void,
                len as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE
                    | libc::MAP_ANONYMOUS
                    | libc::MAP_FIXED_NOREPLACE
                    | reserve_flag(noreserve),
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            let err = io::Error::last_os_error();
            self.reserve(addr, len);
            return Err(if err.raw_os_error() == Some(libc::EEXIST) {
                MapError::AddressInUse { addr, len }
            } else {
                MapError::Host(err)
            });
        }
        if ptr as u64 != addr {
            // A kernel without MAP_FIXED_NOREPLACE treats the address as a hint.
            // SAFETY: unmapping the region we were just given.
            unsafe { libc::munmap(ptr, len as usize) };
            self.reserve(addr, len);
            return Err(MapError::AddressInUse { addr, len });
        }
        tables.host_pages.extend(first..first + count);
        Ok(())
    }
}

/// `MAP_NORESERVE` for the host mapping of a guest reservation (`PROT_NONE`):
/// host pages are always read-write (ADR-0009), and a read-write private
/// mapping is charged to the kernel's overcommit accounting, which refuses
/// a large one (V8 reserves 32 GiB for each Chromium renderer); a
/// `PROT_NONE` mapping is not charged on a native system either.
fn reserve_flag(noreserve: bool) -> libc::c_int {
    if noreserve { libc::MAP_NORESERVE } else { 0 }
}

/// Sets the permissions of `[start, end)`. Returns whether code may have
/// changed: a page was or becomes executable (decoded instructions of
/// other pages stay valid).
/// Whether a guest page of `[addr, end)` is mapped, visiting only the
/// host pages the address space has.
fn any_mapped_in(tables: &Tables, host_page_size: u64, addr: u64, end: u64) -> bool {
    if addr >= end {
        return false;
    }
    let per_host = host_page_size / GUEST_PAGE_SIZE;
    let host_range = addr / host_page_size..end.div_ceil(host_page_size);
    tables.host_pages.range(host_range).any(|&host_page| {
        let first = (host_page * per_host).max(addr / GUEST_PAGE_SIZE);
        let last = ((host_page + 1) * per_host).min(end.div_ceil(GUEST_PAGE_SIZE));
        (first..last).any(|page| tables.pages.contains_key(page))
    })
}

/// The host pages `[first, end)` as runs (first page, count, owned), from
/// the sorted pages of the range that are `owned`: work proportional to
/// those, not to the range.
fn host_runs(first: u64, end: u64, owned: &[u64]) -> Vec<(u64, u64, bool)> {
    let mut runs: Vec<(u64, u64, bool)> = Vec::new();
    let mut next = first;
    for &page in owned {
        if page > next {
            runs.push((next, page - next, false));
        }
        match runs.last_mut() {
            Some((start, count, true)) if *start + *count == page => *count += 1,
            _ => runs.push((page, 1, true)),
        }
        next = page + 1;
    }
    if end > next {
        runs.push((next, end - next, false));
    }
    runs
}

/// With host protection (ADR-0046), the host protection of a guest page:
/// inaccessible for no access; writable only for a writable page that is
/// not executable (writes to code must reach the interpreter, which
/// invalidates translations) nor a shared page the host maps read-only;
/// otherwise read-only.
fn host_prot_of(tables: &Tables, page: u64) -> libc::c_int {
    match tables.pages.get(page) {
        Some(Prot::NONE) | None => libc::PROT_NONE,
        Some(prot)
            if prot.contains(Prot::WRITE)
                && !prot.contains(Prot::EXEC)
                && tables.shared.get(&page) != Some(&false) =>
        {
            libc::PROT_READ | libc::PROT_WRITE
        }
        Some(_) => libc::PROT_READ,
    }
}

fn set_pages(tables: &mut Tables, start: u64, end: u64, prot: Prot) -> bool {
    let mut executable = prot.contains(Prot::EXEC);
    for page in start / GUEST_PAGE_SIZE..end / GUEST_PAGE_SIZE {
        if let Some(old) = tables.pages.insert(page, prot) {
            executable |= old.contains(Prot::EXEC);
        }
    }
    executable
}

/// Checks that `[addr, addr + len)` is mapped and that `allowed` accepts
/// the permissions of every page; reports the first faulting byte.
/// Returns whether any of the pages is executable.
fn check(
    tables: &Tables,
    addr: u64,
    len: usize,
    access: Access,
    allowed: impl Fn(Prot) -> bool,
) -> Result<bool, MemFault> {
    check_with(|page| tables.pages.get(page), addr, len, access, allowed)
}

/// `check` with the page permissions from `prot_of` (a page number).
fn check_with(
    prot_of: impl Fn(u64) -> Option<Prot>,
    addr: u64,
    len: usize,
    access: Access,
    allowed: impl Fn(Prot) -> bool,
) -> Result<bool, MemFault> {
    if len == 0 {
        return Ok(false);
    }
    let fault = |at| MemFault { addr: at, access };
    let last = addr.checked_add(len as u64 - 1).ok_or(fault(addr))?;
    let mut executable = false;
    for page in addr / GUEST_PAGE_SIZE..=last / GUEST_PAGE_SIZE {
        match prot_of(page) {
            Some(prot) if allowed(prot) => executable |= prot.contains(Prot::EXEC),
            _ => return Err(fault(addr.max(page * GUEST_PAGE_SIZE))),
        }
    }
    Ok(executable)
}

/// Guest reads, writes and fetches on locked page tables, shared by
/// `AddressSpace` (which locks for each access) and `MemView`.
fn read_locked(tables: &Tables, addr: u64, buf: &mut [u8]) -> Result<(), MemFault> {
    check(tables, addr, buf.len(), Access::Read, |p| {
        p.allows(Access::Read)
    })?;
    // SAFETY: `check` verified that every byte is in a mapped guest page,
    // backed by a read-write host page owned by this address space; the
    // tables lock keeps it mapped.
    unsafe { std::ptr::copy_nonoverlapping(addr as *const u8, buf.as_mut_ptr(), buf.len()) };
    Ok(())
}

fn write_locked(
    space: &AddressSpace,
    tables: &Tables,
    addr: u64,
    data: &[u8],
) -> Result<(), MemFault> {
    let executable = check(tables, addr, data.len(), Access::Write, |p| {
        p.allows(Access::Write)
    })?;
    // SAFETY: as in `read_locked`; the host page is writable regardless of
    // the guest permissions, which `check` enforced, or made so by
    // `with_access` (an executable page with host protection).
    space.with_access(tables, addr, data.len(), true, || unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), addr as *mut u8, data.len())
    });
    if executable {
        // Self-modifying code: decoded instructions may be stale.
        space.code_changed();
    }
    Ok(())
}

fn fetch_locked(tables: &Tables, addr: u64, buf: &mut [u8]) -> usize {
    let mut count = 0;
    while count < buf.len() {
        let at = addr.wrapping_add(count as u64);
        let prot = tables.pages.get(at / GUEST_PAGE_SIZE);
        if !prot.is_some_and(|p| p.allows(Access::Execute)) {
            break;
        }
        // `at` is mapped, hence far below the top of the address space.
        let page_end = round_up(at + 1, GUEST_PAGE_SIZE).unwrap_or(u64::MAX);
        let n = (buf.len() - count).min((page_end - at) as usize);
        // SAFETY: `at..at + n` lies in one mapped guest page (checked
        // above), backed by a host page owned by this address space.
        unsafe { std::ptr::copy_nonoverlapping(at as *const u8, buf[count..].as_mut_ptr(), n) };
        count += n;
    }
    count
}

/// `AddressSpace::view`: guest memory with the page tables locked, so
/// that a block of accesses takes the lock once.
/// The page tables held for writing (`AddressSpace::exclusive`).
pub struct Exclusive<'a>(#[allow(dead_code)] RwLockWriteGuard<'a, Tables>);

pub struct MemView<'a> {
    space: &'a AddressSpace,
    tables: RwLockReadGuard<'a, Tables>,
}

impl MemView<'_> {
    /// Whether concurrent mode is on (ADR-0023): other code may write the
    /// guest's memory in parallel.
    pub fn is_concurrent(&self) -> bool {
        self.space.is_concurrent()
    }

    /// Whether a byte of `[addr, addr + len)` lies in a shared mapping's
    /// host pages (ADR-0022): memory another process may change at any
    /// moment.
    pub fn in_shared(&self, addr: u64, len: usize) -> bool {
        let host = self.space.host_page_size;
        let end = addr.saturating_add(len.max(1) as u64);
        self.tables
            .shared
            .range(addr / host..end.div_ceil(host))
            .next()
            .is_some()
    }
}

impl MemView<'_> {
    fn prot_of(&self, page: u64) -> Option<Prot> {
        self.tables.pages.get(page)
    }

    /// The flat page table, for the JIT's generated code (ADR-0021): one
    /// byte per guest page below 2^47, indexed by `addr >> 12`; 0 for an
    /// unmapped page, otherwise `Prot::bits`, a high bit and the derived
    /// bits `PAGE_READABLE_BIT` and `PAGE_WRITABLE_DATA_BIT`. The pointer
    /// never changes; the bytes only change while no `MemView` exists.
    /// The one place a host pointer leaves this crate.
    pub fn page_table(&self) -> *const u8 {
        self.tables.pages.base.as_ptr()
    }

    /// `AddressSpace::host_protection`.
    pub fn host_protection(&self) -> Option<u64> {
        self.space.host_protection()
    }
}

impl GuestMemory for MemView<'_> {
    fn time_stamp_counter(&self) -> u64 {
        time_stamp_counter()
    }

    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemFault> {
        check_with(
            |p| self.prot_of(p),
            addr,
            buf.len(),
            Access::Read,
            |p| p.allows(Access::Read),
        )?;
        // SAFETY: as in `read_locked`; the view holds the tables lock.
        unsafe { std::ptr::copy_nonoverlapping(addr as *const u8, buf.as_mut_ptr(), buf.len()) };
        if self.space.is_concurrent() {
            // x86 loads are not reordered with later accesses (TSO).
            fence(Ordering::Acquire);
        }
        Ok(())
    }

    fn write(&self, addr: u64, data: &[u8]) -> Result<(), MemFault> {
        let executable = check_with(
            |p| self.prot_of(p),
            addr,
            data.len(),
            Access::Write,
            |p| p.allows(Access::Write),
        )?;
        if self.space.is_concurrent() {
            // x86 stores are not reordered with earlier accesses (TSO).
            fence(Ordering::Release);
        }
        // SAFETY: as in `write_locked`; the view holds the tables lock.
        self.space
            .with_access(&self.tables, addr, data.len(), true, || unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), addr as *mut u8, data.len())
            });
        if executable {
            self.space.code_changed();
        }
        Ok(())
    }

    fn fetch(&self, addr: u64, buf: &mut [u8]) -> usize {
        fetch_locked(&self.tables, addr, buf)
    }

    fn code_generation(&self) -> u64 {
        self.space.code_generation.load(Ordering::Acquire)
    }

    #[cfg(target_arch = "aarch64")]
    fn compare_exchange_16(&self, addr: u64, expected: u128, new: u128) -> Result<u128, MemFault> {
        check_with(
            |p| self.prot_of(p),
            addr,
            16,
            Access::Read,
            |p| p.allows(Access::Read),
        )?;
        let executable = check_with(
            |p| self.prot_of(p),
            addr,
            16,
            Access::Write,
            |p| p.allows(Access::Write),
        )?;
        let old = if addr.is_multiple_of(16) {
            // SAFETY: aligned, and checked mapped and writable for the
            // guest, hence read-write in the host (as in `atomic_update`);
            // the view holds the tables lock.
            self.space
                .with_access(&self.tables, addr, 16, true, || unsafe {
                    host_compare_exchange_16(addr, expected, new)
                })
        } else {
            // cmpxchg16b raises #GP first: only reachable from armor itself.
            let mut bytes = [0u8; 16];
            self.read(addr, &mut bytes)?;
            let old = u128::from_le_bytes(bytes);
            self.write(
                addr,
                &(if old == expected { new } else { old }).to_le_bytes(),
            )?;
            old
        };
        if executable {
            self.space.code_changed();
        }
        Ok(old)
    }

    fn atomic_update(
        &self,
        addr: u64,
        size: usize,
        update: &mut dyn FnMut(u64) -> u64,
    ) -> Result<u64, MemFault> {
        check_with(
            |p| self.prot_of(p),
            addr,
            size,
            Access::Read,
            |p| p.allows(Access::Read),
        )?;
        let executable = check_with(
            |p| self.prot_of(p),
            addr,
            size,
            Access::Write,
            |p| p.allows(Access::Write),
        )?;
        // SAFETY: checked mapped and writable for the guest, hence backed by
        // host pages of this address space mapped read-write (a shared page
        // is guest-writable only if the host maps it writable); the view
        // holds the tables lock.
        let old = self
            .space
            .with_access(&self.tables, addr, size, true, || unsafe {
                host_atomic_update(addr, size, update)
            });
        if executable {
            self.space.code_changed();
        }
        Ok(old)
    }
}

impl Default for AddressSpace {
    fn default() -> Self {
        AddressSpace::new()
    }
}

impl GuestMemory for AddressSpace {
    fn time_stamp_counter(&self) -> u64 {
        time_stamp_counter()
    }

    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemFault> {
        read_locked(&self.tables(), addr, buf)
    }

    fn write(&self, addr: u64, data: &[u8]) -> Result<(), MemFault> {
        write_locked(self, &self.tables(), addr, data)
    }

    fn fetch(&self, addr: u64, buf: &mut [u8]) -> usize {
        fetch_locked(&self.tables(), addr, buf)
    }

    fn code_generation(&self) -> u64 {
        self.code_generation.load(Ordering::Acquire)
    }

    fn atomic_update(
        &self,
        addr: u64,
        size: usize,
        update: &mut dyn FnMut(u64) -> u64,
    ) -> Result<u64, MemFault> {
        self.view().atomic_update(addr, size, update)
    }

    fn compare_exchange_16(&self, addr: u64, expected: u128, new: u128) -> Result<u128, MemFault> {
        self.view().compare_exchange_16(addr, expected, new)
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        let tables = self.tables.get_mut().expect("page tables lock poisoned");
        for &page in &tables.host_pages {
            // SAFETY: every page in `host_pages` was mapped by this address
            // space, and no guest memory is accessed after drop.
            unsafe {
                libc::munmap(
                    (page * self.host_page_size) as *mut libc::c_void,
                    self.host_page_size as usize,
                )
            };
        }
    }
}

/// `value` rounded up to a multiple of `align`, or `None` on overflow.
fn round_up(value: u64, align: u64) -> Option<u64> {
    value.checked_next_multiple_of(align)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file mapped shared, grown with remap_shared in place and moved:
    /// the same file pages stay behind it, and the tables follow.
    #[test]
    fn shared_file_mappings_grow() {
        let mem = AddressSpace::new();
        let host = mem.host_page_size();
        let path = std::env::temp_dir().join(format!("armor-remap-{}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(8 * host).unwrap();
        use std::os::fd::AsRawFd;
        let source = SharedSource::File {
            fd: file.as_raw_fd(),
            offset: 0,
        };
        let addr = mem
            .map_shared(None, host, Prot::READ | Prot::WRITE, source, true)
            .unwrap();
        mem.write(addr, b"first").unwrap();
        // Grow: in place or moved, the bytes and the file stay the same.
        let grown = mem.remap_shared(addr, host, 3 * host, true).unwrap();
        let mut buf = [0u8; 5];
        mem.read(grown, &mut buf).unwrap();
        assert_eq!(&buf, b"first");
        mem.write(grown + 2 * host, b"third").unwrap();
        let mut from_file = [0u8; 5];
        use std::os::unix::fs::FileExt;
        file.read_exact_at(&mut from_file, 2 * host).unwrap();
        assert_eq!(&from_file, b"third");
        assert_eq!(mem.prot(grown + 2 * host), Some(Prot::READ | Prot::WRITE));
        if grown != addr {
            assert_eq!(mem.prot(addr), None);
        }
        // Not a shared mapping, or not on host-page boundaries: refused.
        let private = mem.map_anywhere(host, Prot::READ).unwrap();
        assert!(mem.remap_shared(private, host, 2 * host, true).is_err());
        assert!(mem.remap_shared(grown, host / 2, host, true).is_err());
        drop(file);
        let _ = std::fs::remove_file(&path);
    }

    /// A shared mapping of one guest page grown to three (on a host with
    /// 16K pages, inside its host page) and then to five (into a second
    /// host page there): the bytes and the file stay the same.
    #[test]
    fn shared_mappings_smaller_than_a_host_page_grow() {
        let mem = AddressSpace::new();
        let host = mem.host_page_size();
        let path = std::env::temp_dir().join(format!("armor-remap-small-{}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(8 * host).unwrap();
        use std::os::fd::AsRawFd;
        let source = SharedSource::File {
            fd: file.as_raw_fd(),
            offset: 0,
        };
        let page = GUEST_PAGE_SIZE;
        let addr = mem
            .map_shared(None, page, Prot::READ | Prot::WRITE, source, true)
            .unwrap();
        mem.write(addr, b"first").unwrap();
        let three = mem.remap_shared(addr, page, 3 * page, true).unwrap();
        assert_eq!(mem.prot(three + 2 * page), Some(Prot::READ | Prot::WRITE));
        assert_eq!(mem.prot(three + 3 * page), None);
        mem.write(three + 2 * page, b"third").unwrap();
        let five = mem.remap_shared(three, 3 * page, 5 * page, true).unwrap();
        let mut buf = [0u8; 5];
        mem.read(five, &mut buf).unwrap();
        assert_eq!(&buf, b"first");
        mem.read(five + 2 * page, &mut buf).unwrap();
        assert_eq!(&buf, b"third");
        mem.write(five + 4 * page, b"fifth").unwrap();
        use std::os::unix::fs::FileExt;
        file.read_exact_at(&mut buf, 4 * page).unwrap();
        assert_eq!(&buf, b"fifth");
        assert_eq!(mem.prot(five + 5 * page), None);
        drop(file);
        let _ = std::fs::remove_file(&path);
    }

    /// The 16-byte compare-and-swap, with LSE (when the host has it) and
    /// with the ARMv8.0 exclusive loop: equal, different, different only
    /// in the high half; the old value is returned and memory written as
    /// x86 does.
    #[test]
    #[cfg(target_arch = "aarch64")]
    fn compare_exchange_16_both_ways() {
        #[repr(align(16))]
        struct Cell(u128);
        type Cas = unsafe fn(u64, u128, u128) -> u128;
        let implementations: [(&str, Cas); 2] = [
            ("lse", compare_exchange_16_lse),
            ("exclusive", compare_exchange_16_exclusive),
        ];
        for (name, cas) in implementations {
            if name == "lse" && !host_has_lse() {
                continue;
            }
            let start = 0x1111_2222_3333_4444_5555_6666_7777_8888u128;
            let mut cell = Cell(start);
            let addr = &raw mut cell.0 as u64;
            // SAFETY: `cell` is 16-byte aligned and alive for each call.
            let old = unsafe { cas(addr, start, 42) };
            assert_eq!((old, cell.0), (start, 42), "{name}: equal");
            // SAFETY: as above.
            let old = unsafe { cas(addr, 7, 99) };
            assert_eq!((old, cell.0), (42, 42), "{name}: different");
            // SAFETY: as above.
            let old = unsafe { cas(addr, 42 | 1 << 64, 5) };
            assert_eq!((old, cell.0), (42, 42), "{name}: high half different");
        }
    }

    // Tests run in parallel threads of one process: each test maps its own
    // fixed address so that mappings never collide. Below 2^39: kernels
    // with 4K pages often give processes 39-bit addresses (the Raspberry
    // Pi's 4K kernel does).
    const BASE: u64 = 0x20_0000_0000;
    /// A fixed address for one test: every test uses its own slot, since
    /// the tests run in parallel in one process.
    fn test_addr(slot: u64) -> u64 {
        BASE + slot * 0x100_0000
    }

    #[test]
    fn the_memory_limit_refuses_mappings_beyond_it() {
        let mem = AddressSpace::new();
        let host = mem.host_page_size();
        mem.set_limit(4 * host);
        let a = mem.map_anywhere(3 * host, Prot::READ).unwrap();
        assert!(matches!(
            mem.map_anywhere(2 * host, Prot::READ),
            Err(MapError::Host(e)) if e.raw_os_error() == Some(libc::ENOMEM)
        ));
        // Freed pages count no more.
        mem.unmap(a, 3 * host).unwrap();
        mem.map_anywhere(4 * host, Prot::READ).unwrap();
    }

    #[test]
    fn permissions_are_enforced_per_guest_page() {
        let mem = AddressSpace::new();
        let a = test_addr(1);
        mem.map_fixed(a, GUEST_PAGE_SIZE, Prot::READ).unwrap();
        mem.map_fixed(
            a + GUEST_PAGE_SIZE,
            GUEST_PAGE_SIZE,
            Prot::READ | Prot::EXEC,
        )
        .unwrap();
        mem.map_fixed(
            a + 2 * GUEST_PAGE_SIZE,
            GUEST_PAGE_SIZE,
            Prot::READ | Prot::WRITE,
        )
        .unwrap();

        let mut byte = [0u8; 1];
        assert_eq!(mem.read(a, &mut byte), Ok(()));
        assert_eq!(
            mem.write(a, &[1]),
            Err(MemFault {
                addr: a,
                access: Access::Write
            })
        );
        assert_eq!(mem.write(a + 2 * GUEST_PAGE_SIZE, &[1]), Ok(()));
        assert_eq!(mem.fetch(a, &mut byte), 0);
        assert_eq!(mem.fetch(a + GUEST_PAGE_SIZE, &mut byte), 1);
    }

    #[test]
    fn privileged_writes_ignore_guest_permissions() {
        let mem = AddressSpace::new();
        let a = test_addr(2);
        mem.map_fixed(a, GUEST_PAGE_SIZE, Prot::READ).unwrap();
        mem.write_privileged(a + 8, b"armor").unwrap();
        let mut buf = [0u8; 5];
        mem.read(a + 8, &mut buf).unwrap();
        assert_eq!(&buf, b"armor");
    }

    #[test]
    fn access_crossing_into_an_unmapped_page_reports_the_first_bad_byte() {
        let mem = AddressSpace::new();
        let a = test_addr(3);
        mem.map_fixed(a, GUEST_PAGE_SIZE, Prot::READ | Prot::WRITE)
            .unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(
            mem.read(a + GUEST_PAGE_SIZE - 8, &mut buf),
            Err(MemFault {
                addr: a + GUEST_PAGE_SIZE,
                access: Access::Read
            })
        );
    }

    #[test]
    fn fetch_stops_at_the_first_non_executable_page() {
        let mem = AddressSpace::new();
        let a = test_addr(4);
        mem.map_fixed(a, GUEST_PAGE_SIZE, Prot::READ | Prot::EXEC)
            .unwrap();
        mem.map_fixed(a + GUEST_PAGE_SIZE, GUEST_PAGE_SIZE, Prot::READ)
            .unwrap();
        let mut buf = [0u8; 15];
        assert_eq!(mem.fetch(a + GUEST_PAGE_SIZE - 4, &mut buf), 4);
    }

    #[test]
    fn remapping_replaces_permissions_and_zeroes_contents() {
        let mem = AddressSpace::new();
        let a = test_addr(5);
        mem.map_fixed(a, GUEST_PAGE_SIZE, Prot::READ | Prot::WRITE)
            .unwrap();
        mem.write(a, &[0xaa; 4]).unwrap();
        mem.map_fixed(a, GUEST_PAGE_SIZE, Prot::READ).unwrap();
        let mut buf = [0xffu8; 4];
        mem.read(a, &mut buf).unwrap();
        assert_eq!(buf, [0; 4]);
        assert!(mem.write(a, &[1]).is_err());
    }

    #[test]
    fn write_only_pages_are_readable_like_on_x86() {
        let mem = AddressSpace::new();
        let a = test_addr(6);
        mem.map_fixed(a, GUEST_PAGE_SIZE, Prot::WRITE).unwrap();
        assert_eq!(mem.read(a, &mut [0u8; 1]), Ok(()));
        mem.map_fixed(a, GUEST_PAGE_SIZE, Prot::NONE).unwrap();
        assert!(mem.read(a, &mut [0u8; 1]).is_err());
    }

    #[test]
    fn mappings_stay_below_the_ceiling() {
        let mem = AddressSpace::new();
        let ceiling = 1 << 32;
        mem.set_ceiling(ceiling);
        let rw = Prot::READ | Prot::WRITE;
        let mut placed = Vec::new();
        for len in [1, 100_000, 3 << 20, 5_000] {
            let a = mem.map_anywhere(len, rw).unwrap();
            assert!(a >= PLACEMENT_FLOOR && a + len <= ceiling, "{a:#x}");
            assert_eq!(a % mem.host_page_size(), 0);
            mem.write(a + len - 1, &[1]).unwrap();
            placed.push((a, len));
        }
        // Top-down, and never over each other.
        for pair in placed.windows(2) {
            let ((high, _), (low, low_len)) = (pair[0], pair[1]);
            assert!(low + low_len <= high, "{low:#x} under {high:#x}");
        }
        // A freed range is used again.
        let (a, len) = placed[1];
        mem.unmap(a, len).unwrap();
        assert_eq!(mem.map_anywhere(len, rw).unwrap(), a);
        // Shared mappings too.
        let shared = mem
            .map_shared(None, 10_000, rw, SharedSource::Anonymous, true)
            .unwrap();
        assert!(shared + 10_000 <= ceiling, "{shared:#x}");
    }

    #[test]
    fn map_anywhere_returns_a_usable_host_aligned_range() {
        let mem = AddressSpace::new();
        let a = mem.map_anywhere(10_000, Prot::READ | Prot::WRITE).unwrap();
        assert_eq!(a % mem.host_page_size(), 0);
        mem.write(a + 9_999, &[7]).unwrap();
        let mut buf = [0u8; 1];
        mem.read(a + 9_999, &mut buf).unwrap();
        assert_eq!(buf, [7]);
    }

    #[test]
    fn mapping_over_host_memory_in_use_fails() {
        let host = AddressSpace::new();
        let taken = host.map_anywhere(1, Prot::READ).unwrap();
        let guest = AddressSpace::new();
        assert!(matches!(
            guest.map_fixed(taken, GUEST_PAGE_SIZE, Prot::READ),
            Err(MapError::AddressInUse { .. })
        ));
    }

    #[test]
    fn page_table_bytes_carry_the_bits_the_jit_tests() {
        let mem = AddressSpace::new();
        let a = test_addr(10);
        mem.map_fixed(a, 8 * GUEST_PAGE_SIZE, Prot::NONE).unwrap();
        for bits in 0..8u8 {
            let addr = a + u64::from(bits) * GUEST_PAGE_SIZE;
            mem.protect(addr, GUEST_PAGE_SIZE, Prot::from_bits(bits))
                .unwrap();
            let view = mem.view();
            // SAFETY: the page is mapped, so its byte is inside the table.
            let byte = unsafe { *view.page_table().add((addr / GUEST_PAGE_SIZE) as usize) };
            let readable = bits != 0;
            let data = bits & 2 != 0 && bits & 4 == 0;
            assert_eq!(byte >> PAGE_READABLE_BIT & 1 == 1, readable, "prot {bits}");
            assert_eq!(byte >> PAGE_WRITABLE_DATA_BIT & 1 == 1, data, "prot {bits}");
            // The next page is still mapped PROT_NONE (or the last one of
            // the range: then unmapped): no span.
            assert_eq!(byte >> PAGE_READABLE_SPAN_BIT & 1, 0, "prot {bits}");
            assert_eq!(
                view.prot_of(addr / GUEST_PAGE_SIZE),
                Some(Prot::from_bits(bits))
            );
        }
    }

    /// The span bits of a page follow its own permissions and the next
    /// page's, whichever of the two changes.
    #[test]
    fn span_bits_follow_both_pages() {
        let mem = AddressSpace::new();
        let a = test_addr(11);
        let rw = Prot::READ | Prot::WRITE;
        let byte = |mem: &AddressSpace, page: u64| {
            let view = mem.view();
            // SAFETY: the page number is inside the table.
            unsafe {
                *view
                    .page_table()
                    .add(((a / GUEST_PAGE_SIZE) + page) as usize)
            }
        };
        let spans = |b: u8| {
            (
                b >> PAGE_READABLE_SPAN_BIT & 1,
                b >> PAGE_WRITABLE_DATA_SPAN_BIT & 1,
            )
        };
        mem.map_fixed(a, 2 * GUEST_PAGE_SIZE, rw).unwrap();
        assert_eq!(spans(byte(&mem, 0)), (1, 1));
        assert_eq!(spans(byte(&mem, 1)), (0, 0), "the page after is unmapped");
        // The second page changes: the first page's spans follow.
        mem.protect(a + GUEST_PAGE_SIZE, GUEST_PAGE_SIZE, Prot::READ)
            .unwrap();
        assert_eq!(spans(byte(&mem, 0)), (1, 0));
        mem.protect(
            a + GUEST_PAGE_SIZE,
            GUEST_PAGE_SIZE,
            Prot::READ | Prot::WRITE | Prot::EXEC,
        )
        .unwrap();
        assert_eq!(spans(byte(&mem, 0)), (1, 0), "code is not writable data");
        mem.protect(a + GUEST_PAGE_SIZE, GUEST_PAGE_SIZE, Prot::NONE)
            .unwrap();
        assert_eq!(spans(byte(&mem, 0)), (0, 0));
        mem.unmap(a + GUEST_PAGE_SIZE, GUEST_PAGE_SIZE).unwrap();
        assert_eq!(spans(byte(&mem, 0)), (0, 0));
        mem.map_fixed(a + GUEST_PAGE_SIZE, GUEST_PAGE_SIZE, rw)
            .unwrap();
        assert_eq!(spans(byte(&mem, 0)), (1, 1));
        // The first page changes.
        mem.protect(a, GUEST_PAGE_SIZE, Prot::READ).unwrap();
        assert_eq!(spans(byte(&mem, 0)), (1, 0));
        mem.unmap(a, GUEST_PAGE_SIZE).unwrap();
        assert_eq!(byte(&mem, 0), 0);
    }

    /// A temporary file of `pages` guest pages, each filled with its index.
    fn page_file(pages: u64) -> std::fs::File {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("armor-mem-shared-{}-{pages}", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        for page in 0..pages {
            file.write_all(&vec![page as u8; GUEST_PAGE_SIZE as usize])
                .unwrap();
        }
        file
    }

    /// ADR-0022: without MAP_FIXED, the guest address has the offset's
    /// remainder modulo the host page; with it, the remainder must match
    /// and the host pages must hold no other guest page.
    #[test]
    fn shared_mappings_follow_the_offset_modulo_the_host_page() {
        use std::os::fd::AsRawFd;
        let mem = AddressSpace::new();
        let host = mem.host_page_size();
        let file = page_file(8);
        let fd = file.as_raw_fd();
        let rw = Prot::READ | Prot::WRITE;
        let offset = GUEST_PAGE_SIZE;
        let source = SharedSource::File { fd, offset };
        let addr = mem
            .map_shared(None, GUEST_PAGE_SIZE, rw, source, true)
            .unwrap();
        assert_eq!(addr % host, offset % host);
        let mut byte = [0u8];
        mem.read(addr, &mut byte).unwrap();
        assert_eq!(byte, [1], "the page at offset 4K");
        // The rest of the host page is not the guest's.
        if host > GUEST_PAGE_SIZE {
            assert_eq!(mem.prot(addr - GUEST_PAGE_SIZE), None);
        }
        assert!(mem.any_shared(addr, 1));
        // A write reaches the file.
        mem.write(addr, &[0xee]).unwrap();
        let mut from_file = [0u8];
        std::os::unix::fs::FileExt::read_at(&file, &mut from_file, offset).unwrap();
        assert_eq!(from_file, [0xee]);

        if host > GUEST_PAGE_SIZE {
            // MAP_FIXED with the wrong remainder.
            let base = mem.map_anywhere(4 * host, Prot::NONE).unwrap();
            assert!(matches!(
                mem.map_shared(Some(base), GUEST_PAGE_SIZE, rw, source, true),
                Err(MapError::SharedPlacement(_))
            ));
            // The right remainder, but the reservation shares the host page.
            assert!(matches!(
                mem.map_shared(Some(base + offset), GUEST_PAGE_SIZE, rw, source, true),
                Err(MapError::SharedPlacement(_))
            ));
            // Whole host pages of the reservation: replaced.
            let at = base + host;
            let source0 = SharedSource::File { fd, offset: 0 };
            assert_eq!(
                mem.map_shared(Some(at), host, rw, source0, true).unwrap(),
                at
            );
            // Page 1 of the file, written through the first mapping.
            mem.read(at + GUEST_PAGE_SIZE, &mut byte).unwrap();
            assert_eq!(byte, [0xee]);
            // A private mapping over all of it gets private zeroed pages
            // and leaves the file alone.
            mem.map_fixed(at, host, rw).unwrap();
            assert!(!mem.any_shared(at, host));
            mem.write(at + GUEST_PAGE_SIZE, &[0x55]).unwrap();
            std::os::unix::fs::FileExt::read_at(&file, &mut from_file, GUEST_PAGE_SIZE).unwrap();
            assert_eq!(from_file, [0xee]);
            // A private mapping over part of a shared host page: refused.
            let shared = mem.map_shared(Some(at), host, rw, source0, true).unwrap();
            assert!(matches!(
                mem.map_fixed(shared, GUEST_PAGE_SIZE, rw),
                Err(MapError::SharedPlacement(_))
            ));
        }
    }

    #[test]
    fn shared_pages_keep_their_rules() {
        use std::os::fd::AsRawFd;
        let mem = AddressSpace::new();
        let file = page_file(4);
        let source = SharedSource::File {
            fd: file.as_raw_fd(),
            offset: 0,
        };
        // Host pages mapped read-only: no write permission later.
        let addr = mem
            .map_shared(None, 2 * GUEST_PAGE_SIZE, Prot::READ, source, false)
            .unwrap();
        assert_eq!(
            mem.protect(addr, GUEST_PAGE_SIZE, Prot::READ | Prot::WRITE),
            Err(ProtectError::ReadOnlyShared)
        );
        assert_eq!(
            mem.protect(addr, GUEST_PAGE_SIZE, Prot::READ | Prot::EXEC),
            Err(ProtectError::ExecutableShared)
        );
        assert_eq!(mem.protect(addr, GUEST_PAGE_SIZE, Prot::NONE), Ok(()));
        mem.protect(addr, GUEST_PAGE_SIZE, Prot::READ).unwrap();
        // MADV_DONTNEED keeps the contents of shared pages.
        mem.discard(addr, 2 * GUEST_PAGE_SIZE).unwrap();
        let mut byte = [0u8];
        mem.read(addr + GUEST_PAGE_SIZE, &mut byte).unwrap();
        assert_eq!(byte, [1]);
        // msync: mapped range, then a range that is not.
        assert_eq!(mem.sync(addr, 2 * GUEST_PAGE_SIZE, libc::MS_SYNC), Ok(()));
        assert_eq!(
            mem.sync(addr, 64 * GUEST_PAGE_SIZE, libc::MS_SYNC),
            Err(libc::ENOMEM)
        );
        mem.unmap(addr, 2 * GUEST_PAGE_SIZE).unwrap();
        assert!(!mem.any_shared(addr, 2 * GUEST_PAGE_SIZE));
    }

    /// ADR-0023: concurrent mode starts a new code generation once, so
    /// that code translated for a single runner is translated again.
    #[test]
    fn concurrent_mode_starts_a_new_code_generation_once() {
        let mem = AddressSpace::new();
        assert!(!mem.is_concurrent());
        let before = mem.code_generation();
        mem.enter_concurrent_mode();
        assert!(mem.is_concurrent() && mem.view().is_concurrent());
        assert_eq!(mem.code_generation(), before + 1);
        mem.enter_concurrent_mode();
        assert_eq!(mem.code_generation(), before + 1);
        // A writable shared mapping turns it on too.
        let other = AddressSpace::new();
        other
            .map_shared(
                None,
                4096,
                Prot::READ | Prot::WRITE,
                SharedSource::Anonymous,
                true,
            )
            .unwrap();
        assert!(other.is_concurrent());
    }

    /// A guest may pass any length to munmap, mprotect, madvise or brk.
    /// These visit only the pages that exist: page by page, 2^34 steps
    /// took minutes (found by the syscall fuzz test).
    #[test]
    fn huge_ranges_cost_only_the_mapped_pages() {
        let mem = AddressSpace::new();
        let a = test_addr(13);
        mem.map_fixed(a, GUEST_PAGE_SIZE, Prot::READ | Prot::WRITE)
            .unwrap();
        let huge = 1u64 << 46;
        let start = std::time::Instant::now();
        assert_eq!(
            mem.protect(0, huge, Prot::READ),
            Err(ProtectError::NotMapped)
        );
        assert!(mem.any_mapped(0, huge));
        assert!(!mem.any_mapped(a + mem.host_page_size(), huge));
        assert!(!mem.any_mapped(0, a));
        assert_eq!(
            mem.discard(0, huge),
            Err(MemFault {
                addr: 0,
                access: Access::Write
            })
        );
        mem.unmap(0, huge).unwrap();
        assert_eq!(mem.prot(a), None);
        let elapsed = start.elapsed();
        assert!(elapsed < std::time::Duration::from_secs(1), "{elapsed:?}");
    }

    #[test]
    fn discard_zeroes_mapped_pages_and_reports_holes() {
        let mem = AddressSpace::new();
        let host = mem.host_page_size();
        let a = test_addr(9);
        // Three host pages, then a hole of one guest page, then one more.
        let mapped = 3 * host;
        mem.map_fixed(a, mapped, Prot::READ | Prot::WRITE).unwrap();
        mem.map_fixed(
            a + mapped + GUEST_PAGE_SIZE,
            GUEST_PAGE_SIZE,
            Prot::READ | Prot::WRITE,
        )
        .unwrap();
        let pattern = vec![0xa5u8; mapped as usize];
        mem.write(a, &pattern).unwrap();
        mem.write(a + mapped + GUEST_PAGE_SIZE, &[0xa5; 16])
            .unwrap();
        // From the middle of the first host page (partial) through two
        // whole host pages, the hole and the last page.
        let start = a + GUEST_PAGE_SIZE;
        let len = mapped + 2 * GUEST_PAGE_SIZE - GUEST_PAGE_SIZE;
        assert_eq!(
            mem.discard(start, len),
            Err(MemFault {
                addr: a + mapped,
                access: Access::Write
            })
        );
        let mut bytes = vec![0u8; mapped as usize];
        mem.read(a, &mut bytes).unwrap();
        let first = GUEST_PAGE_SIZE as usize;
        assert!(
            bytes[..first].iter().all(|&b| b == 0xa5),
            "before the range"
        );
        assert!(bytes[first..].iter().all(|&b| b == 0), "the range");
        let mut last = [0u8; 16];
        mem.read(a + mapped + GUEST_PAGE_SIZE, &mut last).unwrap();
        assert_eq!(last, [0; 16], "after the hole");
    }

    #[test]
    fn unmap_removes_pages_and_frees_empty_host_pages() {
        let mem = AddressSpace::new();
        let a = test_addr(8);
        let host = mem.host_page_size();
        mem.map_fixed(a, 2 * host, Prot::READ | Prot::WRITE)
            .unwrap();
        mem.unmap(a, GUEST_PAGE_SIZE).unwrap();
        assert!(mem.read(a, &mut [0u8; 1]).is_err());
        assert!(mem.read(a + GUEST_PAGE_SIZE, &mut [0u8; 1]).is_ok());
        mem.unmap(a, 2 * host).unwrap();
        assert!(mem.tables().host_pages.is_empty());
        // The host range is free again.
        mem.map_fixed(a, host, Prot::READ).unwrap();
    }

    #[test]
    fn protect_changes_permissions_of_mapped_pages_only() {
        let mem = AddressSpace::new();
        let a = test_addr(12);
        mem.map_fixed(a, 2 * GUEST_PAGE_SIZE, Prot::READ | Prot::WRITE)
            .unwrap();
        mem.protect(a, GUEST_PAGE_SIZE, Prot::READ).unwrap();
        assert!(mem.write(a, &[1]).is_err());
        assert!(mem.write(a + GUEST_PAGE_SIZE, &[1]).is_ok());
        assert_eq!(
            mem.protect(a, 3 * GUEST_PAGE_SIZE, Prot::NONE),
            Err(ProtectError::NotMapped)
        );
        assert!(mem.write(a + GUEST_PAGE_SIZE, &[1]).is_ok(), "unchanged");
    }

    #[test]
    fn map_anywhere_maps_only_the_requested_guest_pages() {
        let mem = AddressSpace::new();
        let a = mem.map_anywhere(GUEST_PAGE_SIZE, Prot::READ).unwrap();
        assert!(mem.read(a + GUEST_PAGE_SIZE - 1, &mut [0u8; 1]).is_ok());
        assert!(mem.read(a + GUEST_PAGE_SIZE, &mut [0u8; 1]).is_err());
    }

    #[test]
    fn huge_ranges_are_errors_not_panics() {
        let mem = AddressSpace::new();
        let top = !(GUEST_PAGE_SIZE - 1);
        assert!(matches!(
            mem.map_fixed(top, GUEST_PAGE_SIZE, Prot::READ),
            Err(MapError::OutOfRange)
        ));
        assert!(matches!(
            mem.map_anywhere(u64::MAX, Prot::READ),
            Err(MapError::OutOfRange)
        ));
        assert!(matches!(
            mem.unmap(top, u64::MAX),
            Err(MapError::OutOfRange)
        ));
        assert_eq!(
            mem.protect(top, u64::MAX, Prot::READ),
            Err(ProtectError::NotMapped)
        );
        assert!(mem.read(u64::MAX, &mut [0u8; 2]).is_err());
    }

    #[test]
    fn unaligned_mappings_are_rejected() {
        let mem = AddressSpace::new();
        assert!(matches!(
            mem.map_fixed(test_addr(7) + 1, 1, Prot::READ),
            Err(MapError::Unaligned { .. })
        ));
    }
}
