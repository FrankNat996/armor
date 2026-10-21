/*
 * vmovaps with a 256-bit operand needs 32-byte alignment: an address
 * aligned to 16 only raises #GP (SIGSEGV).
 */
#include "dump.h"

    .bss
    .balign 32
buf:
    .zero 64

    .text
    .globl _start
_start:
    vmovups ymm0, [rip + buf + 16]
    vmovaps ymm0, [rip + buf + 16]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
