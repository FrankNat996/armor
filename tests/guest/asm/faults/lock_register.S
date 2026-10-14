/* LOCK on a register destination raises #UD: SIGILL. */
#include "dump.h"

    .text
    .globl _start
_start:
    .byte 0xf0, 0x01, 0xd8      /* lock add eax, ebx */
    EXIT 0

    .section .note.GNU-stack, "", @progbits
