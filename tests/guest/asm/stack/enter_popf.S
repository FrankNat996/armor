/*
 * enter (levels 0, 1, 2, 3 and 33, which is taken modulo 32) and popf.
 * Stack addresses differ between runs, so frame pointers are dumped as
 * distances from the stack pointer at the start.
 */
#include "dump.h"

/* DUMP_DISTANCE reg: dumps r12 - reg. */
.macro DUMP_DISTANCE reg
    mov rax, r12
    sub rax, \reg
    DUMP rax
.endm

/* POPF_DUMP value: popf of value, then the flags pushf sees. */
.macro POPF_DUMP value
    mov rax, \value
    push rax
    popfq
    pushfq
    pop rax
    DUMP rax
.endm

    .text
    .globl _start
_start:
    mov r12, rsp
    mov rbp, r12

    enter 0, 0
    DUMP_DISTANCE rbp                  /* 8 */
    DUMP_DISTANCE rsp                  /* 8 */
    mov rbx, [rbp]
    DUMP_DISTANCE rbx                  /* 0: the old rbp */
    leave
    DUMP_DISTANCE rsp                  /* 0 */
    DUMP_DISTANCE rbp

    enter 0x123, 0
    DUMP_DISTANCE rsp                  /* 8 + 0x123 */
    leave

    /* Nested frames: each level copies the enclosing frame pointers. */
    enter 0, 1
    DUMP_DISTANCE rbp
    DUMP_DISTANCE rsp
    enter 0, 2
    DUMP_DISTANCE rbp
    DUMP_DISTANCE rsp
    enter 0, 3
    DUMP_DISTANCE rbp
    DUMP_DISTANCE rsp
    enter 0x10, 33                     /* level 33 mod 32 = 1 */
    DUMP_DISTANCE rbp
    DUMP_DISTANCE rsp
    /* Every slot written so far, as distances. */
    lea r13, [rsp + 0x10]
    mov ecx, 11                        /* 88 bytes pushed by the four enters */
1:
    mov rbx, [r13]
    DUMP_DISTANCE rbx
    add r13, 8
    dec ecx
    jnz 1b
    mov rsp, r12
    mov rbp, r12

    pushfq
    pop rax
    DUMP rax                           /* the initial flags */
    POPF_DUMP 0
    POPF_DUMP 0x8d5                    /* status flags */
    POPF_DUMP 0x400                    /* DF */
    POPF_DUMP 0x4000                   /* NT */
    POPF_DUMP 0x40000                  /* AC */
    POPF_DUMP 0x200000                 /* ID: how old code detects CPUID */
    POPF_DUMP 0xfffffffffffffeff       /* everything but TF */
    POPF_DUMP 0x202
    EXIT 0

    .section .note.GNU-stack, "", @progbits
