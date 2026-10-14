/*
 * adc and sbb with carry in and out, at every operand size, and a 128-bit
 * addition / subtraction built from them.
 */
#include "dump.h"

/* Sets CF to `cf` (0 or 1) without touching rax/rcx. */
.macro SET_CF cf
    mov rdx, 0
    cmp rdx, \cf                        /* 0 - cf borrows iff cf = 1 */
.endm

.macro BINC op, dst, src, a, b, cf
    mov rax, \a
    mov rcx, \b
    SET_CF \cf
    \op \dst, \src
    DUMP rax
    DUMP_FLAGS FLAGS_STATUS
.endm

    .text
    .globl _start
_start:
    BINC adc, al, cl, 0xff, 0, 1
    BINC adc, al, cl, 0x7f, 0, 1
    BINC adc, ax, cx, 0xfffe, 1, 1
    BINC adc, eax, ecx, 0xffffffff, 0, 1
    BINC adc, rax, rcx, -1, -1, 1
    BINC adc, rax, rcx, 1, 1, 0
    BINC adc, rax, 0, 0, 0, 1           /* 83 /2 ib */

    BINC sbb, al, cl, 0, 0, 1
    BINC sbb, al, cl, 0x80, 0, 1
    BINC sbb, ax, cx, 0, 0xffff, 1
    BINC sbb, eax, ecx, 1, 0, 1
    BINC sbb, rax, rcx, 0, -1, 1
    BINC sbb, rax, rcx, 5, 2, 0
    BINC sbb, rax, 1, 1, 0, 1           /* 83 /3 ib */

    /* 128-bit: (2^64 - 1) + 1 = 2^64, then back. */
    mov rax, -1
    mov rdx, 0
    add rax, 1
    adc rdx, 0
    DUMP rax
    DUMP rdx
    sub rax, 1
    sbb rdx, 0
    DUMP rax
    DUMP rdx
    DUMP_FLAGS FLAGS_STATUS
    EXIT 0

    .section .note.GNU-stack, "", @progbits
