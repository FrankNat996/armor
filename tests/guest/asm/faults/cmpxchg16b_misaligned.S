/* cmpxchg16b needs a 16-byte aligned operand: #GP, SIGSEGV. */
#include "dump.h"

    .bss
    .balign 16
buf:
    .zero 32

    .text
    .globl _start
_start:
    cmpxchg16b [rip + buf + 8]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
