/*
 * FMA3 (run with `+xsave,+avx,+fma` declared, difftest's CASE_FEATURES;
 * docs/tech/avx.md): the three operand orders, every sign variant,
 * packed and scalar, single and double, 128 and 256 bits; operands whose
 * fused result differs from a multiplication and an addition rounded
 * apart; NaNs in each operand (which one propagates), 0 * inf, inf - inf;
 * the four rounding modes and FTZ; MXCSR after each group. No operand is
 * denormal: qemu does not raise DE (docs/testing.md).
 */
#include "dump.h"

.macro SETY n, label
    vmovdqu ymm\n, [rip + \label]
.endm

.macro DUMPY n
    vmovdqu [rip + ymm_out], ymm\n
    DUMP_WRITE ymm_out, 32
.endm

/* MXCSR, then back to the default with no flags. */
.macro DUMP_MXCSR
    stmxcsr [rip + mxcsr]
    DUMP_WRITE mxcsr, 4
    mov dword ptr [rip + mxcsr], 0x1f80
    ldmxcsr [rip + mxcsr]
.endm

/* Every form of one operation on ymm1 (dst), ymm2 (src1), ymm3 (src). */
.macro FORMS name
    SETY 1, \name\()_a
    SETY 2, \name\()_b
    SETY 3, \name\()_c
.endm

/* ymm0 = op(ymm4, ymm2, ymm3) with ymm4 a copy of ymm1. */
.macro OP insn, width=ymm
    vmovdqu ymm4, ymm1
    \insn \width\()4, \width\()2, \width\()3
    DUMPY 4
.endm

    .data
    .balign 32
/* Singles: 1 + 2^-12 squared needs 25 bits; minus its rounding. */
s_a:
    .float 1.000244140625, 3.0, -2.5, 1e30, 0.1, 7.0, -1.5, 65536.0
s_b:
    .float 1.000244140625, 0.33333334, 4.0, 1e10, 10.0, -7.0, 1.5, 65536.0
s_c:
    .float -1.00048828125, -1.0, 10.0, -1e38, -1.0, 49.0, 2.25, -4294967296.0
/* Doubles: (1 + 2^-27)^2 - (1 + 2^-26) = 2^-54, lost when rounded apart. */
d_a:
    .double 1.000000007450580596923828125, 0.1, -3.0, 1e300
d_b:
    .double 1.000000007450580596923828125, 10.0, 3.0, 1e10
d_c:
    .double -1.0000000149011611938476562, -1.0, 9.0, -1e308
/* NaNs: an SNaN and two QNaNs in different lanes and operands; 0 * inf;
   inf - inf. */
n_a:
    .long 0x7fa00001, 0x3f800000, 0x7fc00002, 0, 0x7f800000, 0x40000000, 0x7fc00005, 0x3f800000
n_b:
    .long 0x40000000, 0x7fc00003, 0x7fa00004, 0x7f800000, 0x40000000, 0x40400000, 0x7fc00006, 0x3f800000
n_c:
    .long 0x7fc00007, 0x7fc00008, 0x7fc00009, 0x3f800000, 0xff800000, 0x7fa0000a, 0x7fc0000b, 0x7fc0000c
/* Rounding: 1/3 * 3 - 1 and its neighbours, and a tiny product. */
r_a:
    .double 0.33333333333333331, 1e-200, -0.33333333333333331, 1.5
r_b:
    .double 3.0, 1e-200, 3.0, 1.0000000000000002
r_c:
    .double -1.0, 0.0, 1.0, -1.5
ymm_out:
    .zero 32
mxcsr:
    .long 0

    .text
    .globl _start
_start:
    .irp set, s, n
    FORMS \set
    .irp insn, vfmadd132ps, vfmadd213ps, vfmadd231ps, vfmsub132ps, vfmsub213ps, vfmsub231ps, vfnmadd132ps, vfnmadd213ps, vfnmadd231ps, vfnmsub132ps, vfnmsub213ps, vfnmsub231ps, vfmaddsub132ps, vfmaddsub213ps, vfmaddsub231ps, vfmsubadd132ps, vfmsubadd213ps, vfmsubadd231ps
    OP \insn
    OP \insn, xmm
    .endr
    .irp insn, vfmadd132ss, vfmadd213ss, vfmadd231ss, vfmsub132ss, vfmsub213ss, vfmsub231ss, vfnmadd132ss, vfnmadd213ss, vfnmadd231ss, vfnmsub132ss, vfnmsub213ss, vfnmsub231ss
    OP \insn, xmm
    .endr
    DUMP_MXCSR
    .endr

    FORMS d
    .irp insn, vfmadd132pd, vfmadd213pd, vfmadd231pd, vfmsub132pd, vfmsub213pd, vfmsub231pd, vfnmadd132pd, vfnmadd213pd, vfnmadd231pd, vfnmsub132pd, vfnmsub213pd, vfnmsub231pd, vfmaddsub132pd, vfmaddsub213pd, vfmaddsub231pd, vfmsubadd132pd, vfmsubadd213pd, vfmsubadd231pd
    OP \insn
    OP \insn, xmm
    .endr
    .irp insn, vfmadd132sd, vfmadd213sd, vfmadd231sd, vfmsub132sd, vfmsub213sd, vfmsub231sd, vfnmadd132sd, vfnmadd213sd, vfnmadd231sd, vfnmsub132sd, vfnmsub213sd, vfnmsub231sd
    OP \insn, xmm
    .endr
    DUMP_MXCSR

    /* A memory source, and the destination as the multiplied register. */
    FORMS d
    vfmadd231pd ymm1, ymm2, [rip + d_c]
    DUMPY 1
    vfmadd132sd xmm2, xmm1, [rip + d_c + 8]
    DUMPY 2
    DUMP_MXCSR

    /* The rounding modes, then FTZ with a tiny result. */
    FORMS r
    .irp mode, 0x1f80, 0x3f80, 0x5f80, 0x7f80, 0x9f80
    mov dword ptr [rip + mxcsr], \mode
    ldmxcsr [rip + mxcsr]
    OP vfmadd231pd
    OP vfnmsub213sd, xmm
    DUMP_MXCSR
    .endr
    EXIT 0

    .section .note.GNU-stack, "", @progbits
