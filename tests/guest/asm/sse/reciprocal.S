/*
 * rcpps / rsqrtps and the scalar rcpss / rsqrtss (the upper lanes stay),
 * on normal values, zeros, infinities, NaNs and denormals. The results
 * are approximations whose bits differ between processors (the SDM
 * bounds the relative error by 1.5 * 2^-12): armor and qemu compute them
 * exactly, an Intel Xeon X5675 gives its own approximation, so this test
 * is compared with qemu only, not with the hardware oracle
 * (docs/testing.md).
 */
#include "dump.h"

/* Doubles. */
#define D_ONE   0x3ff0000000000000
#define D_THREE 0x4008000000000000
#define D_2_5   0x4004000000000000
#define D_M2_5  0xc004000000000000
#define D_INF   0x7ff0000000000000
#define D_MINF  0xfff0000000000000
#define D_QNAN  0x7ff8000000000123
#define D_SNAN  0x7ff0000000000456
#define D_MZERO 0x8000000000000000
#define D_TINY  0x0000000000000001
#define D_BIG   0x41f0000000000000      /* 2^32: out of int32 range */
#define D_M1    0xbff0000000000000
/* Pairs of singles in one quadword: (high << 32) | low. */
#define S_ONE   0x3f800000
#define S_THREE 0x40400000
#define S_2_5   0x40200000
#define S_M2_5  0xc0200000
#define S_INF   0x7f800000
#define S_MINF  0xff800000
#define S_QNAN  0x7fc00007
#define S_SNAN  0x7f800009
#define S_MZERO 0x80000000
#define S_ZERO  0x00000000
#define S_TINY  0x00000001
#define S_FOUR  0x40800000
#define S_M1    0xbf800000
#define S_BIG   0x4f000000              /* 2^31: out of int32 range */
#define PAIR(hi, lo) (((hi) << 32) | (lo))

/* xmm0 = (a1:a0), xmm1 = (b1:b0); `op xmm0, xmm1`; dump xmm0. */
.macro P op, a0, a1, b0, b1
    SETX xmm0, \a0, \a1
    SETX xmm1, \b0, \b1
    \op xmm0, xmm1
    DUMPX xmm0
.endm

/* `op xmm0, xmm1, imm` for every predicate 0..7. */
.macro PRED op, a0, a1, b0, b1
    .irp p, 0, 1, 2, 3, 4, 5, 6, 7
    SETX xmm0, \a0, \a1
    SETX xmm1, \b0, \b1
    \op xmm0, xmm1, \p
    DUMPX xmm0
    .endr
.endm

    .text
    .globl _start
_start:
    /* Reciprocals: packed, and scalar (the upper lanes stay). */
    P rcpps, 0, 0, PAIR(S_FOUR, S_ONE), PAIR(S_MZERO, S_INF)
    P rcpps, 0, 0, PAIR(S_THREE, S_QNAN), PAIR(S_TINY, S_M2_5)
    P rsqrtps, 0, 0, PAIR(S_FOUR, S_ONE), PAIR(S_MZERO, S_INF)
    P rsqrtps, 0, 0, PAIR(S_THREE, S_M1), PAIR(S_TINY, S_SNAN)
    P rcpss, PAIR(0x11111111, 0x22222222), 0x3333333344444444, S_FOUR, 0
    P rsqrtss, PAIR(0x11111111, 0x22222222), 0x3333333344444444, S_FOUR, 0

    EXIT 0
