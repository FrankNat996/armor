/*
 * xrstor of an area with a reserved header byte set (byte 16, after
 * XCOMP_BV) raises #GP (SIGSEGV), as the Intel SDM's standard-form check
 * of header bytes 8..23 says.
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
    mov byte ptr [rip + area + 528], 1
    mov eax, 3
    xor edx, edx
    xrstor [rip + area]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
