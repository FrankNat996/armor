/*
 * Prefixes and no-op forms seen in compiled code: endbr64, multi-byte nops
 * with operand-size and segment prefixes, pause, notrack and bnd branches,
 * `rep ret`, `addr32 call`, and 32-bit addressing with the 67 prefix.
 */
#include "dump.h"

    .bss
    .balign 16
buf:
    .zero 16

    .text
    .globl _start
_start:
    endbr64
    nop
    xchg ax, ax
    nop word ptr [rax + rax]
    .byte 0x66, 0x2e, 0x0f, 0x1f, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00
    .byte 0x0f, 0x1f, 0x40, 0x00
    pause
    prefetcht0 [rip + buf]
    prefetchnta [rip + buf]
    sfence
    lfence
    mfence

    lea rax, [rip + target]
    notrack jmp rax
    mov rbx, 1
target:
    xor ebx, ebx
    .byte 0x67                  /* addr32 call (67 E8) */
    call function
    .byte 0xf2                  /* bnd call (F2 E8) */
    call function
    DUMP rbx                    /* 2 */

    /* 67: the address is computed in 32 bits. */
    mov rax, 0xffffffff00000000
    lea rcx, [rip + buf]
    add rax, rcx
    mov qword ptr [rcx], 0x5a5a
    mov rdx, qword ptr [eax]    /* only the low 32 bits of rax are used */
    DUMP rdx
    lea edx, [eax + 0x10]
    sub rdx, rcx
    DUMP rdx                    /* 16 */
    EXIT 0

function:
    add rbx, 1
    rep ret

    .section .note.GNU-stack, "", @progbits
