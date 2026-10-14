/*
 * fxsave, fxrstor, ldmxcsr, stmxcsr: the whole 512-byte FXSAVE area
 * (prefilled with a pattern, to show which bytes fxsave writes), then a
 * round trip of MXCSR and xmm registers through fxrstor. The dynamic
 * loader's symbol resolver saves and restores registers this way.
 */
#include "dump.h"

/*
 * The 6 reserved bytes after each 80-bit register (FXSAVE offsets
 * 42..47, 58..63, ..., 154..159): an Intel Xeon X5675 and armor write
 * zeros there, qemu leaves them as they were (docs/testing.md). Zeroed
 * here so that the dump compares with qemu; armor's zeros are checked by
 * a unit test.
 */
.macro CLEAR_ST_PADDING
    .irp i, 0, 1, 2, 3, 4, 5, 6, 7
    mov dword ptr [rip + area + 42 + 16 * \i], 0
    mov word ptr [rip + area + 46 + 16 * \i], 0
    .endr
.endm

    .bss
    .balign 16
area:
    .zero 512
mxcsr:
    .zero 4

    .text
    .globl _start
_start:
    /* Fill the area with 0xa5. */
    lea rdi, [rip + area]
    mov ecx, 512
    mov al, 0xa5
    rep stosb

    SETX xmm0, 0x0011223344556677, 0x8899aabbccddeeff
    SETX xmm7, 0x0101010101010101, 0x0202020202020202
    SETX xmm15, 0xfedcba9876543210, 0x0f0e0d0c0b0a0908
    fxsave [rip + area]
    /* FOP: the SDM says fxsave writes it, qemu leaves it unchanged. */
    mov word ptr [rip + area + 6], 0
    CLEAR_ST_PADDING
    DUMP_WRITE area, 512

    /*
     * stmxcsr / ldmxcsr round trip, with every status flag set. armor only
     * supports the default control bits (rounding to nearest, exceptions
     * masked): other values stop it with "not supported yet".
     */
    stmxcsr [rip + mxcsr]
    DUMP_WRITE mxcsr, 4
    mov dword ptr [rip + mxcsr], 0x1fbf
    ldmxcsr [rip + mxcsr]
    mov dword ptr [rip + mxcsr], 0
    stmxcsr [rip + mxcsr]
    DUMP_WRITE mxcsr, 4

    /* fxsave64 (REX.W) of the new MXCSR, then restore from the first save. */
    rex64 fxsave [rip + area]
    mov word ptr [rip + area + 6], 0
    CLEAR_ST_PADDING
    DUMP_WRITE area, 416
    pxor xmm0, xmm0
    pxor xmm7, xmm7
    pxor xmm15, xmm15
    mov dword ptr [rip + area + 24], 0x1f80
    fxrstor [rip + area]
    DUMPX xmm0
    DUMPX xmm7
    DUMPX xmm15
    stmxcsr [rip + mxcsr]
    DUMP_WRITE mxcsr, 4
    /*
     * A reserved MXCSR bit raises #GP in ldmxcsr per the Intel SDM; qemu
     * accepts it, so that case is an armor-cpu unit test instead.
     */
    EXIT 0
