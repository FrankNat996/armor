/*
 * Scalar SSE/SSE2 floating point: movss/movsd, add/sub/mul/div/min/max/
 * sqrt (ss and sd), comis/ucomis flags, conversions between integers,
 * single and double, movmskps/movmskpd, cmpss/cmpsd predicates,
 * shufps/shufpd. The operands include quiet and
 * signalling NaNs, infinities, signed zeros and invalid operations, where
 * x86 differs from IEEE defaults (which NaN propagates, the negative
 * "indefinite" NaN, min/max on ties, integer indefinite).
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
#define D_BIG   0x41e65a0bc0000000      /* 3e9 */
#define D_TINY  0x0000000000000001      /* smallest denormal */
/* Singles, in the low dword. */
#define S_ONE   0x3f800000
#define S_THREE 0x40400000
#define S_QNAN  0x7fc00007
#define S_SNAN  0x7f800009
#define S_INF   0x7f800000
#define S_MZERO 0x80000000
#define UPPER   0xaaaaaaaabbbbbbbb      /* must survive scalar ops */

/* xmm0 = a (with UPPER above), xmm1 = b; `op xmm0, xmm1`; dump xmm0. */
.macro SD op, a, b
    SETX xmm0, \a, UPPER
    SETX xmm1, \b, 0x1111111111111111
    \op xmm0, xmm1
    DUMPX xmm0
.endm

.macro SS op, a, b
    SETX xmm0, (UPPER & 0xffffffff00000000) | \a, UPPER
    SETX xmm1, \b, 0x1111111111111111
    \op xmm0, xmm1
    DUMPX xmm0
.endm

/* Flags of `op xmm0, xmm1`; 0x7f + 0x7f first sets OF, SF and AF. */
.macro CMP op, a, b
    SETX xmm0, \a, 0
    SETX xmm1, \b, 0
    mov al, 0x7f
    add al, 0x7f
    \op xmm0, xmm1
    DUMP_FLAGS FLAGS_STATUS
.endm

/* A conversion to a GPR: `op reg, xmm1`, dump rax. */
.macro TOINT op, reg, b
    SETX xmm1, \b, 0
    mov rax, 0x5555555555555555
    \op \reg, xmm1
    DUMP rax
.endm

    .data
    .balign 16
mem_d:
    .quad D_THREE
mem_s:
    .long S_THREE
    .long 0
out:
    .quad 0x5555555555555555, 0x5555555555555555

    .text
    .globl _start
