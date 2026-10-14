/*
 * i386 string instructions (ADR-0029): esi, edi and ecx with 32-bit
 * addresses. rep movsb and movsd, rep stosd, repe cmpsb, repne scasb,
 * lodsb, backwards with std, and xlat (ebx + al).
 */
#include "dump32.h"

    .section .rodata
src:
    .ascii "abcdefghijklmnop"
hex:
    .ascii "0123456789abcdef"

    .bss
dst:
    .zero 64

    .text
    .globl _start
_start:
    cld
    /* rep movsb: 11 bytes; esi and edi advance, ecx reaches 0. */
    mov esi, offset src
    mov edi, offset dst
    mov ecx, 11
    rep movsb
    DUMP ecx
    sub esi, offset src
    DUMP esi                    /* 11 */
    sub edi, offset dst
    DUMP edi                    /* 11 */
    DUMP_WRITE dst, 11

    /* rep stosd: 3 dwords of eax. */
    mov edi, offset dst + 16
    mov eax, 0x44434241
    mov ecx, 3
    rep stosd
    DUMP_WRITE dst + 16, 12

    /* rep movsd. */
    mov esi, offset src
    mov edi, offset dst + 32
    mov ecx, 4
    rep movsd
    DUMP_WRITE dst + 32, 16

    /* repe cmpsb: stops after the first difference. */
    mov byte ptr [dst + 32 + 5], 'X'
    mov esi, offset src
    mov edi, offset dst + 32
    mov ecx, 16
    repe cmpsb
    DUMP ecx                    /* 10 */
    DUMP_FLAGS FLAGS_STATUS

    /* repne scasb: finds 'h'. */
    mov edi, offset src
    mov al, 'h'
    mov ecx, 16
    repne scasb
    DUMP ecx                    /* 8 */
    sub edi, offset src
    DUMP edi                    /* 8 */

    /* lodsb backwards. */
    std
    mov esi, offset src + 15
    lodsb
    lodsb
    movzx eax, al
    DUMP eax                    /* 'o' */
    sub esi, offset src
    DUMP esi                    /* 13 */
    /* rep movsb backwards. */
    mov esi, offset src + 3
    mov edi, offset dst + 48 + 3
    mov ecx, 4
    rep movsb
    cld
    DUMP_WRITE dst + 48, 4

    /* xlat: [ebx + al]. */
    mov ebx, offset hex
    mov eax, 0x1234560c
    xlat
    DUMP eax                    /* 0x12345663 ('c') */

    EXIT 0

    .section .note.GNU-stack, "", @progbits
