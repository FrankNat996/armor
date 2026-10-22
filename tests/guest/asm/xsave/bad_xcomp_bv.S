/*
 * xrstor of an area whose XCOMP_BV (header bytes 8..15) is not zero
 * raises #GP (SIGSEGV): the compacted format needs XSAVEC, which is not
 * declared.
 */
#include "dump.h"

    .bss
    .balign 64
area:
    .zero 576

    .text
    .globl _start
_start:
    mov eax, 3
    xor edx, edx
    xsave [rip + area]
    mov rax, 0x8000000000000003
    mov qword ptr [rip + area + 520], rax
    mov eax, 3
    xor edx, edx
    xrstor [rip + area]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
