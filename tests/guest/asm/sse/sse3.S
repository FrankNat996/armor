/*
 * SSE3: addsubps/pd, haddps/pd, hsubps/pd (result and MXCSR flags, with
 * NaNs, infinities and inexact results), movddup (register and m64),
 * movshdup, movsldup (register and m128), lddqu (unaligned m128). No
 * operation gets a signalling NaN before a quiet one, where qemu propagates
 * the wrong NaN (docs/testing.md).
 */
#include "dump.h"
#include "mxcsr.h"

#define S_THREE 0x40400000
#define S_INF   0x7f800000
#define S_MINF  0xff800000
#define S_HALF  0x3f000000

    .data
    .balign 16
mem_pd:
    .quad D_THREE, D_QNAN
mem_ps:
    .quad PAIR(S_TWO, S_ONE), PAIR(S_THREE, S_HALF)
    .byte 0
unaligned:
    .quad 0x0706050403020100, 0x0f0e0d0c0b0a0908

    .text
    .globl _start
_start:
    PK addsubpd, D_ONE, D_ONE, D_THREE, D_THREE
    PK addsubpd, D_INF, D_INF, D_INF, D_INF
    PK addsubpd, D_QNAN, D_ONE, D_ONE, D_SNAN
    PK addsubps, PAIR(S_ONE, S_ONE), PAIR(S_ONE, S_ONE), PAIR(S_SMALL, S_SMALL), PAIR(S_TWO, S_MAX)
    PK addsubps, PAIR(S_INF, S_INF), PAIR(S_QNAN, S_ONE), PAIR(S_INF, S_INF), PAIR(S_SNAN, S_ONE)

    PK haddpd, D_ONE, D_THREE, D_THIRD, D_ONE
    PK haddpd, D_INF, D_MINF, D_QNAN, D_SNAN
    PK hsubpd, D_ONE, D_THREE, D_MAX, D_MINF
    PK hsubpd, D_SNAN, D_ONE, D_ONE, D_QNAN
    PK haddps, PAIR(S_TWO, S_ONE), PAIR(S_HALF, S_THREE), PAIR(S_SMALL, S_ONE), PAIR(S_MINF, S_INF)
    PK hsubps, PAIR(S_TWO, S_ONE), PAIR(S_HALF, S_THREE), PAIR(S_SMALL, S_ONE), PAIR(S_INF, S_INF)
    PK hsubps, PAIR(S_ONE, S_SNAN), PAIR(S_MAX, S_MAX), PAIR(S_ONE, S_QNAN), 0

    /* Memory sources. */
    ldmxcsr [rip + mxcsr_default]
    SETX xmm0, D_ONE, D_ONE
    haddpd xmm0, [rip + mem_pd]
    DUMP_MXCSR
    DUMPX xmm0
    SETX xmm0, PAIR(S_ONE, S_ONE), PAIR(S_ONE, S_ONE)
    addsubps xmm0, [rip + mem_ps]
    DUMPX xmm0

    /* Duplicating moves. */
    SETX xmm1, 0x1111111122222222, 0x3333333344444444
    movddup xmm0, xmm1
    DUMPX xmm0
    movshdup xmm0, xmm1
    DUMPX xmm0
    movsldup xmm0, xmm1
    DUMPX xmm0
    movddup xmm0, qword ptr [rip + mem_pd + 8]
    DUMPX xmm0
    movshdup xmm0, [rip + mem_ps]
    DUMPX xmm0
    movsldup xmm0, [rip + mem_ps]
    DUMPX xmm0
    lddqu xmm0, [rip + unaligned]
    DUMPX xmm0

    EXIT 0
