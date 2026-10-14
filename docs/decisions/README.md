# Architecture decisions (ADR)

Every significant decision has its own file `NNNN-title.md`. ADRs are never deleted: when a decision changes, a new ADR is written and the old one becomes "Superseded by NNNN".

## Statuses
- **Proposed:** written, not yet accepted.
- **Accepted:** in force, with the date.
- **Rejected:** evaluated and discarded (kept as a record of why).
- **Superseded by NNNN:** replaced by a later ADR.

## Index
| # | Title | Status |
|---|---|---|
| [0001](0001-language-rust.md) | Language: Rust | Accepted (2026-10-12) |
| [0002](0002-user-mode-emulation.md) | User-mode emulation Linux x86_64 → Linux AArch64 | Accepted (2026-10-12) |
| [0003](0003-interpreter-before-jit.md) | The interpreter first, then the JIT | Accepted (2026-10-12) |
| [0004](0004-test-oracles.md) | qemu-x86_64 and objdump as test oracles | Accepted (2026-10-12) |
| [0005](0005-4k-guest-pages-on-16k-hosts.md) | The guest sees 4K pages even on 16K hosts | Accepted (2026-10-12) |
| [0006](0006-license.md) | License: MIT OR Apache-2.0 | Accepted (2026-10-12) |
| [0007](0007-project-name.md) | Project name: armor | Accepted (2026-10-12) |
| [0008](0008-dependencies.md) | Dependencies: `libc` for host syscalls, our own ELF parser | Accepted (2026-10-12) |
| [0009](0009-4k-permission-table.md) | 4K pages in M1: software permission table, host pages always RW | Accepted (2026-10-12) |
| [0010](0010-cpuid-model.md) | CPUID: qemu64 model reduced to the implemented features | Accepted (2026-10-12) |
| [0011](0011-dynamic-linking-rootfs.md) | Dynamic linking: the real x86-64 `ld.so` from a guest rootfs; M5 brought forward | Accepted (2026-10-12) |
| [0012](0012-file-mmap-by-copy.md) | File `mmap`: copy into anonymous memory (4K on 16K hosts) | Accepted (2026-10-12); superseded by 0022 for `MAP_SHARED` |
| [0013](0013-guest-root.md) | Guest root: `ARMOR_ROOT`, with the same resolution as `qemu -L` | Accepted (2026-10-12) |
| [0014](0014-rootfs-from-debian-packages.md) | Guest root for the tests: pinned Debian amd64 packages (`fetch-rootfs`) | Accepted (2026-10-12) |
| [0015](0015-scalar-sse-floating-point.md) | Scalar SSE with Rust's `f32`/`f64` and x86 rules by hand; default MXCSR only | Accepted (2026-10-12); superseded by 0024 for MXCSR and for the interpreter |
| [0016](0016-own-x87-softfloat.md) | x87: 80-bit arithmetic written by us, verified against qemu | Accepted (2026-10-12) |
| [0017](0017-signals-through-the-host-kernel.md) | Signals: delivered by the host kernel, x86_64 frame built by armor | Accepted (2026-10-12) |
| [0018](0018-threads-with-a-global-lock.md) | Threads: 1:1 with the host, only one thread at a time runs guest code (GIL) | Superseded by 0023 for code execution (2026-10-13) |
| [0019](0019-processes-fork-exec.md) | Processes: host `fork`, `execve` that relaunches armor | Accepted (2026-10-12) |
| [0020](0020-instruction-cache.md) | Cache of decoded instructions, invalidated by a code generation | Accepted (2026-10-12) |
| [0021](0021-jit-design.md) | AArch64 JIT design: permissions in a flat table, basic blocks, no IR, lockstep with the interpreter | Accepted (2026-10-12) |
| [0022](0022-map-shared.md) | `MAP_SHARED`: real host shared mappings, in whole host pages | Accepted (2026-10-13) |
| [0023](0023-parallel-threads-tso.md) | Parallel threads and the TSO memory model (LDAPR/STLR, concurrent mode) | Accepted (2026-10-13) |
| [0024](0024-full-mxcsr.md) | Full MXCSR: SSE in software in the interpreter, hardware flags corrected in the JIT, DE as per the SDM | Accepted (2026-10-13) |
| [0025](0025-hardware-oracle.md) | Third oracle: a real x86-64 machine over SSH (`difftest --oracle`) | Accepted (2026-10-13) |
| [0026](0026-undeclared-instructions.md) | Instructions from extensions that CPUID does not declare: #UD (SIGILL) | Accepted (2026-10-13) |
| [0027](0027-guest-isolation.md) | Guest isolation: namespaces, seccomp and limits, on request (`--sandbox`) | Accepted (2026-10-13) |
| [0028](0028-compatibility-manager.md) | Compatibility Manager: automatic path (JIT by default), per-user profiles, diagnostics | Accepted (2026-10-13) |
| [0029](0029-32-bit-x86.md) | 32-bit x86 guests (i386): identity below 4 GiB, interpreter then JIT, `int 0x80` syscalls | Accepted (2026-10-13) |
| [0030](0030-x87-transcendentals.md) | Correctly rounded x87 transcendentals | Accepted (2026-10-13) |
| [0031](0031-steam-root.md) | Guest root for Steam: amd64 and i386 in the same tree (`fetch-rootfs --steam`) | Superseded by 0034 (2026-10-14) |
| [0032](0032-integrate-existing-components.md) | Integrate the existing components (Steam ARM64, Proton ARM64, FEX, box64): armor as glue, test bench and engine where needed | Accepted (2026-10-14) |
| [0033](0033-installing-applications.md) | `armor install`: installing x86 Linux and Windows applications on ARM64, one root per application, Wine ARM64 + FEX for Windows | Accepted (2026-10-14) |
| [0034](0034-priorities-an-application-for-everyone.md) | Priorities: armor as an application for everyone; roadmap M7–M10 redone | Accepted (2026-10-14) |
| [0035](0035-native-gtk-window.md) | The window: a native GTK 4 application (`armor-gui`), outside the engine's workspace | Accepted (2026-10-14) |
| [0036](0036-live-signed-package-indexes.md) | Linux applications: Debian's live indexes, verified by signature, not pinned | Accepted (2026-10-14) |
| [0037](0037-chromium-applications-in-m8.md) | Chromium-based applications move from M7 to M8 | Accepted (2026-10-14) |
| [0038](0038-steam-installed-by-armor.md) | Steam installed by armor: Valve's ARM64 client from Valve's manifest | Accepted (2026-10-15) |
| [0039](0039-armor-updates-itself.md) | armor updates itself from its releases (a notice and a button, pre-releases included) | Accepted (2026-10-15) |
| [0040](0040-steam-proton-for-windows-programs.md) | Windows programs run with Steam's ARM64 Proton when Steam is installed | Accepted (2026-10-15) |

## Template
```markdown
# NNNN — Title

- **Status:** Proposed | Accepted (YYYY-MM-DD) | Rejected | Superseded by NNNN
- **Date:** YYYY-MM-DD

## Context
What the problem is, what constraints there are.

## Decision
What is done.

## Alternatives considered
What was discarded and why.

## Consequences
What becomes easier, what becomes harder, what remains open.
```
