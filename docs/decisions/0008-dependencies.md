# 0008 — Dependencies: the `libc` crate for host syscalls, our own ELF parser

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
In M1 armor must call host syscalls that Rust's standard library does not offer, for example `mmap` at a fixed address. Later it will also have to forward the guest's syscalls with arbitrary arguments. It must also read ELF64 headers and program headers of the guest binaries. Until this decision the policy was: zero dependencies.

## Decision
- **Host syscalls: the `libc` crate.**
  - `libc::syscall` is used to forward the guest's syscalls;
  - the crate also provides constants and structs of the aarch64 host.
  - It is used only in the crates where `unsafe` is allowed (`armor-mem`, `armor-linux`, `armor-jit`), encapsulated in functions with a `// SAFETY:` comment.
- **ELF parsing: a parser written by us** in `armor-elf`, safe (`#![forbid(unsafe_code)]`) and without dependencies.
- Every **other** external dependency requires a new ADR, with a license check (it must be compatible with `MIT OR Apache-2.0`, ADR-0006).

## Alternatives considered
- **`rustix`:** safe wrappers, but it brings other dependencies and does not expose a generic raw syscall, which an emulator often needs.
- **Direct `asm!` (`svc #0`):** zero dependencies, but all the host's constants and structs would have to be defined by hand.
- **The `object` crate for ELF:** mature and complete, but for M1 a few hundred lines are enough. It can be reconsidered for symbols and relocations (M5).

## Consequences
- \+ A single external dependency, very widespread, with license `MIT OR Apache-2.0` ✅ (`license` field of the `Cargo.toml` of `libc` 0.2.190, verified on 2026-10-12 in cargo's local registry). Used by `armor-mem`, `armor-linux` and `armor-jit`.
- \+ The ELF parser stays entirely safe and testable with unit tests.
- − Calls to `libc` are `unsafe`: they must be kept in small, documented wrappers.
- The **guest's** x86_64 structs must still be defined by us, because `libc` describes only the host (`tech/syscalls.md`).
