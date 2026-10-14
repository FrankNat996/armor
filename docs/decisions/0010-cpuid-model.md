# 0010 — CPUID: qemu64 model reduced to the implemented features

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
glibc chooses the variants of its functions based on CPUID. If armor declared features it does not implement (AVX2, …), programs would stop. If it declared features different from those qemu sees in `difftest`, glibc would take different paths under the two and the comparison would not verify the same instructions.

## Decision
- armor reproduces the CPUID values of qemu's `qemu64` model **without** SSE3, CX16, LAHF-LM and SVM, measured leaf by leaf (`tech/cpuid.md`).
- `difftest` launches qemu with `-cpu qemu64,-sse3,-cx16,-lahf-lm,-svm` (constant `QEMU_CPU`).
- Two exceptions, so that armor does not pass itself off as qemu:
  - the CPU name (brand string) is "armor virtual x86-64 CPU (qemu64 features)";
  - the hypervisor leaves 0x4000_0000 are empty.
- Every time a new extension is implemented, its bit is turned on and removed from the list of `-` (procedure in `tech/cpuid.md`).

## Update of 2026-10-13: CX16 and LAHF-LM declared
Update of 2026-10-13: enable the two features to implement `cmpxchg16b` and `lahf`/`sahf` in 64-bit mode (roadmap M3, remaining integer instructions).
- armor now declares CX16 (leaf 1, ecx bit 13) and LAHF-LM (leaf 0x8000_0001, ecx bit 0). Before changing the values, the only two bits that differ between the two models were measured under qemu (`tech/cpuid.md`).
- `QEMU_CPU` becomes `qemu64,-sse3,-svm`.
- Verification: `difftest` 240/240 with both engines and in lockstep with the new model; glibc and the programs of the guest root do not change behaviour.

## Update of 2026-10-13: SSE3 declared (M4)
With the procedure of this ADR, within the completion of M4: armor implements SSE3 and declares it (leaf 1, ecx bit 0); `QEMU_CPU` becomes `qemu64,-svm`. Between the two qemu models only that bit changes (measured with `basic/cpuid`). Verification: `difftest` 269/269.
- Then SSSE3 (leaf 1, ecx bit 9): `QEMU_CPU` = `qemu64,-svm,+ssse3`; only that bit changes. Verification: `difftest` 270/270.
- Then SSE4.1 (leaf 1, ecx bit 19): `QEMU_CPU` = `qemu64,-svm,+ssse3,+sse4.1`; only that bit changes. Verification: `difftest` 271/271.
- Then SSE4.2 and POPCNT (leaf 1, ecx bits 20 and 23): `QEMU_CPU` = `qemu64,-svm,+ssse3,+sse4.1,+sse4.2,+popcnt`; only those two bits change. Verification: `difftest` 272/272.

## Alternatives considered
- **Values invented from scratch** (vendor "GenuineIntel", family and model of our choosing): not verifiable against an oracle, and glibc takes different paths for Intel and AMD.
- **Also copying qemu's brand and hypervisor leaves:** simpler comparison, but armor would declare that it is qemu/TCG.
- **A CPU with AVX2:** better performance in the future, but it requires implementing AVX/AVX2 before glibc starts.

## Consequences
- \+ glibc uses only baseline + SSE2, already implemented.
- \+ glibc's behaviour is identical under armor and qemu, including the choice of the memcpy/memset thresholds, which depend on the cache leaves 0x8000_0005/6.
- − Vendor "AuthenticAMD": programs that behave differently on Intel and AMD follow the AMD path.
- − The `basic/cpuid` test cannot compare brand and hypervisor leaves.
