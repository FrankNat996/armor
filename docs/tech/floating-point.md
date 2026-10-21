# Floating point

State as of 2026-10-13: **scalar and packed SSE/SSE2** (ADR-0015) with a **complete MXCSR** like the hardware (ADR-0024): exception flags in the interpreter and in the JIT, the four rounding modes, FZ, DAZ and unmasked exceptions (#XM → SIGFPE), and **x87** (ADR-0016, without transcendentals) implemented.

## Scalar SSE (`crates/armor-cpu/src/float.rs`, `sse.rs`)
Instructions: `movss`/`movsd`, `addss/sd`, `subss/sd`, `mulss/sd`, `divss/sd`, `minss/sd`, `maxss/sd`, `sqrtss/sd`, `comiss/sd`, `ucomiss/sd`, `cmpss/sd` (the 8 SSE predicates; the AVX ones are not there), `cvtsi2ss/sd`, `cvt(t)ss2si`, `cvt(t)sd2si` (32-bit and 64-bit), `cvtss2sd`, `cvtsd2ss`, `movmskps/pd`, `shufps/pd` (bit movement only). Chosen because coreutils' `sort` and `tail` use them (measured with `decoder-check`); the other scalar forms in the same opcode row cost little and were added together.

### Packed (since 2026-10-13, M4)
`addps/pd`, `subps/pd`, `mulps/pd`, `divps/pd`, `minps/pd`, `maxps/pd`, `sqrtps/pd`, `rcpps`/`rcpss`, `rsqrtps`/`rsqrtss`, `cmpps/pd` (the 8 SSE predicates), and the conversions `cvtps2pd`, `cvtpd2ps`, `cvtdq2ps`, `cvtps2dq`, `cvttps2dq`, `cvtdq2pd`, `cvtpd2dq`, `cvttpd2dq`.
- Each lane follows the same rules as the scalar forms below (NaN, indefinite, `min`/`max`, integer indefinite): the same functions of `float.rs`, applied lane by lane.
- A 128-bit memory operand must be aligned to 16 bytes (#GP otherwise); the conversions that read two 32-bit lanes (`cvtps2pd`, `cvtdq2pd`) take an m64 without constraints.
- **`rcp` and `rsqrt`:** armor computes `1/x` and `1/√x` rounded (the root first, then the division), which is within the SDM's error bound (1.5·2⁻¹²). ✅ Identical to qemu on `sse/reciprocal`, special values included. ✅ The hardware gives an approximation: on the Xeon X5675 `rcpps(1)` = `0x3f7ff000` (0.99976) and `rcpps(4)` = `0x3e7ff000`; Intel and AMD differ from each other ⚠️ (recalled). armor does not imitate a vendor's table: `sse/reciprocal` is not compared with the hardware oracle.
- Python 3.13 uses them for complex numbers (`mulpd`, `subpd`, …): `interpreters/python-complex`.
- The JIT leaves them to the interpreter (`tech/jit.md`).

### SSE3 (since 2026-10-13, M4)
`addsubps/pd` (even lanes `dst − src`, odd lanes `dst + src`), `haddps/pd` and `hsubps/pd` (adjacent pairs of dst in the lower half, of src in the upper half), `movddup` (register or m64), `movshdup`, `movsldup`, `lddqu` (m128 without an alignment constraint); `fisttp` is in the x87. The same rules and MXCSR flags as the packed forms. ✅ `sse/sse3` identical to qemu and to the Xeon X5675 with the interpreter, the JIT and in lockstep (the JIT leaves them to the interpreter). Declared in CPUID since 2026-10-13 (`tech/cpuid.md`).

### SSE4.1 floating point (since 2026-10-13, M4)
- **`roundps/pd/ss/sd`:** to the integer value according to bits 0–1 of the immediate (to nearest, towards −∞, towards +∞, towards zero) or, with bit 2, according to MXCSR's RC; bit 3 suppresses PE. A zero keeps its sign (`round(−0.4)` = −0). An SNaN becomes quiet with IE; never DE (SDM, ROUNDSD); with DAZ a denormal counts as zero. Engine: `f80::round_to_integral`.
- **`dpps`/`dppd`:** the products of the lanes chosen by bits 4–7 (the others count as +0 and are **not** computed: an SNaN in an excluded lane gives no IE), summed in pairs (`(p0 + p1) + (p2 + p3)`), written into the lanes of bits 0–3 (zero in the others); the flags are those of every operation performed.
- **`blendvps/pd`, `blendps/pd`, `insertps`, `extractps`:** bit movements only.
- ✅ `sse/sse41` identical to qemu and to the Xeon X5675 (every rounding immediate, MXCSR's RC, DAZ, NaN, excluded lanes of `dpps`); four mutations caught.

### How it is computed (since 2026-10-13, ADR-0024)
- **In software, without host floating-point instructions:** `float.rs` brings the operands into the 80-bit format of `f80.rs` (exact for single and double), computes with the exact integer arithmetic of `f80.rs` and rounds **only once** to the destination format (`Mode::float`: 24 or 53 bits of mantissa and the exponent range of single or double, denormals included). Each operation returns the result and the flags, which the instruction adds to MXCSR (they are "sticky").
- The engine follows MXCSR's control: the four rounding modes, FZ and DAZ (below).
- The rules where x86 and AArch64 differ:
  - **one NaN operand** is propagated, made quiet; **with two NaNs the first source operand wins** ✅ (SDM vol. 1 §4.8.3.5, cited by qemu commit 62d39b28 "target/i386: Set 2-NaN propagation rule explicitly", which flags it as a TODO not yet followed by qemu);
  - **an invalid operation** without NaN operands (`inf − inf`, `0 × inf`, `0 / 0`, root of a negative) gives the **"QNaN indefinite"**, with the sign bit at 1 (`0xFFF8…` in double, `0xFFC00000` in single) ✅ (verified with `difftest` against qemu). AArch64's default NaN has the sign at 0;
  - **`min`/`max`** return the second operand if either is NaN or if both are zero (even with different signs) ✅ (felixcloutier, MINSD; and `difftest`);
  - **conversion to integer** of a NaN or of an out-of-range value: "integer indefinite", that is the most negative value (`0x80000000` or `0x8000000000000000`) ✅ (`difftest`). `cvtsd2si` rounds to nearest even, `cvttsd2si` truncates;
  - **`cvtss2sd`/`cvtsd2ss` of a NaN**: sign and high payload bits kept, made quiet ✅ (`difftest`).
- **Exception flags in MXCSR** (bits 0–5: IE, DE, ZE, OE, UE, PE) ✅ Xeon X5675, 2026-10-13 (106 cases of `sse/mxcsr_flags` and `sse/mxcsr_hardware`, each with the same MXCSR and the same result on the hardware and in armor, interpreter and JIT):
  - **PE** if the result is rounded; **OE** (with PE) if it exceeds the maximum; **UE** (with PE) if it is tiny and inexact: with masked exceptions a tiny but exact result does not give UE;
  - **tininess is measured after rounding:** `DBL_MIN·(1+2⁻⁵²) × (1−2⁻⁵²)` rounds to `DBL_MIN` and gives only PE ✅ hardware (qemu does the same);
  - **ZE** for a division of a non-zero finite value by zero; **IE** for an invalid operation or an SNaN operand;
  - **DE** for a denormal operand, **except** if the operation gives IE or ZE (`denormal / 0` gives only ZE) or if an operand is NaN (`denormal + QNaN` gives nothing); DE coexists with OE, UE and PE ✅ hardware. **qemu never gives DE** (`testing.md`, known discrepancies);
  - **`min`/`max`:** IE if either is NaN, even quiet ✅ hardware;
  - **`comis`** and the "ordered" `cmp` predicates (`lt`, `le`, `nlt`, `nle`): IE for every NaN; **`ucomis`** and the other predicates only for SNaNs ✅ hardware;
  - **conversions to integer:** PE if rounded, IE (and integer indefinite) for NaN or out of range, never DE ✅ hardware;
  - **`cvtsd2ss`** of a denormal: DE, UE and PE; **`cvtss2sd`** of a denormal: DE ✅ hardware;
  - **`rcp`/`rsqrt`:** no flag, whatever the operand ✅ hardware;
  - the packed forms add up the flags of all lanes.
- **MXCSR control** (since 2026-10-13; `ldmxcsr`, `fxrstor` and `rt_sigreturn` accept every value, the reserved bits give #GP) ✅ Xeon X5675 (`sse/mxcsr_control`, `sse/mxcsr_hardware`, `c/sse_exceptions`):
  - **RC:** to nearest, towards −∞, towards +∞, towards zero, for arithmetic, `sqrt` and conversions (`cvtsd2si` yes, `cvttsd2si` always truncates); `1 − 1` gives −0 towards −∞; an overflow gives ±∞ or ±`DBL_MAX` depending on the direction;
  - **FZ** (only with UE masked, as the SDM says): a result that is tiny after rounding becomes a signed zero, with UE and PE, **even if exact** (`den + den` with FZ gives 0, UE, PE); a result that rounds to the smallest normal is not tiny and stays (qemu flushes it to zero);
  - **DAZ:** denormal operands count as a signed zero, without DE, also in comparisons and in `min`/`max`, which return the zero instead of the denormal (qemu returns the denormal);
  - **unmasked exceptions (#XM → SIGFPE):** the instruction does not write the destination (nor RFLAGS for `comis`). If one of the "pre-computation" exceptions (IE, DE, ZE) is unmasked, MXCSR receives only those flags, from all lanes; otherwise all of them, including OE, UE and PE. With OE or UE unmasked, x86 judges the result rounded to the format's precision with an unbounded exponent: OE if too large, UE if tiny **even if exact**, and PE only if that rounding is inexact (`DBL_MAX × 2` gives OE alone, `DBL_MIN / 2` UE alone, `DBL_MIN / 3` UE and PE). An old flag that is set and unmasked raises nothing: only the instruction's own flag counts. `si_code` in `tech/signals.md`. qemu ignores the masks: it never raises #XM.
- Denormals behave as on x86 with the default MXCSR ✅ (`difftest`, a case with the smallest denormal). In the AArch64 JIT they are not flushed to zero, because FZ in FPCR is off.

### Tests
- `sse/scalar_float` (asm, `difftest`): every instruction, with quiet and signalling NaNs, infinities, signed zeros, denormals, out-of-range conversions, upper parts of registers that must stay intact. Mutation test (indefinite NaN with the wrong sign, `min` with `<=`): it fails as it should.
- Unit tests in `float.rs`, including the two-NaN case where qemu is wrong (`testing.md`, known discrepancies), and `flags_follow_the_hardware` with the flags measured on the Xeon X5675.
- `sse/mxcsr_control` (asm, `difftest`): the four rounding modes, FZ and DAZ with masked exceptions, identical to qemu and to the hardware; `c/sse_exceptions` (only `--oracle`): 33 cases of unmasked exceptions, `si_code`, destination not written, MXCSR in the frame, and the FPU state on entry to the handler.
- `sse/mxcsr_flags` (asm, `difftest`): MXCSR and result after each instruction, starting each time from the default (71 cases: arithmetic, NaN, `sqrt`, `min`/`max`, `comis`/`ucomis`, the predicates, conversions, packed forms, `rcp`/`rsqrt`, cumulative flags). `sse/mxcsr_hardware` (33 cases + 2 with memory operands): denormals and NaNs, where qemu is wrong; it runs only with `difftest --oracle`.
- Real programs: `coreutils/sort` (3 cases), `coreutils/tail`.

### In the JIT (since 2026-10-13, `tech/jit.md` increment 17)
The JIT translates the same scalar instructions with the host's floating-point instructions:
- **arithmetic:** the same IEEE 754, rounding to nearest even (the only MXCSR mode accepted); identical results when there is no NaN. If the result is NaN (a NaN operand or an invalid operation) the instruction goes to the slow path, and the interpreter applies the x86 rules above;
- **`min`/`max`:** `fcmp` + `fcsel` with the `mi`/`gt` conditions, which give the second operand on NaN and on equal values (±0), as x86 does;
- **`comiss`/`ucomiss`** and **`cmpss`/`cmpsd`:** host conditions after `fcmp`;
- **conversions to integer:** `fcvtzs`/`fcvtns`. Where the host saturates, that is on the extreme values and on NaNs, x86 gives the integer indefinite: those cases go to the interpreter;
- **`cvtss2sd`/`cvtsd2ss`:** slow path on NaNs. On the NaNs tested the host conversion already gives the same result (equivalent mutation), but the check stays out of caution;
- **only with the default MXCSR control** (ADR-0024): with another rounding mode, FZ, DAZ or an unmasked exception, SSE floating point goes to the interpreter. The engine flushes the block cache when the control enters or leaves the default (`Jit::sync_mxcsr`, after every interpreted instruction and at every `run`);
- **MXCSR exception flags** (since 2026-10-13, ADR-0024): from the host's FPSR register, which the engine adds to MXCSR after each execution of translated code. Where ARM differs: denormal operands go to the interpreter (DE), UFC corrected out of line after `mul`/`div`/`cvtsd2ss` when the result rounds to the smallest normal (tininess before rounding on ARM), and since 2026-10-21 a denormal result of those goes to the interpreter too, so that the JIT can skip the DE test of operands known not to be denormal (`tech/jit.md`, "Operands known not to be denormal"); FCMPE for the signalling comparisons, whose rflags wait until read since 2026-10-21 (`tech/jit.md`, "Floating-point comparisons deferred"). Details in `tech/jit.md`, "MXCSR flags".

Verification: random tests with special values, and all pairs of special values for each operation (`float_operations_on_special_values`).

## x87 (`crates/armor-cpu/src/f80.rs`, `x87.rs`; ADR-0016)
### 80-bit arithmetic (`f80.rs`)
- Register format: sign, 15-bit exponent, 64-bit significand with an explicit integer bit.
- Each operation is computed **exactly on integers** (`u128` with a "sticky" bit) and rounded **only once** by `round`, parameterised by precision and exponent range: 64 bits for the registers, 53 and 24 for `fst` to double and single. The same routine serves all four rounding modes.
- Algorithms: addition with alignment and sticky bit; exact 64×64 product; 128-bit long division plus a sticky bit for the remainder; square root with `u128::isqrt` and a comparison of the remainder (a tie is impossible).
- x87 rules, verified bit for bit against qemu by the `c/x87` test:
  - non-canonical encodings (unnormals, pseudo-infinities, pseudo-NaNs) are invalid operands → IE and indefinite; pseudo-denormals count as denormals;
  - **NaN:** a QNaN wins over an SNaN; otherwise the larger significand wins; on a tie, the positive operand. The result is always quiet;
  - invalid operation → **indefinite** (`FFFF C000000000000000`);
  - underflow (masked exception) = a result that is tiny **and** inexact, with tininess measured before rounding;
  - overflow: infinity or the largest finite value, depending on the rounding mode;
  - out-of-range conversions to integer, NaNs and infinities → integer indefinite and IE.
- **C1** = 1 if rounding increased the magnitude (SDM) ✅ Xeon X5675 (2026-10-13). qemu does not implement it: the differential tests mask it.
- **DE** (denormal operand), which qemu never sets, follows the exception lists of the SDM entries ✅ (felixcloutier, 2026-10-12): set by FLD m32/m64, by arithmetic (FADD & co.), FSQRT, FCOM & co.; **not** by FST/FSTP ("a denormal value does not generate #D") nor by FIST/FISTP (only #IS, #IA, #P). Covered by unit tests; ✅ Xeon X5675 on `fld` of a denormal double and on an addition with an 80-bit denormal.

### State and instructions (`x87.rs`)
- 8 physical registers, TOP, tags (empty/full), control word, status word. Initial state = `fninit` (control word `0x037F`), as Linux does at process start ✅ (`sse/state`: `fxsave` at startup gives the same bytes under qemu).
- Instructions: `fld`/`fst`/`fstp` (m32, m64, m80, ST(i)), `fild`/`fist`/`fistp` (m16, m32, m64), arithmetic in all forms (real and integer memory, ST(0) or ST(i) as destination, with pop), `fcom`/`fcomp`/`fcompp`/`fucom`/`fucomp`/`fucompp`/`ficom`, `fcomi`/`fucomi` (with pop), `ftst`, `fxch`, `fchs`, `fabs`, `fsqrt`, `fld1`, `fldz`, `fcmovcc`, `fldcw`/`fnstcw`, `fnstsw` (memory and AX), `fninit`, `fnclex`, `ffree`, `fnop`, `fwait`. `fxsave`/`fxrstor` save the real state.
- **Added in M4 (2026-10-13)**, because Debian's libm uses them (python's `math.fsum` and `math.hypot` stopped on `fnstenv`):
  - `fnstenv`/`fldenv` (28-byte environment, 32-bit format; the 16-bit one, with the 66 prefix, stops armor with a message) and `fnsave`/`frstor` (108 bytes), with the full tag word (valid, zero, special, empty);
  - `fxam`, also on an empty register (C3 and C0, and in C1 the sign of what the register contains);
  - `fldpi`, `fldl2e`, `fldl2t`, `fldlg2`, `fldln2`, rounded according to the rounding mode from the exact value (128 bits computed with Python's `decimal` module) ✅ identical to qemu in the four modes;
  - `frndint`, `fscale`, `fxtract`, `fprem`, `fprem1`, `fincstp`, `fdecstp`, `ffreep` (documented by AMD, not by Intel; libm uses it), `fisttp` (from SSE3, but without dependencies);
  - `fninit` (also at the end of `fnsave`) leaves the contents of the registers, like the hardware and qemu: before, armor zeroed them, and `fnsave` showed it;
  - **exact `fprem`/`fprem1`** on integers: the quotient and the remainder are computed without rounding; with exponents 64 or more apart the reduction is partial (C2 = 1), and armor reduces the distance by `32 + (d − 64) mod 32` bits ✅ (the same remainders as qemu, step by step, at distances 64, 300 and 1000);
  - **differences from qemu, decided by the hardware** (Xeon X5675 oracle, `testing.md`): the quotient bits of `fprem1`, which qemu leaves at zero; `fscale` with two NaNs, where qemu always returns the one in ST(1). In a partial step the hardware clears C0, C1, C3, as qemu does: at first armor left them as they were, reading the SDM's pseudocode, and this was corrected;
  - **environment like the hardware** ✅ Xeon X5675 (byte for byte): `ffff` in the reserved fields; FIP, FCS (`0x33`), FOP, FDP and FDS (`0x2b`) of the last non-control x87 instruction, which armor now keeps up to date (control instructions, `fwait` included, do not touch them; `fnop` does). `fxsave` writes them as 0: armor's CPUID says AMD, and on AMD `fxsave` saves them only with a pending exception ⚠️ (recalled);
  - **`fxsave` writes zeros** in the 6 bytes after each 80-bit register ✅ Xeon X5675 (qemu leaves them as they were).
- **Control word:** any rounding and any precision (24, 53, 64 bits); all exceptions masked is mandatory, and the reserved precision value (`01`) stops armor with "not supported yet".
- **Reduced precision** (`Mode` in `f80.rs`): only the arithmetic operations (`fadd`, `fsub`, `fmul`, `fdiv`, `fsqrt` and variants, also `x + 0`) round the significand to 24 or 53 bits; the exponent stays at 15 bits, and loads, stores, conversions and comparisons do not change. ✅ `c/x87`, with the same operations at 24 and 53 bits in every rounding mode; the mutation test (`x + 0` not rounded) fails. Measured: python sets 53 bits.
- **Stack faults** (empty register read, push onto a full stack): the SDM's masked response, that is IE, SF, C1 (0 underflow, 1 overflow) and indefinite ⚠️. qemu does not implement them: covered by unit tests.
- **Packed BCD** (`fbld`, `fbstp`, since 2026-10-20; Inno Setup's installers, written in Delphi, use `fbstp` and stopped there under armor's WoW64 backend, ADR-0047): `fbstp` rounds ST(0) to an integer by the rounding control (PE when inexact), stores 18 digits, two per byte from the lowest, and the sign in bit 7 of the tenth byte, also for a zero; a NaN, an infinity or a value of more than 18 digits after rounding raises IE and stores the BCD indefinite `ffff c000 0000 0000 0000`. `fbld` is exact (18 digits fit in 64 bits) and keeps the sign of zero; nibbles above 9 are undefined on x86 and count with their value in armor. ✅ `c/x87_bcd` and its i386 variants (fractions in every rounding mode, both zeros, the 18-digit limits and past them, infinities, NaNs; `fbld` of both signs and the largest values), identical under qemu and on the Xeon X5675 (`difftest --oracle`).
- Not implemented, named in the message: the 16-bit environment, reserved forms and undocumented aliases other than `ffreep`. The transcendentals are done since 2026-10-14 (below).

### Transcendentals (ADR-0030, since 2026-10-14)
`f2xm1`, `fyl2x`, `fyl2xp1`, `fpatan`, `fsin`, `fcos`, `fsincos`, `fptan`, in `crates/armor-cpu/src/f80/transcendental.rs`. They are used by the i386 libm (always: `sin`, `atan2`, `log`, `exp`, … of i386 glibc go through here) and by the x86-64 libm in the `long double` functions.
- **Correctly rounded**, in every rounding mode: the value is computed in multi-word binary arithmetic (`Big`: integers, truncation, no dependencies) with a known error bound, and rounded once to 64 bits; if the two ends of the error interval round differently it is recomputed with twice the precision (Ziv's strategy: 192, 384, … up to 3072 bits). π (Euler's series for atan(1)) and ln 2 (2 atanh(1/3)) are computed to the precision required; logarithms with atanh around 1, `2^x − 1` with the expm1 series, atan with Euler's series, sine and cosine with Taylor after reduction by the nearest multiple of π/2, with π at 128 extra bits and the cancellation measured. The precision control does not apply (the SDM limits it to `fadd`, `fsub`, `fmul`, `fdiv`, `fsqrt`) ⚠️ not measured on the Xeon.
  - **Tiny arguments** (below 2^-34): the second term of the series for sine, tangent, cosine − 1 and atan is below any useful precision, so the result is the first term shifted slightly in a known direction, and that is what gets rounded. This way C1 and UE are right even for a denormal.
  - **Exact cases** (`f2xm1` of an integer, `fyl2x` of a power of two, `fyl2xp1` with 1 + x a power of two): computed exactly and rounded once; **PE stays set**, as the Xeon does ✅ (`f2xm1(1) = 1`, `fyl2x(3, 8) = 9`, `fyl2xp1(2, 1) = 2`, all with PE).
  - **Special cases and flags** as in the SDM: NaNs, infinities, signed zeros, IE for `fyl2x` of a negative and for 0·∞, ZE for `fyl2x(y, 0)`, exact angles of `fpatan` (±π/4, ±3π/4, ±π/2, ±π), C2 = 1 and ST0 unchanged for |x| ≥ 2^63 in the trigonometric functions, DE for denormal operands, UE for tiny results ✅ Xeon (`c/x87_transcendental`, also `fsin` of a denormal: UE and PE). C1 after `fsincos`: the cosine's ⚠️.
- **Verification of the rounding** ✅ (2026-10-14): 96,000 random cases (8 instructions × 4 modes, normal, tiny and large arguments, two different seeds) compared with an independent reference, `tests/x87/reference.py` (Python's `decimal` module at 220 digits, series written separately, exact rounding with fractions): **0 differences** in the result and in C1. How to redo it: `testing.md`.
- **Distance from the hardware** ✅ (2026-10-14, Intel Xeon X5675, probe `tests/x87/probe.c`, 20,000 arguments per instruction, 64-bit ulp):

| Instruction, argument | Equal | 1 ulp | 2–15 ulp | ≥ 16 ulp |
|---|---|---|---|---|
| `f2xm1` (\|x\| < 1) | 89.9% | 10.1% | 0 | 0 |
| `fyl2x` | 93.7% | 6.3% | 0 | 0 |
| `fyl2xp1` | 89.7% | 10.3% | 0 | 0 |
| `fpatan` | 97.1% | 2.9% | 0 | 0 |
| `fsin`, \|x\| < 1 | 99.4% | 0.6% | 0 | 0 |
| `fcos`, \|x\| < 1 | 99.7% | 0.3% | 0 | 0 |
| `fptan`, \|x\| < 1 | 95.3% | 4.7% | 0 | 0 |
| `fsin`, 1 ≤ \|x\| < 32 | 73.9% | 18.8% | 6.9% | 0.4% |
| `fsin`, 32 ≤ \|x\| < 1024 | 12.5% | 18.3% | 50.1% | 19.0% |
| `fsin`, 2^10 ≤ \|x\| < 2^21 | 0.1% | 0.2% | 4.5% | 95.1% |
| `fsin`, 2^40 ≤ \|x\| < 2^63 | 0 | 0 | 0 | 100% |

  The hardware is off by at most one unit where the argument does not need reducing; beyond that the error grows with the argument, because of the reduction with a 66-bit π (SDM vol. 1, 8.3.10): near a multiple of π even the double result changes (`fsin(3.14159)`: 2.6535897932354155e-06 on the Xeon, …4193e-06 the true value). `fcos` and `fptan` like `fsin`. The `c/x87_transcendental` test compares with the Xeon only values rounded to double and arguments where the hardware is not wrong; **qemu does not serve as an oracle** (50 lines out of 124 differ from the Xeon).

### MMX (since 2026-10-14)
- **Registers:** `mmN` is the significand of the x87's physical register RN. Writing it sets the 16 bits of exponent and sign to one (SDM vol. 1, 9.5.1) ✅ Xeon X5675 (`mmx/x87_state`; qemu leaves them at 0); every MMX instruction, except `emms`, sets TOP to 0 and all tags to "valid" (the full tag word of `fnstenv` then shows `aaaa`, special registers, both on the Xeon and under qemu) ✅; `emms` empties the tags ✅.
- **Execution** (`Cpu::mmx` in `sse.rs`): the corresponding SSE2/SSSE3 operation runs on two 128-bit scratch registers (`Reg::Mm(8)` and `Reg::Mm(9)`, zero between one instruction and the next) with the operands zero-extended, and the low 64 bits are kept: for per-lane operations they depend only on the low 64 bits of the operands. The operations that mix the halves have their operands prepared: `pack` and the horizontal sums with both operands in the same register, the high `unpack`s with the upper halves (bytes 4–7) moved down, `pshufb` with 3-bit indices, `palignr` with dst above src. Memory operands are 64-bit and without an alignment requirement, except the m128 of `cvtpd2pi` (#GP if not aligned, like the hardware: qemu does not check).
- **Conversions:** `cvtpi2ps` keeps the upper half of xmm, `cvtps2pi`/`cvttps2pi` convert only the two low lanes (the high ones give no flags), `cvtpi2pd` and `cvtpd2pi` like the SSE2 forms on two lanes; MXCSR flags as for SSE. The switch to the MMX state happens when an operand is an mm register ⚠️ (for a memory operand the SDM is not explicit).
- **The JIT** does not translate the MMX instructions: they go to the interpreter (the state is in the x87 registers, which the JIT does not keep).
- **Tests:** `mmx/ops` (103 results: arithmetic, saturation, comparisons, logic, pack and unpack, shifts, SSSE3, `pshufw`, `pinsrw`/`pextrw`, `pmovmskb`, `psadbw`, `maskmovq`, `movntq`, conversions) identical to qemu and to the Xeon; `mmx/x87_state` to the Xeon; `i386/mmx/bignum` (the forms the Steam client uses) to qemu-i386 and to the Xeon. Mutation caught: the high `unpack`s without the shift.

### Tests
- `c/x87` (C with inline asm, in `difftest`): 400 deterministic pseudo-random pairs of operands (normal, huge, tiny, denormal, NaN, infinities, zeros, non-canonical encodings, equal, opposite and close pairs), each operation in all four rounding modes, the arithmetic also at 24 and 53 bits: 116 results per pair, 46,400 in all, identical to qemu. Run it with `-v` for one readable line per result.
- `x87/stack` (asm): stack instructions, integers in memory, comparisons, `fcmov`, control word, `fxsave` of a live state.
- Mutation tests: rounding to even turned into "half up"; QNaN/SNaN rule inverted. The test fails in both cases.
- Real programs: coreutils `seq` (3 cases) and `numfmt` (3), busybox `seq` (2), `awk`, `sleep`, python (53-bit precision).

## Still to do (M4)
- **Packed SSE:** done on 2026-10-13 (above). It was required by python 3.13: complex numbers stopped on `mulpd`/`subpd`.
- **MMX:** done on 2026-10-14 (above, "MMX"). The Steam client needed it (2,567 MMX instructions, `tech/steam.md`).
- **x87 environment:** done on 2026-10-13 (above), together with the other non-transcendental instructions.
- **x87, transcendentals:** done on 2026-10-14 (ADR-0030, above).
