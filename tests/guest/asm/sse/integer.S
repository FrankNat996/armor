/*
 * SSE2 integer operations: add/sub (wrapping), compares, unsigned and
 * signed min/max, logic, pmuludq, pmovmskb, pshufd, shifts by immediate,
 * byte shifts, unpacks, saturating packs, word shuffles, pextrw, pinsrw.
 */
#include "dump.h"

#define A_LO 0x7f80ff0001fe8001
#define A_HI 0x8000000000000001
#define B_LO 0x0180010100ff7fff
#define B_HI 0xffffffff7fffffff

/* xmm0 = A, xmm1 = B; run `op xmm0, xmm1`; dump xmm0. */
.macro BIN op
    SETX xmm0, A_LO, A_HI
    SETX xmm1, B_LO, B_HI
    \op xmm0, xmm1
    DUMPX xmm0
.endm

/* xmm0 = A; run `op xmm0, imm`; dump xmm0. */
.macro IMM op, imm
    SETX xmm0, A_LO, A_HI
    \op xmm0, \imm
    DUMPX xmm0
.endm

    .section .rodata
    .balign 16
operand:
    .quad B_LO, B_HI

    .text
    .globl _start
_start:
    BIN packsswb
    BIN packssdw
    BIN packuswb
    BIN paddb
    BIN paddw
    BIN paddd
    BIN paddq
    BIN psubb
    BIN psubw
    BIN psubd
    BIN psubq
    BIN pcmpeqb
    BIN pcmpeqw
    BIN pcmpeqd
    BIN pcmpgtb
    BIN pcmpgtw
    BIN pcmpgtd
    BIN pminub
    BIN pmaxub
    BIN pminsw
    BIN pmaxsw
    BIN pand
    BIN pandn
    BIN por
    BIN pxor
    BIN andps
    BIN andnps
    BIN orps
    BIN xorpd
    BIN pmuludq
    BIN punpcklbw
    BIN punpcklwd
    BIN punpckldq
    BIN punpcklqdq
    BIN punpckhbw
    BIN punpckhwd
    BIN punpckhdq
    BIN punpckhqdq
    BIN unpcklps
    BIN unpckhps
    BIN unpcklpd
    BIN unpckhpd

    /* Memory source (16-byte aligned). */
    SETX xmm0, A_LO, A_HI
    paddb xmm0, [rip + operand]
    DUMPX xmm0

    SETX xmm1, A_LO, A_HI
    pshufd xmm0, xmm1, 0x1b
    DUMPX xmm0
    pshufd xmm0, xmm1, 0x00
    DUMPX xmm0
    pmovmskb eax, xmm1
    DUMP rax
    mov rax, -1
    pmovmskb rax, xmm1
    DUMP rax

    IMM psllw, 3
    IMM psllw, 16
    IMM psrlw, 9
    IMM psraw, 4
    IMM psraw, 40
    IMM pslld, 31
    IMM psrld, 1
    IMM psrad, 31
    IMM psllq, 63
    IMM psllq, 64
    IMM psrlq, 4
    IMM pslldq, 3
    IMM psrldq, 15
    IMM psrldq, 16

    /* Word shuffles, extract and insert. */
    SETX xmm1, B_LO, B_HI
    pshuflw xmm0, xmm1, 0x1b
    DUMPX xmm0
    pshufhw xmm0, xmm1, 0xe4
    DUMPX xmm0
    pshufhw xmm0, [rip + operand], 0x00
    DUMPX xmm0
    pextrw eax, xmm1, 0
    DUMP rax
    pextrw eax, xmm1, 7
    DUMP rax
    pextrw eax, xmm1, 11                 /* index taken modulo 8 */
    DUMP rax
    SETX xmm0, A_LO, A_HI
    mov eax, 0x12345678
    pinsrw xmm0, eax, 5
    DUMPX xmm0
    pinsrw xmm0, word ptr [rip + operand], 0
    DUMPX xmm0
    EXIT 0

    .section .note.GNU-stack, "", @progbits
