/*
 * The VEX.128 forms of SSE instructions (AVX; run with `+xsave,+avx`
 * declared, difftest's CASE_FEATURES; docs/tech/avx.md): three operands
 * with the destination distinct from both sources, equal to the first,
 * equal to the second; memory operands without alignment; the forms
 * without VEX.vvvv and the shifts whose VEX.vvvv is the destination; and
 * the upper halves of the ymm registers, set through xrstor and read
 * through xsave (AVX component): a VEX.128 instruction writing an xmm
 * register zeroes them, a legacy SSE one keeps them, vzeroupper and
 * vzeroall clear them. Float operands are never denormal: qemu does not
 * raise DE (docs/testing.md).
 */
#include "dump.h"

/*
 * Every upper ymm half set to its pattern (the AVX component only).
 * MXCSR is kept around it: the SDM loads MXCSR when the AVX component
 * is requested, qemu only with SSE (docs/tech/avx.md).
 */
.macro RESET_HI
    stmxcsr [rip + saved_mxcsr]
    mov eax, 4
    xor edx, edx
    xrstor [rip + hi_area]
    ldmxcsr [rip + saved_mxcsr]
.endm

/* The 16 upper halves (256 bytes). */
.macro DUMP_HI
    mov eax, 4
    xor edx, edx
    xsave [rip + out_area]
    DUMP_WRITE out_area + 576, 256
.endm

.macro SOURCES
    SETX xmm1, 0x3ff0000000000000, 0x4000000000000000
    SETX xmm2, 0x40490fdb3f800000, 0xc0000000bf800000
    SETX xmm3, 0x8000000180000001, 0x7fffffff00000000
    SETX xmm4, 0x0123456789abcdef, 0xfedcba9876543210
.endm

    .data
    .balign 64
hi_area:
    .zero 832
    .balign 64
out_area:
    .zero 832
    .balign 16
    .zero 1
unaligned:
    .quad 0x3f8000003f800000, 0x4000000040000000
    .quad 0x00ff00ff00ff00ff, 0x8080808080808080
    .balign 16
aligned:
    .quad 0x1111111111111111, 0x2222222222222222
mxcsr:
    .long 0
saved_mxcsr:
    .long 0

    .text
    .globl _start
_start:
    /* The pattern of the upper halves: byte i of half n is n * 16 + i. */
    mov eax, 7
    xor edx, edx
    xsave [rip + hi_area]
    lea rdi, [rip + hi_area + 576]
    xor ecx, ecx
