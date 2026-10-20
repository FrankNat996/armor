//! What armor's interpreter needs from guest memory, without the host:
//! the `GuestMemory` trait and its types. `armor-mem` implements it on a
//! Linux address space; the Windows backend (ADR-0047) on the memory of
//! the process Wine runs. No standard library, so that both can use it.

#![no_std]

use core::fmt;
use core::ops::BitOr;

pub const GUEST_PAGE_SIZE: u64 = 4096;

/// Guest page permissions, as in `PROT_*`.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Prot(u8);

impl Prot {
    pub const NONE: Prot = Prot(0);
    pub const READ: Prot = Prot(1);
    pub const WRITE: Prot = Prot(2);
    pub const EXEC: Prot = Prot(4);

    /// The `PROT_*` bits, as stored in the page table (`MemView::page_table`).
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The permissions of `PROT_*` bits (the low three).
    pub const fn from_bits(bits: u8) -> Prot {
        Prot(bits & 7)
    }

    pub fn contains(self, other: Prot) -> bool {
        self.0 & other.0 == other.0
    }

    /// x86-64 page tables cannot express write-only or execute-only pages:
    /// every page that is mapped with any permission is readable.
    pub fn allows(self, access: Access) -> bool {
        match access {
            Access::Read => self != Prot::NONE,
            Access::Write => self.contains(Prot::WRITE),
            Access::Execute => self.contains(Prot::EXEC),
        }
    }
}

impl BitOr for Prot {
    type Output = Prot;
    fn bitor(self, rhs: Prot) -> Prot {
        Prot(self.0 | rhs.0)
    }
}

impl fmt::Debug for Prot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let flag = |p: Prot, c: char| if self.contains(p) { c } else { '-' };
        write!(
            f,
            "{}{}{}",
            flag(Prot::READ, 'r'),
            flag(Prot::WRITE, 'w'),
            flag(Prot::EXEC, 'x')
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Execute,
}

/// A guest access that the guest page permissions do not allow.
/// `addr` is the first byte of the access that faulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemFault {
    pub addr: u64,
    pub access: Access,
}

impl fmt::Display for MemFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.access {
            Access::Read => "read",
            Access::Write => "write",
            Access::Execute => "execute",
        };
        write!(f, "invalid {what} at {:#x}", self.addr)
    }
}

/// What the interpreter needs from guest memory.
pub trait GuestMemory {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemFault>;
    fn write(&self, addr: u64, data: &[u8]) -> Result<(), MemFault>;
    /// Copies executable bytes starting at `addr` into `buf`, stopping at the
    /// first byte whose page is not executable. Returns how many were copied.
    fn fetch(&self, addr: u64, buf: &mut [u8]) -> usize;
    /// The value rdtsc reads: a count that only grows (armor's are
    /// nanoseconds). The host provides it; this default is 0.
    fn time_stamp_counter(&self) -> u64 {
        0
    }
    /// A counter that changes whenever executable bytes or permissions may
    /// have changed, so that decoded instructions can be cached.
    fn code_generation(&self) -> u64 {
        0
    }
    /// An x86 `LOCK` read-modify-write of the `size`-byte value at `addr`
    /// (1, 2, 4 or 8 bytes): replaces it with `update(old)` and returns
    /// `old`. Another process sharing the memory (ADR-0022) sees either
    /// value, never a mix. This default reads and writes, which is exact
    /// for memory nothing else can see.
    fn atomic_update(
        &self,
        addr: u64,
        size: usize,
        update: &mut dyn FnMut(u64) -> u64,
    ) -> Result<u64, MemFault> {
        let mut bytes = [0u8; 8];
        self.read(addr, &mut bytes[..size])?;
        let old = u64::from_le_bytes(bytes);
        self.write(addr, &update(old).to_le_bytes()[..size])?;
        Ok(old)
    }
    /// `lock cmpxchg16b`: if the 16 bytes at `addr` (16-byte aligned)
    /// equal `expected`, replaces them with `new`; returns the old value
    /// either way, and writes in both cases as x86 does. Atomic like
    /// `atomic_update`; this default reads and writes.
    fn compare_exchange_16(&self, addr: u64, expected: u128, new: u128) -> Result<u128, MemFault> {
        let mut bytes = [0u8; 16];
        self.read(addr, &mut bytes)?;
        let old = u128::from_le_bytes(bytes);
        let value = if old == expected { new } else { old };
        self.write(addr, &value.to_le_bytes())?;
        Ok(old)
    }
}

/// The layout of `armor-mem`'s permission table, which the JIT reads
/// (ADR-0021).
/// Bits of a `PageTable` byte derived from the `Prot`, for the JIT: one
/// `tbz` per check instead of a mask and a compare.
/// Set when the page is readable (any permission, as on x86).
pub const PAGE_READABLE_BIT: u32 = 3;
/// Set when the page is writable and not executable: writes to code must
/// go through the interpreter, which invalidates translations.
pub const PAGE_WRITABLE_DATA_BIT: u32 = 4;
/// Set when this page and the next one are both readable: an access that
/// starts here may cross into the next page.
pub const PAGE_READABLE_SPAN_BIT: u32 = 5;
/// Set when this page and the next one are both writable data pages.
pub const PAGE_WRITABLE_DATA_SPAN_BIT: u32 = 6;
