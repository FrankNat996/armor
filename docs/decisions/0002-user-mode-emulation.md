# 0002 — User-mode emulation Linux x86_64 → Linux AArch64

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
The goal is to run x86_64 programs on a Raspberry Pi 5 simply and with acceptable performance.

## Decision
armor emulates **user space only**:
- guest: Linux x86_64 ELF executables;
- host: Linux AArch64;
- the guest's syscalls are translated and passed to the host kernel.

## Alternatives considered
- **Full-system emulation** (in the style of `qemu-system-x86_64`): much slower and more complex, because it needs a guest kernel and devices. Not needed for the goal.
- **Support for 32-bit x86 as well:** postponed; added on 2026-10-13 by ADR-0029.

## Consequences
- \+ The host's kernel, file systems and drivers are reused.
- − An accurate translation of syscalls and structs is needed (`tech/syscalls.md`).
- − The guest shares the address space with armor (`tech/memory.md`).
