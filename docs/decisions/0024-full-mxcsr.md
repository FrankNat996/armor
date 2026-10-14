# 0024 — Full MXCSR: rounding, FZ/DAZ, SSE flags and exceptions

- **Status:** Accepted (2026-10-13)
- **Date:** 2026-10-13
- Replaces the part of ADR-0015 on MXCSR ("default control only, flags not updated").

## Context
M4 asks for full MXCSR: the four rounding modes, FZ and DAZ, the exception flags (IE, DE, ZE, OE, UE, PE) and unmasked exceptions (#XM → SIGFPE). Today (ADR-0015) an MXCSR other than the default stops armor, and the flags are never updated: a program that reads `fetestexcept` sees wrong values.

Facts measured on 2026-10-13 with the same C program (`stmxcsr` under qemu-x86_64 10.0.13, `mrs fpsr` natively on the Cortex-A76):
- IE, ZE, OE, PE agree between qemu and the host flags (IOC, DZC, OFC, IXC) on: inexact, division by zero, overflow, `inf - inf`, SNaN, square root of a negative number, ordered and unordered comparisons;
- **underflow:** x86 detects "tininess" **after** rounding, AArch64 **before**. On `DBL_MIN·(1+2⁻⁵²) × (1−2⁻⁵²)` (exact result just below `DBL_MIN`, rounded to `DBL_MIN`) qemu gives UE=0, the host UFC=1; the same in single precision. It is also SoftFloat's rule for x86 ⚠️ (remembered);
- **out-of-range conversion to integer:** qemu gives only IE, the host also IXC. *Corrected on 2026-10-13:* measured again instruction by instruction (`fcvtzs`/`fcvtns` at 32 and 64 bits, positive and negative out-of-range values), the host gives **only IOC**: no IXC. The first measurement probably went through a conversion by the C compiler, not through a single instruction;
- **DE** (denormal operand): neither qemu nor the host sets it. The SDM requires it when DAZ=0 ⚠️ (to be cited from the SDM entry);
- the host has no traps for floating-point exceptions (Cortex-A76), and its FZ applies to inputs and outputs together, whereas x86 separates DAZ and FZ;
- armor does no host floating-point computation outside the SSE instructions (`grep` on `armor`, `armor-linux`, the JIT engine); x87 is all on integers (ADR-0016).
- **Verified on hardware** on 2026-10-13, same program on an Intel Xeon X5675 (the oracle of ADR-0025): tininess **after** rounding (UE=0 in the edge case, like qemu); out-of-range conversion with only IE (like qemu); **DE set** for denormal operands (as in the SDM, unlike qemu and ARM). The three choices of the decision below are those of the hardware.

## Decision
1. **Interpreter: SSE in software, always.** SSE floating-point operations (scalar and packed) are computed with the integer engine of `f80.rs`, generalised to the single and double formats with a single rounding. This way, without host floating-point instructions:
   - all four rounding modes, FZ and DAZ separate;
   - exact flags: tininess after rounding for SSE (x87 stays "before", as today and as qemu); DE as per the SDM;
   - unmasked exceptions: #XM → SIGFPE (`FPE_FLT*` in `si_code`), with the state from before the instruction.
2. **JIT: translates SSE floating point only with the default control** (exceptions masked, round to nearest, no FZ/DAZ). An `ldmxcsr`/`fxrstor` that changes the control advances the code generation, and from then on those instructions go to the interpreter. It is rare, so it matters little that it is slow.
3. **JIT: flags from the hardware, corrected where it differs.** Translated code lets the flags accumulate in FPSR, which is per thread and "sticky" like MXCSR. MXCSR as seen by the guest = flags saved in the `Cpu` + FPSR flags. They are read (and FPSR cleared) when the guest reads MXCSR (`stmxcsr`, `fxsave`, signal frames) and when it writes it. Where the hardware differs, translated code goes to the slow path **before** dirtying FPSR, or removes what it dirtied:
   - conversions to integer: range check before converting (no spurious IXC);
   - multiplication, division, `cvtsd2ss`: if a result has magnitude ≤ `DBL_MIN` (or `FLT_MIN`), UFC is cleared and the instruction goes to the interpreter. A small sum is always exact, so it is not needed there;
   - denormal operands (DE): check of the operands before the operation, slow path if there is one.
   - *Implementation (2026-10-13, `tech/jit.md`, "MXCSR flags"):* FPSR is cleared at every entry into translated code and added to MXCSR at every exit, which is simpler and covers every read; conversions do not need the range check (see the correction in the context); for underflow, instead of going to the interpreter, translated code corrects UFC out of line when the result has magnitude ≤ `DBL_MIN`/`FLT_MIN`, and never leaves UFC set in FPSR. Saving FPSR before every multiplication (to be able to restore it) cost 68% on `bench float`.
   - *Implementation of points 1 and 2 (2026-10-13):* as written, with two facts measured on the Xeon X5675 that the SDM leaves implicit: with an unmasked "pre-computation" exception (IE, DE, ZE) MXCSR receives only those flags, otherwise all of them; with OE or UE unmasked, UE arrives even on an exact small result and PE only if rounding to the precision is inexact. In addition, on entry to a signal handler the FPU state returns to the initial one, as in Linux (`tech/signals.md`).
4. Order: first the packed SSE/SSE2 instructions with today's rules; then the software engine and the flags in the interpreter; then the flags in the JIT; then rounding, FZ/DAZ and unmasked exceptions. Every step with `difftest` and mutations, and a before-and-after measurement of `bench float` and of the python loops.

## Alternatives considered
- **Host FPCR for rounding and FZ, in the interpreter too:** fast, but in Rust changing the floating-point environment is not supported (the compiler assumes the default), the host's FZ does not separate DAZ and FZ, and tininess would stay "before". Discarded.
- **Hardware flags accepting the differences** (UE at the edges, IXC in conversions, no DE): simpler, but a silent wrong result, against the project's rules.
- **No flags in the JIT** (exact flags only in the interpreter): translated programs would see wrong flags.
- **DE like qemu (never set):** it would compare with the oracle, but the project's rule is that the SDM decides (`testing.md`), as already for the x87 (ADR-0016).

## Consequences
- \+ Exact MXCSR in every mode, in both engines; no program stops any more because of an MXCSR other than the default.
- \+ No host floating-point computation in the interpreter: no side effects on FPSR.
- − The interpreter becomes slower on SSE floating point (to be measured; the hot loops are in the JIT).
- − Translated code for floating point has a few more instructions (DE, underflow edges): to be measured with `bench float`.
- − DE and tininess at the edges cannot be compared with qemu (it does not set DE): unit tests based on the SDM, as for the x87.
