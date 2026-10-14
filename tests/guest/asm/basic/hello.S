/*
 * Prints "hello" to stdout through write(2), then exits with status 0.
 * M1 acceptance test: adds `lea` with RIP-relative addressing and a
 * read-only data segment.
 */
#include <asm/unistd_64.h>

    .intel_syntax noprefix

    .section .rodata
msg:
    .ascii "hello\n"
    msg_len = . - msg

    .text
    .globl _start
_start:
    mov eax, __NR_write
    mov edi, 1                  /* stdout */
    lea rsi, [rip + msg]
    mov edx, msg_len
    syscall

    mov eax, __NR_exit_group
    xor edi, edi
    syscall

    .section .note.GNU-stack, "", @progbits
