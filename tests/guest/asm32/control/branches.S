/*
 * i386 control flow (ADR-0029): jmp rel8 and rel32, jcc, indirect jmp and
 * call through a register and through a table in memory, loop, loope,
 * loopne and jecxz counting in ecx, and the one-byte inc and dec of
 * 40-4F (REX prefixes in 64-bit mode) with their flags.
 */
#include "dump32.h"

    .section .rodata
    .balign 4
table:
    .long case0, case1, case2

    .text
    .globl _start
_start:
    /* jmp rel8 and rel32. */
    xor eax, eax
    jmp 1f
    mov eax, 0xdead
1:  jmp far_target
back:
    DUMP eax                    /* 0x22 */

    /* jcc both ways. */
    mov ecx, 5
    cmp ecx, 5
    jne 2f
    or eax, 0x100
2:  cmp ecx, 6
    jl 3f
    or eax, 0x200
3:  DUMP eax                    /* 0x122 */

    /* Indirect through a register, and through the table. */
    mov edx, offset ind_target
    jmp edx
ind_back:
    DUMP eax                    /* 0x3122 */
    xor esi, esi
    xor ebx, ebx
4:  call [table + esi * 4]
    inc esi
    cmp esi, 3
    jb 4b
    DUMP ebx                    /* 0x321 */
    mov edx, offset case2
    call edx
    DUMP ebx                    /* 0x3213 */

    /* loop: ecx times. */
    mov ecx, 7
    xor eax, eax
5:  add eax, 2
    loop 5b
    DUMP eax                    /* 14 */
    DUMP ecx                    /* 0 */
    /* loope stops when ZF clears, loopne when it sets. */
    mov ecx, 10
    xor eax, eax
6:  inc eax
    cmp eax, 4
    loopne 6b
    DUMP eax                    /* 4 */
    DUMP ecx                    /* 6 */
    mov ecx, 10
    xor eax, eax
7:  inc eax
    cmp eax, eax
    loope 7b
    DUMP eax                    /* 10 */
    /* jecxz. */
    xor ecx, ecx
    mov eax, 1
    jecxz 8f
    mov eax, 2
8:  DUMP eax                    /* 1 */

    /* One-byte inc and dec: flags, CF untouched. */
    stc
    mov eax, 0x7fffffff
    inc eax                     /* 40: OF SF, CF kept */
    DUMP eax
    DUMP_FLAGS FLAGS_STATUS
    mov edi, 1
    dec edi                     /* 4f: ZF */
    DUMP edi
    DUMP_FLAGS FLAGS_STATUS

    EXIT 0

case0:
    or ebx, 1
    ret
case1:
    shl ebx, 4
    or ebx, 2
    ret
case2:
    shl ebx, 4
    or ebx, 3
    ret

ind_target:
    or eax, 0x3000
    jmp ind_back

    /* Far enough for a rel32 jump. */
    .skip 300, 0x90
far_target:
    mov eax, 0x22
    jmp back

    .section .note.GNU-stack, "", @progbits
