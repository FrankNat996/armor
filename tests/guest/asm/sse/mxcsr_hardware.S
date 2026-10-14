/*
 * MXCSR exception flags where qemu differs from the hardware (an Intel
 * Xeon X5675, docs/testing.md): qemu never raises DE for a denormal
 * operand, propagates the second NaN when the first is signalling and
 * the second quiet, detects tininess before rounding with FZ, and with
 * DAZ returns the denormal itself from min. Runs only against the hardware oracle (`cargo xtask
 * difftest --oracle`). Same layout as sse/mxcsr_flags.
 */
#include "dump.h"
#include "mxcsr.h"

    .text
    .globl _start
_start:
    /* DE with the other flags: PE, UE; left out by IE and ZE. */
    SD addsd, D_DEN, D_ONE
    SD addsd, D_DEN, D_DEN
    SD mulsd, D_DEN, D_ZERO
    SD mulsd, D_DEN, D_HALF
    SD divsd, D_DEN, D_ZERO
    SD divsd, D_ZERO, D_DEN
    SD mulsd, D_DEN, D_INF
    SD subsd, D_MDEN, D_ONE
    SD addss, S_DEN, S_ONE
    SD sqrtsd, 0, D_DEN
    SD sqrtsd, 0, D_MDEN
    SD minsd, D_DEN, D_ONE
    SD maxsd, D_ONE, D_MDEN

    /* A NaN operand: no DE. */
    SD addsd, D_DEN, D_QNAN
    SD addsd, D_QNAN, D_DEN
    SD addsd, D_DEN, D_SNAN
    SD minsd, D_DEN, D_QNAN

    /* The first NaN propagates, also when it is the signalling one. */
    SD addsd, D_SNAN, D_QNAN
    SD mulss, S_SNAN, S_QNAN

    /* Compares. */
    CMPF ucomisd, D_DEN, D_ONE
    CMPF comisd, D_DEN, D_QNAN
    CMPF ucomisd, D_DEN, D_QNAN
    SDI cmpsd, 0, D_DEN, D_ONE
    SDI cmpsd, 1, D_DEN, D_QNAN

    /* Conversions: no DE towards integers. */
    TOINT "cvtsd2si rax", D_DEN
    TOINT "cvttsd2si eax", D_MDEN
    TOINT "cvtss2si eax", S_DEN
    SD cvtsd2ss, 0, D_DEN
    SD cvtss2sd, 0, S_DEN
    PK cvtps2pd, 0, 0, PAIR(S_DEN, S_ONE), 0
    PK cvtpd2ps, 0, 0, D_DEN, D_ONE

    /* Packed: DE from one lane, PE from another. */
    PK addpd, D_DEN, D_ONE, D_ONE, D_THIRD
    PK mulps, PAIR(S_DEN, S_ONE), 0, PAIR(S_ONE, S_ONE), 0

    /* MXCSR control (sse/mxcsr_control has the rest): DE in every
     * rounding mode; FZ with tininess after rounding (the product rounds
     * up to the smallest normal: not flushed), and on exact tiny results
     * (flushed, with UE and PE); DAZ in min, which returns the zero the
     * denormal stands for. */
    .irp control, 0x1f80, 0x3f80, 0x5f80, 0x7f80
    SDC \control, addsd, D_ONE, D_DEN
    SDC \control, subsd, D_ONE, D_DEN
    .endr
    .irp control, 0x9f80, 0x9fc0
    SDC \control, mulsd, D_MIN_PLUS, D_ALMOST_ONE
    .endr
    SDC 0x9f80, mulss, S_ONE, S_DEN
    SDC 0x9f80, addsd, D_DEN, D_DEN
    .irp control, 0x1fc0, 0x9fc0
    SDC \control, minsd, D_ZERO, D_DEN
    .endr

    /* A denormal memory operand. */
    ldmxcsr [rip + mxcsr_default]
    SETX xmm0, D_ONE, 0
    addsd xmm0, [rip + mem_den]
    DUMP_MXCSR
    DUMPX xmm0
    ldmxcsr [rip + mxcsr_default]
    SETX xmm0, D_ONE, 0
    ucomiss xmm0, [rip + mem_den]
    DUMP_MXCSR
    DUMP_FLAGS FLAGS_STATUS

    EXIT 0

    .data
    .balign 8
mem_den:
    .quad D_DEN
