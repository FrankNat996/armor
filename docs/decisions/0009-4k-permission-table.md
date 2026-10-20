# 0009 — 4K pages in M1: software permission table, host pages always RW

- **Status:** Accepted (2026-10-12); superseded by 0046 on hosts with 4K pages
- **Date:** 2026-10-12

## Context
ADR-0005 requires the guest to see 4K pages on a 16K host. In M1 it turned out that even the simplest binary (`basic/exit42`) has segments with different permissions (R, R E) within the same 16K host page (`tech/memory.md`). The complete strategy also concerns file mapping, `mprotect` and the JIT, but the M1 interpreter needs only part of it.

## Decision
In `armor-mem`:
- **the host pages** that contain guest memory are always mapped `PROT_READ | PROT_WRITE`, never executable;
- **the guest's permissions** live in a software table with one entry per 4K page (`BTreeMap<page number, Prot>`);
- every interpreter access goes through `GuestMemory::{read, write, fetch}`, which check the table:
  - read allowed with any permission other than `NONE` (on x86 there are no write-only or execute-only pages ⚠️);
  - write only with `WRITE`;
  - fetch only with `EXEC`;
- fixed-address mappings use `MAP_FIXED_NOREPLACE`, so a collision with armor's memory becomes an error instead of overwriting it;
- a new mapping over pages already belonging to the guest replaces them and zeroes them, like `mmap(MAP_FIXED)`.

## Alternatives considered
- **Host permissions = union of the permissions of the 4 subpages:** really needed only by the JIT, which runs code directly on the host. With the interpreter it gives no advantage and complicates the code.
- **Requiring a 4K kernel:** excluded by ADR-0005.

## Consequences
- \+ Exact 4K permissions, verified by the `faults/*` tests against qemu.
- \+ All the `unsafe` memory access code is in a single crate.
- − Every access consults a `BTreeMap`: it is slow, but acceptable for the interpreter. To be revisited with the JIT (M6) or if benchmarks require it. *Update of 2026-10-12:* the `BTreeMap` became a flat table, one byte per page (ADR-0021, JIT increment 0); the model (permissions in software, host pages always RW) does not change.
- − The rest remained open: file mappings with offsets not aligned to 16K, `MAP_SHARED`, partial `mprotect`, JIT. Resolved by ADR-0012, 0022 and 0021 (closed on 2026-10-13).
