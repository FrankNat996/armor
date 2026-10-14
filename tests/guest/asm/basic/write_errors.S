/*
 * write(2) errors come back as -errno in rax.
 */
#include "dump.h"

    .text
    .globl _start
_start:
    mov eax, __NR_write
    mov edi, 1
    xor esi, esi                /* NULL buffer */
    mov edx, 8
    syscall
    DUMP rax                    /* -EFAULT */

    mov eax, __NR_write
    mov edi, 99                 /* not an open file descriptor */
    lea rsi, [rip + _start]
    mov edx, 1
    syscall
    DUMP rax                    /* -EBADF */

    mov eax, __NR_write
    mov edi, 1
    lea rsi, [rip + _start]
    xor edx, edx                /* zero bytes */
    syscall
    DUMP rax                    /* 0 */
    EXIT 0

    .section .note.GNU-stack, "", @progbits
