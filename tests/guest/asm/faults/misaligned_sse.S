/* movdqa with a misaligned address raises #GP: SIGSEGV. */
#include "dump.h"

    .bss
    .balign 16
buf:
    .zero 32

    .text
    .globl _start
_start:
    movdqa xmm0, [rip + buf + 8]
    EXIT 0

    .section .note.GNU-stack, "", @progbits
