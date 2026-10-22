/* Without AVX in CPUID (the default model) a VEX.128 SSE form raises #UD: SIGILL. */
#include "dump.h"

    .text
    .globl _start
_start:
    vaddps xmm0, xmm1, xmm2
    EXIT 0

    .section .note.GNU-stack, "", @progbits
