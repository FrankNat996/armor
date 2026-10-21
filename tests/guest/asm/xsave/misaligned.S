/* xsave with an area aligned to 16 bytes, not 64, raises #GP: SIGSEGV. */
#include "dump.h"

    .bss
    .balign 64
area:
    .zero 1024

    .text
    .globl _start
_start:
    mov eax, 3
    xor edx, edx
    xsave [rip + area + 16]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
