# 0005 — The guest sees 4K pages even on 16K hosts

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
The host uses 16K pages (`environment.md`, verified). x86_64 programs expect 4K pages in several places: `AT_PAGESZ`, alignment of `mmap`/`mprotect`/`munmap`, alignment of ELF segments, offsets of mapped files. A 4K kernel can be installed, but it requires changing the boot configuration.

## Decision
- armor **guarantees 4K semantics to the guest** even on 16K hosts (`AT_PAGESZ=4096`, 4K granularity for the memory syscalls).
- The implementation strategy was open: the options are in `tech/memory.md`. Closed on 2026-10-13 by ADR-0009, 0012, 0021 and 0022.

## Alternatives considered
- **Requiring a 4K kernel:** contradicts the goal of ease of use, because the Pi 5 is 16K by default.
- **Exposing 16K to the guest:** x86_64 binaries assume 4K. ✅ Measured on 2026-10-12: even the minimal binary produced by gcc/binutils has `PT_LOAD` segments with `p_align` 0x1000 and different permissions within the same 16K page (`tech/memory.md`).

Cross-check ✅ (2026-10-12): `qemu-x86_64` on this 16K host also declares `AT_PAGESZ` = 4096 to the guest, so it solves the same problem. qemu's code (`linux-user/`) can be consulted as a reference, without copying it (GPL).

Implementation for M1: ADR-0009.

## Consequences
- \+ Works on the Pi 5's default configuration.
- − Complexity in the memory manager: permission table per 4K page, file mappings with offsets not aligned to 16K, loss of granularity in faults.
- − Detection of self-modifying code via `mprotect` works at 16K granularity.
