/* hlt is privileged: #GP, delivered as SIGSEGV. */
#include "dump.h"

    .text
    .globl _start
_start:
    hlt
    EXIT 0

    .section .note.GNU-stack, "", @progbits
