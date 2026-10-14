# 0020 — Cache of decoded instructions, invalidated by a code generation

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
`perf` on the python traceback (13 s, against 0.8 s for qemu) showed that 47% of the time went to re-reading and re-decoding instructions already seen, and 16% to the page-table lock taken at every access (`tech/performance.md`). A cache of decoded instructions must stay correct if the code changes:
- self-modifying code, JIT compilers in the guest;
- `munmap` and `mmap` at the same address;
- `mprotect` that removes execute permission.

## Decision
- **Per-thread cache**, direct-mapped (32,768 entries), of address → decoded instruction, in the `Cpu`.
- **Code generation** in `AddressSpace`: an atomic counter that grows at every write to an executable page and at every `map_fixed`/`map_anywhere`/`unmap`/`protect` that touches executable pages (before or after). An entry is valid only for the generation in which it was decoded. It is a simple, conservative rule: when in doubt, decode again.
- **One table lock per block of instructions** (`AddressSpace::view`), with a small permission TLB valid for the lifetime of the view (replaced on 2026-10-12 by the flat permission table of ADR-0021).

## Alternatives considered
- **Per-page invalidation** (one generation per page, or the list of entries of each page): more precise for programs that often write to executable pages (JIT in the guest), but more expensive to maintain. To be reconsidered with armor's JIT (M6), which will have to track translated pages anyway.
- **Protecting code pages and catching writes with SIGSEGV** (as many JITs do): useful in M6 with native code, useless with the interpreter, which already checks every write in software.

## Consequences
- \+ Python 3.2 times faster, `difftest` more than halved, no difference in behaviour (217 cases out of 217).
- \+ Self-modifying code stays correct: three dedicated tests and mutation tests.
- − 2.4 MB of cache per guest thread.
- − A program that continually writes to executable pages invalidates the whole cache at every write: it would be slow, but correct.
