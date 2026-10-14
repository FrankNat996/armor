/*
 * mov at every operand size: 32-bit writes zero-extend, 8- and 16-bit
 * writes preserve the rest of the register; AH..BH vs SPL..DIL; imm64 and
 * sign-extended imm32.
 */
#include "dump.h"

    .text
    .globl _start
_start:
    mov rax, 0x1122334455667788 /* REX.W B8: 64-bit immediate */
    DUMP rax
    mov al, 0xaa
    DUMP rax
    mov ah, 0xbb
    DUMP rax
    mov ax, 0xcccc
    DUMP rax
    mov eax, 0xdddddddd         /* zero-extends */
    DUMP rax

    mov rbx, -2                 /* C7 /0 with REX.W: sign-extended imm32 */
    DUMP rbx
    mov r9d, -1
    DUMP r9
    mov r12b, 5                 /* REX.B selects r12 */
    DUMP r12

    mov rsi, 0x1111111111111111
    mov rdi, 0x2222222222222222
    mov sil, dil                /* REX: SIL/DIL, not DH/BH */
    DUMP rsi
    mov rdx, 0x3333333333333333
    mov rbx, 0x4444444444444444
    mov dh, bl                  /* no REX: DH/BL */
    DUMP rdx

    mov rcx, rax                /* 89 /r */
    DUMP rcx
    mov r8, 0x8888888888888888
    mov ecx, r8d                /* REX.R source, zero-extends */
    DUMP rcx
    mov r10, rcx                /* 8B /r with REX.R */
    DUMP r10
    EXIT 0

    .section .note.GNU-stack, "", @progbits
