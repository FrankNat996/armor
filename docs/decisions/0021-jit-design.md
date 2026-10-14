# 0021 — Design of the AArch64 JIT (M6)

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12
- Closes the open decision "IR yes/no, to be taken before M6" (ADR-0003, `architecture.md`).

## Context
After the instruction cache (ADR-0020) python is still 5 times slower than under qemu: the remaining time is interpretation itself (`tech/performance.md`). Project constraints the design must respect:
- **4K guest pages on 16K hosts:** even the simplest binaries put R, RX and RW pages within the same host page (ADR-0009), so host protections cannot enforce the guest's permissions;
- **threads with a GIL** (ADR-0018);
- **signals with precise state** of the guest (ADR-0017);
- **self-modifying code** (ADR-0020);
- **no dependencies without an ADR** (ADR-0008);
- **the interpreter as oracle** (ADR-0003).

## Decision
1. **Permissions checked in software, with a flat table.** An array of one byte per 4K guest page, indexed by `addr >> 12`, reserved once with `MAP_NORESERVE`: 2^35 bytes = 32 GiB of virtual space, of which only what is touched is used. A guest access becomes shift, one-byte load and test, about 3 instructions.
   - ✅ Verified on the host on 2026-10-12: `overcommit_memory` = 0 and a 32 GiB `mmap` with `MAP_NORESERVE` succeeds.
   - **Every guest fault is an explicit branch with the rip known.** A host SIGSEGV inside translated code is an armor bug. This supersedes two points of `tech/memory.md`: "the JIT relies on the host and handles the exceptions" and self-modifying code via write protection.
   - The same table replaces `BTreeMap` and TLB in the interpreter too. It is **increment 0** of M6, measurable at once.
2. **Translation unit: the basic block.** It ends at a jump or at the first instruction the JIT does not translate: the interpreter runs that one, then execution resumes. This way the JIT is correct from the first increment, with very few instructions. Block chaining comes later.
3. **No IR.** `armor_x86::Op` is already a structured representation: one emitter per `Op`, plus a backward pass per block that computes which flags are needed.
4. **Registers: static mapping.** The 16 x86 general-purpose registers go to callee-saved host registers, plus one reserved for the pointer to the `Cpu`.
   - The state is written back to the `Cpu` at block exit, before every call to a helper and on the slow path of a failed permission check: precise state where needed, including for signal frames.
   - `Cpu` becomes `#[repr(C)]` (offsets with `offset_of!`).
   - `armor-cpu` stays without `unsafe`: the new crate `armor-jit` contains all the `unsafe` and calls `extern "C"` shims around the existing helpers (x87, `float.rs`, strings, CPUID).
5. **Flags:** `rflags` in the `Cpu` stays the architectural state at block boundaries, the same one the interpreter sees. Inside a block only the live flags are computed: host NZCV where the correspondence is direct, helpers for PF and AF. At block exit all flags are considered live, until chaining allows doing better.
6. **Signals and GIL:** a check for pending signals at the entry of every block (one load and one branch); the GIL changes hands after a number of blocks. The GIL stays for the whole first phase of M6: no memory barriers (ADR-0018). LSE atomics are emitted for `LOCK` anyway, and an unaligned `LOCK` goes to a helper. Parallelism and TSO: a future ADR.
7. **Per-page invalidation:** every block registers itself on the guest pages it covers. A write to an executable page goes to the slow path, which invalidates the blocks of that page and exits the current block. The global generation (ADR-0020) stays for the interpreter's cache.
8. **AArch64 encoder written by us**, for the subset that is needed, verified like the x86 decoder: an `xtask` command compares the emitted bytes with those of the host's native GNU assembler (`as` and `objcopy`, binutils 2.44: `cargo xtask encoder-check`).
9. **Verification, on three levels:**
   1. `difftest` runs every case with both engines (`--jit`);
   2. a **lockstep mode**, the debugging tool: for every instruction the `Cpu` is snapshotted, the interpreter executes it while a `GuestMemory` records the bytes written, the writes are undone, the translation of that single instruction is run from the same snapshot, and `Cpu` and writes are compared;
   3. the encoder check against objdump.

**Postponed, and written down as such:**
- traces;
- SMC through host protection;
- floating point in generated code (initially with helpers);
- parallelism and TSO;
- the choice between W^X with double mapping and `mprotect` (to be made in `tech/jit.md` at the first increment).

Details in `tech/jit.md`.

## Implementation status
Intermediate stages, chosen to have an exact state at every instruction while the translator grows. The decision remains the one above: every stage will be replaced by a measured increment (`tech/jit.md`).

Increment 3 (2026-10-12, note added):
- §4: the guest's registers stay in memory in the `Cpu`, loaded and written back at every instruction. Since increment 13 (2026-10-13) 12 registers out of 16 live in host registers; r8–r11, rarely used, stay in the `Cpu`;
- §5: the status flags are computed after every instruction that writes them, not only the live ones. Since increment 9 (2026-10-13) they are lazy inside the block; since increment 12 also across blocks (code of the pending operation in a register, computation at the first reader or at the exit to the engine). §5 planned "live" flags with a backward pass: this is a different technique (lazy flags) with the same purpose;
- §6: signals and GIL are checked every ~10,000 instructions, as in the interpreter, not at the entry of every block: without chaining, every block returns to the engine anyway. Since increment 5 (chaining) every block subtracts its instructions from a budget at entry and returns to the engine when it runs out: pending signals are delivered then, within ~10,000 instructions. Since increment 16 (2026-10-13) instructions with `LOCK` are translated as normal read-modify-writes, not with LSE atomics: with the GIL and without writable memory shared between processes it is exact; to be revisited together with parallelism. With `MAP_SHARED` (ADR-0022, same day), as soon as writable shared memory exists the JIT no longer translates them and the interpreter executes them with a host compare-and-swap. With ADR-0023 (parallel threads) the same holds throughout concurrent mode, and §6 on GIL and parallelism is replaced by that ADR;
- §7: invalidation is global (code generation, ADR-0020), not per page.
- Postponed, "floating point in generated code (initially with helpers)": since increment 17 (2026-10-13) scalar SSE is translated directly, without helpers; the cases with NaNs and out-of-range conversions go to the interpreter. x87 and packed floating-point SSE stay in the interpreter.

## Alternatives considered
- **Host protection instead of software checks:** impossible at 4K on a 16K host without false positives on almost every binary (ADR-0009). It could be useful on a 4K host, as an optimisation.
- **An IR (FEX style):** better optimisations, but a large extra layer to write and verify before seeing the first fast block. It can be introduced later, if benchmarks justify it.
- **dynasm-rs or another assembler as a dependency:** less code, but a dependency (ADR-0008) for a subset that can be written and verified against objdump.
- **Traces instead of basic blocks:** faster on hot loops, but more complex to invalidate; after chaining.

## Consequences
- \+ Precise faults without complex SIGSEGV handlers; the same permission model for interpreter and JIT.
- \+ The JIT can grow one instruction at a time, with the interpreter as safety net and as oracle (lockstep).
- − One permission check per memory access (about 3 instructions).
- − 32 GiB of reserved virtual space (not memory): to be documented in `environment.md`.
- − New `unsafe` code (execution of generated code, executable buffers, instruction cache flush), confined in `armor-jit`.
