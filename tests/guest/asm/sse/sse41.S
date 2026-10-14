/*
 * SSE4.1, xmm forms: blends (immediate and variable), pmuldq, pmulld,
 * pcmpeqq, packusdw, pmin/pmax (sb, sd, uw, ud), phminposuw, mpsadbw,
 * ptest, pmovsx/pmovzx (register and unaligned memory), pextrb/w/d/q and
 * extractps (to registers and memory), pinsrb/d/q, insertps, movntdqa,
 * roundps/pd/ss/sd (every rounding immediate, MXCSR's RC, precision
 * suppressed; MXCSR dumped), dpps/dppd (MXCSR dumped).
 */
#include "dump.h"
#include "mxcsr.h"

#define A0 0x80ff7f0100fe0281
#define A1 0x7fff800000017ffe
#define B0 0x0102030485868788
#define B1 0xff00807f0f8e1d2c
#define C0 0x8000000080007fff
#define C1 0x7fffffff00000001

.macro P op, a0, a1, b0, b1
    SETX xmm0, \a0, \a1
    SETX xmm1, \b0, \b1
    \op xmm0, xmm1
    DUMPX xmm0
.endm

.macro ALL op
    P \op, A0, A1, B0, B1
    P \op, B0, B1, A0, A1
    P \op, C0, C1, C0, C1
    P \op, A0, A1, C0, C1
    P \op, C0, C1, B0, B1
.endm

/* `op xmm0, xmm1, imm`. */
.macro PI op, imm, a0, a1, b0, b1
    SETX xmm0, \a0, \a1
    SETX xmm1, \b0, \b1
    \op xmm0, xmm1, \imm
    DUMPX xmm0
.endm

/* Variable blend: xmm2 = a, xmm1 = b, xmm0 = mask. */
.macro BV op, a0, a1, b0, b1, m0, m1
    SETX xmm2, \a0, \a1
    SETX xmm1, \b0, \b1
    SETX xmm0, \m0, \m1
    \op xmm2, xmm1
    DUMPX xmm2
.endm

/* Round: control, op, imm, (a1:a0) as the source. */
.macro RND control, op, imm, a0, a1
    LOAD_CONTROL \control
    SETX xmm0, 0x5555555555555555, 0x6666666666666666
    SETX xmm1, \a0, \a1
    \op xmm0, xmm1, \imm
    DUMP_MXCSR
    DUMPX xmm0
.endm

#define D_0_5   0x3fe0000000000000
#define D_M0_5  0xbfe0000000000000
#define D_1_5   0x3ff8000000000000
#define D_M2_5  0xc004000000000000
#define D_M0_4  0xbfd999999999999a
#define D_2P52  0x4330000000000001  /* 2^52 + 1: integral */
#define S_1_5   0x3fc00000
#define S_M2_5  0xc0200000
#define S_M0_4  0xbecccccd
#define S_0_5   0x3f000000
#define S_INF   0x7f800000

    .data
    .balign 16
aligned:
    .quad A0, A1
    .byte 0
unaligned:
    .quad B0, B1
    .balign 8
slot:
    .quad 0x1111111111111111

    .text
    .globl _start
