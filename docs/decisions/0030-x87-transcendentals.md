# 0030 — Correctly rounded x87 transcendentals

- **Status:** Accepted (2026-10-13)
- **Date:** 2026-10-13
- Extends ADR-0016 (x87 with exact 80-bit arithmetic), which had excluded them "until a program uses them".

## Context
The i386 libm uses the x87 transcendental instructions (`fsin`, `fcos`, `fsincos`, `fptan`, `fpatan`, `fyl2x`, `fyl2xp1`, `f2xm1`; measured on a static i386 hello, ADR-0029). Intel hardware does not always round these functions correctly (errors up to about 1 ulp, and argument reduction with a 66-bit π), and its algorithm is not public: reproducing it bit for bit cannot be guaranteed.

## Decision
- Every transcendental is computed in extended precision (integer arithmetic, like `f80.rs`) and rounded **once** to the destination format according to the x87 control: the exact result, rounded.
- Flags, special cases (NaN, infinities, zeros, denormals, out-of-domain arguments, C2 for arguments too large for `fsin`/`fcos`/`fptan`) as the SDM says, verified on the Xeon.
- Where armor and the Xeon differ in the last bit, the difference is measured (in ulp) and documented in `tech/floating-point.md`; the tests compare exactly only where the hardware is exact, elsewhere with the measured distance.

## Implementation (2026-10-14)
- `crates/armor-cpu/src/f80/transcendental.rs`: multi-word arithmetic and Ziv's strategy (`tech/floating-point.md`, "Transcendentals").
- Verification: 96,000 random cases against an independent reference (`tests/x87/reference.py`): 0 differences; `c/x87_transcendental` (x86-64 and i386, six variants) identical to the Xeon, only with `--oracle`.
- Measured on the Xeon, and followed: PE set even in the exact results of `f2xm1`, `fyl2x`, `fyl2xp1`; UE for `fsin` of a denormal.
- Measured distance from the hardware: at most 1 ulp without argument reduction; for sine, cosine and tangent it grows with the argument (66-bit π), up to having no bits in common beyond 2^40 (`tech/floating-point.md`).

## Alternatives considered
- **Like the hardware:** faithful where measured, but not guaranteeable in general (non-public algorithm).
- **Like qemu:** qemu is not the hardware; for the transcendentals it is just another algorithm.

## Consequences
- \+ Deterministic results, more precise than the hardware; no table to measure.
- − Sometimes different from the Xeon in the last bit: the tests must know this.
