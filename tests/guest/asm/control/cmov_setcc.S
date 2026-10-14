/*
 * cmovcc and setcc for all conditions after cmp. A 32-bit cmov
 * zero-extends its destination even when the condition is false.
 */
#include "dump.h"

.macro COND a, b
    mov rax, \a
    mov rcx, \b
    cmp rax, rcx
    mov rdx, 0x1111111111111111
    mov rbx, -1
    cmove rdx, rbx
    DUMP rdx
    mov rdx, 0x1111111111111111
    cmovne edx, ebx
    DUMP rdx
    mov rdx, 0x2222222222222222
    cmovl rdx, rbx
    DUMP rdx
    mov rdx, 0x3333333333333333
    cmova rdx, rbx
    DUMP rdx
    mov rdx, 0x4444444444444444
    cmovbe dx, bx
    DUMP rdx
    xor edx, edx
    cmp rax, rcx
    seto dl
    shl edx, 1
    cmp rax, rcx
    setb dl
    shl edx, 1
    cmp rax, rcx
    sete dl
    shl edx, 1
    cmp rax, rcx
    setbe dl
    shl edx, 1
    cmp rax, rcx
    sets dl
    shl edx, 1
    cmp rax, rcx
    setp dl
    shl edx, 1
    cmp rax, rcx
    setl dl
    shl edx, 1
    cmp rax, rcx
    setle dl
    shl edx, 1
    cmp rax, rcx
    setg dl
    shl edx, 1
    cmp rax, rcx
    setge dl
    shl edx, 1
    cmp rax, rcx
    seta dl
    shl edx, 1
    cmp rax, rcx
    setae dl
    shl edx, 1
    cmp rax, rcx
    setne dl
    shl edx, 1
    cmp rax, rcx
    setno dl
    shl edx, 1
    cmp rax, rcx
    setns dl
    shl edx, 1
    cmp rax, rcx
    setnp dl
    DUMP rdx
.endm

    .bss
buf:
    .zero 8

    .text
    .globl _start
_start:
    COND 1, 1
    COND 1, 2
    COND 2, 1
    COND -1, 1
    COND 0x8000000000000000, 1
    COND 3, 0

    /* setcc to memory and with a REX byte register. */
    lea rbx, [rip + buf]
    mov qword ptr [rbx], -1
    xor eax, eax
    sete byte ptr [rbx + 2]
    setne sil
    mov rax, [rbx]
    DUMP rax
    movzx eax, sil
    DUMP rax
    /* cmov with a memory source. */
    mov qword ptr [rbx], 77
    mov rax, 1
    cmp rax, 1
    cmove rax, qword ptr [rbx]
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
