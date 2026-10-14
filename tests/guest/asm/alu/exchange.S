/*
 * xchg, cmpxchg and xadd, with and without lock, and lock forms of
 * ordinary read-modify-write instructions.
 */
#include "dump.h"

    .bss
buf:
    .zero 16

    .text
    .globl _start
_start:
    lea rbx, [rip + buf]
    mov rax, 1
    mov rcx, 2
    xchg rax, rcx
    DUMP rax
    DUMP rcx
    mov r9, 9
    xchg rax, r9                /* 90+r with REX.B */
    DUMP rax
    DUMP r9
    mov eax, 0x11111111
    mov rdx, -1
    xchg edx, eax               /* 32-bit: zero-extends both */
    DUMP rax
    DUMP rdx
    mov qword ptr [rbx], 0x55
    mov rax, 0x66
    xchg [rbx], rax
    DUMP rax
    mov rax, [rbx]
    DUMP rax
    mov rcx, 0x1234
    mov rax, 0x1234
    xchg cx, ax
    DUMP rcx

    /* cmpxchg: success stores the source, failure loads rax. */
    mov qword ptr [rbx], 10
    mov rax, 10
    mov rcx, 20
    lock cmpxchg [rbx], rcx
    DUMP_FLAGS FLAGS_STATUS
    DUMP rax
    mov rax, [rbx]
    DUMP rax
    mov rax, 99
    lock cmpxchg [rbx], rcx
    DUMP_FLAGS FLAGS_STATUS
    DUMP rax
    mov rax, 0xffffffff00000005
    mov edx, 5
    mov ecx, 7
    cmpxchg edx, ecx
    DUMP rdx
    DUMP rax
    /* 32-bit register destination, compare fails: eax gets the
       destination; is the destination register written back? */
    mov rax, 0xffffffff00000005
    mov rdx, 0xffffffff00000009
    mov ecx, 7
    cmpxchg edx, ecx
    DUMP rdx
    DUMP rax
    mov al, 3
    mov byte ptr [rbx + 8], 4
    cmpxchg [rbx + 8], cl
    DUMP rax

    /* xadd: destination gets the sum, source the old destination. */
    mov qword ptr [rbx], 100
    mov rcx, 5
    lock xadd [rbx], rcx
    DUMP rcx
    mov rax, [rbx]
    DUMP rax
    DUMP_FLAGS FLAGS_STATUS
    mov rax, 3
    xadd rax, rax
    DUMP rax
    mov rax, 1
    mov rdx, 0xff
    xadd dl, al
    DUMP rax
    DUMP rdx
    DUMP_FLAGS FLAGS_STATUS

    mov qword ptr [rbx], 1
    lock add qword ptr [rbx], 2
    lock sub qword ptr [rbx], 1
    lock or qword ptr [rbx], 0x100
    lock and dword ptr [rbx], 0xff0f
    lock xor word ptr [rbx], 3
    lock dec qword ptr [rbx]
    lock neg qword ptr [rbx]
    lock not qword ptr [rbx]
    lock btc qword ptr [rbx], 63
    mov rax, [rbx]
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
