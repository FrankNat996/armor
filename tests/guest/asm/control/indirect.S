/*
 * Indirect call and jmp through registers, memory and a jump table;
 * ret imm16, leave, push/pop with memory operands, loop/loope/loopne/jrcxz.
 */
#include "dump.h"

    .section .rodata
    .balign 8
table:
    .quad case0, case1, case2

    .bss
slot:
    .zero 16

    .text
    .globl _start
_start:
    mov rbp, rsp
    lea rax, [rip + add_one]
    xor edx, edx
    call rax
    DUMP rdx                    /* 1 */
    lea rbx, [rip + slot]
    mov [rbx], rax
    call qword ptr [rbx]
    DUMP rdx                    /* 2 */

    xor ebx, ebx
    mov ecx, 2
    lea rsi, [rip + table]
    jmp qword ptr [rsi + rcx*8]
case0:
    add rbx, 1
case1:
    add rbx, 10
case2:
    add rbx, 100
    DUMP rbx                    /* 100 */
    lea rax, [rip + after_jump]
    jmp rax
    mov rbx, 0
after_jump:
    DUMP rbx

    push 7
    push 8
    call pop_two_args
    mov rax, rbp
    sub rax, rsp
    DUMP rax                    /* 0: ret 16 released the arguments */
    DUMP rdx                    /* 15 */

    call with_frame
    DUMP rax
    mov rax, rbp
    sub rax, rsp
    DUMP rax

    lea rbx, [rip + slot]
    mov qword ptr [rbx], 0x1234
    push qword ptr [rbx]
    pop qword ptr [rbx + 8]
    mov rax, [rbx + 8]
    DUMP rax
    push 0x55
    pop qword ptr [rsp - 8]     /* address uses the incremented rsp */
    mov rax, [rsp - 8]
    DUMP rax

    xor eax, eax
    mov ecx, 5
1:
    add rax, 1
    loop 1b
    DUMP rax                    /* 5 */
    DUMP rcx                    /* 0 */
    mov ecx, 10
    xor eax, eax
2:
    add rax, 1
    cmp rax, 3
    loopne 2b                   /* stops when rax == 3 */
    DUMP rax
    DUMP rcx
    mov ecx, 10
    xor eax, eax
3:
    add rax, 1
    cmp rax, rax
    loope 3b                    /* runs until rcx == 0 */
    DUMP rax
    xor ecx, ecx
    mov rax, 1
    jrcxz 4f
    mov rax, 2
4:
    DUMP rax
    EXIT 0

add_one:
    add rdx, 1
    ret

/* Adds its two stack arguments into rdx and releases them. */
pop_two_args:
    mov rdx, [rsp + 8]
    add rdx, [rsp + 16]
    ret 16

/* A function with a frame pointer, left with leave. */
with_frame:
    push rbp
    mov rbp, rsp
    sub rsp, 32
    mov qword ptr [rbp - 8], 42
    mov rax, [rbp - 8]
    leave
    ret

    .section .note.GNU-stack, "", @progbits
