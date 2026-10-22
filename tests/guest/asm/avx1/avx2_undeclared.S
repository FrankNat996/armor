/*
 * With AVX declared and AVX2 not (`+xsave,+avx`), the 256-bit float forms
 * run and the 256-bit integer ones, AVX2 instructions, raise #UD: SIGILL.
 */
#include "dump.h"

    .text
    .globl _start
_start:
    vaddps ymm0, ymm1, ymm2
    vxorps ymm3, ymm3, ymm3
    vpaddd ymm0, ymm1, ymm2
    EXIT 0

    .section .note.GNU-stack, "", @progbits
