/* ud2 raises #UD: SIGILL. */
#include "dump.h"

    .text
    .globl _start
_start:
    ud2
    EXIT 0

    .section .note.GNU-stack, "", @progbits
