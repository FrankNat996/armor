/*
 * Without BMI1 in CPUID (the default model) andn, a VEX-encoded BMI1
 * instruction, raises #UD: SIGILL, as on a CPU without it.
 */
#include "dump.h"

    .text
    .globl _start
_start:
    andn rax, rbx, rcx
    EXIT 0

    .section .note.GNU-stack, "", @progbits