1:
    mov [rdi + rcx], cl
    inc ecx
    cmp ecx, 256
    jne 1b
    RESET_HI
    DUMP_HI

    /* Legacy SSE keeps the upper halves; VEX.128 zeroes the destination's. */
    SOURCES
    addps xmm5, xmm1
    pxor xmm6, xmm6
    vaddps xmm7, xmm1, xmm2
    vpxor xmm8, xmm8, xmm8
    DUMP_HI
    DUMPX xmm7
    DUMPX xmm8

    /* Arithmetic: distinct, dst = src1, dst = src2. */
    RESET_HI
    SOURCES
    vaddps xmm0, xmm1, xmm2
    DUMPX xmm0
    vmulpd xmm1, xmm1, xmm2
    DUMPX xmm1
    SOURCES
    vsubps xmm2, xmm1, xmm2
    DUMPX xmm2
    SOURCES
    vdivss xmm5, xmm2, xmm1
    DUMPX xmm5
    vsqrtsd xmm6, xmm4, xmm1
    DUMPX xmm6
    vsqrtps xmm9, xmm2
    DUMPX xmm9
    vmaxps xmm10, xmm4, xmm2
    DUMPX xmm10
    vminsd xmm11, xmm2, xmm4
    DUMPX xmm11
    vaddsubps xmm12, xmm1, xmm2
    DUMPX xmm12
    vhaddpd xmm13, xmm1, xmm2
    DUMPX xmm13
    vaddps xmm14, xmm1, [rip + unaligned]
    DUMPX xmm14
    vmulss xmm15, xmm1, [rip + unaligned + 4]
    DUMPX xmm15
    DUMP_HI

    /* Logic and integer lanes. */
    RESET_HI
    SOURCES
    vandps xmm0, xmm3, xmm4
    vandnpd xmm5, xmm3, xmm4
    vorps xmm6, xmm3, xmm4
    vxorpd xmm7, xmm3, xmm4
    vpaddd xmm8, xmm3, xmm4
    vpsubq xmm9, xmm4, xmm3
    vpcmpeqb xmm10, xmm4, [rip + unaligned + 16]
    vpcmpgtw xmm11, xmm3, xmm4
    vpminub xmm12, xmm3, xmm4
    vpmaxsd xmm13, xmm3, xmm4
    vpmulld xmm14, xmm3, xmm4
    vpcmpgtq xmm15, xmm3, xmm4
    .irp r, 0, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
    DUMPX xmm\r
    .endr
    DUMP_HI
    SOURCES
    vpshufb xmm0, xmm4, xmm3
    vpalignr xmm5, xmm3, xmm4, 5
    vpunpcklbw xmm6, xmm3, xmm4
    vpunpckhqdq xmm7, xmm3, xmm4
    vpackuswb xmm8, xmm3, xmm4
    vpackssdw xmm9, xmm3, xmm4
    vpmaddwd xmm10, xmm3, xmm4
    vpsadbw xmm11, xmm3, xmm4
    vpmuludq xmm12, xmm3, xmm4
    vphaddd xmm13, xmm3, xmm4
    vpsignb xmm14, xmm4, xmm3
    vpavgb xmm15, xmm3, xmm4
    .irp r, 0, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
    DUMPX xmm\r
    .endr

    /* Shifts: by an immediate (VEX.vvvv the destination), by xmm. */
    RESET_HI
    SOURCES
    vpsllw xmm0, xmm4, 3
    vpsrld xmm5, xmm4, 31
    vpsraw xmm6, xmm3, 9
    vpsrlq xmm7, xmm4, 64
    vpslldq xmm8, xmm4, 3
    vpsrldq xmm9, xmm4, 15
    vpsllq xmm10, xmm4, xmm1
    vpsrad xmm11, xmm3, [rip + aligned]
    vpsrld xmm4, xmm4, 1
    .irp r, 0, 4, 5, 6, 7, 8, 9, 10, 11
    DUMPX xmm\r
    .endr
    DUMP_HI

    /* Shuffles, blends, inserts and extracts. */
    RESET_HI
    SOURCES
    vpshufd xmm0, xmm4, 0x1b
    vpshufhw xmm5, xmm4, 0x4e
    vshufps xmm6, xmm3, xmm4, 0x9c
    vshufpd xmm7, xmm3, xmm4, 1
    vunpcklps xmm8, xmm3, xmm4
    vblendps xmm9, xmm3, xmm4, 5
    vpblendw xmm10, xmm3, xmm4, 0x96
    vinsertps xmm11, xmm3, xmm4, 0x9a
    vpinsrw xmm12, xmm3, eax, 5
    mov rax, 0x1122334455667788
    vpinsrq xmm13, xmm3, rax, 1
    vpinsrb xmm14, xmm4, [rip + aligned], 9
    vmovhlps xmm15, xmm3, xmm4
    .irp r, 0, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
    DUMPX xmm\r
    .endr
    vpextrw eax, xmm4, 6
    DUMP rax
    vpextrq rax, xmm4, 1
    DUMP rax
    vextractps eax, xmm2, 3
    DUMP rax
    vpmovmskb eax, xmm3
    DUMP rax
    vmovmskpd eax, xmm2
    DUMP rax
    DUMP_HI

    /* Moves. */
    RESET_HI
    SOURCES
    vmovdqu xmm0, [rip + unaligned]
    vmovups xmm5, [rip + unaligned + 16]
    vmovdqa xmm6, [rip + aligned]
    vmovss xmm7, [rip + unaligned]
    vmovsd xmm8, xmm3, xmm4
    vmovss xmm9, xmm4, xmm2
    vmovlps xmm10, xmm4, [rip + aligned]
    vmovhpd xmm11, xmm3, [rip + aligned + 8]
    vmovd xmm12, eax
    vmovq xmm13, xmm4
    vmovddup xmm14, xmm4
    vmovshdup xmm15, xmm4
    .irp r, 0, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
    DUMPX xmm\r
    .endr
    DUMP_HI
    vmovups [rip + unaligned + 16], xmm4
    vmovss [rip + unaligned], xmm2
    vmovq rax, xmm3
    DUMP rax
    mov rax, [rip + unaligned]
    DUMP rax
    mov rax, [rip + unaligned + 16]
    DUMP rax
    /* A store keeps the upper halves: RESET_HI above, still set. */
    RESET_HI
    vmovdqu [rip + unaligned + 16], xmm4
    DUMP_HI

    /* Conversions and rounding. */
    RESET_HI
    SOURCES
    mov rax, -7
    vcvtsi2sd xmm0, xmm4, rax
    vcvtsi2ss xmm5, xmm4, eax
    vcvtss2sd xmm6, xmm4, xmm2
    vcvtsd2ss xmm7, xmm2, xmm1
    vcvtdq2ps xmm8, xmm4
    vcvtps2pd xmm9, xmm2
    vcvttps2dq xmm10, xmm2
    vcvtpd2ps xmm11, xmm1
    vroundsd xmm12, xmm4, xmm2, 1
    vroundps xmm13, xmm2, 2
    vdpps xmm14, xmm2, xmm2, 0xf1
    vpmovzxbw xmm15, xmm4
    .irp r, 0, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
    DUMPX xmm\r
    .endr
    vcvttsd2si rax, xmm1
    DUMP rax
    vcvtss2si eax, xmm2
    DUMP rax
    vpmovsxwd xmm0, [rip + aligned]
    vpabsd xmm5, xmm3
    vphminposuw xmm6, xmm4
    vmpsadbw xmm7, xmm3, xmm4, 5
    vpmuldq xmm8, xmm3, xmm4
    vpackusdw xmm9, xmm3, xmm4
    .irp r, 0, 5, 6, 7, 8, 9
    DUMPX xmm\r
    .endr
    DUMP_HI

    /* Compares: every VEX predicate, packed and scalar, with a NaN. */
    SETX xmm1, 0x7fc000003f800000, 0x40000000c0000000
    SETX xmm2, 0x3f8000003f800000, 0x3f8000004f800000
    .irp p, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31
    vcmpps xmm0, xmm1, xmm2, \p
    DUMPX xmm0
    vcmpsd xmm5, xmm2, xmm1, \p
    DUMPX xmm5
    .endr
    vstmxcsr [rip + mxcsr]
    DUMP_WRITE mxcsr, 4
    mov dword ptr [rip + mxcsr], 0x1f80
    vldmxcsr [rip + mxcsr]
    stmxcsr [rip + mxcsr]
    DUMP_WRITE mxcsr, 4
    vcomiss xmm1, xmm2
    DUMP_FLAGS FLAGS_STATUS
    vucomisd xmm2, xmm1
    DUMP_FLAGS FLAGS_STATUS
    vptest xmm3, xmm4
    DUMP_FLAGS FLAGS_STATUS

    /* String compares: the index into ecx, the mask into xmm0. */
    RESET_HI
    SOURCES
    vpcmpistri xmm4, xmm3, 0x08
    DUMP rcx
    vpcmpistrm xmm4, xmm3, 0x48
    DUMPX xmm0
    DUMP_HI

    /* vzeroupper, then vzeroall. */
    RESET_HI
    vzeroupper
    DUMP_HI
    DUMPX xmm4
    RESET_HI
    vzeroall
    DUMP_HI
    DUMPX xmm4
    EXIT 0

    .section .note.GNU-stack, "", @progbits
