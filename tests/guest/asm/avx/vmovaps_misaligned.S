/*
 * vmovaps keeps the alignment requirement of movaps: a misaligned
 * address raises #GP (SIGSEGV), while other VEX instructions accept it
 * (avx/vex128).
 */
#include "dump.h"

    .bss
    .balign 16
buf:
    .zero 32

    .text
    .globl _start
_start:
    vaddps xmm1, xmm0, [rip + buf + 8]
    vmovaps xmm0, [rip + buf + 8]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
