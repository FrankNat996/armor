/*
 * bsf, bsr (and the F3-prefixed tzcnt/lzcnt encodings, which run as bsf/bsr
 * on a CPU without BMI1/LZCNT), bswap, bt, bts, btr, btc.
 * For bsf/bsr only ZF and a non-zero source's result are defined; for the
 * bit tests only CF.
 */
#include "dump.h"

#define FLAGS_ZF 0x40
#define FLAGS_CF 0x1

    .bss
bits:
    .zero 32

    .text
    .globl _start
_start:
    mov rcx, 0x0000010000008000
    bsf rax, rcx
    DUMP rax
    DUMP_FLAGS FLAGS_ZF
    bsr rax, rcx
    DUMP rax
    bsf eax, ecx
    DUMP rax
    bsr ax, cx
    DUMP rax
    xor ecx, ecx
    mov rax, 5
    bsf rax, rcx
    DUMP_FLAGS FLAGS_ZF
    mov rcx, 0x80
    rep bsf rdx, rcx            /* tzcnt encoding */
    DUMP rdx
    rep bsr rdx, rcx            /* lzcnt encoding */
    DUMP rdx

    mov rax, 0x1122334455667788
    bswap rax
    DUMP rax
    bswap eax
    DUMP rax
    mov r10, 0x0102030405060708
    bswap r10
    DUMP r10

    mov rax, 0x10
    bt rax, 4
    DUMP_FLAGS FLAGS_CF
    bt rax, 68                  /* immediate taken modulo 64 */
    DUMP_FLAGS FLAGS_CF
    mov rcx, 3
    bts rax, rcx
    DUMP rax
    DUMP_FLAGS FLAGS_CF
    btr rax, 4
    DUMP rax
    DUMP_FLAGS FLAGS_CF
    btc eax, 31
    DUMP rax
    btc ax, cx
    DUMP rax

    /* Memory bit strings: a register offset can leave the word. */
    lea rbx, [rip + bits + 16]
    mov rcx, 70
    bts qword ptr [rbx], rcx
    mov rcx, -1
    bts qword ptr [rbx], rcx
    mov rcx, -64
    bts dword ptr [rbx], ecx
    bt qword ptr [rbx], 2
    DUMP_FLAGS FLAGS_CF
    mov rax, [rbx - 16]
    DUMP rax
    mov rax, [rbx - 8]
    DUMP rax
    mov rax, [rbx]
    DUMP rax
    mov rax, [rbx + 8]
    DUMP rax
    mov rcx, 70
    btr qword ptr [rbx], rcx
    DUMP_FLAGS FLAGS_CF
    mov rax, [rbx + 8]
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
