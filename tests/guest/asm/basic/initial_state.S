/*
 * Registers, flags and stack at process entry: every register but rsp is
 * zero, RFLAGS is 0x202, rsp is 16-byte aligned and points at argc.
 * rsp itself is not dumped: its value depends on where the stack is mapped.
 */
#include "dump.h"

    .text
    .globl _start
_start:
    DUMP_FLAGS -1
    DUMP rax
    DUMP rbx
    DUMP rcx
    DUMP rdx
    DUMP rsi
    DUMP rdi
    DUMP rbp
    DUMP r8
    DUMP r9
    DUMP r10
    DUMP r11
    DUMP r12
    DUMP r13
    DUMP r14
    DUMP r15
    mov rax, rsp
    and rax, 15
    DUMP rax                    /* rsp % 16 */
    mov rax, [rsp]
    DUMP rax                    /* argc */
    mov rax, [rsp + 16]
    DUMP rax                    /* argv[argc] == NULL */
    EXIT 0

    .section .note.GNU-stack, "", @progbits
