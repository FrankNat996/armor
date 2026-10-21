/*
 * xrstor checks the XSAVE header before loading anything: it raises #GP
 * (SIGSEGV) for an XSTATE_BV bit that XCR0 does not enable. First a valid
 * header, restored (a marker is written), then XSTATE_BV with AVX, which
 * XCR0 = x87 | SSE does not enable.
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
    xrstor [rip + area]
    mov rax, 0x1111
    DUMP rax
    mov qword ptr [rip + area + 512], 7
    mov eax, 3
    xor edx, edx
    xrstor [rip + area]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
