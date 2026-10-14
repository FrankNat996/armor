# 0003 — The interpreter first, then the JIT

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
A JIT is indispensable for performance, but it is hard to write and to debug. At the start, correctness and development speed matter.

## Decision
- An **interpreter** is written first (M1–M5). The **AArch64 JIT** arrives in M6 (`roadmap.md`).
- Decoder shared between the two (`architecture.md`).
- The interpreter stays as a fallback and as an internal oracle for testing the JIT.

## Alternatives considered
- **JIT from the start:** performance first, but every semantic bug gets mixed up with code generation bugs.

## Consequences
- \+ Semantics verifiable instruction by instruction, simple debugging.
- \+ The JIT can be tested differentially against the interpreter, as well as against qemu.
- − The interpreter is slow: large programs will be barely usable until M6.
- − Risk that the interpreter's structure does not suit the JIT. Mitigation: shared decoder and the decision on the IR taken before M6.
