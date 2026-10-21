/*
 * BMI1, BMI2, LZCNT and MOVBE (run with `+bmi1,+bmi2,+abm,+movbe`
 * declared, difftest's CASE_FEATURES; docs/tech/avx.md): every operation
 * on edge values (zero, all ones, counts and indexes at and past the
 * operand size), 32 and 64 bits, with the defined flags only (the masks
 * below, from the Intel SDM's flags sections).
 */
#include "dump.h"

/* Defined flags: SF ZF CF OF; ZF CF OF (bextr); ZF CF (tzcnt, lzcnt). */
#define FLAGS_BMI 0x8c1
#define FLAGS_BEXTR 0x841
#define FLAGS_COUNT 0x41

/* Every status flag set, to show that an operation leaves them. */
.macro SET_FLAGS
    push 0x8d7
    popfq
.endm

/* dst = op(rbx, rcx) for 64 bits and 32 bits (rax prefilled with ones,
   so that the 32-bit form shows its zero extension). */
.macro RR op, a, b, mask
    mov rbx, \a
    mov rcx, \b
    mov rax, -1
    \op rax, rbx, rcx
    DUMP rax
    DUMP_FLAGS \mask
    mov rax, -1
    \op eax, ebx, ecx
    DUMP rax
    DUMP_FLAGS \mask
.endm

/* dst = op(rbx), the one-source forms (blsi, blsmsk, blsr, tzcnt,
   lzcnt). */
.macro R op, a, mask
    mov rbx, \a
    mov rax, -1
    \op rax, rbx
    DUMP rax
    DUMP_FLAGS \mask
    mov rax, -1
    \op eax, ebx
    DUMP rax
    DUMP_FLAGS \mask
.endm

    .data
    .balign 8
value:
    .quad 0x0123456789abcdef
out:
    .quad 0

    .text
    .globl _start
_start:
    /* andn: ~rbx & rcx. */
    .irp a, 0, -1, 0x00ff00ff00ff00ff
    .irp b, 0, -1, 0x0f0f0f0f0f0f0f0f
    RR andn, \a, \b, FLAGS_BMI
    .endr
    .endr
    andn rax, rbx, [rip + value]
    DUMP rax

    /* bextr: start in bits 7:0 of rcx, length in 15:8. */
    .irp control, 0, 0x0804, 0x1000, 0x2010, 0x4000, 0x08ff, 0xff20, 0x1f3f, 0x0040, 0x0820
    RR bextr, 0x8123456789abcdef, \control, FLAGS_BEXTR
    .endr

    /* blsi, blsmsk, blsr. */
    .irp a, 0, 1, 0x8000000000000000, 0x80000000, 0x00f0f000, -1
    R blsi, \a, FLAGS_BMI
    R blsmsk, \a, FLAGS_BMI
    R blsr, \a, FLAGS_BMI
    .endr
    blsr rax, [rip + value]
    DUMP rax

    /* bzhi: the index in bits 7:0 of rcx. */
    .irp index, 0, 1, 31, 32, 33, 63, 64, 0x1ff, 0x100, 200
    RR bzhi, -1, \index, FLAGS_BMI
    .endr

    /* pdep, pext (no flags). */
    SET_FLAGS
    .irp a, 0, -1, 0x123456789abcdef0
    .irp b, 0, -1, 0x00ff00ff0f0f0f0f, 0x8000000000000001
    RR pdep, \a, \b, FLAGS_STATUS
    RR pext, \a, \b, FLAGS_STATUS
    .endr
    .endr

    /* shlx, shrx, sarx: the count masked to the operand size. */
    .irp count, 0, 1, 31, 32, 63, 64, 65
    SET_FLAGS
    RR shlx, 0x8000000080000001, \count, FLAGS_STATUS
    RR shrx, 0x8000000080000001, \count, FLAGS_STATUS
    RR sarx, 0x8000000080000001, \count, FLAGS_STATUS
    .endr
    shlx rax, [rip + value], rcx
    DUMP rax

    /* rorx. */
    mov rbx, 0x0123456789abcdef
    SET_FLAGS
    .irp count, 0, 4, 31, 32, 36, 63, 64
    mov rax, -1
    rorx rax, rbx, \count
    DUMP rax
    mov rax, -1
    rorx eax, ebx, \count
    DUMP rax
    .endr
    DUMP_FLAGS FLAGS_STATUS

    /* mulx: rdx times the source, high half in the first operand. */
    .irp a, 0, 1, -1, 0x8000000000000000
    .irp b, 0, 2, -1, 0x123456789abcdef1
    mov rdx, \a
    mov rbx, \b
    SET_FLAGS
    mulx rcx, rax, rbx
    DUMP rcx
    DUMP rax
    mulx ecx, eax, ebx
    DUMP rcx
    DUMP rax
    DUMP_FLAGS FLAGS_STATUS
    .endr
    .endr
    /* The same register for both halves: the high half stays. */
    mov rdx, -1
    mov rbx, 3
    mulx rax, rax, rbx
    DUMP rax

    /* tzcnt, lzcnt: the operand size for 0; 16 bits too. */
    .irp a, 0, 1, 0x8000000000000000, 0x80000000, 0x00f00000, -1, 0x10000
    R tzcnt, \a, FLAGS_COUNT
    R lzcnt, \a, FLAGS_COUNT
    mov rbx, \a
    mov rax, -1
    tzcnt ax, bx
    DUMP rax
    DUMP_FLAGS FLAGS_COUNT
    lzcnt ax, bx
    DUMP rax
    DUMP_FLAGS FLAGS_COUNT
    .endr
    lzcnt rax, [rip + value]
    DUMP rax

    /* movbe: loads and stores of 16, 32 and 64 bits. */
    mov rax, -1
    movbe rax, [rip + value]
    DUMP rax
    mov rax, -1
    movbe eax, [rip + value]
    DUMP rax
    mov rax, -1
    movbe ax, [rip + value]
    DUMP rax
    mov rbx, 0x1122334455667788
    movbe [rip + out], rbx
    mov rax, [rip + out]
    DUMP rax
    movbe [rip + out], ebx
    mov rax, [rip + out]
    DUMP rax
    movbe [rip + out], bx
    mov rax, [rip + out]
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
