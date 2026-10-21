/*
 * F16C (run with `+xsave,+avx,+f16c` declared, difftest's CASE_FEATURES;
 * docs/tech/avx.md): vcvtph2ps and vcvtps2ph on 128 and 256 bits, with
 * registers and memory, on normal, denormal, infinite and NaN halves;
 * vcvtps2ph with the four rounding modes of the immediate and with
 * MXCSR's, on values that round, overflow and underflow; MXCSR after each
 * group. No single operand is denormal: qemu does not raise DE
 * (docs/testing.md).
 */
#include "dump.h"

.macro DUMPY n
    vmovdqu [rip + ymm_out], ymm\n
    DUMP_WRITE ymm_out, 32
.endm

.macro DUMP_MXCSR
    stmxcsr [rip + mxcsr]
    DUMP_WRITE mxcsr, 4
    mov dword ptr [rip + mxcsr], 0x1f80
    ldmxcsr [rip + mxcsr]
.endm

    .data
    .balign 32
/* 1.0, -2.0, 65504 (max), a denormal, +inf, a QNaN, an SNaN, 0x3555. */
halves:
    .short 0x3c00, 0xc000, 0x7bff, 0x0001, 0x7c00, 0x7e01, 0x7d23, 0x3555
/* 1/3, 65520 (overflows to nearest), 1e-6 (tiny), -1e-8 (underflows to
   -0), 2049, a QNaN with payload, an SNaN, 0.1. */
singles:
    .long 0x3eaaaaab, 0x477ff000, 0x358637bd, 0xb22bcc77, 0x45001000, 0x7fc12345, 0x7f812345, 0x3dcccccd
ymm_out:
    .zero 32
store:
    .zero 16
mxcsr:
    .long 0

    .text
    .globl _start
_start:
    vcvtph2ps ymm0, [rip + halves]
    DUMPY 0
    vmovdqu xmm1, [rip + halves]
    vcvtph2ps xmm2, xmm1
    DUMPY 2
    DUMP_MXCSR

    vmovdqu ymm3, [rip + singles]
    .irp imm, 0, 1, 2, 3, 4
    vcvtps2ph xmm4, ymm3, \imm
    DUMPY 4
    DUMP_MXCSR
    .endr
    mov dword ptr [rip + mxcsr], 0x3f80
    ldmxcsr [rip + mxcsr]
    vcvtps2ph xmm4, xmm3, 4
    DUMPY 4
    DUMP_MXCSR
    vcvtps2ph [rip + store], ymm3, 0
    DUMP_WRITE store, 16
    vcvtps2ph [rip + store], xmm3, 3
    DUMP_WRITE store, 16
    DUMP_MXCSR
    EXIT 0

    .section .note.GNU-stack, "", @progbits