_start:
    ALL pmuldq
    ALL pmulld
    ALL pcmpeqq
    P pcmpeqq, A0, A1, A0, B1
    ALL packusdw
    ALL pminsb
    ALL pminsd
    ALL pminuw
    ALL pminud
    ALL pmaxsb
    ALL pmaxsd
    ALL pmaxuw
    ALL pmaxud
    ALL phminposuw
    P phminposuw, 0, 0, 0x0005000300030009, 0x0003000400050006
    .irp imm, 0, 1, 2, 3, 4, 5, 6, 7
    PI mpsadbw, \imm, A0, A1, B0, B1
    .endr

    .irp imm, 0x00, 0xa5, 0xff
    PI pblendw, \imm, A0, A1, B0, B1
    .endr
    PI blendps, 5, A0, A1, B0, B1
    PI blendps, 0xa, A0, A1, B0, B1
    PI blendpd, 2, A0, A1, B0, B1
    BV pblendvb, A0, A1, B0, B1, 0x8000800080008000, 0xff00ff0000ff00ff
    BV blendvps, A0, A1, B0, B1, 0x8000000000000000, 0x0000000080000000
    BV blendvpd, A0, A1, B0, B1, 0x8000000000000000, 0x7fffffffffffffff

    /* ptest: ZF and CF. */
    SETX xmm0, A0, A1
    SETX xmm1, B0, B1
    ptest xmm0, xmm1
    DUMP_FLAGS FLAGS_STATUS
    SETX xmm1, 0, 0
    ptest xmm0, xmm1
    DUMP_FLAGS FLAGS_STATUS
    SETX xmm1, A0, A1
    ptest xmm0, xmm1
    DUMP_FLAGS FLAGS_STATUS
    SETX xmm0, -1, -1
    ptest xmm0, xmm1
    DUMP_FLAGS FLAGS_STATUS
    SETX xmm0, 0, 0
    ptest xmm0, xmm1
    DUMP_FLAGS FLAGS_STATUS

    /* pmovsx / pmovzx from a register and from unaligned memory. */
    SETX xmm1, A0, A1
    .irp op, pmovsxbw, pmovsxbd, pmovsxbq, pmovsxwd, pmovsxwq, pmovsxdq, pmovzxbw, pmovzxbd, pmovzxbq, pmovzxwd, pmovzxwq, pmovzxdq
    \op xmm0, xmm1
    DUMPX xmm0
    .endr
    pmovsxbw xmm0, qword ptr [rip + unaligned]
    DUMPX xmm0
    pmovzxbd xmm0, dword ptr [rip + unaligned]
    DUMPX xmm0
    pmovsxbq xmm0, word ptr [rip + unaligned]
    DUMPX xmm0
    pmovzxdq xmm0, qword ptr [rip + unaligned + 8]
    DUMPX xmm0

    /* Extraction to registers (zero-extended) and memory. */
    SETX xmm1, A0, A1
    mov rax, -1
    pextrb eax, xmm1, 15
    DUMP rax
    mov rax, -1
    pextrb eax, xmm1, 17
    DUMP rax
    mov rax, -1
    pextrw eax, xmm1, 3
    DUMP rax
    mov rax, -1
    pextrd eax, xmm1, 2
    DUMP rax
    pextrq rax, xmm1, 1
    DUMP rax
    mov rax, -1
    extractps eax, xmm1, 3
    DUMP rax
    pextrb byte ptr [rip + slot], xmm1, 1
    pextrw word ptr [rip + slot + 2], xmm1, 7
    DUMP_WRITE slot, 8
    pextrd dword ptr [rip + slot], xmm1, 3
    extractps dword ptr [rip + slot + 4], xmm1, 0
    DUMP_WRITE slot, 8
    pextrq qword ptr [rip + slot], xmm1, 0
    DUMP_WRITE slot, 8

    /* Insertion. */
    mov rax, 0x123456789abcdef0
    SETX xmm0, A0, A1
    pinsrb xmm0, eax, 9
    DUMPX xmm0
    pinsrd xmm0, eax, 3
    DUMPX xmm0
    pinsrq xmm0, rax, 0
    DUMPX xmm0
    pinsrb xmm0, byte ptr [rip + unaligned + 3], 0
    DUMPX xmm0
    pinsrd xmm0, dword ptr [rip + unaligned + 1], 1
    DUMPX xmm0
    pinsrq xmm0, qword ptr [rip + unaligned + 5], 1
    DUMPX xmm0
    .irp imm, 0x00, 0x1f, 0xe4, 0x72, 0x30, 0x9a
    PI insertps, \imm, A0, A1, B0, B1
    .endr
    SETX xmm0, A0, A1
    insertps xmm0, dword ptr [rip + unaligned + 2], 0xd1
    DUMPX xmm0

    /* movntdqa: an aligned load. */
    movntdqa xmm0, [rip + aligned]
    DUMPX xmm0

    /* round*: imm 0–3 (nearest, down, up, zero), 4 (MXCSR), +8 (no PE). */
    .irp imm, 0, 1, 2, 3, 4, 8, 9, 0xb
    RND 0x1f80, roundsd, \imm, D_1_5, 0
    RND 0x1f80, roundsd, \imm, D_M2_5, 0
    RND 0x1f80, roundsd, \imm, D_M0_4, 0
    RND 0x1f80, roundpd, \imm, D_0_5, D_M0_5
    RND 0x1f80, roundss, \imm, S_M2_5, 0
    RND 0x1f80, roundps, \imm, PAIR(S_1_5, S_M0_4), PAIR(S_0_5, S_M2_5)
    .endr
    RND 0x3f80, roundsd, 4, D_1_5, 0
    RND 0x5f80, roundsd, 4, D_1_5, 0
    RND 0x7f80, roundpd, 4, D_M2_5, D_1_5
    RND 0x1f80, roundpd, 0, D_SNAN, D_QNAN
    RND 0x1f80, roundpd, 0, D_INF, D_MZERO
    RND 0x1f80, roundpd, 1, D_2P52, D_MAX
    RND 0x1f80, roundsd, 1, D_DEN, 0
    RND 0x1fc0, roundsd, 2, D_DEN, 0
    RND 0x1f80, roundss, 2, S_DEN, 0
    RND 0x1f80, roundps, 0, PAIR(S_QNAN, S_SNAN), PAIR(S_INF, S_MAX)

    /* dpps / dppd. */
    .irp imm, 0xff, 0xf1, 0x31, 0x5a, 0x0f, 0xf0
    LOAD_CONTROL 0x1f80
    SETX xmm0, PAIR(S_TWO, S_ONE), PAIR(S_0_5, S_1_5)
    SETX xmm1, PAIR(S_1_5, S_M2_5), PAIR(S_MAX, S_0_5)
    dpps xmm0, xmm1, \imm
    DUMP_MXCSR
    DUMPX xmm0
    .endr
    .irp imm, 0x33, 0x31, 0x12, 0x23, 0x03
    LOAD_CONTROL 0x1f80
    SETX xmm0, D_ONE, D_MAX
    SETX xmm1, D_THIRD, D_TWO
    dppd xmm0, xmm1, \imm
    DUMP_MXCSR
    DUMPX xmm0
    .endr
    /* A NaN in a lane left out of the products. */
    LOAD_CONTROL 0x1f80
    SETX xmm0, PAIR(S_SNAN, S_ONE), PAIR(S_ONE, S_ONE)
    SETX xmm1, PAIR(S_ONE, S_ONE), PAIR(S_ONE, S_ONE)
    dpps xmm0, xmm1, 0xdf
    DUMP_MXCSR
    DUMPX xmm0
    LOAD_CONTROL 0x1f80
    SETX xmm0, D_ONE, D_SNAN
    SETX xmm1, D_ONE, D_ONE
    dppd xmm0, xmm1, 0x13
    DUMP_MXCSR
    DUMPX xmm0

    EXIT 0
