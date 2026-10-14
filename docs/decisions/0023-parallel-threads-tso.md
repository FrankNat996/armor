# 0023 — Parallel threads and the TSO memory model

- **Status:** Accepted (2026-10-13). Real parallel threads for the M6 TSO item (the alternative was to keep the GIL and rephrase the item); the details below can be revised. Replaces ADR-0018 for the execution of guest code; 1:1, real `futex` and the rest of ADR-0018 remain.
- **Date:** 2026-10-13

## Context
With ADR-0018 only one guest thread at a time runs code (the GIL):
- the ordering seen by the guest is sequentially consistent;
- `LOCK` instructions are atomic by construction.

The price is that a program with N compute threads runs at the speed of one: `cargo xtask bench threads` gives 0.93 s against 0.35 s for qemu, which runs the threads in parallel (`tech/performance.md`). With `MAP_SHARED` (ADR-0022) different processes also write the same memory in parallel, so ordering already matters today.

x86 guarantees TSO: stores go out in order, loads do not overtake each other, only a load can pass an earlier store to a different address. AArch64 allows much more.

Verified host facts (2026-10-13, `/proc/cpuinfo` and a native program):
- Cortex-A76 with `lrcpc` (load-acquire RCpc, `LDAPR`) and `atomics` (LSE);
- **without** `uscat` (LSE2): `LDAPR` and `STLR` on an unaligned address give SIGBUS, while normal unaligned `LDR` and `STR` work.

## Decision
- **Concurrent mode, per process.**
  - It turns on at the first writable shared mapping (ADR-0022) or at the first `clone` with `CLONE_THREAD`, and never turns off.
  - When it turns on it advances the code generation, before the new thread starts: the code translated so far is translated again.
  - Single-threaded programs without shared memory stay exactly as today, with the same performance.
- **In concurrent mode guest code runs in parallel:** no GIL for the interpreter and the JIT. The GIL stays for `--lockstep`, whose double-execution-and-compare scheme works only with one executor at a time, and for `ARMOR_GIL=1`, an escape hatch to isolate problems.
- **TSO ordering in concurrent mode:**
  - **JIT:**
    - general-purpose register loads with `LDAPR` (RCpc: together with store-release it gives TSO, which allows a load to pass an earlier store), stores with `STLR`;
    - an unaligned access goes to an out-of-line piece with normal `LDR`/`STR` between two `DMB`s;
    - 128-bit and floating-point accesses with `DMB` before stores and after loads;
    - `mfence` becomes `DMB ISH`, and so do `lfence` and `sfence`.
  - **Interpreter:** acquire barrier after every guest load, release before every store, real barriers for `mfence`, `lfence` and `sfence`.
  - **`LOCK` and `xchg` with memory:**
    - host atomics (compare-and-swap, `GuestMemory::atomic_update`), which are also full barriers as on x86;
    - at first the JIT leaves them to the interpreter, as it already does with shared memory; LSE atomics in the JIT later, if measurements call for it (step 4, done on 2026-10-13: `LDADDAL`, `LDCLRAL`, `LDEORAL`, `LDSETAL`, `SWPAL`, `CASAL` for add, sub, and, or, xor, inc, dec, not, `xchg`, `cmpxchg`, `xadd` on an aligned operand; the rest, including split locks, stays in the interpreter);
    - `cmpxchg8b` and `cmpxchg16b`: host compare-and-swap at 8 and 16 bytes (LSE's `CASPAL` for the 16 bytes);
    - an unaligned `LOCK` ("split lock") stays non-atomic: x86 makes it atomic, armor does not (rare case, documented).
- **Code modified by another thread:** a block already running finishes with the old code, and the new generation applies from the next block. x86 requires a serialization anyway for code modified across processors.
- **Order of the steps:**
  1. concurrent mode and interpreter barriers, with the GIL still active;
  2. ordering in the JIT, with the GIL still active;
  3. remove the GIL;
  4. possibly LSE atomics in the JIT (done on 2026-10-13, `tech/jit.md`).

  Every step has its own verification. The M6 TSO item is ticked only after step 3 with the litmus tests green.
- **Verification:**
  - **Litmus tests** (`c/litmus`): message passing (MP) and store buffering with `mfence` (SB). They count the results that TSO forbids, which must be 0, as under qemu-x86_64. The same source compiled for AArch64 shows forbidden results (MP always; SB without the barrier): this showed that the test is sensitive to weak ordering;
  - **repetitions:** `c/threads`, `c/threads_signals`, `c/shared_atomics` and the litmus tests, dozens of runs per engine.

## Alternatives considered
- **Keeping the GIL and declaring TSO respected** (stronger than TSO): correct, but without parallelism; discarded.
- **`LDAR` (RCsc) instead of `LDAPR`:** stronger than needed and slower.
- **Barriers only where needed, found with an analysis:** cheaper, but a mistake gives rare, invisible bugs. Perhaps later, with measurements.
- **Ordering always, even with a single thread:** simple, but it would slow down all programs for nothing.

## Consequences
- \+ Programs with several compute threads use all the cores.
- \+ Memory shared between processes follows x86 ordering.
- − In concurrent mode every memory access costs more (barriers, alignment check): to be measured with `cargo xtask bench`.
- − All of armor's state that the GIL implicitly protected had to be reviewed (step 3, done on 2026-10-13): signal dispositions, `execve` with several threads, the page-table lock held for a whole block.
- − Lockstep runs multi-threaded programs under the GIL too: it does not see ordering problems, which are the job of the litmus tests.
