# 0046 — Permission checks by the host's MMU on hosts with 4K pages

- **Status:** Accepted (2026-10-20)
- **Date:** 2026-10-20

## Context
ADR-0009 keeps the guest's permissions in a software table, with every host page that holds guest memory mapped read-write; ADR-0021 made the table flat, and translated code checks it before each guest access. The check is 5 AArch64 instructions on the hot path, one of them a load from the table that the access depends on (`Translator::check`):
```
ands xzr, x7, #~(2^47 - 1)    ; beyond the guest's half: slow path
b.ne slow
lsr  x10, x7, #12
ldrb w10, [x1, x10]           ; the page's byte
tbz  w10, #bit, edge          ; not allowed (or the next page not allowed)
```
Measured on 2026-10-15 (`tech/performance.md`, "7-Zip"): about 9% of 7-Zip's time under the JIT, and a `push` is 10 instructions, half of them the check. box64 and FEX do no check at all: guest accesses go straight to memory, and the guest can read or write the engine's own memory.

ADR-0009 had a reason that still holds: on hosts with 16K or 64K pages (ADR-0005, ADR-0042) a host page holds 4 or 16 guest pages with different permissions, so the host cannot enforce them. On hosts with 4K pages, host and guest pages coincide (the Pi 5 since 2026-10-14, most ARM64 servers, the GitHub runner), and the host's MMU can do the work for free.

Two problems the software table also solves today:
- **armor's own memory:** guest and armor share one address space with identity mapping (`tech/memory.md`). A wild guest pointer into armor's heap must give the guest SIGSEGV, as on x86, not read or corrupt armor. On a 48-bit host armor's memory is above 2^47, out of the guest's reach, and the first instruction pair above already sends such addresses to the slow path. On a 39-bit host (the Pi's 4K kernel) the kernel places both in the same 512 GiB.
- **self-modifying code:** a write to a guest page that holds translated code must throw the translations away (ADR-0020, ADR-0021); today every write to an executable page goes to the interpreter.

## Decision
On hosts whose page size is 4K, armor enforces the guest's page permissions with the host's page protections, and translated code no longer reads the permission table. Hosts with 16K and 64K pages keep ADR-0009 as it is.

1. **Host protections follow the guest's**, without execute (the guest's code never runs in place): `PROT_NONE` for an inaccessible guest page, read-only for R and R+X, read-write for R+W. A page that is R+W+X is read-only while it holds translated code and read-write otherwise (point 4). The software table stays the source of truth: the interpreter, the syscalls and the slow paths keep using it, and every change to it (`mmap`, `mprotect`, `munmap`, `brk`, `mremap`, the loader) also sets the host protections.
2. **A guest-only region.** At startup armor reserves the guest's part of the address space with an inaccessible `MAP_NORESERVE` mapping: below 2^47 on hosts with 48 bits or more, the lower half (below 2^38) on 39-bit hosts. armor's own later mappings, which the kernel places, then land above it; the guest's mappings go inside it at addresses armor chooses (as already for i386 guests and for 48-bit hosts, `set_ceiling`), and unmapping a guest range puts the reservation back instead of leaving a hole the kernel could give to armor. Translated code keeps one test, the address against the region's limit (`ands` + `b.ne`, no load).
3. **Faults in translated code go back to the guest instruction.** armor installs a handler for SIGSEGV and SIGBUS. A fault whose PC is in the code buffer is looked up in the block's table of accesses (host offset of each access → its slow path, recorded by the translator) and resumed at that slow path: the interpreter runs the instruction again from the exact guest state, as for a failed software check today, and raises the guest's signal or does the access. Any other fault is armor's own: the default action, as today. The translator guarantees that no guest-visible state changes before the last access of an instruction that can fault; lockstep and the `faults/*` cases check it.
4. **Self-modifying code through the MMU.** When the JIT translates code from a guest page that is also writable, it makes the host page read-only. A write to it then faults (translated code) or is seen by the software table (the interpreter, the syscalls that write guest memory): the translations are thrown away as today, the page becomes read-write again, and the write is done by the interpreter. Writes to pages without translations no longer leave translated code.
5. **Stores that cross a page boundary** keep a cheap test without a load (the offset within the page, out of line near the end of a page), because AArch64 may write part of an unaligned store whose second page faults, where x86 writes nothing. ⚠️ Whether the Cortex-A76 does is to be measured; if the architecture allows it, the test stays whatever one CPU does.
6. **An option to compare:** `ARMOR_PERMISSIONS=table` keeps the software checks on a 4K host, so that the chain runs both paths and `bench` measures the difference.

## Alternatives considered
- **No check at all, as box64 and FEX:** the fastest, but a wild guest pointer would read or corrupt armor, and guest faults on armor's addresses would not happen. armor's exactness (the `faults/*` cases, the hardware oracle) is worth the one test of point 2.
- **Translated code with a guest memory base in a register** (each address plus a base, as qemu's guest base): it separates guest and armor without a reservation, but adds an instruction to every access and breaks the identity mapping the syscalls rely on.
- **Keeping the table and caching it better:** increment 18 already reduced the check to the 5 instructions above; the dependent load remains.
- **Doing it on 16K and 64K hosts too:** impossible with mixed permissions in one host page (ADR-0009); those hosts keep the table.

## Consequences
- \+ About 3 instructions and a dependent load less on every guest access of translated code; a `push` from 10 to about 6 instructions (with item 5 of M11, `push` as one store with writeback, fewer). Expected: most of the 9% measured on 7-Zip; to be measured with `cargo xtask bench`.
- \+ Writes to writable pages near code, and to pages that were executable but hold no translation, stay in translated code.
- − Two ways of checking, chosen by the page size: both stay in the chain (the 16K and 64K virtual machines, `ARMOR_PERMISSIONS=table`).
- − On 39-bit hosts the guest gets 256 GiB of address space instead of 512.
- − A host signal handler in armor for SIGSEGV and SIGBUS, which must stay async-signal-safe (a lookup in tables built before the code runs, no allocation).
- − Syscalls that write guest memory must make a read-only code page writable first (point 4), or the host kernel answers EFAULT where x86 Linux succeeds.
