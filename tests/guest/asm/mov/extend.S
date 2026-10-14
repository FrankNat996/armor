/*
 * movzx, movsx, movsxd and the accumulator sign extensions
 * (cbw, cwde, cdqe, cwd, cdq, cqo).
 */
#include "dump.h"

    .bss
buf:
    .zero 8

    .text
    .globl _start
_start:
    mov rcx, 0x1122334455668899
    mov rax, -1
    movzx eax, cl
    DUMP rax
    mov rax, -1
    movzx ax, cl                /* 16-bit destination keeps the rest */
    DUMP rax
    movzx rax, cx
    DUMP rax
    movsx eax, cl
    DUMP rax
    movsx rax, cl
    DUMP rax
    movsx ax, cl
    DUMP rax
    movsx rax, cx
    DUMP rax
    movsxd rax, ecx
    DUMP rax
    mov rcx, 0x7fffffff
    movsxd rax, ecx
    DUMP rax
    mov rdx, 0x123456789abcdef0
    movzx eax, dh               /* high byte source */
    DUMP rax
    lea rbx, [rip + buf]
    mov qword ptr [rbx], 0x80ff
    movsx rax, byte ptr [rbx + 1]
    DUMP rax
    movzx eax, word ptr [rbx]
    DUMP rax

    mov rax, 0x1234567890abcd80
    cbw
    DUMP rax
    mov rax, 0x123456789abc8000
    cwde
    DUMP rax
    mov rax, 0x1234567880000000
    cdqe
    DUMP rax
    mov rax, 0x8000
    mov rdx, 0x5555555555555555
    cwd
    DUMP rdx
    mov rax, 0x80000000
    cdq
    DUMP rdx
    mov rax, 1
    cqo
    DUMP rdx
    mov rax, -1
    cqo
    DUMP rdx
    EXIT 0

    .section .note.GNU-stack, "", @progbits
