# x86-64 flags (RFLAGS)

**Reference source:** Intel SDM Vol. 1, §3.4.3 (EFLAGS), and the pages of the individual instructions in Vol. 2 ("Flags Affected" section).
**Verification status:** markers per section. The ✅ parts were verified against qemu (2026-10-12); the ⚠️ parts are written from memory and must be verified on the SDM before implementing them.

## RFLAGS at process start
`0x202`: reserved bit 1 and IF. ✅ Observed under qemu-x86_64 (test `basic/initial_state`). armor uses the same value (`INITIAL_RFLAGS` in `armor-cpu`). `pushfq` shows it as it is.

## M1 implementation (`armor_cpu::alu`)
- Flags computed immediately, not lazily. ADD/ADC and SUB/SBB/CMP use 128-bit arithmetic for CF, then truncate to the operand size.
- OF from the sign bit of `(a^r)&(b^r)` (addition) or of `(a^b)&(a^r)` (subtraction); AF from `(a^b^r) & 0x10`; PF from the parity of the low byte.
- AND/OR/XOR/TEST: CF=OF=0. AF is set to 0, but it is **undefined**, and the tests exclude it with `FLAGS_NO_AF`.
- Verified against qemu by the tests `alu/*` and `control/jcc`, on all operand sizes.

## M2 implementation: values chosen for undefined flags
armor must still put a value in the flags that the architecture leaves undefined. The differential tests exclude them with masks.

| Instructions | Defined flags (compared) | What armor does with the undefined ones |
|---|---|---|
| `inc`/`dec` | all except CF, which stays unchanged | — |
| `neg` | all (as `sub 0, x`) | — |
| shifts (`shl`/`shr`/`sar`), count ≠ 0 | CF, SF, ZF, PF; OF only with count 1 | OF computed as for count 1; AF = 0 |
| rotations (`rol`/`ror`/`rcl`/`rcr`) | CF; OF only with count 1; SF, ZF, AF, PF unchanged | OF computed as for count 1 |
| any shift or rotation with masked count = 0 | no flag changes | — |
| `mul`/`imul` | CF = OF (overflow) | SF, ZF, PF from the low result; AF = 0 |
| `div`/`idiv` | none | unchanged |
| `bsf`/`bsr` | ZF; destination only if the source ≠ 0 | other flags unchanged; with source 0 the destination stays unchanged |
| `bt*` | CF | ZF unchanged (as Intel says), others unchanged |
| `tzcnt`/`lzcnt` counting zeros (BMI1/LZCNT declared, `tech/avx.md`) | CF, ZF | others unchanged |
| `andn`, `blsi`, `blsmsk`, `blsr`, `bzhi` | SF, ZF, CF, OF | PF from the result, AF = 0 |
| `bextr` | ZF, CF, OF | SF, PF from the result, AF = 0 |
| `mulx`, `pdep`, `pext`, `rorx`, `sarx`, `shlx`, `shrx` | none changed | — |

⚠️ The exact behaviour of Intel/AMD hardware on undefined flags has not been measured (no x86 hardware available).

## Bits relevant in user mode
Positions of CF, PF, AF, ZF, SF and OF: ✅ confirmed by the differential tests, which compare with qemu the flags written by `pushfq` for every flag set. The other rows: ⚠️.
| Bit | Name | Meaning |
|---|---|---|
| 0 | CF | Carry (unsigned carry or borrow) |
| 1 | — | Reserved, always 1 |
| 2 | PF | Parity: 1 if the **low 8 bits** of the result have an even number of bits set to 1 |
| 4 | AF | Carry or borrow from bit 3 (BCD arithmetic) |
| 6 | ZF | Zero result |
| 7 | SF | Sign bit of the result |
| 8 | TF | Trap (single step) |
| 10 | DF | Direction of the string instructions |
| 11 | OF | Signed overflow |
| 18 | AC | Alignment check |
| 21 | ID | If it can be modified, it indicates that CPUID is supported (old test via `pushf`/`popf`) |

## Lazy flags
Implemented in the JIT (increments 9, 10 and 12 of `tech/jit.md`, 2026-10-13), together with a per-block liveness analysis that skips flags never read (increment 19); the interpreter computes the flags immediately. Instead of updating RFLAGS at every instruction, the following are saved:
- the operation;
- the size;
- the operands and/or the result.

The flags are computed only when they are read (`jcc`, `setcc`, `cmovcc`, `adc`/`sbb`, `pushf`, …). Many flags are overwritten before being read, so computing them immediately is wasteful.

Notes for the JIT (M6):
- ARM's NZCV correspond roughly to SF/ZF/CF/OF. ✅ **ARM's carry after a subtraction is inverted** with respect to x86's borrow (on ARM C=1 means "no borrow"): verified by the JIT, whose flag table inverts it, and by a caught mutation that removed the inversion (`tech/jit.md`).
- PF and AF have no equivalent on ARM: they are computed in software (PF from the flag table, AF from the operands), lazily.

## Undefined flags ⚠️
After some instructions certain flags are **architecturally undefined**. The tests must exclude them with a mask (`testing.md`).
Examples to verify on the SDM:

| Instruction | Undefined flags (to verify) |
|---|---|
| `AND`/`OR`/`XOR`/`TEST` | AF |
| `MUL`/`IMUL` | SF, ZF, AF, PF |
| `SHL`/`SHR`/`SAR` with count > 1 | OF |
| `BSF`/`BSR` | all except ZF; with source 0 also the destination (Intel declares it undefined, AMD leaves it unchanged) |
| `DIV`/`IDIV` | CF, OF, SF, ZF, AF, PF |

Rule: for every implemented instruction, the mask of the defined flags is written in the test, with the reference to the SDM page.

## DF and the ABI ⚠️
The psABI requires DF=0 on entry to and exit from functions. The kernel clears DF before entering a signal handler ⚠️ (remembered; to verify in `arch/x86/kernel/signal.c`). armor does so (`signal.rs`, `ENTRY_CLEARED_FLAGS`; `tech/signals.md`).
