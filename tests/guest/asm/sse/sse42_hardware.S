/*
 * SSE4.2 where qemu differs from the hardware (an Intel Xeon X5675,
 * docs/testing.md): with REX.W, pcmpestri and pcmpestrm take their
 * lengths from the whole of rax and rdx; qemu reads only eax and edx.
 * Runs only against the hardware oracle (`cargo xtask difftest
 * --oracle`).
 */
#include "dump.h"

#define SET0 0x00000000002c6f77  /* "wo," */

    .data
text:
    .ascii "hello, world"
    .byte 0, 0, 0, 0

    .text
    .globl _start
_start:
    SETX xmm1, SET0, 0
    SETX xmm2, 0x77202c6f6c6c6568, 0x00000000646c726f
    /* Lengths whose low 32 bits differ from the whole: 2^32 + 3 is 16
     * valid elements, not 3; -2^32 is 16, not 0. */
    .irp rax_value, 0x100000003, -0x100000000, 3
    .irp rdx_value, 12, 0x100000000, -0xfffffff4
    mov rax, \rax_value
    mov rdx, \rdx_value
    /* pcmpestri xmm1, xmm2, 0 with REX.W (66 48 0F 3A 61 CA 00). */
    .byte 0x66, 0x48, 0x0f, 0x3a, 0x61, 0xca, 0x00
    DUMP rcx
    DUMP_FLAGS FLAGS_STATUS
    mov rax, \rax_value
    mov rdx, \rdx_value
    /* pcmpestrm xmm1, xmm2, 0x40 with REX.W (66 48 0F 3A 60 CA 40). */
    .byte 0x66, 0x48, 0x0f, 0x3a, 0x60, 0xca, 0x40
    DUMPX xmm0
    DUMP_FLAGS FLAGS_STATUS
    .endr
    .endr

    EXIT 0
