/*
 * lahf, sahf (64-bit mode, with LAHF-LM), xlat, and mov between the
 * accumulator and an absolute address (A0-A3, "moffs"), with 64-bit and
 * (address-size prefix) 32-bit addresses.
 */
#include "dump.h"

    .data
    .balign 8
table:
    .byte 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17
    .fill 247, 1, 0x5a
    .byte 0xee
value:
    .quad 0x8877665544332211
target:
    .quad 0

    .text
    .globl _start
_start:
    /* lahf after different flag states; bits 3 and 5 read as 0. */
    mov rax, 0x1122334455667788
    mov ecx, 0x7fffffff
    add ecx, 1                         /* OF SF AF PF, no CF ZF */
    lahf
    DUMP rax
    xor ecx, ecx                       /* ZF PF */
    lahf
    DUMP rax
    stc
    lahf
    DUMP rax

    /* sahf sets SF ZF AF PF CF from AH and leaves OF alone. */
    mov ecx, 0x7fffffff
    add ecx, 1                         /* OF = 1 */
    mov ah, 0xff
    sahf
    pushfq
    pop rbx
    DUMP rbx
    mov ah, 0
    sahf
    pushfq
    pop rbx
    DUMP rbx
    mov ah, 0x2a                       /* bits 1, 3, 5 are ignored */
    sahf
    pushfq
    pop rbx
    DUMP rbx

    /* xlat: al = [rbx + al], the rest of rax unchanged. */
    lea rbx, [rip + table]
    mov rax, 0x1234567812345605
    xlat
    DUMP rax
    mov al, 0xff
    xlat
    DUMP rax

    /* moffs loads and stores at every size. */
    mov rax, -1
    movabs al, [value]
    DUMP rax
    movabs ax, [value]
    DUMP rax
    movabs eax, [value]
    DUMP rax
    movabs rax, [value]
    DUMP rax
    mov rax, 0x0102030405060708
    movabs [target], rax
    mov rbx, [rip + target]
    DUMP rbx
    mov al, 0xaa
    movabs [target], al
    mov rbx, [rip + target]
    DUMP rbx
    mov eax, 0xccddeeff
    movabs [target], eax
    mov rbx, [rip + target]
    DUMP rbx
    /* 67 A1 imm32: mov eax, [imm32]. */
    mov rax, -1
    .byte 0x67, 0xa1
    .long value
    DUMP rax
    EXIT 0

    .section .note.GNU-stack, "", @progbits
