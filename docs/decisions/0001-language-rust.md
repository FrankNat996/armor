# 0001 — Language: Rust

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
The alternatives considered were C and Rust. In a binary translator most bugs are in the ELF loader, in syscall translation and in guest memory management, that is, in the code that manipulates pointers and struct layouts.

## Decision
The project is written in **Rust** (edition 2024).

## Alternatives considered
- **C:** total control, and it is the same language as box64, so useful for comparing code. Discarded in favour of Rust's memory safety.

## Consequences
- \+ Memory safety in the pure crates (decoder, ELF parser) and `unsafe` confined and documented (`conventions.md`).
- \+ `cargo test` built in for unit tests and harnesses.
- − The JIT, guest memory accesses and signal handlers remain inherently `unsafe`.
- − Compile times on the Pi 5 are longer than in C.
- Was left open: toolchain from Debian or from rustup. Closed on 2026-10-12: rustup, stable channel (`conventions.md`).
