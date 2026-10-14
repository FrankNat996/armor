/*
 * x87 instructions that work on the register stack or on integers in
 * memory, complementing the c/x87 differential test of the arithmetic:
 * fld/fst/fstp st(i), fxch, ffree, fchs, fabs, fld1, fldz, fcmovcc, ftst,
 * fcom/fcomp/fcompp, fucompp, ficom, fiadd..fidivr with m16 and m32,
 * fnstsw ax and m16, fnstcw/fldcw, fnclex, fninit, fwait, and fxsave of a
 * live x87 state.
 *
 * Status words are dumped without C1: armor sets it as the round-up
 * indicator of the SDM, qemu does not (docs/testing.md). Operands are
 * never denormal (DE is the other known difference).
 */
#include "dump.h"

#define SW_MASK 0xfdff

    .data
    .balign 16
area:
    .zero 512
value80:
    .zero 16
word:
    .short 0, 0
int16:
    .short -7
int32:
    .long 100000
three:
    .double 3.0
half:
    .float 0.5

    .text
/* Pops st(0) into value80 and dumps its 10 bytes. */
.macro DUMP_ST0
    fstpt [rip + value80]
    DUMP_WRITE value80, 10
.endm

/* fnstsw to memory, without C1. */
.macro DUMP_SW
    fnstsw [rip + word]
    and word ptr [rip + word], SW_MASK
    DUMP_WRITE word, 2
.endm

    .globl _start
_start:
    fninit
    /* Push 1, 0, 3.0, 0.5: st0 = 0.5, st1 = 3, st2 = 0, st3 = 1. */
    fld1
    fldz
    fld qword ptr [rip + three]
    fld dword ptr [rip + half]
    DUMP_SW                              /* TOP = 4 */
    fld st(2)                            /* push a copy of 3.0 */
    fxch st(4)                           /* swap with the 1 at the bottom */
    fchs                                 /* -1 */
    fst st(2)                            /* st2 = -1 */
    fabs
    DUMP_ST0                             /* 1 */
    DUMP_ST0                             /* 0.5 */
    DUMP_ST0                             /* -1 */
    DUMP_ST0                             /* 0 */
    DUMP_ST0                             /* 3 */
    DUMP_SW                              /* TOP back to 0 */

    /* Integer memory operands. */
    fninit
    fild word ptr [rip + int16]          /* -7 */
    fiadd dword ptr [rip + int32]        /* 99993 */
    fimul word ptr [rip + int16]         /* -699951 */
    fisub dword ptr [rip + int32]
    fisubr word ptr [rip + int16]
    fidiv dword ptr [rip + int32]
    fidivr word ptr [rip + int16]
    fst st(1)
    DUMP_ST0
    fist dword ptr [rip + word]
    DUMP_WRITE word, 4
    fistp word ptr [rip + word]
    DUMP_WRITE word, 2
    DUMP_SW

    /* Compares: C3 C2 C0 through fnstsw ax and sahf-free bit tests. */
    fninit
    fld qword ptr [rip + three]
    fld1
    fcom st(1)                           /* 1 < 3: C0 */
    fnstsw ax
    and ax, SW_MASK
    DUMP rax
    fcomp qword ptr [rip + three]        /* pops */
    DUMP_SW
    ftst                                 /* 3 > 0 */
    DUMP_SW
    fldz
    ftst                                 /* 0 = 0: C3 */
    DUMP_SW
    fchs
    ftst                                 /* -0 = 0: C3 */
    DUMP_SW
    ficom word ptr [rip + int16]         /* -0 > -7 */
    DUMP_SW
    fcompp                               /* -0 < 3, pops both */
    DUMP_SW
    fld1
    fld1
    fucompp                              /* equal */
    DUMP_SW

    /* fcmovcc after integer compares. */
    fninit
    fld qword ptr [rip + three]
    fld1
    mov eax, 1
    cmp eax, 2                           /* CF = 1 */
    fcmovb st, st(1)                     /* taken: st0 = 3 */
    fcmove st, st(1)                     /* not taken */
    fcmovnb st, st(1)
    DUMP_ST0
    fld1
    cmp eax, 1                           /* ZF = 1 */
    fcmovbe st, st(1)                    /* taken */
    fcmovu st, st(1)                     /* not taken */
    fcmovne st, st(1)
    fcmovnbe st, st(1)
    fcmovnu st, st(1)                    /* taken */
    DUMP_ST0
    DUMP_ST0

    /* Control word, fnclex, ffree, fwait. */
    fninit
    fnstcw [rip + word]
    DUMP_WRITE word, 2
    mov word ptr [rip + word], 0x0f7f    /* truncate */
    fldcw [rip + word]
    fld qword ptr [rip + three]
    fdiv dword ptr [rip + half]          /* 6, exact */
    fld1
    fdivrp st(1), st                     /* 1/6 truncated: PE */
    fwait
    DUMP_SW
    fnclex
    DUMP_SW
    fnstcw [rip + word]
    DUMP_WRITE word, 2
    fld1
    ffree st(0)
    DUMP_SW

    /* fxsave with a live stack: FCW, FSW (without C1), abridged FTW, ST. */
    fninit
    fld1
    fld qword ptr [rip + three]
    fxsave [rip + area]
    and word ptr [rip + area + 2], SW_MASK
    DUMP_WRITE area, 64
    fninit
    fxrstor [rip + area]
    DUMP_ST0                             /* 3 */
    DUMP_ST0                             /* 1 */
    DUMP_SW
    EXIT 0

    .section .note.GNU-stack, "", @progbits
