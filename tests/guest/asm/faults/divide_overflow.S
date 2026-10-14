/* A quotient that does not fit raises #DE: SIGFPE. */
#include "dump.h"

    .text
    .globl _start
_start:
    mov rax, 0x8000000000000000
    mov rdx, -1
    mov rcx, -1
    idiv rcx
    EXIT 0

    .section .note.GNU-stack, "", @progbits
