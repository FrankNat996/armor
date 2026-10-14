/*
 * MXCSR exception flags (IE, DE, ZE, OE, UE, PE) raised by SSE/SSE2
 * floating point, all exceptions masked: each case starts from the
 * default MXCSR (0x1f80) and dumps the result and MXCSR. Arithmetic,
 * min/max, sqrt, comis/ucomis, cmp predicates, conversions, packed lanes
 * (flags of all lanes together), rcp/rsqrt (no flags) and stickiness.
 * Denormal operands (DE) and the other cases where qemu differs from the
 * hardware are in sse/mxcsr_hardware.
 */
#include "dump.h"
#include "mxcsr.h"

    .text
    .globl _start
_start:
    /* Arithmetic: exact, inexact, ÷0, invalid, overflow, underflow. */
    SD addsd, D_ONE, D_TWO
    SD divsd, D_ONE, D_THREE
    SD divsd, D_ONE, D_ZERO
    SD divsd, D_ZERO, D_ZERO
    SD subsd, D_INF, D_INF
    SD mulsd, D_ZERO, D_MINF
    SD mulsd, D_MAX, D_TWO
    SD divsd, D_MIN, D_THREE
    SD mulsd, D_MIN, D_HALF
    /* Tiny before rounding, not after: x86 detects tininess after
     * rounding, so PE alone. */
    SD mulsd, D_MIN_PLUS, D_ALMOST_ONE
    SD addss, S_ONE, S_SMALL
    SD mulss, S_MAX, S_TWO
    SD divss, S_ONE, S_ZERO

    /* NaN operands: IE only for signalling ones. */
    SD addsd, D_QNAN, D_ONE
    SD addsd, D_ONE, D_SNAN
    SD mulss, S_QNAN, S_ONE
    SD mulss, S_SNAN, S_ONE

    /* sqrt. */
    SD sqrtsd, 0, D_TWO
    SD sqrtsd, 0, D_FOUR
    SD sqrtsd, 0, D_M1
    SD sqrtsd, 0, D_MZERO
    SD sqrtss, 0, S_SNAN

    /* min and max: IE for any NaN, quiet ones too. */
    SD minsd, D_QNAN, D_ONE
    SD maxsd, D_ONE, D_QNAN
    SD minss, S_ONE, S_SNAN
    SD maxsd, D_ONE, D_TWO

    /* comis: IE for any NaN; ucomis: for signalling NaNs only. */
    CMPF comisd, D_QNAN, D_ONE
    CMPF ucomisd, D_QNAN, D_ONE
    CMPF ucomisd, D_ONE, D_SNAN
    CMPF comiss, S_ONE, S_QNAN
    CMPF ucomiss, S_ONE, S_ONE

    /* Predicates: lt, le, nlt, nle signal on quiet NaNs. */
    .irp p, 0, 1, 2, 3, 4, 5, 6, 7
    SDI cmpsd, \p, D_QNAN, D_ONE
    .endr
    SDI cmpss, 0, S_SNAN, S_ONE

    /* Conversions between formats. */
    SD cvtsd2ss, 0, D_THIRD
    SD cvtsd2ss, 0, D_HUGE
    SD cvtsd2ss, 0, D_MIN
    SD cvtsd2ss, 0, D_SNAN
    SD cvtsd2ss, 0, D_TWO
    SD cvtss2sd, 0, S_SNAN
    SD cvtss2sd, 0, S_QNAN

    /* Conversions to integers: PE when rounded, IE out of range or NaN. */
    TOINT "cvtsd2si eax", D_2_5
    TOINT "cvttsd2si eax", D_2_5
    TOINT "cvtsd2si eax", D_BIG
    TOINT "cvtsd2si rax", D_BIG
    TOINT "cvttsd2si rax", D_QNAN
    TOINT "cvtss2si eax", S_ONE
    TOINT "cvttss2si rax", S_SNAN

    /* Conversions from integers: PE when rounded. */
    FROMINT "cvtsi2sd xmm0, eax", 0x7fffffff
    FROMINT "cvtsi2sd xmm0, rax", 0x7fffffffffffffff
    FROMINT "cvtsi2ss xmm0, eax", 0x7fffffff
    FROMINT "cvtsi2ss xmm0, eax", 0x1000

    /* Packed: the flags of every lane together. */
    PK divpd, D_ONE, D_ONE, D_THREE, D_ZERO
    PK mulpd, D_MAX, D_QNAN, D_TWO, D_ONE
    PK addps, PAIR(S_ONE, S_SNAN), PAIR(S_ONE, S_ONE), PAIR(S_SMALL, S_ONE), PAIR(S_ONE, S_ONE)
    PKI cmppd, 1, D_QNAN, D_ONE, D_ONE, D_ONE
    PK cvtpd2dq, 0, 0, D_2_5, D_BIG
    PK cvttps2dq, 0, 0, PAIR(S_ONE, S_QNAN), 0
    PK cvtpd2ps, 0, 0, D_THIRD, D_ONE
    PK cvtdq2ps, 0, 0, 0x7fffffff00000001, 0

    /* rcp and rsqrt raise nothing, whatever the operand. */
    SD rcpss, 0, S_ZERO
    SD rcpss, 0, S_SNAN
    SD rsqrtss, 0, S_M1
    SD rsqrtss, 0, S_ZERO

    /* Flags are sticky: a second operation adds to them. */
    ldmxcsr [rip + mxcsr_default]
    SETX xmm0, D_ONE, 0
    SETX xmm1, D_THREE, 0
    divsd xmm0, xmm1
    SETX xmm1, D_ZERO, 0
    divsd xmm0, xmm1
    DUMP_MXCSR

    EXIT 0
