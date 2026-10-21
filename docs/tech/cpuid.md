# CPUID: the CPU that armor presents to the guest

**Decision:** ADR-0010. **Code:** `crates/armor-cpu/src/cpuid.rs`. **Tests:** `basic/cpuid` (difftest), unit tests in `cpuid.rs`.

## Why it matters
glibc reads CPUID at startup (`init_cpu_features`, `init_cacheinfo`) and, based on the declared features, chooses which variants of the string and memory functions to use (ifunc: sse2, avx2, evex, …). armor must therefore:
- **declare only what it implements**, otherwise glibc would execute AVX instructions and armor would stop;
- **declare the same things that qemu sees** in `difftest`, otherwise glibc would take different paths under the two and the comparison would be meaningless.

## Chosen model
✅ Measured on 2026-10-12 by running `cpuid` under `qemu-x86_64 10.0.13`, for leaves 0–0x20 (subleaves 0–3), 0x4000_0000–0x4000_0010 and 0x8000_0000–0x8000_0020 (subleaves 0–1).

The reference is the `qemu64` model without the features that armor does not implement:
```
-cpu qemu64,-svm,+ssse3,+sse4.1,+sse4.2,+popcnt
```
Since 2026-10-13 (M4) armor also declares SSE3, SSSE3, SSE4.1, SSE4.2 and POPCNT: the model was `qemu64,-sse3,-svm`. Between one model and the next only the feature bits in ecx of leaf 1 change (bit 0 SSE3, bit 9 SSSE3, bit 19 SSE4.1, bit 20 SSE4.2, bit 23 POPCNT; ✅ measured with `basic/cpuid` under qemu 10.0.13).
Until 2026-10-12 CX16 and LAHF-LM were also removed (`-cx16,-lahf-lm`). Since 2026-10-13 armor declares them (ADR-0010, update): it implements `cmpxchg16b` and `lahf`/`sahf`. ✅ Measured under qemu 10.0.13 by comparing the two models leaf by leaf (same program, leaves 0, 1, 5, 7, 0xb, 0xd, 0x8000_0000–0x8000_000a). Only leaf 1 ecx (0x80000000 → 0x80002000, bit 13) and leaf 0x8000_0001 ecx (0 → 1, bit 0) change.
It is the constant `QEMU_CPU` in `xtask/src/main.rs`: `difftest` and `run` always launch qemu with this model.

| Leaf | eax | ebx | ecx | edx | Notes |
|---|---|---|---|---|---|
| 0 | 0x0000000d | 0x68747541 | 0x444d4163 | 0x69746e65 | max base leaf 0xd, "AuthenticAMD" |
| 1 | 0x00060fb1 | 0x00000800 | 0x80982201 | 0x078bfbfd | edx: FPU…CMOV, MMX, FXSR, SSE, SSE2; ecx: SSE3, SSSE3, CX16, SSE4.1, SSE4.2, POPCNT and "hypervisor" |
| 5 | 0 | 0 | 3 | 0 | |
| 0xb | 0 | 1 / 1 / 0 | 0x100 / 0x201 / n | 0 | subleaf 0 / 1 / n≥2 |
| 0x8000_0000 | 0x8000000a | as leaf 0 | | | |
| 0x8000_0001 | 0x00060fb1 | 0 | 1 | 0x2193fbfd | LAHF-LM; SYSCALL, NX, LM |
| 0x8000_0002–4 | brand string | | | | **different from qemu**, see below |
| 0x8000_0005 | 0x01ff01ff | 0x01ff01ff | 0x40020140 | 0x40020140 | L1 cache |
| 0x8000_0006 | 0 | 0x42004200 | 0x02008140 | 0x00808140 | L2/L3 cache |
| 0x8000_0008 | 0x00003028 | 0 | 0 | 0 | 48-bit virtual addresses, 40-bit physical |
| 0xd | 0 | 0 | 0 | 0 | XSAVE: only with `+xsave` declared (below) |
| others up to 0xd and 0x8000_000a | 0 | 0 | 0 | 0 | as qemu |
| past 0xd or 0x8000_000a | leaf 0xd | | | | as qemu: the highest basic leaf, with the subleaf in ecx ✅ (qemu 10.0.13 with `+xsave`, `tech/avx.md`); zero while leaf 0xd is |

