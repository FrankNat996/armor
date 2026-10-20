//! A cache of decoded instructions, per thread. Without it the interpreter
//! fetched and decoded every instruction each time it ran: 47% of the time
//! of a python run, measured with perf on 2026-10-12.
//!
//! An entry is valid only for the `GuestMemory::code_generation` it was
//! decoded under; the address space changes the generation on every
//! mapping change and every write to an executable page, so stale code is
//! never executed (self-modifying code, `mprotect`, `munmap`).
use alloc::boxed::Box;
use alloc::vec;

use armor_x86::Instruction;

/// Entries: a power of two, direct-mapped by address.
const SIZE: usize = 32768;

#[derive(Clone, Copy)]
struct Entry {
    rip: u64,
    generation: u64,
    insn: Instruction,
}

#[derive(Clone)]
pub struct DecodeCache {
    entries: Box<[Option<Entry>]>,
}

impl DecodeCache {
    pub fn new() -> DecodeCache {
        DecodeCache {
            entries: vec![None; SIZE].into_boxed_slice(),
        }
    }

    fn slot(rip: u64) -> usize {
        (rip ^ (rip >> 12)) as usize & (SIZE - 1)
    }

    pub fn get(&self, rip: u64, generation: u64) -> Option<Instruction> {
        match self.entries[Self::slot(rip)] {
            Some(entry) if entry.rip == rip && entry.generation == generation => Some(entry.insn),
            _ => None,
        }
    }

    pub fn insert(&mut self, rip: u64, generation: u64, insn: Instruction) {
        self.entries[Self::slot(rip)] = Some(Entry {
            rip,
            generation,
            insn,
        });
    }
}

impl Default for DecodeCache {
    fn default() -> Self {
        DecodeCache::new()
    }
}

// The cache is not part of the architectural state.
impl PartialEq for DecodeCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for DecodeCache {}

impl core::fmt::Debug for DecodeCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("DecodeCache")
    }
}
