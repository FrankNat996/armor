/* xsetbv is privileged: #GP in user mode, SIGSEGV. */
#include "dump.h"

    .text
    .globl _start
_start:
    xor ecx, ecx
    mov eax, 3
    xor edx, edx
    xsetbv
    EXIT 0

    .section .note.GNU-stack, "", @progbits
