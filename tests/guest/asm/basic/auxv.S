/*
 * Auxiliary vector entries whose values do not depend on the host:
 * AT_PHDR, AT_PHENT, AT_PHNUM, AT_PAGESZ, AT_BASE, AT_ENTRY.
 * Exercises [base + index*8 + disp] addressing, loops and conditional jumps.
 */
#include "dump.h"

#define AT_NULL 0

    .bss
auxv_start:
    .zero 8

/* Dumps the value of auxv entry `type`, or -1 if it is missing. */
.macro AUXV type
    mov rsi, [rip + auxv_start]
1:
    mov rax, [rsi]
    cmp rax, \type
    je 2f
    cmp rax, AT_NULL
    je 3f
    add rsi, 16
    jmp 1b
2:
    mov rax, [rsi + 8]
    jmp 4f
3:
    mov rax, -1
4:
    DUMP rax
.endm

    .text
    .globl _start
_start:
    mov rcx, [rsp]              /* argc */
    lea rsi, [rsp + rcx*8 + 16] /* envp */
1:
    mov rax, [rsi]
    add rsi, 8
    test rax, rax
    jne 1b
    mov [rip + auxv_start], rsi

    AUXV 3                      /* AT_PHDR */
    AUXV 4                      /* AT_PHENT */
    AUXV 5                      /* AT_PHNUM */
    AUXV 6                      /* AT_PAGESZ */
    AUXV 7                      /* AT_BASE */
    AUXV 9                      /* AT_ENTRY */
    EXIT 0

    .section .note.GNU-stack, "", @progbits
