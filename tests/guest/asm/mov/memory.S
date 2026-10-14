/*
 * mov to and from memory: RIP-relative, base + index*scale + disp, every
 * operand size, immediates to memory.
 */
#include "dump.h"

    .bss
buf:
    .zero 32

    .text
    .globl _start
_start:
    mov rax, 0x0123456789abcdef
    mov [rip + buf], rax
    mov rbx, [rip + buf]
    DUMP rbx

    lea rbx, [rip + buf]
    mov rcx, 1
    mov dword ptr [rbx + rcx*8], 0x89abcdef
    mov word ptr [rbx + rcx*8 + 4], 0x1234
    mov byte ptr [rbx + rcx*8 + 6], 0x56
    mov rax, [rbx + 8]
    DUMP rax

    mov qword ptr [rbx + 16], -3     /* sign-extended imm32 */
    mov rax, [rbx + 16]
    DUMP rax

    mov rdx, -1
    mov edx, [rbx]              /* 32-bit load zero-extends */
    DUMP rdx
    mov rdx, -1
    mov dx, [rbx]               /* 16-bit load preserves the rest */
    DUMP rdx
    mov rdx, -1
    mov dl, [rbx + 1]
    DUMP rdx
    mov rax, 0x4142
    mov [rbx + 24], ah          /* high byte to memory */
    mov [rbx + 25], al
    mov rax, [rbx + 24]
    DUMP rax

    mov r13, rbx
    mov rax, [r13]              /* r13 base needs disp8 0 */
    DUMP rax
    mov r12, rbx
    mov rax, [r12 + 8]          /* r12 base needs a SIB */
    DUMP rax
    mov r9, 2
    mov rax, [rbx + r9*4]       /* REX.X index */
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
