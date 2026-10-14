/*
 * lea computes addresses without touching memory or flags, at every size.
 */
#include "dump.h"

    .text
    .globl _start
_start:
    mov rbx, 0x1000
    mov rcx, 3
    lea rax, [rbx + rcx*4 + 0x10]
    DUMP rax
    lea rax, [rbx + rcx*8 - 1]
    DUMP rax
    mov rbx, 0xffffffff
    lea eax, [rbx + 2]          /* 32-bit: wraps and zero-extends */
    DUMP rax
    mov rax, -1
    lea ax, [rcx + 1]           /* 16-bit: preserves the rest */
    DUMP rax
    lea rax, [rip + here]
here:
    lea rcx, [rip + here]
    sub rax, rcx
    DUMP rax                    /* 0 */
    mov rcx, 0x10
    lea rax, [rcx*2]            /* no base: SIB with base=101, mod=00 */
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
