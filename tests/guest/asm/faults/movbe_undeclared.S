/* Without MOVBE in CPUID (the default model) movbe raises #UD: SIGILL. */
#include "dump.h"

    .text
    .globl _start
_start:
    movbe rax, [rsp]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
