/*
 * cmpxchg8b writes its operand back even when the comparison fails, so a
 * read-only operand faults either way: SIGSEGV.
 */
#include "dump.h"

    .section .rodata
    .balign 8
constant:
    .quad 0x1234

    .text
    .globl _start
_start:
    xor eax, eax                       /* does not match */
    xor edx, edx
    cmpxchg8b [rip + constant]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
