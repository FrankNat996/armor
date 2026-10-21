/*
 * XSAVE with XCR0 = x87 | SSE (run with `+xsave` declared, difftest's
 * CASE_FEATURES): xgetbv, CPUID leaf 0xd, then xsave of a 576-byte area
 * prefilled with a pattern under several masks in edx:eax (which bytes
 * each component writes, and XSTATE_BV), and xrstor with components in
 * and out of XSTATE_BV (loaded, or set to their initial state). The
 * dynamic loader's symbol resolver saves the vector registers this way
 * once XSAVE is declared (docs/tech/avx.md).
 */
#include "dump.h"

/*
 * As in sse/state.S: the reserved bytes after each 80-bit register and
 * FOP, which armor writes as zeros and qemu leaves as they were.
 */
.macro CLEAR_X87_UNDEFINED base
    .irp i, 0, 1, 2, 3, 4, 5, 6, 7
    mov dword ptr [rip + \base + 42 + 16 * \i], 0
    mov word ptr [rip + \base + 46 + 16 * \i], 0
    .endr
    mov word ptr [rip + \base + 6], 0
.endm

/* Fills the area with 0xa5. */
.macro FILL
    lea rdi, [rip + area]
    mov ecx, 576
    mov al, 0xa5
    rep stosb
.endm

/* xsave with edx:eax = hi:lo into the prefilled area, then dumps it. */
.macro SAVE lo, hi
    FILL
    mov eax, \lo
    mov edx, \hi
    xsave [rip + area]
    CLEAR_X87_UNDEFINED area
    DUMP_WRITE area, 576
.endm

/* The state after an xrstor: fxsave's image of it (x87, MXCSR, xmm). */
.macro DUMP_STATE
    fxsave [rip + after]
    CLEAR_X87_UNDEFINED after
    DUMP_WRITE after, 416
.endm

.macro LEAF leaf, sub
    mov eax, \leaf
    mov ecx, \sub
    cpuid
    DUMP rax
    DUMP rbx
    DUMP rcx
    DUMP rdx
.endm

    .bss
    .balign 64
area:
    .zero 576
    .balign 64
saved:
    .zero 576
    .balign 16
after:
    .zero 512
mxcsr:
    .zero 4

    .text
    .globl _start
_start:
    xor ecx, ecx
    mov rax, -1
    mov rdx, -1
    xgetbv
    DUMP rax
    DUMP rdx
    LEAF 1, 0
    LEAF 0xd, 0
    LEAF 0xd, 1
    LEAF 0xd, 2
    LEAF 0xe, 0

    /* A state to save: two x87 values, xmm registers, MXCSR with PE. */
    fld1
    fldpi
    SETX xmm0, 0x0011223344556677, 0x8899aabbccddeeff
    SETX xmm9, 0x0101010101010101, 0x0202020202020202
    SETX xmm15, 0xfedcba9876543210, 0x0f0e0d0c0b0a0908
    mov dword ptr [rip + mxcsr], 0x1fa0
    ldmxcsr [rip + mxcsr]

    /* Nothing requested: only XSTATE_BV is written, unchanged. */
    SAVE 0, 0
    /* x87 only: bytes 0..24 and 32..160, XSTATE_BV bit 0. */
    SAVE 1, 0
    /* SSE only: MXCSR, MXCSR_MASK and the xmm registers, bit 1. */
    SAVE 2, 0
    /* AVX only, which XCR0 does not enable: as with nothing. */
    SAVE 4, 0
    /* Everything: what XCR0 enables. */
    SAVE -1, -1
    /* The dynamic loader's mask (SSE, AVX, AVX-512 and more, no x87). */
    SAVE 0x800ee, 0
    /* xsave64: the same with REX.W. */
    FILL
    mov eax, -1
    mov edx, -1
    xsave64 [rip + area]
    CLEAR_X87_UNDEFINED area
    DUMP_WRITE area, 576

    /* Keep a full image to restore from. */
    FILL
    mov eax, 3
    xor edx, edx
    xsave [rip + saved]

    /* Every component from the image: the state comes back. */
    fninit
    pxor xmm0, xmm0
    pxor xmm9, xmm9
    pxor xmm15, xmm15
    mov eax, 3
    xor edx, edx
    xrstor [rip + saved]
    DUMP_STATE

    /*
     * XSTATE_BV 0: both components to their initial state; MXCSR still
     * comes from the image (0x1f81 here).
     */
    mov dword ptr [rip + saved + 24], 0x1f81
    mov qword ptr [rip + saved + 512], 0
    mov eax, 3
    xor edx, edx
    xrstor [rip + saved]
    DUMP_STATE

    /* XSTATE_BV SSE only: xmm loaded, x87 initial. */
    fld1
    mov qword ptr [rip + saved + 512], 2
    mov eax, 3
    xor edx, edx
    xrstor [rip + saved]
    DUMP_STATE

    /* Requested x87 only: the xmm registers keep their values. */
    SETX xmm0, 0x1111111111111111, 0x2222222222222222
    mov qword ptr [rip + saved + 512], 3
    mov eax, 1
    xor edx, edx
    xrstor [rip + saved]
    DUMP_STATE

    /* Nothing requested: nothing changes. */
    fninit
    SETX xmm9, 0x3333333333333333, 0x4444444444444444
    xor eax, eax
    xor edx, edx
    xrstor [rip + saved]
    DUMP_STATE

    /* xrstor64 of everything. */
    mov eax, -1
    mov edx, -1
    xrstor64 [rip + saved]
    DUMP_STATE
    EXIT 0

    .section .note.GNU-stack, "", @progbits
