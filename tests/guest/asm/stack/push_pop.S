/*
 * push and pop: immediates are sign-extended to 64 bits, push rsp pushes
 * the old value, pushfq pushes RFLAGS.
 */
#include "dump.h"

    .text
    .globl _start
_start:
    mov rbx, 0x7777
    mov rbp, rsp
    push -1                             /* 6A ib */
    pop rax
    DUMP rax
    push -0x12345                       /* 68 id */
    pop rax
    DUMP rax
    push 0x7f
    pop rax
    DUMP rax

    mov r15, 0x1122334455667788
    push r15                            /* REX.B */
    pop r8
    DUMP r8

    push 1
    push 2
    mov rax, [rsp + 8]
    DUMP rax
    mov rax, [rsp]
    DUMP rax
    pop rax
    pop rax

    push rsp
    pop rax
    sub rax, rsp
    DUMP rax                            /* 0: the pushed value is the old rsp */
    mov rax, rbp
    sub rax, rsp
    DUMP rax                            /* 0 */

    push rbx
    pop rbx
    DUMP rbx

    xor eax, eax                        /* ZF and PF set */
    pushfq
    pop rax
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
