/*
 * mul and imul (one, two and three operands) and div/idiv at every size.
 * After multiplication only CF and OF are defined; after division no flag
 * is.
 */
#include "dump.h"

#define FLAGS_MUL 0x801

.macro MUL1 op, src, a, b, d
    mov rax, \a
    mov rcx, \b
    mov rdx, \d
    \op \src
    DUMP rax
    DUMP rdx
    DUMP_FLAGS FLAGS_MUL
.endm

.macro DIV1 op, src, a, b, d
    mov rax, \a
    mov rcx, \b
    mov rdx, \d
    \op \src
    DUMP rax
    DUMP rdx
.endm

    .bss
buf:
    .zero 8

    .text
    .globl _start
_start:
    MUL1 mul, cl, 0xff, 0xff, 0x5555
    MUL1 mul, cl, 0x10, 0x0f, 0
    MUL1 mul, cx, 0xffff, 2, 0x5555
    MUL1 mul, ecx, 0xffffffff, 0xffffffff, -1
    MUL1 mul, rcx, -1, -1, 0
    MUL1 mul, rcx, 3, 4, 7
    MUL1 imul, cl, 0x80, 0x80, 0
    MUL1 imul, cl, 0x7f, 1, 0
    MUL1 imul, cx, -2, 3, 0
    MUL1 imul, ecx, 0x80000000, -1, 0
    MUL1 imul, rcx, -3, 7, 0
    MUL1 imul, rcx, 0x4000000000000000, 2, 0

    /* Two- and three-operand imul. */
    mov rax, 1000
    mov rcx, -3
    imul rax, rcx
    DUMP rax
    DUMP_FLAGS FLAGS_MUL
    mov eax, 0x10000
    imul eax, eax
    DUMP rax
    DUMP_FLAGS FLAGS_MUL
    mov rcx, 12
    imul rax, rcx, 100
    DUMP rax
    imul rax, rcx, -1000000
    DUMP rax
    imul cx, cx, 0x4000
    DUMP rcx
    DUMP_FLAGS FLAGS_MUL
    lea rbx, [rip + buf]
    mov qword ptr [rbx], 9
    imul rax, qword ptr [rbx], 9
    DUMP rax

    DIV1 div, cl, 1000, 7, 0
    DIV1 div, cx, 0xffff, 0x100, 0x00ff
    DIV1 div, ecx, 0xffffffff, 10, 9
    DIV1 div, rcx, -1, 3, 2
    DIV1 idiv, cl, -100, 7, 0
    DIV1 idiv, cx, 0xfff0, 3, 0xffff
    DIV1 idiv, ecx, -7, 2, 0xffffffff
    DIV1 idiv, rcx, -7, -2, -1
    DIV1 idiv, rcx, 7, -2, 0
    mov qword ptr [rbx], 1000
    mov rax, 123456
    xor edx, edx
    div qword ptr [rbx]
    DUMP rax
    DUMP rdx
    EXIT 0

    .section .note.GNU-stack, "", @progbits
