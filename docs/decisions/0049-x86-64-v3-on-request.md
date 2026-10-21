# 0049 — x86-64-v3: interpreter first, declared on request

- **Status:** Proposed
- **Date:** 2026-10-22

## Context
Roadmap M11 asks for AVX and AVX2. Software built for x86-64-v3 (RHEL 10, Ubuntu's `amd64v3` variant, games that need AVX) uses its instructions without checking CPUID, so it stops under armor at the first VEX instruction (`tech/avx.md`, survey). v3 is a set: AVX, AVX2, BMI1, BMI2, F16C, FMA, LZCNT, MOVBE and OSXSAVE, with XCR0 enabling the AVX state (x86-64 psABI). Declaring part of it, or declaring it before it all works, makes glibc and programs choose code paths armor cannot run, and changes the paths `difftest` compares.

## Decision
- The features of v3 are implemented first in the interpreter, the JIT leaving them to it (as for packed SSE floating point), so they are correct before they are fast.
- They are declared in CPUID only on request, by `ARMOR_CPU_FEATURES` with qemu's feature names (`+xsave,+avx,…`). `difftest` adds the same list to qemu's `-cpu` model, from its environment and per case (`CASE_FEATURES`), so both sides see the same CPU. Without the variable the CPU model is unchanged.
- Instructions of a feature that is not declared raise #UD, as on a CPU without it; `tzcnt`/`lzcnt` then run as `bsf`/`bsr`.
- Where qemu and the Intel SDM disagree (MXCSR with the AVX component alone in `xsave`/`xrstor`; masked loads at the end of a mapping), armor follows the SDM; the tests leave those cases out and `tech/avx.md` lists them.
- v3 is declared by default only when its hot instructions are translated by the JIT and the cost of `xsave` on the dynamic loader's hot path is measured.

## Alternatives considered
- **Declaring each feature as soon as it works:** glibc's v3 variants need the whole set, and declaring BMI before AVX matches no real CPU (FEX declares them together).
- **Translating in the JIT first:** faster sooner, but every form would need its own NEON translation before any test could run; the interpreter reuses the SSE implementation for the VEX forms.
- **qemu's behaviour where it differs from the SDM:** the comparison would be simpler, but armor would reproduce qemu's errors, as qemu's DE and NaN errors were not reproduced for SSE (`sse/mxcsr_hardware`).

## Consequences
- Every instruction of v3 runs when declared, and `ld.so` then reports `x86-64-v3 (supported)`; the default behaviour, and its measurements, do not change.
- With the features declared, VEX code is interpreted: slow until the JIT translates it.
- The cases that need features cannot run on the hardware oracle, which has none of them; the SDM-versus-qemu differences need an x86-64 machine with AVX2 to be verified.
