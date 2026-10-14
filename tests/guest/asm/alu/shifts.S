/*
 * shl, shr, sar, rol, ror, rcl, rcr with counts 1, immediate and cl.
 * OF is only defined for count 1, AF never; rotates leave SF, ZF, PF and AF
 * unchanged, which the full status mask checks. A zero count changes
 * nothing.
 */
#include "dump.h"

#define FLAGS_SHIFT_1 0x8c5    /* CF PF ZF SF OF */
#define FLAGS_SHIFT_N 0x0c5    /* CF PF ZF SF */
#define FLAGS_ROT_N 0x0d5      /* everything but OF */

/* rax = a, rcx = count; the flags start from `cmp 0, cf`, so CF = cf. */
.macro SH op, dst, count, a, cnt, cf, mask
    mov rax, \a
    mov rcx, \cnt
    mov rdx, 0
    cmp rdx, \cf
    \op \dst, \count
    DUMP rax
    DUMP_FLAGS \mask
.endm

    .bss
buf:
    .zero 8

    .text
    .globl _start
_start:
    SH shl, al, 1, 0x81, 0, 0, FLAGS_SHIFT_1
    SH shl, al, 3, 0x31, 0, 0, FLAGS_SHIFT_N
    SH shl, ax, cl, 0x8001, 15, 0, FLAGS_SHIFT_N
    SH shl, eax, 1, 0x80000000, 0, 0, FLAGS_SHIFT_1
    SH shl, rax, 63, 3, 0, 0, FLAGS_SHIFT_N
    SH shl, rax, cl, 1, 65, 0, FLAGS_SHIFT_1    /* count masked to 1 */
    SH shl, eax, cl, 1, 32, 1, FLAGS_STATUS     /* masked to 0: unchanged */
    SH shr, al, 1, 0x81, 0, 0, FLAGS_SHIFT_1
    SH shr, ax, 4, 0x1234, 0, 0, FLAGS_SHIFT_N
    SH shr, eax, cl, 0xffffffff, 31, 0, FLAGS_SHIFT_N
    SH shr, rax, 1, -1, 0, 0, FLAGS_SHIFT_1
    SH sar, al, 1, 0x81, 0, 0, FLAGS_SHIFT_1
    SH sar, ax, 15, 0x8000, 0, 0, FLAGS_SHIFT_N
    SH sar, eax, cl, 0x80000001, 4, 0, FLAGS_SHIFT_N
    SH sar, rax, 62, 0x8000000000000000, 0, 0, FLAGS_SHIFT_N
    SH rol, al, 1, 0x81, 0, 0, FLAGS_STATUS
    SH rol, ax, 4, 0x1234, 0, 1, FLAGS_ROT_N
    SH rol, eax, cl, 0x80000001, 31, 0, FLAGS_ROT_N
    SH rol, rax, 1, 0x4000000000000000, 0, 1, FLAGS_STATUS
    SH ror, al, 1, 0x01, 0, 0, FLAGS_STATUS
    SH ror, al, 1, 0x81, 0, 0, FLAGS_STATUS     /* OF = bit 7 ^ bit 6 = 0 */
    SH ror, al, 1, 0x80, 0, 0, FLAGS_STATUS     /* OF = 1 */
    SH ror, ax, 4, 0x1234, 0, 0, FLAGS_ROT_N
    SH ror, eax, cl, 3, 33, 1, FLAGS_STATUS     /* count 1 */
    SH ror, rax, 8, 0x1122334455667788, 0, 0, FLAGS_ROT_N
    SH rol, al, cl, 0x81, 8, 1, FLAGS_ROT_N     /* full rotation */
    SH rcl, al, 1, 0x80, 0, 1, FLAGS_STATUS
    SH rcl, ax, 3, 0x8001, 0, 1, FLAGS_ROT_N
    SH rcl, eax, cl, 0x80000000, 1, 0, FLAGS_STATUS
    SH rcl, rax, 4, -1, 0, 0, FLAGS_ROT_N
    SH rcr, al, 1, 0x01, 0, 1, FLAGS_STATUS
    SH rcr, ax, cl, 0x0001, 2, 1, FLAGS_ROT_N
    SH rcr, eax, 31, 0x12345678, 0, 1, FLAGS_ROT_N
    SH rcr, rax, 1, 1, 0, 0, FLAGS_STATUS

    lea rbx, [rip + buf]
    mov rax, 0x0102030405060708
    mov [rbx], rax
    shl qword ptr [rbx], 4
    shr dword ptr [rbx], 1
    rol word ptr [rbx + 4], 8
    sar byte ptr [rbx + 7], 1
    mov rax, [rbx]
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
