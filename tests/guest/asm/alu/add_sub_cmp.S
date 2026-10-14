/*
 * add, sub, cmp: result and all six status flags at every operand size,
 * with register, immediate (imm8 sign-extended, imm32, AL/eAX short forms)
 * and memory operands.
 */
#include "dump.h"

/* rax = a, rcx = b; run `op dst, src`; dump rax and the status flags. */
.macro BIN op, dst, src, a, b
    mov rax, \a
    mov rcx, \b
    \op \dst, \src
    DUMP rax
    DUMP_FLAGS FLAGS_STATUS
.endm

    .bss
buf:
    .zero 16

    .text
    .globl _start
_start:
    /* add, 8 bits: signed overflow; carry with zero result. */
    BIN add, al, cl, 0x7f, 1
    BIN add, al, cl, 0xff, 1
    BIN add, al, 0x7f, 0x01, 0          /* 04 ib */
    BIN add, ah, cl, 0x8000, 0x80       /* high byte */
    /* add, 16 and 32 bits: upper bits preserved / zero-extended. */
    BIN add, ax, cx, 0x1234ffff, 1
    BIN add, eax, ecx, 0xffffffff, 1
    BIN add, eax, ecx, 0x7fffffff, 1
    BIN add, eax, 1, 0x0f, 0            /* 83 /0 ib */
    BIN add, eax, 0x12345678, 1, 0      /* 05 id */
    /* add, 64 bits. */
    BIN add, rax, rcx, -1, 1
    BIN add, rax, rcx, 0x7fffffffffffffff, 1
    BIN add, rax, -1, 5, 0              /* sign-extended imm8 */
    BIN add, rax, 0x7fffffff, 1, 0      /* sign-extended imm32 */
    BIN add, rax, -0x80000000, 0, 0

    /* sub. */
    BIN sub, al, cl, 0, 1
    BIN sub, al, cl, 0x80, 1
    BIN sub, ax, cx, 0x10000, 1
    BIN sub, eax, ecx, 0, 1
    BIN sub, eax, 0x10, 0x10, 0         /* 83 /5 ib */
    BIN sub, eax, 0x12345, 0x12345, 0   /* 2D id */
    BIN sub, rax, rcx, 0x8000000000000000, 1
    BIN sub, rax, rcx, 5, 5
    BIN sub, rax, rcx, 0x10, 0x01       /* auxiliary borrow */

    /* cmp: flags only, rax unchanged. */
    BIN cmp, rax, rcx, 1, 1
    BIN cmp, rax, rcx, 1, 2
    BIN cmp, rax, rcx, 2, 1
    BIN cmp, eax, ecx, 0x80000000, 1
    BIN cmp, al, 0x80, 0x7f, 0
    BIN cmp, rax, -1, -1, 0

    /* Memory destinations and sources. */
    lea rbx, [rip + buf]
    mov qword ptr [rbx], 5
    add qword ptr [rbx], -6
    DUMP_FLAGS FLAGS_STATUS
    mov rax, [rbx]
    DUMP rax
    mov rax, 10
    sub rax, [rbx]                      /* 10 - (-1) */
    DUMP rax
    DUMP_FLAGS FLAGS_STATUS
    mov byte ptr [rbx + 8], 0xff
    add byte ptr [rbx + 8], 1
    DUMP_FLAGS FLAGS_STATUS
    mov rax, [rbx + 8]
    DUMP rax
    cmp byte ptr [rbx + 8], 0
    DUMP_FLAGS FLAGS_STATUS
    EXIT 0

    .section .note.GNU-stack, "", @progbits
