# 0026 — Instructions from undeclared extensions: #UD (SIGILL)

- **Status:** Accepted (2026-10-13)
- **Date:** 2026-10-13
- Completes ADR-0010 (CPUID model).

## Context
The CPU that armor presents (ADR-0010) does not declare AVX, AVX-512, AES-NI, PCLMULQDQ, SHA, XOP, 3DNow! or VIA PadLock. On a CPU without an extension, its instructions raise #UD, which Linux delivers as SIGILL (`ILL_ILLOPN`). Until this ADR armor instead stopped with "not supported yet" (exit 125), which told more to whoever uses armor but was not the behaviour of the emulated CPU.

Since 2026-10-13 the length-only decoder (`armor_x86::length`, `tech/x86-64-encoding.md`) knows which extension any instruction belongs to.

Verified on 2026-10-13 with `c/undeclared_extensions` (VEX, EVEX, BMI1, SHA, XOP, 3DNow!, PadLock): qemu-x86_64 with armor's model and an Intel Xeon X5675, which has none of these extensions, give SIGILL, `si_code` 2, address and rip of the instruction.

## Decision
- When `decode` cannot execute an instruction, the interpreter asks `length` for its extension. If armor's CPUID does not declare it (`cpuid::reports`), the instruction raises #UD: SIGILL to the guest, which can handle it like any fault.
- Extensions that are declared but not yet implemented (today MMX, declared by the `qemu64` model) still stop armor with "not supported yet": there a real CPU would execute the instruction, and a SIGILL would be a silent wrong result.
- `cpuid::reports` is checked against the CPUID bits by a unit test, so the two cannot drift apart.

## Alternatives considered
- **Exit 125 as before:** clearer for whoever uses armor, but different from the emulated CPU, from qemu and from the hardware; a program that handles SIGILL (for example to discover features by trying) would behave differently.
- **SIGILL for MMX too:** wrong, because CPUID declares it.

## Consequences
- \+ A program that uses AVX without checking CPUID dies of SIGILL as on a CPU without AVX, or handles the signal.
- − armor's message is gone: whoever launches a program that dies of SIGILL sees only "Illegal instruction". `armor inspect` lists the missing extensions (the `missing` line), and that is where one finds out why.
