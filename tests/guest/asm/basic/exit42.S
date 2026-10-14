/*
 * Exits with status 42 through exit_group(2).
 * First M1 acceptance test: needs only `mov` and `syscall`.
 */
#include <asm/unistd_64.h>

    .intel_syntax noprefix

    .text
    .globl _start
_start:
    mov eax, __NR_exit_group
    mov edi, 42
    syscall

    .section .note.GNU-stack, "", @progbits
