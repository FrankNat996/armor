/* Reading address 0 ends the process with SIGSEGV. */
#include "dump.h"

    .text
    .globl _start
_start:
    xor eax, eax
    mov rax, [rax]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
