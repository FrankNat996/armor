/*
 * The AVX2 gathers (run with `+xsave,+avx,+avx2` declared, difftest's
 * CASE_FEATURES; docs/tech/avx.md): the eight forms in 128 and 256 bits,
 * dword and qword indices (negative ones too) with each scale, a base
 * register and a displacement, a partial mask (the masked-off lanes keep
 * the destination's value), and the mask register cleared afterwards.
 */
#include "dump.h"

.macro SETY n, label
    vmovdqu ymm\n, [rip + \label]
.endm

.macro DUMPY n
    vmovdqu [rip + ymm_out], ymm\n
    DUMP_WRITE ymm_out, 32
.endm

/* A gather into ymm0 with mask ymm1 and indices ymm2, then both dumped. */
.macro GATHER insn, width, scale, mask
    SETY 0, old
    SETY 1, \mask
    \insn \width\()0, [rbx + 8 + \width\()2 * \scale], \width\()1
    DUMPY 0
    DUMPY 1
.endm

    .data
    .balign 32
table:
    .quad 0x1111111111111111, 0x2222222222222222, 0x3333333333333333, 0x4444444444444444
    .quad 0x5555555555555555, 0x6666666666666666, 0x7777777777777777, 0x8888888888888888
    .quad 0x9999999999999999, 0xaaaaaaaaaaaaaaaa, 0xbbbbbbbbbbbbbbbb, 0xcccccccccccccccc
old:
    .quad 0x0123456789abcdef, 0xfedcba9876543210, 0x0f0f0f0f0f0f0f0f, 0xf0f0f0f0f0f0f0f0
ones:
    .quad -1, -1, -1, -1
some:
    .quad 0x8000000000000000, 0x0000000080000000, 0x8000000000000000, 0x80000000ffffffff
dword_indices:
    .long 0, 3, 1, -1, 7, 2, 5, 4
qword_indices:
    .quad 2, -1, 9, 0
ymm_out:
    .zero 32

    .text
    .globl _start
_start:
    lea rbx, [rip + table + 16]
    SETY 2, dword_indices
    .irp mask, ones, some
    .irp scale, 1, 4, 8
    GATHER vpgatherdd, xmm, \scale, \mask
    GATHER vpgatherdd, ymm, \scale, \mask
    GATHER vgatherdps, ymm, \scale, \mask
    GATHER vpgatherdq, xmm, \scale, \mask
    .endr
    .endr
    /* vpgatherdq and vgatherdpd ymm take xmm indices. */
    SETY 0, old
    SETY 1, ones
    vpgatherdq ymm0, [rbx + xmm2 * 8], ymm1
    DUMPY 0
    DUMPY 1
    SETY 0, old
    SETY 1, some
    vgatherdpd ymm0, [rbx + xmm2 * 2], ymm1
    DUMPY 0
    DUMPY 1

    SETY 2, qword_indices
    .irp mask, ones, some
    .irp scale, 2, 8
    GATHER vpgatherqq, ymm, \scale, \mask
    GATHER vgatherqpd, xmm, \scale, \mask
    GATHER vpgatherqd, xmm, \scale, \mask
    .endr
    .endr
    /* vpgatherqd and vgatherqps with ymm indices write xmm. */
    SETY 0, old
    SETY 1, ones
    vpgatherqd xmm0, [rbx + ymm2 * 4], xmm1
    DUMPY 0
    DUMPY 1
    SETY 0, old
    SETY 1, some
    vgatherqps xmm0, [rbx + ymm2 * 8], xmm1
    DUMPY 0
    DUMPY 1
    EXIT 0

    .section .note.GNU-stack, "", @progbits
