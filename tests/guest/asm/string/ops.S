/*
 * movs, stos, lods, cmps, scas at every size, single and with
 * rep/repe/repne, forward and backward (DF=1), rcx = 0, a large rep stos
 * and an overlapping forward rep movsb (which repeats the source pattern).
 */
#include "dump.h"

    .bss
    .balign 16
src:
    .zero 64
dst:
    .zero 64
big:
    .zero 20000

    .text
    .globl _start
_start:
    /* Fill src with 0x00..0x3f using stosb + inc. */
    lea rdi, [rip + src]
    xor eax, eax
    mov ecx, 64
1:
    stosb
    inc al
    loop 1b
    DUMP rdi
    lea rax, [rip + src + 64]
    sub rdi, rax
    DUMP rdi                    /* 0 */

    lea rsi, [rip + src]
    lea rdi, [rip + dst]
    mov ecx, 3
    rep movsq
    DUMP rcx
    movsd                       /* 4 more bytes */
    movsw
    movsb
    mov rax, [rip + dst + 24]
    DUMP rax
    mov rax, [rip + dst + 16]
    DUMP rax

    lea rsi, [rip + src + 8]
    lodsq
    DUMP rax
    lodsb
    DUMP rax
    lodsw
    DUMP rax
    lodsd
    DUMP rax

    /* Backward copy with DF=1. */
    std
    lea rsi, [rip + src + 63]
    lea rdi, [rip + dst + 63]
    mov ecx, 8
    rep movsb
    cld
    mov rax, [rip + dst + 56]
    DUMP rax
    lea rax, [rip + dst + 55]
    sub rdi, rax
    DUMP rdi                    /* 0 */

    /* rep with rcx = 0 does nothing. */
    xor ecx, ecx
    lea rdi, [rip + dst]
    mov al, 0xee
    rep stosb
    mov rax, [rip + dst]
    DUMP rax

    /* repe cmpsb stops at the first difference. */
    lea rsi, [rip + src]
    lea rdi, [rip + dst]
    mov byte ptr [rip + dst + 5], 0x99
    mov ecx, 20
    repe cmpsb
    DUMP rcx
    DUMP_FLAGS FLAGS_STATUS
    lea rax, [rip + src]
    sub rsi, rax
    DUMP rsi                    /* 6 */

    /* repne scasb finds a byte; scasq compares once. */
    lea rdi, [rip + src]
    mov al, 0x10
    mov ecx, 64
    repne scasb
    DUMP rcx
    DUMP_FLAGS FLAGS_STATUS
    lea rdi, [rip + src]
    mov rax, -1
    scasq
    DUMP_FLAGS FLAGS_STATUS
    lea rsi, [rip + src]
    lea rdi, [rip + src]
    mov ecx, 8
    repe cmpsd
    DUMP rcx                    /* 0: all equal */
    DUMP_FLAGS FLAGS_STATUS

    /* Large rep stosq and rep stosb. */
    lea rdi, [rip + big]
    mov rax, 0x0101010101010101
    mov ecx, 2000
    rep stosq
    mov al, 0x7f
    mov ecx, 3999
    rep stosb
    mov rax, [rip + big + 15992]
    DUMP rax
    mov rax, [rip + big + 3992]
    DUMP rax
    mov rax, [rip + big + 3998]
    DUMP rax

    /* Overlapping forward copy: dst = src + 1 repeats the first byte. */
    lea rsi, [rip + big]
    mov byte ptr [rsi], 0xab
    lea rdi, [rip + big + 1]
    mov ecx, 100
    rep movsb
    mov rax, [rip + big + 90]
    DUMP rax
    /* Non-overlapping large rep movsb. */
    lea rsi, [rip + big]
    lea rdi, [rip + big + 10000]
    mov ecx, 9000
    rep movsb
    mov rax, [rip + big + 10000]
    DUMP rax
    mov rax, [rip + big + 18992]
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
