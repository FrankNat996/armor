/*
 * MXCSR control with every exception masked: the four rounding modes,
 * FZ (flush to zero) and DAZ (denormals are zero), on arithmetic, sqrt,
 * conversions and compares. Each case loads MXCSR and dumps MXCSR and the
 * result. Unmasked exceptions are in c/sse_exceptions.
 */
#include "dump.h"
#include "mxcsr.h"

#define NEAREST 0x1f80
#define DOWN    0x3f80
#define UP      0x5f80
#define ZERO    0x7f80
#define FZ      0x9f80
#define DAZ     0x1fc0
#define FZ_DAZ  0x9fc0

#define D_M_THIRD 0xbfd5555555555555
#define D_M2_5    0xc004000000000000
#define D_2_7     0x400599999999999a
#define D_MMAX    0xffefffffffffffff
#define D_2M60    0x3c30000000000000  /* 2^-60 */

    .text
    .globl _start
_start:
    .irp rc, NEAREST, DOWN, UP, ZERO
    SDC \rc, divsd, D_ONE, D_THREE
    SDC \rc, divsd, D_M1, D_THREE
    SDC \rc, addsd, D_ONE, D_2M60
    SDC \rc, subsd, D_ONE, D_2M60
    SDC \rc, sqrtsd, 0, D_TWO
    SDC \rc, mulsd, D_MAX, D_TWO
    SDC \rc, mulsd, D_MMAX, D_TWO
    SDC \rc, divsd, D_MIN, D_THREE
    SDC \rc, cvtsd2ss, 0, D_THIRD
    SDC \rc, cvtsd2ss, 0, D_M_THIRD
    SDC \rc, subsd, D_ONE, D_ONE
    TOINTC \rc, "cvtsd2si eax", D_2_5
    TOINTC \rc, "cvtsd2si eax", D_M2_5
    TOINTC \rc, "cvtsd2si rax", D_2_7
    TOINTC \rc, "cvttsd2si eax", D_M2_5
    TOINTC \rc, "cvtss2si eax", S_ONE
    FROMINTC \rc, "cvtsi2sd xmm0, rax", 0x20000000000001
    FROMINTC \rc, "cvtsi2sd xmm0, rax", -0x20000000000001
    FROMINTC \rc, "cvtsi2ss xmm0, eax", 0x7fffffff
    PKC \rc, cvtpd2dq, 0, 0, D_2_5, D_M2_5
    .endr

    /* Packed forms follow RC too. */
    LOAD_CONTROL UP
    SETX xmm0, D_ONE, D_M1
    SETX xmm1, D_THREE, D_THREE
    divpd xmm0, xmm1
    DUMP_MXCSR
    DUMPX xmm0

    /* FZ: tiny results become zeros, with UE and PE (more cases, where
     * qemu differs, in sse/mxcsr_hardware). */
    .irp control, FZ, FZ_DAZ
    SDC \control, divsd, D_MIN, D_THREE
    SDC \control, mulsd, D_MIN, D_HALF
    SDC \control, cvtsd2ss, 0, D_MIN
    SDC \control, subsd, D_MIN_PLUS, D_MIN
    .endr
    SDC FZ_DAZ, mulss, S_ONE, S_DEN
    SDC FZ_DAZ, addsd, D_DEN, D_DEN

    /* DAZ: denormal operands are zeros, without DE. */
    .irp control, DAZ, FZ_DAZ
    SDC \control, addsd, D_DEN, D_ONE
    SDC \control, mulsd, D_DEN, D_MAX
    SDC \control, divsd, D_ONE, D_DEN
    SDC \control, divsd, D_DEN, D_ONE
    SDC \control, sqrtsd, 0, D_MDEN
    SDC \control, minsd, D_DEN, D_ZERO
    SDC \control, maxsd, D_MDEN, D_ZERO
    SDC \control, cvtss2sd, 0, S_DEN
    SDC \control, cvtsd2ss, 0, D_DEN
    CMPFC \control, ucomisd, D_DEN, D_MDEN
    CMPFC \control, comisd, D_DEN, D_ZERO
    TOINTC \control, "cvtsd2si eax", D_DEN
    SDC \control, addss, S_DEN, S_DEN
    .endr

    EXIT 0
