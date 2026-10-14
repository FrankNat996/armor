/*
 * Constants and macros for the MXCSR tests (sse/mxcsr_*): each case
 * starts from the default MXCSR and dumps the result and MXCSR.
 */

/* Doubles. */
#define D_ZERO       0x0000000000000000
#define D_MZERO      0x8000000000000000
#define D_ONE        0x3ff0000000000000
#define D_M1         0xbff0000000000000
#define D_TWO        0x4000000000000000
#define D_THREE      0x4008000000000000
#define D_FOUR       0x4010000000000000
#define D_HALF       0x3fe0000000000000
#define D_2_5        0x4004000000000000
#define D_THIRD      0x3fd5555555555555
#define D_INF        0x7ff0000000000000
#define D_MINF       0xfff0000000000000
#define D_QNAN       0x7ff8000000000123
#define D_SNAN       0x7ff0000000000456
#define D_MAX        0x7fefffffffffffff
#define D_MIN        0x0010000000000000      /* smallest normal */
#define D_MIN_PLUS   0x0010000000000001
#define D_ALMOST_ONE 0x3feffffffffffffe      /* 1 - 2^-52 */
#define D_HUGE       0x7e37e43c8800759c      /* 1e300 */
#define D_BIG        0x41e65a0bc0000000      /* 3e9: out of int32 range */
#define D_DEN        0x0000000000000001      /* smallest denormal */
#define D_MDEN       0x800fffffffffffff      /* largest negative denormal */
/* Singles, in the low dword. */
#define S_ZERO       0x00000000
#define S_ONE        0x3f800000
#define S_M1         0xbf800000
#define S_TWO        0x40000000
#define S_SMALL      0x33800000              /* 2^-24: 1 + it is inexact */
#define S_MAX        0x7f7fffff
#define S_QNAN       0x7fc00007
#define S_SNAN       0x7f800009
#define S_DEN        0x00000001
#define PAIR(hi, lo) (((hi) << 32) | (lo))

    .data
    .balign 8
mxcsr_default:
    .long 0x1f80
mxcsr_out:
    .quad 0

/* Writes MXCSR (8 bytes, zero-extended) to stdout. */
.macro DUMP_MXCSR
    stmxcsr [rip + mxcsr_out]
    DUMP_WRITE mxcsr_out, 8
.endm

/* xmm0 = a, xmm1 = b (low lanes); `op xmm0, xmm1`; dump xmm0 and MXCSR. */
.macro SD op, a, b
    ldmxcsr [rip + mxcsr_default]
    SETX xmm0, \a, 0
    SETX xmm1, \b, 0
    \op xmm0, xmm1
    DUMP_MXCSR
    DUMPX xmm0
.endm

/* `op xmm0, xmm1, imm` (cmpsd, cmpss), as SD. */
.macro SDI op, imm, a, b
    ldmxcsr [rip + mxcsr_default]
    SETX xmm0, \a, 0
    SETX xmm1, \b, 0
    \op xmm0, xmm1, \imm
    DUMP_MXCSR
    DUMPX xmm0
.endm

/* The same with both lanes: xmm0 = (a1:a0), xmm1 = (b1:b0). */
.macro PK op, a0, a1, b0, b1
    ldmxcsr [rip + mxcsr_default]
    SETX xmm0, \a0, \a1
    SETX xmm1, \b0, \b1
    \op xmm0, xmm1
    DUMP_MXCSR
    DUMPX xmm0
.endm

/* `op xmm0, xmm1, imm` (cmppd, cmpps), as PK. */
.macro PKI op, imm, a0, a1, b0, b1
    ldmxcsr [rip + mxcsr_default]
    SETX xmm0, \a0, \a1
    SETX xmm1, \b0, \b1
    \op xmm0, xmm1, \imm
    DUMP_MXCSR
    DUMPX xmm0
.endm

/* comis / ucomis xmm0, xmm1: dump MXCSR and the status flags. */
.macro CMPF op, a, b
    ldmxcsr [rip + mxcsr_default]
    SETX xmm0, \a, 0
    SETX xmm1, \b, 0
    \op xmm0, xmm1
    DUMP_MXCSR
    DUMP_FLAGS FLAGS_STATUS
.endm

/* `insn, xmm1` with xmm1 = a (insn names the destination GPR): dump rax
 * and MXCSR. */
.macro TOINT insn, a
    ldmxcsr [rip + mxcsr_default]
    SETX xmm1, \a, 0
    xor eax, eax
    \insn, xmm1
    DUMP_MXCSR
    DUMP rax
.endm

/* `insn` (from rax or eax) with rax = value: dump MXCSR and xmm0. */
.macro FROMINT insn, value
    ldmxcsr [rip + mxcsr_default]
    pxor xmm0, xmm0
    mov rax, \value
    \insn
    DUMP_MXCSR
    DUMPX xmm0
.endm

/* The same macros with MXCSR = control instead of the default. */
    .data
    .balign 4
mxcsr_control:
    .long 0
    .text

.macro LOAD_CONTROL control
    mov dword ptr [rip + mxcsr_control], \control
    ldmxcsr [rip + mxcsr_control]
.endm

.macro SDC control, op, a, b
    LOAD_CONTROL \control
    SETX xmm0, \a, 0
    SETX xmm1, \b, 0
    \op xmm0, xmm1
    DUMP_MXCSR
    DUMPX xmm0
.endm

.macro CMPFC control, op, a, b
    LOAD_CONTROL \control
    SETX xmm0, \a, 0
    SETX xmm1, \b, 0
    \op xmm0, xmm1
    DUMP_MXCSR
    DUMP_FLAGS FLAGS_STATUS
.endm

.macro TOINTC control, insn, a
    LOAD_CONTROL \control
    SETX xmm1, \a, 0
    xor eax, eax
    \insn, xmm1
    DUMP_MXCSR
    DUMP rax
.endm

.macro FROMINTC control, insn, value
    LOAD_CONTROL \control
    pxor xmm0, xmm0
    mov rax, \value
    \insn
    DUMP_MXCSR
    DUMPX xmm0
.endm

.macro PKC control, op, a0, a1, b0, b1
    LOAD_CONTROL \control
    SETX xmm0, \a0, \a1
    SETX xmm1, \b0, \b1
    \op xmm0, xmm1
    DUMP_MXCSR
    DUMPX xmm0
.endm