_start:
    /* Arithmetic, double. */
    SD addsd, D_ONE, D_2_5
    SD subsd, D_ONE, D_2_5
    SD mulsd, D_2_5, D_M2_5
    SD divsd, D_ONE, D_THREE
    SD sqrtsd, D_ONE, D_THREE
    SD addsd, D_TINY, D_TINY
    SD mulsd, D_TINY, 0x3fe0000000000000     /* denormal halved: rounding */
    /*
     * NaNs: the first NaN operand, quieted; invalid -> negative indefinite.
     * With two NaNs, qemu applies the x87 rule (QNaN over SNaN, then the
     * larger significand) instead of the SSE one (always the first): only
     * pairs where both rules agree are here (docs/testing.md).
     */
    SD addsd, D_QNAN, D_SNAN
    SD mulsd, D_ONE, D_SNAN
    SD subsd, D_INF, D_INF
    SD mulsd, D_INF, 0
    SD divsd, 0, 0
    SD divsd, D_ONE, 0
    SD sqrtsd, D_ONE, D_M2_5
    SD sqrtsd, D_ONE, D_MZERO
    SD sqrtsd, D_ONE, D_SNAN
    /* min / max: the second operand on ties and NaNs. */
    SD minsd, 0, D_MZERO
    SD maxsd, D_MZERO, 0
    SD minsd, D_QNAN, D_ONE
    SD maxsd, D_ONE, D_SNAN
    SD minsd, D_ONE, D_THREE
    SD maxsd, D_ONE, D_THREE

    /* Arithmetic, single. */
    SS addss, S_ONE, S_THREE
    SS divss, S_ONE, S_THREE
    SS mulss, S_INF, 0
    SS subss, S_QNAN, S_SNAN
    SS sqrtss, S_ONE, S_THREE
    SS minss, S_MZERO, 0
    SS maxss, S_SNAN, S_ONE

    /* Memory source operands (no alignment needed). */
    SETX xmm0, D_ONE, UPPER
    addsd xmm0, [rip + mem_d]
    DUMPX xmm0
    SETX xmm0, S_ONE, UPPER
    mulss xmm0, [rip + mem_s]
    DUMPX xmm0

    /* comis / ucomis: ZF PF CF, OF SF AF cleared. */
    CMP comisd, D_ONE, D_THREE
    CMP comisd, D_THREE, D_ONE
    CMP comisd, 0, D_MZERO
    CMP ucomisd, D_QNAN, D_ONE
    CMP comisd, D_ONE, D_SNAN
    CMP comiss, S_ONE, S_THREE
    CMP ucomiss, S_THREE, S_QNAN

    /* movsd / movss: loads zero the rest, register moves keep it. */
    SETX xmm0, 0x1234, UPPER
    movsd xmm0, [rip + mem_d]
    DUMPX xmm0
    SETX xmm0, 0x1234, UPPER
    movss xmm0, [rip + mem_s]
    DUMPX xmm0
    SD movsd, D_ONE, D_THREE
    SS movss, S_ONE, S_THREE
    SETX xmm2, D_INF, UPPER
    movsd [rip + out], xmm2
    movss [rip + out + 8], xmm2
    DUMP_WRITE out, 16

    /* Integer to float. */
    SETX xmm0, 0, UPPER
    mov rax, -7
    cvtsi2sd xmm0, rax
    DUMPX xmm0
    mov eax, -7
    cvtsi2sd xmm0, eax
    DUMPX xmm0
    mov rax, 0x7fffffffffffffff
    cvtsi2sd xmm0, rax                       /* rounds */
    DUMPX xmm0
    mov rax, 0x00ffffff00000001
    cvtsi2ss xmm0, rax
    DUMPX xmm0
    mov dword ptr [rip + out], 16777217
    cvtsi2ss xmm0, dword ptr [rip + out]     /* 2^24 + 1 rounds to even */
    DUMPX xmm0

    /* Float to integer: truncation, rounding to nearest-even, indefinite. */
    TOINT cvttsd2si, eax, D_2_5
    TOINT cvtsd2si, eax, D_2_5
    TOINT cvtsd2si, eax, 0x400c000000000000  /* 3.5 */
    TOINT cvttsd2si, eax, D_M2_5
    TOINT cvtsd2si, rax, D_M2_5
    TOINT cvttsd2si, eax, D_BIG
    TOINT cvttsd2si, rax, D_BIG
    TOINT cvttsd2si, eax, D_QNAN
    TOINT cvttsd2si, rax, D_MINF
    TOINT cvttss2si, eax, S_THREE
    TOINT cvtss2si, rax, S_INF
    TOINT cvttss2si, eax, S_QNAN

    /* Single <-> double. */
    SD cvtss2sd, D_ONE, S_THREE
    SD cvtss2sd, D_ONE, S_SNAN
    SD cvtss2sd, D_ONE, 0x00000001           /* denormal single */
    SS cvtsd2ss, S_ONE, 0x3fd5555555555555   /* 1/3 rounds */
    SS cvtsd2ss, S_ONE, 0x7ff8000012345678   /* NaN payload high bits */
    SS cvtsd2ss, S_ONE, 0x47efffffffffffff   /* overflows to infinity */

    /* cmpsd / cmpss: all eight predicates, ordered, equal and with NaN. */
.irp pred, 0, 1, 2, 3, 4, 5, 6, 7
    SETX xmm0, D_ONE, UPPER
    SETX xmm1, D_THREE, 0
    cmpsd xmm0, xmm1, \pred
    DUMPX xmm0
    SETX xmm0, D_THREE, UPPER
    SETX xmm1, D_THREE, 0
    cmpsd xmm0, xmm1, \pred
    DUMPX xmm0
    SETX xmm0, D_QNAN, UPPER
    SETX xmm1, D_ONE, 0
    cmpsd xmm0, xmm1, \pred
    DUMPX xmm0
    SETX xmm0, (UPPER & 0xffffffff00000000) | S_THREE, UPPER
    SETX xmm1, S_ONE, 0
    cmpss xmm0, xmm1, \pred
    DUMPX xmm0
.endr

    /* shufps / shufpd: low lanes from dst, high lanes from src. */
    SETX xmm0, 0x1111111122222222, 0x3333333344444444
    SETX xmm1, 0x5555555566666666, 0x7777777788888888
    shufps xmm0, xmm1, 0x1b
    DUMPX xmm0
    SETX xmm0, 0x1111111122222222, 0x3333333344444444
    shufps xmm0, [rip + out], 0xe4
    DUMPX xmm0
    SETX xmm0, 0x1111111122222222, 0x3333333344444444
    shufpd xmm0, xmm1, 1
    DUMPX xmm0
    SETX xmm0, 0x1111111122222222, 0x3333333344444444
    shufpd xmm0, xmm1, 2
    DUMPX xmm0

    /* Sign masks. */
    SETX xmm3, 0x8000000000000000, 0x0000000080000000
    movmskpd eax, xmm3
    DUMP rax
    movmskps eax, xmm3
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
