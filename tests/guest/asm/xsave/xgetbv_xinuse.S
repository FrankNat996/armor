/* xgetbv with ecx = 1 (XINUSE) without XGETBV_ECX_1 in CPUID raises #GP: SIGSEGV. */
#include "dump.h"

    .text
    .globl _start
_start:
    mov ecx, 1
    xgetbv
    EXIT 0

    .section .note.GNU-stack, "", @progbits
