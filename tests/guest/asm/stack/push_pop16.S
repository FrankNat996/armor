/*
 * push and pop with the operand-size prefix (66): they move 2 bytes and
 * rsp by 2; pop of a 16-bit register keeps its upper bits. Obfuscated
 * code uses them (one ran in a Spotify build).
 */
#include "dump.h"

    .text
    .globl _start
_start:
    mov rbp, rsp
    mov rax, 0x1111222233334444
    push ax                             /* 66 50 */
    mov rcx, rbp
    sub rcx, rsp
    DUMP rcx                            /* 2 */
    mov rbx, 0xaaaabbbbccccdddd
    pop bx                              /* 66 5B: 0xaaaabbbbcccc4444 */
    DUMP rbx

    mov r15, rbp
    sub r15, 64
    mov word ptr [r15], 0x5a5a
    push word ptr [r15]                 /* 66 41 FF 37 */
    mov rdx, -1
    pop dx
    DUMP rdx
    pushw 0x1234                        /* 66 68 iw */
    pushw -2                            /* 66 6A ib */
    mov rsi, [rsp]
    DUMP rsi                            /* the two words: 0x1234fffe in the low half */
    pop word ptr [r15]                  /* 66 41 8F 07 */
    movzx edi, word ptr [r15]
    DUMP rdi                            /* 0xfffe */
    pop ax
    mov rcx, rbp
    sub rcx, rsp
    DUMP rcx                            /* 0 */
    EXIT 0

    .section .note.GNU-stack, "", @progbits
