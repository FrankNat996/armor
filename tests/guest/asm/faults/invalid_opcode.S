/* daa (27) is invalid in 64-bit mode: #UD, delivered as SIGILL. */
#include "dump.h"

    .text
    .globl _start
_start:
    .byte 0x27
    EXIT 0

    .section .note.GNU-stack, "", @progbits
