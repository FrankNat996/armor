/*
 * shld and shrd with immediate and cl counts, register and memory
 * destinations, 16/32/64-bit. OF is only defined for count 1, AF never; a
 * count masked to zero changes nothing, flags included. 16-bit counts stay
 * within the operand size (larger ones give undefined results).
 */
#include "dump.h"

#define FLAGS_SHIFT_1 0x8c5    /* CF PF ZF SF OF */
#define FLAGS_SHIFT_N 0x0c5    /* CF PF ZF SF */

/* rax = a, rbx = fill, rcx = count; flags start from `cmp 0, cf`. */
.macro DSH op, dst, src, count, a, fill, cnt, cf, mask
    mov rax, \a
    mov rbx, \fill
    mov rcx, \cnt
    mov rdx, 0
    cmp rdx, \cf
    \op \dst, \src, \count
    DUMP rax
    DUMP_FLAGS \mask
.endm

    .bss
buf:
    .zero 8

    .text
    .globl _start
_start:
    DSH shld, ax, bx, 4, 0x1234, 0xabcd, 0, 0, FLAGS_SHIFT_N
    DSH shld, ax, bx, 1, 0x4000, 0x8000, 0, 0, FLAGS_SHIFT_1
    DSH shld, eax, ebx, cl, 0x80000001, 0xf0000000, 4, 0, FLAGS_SHIFT_N
    DSH shld, eax, ebx, 1, 0x80000000, 0, 0, 0, FLAGS_SHIFT_1
    DSH shld, rax, rbx, 8, 0x1122334455667788, 0x99aabbccddeeff00, 0, 0, FLAGS_SHIFT_N
    DSH shld, rax, rbx, cl, 0x8000000000000000, -1, 63, 0, FLAGS_SHIFT_N
    DSH shld, rax, rbx, cl, 1, 2, 64, 1, FLAGS_STATUS   /* masked to 0 */
    DSH shld, eax, ebx, cl, 1, 2, 32, 1, FLAGS_STATUS   /* masked to 0 */
    DSH shrd, ax, bx, 4, 0x1234, 0xabcd, 0, 0, FLAGS_SHIFT_N
    DSH shrd, ax, bx, 1, 0x0001, 0x0001, 0, 0, FLAGS_SHIFT_1
    DSH shrd, eax, ebx, cl, 0x80000001, 0x0000000f, 4, 0, FLAGS_SHIFT_N
    DSH shrd, eax, ebx, 1, 1, 0, 0, 0, FLAGS_SHIFT_1
    DSH shrd, rax, rbx, 8, 0x1122334455667788, 0x99aabbccddeeff00, 0, 0, FLAGS_SHIFT_N
    DSH shrd, rax, rbx, cl, 0x8000000000000000, 1, 63, 0, FLAGS_SHIFT_N
    DSH shrd, rax, rbx, cl, 0, 0, 65, 0, FLAGS_SHIFT_1  /* count 1, zero result */

    /* Memory destination. */
    mov qword ptr [rip + buf], 0
    mov rax, 0x0123456789abcdef
    mov [rip + buf], rax
    mov rbx, 0xfedcba9876543210
    shld qword ptr [rip + buf], rbx, 12
    mov rax, [rip + buf]
    DUMP rax
    DUMP_FLAGS FLAGS_SHIFT_N
    mov cl, 20
    shrd dword ptr [rip + buf], ebx, cl
    mov rax, [rip + buf]
    DUMP rax
    DUMP_FLAGS FLAGS_SHIFT_N
    EXIT 0
