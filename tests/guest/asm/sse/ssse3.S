/*
 * SSSE3 (66 0F 38, 66 0F 3A 0F), xmm forms: pshufb, phaddw/d/sw,
 * phsubw/d/sw, pmaddubsw, pmulhrsw, psignb/w/d, pabsb/w/d, palignr with
 * every interesting shift; register and aligned m128 sources. The values
 * hit the edges: saturation, -32768 / -128, zero, pshufb controls with
 * bit 7 set.
 */
#include "dump.h"

#define A0 0x80ff7f0100fe0281
#define A1 0x7fff800000017ffe
#define B0 0x0102030485868788
#define B1 0xff00807f0f8e1d2c
#define C0 0x8000800080007fff
#define C1 0x7fff7fff00000001

/* xmm0 = (a1:a0), xmm1 = (b1:b0); `op xmm0, xmm1`; dump xmm0. */
.macro P op, a0, a1, b0, b1
    SETX xmm0, \a0, \a1
    SETX xmm1, \b0, \b1
    \op xmm0, xmm1
    DUMPX xmm0
.endm

/* Every operand pair of three values. */
.macro ALL op
    P \op, A0, A1, B0, B1
    P \op, B0, B1, A0, A1
    P \op, C0, C1, C0, C1
    P \op, A0, A1, C0, C1
    P \op, C0, C1, B0, B1
.endm

    .data
    .balign 16
mem:
    .quad B0, B1

    .text
    .globl _start
_start:
    ALL pshufb
    P pshufb, A0, A1, 0x0f0e0d0c0b0a0908, 0x0706050403020100
    ALL phaddw
    ALL phaddd
    ALL phaddsw
    ALL phsubw
    ALL phsubd
    ALL phsubsw
    ALL pmaddubsw
    ALL pmulhrsw
    ALL psignb
    ALL psignw
    ALL psignd
    ALL pabsb
    ALL pabsw
    ALL pabsd
    .irp shift, 0, 1, 7, 8, 15, 16, 17, 31, 32, 255
    SETX xmm0, A0, A1
    SETX xmm1, B0, B1
    palignr xmm0, xmm1, \shift
    DUMPX xmm0
    .endr

    /* Memory sources. */
    SETX xmm0, A0, A1
    pshufb xmm0, [rip + mem]
    DUMPX xmm0
    phaddsw xmm0, [rip + mem]
    DUMPX xmm0
    pabsb xmm0, [rip + mem]
    DUMPX xmm0
    palignr xmm0, [rip + mem], 5
    DUMPX xmm0

    EXIT 0
