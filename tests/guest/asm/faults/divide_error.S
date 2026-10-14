/* Division by zero raises #DE: SIGFPE. */
#include "dump.h"

    .text
    .globl _start
_start:
    mov rax, 1
    xor ecx, ecx
    xor edx, edx
    div rcx
    EXIT 0

    .section .note.GNU-stack, "", @progbits
