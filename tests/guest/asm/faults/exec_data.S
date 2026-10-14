/* Jumping into a non-executable segment ends the process with SIGSEGV. */
#include "dump.h"

    .data
code_in_data:
    .byte 0xc3                          /* ret */

    .text
    .globl _start
_start:
    call code_in_data
    EXIT 0

    .section .note.GNU-stack, "", @progbits
