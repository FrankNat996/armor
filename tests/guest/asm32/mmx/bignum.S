/*
 * i386 MMX (ADR-0029): the forms the Steam client uses most (movq, pxor,
 * paddq, psrlq, psllq, pmuludq, movd, pand, por, pinsrw), as in 64-bit
 * limb arithmetic: a 4-limb product with carries through 32-bit halves.
 */
#include "dump32.h"

    .section .rodata
    .balign 8
x:  .long 0xffffffff, 0x89abcdef, 0x01234567, 0xdeadbeef
y:  .long 0x9e3779b9, 0xffffffff, 0x7f4a7c15, 0x00000003
low: .quad 0x00000000ffffffff

    .bss
    .balign 8
prod: .zero 32
out: .zero 8

    .text
.macro DUMPM r
    movq [out], \r
    DUMP_WRITE out, 8
.endm

    .globl _start
_start:
    /* prod = x[0..3] * y[0] with 64-bit carries in mm registers. */
    pxor mm7, mm7                  /* carry */
    movd mm6, [y]
    xor esi, esi
1:  movd mm0, [x + esi * 4]
    pmuludq mm0, mm6               /* 32 x 32 -> 64 */
    paddq mm0, mm7
    movd [prod + esi * 4], mm0
    psrlq mm0, 32
    movq mm7, mm0
    inc esi
    cmp esi, 4
    jb 1b
    movd [prod + 16], mm7
    DUMP_WRITE prod, 20

    /* Shifts and masks across 64 bits. */
    movq mm1, [x]
    movq mm2, mm1
    psllq mm2, 17
    psrlq mm1, 47
    por mm1, mm2                   /* rotate left by 17 */
    DUMPM mm1
    pand mm1, [low]
    DUMPM mm1
    mov eax, 0x5a5a
    pinsrw mm1, eax, 3
    DUMPM mm1
    paddq mm1, [y]
    DUMPM mm1
    movd eax, mm1
    DUMP eax
    emms
    EXIT 0

    .section .note.GNU-stack, "", @progbits
