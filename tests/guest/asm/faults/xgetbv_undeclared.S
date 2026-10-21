/*
 * Without XSAVE in CPUID (the default model) xgetbv raises #UD: SIGILL,
 * as on a CPU without XSAVE (docs/tech/avx.md).
 */
#include "dump.h"

    .text
    .globl _start
_start:
    xor ecx, ecx
    xgetbv
    EXIT 0

    .section .note.GNU-stack, "", @progbits
