/*
 * All 16 conditions, short (rel8) and near (rel32) forms, after cmp with
 * operand pairs chosen to cover CF, ZF, SF, OF and PF. For each pair, bit n
 * of rbx is set when condition n is NOT taken.
 */
#include "dump.h"

.macro CHECK cc, bit
    cmp rax, rcx
    j\cc 1f
    or rbx, \bit
1:
.endm

.macro CHECK32 cc, bit
    cmp rax, rcx
    {disp32} j\cc 1f
    or rbx, \bit
1:
.endm

.macro ALL a, b
    mov rax, \a
    mov rcx, \b
    xor ebx, ebx
    CHECK o, 0x1
    CHECK no, 0x2
    CHECK b, 0x4
    CHECK ae, 0x8
    CHECK e, 0x10
    CHECK ne, 0x20
    CHECK be, 0x40
    CHECK a, 0x80
    CHECK32 s, 0x100
    CHECK32 ns, 0x200
    CHECK32 p, 0x400
    CHECK32 np, 0x800
    CHECK32 l, 0x1000
    CHECK32 ge, 0x2000
    CHECK32 le, 0x4000
    CHECK32 g, 0x8000
    DUMP rbx
.endm

    .text
    .globl _start
_start:
    ALL 1, 1
    ALL 1, 2
    ALL 2, 1
    ALL 3, 0                            /* result 3: even parity */
    ALL 1, 0                            /* result 1: odd parity */
    ALL -1, 1
    ALL 0x8000000000000000, 1           /* signed overflow */
    ALL 0x7fffffffffffffff, -1          /* signed overflow */
    ALL -2, -1

    /* Unconditional jumps, both sizes, forward and backward. */
    xor ebx, ebx
    jmp 2f
    or rbx, 1
2:
    {disp32} jmp 3f
    or rbx, 2
3:
    mov ecx, 3
4:
    add rbx, 0x10
    sub ecx, 1
    jne 4b
    DUMP rbx                            /* 0x30 */
    EXIT 0

    .section .note.GNU-stack, "", @progbits
