# 0025 — Third oracle: a real x86-64 machine over SSH

- **Status:** Accepted (2026-10-13)
- **Date:** 2026-10-13
- Extends ADR-0004 (qemu-x86_64 and objdump as oracles).

## Context
qemu is the oracle of `difftest`, but it is not the truth: where it departs from the Intel SDM armor follows the SDM (`testing.md`). Several of these choices were ⚠️, because they were remembered or derived by analogy, and some were wrong. What was missing was "an x86_64 PC as a third oracle", as `testing.md` said.

The available machine (verified on 2026-10-13 with `/proc/cpuinfo`):
- Intel Xeon X5675 (Westmere, family 6 model 44), SSE up to SSE4.2, POPCNT, AES, PCLMULQDQ, **no AVX**;
- Ubuntu 24.04, glibc 2.39, 4K pages, Proxmox kernel 7.0;
- no compiler: programs are compiled on the Pi (static) and copied over.

## Decision
- **`cargo xtask difftest --oracle`** compares armor (interpreter and JIT) with the machine instead of with qemu (`xtask/src/oracle.rs`). It copies the programs with `scp` into `/tmp/armor-oracle`, runs them with `ssh` under `timeout`, and reads the exit status from a marker on stderr (`ssh` alone does not distinguish an exit code from a signal).
- **Static programs only:** assembly, `c/`, `c-pie/` and busybox. The dynamic ones depend on the Debian root (ADR-0014), and an already-closed stdout does not cross `ssh`.
- **No data about the machine in the repository:** destination and key come from the variables `ARMOR_ORACLE` (`user@host`) and `ARMOR_ORACLE_KEY`. The key is dedicated (`~/.ssh/armor_oracle`, outside the repository).
- **qemu stays the oracle of `difftest`**, which must always run everywhere; the hardware oracle serves to settle controversial cases and to verify the ⚠️ items. A difference between armor and the hardware is an armor error, except for the implementation-dependent behaviours documented in `testing.md` (for example `rcpps`, CPUID, auxv).
- **Every ⚠️ verified** on the hardware becomes ✅ with the date and "Xeon X5675" as proof.

## Alternatives considered
- **Only qemu and the SDM:** as it was. Some choices based on the SDM turned out to be wrong on hardware (C1 in a partial `fprem`, the reserved fields of the x87 environment): without the hardware they would have stayed.
- **Golden files generated on the hardware and versioned** : useful for running without the machine, but today the container is available and the tests change often. It can be done later, starting from `--oracle`.
- **A compiler on the machine:** not needed, static binaries are enough.

## Consequences
- \+ Controversial cases are decided by a fact, not by a reading of the SDM.
- \+ The M4 extensions (SSE3–SSE4.2) can be verified on a CPU that has them.
- − The container is temporary (it will be removed): the verifications done stay in the documents, dated; without the machine `--oracle` cannot be rerun.
- − A single Intel CPU: behaviours that vary between vendors (Intel and AMD) remain implementation-dependent. armor's CPUID says AMD (ADR-0010).
