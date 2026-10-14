/*
 * MMX and the x87 registers: an MMX instruction sets TOP to 0 and every
 * tag to valid; writing mmN puts the value in the significand of RN and
 * sets its exponent and sign bits (SDM vol. 1, 9.5.1: qemu leaves them at
 * 0, so this runs with --oracle only); emms empties the tags, and x87 code
 * works again.
 */
#include "dump.h"

    .section .rodata
c:  .quad 0x0123456789abcdef

    .bss
    .balign 16
out: .zero 112
env: .zero 28

    .text
    .globl _start
_start:
    fld1
    fldpi
    movq mm0, [rip + c]
    /* The status and tag words only: the reserved halves are 0xffff on
     * hardware, 0 under qemu (docs/testing.md). */
    fnstenv [rip + env]
    DUMP_WRITE env + 4, 2
    DUMP_WRITE env + 8, 2
    fnsave [rip + out]
    DUMP_WRITE out + 28, 10
    frstor [rip + out]
    emms
    fnstenv [rip + env]
    DUMP_WRITE env + 8, 2
    fld1
    fstp qword ptr [rip + out]
    DUMP_WRITE out, 8

    EXIT 0

    .section .note.GNU-stack, "", @progbits
