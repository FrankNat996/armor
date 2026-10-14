/* Writing to a read-only segment ends the process with SIGSEGV. */
#include "dump.h"

    .section .rodata
constant:
    .quad 1

    .text
    .globl _start
_start:
    mov qword ptr [rip + constant], 2
    EXIT 0

    .section .note.GNU-stack, "", @progbits
