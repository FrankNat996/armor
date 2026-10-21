/*
 * The instructions AVX and AVX2 add (run with `+xsave,+avx,+avx2`
 * declared, difftest's CASE_FEATURES; docs/tech/avx.md): broadcasts,
 * vinsertf128/vextractf128 and the i128 forms, vperm2f128/vperm2i128,
 * vpermq, vpermpd, vpermd, vpermps, vpermilps/pd with an immediate and
 * with a vector, vblendvps/pd and vpblendvb, vpblendd, the variable
 * shifts, the masked loads and stores, vtestps/vtestpd, in their 128-
 * and 256-bit forms. That a masked-off lane is not accessed (no fault on
 * an unmapped page) is a unit test: qemu reads the whole vector and
 * faults (docs/tech/avx.md).
 */
#include "dump.h"

.macro SETY n, label
    vmovdqu ymm\n, [rip + \label]
.endm

.macro DUMPY n
    vmovdqu [rip + ymm_out], ymm\n
    DUMP_WRITE ymm_out, 32
.endm

.macro SOURCES
    SETY 1, data_a
    SETY 2, data_b
    SETY 3, control
.endm

    .data
    .balign 32
data_a:
    .quad 0x0123456789abcdef, 0xfedcba9876543210, 0x8000000180000001, 0x7fffffff00000000
data_b:
    .quad 0x00ff00ff00ff00ff, 0x8080808080808080, 0x0102030405060708, 0xf0e0d0c0b0a09080
control:
    .long 7, 0x80000002, 5, 1, 0xfffffff3, 6, 4, 0x80000000
counts:
    .quad 1, 63, 64, 0xffffffff00000003
counts32:
    .long 0, 1, 31, 32, 33, 7, 0x80000000, 16
ymm_out:
    .zero 32
store:
    .zero 64

    .text
    .globl _start
_start:
    SOURCES
    /* Broadcasts. */
    vbroadcastss xmm0, [rip + data_a + 4]
    DUMPY 0
    vbroadcastss ymm0, xmm2
    DUMPY 0
    vbroadcastsd ymm4, [rip + data_b + 8]
    DUMPY 4
    vbroadcastf128 ymm5, [rip + data_a + 16]
    DUMPY 5
    vbroadcasti128 ymm5, [rip + data_b]
    DUMPY 5
    vpbroadcastb ymm6, xmm1
    DUMPY 6
    vpbroadcastw xmm6, [rip + data_b + 2]
    DUMPY 6
    vpbroadcastd ymm7, xmm2
    DUMPY 7
    vpbroadcastq ymm8, [rip + data_a + 24]
    DUMPY 8

    /* 128-bit halves. */
    vinsertf128 ymm9, ymm1, xmm2, 1
    DUMPY 9
    vinserti128 ymm9, ymm1, [rip + data_b + 16], 0
    DUMPY 9
    vextractf128 xmm10, ymm1, 1
    DUMPY 10
    vextracti128 [rip + store], ymm2, 1
    mov rax, [rip + store]
    DUMP rax
    vperm2f128 ymm11, ymm1, ymm2, 0x31
    DUMPY 11
    vperm2i128 ymm11, ymm1, [rip + data_b], 0x82
    DUMPY 11
    vperm2f128 ymm1, ymm1, ymm1, 0x01
    DUMPY 1

    /* Permutations. */
    SOURCES
    vpermq ymm0, ymm1, 0x1b
    DUMPY 0
    vpermpd ymm4, [rip + data_b], 0xd8
    DUMPY 4
    vpermd ymm5, ymm3, ymm1
    DUMPY 5
    vpermps ymm6, ymm3, [rip + data_b]
    DUMPY 6
    vpermilps ymm7, ymm1, 0x4e
    DUMPY 7
    vpermilpd ymm8, ymm1, 0x9
    DUMPY 8
    vpermilps xmm9, xmm2, xmm3
    DUMPY 9
    vpermilps ymm9, ymm2, ymm3
    DUMPY 9
    vpermilpd ymm10, ymm1, [rip + control]
    DUMPY 10

    /* Variable blends. */
    vblendvps ymm11, ymm1, ymm2, ymm3
    DUMPY 11
    vblendvpd xmm12, xmm1, [rip + data_b], xmm1
    DUMPY 12
    vpblendvb ymm13, ymm1, ymm2, ymm2
    DUMPY 13
    vpblendd ymm14, ymm1, ymm2, 0x5a
    DUMPY 14
    vpblendd xmm14, xmm1, xmm2, 0x06
    DUMPY 14

    /* Variable shifts. */
    SETY 4, counts
    SETY 5, counts32
    vpsllvq ymm0, ymm1, ymm4
    DUMPY 0
    vpsrlvq ymm0, ymm1, [rip + counts]
    DUMPY 0
    vpsllvd ymm0, ymm1, ymm5
    DUMPY 0
    vpsrlvd xmm0, xmm1, xmm5
    DUMPY 0
    vpsravd ymm0, ymm1, ymm5
    DUMPY 0

    /* vtestps, vtestpd. */
    vtestps ymm1, ymm2
    DUMP_FLAGS FLAGS_STATUS
    vtestpd xmm3, xmm3
    DUMP_FLAGS FLAGS_STATUS
    vpxor ymm0, ymm0, ymm0
    vtestps ymm0, [rip + data_a]
    DUMP_FLAGS FLAGS_STATUS

    /* Masked loads and stores, the mask in ymm3 (lanes 1 and 7 of
       vmaskmovps, from the sign bits). */
    vmaskmovps ymm0, ymm3, [rip + data_a]
    DUMPY 0
    vmaskmovpd xmm0, xmm2, [rip + data_a]
    DUMPY 0
    vpmaskmovd ymm0, ymm3, [rip + data_b]
    DUMPY 0
    vpmaskmovq ymm0, ymm2, [rip + data_a]
    DUMPY 0
    vmaskmovps [rip + store], ymm3, ymm1
    vpmaskmovq [rip + store + 32], ymm2, ymm2
    DUMP_WRITE store, 64

    EXIT 0

    .section .note.GNU-stack, "", @progbits
