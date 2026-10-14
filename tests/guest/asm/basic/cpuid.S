/*
 * CPUID leaves armor reproduces from qemu's reduced qemu64 model
 * (crates/armor-cpu/src/cpuid.rs). The brand string (0x80000002-4) and the
 * hypervisor leaves (0x40000000) deliberately differ and are not dumped.
 */
#include "dump.h"

.macro LEAF leaf, sub
    mov eax, \leaf
    mov ecx, \sub
    cpuid
    DUMP rax
    DUMP rbx
    DUMP rcx
    DUMP rdx
.endm

    .text
    .globl _start
_start:
    LEAF 0, 0
    LEAF 1, 0
    LEAF 2, 0
    LEAF 4, 0
    LEAF 5, 0
    LEAF 7, 0
    LEAF 0xb, 0
    LEAF 0xb, 1
    LEAF 0xb, 2
    LEAF 0xd, 0
    LEAF 0xd, 1
    LEAF 0xe, 0
    LEAF 0x80000000, 0
    LEAF 0x80000001, 0
    LEAF 0x80000005, 0
    LEAF 0x80000006, 0
    LEAF 0x80000007, 0
    LEAF 0x80000008, 0
    LEAF 0x8000000b, 0
    /* Upper 32 bits of rax and rcx are ignored. */
    mov rax, 0xffffffff00000000
    mov rcx, 0xffffffff00000000
    cpuid
    DUMP rax
    DUMP rbx
    EXIT 0

    .section .note.GNU-stack, "", @progbits
