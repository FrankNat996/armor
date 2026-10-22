/*
 * The VEX.256 forms (AVX, and AVX2's integer ones; run with
 * `+xsave,+avx,+avx2` declared, difftest's CASE_FEATURES;
 * docs/tech/avx.md): each operation on two 128-bit lanes, with the
 * immediates that hold one field per lane (vshufpd, vblendps, vblendpd,
 * vmpsadbw), the conversions between ymm and xmm, the masks and vptest
 * on 256 bits, the shifts by an xmm count, the widening moves, 32-byte
 * aligned and unaligned moves, and destinations equal to a source. Float
 * operands are never denormal: qemu does not raise DE (docs/testing.md).
 */
#include "dump.h"

/* ymm register n from the 32 bytes at `label`. */
.macro SETY n, label
    vmovdqu ymm\n, [rip + \label]
.endm

/* The 32 bytes of ymm register n. */
.macro DUMPY n
    vmovdqu [rip + ymm_out], ymm\n
    DUMP_WRITE ymm_out, 32
.endm

.macro SOURCES
    SETY 1, floats_a
    SETY 2, floats_b
    SETY 3, ints_a
    SETY 4, ints_b
.endm

    .data
    .balign 32
floats_a:
    .float 1.0, 2.5, -3.0, 4.0, 1e10, -0.5, 7.25, 100.0
floats_b:
    .float 3.0, -2.0, 0.25, 4.0, 2.0, 8.0, -1.5, 3.0
doubles:
    .double 1.5, -2.25, 1e100, 9.0
ints_a:
    .quad 0x0123456789abcdef, 0xfedcba9876543210, 0x8000000180000001, 0x7fffffff00000000
ints_b:
    .quad 0x00ff00ff00ff00ff, 0x8080808080808080, 0x0102030405060708, 0xf0e0d0c0b0a09080
ymm_out:
    .zero 32
    .balign 32
aligned:
    .zero 64
    .zero 1
unaligned:
    .zero 64

    .text
    .globl _start
