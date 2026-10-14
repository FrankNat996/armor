/*
 * A syscall returns its result in rax and preserves every register except
 * rcx and r11.
 *
 * rcx and r11 are not dumped: SYSCALL sets rcx to the next rip and r11 to
 * RFLAGS (Intel SDM), but qemu-x86_64 leaves them unchanged, so they cannot
 * be compared here. The armor-cpu unit tests check them instead
 * (docs/testing.md, known qemu discrepancies).
 */
#include "dump.h"

    .section .rodata
msg:
    .ascii "!"

    .text
    .globl _start
_start:
    mov rbx, 0x0b0b
    mov rbp, 0x0d0d
    mov r8, 0x0808
    mov r9, 0x0909
    mov r10, 0x1010
    mov r12, 0x1212
    mov r15, 0x1515
    mov eax, __NR_write
    mov edi, 1
    lea rsi, [rip + msg]
    mov edx, 1
    syscall
    DUMP rax                    /* bytes written */
    DUMP rdi
    lea rax, [rip + msg]
    sub rsi, rax
    DUMP rsi                    /* 0: still msg */
    DUMP rdx
    DUMP rbx
    DUMP rbp
    DUMP r8
    DUMP r9
    DUMP r10
    DUMP r12
    DUMP r15
    EXIT 0

    .section .note.GNU-stack, "", @progbits
