/* int3 raises #BP: SIGTRAP. */
#include "dump.h"

    .text
    .globl _start
_start:
    int3
    EXIT 0

    .section .note.GNU-stack, "", @progbits
