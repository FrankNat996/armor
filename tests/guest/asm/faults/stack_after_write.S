/* Output written before a fault must reach stdout. */
#include "dump.h"

    .text
    .globl _start
_start:
    mov rax, 0x6f6c6c6568
    DUMP rax
    xor ebx, ebx
    mov [rbx], rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
