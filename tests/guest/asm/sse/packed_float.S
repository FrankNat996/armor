/*
 * Packed SSE/SSE2 floating point: add/sub/mul/div/min/max/sqrt on ps and
 * pd (rcp and rsqrt are in sse/reciprocal), cmpps/cmppd with every
 * predicate, and the packed conversions (cvtps2pd, cvtpd2ps, cvtdq2ps,
 * cvt(t)ps2dq, cvtdq2pd, cvt(t)pd2dq), from registers and memory. Every
 * lane gets its own case: quiet and signalling NaNs, infinities, signed
 * zeros, denormals, invalid operations, out-of-range conversions.
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

    .data
    .balign 16
mem_pd:
    .quad D_THREE, D_M2_5
mem_ps:
    .quad PAIR(S_2_5, S_M2_5), PAIR(S_QNAN, S_BIG)
mem_dq:
    .quad 0x80000000ffffffff, 0x7fffffff00000005

    .text
    .globl _start
_start:
    /* Arithmetic, double lanes. */
    P addpd, D_ONE, D_INF, D_2_5, D_MINF
    P addpd, D_QNAN, D_ONE, D_SNAN, D_SNAN
    P subpd, D_INF, D_MZERO, D_INF, D_MZERO
    P mulpd, D_MZERO, D_TINY, D_INF, D_TINY
    P divpd, D_ONE, D_MZERO, D_THREE, D_MZERO
    P minpd, D_MZERO, D_QNAN, D_MZERO, D_ONE
    P maxpd, D_ONE, D_SNAN, D_QNAN, D_ONE
    P sqrtpd, 0, 0, D_THREE, D_M2_5
    P sqrtpd, 0, 0, D_MZERO, D_SNAN

    /* Arithmetic, single lanes. */
    P addps, PAIR(S_INF, S_ONE), PAIR(S_QNAN, S_SNAN), PAIR(S_MINF, S_2_5), PAIR(S_SNAN, S_ONE)
    P subps, PAIR(S_MZERO, S_THREE), PAIR(S_TINY, S_ZERO), PAIR(S_ZERO, S_ONE), PAIR(S_TINY, S_MZERO)
    P mulps, PAIR(S_INF, S_2_5), PAIR(S_TINY, S_M1), PAIR(S_ZERO, S_2_5), PAIR(S_TINY, S_QNAN)
    P divps, PAIR(S_ZERO, S_ONE), PAIR(S_M1, S_INF), PAIR(S_ZERO, S_THREE), PAIR(S_MZERO, S_INF)
    P minps, PAIR(S_MZERO, S_ONE), PAIR(S_QNAN, S_ONE), PAIR(S_ZERO, S_2_5), PAIR(S_ONE, S_SNAN)
    P maxps, PAIR(S_MZERO, S_ONE), PAIR(S_QNAN, S_ONE), PAIR(S_ZERO, S_2_5), PAIR(S_ONE, S_SNAN)
    P sqrtps, 0, 0, PAIR(S_FOUR, S_M1), PAIR(S_MZERO, S_QNAN)

    /* Predicates: equal, less, greater and unordered lanes. */
    PRED cmppd, D_ONE, D_QNAN, D_ONE, D_ONE
    PRED cmppd, D_M2_5, D_INF, D_2_5, D_MINF
    PRED cmpps, PAIR(S_ONE, S_MZERO), PAIR(S_SNAN, S_M1), PAIR(S_2_5, S_ZERO), PAIR(S_ONE, S_ONE)

    /* Conversions. */
    P cvtps2pd, 0, 0, PAIR(S_QNAN, S_M2_5), 0
    P cvtps2pd, 0, 0, PAIR(S_TINY, S_SNAN), 0
    P cvtpd2ps, 0x1111111111111111, 0x2222222222222222, D_THREE, D_QNAN
    P cvtpd2ps, 0, 0, D_TINY, D_INF
    P cvtdq2ps, 0, 0, 0x7fffffff80000000, 0x00000003ffffff01
    P cvtps2dq, 0, 0, PAIR(S_2_5, S_M2_5), PAIR(S_QNAN, S_BIG)
    P cvttps2dq, 0, 0, PAIR(S_2_5, S_M2_5), PAIR(S_QNAN, S_BIG)
    P cvtdq2pd, 0, 0, 0x80000000ffffffff, 0x1234
    P cvtpd2dq, 0x1111111111111111, 0x2222222222222222, D_2_5, D_BIG
    P cvttpd2dq, 0x1111111111111111, 0x2222222222222222, D_M2_5, D_QNAN

    /* Memory operands: m128 (aligned) and m64. */
    SETX xmm0, D_ONE, D_ONE
    addpd xmm0, [rip + mem_pd]
    DUMPX xmm0
    SETX xmm0, PAIR(S_ONE, S_ONE), PAIR(S_ONE, S_ONE)
    mulps xmm0, [rip + mem_ps]
    DUMPX xmm0
    cvtps2pd xmm0, qword ptr [rip + mem_ps]
    DUMPX xmm0
    cvtdq2pd xmm0, qword ptr [rip + mem_dq]
    DUMPX xmm0
    cvtdq2ps xmm0, [rip + mem_dq]
    DUMPX xmm0
    cmpps xmm0, [rip + mem_ps], 4
    DUMPX xmm0

    EXIT 0
