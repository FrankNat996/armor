/*
 * SSE2 moves: 128-bit aligned and unaligned loads and stores, movd/movq
 * between xmm registers, GPRs and memory, and the 64-bit half moves.
 */
#include "dump.h"

    .section .rodata
    .balign 16
pattern:
    .quad 0x0706050403020100, 0x0f0e0d0c0b0a0908, 0x1716151413121110, 0x1f1e1d1c1b1a1918

    .bss
    .balign 16
out:
    .zero 48

    .text
    .globl _start
_start:
    movdqa xmm0, [rip + pattern]
    DUMPX xmm0
    movdqu xmm1, [rip + pattern + 3]
    DUMPX xmm1
    movaps xmm2, [rip + pattern + 16]
    DUMPX xmm2
    movups xmm3, [rip + pattern + 1]
    DUMPX xmm3
    movapd xmm4, xmm0
    movupd xmm5, [rip + pattern + 7]
    DUMPX xmm4
    DUMPX xmm5
    movdqa [rip + out], xmm1
    movdqu [rip + out + 17], xmm2
    movaps [rip + out + 32], xmm3
    movups [rip + out + 31], xmm0
    mov rax, [rip + out]
    DUMP rax
    mov rax, [rip + out + 24]
    DUMP rax
    mov rax, [rip + out + 40]
    DUMP rax
    movntdq [rip + out], xmm0
    movntps [rip + out + 16], xmm1
    mov rax, [rip + out + 8]
    DUMP rax
    mov rax, [rip + out + 16]
    DUMP rax

    /* movd / movq: to xmm zero the rest, from xmm take the low lanes. */
    mov rax, 0x8877665544332211
    movq xmm6, rax
    DUMPX xmm6
    movd xmm6, eax
    DUMPX xmm6
    movd ecx, xmm0
    DUMP rcx
    movq rcx, xmm1
    DUMP rcx
    movd xmm7, dword ptr [rip + pattern + 4]
    DUMPX xmm7
    movq xmm7, qword ptr [rip + pattern + 8]
    DUMPX xmm7
    movq xmm8, xmm1             /* F3 0F 7E */
    DUMPX xmm8
    movq [rip + out], xmm2      /* 66 0F D6 */
    mov rax, [rip + out]
    DUMP rax
    movd dword ptr [rip + out], xmm0
    mov rax, [rip + out]
    DUMP rax
    SETX xmm9, -1, -1
    .byte 0x66, 0x45, 0x0f, 0xd6, 0xc8      /* movq xmm8, xmm9 (66 0F D6, register form) */
    DUMPX xmm8

    /* 64-bit halves. */
    movdqa xmm10, xmm0
    movhps xmm10, [rip + pattern + 16]
    DUMPX xmm10
    movlps xmm10, [rip + pattern + 24]
    DUMPX xmm10
    movhpd xmm10, [rip + pattern]
    movlpd xmm10, [rip + pattern + 8]
    DUMPX xmm10
    movhps [rip + out], xmm2
    movlpd [rip + out + 8], xmm1
    mov rax, [rip + out]
    DUMP rax
    mov rax, [rip + out + 8]
    DUMP rax
    movdqa xmm11, xmm0
    movhlps xmm11, xmm2
    DUMPX xmm11
    movlhps xmm11, xmm1
    DUMPX xmm11
    EXIT 0

    .section .note.GNU-stack, "", @progbits
