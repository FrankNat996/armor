/*
 * and, or, xor, test: CF and OF cleared, SF/ZF/PF from the result. AF is
 * architecturally undefined, so it is masked out.
 */
#include "dump.h"

.macro BIN op, dst, src, a, b
    mov rax, \a
    mov rcx, \b
    SET_FLAGS
    \op \dst, \src
    DUMP rax
    DUMP_FLAGS FLAGS_NO_AF
.endm

/* Start from CF=1 and OF=1 so that clearing them is visible:
   0x7f - 0x80 in 8 bits borrows (unsigned) and overflows (signed). */
.macro SET_FLAGS
    mov rdx, 0x7f
    cmp dl, 0x80
.endm

    .bss
buf:
    .zero 8

    .text
    .globl _start
_start:
    BIN and, rax, rcx, 0xff00ff00ff00ff00, 0x0ff00ff00ff00ff0
    BIN and, eax, ecx, -1, 0x80000000   /* zero-extends */
    BIN and, ax, cx, 0x1234, 0
    BIN and, al, 0x0f, 0xf3, 0          /* 24 ib */
    BIN and, rax, -16, 0x12345678, 0    /* 83 /4, sign-extended */
    BIN and, ecx, 0xff, 0, 0x1234       /* 81 /4 */

    BIN or, rax, rcx, 0x8000000000000000, 1
    BIN or, al, cl, 0, 0
    BIN or, eax, 1, -1, 0               /* zero-extends */
    BIN or, ah, cl, 0x0100, 0x02

    BIN xor, rax, rcx, -1, 0x0f0f0f0f0f0f0f0f
    BIN xor, eax, eax, -1, 0            /* zeroing idiom */
    BIN xor, dil, cl, 0, 0x81           /* REX byte register */
    BIN xor, rax, 0x7f, 0x80, 0

    /* test: flags only. */
    BIN test, rax, rcx, 0x10, 0x01
    BIN test, rax, rcx, 0x8000000000000000, -1
    BIN test, eax, eax, 0x80000000, 0
    BIN test, al, 0x80, 0x80, 0         /* A8 ib */
    BIN test, eax, 0x100, 0x100, 0      /* A9 id */
    BIN test, cl, 3, 0, 0x02            /* F6 /0 ib */
    BIN test, ecx, 0x10000, 0, 0x10000  /* F7 /0 id */

    lea rbx, [rip + buf]
    mov qword ptr [rbx], 0xf0
    SET_FLAGS
    or qword ptr [rbx], 0x0f
    DUMP_FLAGS FLAGS_NO_AF
    mov rax, [rbx]
    DUMP rax
    SET_FLAGS
    test dword ptr [rbx], 0x100
    DUMP_FLAGS FLAGS_NO_AF
    EXIT 0

    .section .note.GNU-stack, "", @progbits
