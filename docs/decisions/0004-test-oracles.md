# 0004 — qemu-x86_64 and objdump as test oracles

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
There is no x86 hardware on the host. An automatic reference is needed to verify decoding and execution. Both `qemu-user` and `binutils-x86-64-linux-gnu` are available in Debian 13 (`environment.md`).

## Decision
- **Decoder:** comparison with `x86_64-linux-gnu-objdump`.
- **Execution:** comparison with `qemu-x86_64` (registers, masked flags, memory, output, exit code).
- Details in `testing.md`.

## Alternatives considered
- **Hand-written tests only:** they do not scale to the thousands of combinations of instructions and operands.
- **Real x86 hardware:** not available on the host. It remains an optional oracle.

## Consequences
- \+ Stability becomes measurable from day one.
- − qemu can be wrong: in case of disagreement the SDM decides, and discrepancies must be documented.
- − Undefined flags require per-instruction masks (`tech/x86-64-flags.md`).
- qemu is GPL-2.0: it is run, not copied (`references.md`).
