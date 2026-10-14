/*
 * MMX: the integer operations on mm0-mm7 (the SSE2 operations without the
 * 66 prefix, on 64 bits), moves, the SSE and SSSE3 additions on mm, the
 * conversions between mm and xmm. How MMX shares the x87 registers is in
 * x87_state.S.
 */
#include "dump.h"

    .section .rodata
    .balign 16
a:  .quad 0x8001_7fff_00ff_ff80
b:  .quad 0x7fff_8000_ff01_0102
c:  .quad 0x0123_4567_89ab_cdef
d:  .quad 0xfedc_ba98_7654_3210
mask: .quad 0x8000_8080_0080_0000
idx: .quad 0x0706_8001_0302_0504
floats: .float 2.5, -7.75, 1e10, 0.5
ints: .long 100, -3
    /* cvtpd2pi m128: 16-byte aligned, or #GP (qemu does not check). */
    .balign 16
dbl: .double 1.5e9, -2.25

    .bss
    .balign 16
out: .zero 64
env: .zero 28

    .text
.macro DUMPM r
    movq [rip + out], \r
    DUMP_WRITE out, 8
.endm

/* op: mm0 = a; mm1 = b; mm0 op= mm1; dump mm0. */
.macro BIN op
    movq mm0, [rip + a]
    movq mm1, [rip + b]
    \op mm0, mm1
    DUMPM mm0
.endm

/* The same with a memory source. */
.macro BINM op
    movq mm2, [rip + c]
    \op mm2, [rip + d]
    DUMPM mm2
.endm

    .globl _start
_start:
    BIN paddb
    BIN paddw
    BIN paddd
    BIN paddq
    BIN psubb
    BIN psubw
    BIN psubd
    BIN psubq
    BIN paddsb
    BIN paddsw
    BIN paddusb
    BIN paddusw
    BIN psubsb
    BIN psubsw
    BIN psubusb
    BIN psubusw
    BIN pmullw
    BIN pmulhw
    BIN pmulhuw
    BIN pmaddwd
    BIN pmuludq
    BIN pand
    BIN pandn
    BIN por
    BIN pxor
    BIN pcmpeqb
    BIN pcmpeqw
    BIN pcmpeqd
    BIN pcmpgtb
    BIN pcmpgtw
    BIN pcmpgtd
    BIN packsswb
    BIN packssdw
    BIN packuswb
    BIN punpcklbw
    BIN punpcklwd
    BIN punpckldq
    BIN punpckhbw
    BIN punpckhwd
    BIN punpckhdq
    BIN pavgb
    BIN pavgw
    BIN pminub
    BIN pmaxub
    BIN pminsw
    BIN pmaxsw
    BIN psadbw
    BIN psrlw
    BIN psrld
    BIN psrlq
    BIN psraw
    BIN psrad
    BIN psllw
    BIN pslld
    BIN psllq
    BINM paddq
    BINM pmaddwd
    BINM packsswb
    BINM punpckhbw
    BINM psubusb
    BINM pxor

    /* Shifts by an immediate, and by a count in memory. */
    movq mm3, [rip + c]
    psrlw mm3, 3
    DUMPM mm3
    psllq mm3, 13
    DUMPM mm3
    psrad mm3, 31
    DUMPM mm3
    movq mm3, [rip + d]
    psrlq mm3, 64
    DUMPM mm3
    movq mm3, [rip + d]
    pslld mm3, [rip + ints + 4]
    DUMPM mm3

    /* SSSE3 on mm. */
    BIN pshufb
    BIN phaddw
    BIN phaddd
    BIN phaddsw
    BIN phsubw
    BIN phsubd
    BIN pmaddubsw
    BIN pmulhrsw
    BIN psignb
    BIN psignw
    BIN psignd
    movq mm0, [rip + a]
    pabsb mm1, mm0
    DUMPM mm1
    pabsw mm1, mm0
    DUMPM mm1
    pabsd mm1, mm0
    DUMPM mm1
    movq mm0, [rip + c]
    movq mm1, [rip + d]
    palignr mm0, mm1, 3
    DUMPM mm0
    movq mm0, [rip + c]
    palignr mm0, [rip + d], 11
    DUMPM mm0
    movq mm0, [rip + c]
    pshufb mm0, [rip + idx]
    DUMPM mm0

    /* pshufw, pinsrw, pextrw, pmovmskb. */
    movq mm0, [rip + c]
    pshufw mm1, mm0, 0x1b
    DUMPM mm1
    pshufw mm1, [rip + d], 0xe4
    DUMPM mm1
    mov eax, 0xbeef
    pinsrw mm1, eax, 6
    DUMPM mm1
    pinsrw mm1, word ptr [rip + a], 1
    DUMPM mm1
    pextrw ecx, mm1, 3
    DUMP rcx
    movq mm2, [rip + a]
    pmovmskb edx, mm2
    DUMP rdx

    /* movd and movq with general registers, both ways. */
    mov rax, -1
    movd mm4, eax
    DUMPM mm4
    mov rax, 0x1122334455667788
    movq mm4, rax
    movd ebx, mm4
    DUMP rbx
    movq rbx, mm4
    DUMP rbx
    movq mm5, mm4
    movq [rip + out + 8], mm5
    DUMP_WRITE out + 8, 8

    /* movntq, maskmovq (to [rdi]). */
    movq mm6, [rip + c]
    movntq [rip + out + 16], mm6
    DUMP_WRITE out + 16, 8
    lea rdi, [rip + out + 24]
    movq mm6, [rip + d]
    movq mm7, [rip + mask]
    maskmovq mm6, mm7
    DUMP_WRITE out + 24, 8

    /* Conversions with xmm. */
    movq mm0, [rip + ints]
    SETX xmm1, 0x1111111111111111, 0x2222222222222222
    cvtpi2ps xmm1, mm0
    DUMPX xmm1
    /* From memory too (it once crashed armor: no mm operand at all). */
    cvtpi2ps xmm1, [rip + ints]
    DUMPX xmm1
    cvtpi2pd xmm1, [rip + ints]
    DUMPX xmm1
    movaps xmm2, [rip + floats]
    cvtps2pi mm1, xmm2
    DUMPM mm1
    cvttps2pi mm1, [rip + floats + 8]
    DUMPM mm1
    cvtpi2pd xmm3, mm0
    DUMPX xmm3
    cvtpd2pi mm2, [rip + dbl]
    DUMPM mm2
    movapd xmm4, [rip + dbl]
    cvttpd2pi mm2, xmm4
    DUMPM mm2
    movq2dq xmm5, mm0
    DUMPX xmm5
    movdq2q mm3, xmm2
    DUMPM mm3

    EXIT 0

    .section .note.GNU-stack, "", @progbits