Features **not** declared, and therefore never used by glibc: AVX/AVX2/AVX-512, BMI1/2, MOVBE, AES, PCLMULQDQ, XSAVE and OSXSAVE (so no `xgetbv`).

## Features declared on request (`ARMOR_CPU_FEATURES`)
Features on the way to x86-64-v3 that armor implements but does not declare by default (`tech/avx.md`): `ARMOR_CPU_FEATURES=+xsave` adds them, named as in qemu's `-cpu` (`cpuid::Features`). `cargo xtask difftest` and `run` add the variable's features to qemu's model too, and the cases that need some declare them in `CASE_FEATURES` (`xtask/src/difftest.rs`).

| Feature | What changes | Measured against |
|---|---|---|
| `xsave` | leaf 1 ecx \|= XSAVE (bit 26), OSXSAVE (27): 0x8c982201; leaf 0xd subleaf 0 (3, 0x240, 0x240, 0), subleaf 1 (0, 0x240, 0, 0); XCR0 = 3 | ✅ `qemu64,...,+xsave` (`basic/cpuid`, `xsave/xsave`) |
| `avx` (with `xsave`) | leaf 1 ecx bit 28: 0x9c982201; leaf 0xd subleaf 0 (7, 0x340, 0x340, 0), subleaf 1 (0, 0x340, 0, 0), subleaf 2 (0x100, 0x240, 0, 0); XCR0 = 7 | ✅ `qemu64,...,+xsave,+avx` (CPUID program, `basic/cpuid`) |
| `bmi1` | leaf 7 ebx bit 3; `tzcnt` counts zeros | ✅ `qemu64,...,+bmi1` (CPUID program, `bmi/bmi`) |
| `bmi2` | leaf 7 ebx bit 8 | ✅ `qemu64,...,+bmi2` |
| `abm` | leaf 0x8000_0001 ecx bit 5 (LZCNT); `lzcnt` counts zeros | ✅ `qemu64,...,+abm` |
| `movbe` | leaf 1 ecx bit 22 | ✅ `qemu64,...,+movbe` |

Without the feature its instructions raise #UD (`cpuid::reports`, `Extension::Xsave`, `Extension::Movbe`; `declared` in `armor-cpu/src/lib.rs` for those the decoder decodes), except F3 0F BC/BD, which then run as `bsf`/`bsr`.

An instruction from an undeclared extension raises #UD (SIGILL), as on a CPU without it (ADR-0026, `cpuid::reports`, checked against these bits by a unit test).

With SSE3–SSE4.2 declared, glibc can choose other variants of the string functions (for example `strcspn` with `pcmpistri`), which the JIT leaves to the interpreter: ✅ `cargo xtask bench` unchanged (2026-10-13, all 11 programs within the variability).

## Intentional differences from qemu
| Leaf | qemu | armor | Reason |
|---|---|---|---|
| 0x8000_0002–4 (brand) | "QEMU Virtual CPU version 2.5+" | "armor virtual x86-64 CPU (qemu64 features)" | armor does not pass itself off as qemu |
| 0x4000_0000 (hypervisor) | "TCGTCGTCGTCG", max leaf 0x40000001 | all zero | same; the "hypervisor" bit of leaf 1 stays set |

The test `basic/cpuid` does not compare these leaves.

## Observed consequences
- With this model static glibc uses only baseline + SSE2 instructions. The qemu log (`-d in_asm`) on the three initial C programs shows 129 distinct forms, none of them AVX.
- `tzcnt`/`lzcnt` (F3 0F BC/BD) are executed as `bsf`/`bsr`, as a CPU without BMI1/LZCNT does. The compiler emits them on purpose because they are compatible. With `bmi1` or `abm` declared they count zeros (`tech/avx.md`).

## When to change it
When armor implements a new extension of x86-64-v3, it is first declared on request (`Features`, above); the default model changes only when the whole of v3 works (`tech/avx.md`). For the others (for example SSE3, SSSE3, SSE4.1/4.2 in M4):
1. set the bit in `cpuid.rs`;
2. remove the feature from the list of `-` in `QEMU_CPU`;
3. measure again with the measurement program above and update this table;
4. `difftest` must stay green.
