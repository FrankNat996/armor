/*
 * cmpxchg8b and cmpxchg16b (CX16), with and without LOCK: success and
 * failure, ZF, the other flags left alone, and the upper halves of rax and
 * rdx after cmpxchg8b fails.
 */
#include "dump.h"

    .data
    .balign 16
slot8:
    .quad 0x1111111122222222
    .quad 0
slot16:
    .quad 0x1111111111111111, 0x2222222222222222

    .text
    .globl _start
_start:
    mov rax, 0xaaaaaaaa22222222        /* edx:eax matches slot8 */
    mov rdx, 0xbbbbbbbb11111111
    mov rbx, 0xcccccccc44444444
    mov rcx, 0xdddddddd33333333
    stc
    lock cmpxchg8b [rip + slot8]
    DUMP_FLAGS FLAGS_STATUS            /* ZF, CF still set */
    DUMP rax
    DUMP rdx
    mov rsi, [rip + slot8]
    DUMP rsi

    clc
    cmpxchg8b [rip + slot8]            /* no longer matches */
    DUMP_FLAGS FLAGS_STATUS
    DUMP rax                           /* the slot's low half */
    DUMP rdx
    mov rsi, [rip + slot8]
    DUMP rsi

    mov rax, 0x1111111111111111
    mov rdx, 0x2222222222222222
    mov rbx, 0x3333333333333333
    mov rcx, 0x4444444444444444
    lock cmpxchg16b [rip + slot16]
    DUMP_FLAGS FLAGS_STATUS
    mov rsi, [rip + slot16]
    DUMP rsi
    mov rsi, [rip + slot16 + 8]
    DUMP rsi
    cmpxchg16b [rip + slot16]          /* fails: rdx:rax = the slot */
    DUMP_FLAGS FLAGS_STATUS
    DUMP rax
    DUMP rdx
    EXIT 0

    .section .note.GNU-stack, "", @progbits
