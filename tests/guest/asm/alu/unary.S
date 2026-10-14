/*
 * inc, dec, not, neg at every operand size, on registers and memory.
 * inc and dec leave CF unchanged; neg sets CF unless the operand is zero.
 */
#include "dump.h"

/* rax = a; set CF to cf; run `op dst`; dump rax and the flags. */
.macro UN op, dst, a, cf
    mov rax, \a
    mov rdx, 0
    cmp rdx, \cf
    \op \dst
    DUMP rax
    DUMP_FLAGS FLAGS_STATUS
.endm

    .bss
buf:
    .zero 8

    .text
    .globl _start
_start:
    UN inc, al, 0x7f, 1
    UN inc, al, 0xff, 0
    UN inc, ax, 0xffff, 1
    UN inc, eax, 0xffffffff, 0
    UN inc, rax, -1, 1
    UN dec, al, 0x80, 0
    UN dec, al, 0, 1
    UN dec, cx, 1, 0
    UN dec, eax, 0, 1
    UN dec, rax, 0x8000000000000000, 0
    UN not, al, 0x0f, 1
    UN not, eax, 0, 0
    UN not, rax, 0x5555555555555555, 1
    UN neg, al, 0x80, 0
    UN neg, al, 0, 1
    UN neg, ax, 1, 0
    UN neg, eax, 0x80000000, 0
    UN neg, rax, 5, 0
    UN neg, rax, 0, 1

    lea rbx, [rip + buf]
    mov qword ptr [rbx], -1
    inc qword ptr [rbx]
    DUMP_FLAGS FLAGS_STATUS
    dec byte ptr [rbx + 1]
    neg word ptr [rbx + 2]
    not dword ptr [rbx + 4]
    lock inc dword ptr [rbx]
    mov rax, [rbx]
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
