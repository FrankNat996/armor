/*
 * SSE2 integer instructions beyond the first set (sse/integer): saturating
 * add and subtract (signed and unsigned, bytes and words), pavgb/pavgw,
 * pmullw/pmulhw/pmulhuw, pmaddwd (with the 0x8000 * 0x8000 * 2 overflow),
 * psadbw, the shifts by a count in an xmm register or m128 (counts within
 * and beyond the lane width), maskmovdqu, and clflush.
 */
#include "dump.h"

#define A_LO 0x80017fff00ff7f80
#define A_HI 0xfffe0001fedc1234
#define B_LO 0x8000800101017f80
#define B_HI 0x00027fffff0080ff

/* xmm0 = A, xmm1 = (b1:b0); `op xmm0, xmm1`; dump xmm0. */
.macro I op, b0=B_LO, b1=B_HI
    SETX xmm0, A_LO, A_HI
    SETX xmm1, \b0, \b1
    \op xmm0, xmm1
    DUMPX xmm0
.endm

    .data
    .balign 16
count_mem:
    .quad 3, 0xffffffffffffffff
buffer:
    .quad 0x5555555555555555, 0x5555555555555555
line:
    .quad 1

    .text
    .globl _start
_start:
    I paddsb
    I paddsw
    I paddusb
    I paddusw
    I psubsb
    I psubsw
    I psubusb
    I psubusw
    I pavgb
    I pavgw
    I pmullw
    I pmulhw
    I pmulhuw
    I pmaddwd
    I pmaddwd, 0x8000800080008000, 0x8000800080008000
    I psadbw

    /* Shifts by xmm: counts 0, 5, lane width, 64 and huge. */
    .irp op, psllw, pslld, psllq, psrlw, psrld, psrlq, psraw, psrad
    I \op, 0, 0
    I \op, 5, 0xffffffffffffffff
    I \op, 16, 0
    I \op, 32, 0
    I \op, 64, 0
    I \op, 0x100000000, 0
    .endr
    /* Count from memory: only the low quadword counts. */
    SETX xmm0, A_LO, A_HI
    psrlw xmm0, [rip + count_mem]
    DUMPX xmm0

    /* maskmovdqu: bytes 0, 3, 8 and 15 of xmm0 go to buffer. */
    SETX xmm0, 0x0706050403020100, 0x0f0e0d0c0b0a0908
    SETX xmm1, 0x0000000080000080, 0x8000000000000080
    lea rdi, [rip + buffer]
    maskmovdqu xmm0, xmm1
    mov rax, [rip + buffer]
    DUMP rax
    mov rax, [rip + buffer + 8]
    DUMP rax

    /* clflush: nothing to see, but it must run. */
    clflush [rip + line]
    mov rax, [rip + line]
    DUMP rax

    EXIT 0
