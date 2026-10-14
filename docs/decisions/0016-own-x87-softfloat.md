# 0016 — x87: 80-bit arithmetic written by us, verified against qemu

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
After scalar SSE (ADR-0015), five measured programs stop on the x87: coreutils' `seq` and `numfmt`, busybox's `seq`, `awk` and `sleep`. AArch64 has no 80-bit format in hardware.

Measurements of 2026-10-12, with `qemu-x86_64 -d in_asm` (instructions in the translated blocks, so executed at least once):
- **busybox** (`seq 1 5`, `awk …`, `sleep 0.01`): only `fnstcw` (reading the control word). Afterwards, `awk` says "Math support is not compiled in": the math functions are not present in this build.
- **coreutils `seq 1 5` and `seq 0 0.1 1`, `numfmt --to=iec 1048576`:** loads and stores (`fld`/`fstp` at 32, 64 and 80 bits, `fild`/`fistp` at 16, 32 and 64 bits, `fld1`, `fldz`), `fadd`/`fsub`/`fsubr`/`fmul`/`fdiv`/`fdivr` (including the `p` forms), `fxch`, `fchs`, `fabs`, `fcmovbe`, the comparisons `fcomi`/`fucomi`/`fcompi`/`fucompi`, `fnstcw`/`fldcw`.
- No transcendental function (`fsin`, `fyl2x`, `f2xm1`, …) and no `fsqrt`.
- `seq` and `numfmt` change the control word **only to set rounding to truncation** (`or $0xc, %ah`) around `fistpll`: it is gcc's `(long) x` cast. Rounding modes are therefore really needed, at least for conversions to integer.

The `qemu-x86_64` oracle implements `floatx80` in software, derived from Berkeley SoftFloat ⚠️ (remembered fact, to verify in qemu's sources). For the basic operations it is exact, so a bit-for-bit comparison is possible.

## Decision
- **Our own implementation in Rust** (`armor-cpu`, safe, without dependencies) of the 80-bit format: 64-bit mantissa with an explicit integer bit, 15-bit exponent, sign.
  - Operations of this step: those measured above, plus `fsqrt`, `fcom`/`fucom` with `fnstsw`, `ftst` and `fprem` if a program needs them.
  - All four **rounding modes** of the control word (RC), in every operation.
  - **Precision control** (PC) at 64 bits only, the Linux default; 24 or 53 bits → "not supported yet", as for MXCSR (ADR-0015). *Update of 2026-10-12:* python sets 53 bits (control word `0x027F`) to read and print floats; now 24, 53 and 64 bits are supported, only the reserved value is still rejected. Verified by `c/x87` against qemu.
  - **Exceptions** masked only, the default (control word `0x037F`); unmasked → "not supported yet". The status flags and the condition codes (C0–C3) of FSW must be updated, because `fnstsw`/`fcom` use them for comparisons.
  - **Stack:** 8 registers, TOP, tag word; stack overflow and underflow with a masked exception produce the indefinite, as described by the SDM.
  - x87 NaN rules (different from the SSE ones), non-canonical formats (pseudo-denormals, unnormals) as the SDM treats them.
  - `fxsave`/`fxrstor` save and restore the real x87 state (before this ADR they wrote the initial one; done, test `x87/stack`).
- **Transcendentals out of scope** until a measured program uses them. qemu's result for `fsin` & co. is not the hardware's: a separate decision on the oracle will be needed.
- **Verification:**
  - assembly tests in `difftest` for every instruction and the edge cases (NaN, infinities, signed zeros, denormals, overflow, every rounding mode);
  - a **random differential test**: a generated program with thousands of operand pairs (random plus edge cases), run under qemu and armor;
  - mutation tests on rounding;
  - then coreutils' `seq` and `numfmt` and busybox's `seq`/`awk`/`sleep` in `difftest`.

## Alternatives considered
- **The `rustc_apfloat` crate** (Rust port of LLVM's APFloat, used by rustc; 0.2.3, license `Apache-2.0 WITH LLVM-exception`, verified on crates.io on 2026-10-12). It has the `X87DoubleExtended` format and the rounding modes. However:
  - it has no `sqrt` (like LLVM's APFloat ⚠️, to verify in the documentation);
  - the x87 specifics would have to be written by hand anyway: NaN rules, non-canonical formats, precision control, condition codes;
  - it is a "living port" that follows LLVM, with 0.x versions;
  - it adds a license different from the project's (compatible, but to be reported).
  It remains plan B if our own implementation turned out to be too fragile.
- **Berkeley SoftFloat 3 in C, via FFI:** has complete `extF80`, including precision control and `sqrt`. BSD license ⚠️ (to verify). But it requires a C toolchain in the build, `unsafe` FFI (today `armor-cpu` is `forbid(unsafe_code)`, it would have to be isolated in a crate) and a non-Rust dependency.
- **Approximating the x87 with `f64`** (faster, the choice of some translators for performance): gives different results in the last bits and on large exponents, so it violates "identical to the oracle". It can be reconsidered only as an optional performance mode, after the JIT (M6).

## Consequences
- \+ No new dependency, safe code, x87 behaviour under our control.
- \+ The same code will serve the JIT (M6), as a helper call.
- − It is the most numerically delicate piece of the project: correctness depends entirely on the differential test against qemu, which must be written first.
- − Slow compared with hardware: every x87 operation becomes tens of integer operations.
- Left open: the transcendentals and their oracle.