_start:
    /* Float arithmetic and logic. */
    SOURCES
    vaddps ymm0, ymm1, ymm2
    DUMPY 0
    vmulps ymm5, ymm1, [rip + floats_b]
    DUMPY 5
    vdivps ymm6, ymm1, ymm2
    DUMPY 6
    vsqrtps ymm7, ymm2
    DUMPY 7
    vmaxps ymm8, ymm1, ymm2
    DUMPY 8
    vaddsubps ymm9, ymm1, ymm2
    DUMPY 9
    vhaddps ymm10, ymm1, ymm2
    DUMPY 10
    vandps ymm11, ymm1, ymm2
    DUMPY 11
    vxorpd ymm12, ymm3, ymm4
    DUMPY 12
    vandnps ymm13, ymm3, ymm4
    DUMPY 13
    SETY 14, doubles
    vaddpd ymm15, ymm14, ymm14
    DUMPY 15
    vsubpd ymm14, ymm15, ymm14
    DUMPY 14
    vcmpps ymm0, ymm1, ymm2, 1
    DUMPY 0
    vcmppd ymm5, ymm15, [rip + doubles], 30
    DUMPY 5
    /* Destination equal to a source. */
    vmulps ymm2, ymm1, ymm2
    DUMPY 2
    SOURCES
    vmulps ymm1, ymm1, ymm2
    DUMPY 1

    /* Shuffles with an immediate per lane. */
    SOURCES
    vshufps ymm0, ymm1, ymm2, 0x9c
    DUMPY 0
    vshufpd ymm5, ymm3, ymm4, 0x6
    DUMPY 5
    vshufpd ymm6, ymm3, ymm4, 0x9
    DUMPY 6
    vunpcklps ymm7, ymm1, ymm2
    DUMPY 7
    vunpckhpd ymm8, ymm3, ymm4
    DUMPY 8
    vblendps ymm9, ymm1, ymm2, 0xa5
    DUMPY 9
    vblendpd ymm10, ymm3, ymm4, 0x6
    DUMPY 10
    vdpps ymm11, ymm1, ymm2, 0xf3
    DUMPY 11
    vroundps ymm12, ymm1, 1
    DUMPY 12
    vmovddup ymm13, ymm3
    DUMPY 13
    vmovddup ymm13, [rip + ints_b]
    DUMPY 13
    vmovshdup ymm14, ymm1
    DUMPY 14

    /* Conversions between ymm and xmm. */
    SOURCES
    vcvtps2pd ymm0, xmm1
    DUMPY 0
    vcvtps2pd ymm5, [rip + floats_b]
    DUMPY 5
    SETY 6, doubles
    vcvtpd2ps xmm7, ymm6
    DUMPY 7
    vcvtpd2ps xmm8, ymmword ptr [rip + doubles]
    DUMPY 8
    vcvtdq2ps ymm9, ymm3
    DUMPY 9
    vcvttps2dq ymm10, ymm1
    DUMPY 10
    vcvtdq2pd ymm11, xmm3
    DUMPY 11
    vcvtpd2dq xmm12, ymm6
    DUMPY 12

    /* Masks and vptest on 256 bits. */
    vmovmskps eax, ymm1
    DUMP rax
    vmovmskpd eax, ymm6
    DUMP rax
    vpmovmskb eax, ymm4
    DUMP rax
    vptest ymm3, ymm4
    DUMP_FLAGS FLAGS_STATUS
    vptest ymm3, ymm3
    DUMP_FLAGS FLAGS_STATUS
    vpxor ymm0, ymm0, ymm0
    vptest ymm0, [rip + ints_a]
    DUMP_FLAGS FLAGS_STATUS

    /* AVX2: integer lanes. */
    SOURCES
    vpaddd ymm0, ymm3, ymm4
    DUMPY 0
    vpsubb ymm5, ymm3, [rip + ints_b]
    DUMPY 5
    vpcmpeqb ymm6, ymm4, ymm4
    DUMPY 6
    vpcmpgtq ymm7, ymm3, ymm4
    DUMPY 7
    vpminub ymm8, ymm3, ymm4
    DUMPY 8
    vpmulld ymm9, ymm3, ymm4
    DUMPY 9
    vpmuludq ymm10, ymm3, ymm4
    DUMPY 10
    vpmaddwd ymm11, ymm3, ymm4
    DUMPY 11
    vpsadbw ymm12, ymm3, ymm4
    DUMPY 12
    vpand ymm13, ymm3, ymm4
    DUMPY 13
    vpabsb ymm14, ymm4
    DUMPY 14
    vphaddw ymm15, ymm3, ymm4
    DUMPY 15

    /* AVX2: shuffles, packs, blends, shifts. */
    SOURCES
    vpshufb ymm0, ymm3, ymm4
    DUMPY 0
    vpalignr ymm5, ymm3, ymm4, 7
    DUMPY 5
    vpshufd ymm6, ymm3, 0x4e
    DUMPY 6
    vpshufhw ymm7, ymm3, 0x1b
    DUMPY 7
    vpunpcklbw ymm8, ymm3, ymm4
    DUMPY 8
    vpackuswb ymm9, ymm3, ymm4
    DUMPY 9
    vpblendw ymm10, ymm3, ymm4, 0x96
    DUMPY 10
    vmpsadbw ymm11, ymm3, ymm4, 0x2d
    DUMPY 11
    vpsllw ymm12, ymm3, 3
    DUMPY 12
    vpsrldq ymm13, ymm3, 5
    DUMPY 13
    vpsllq ymm14, ymm3, xmm4
    DUMPY 14
    mov eax, 9
    vmovd xmm15, eax
    vpsrad ymm15, ymm3, xmm15
    DUMPY 15
    vpsrld ymm3, ymm3, 1
    DUMPY 3

    /* AVX2: widening moves. */
    SOURCES
    vpmovzxbw ymm0, xmm4
    DUMPY 0
    vpmovsxbd ymm5, xmm4
    DUMPY 5
    vpmovsxwq ymm6, [rip + ints_b]
    DUMPY 6
    vpmovzxdq ymm7, xmm3
    DUMPY 7

    /* Aligned and unaligned moves; vzeroupper. */
    SOURCES
    vmovaps [rip + aligned], ymm1
    vmovaps ymm5, [rip + aligned]
    DUMPY 5
    vmovups [rip + unaligned], ymm3
    vmovdqu ymm6, [rip + unaligned]
    DUMPY 6
    vmovntdq [rip + aligned + 32], ymm4
    mov rax, [rip + aligned + 56]
    DUMP rax
    vmovapd ymm7, ymm6
    DUMPY 7
    vzeroupper
    DUMPY 7
    EXIT 0

    .section .note.GNU-stack, "", @progbits
