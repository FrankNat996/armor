/*
 * Self-modifying code: the program writes instructions into a writable
 * and executable page, runs them, rewrites them and runs them again; then
 * unmaps the page and maps new code at the same address. An interpreter
 * that caches decoded instructions must notice every change.
 */
#include "dump.h"

#define PROT_RWX 7
#define MAP_PRIVATE_ANON 0x22
#define MAP_FIXED_PRIVATE_ANON 0x32

/* mov eax, imm32 ; ret */
.macro EMIT_MOV_RET base, imm
    mov byte ptr [\base], 0xb8
    mov dword ptr [\base + 1], \imm
    mov byte ptr [\base + 5], 0xc3
.endm

    .text
    .globl _start
_start:
    /* An RWX page anywhere. */
    mov eax, __NR_mmap
    xor edi, edi
    mov esi, 4096
    mov edx, PROT_RWX
    mov r10d, MAP_PRIVATE_ANON
    mov r8, -1
    xor r9d, r9d
    syscall
    mov rbx, rax

    EMIT_MOV_RET rbx, 1
    call rbx
    DUMP rax                            /* 1 */
    /* Rewrite the immediate only, then the opcode too. */
    mov dword ptr [rbx + 1], 2
    call rbx
    DUMP rax                            /* 2 */
    mov byte ptr [rbx], 0x31            /* xor eax, eax ; ret */
    mov byte ptr [rbx + 1], 0xc0
    mov byte ptr [rbx + 2], 0xc3
    call rbx
    DUMP rax                            /* 0 */
    /* The same code in a loop, changed between iterations. */
    mov r12d, 3
loop:
    EMIT_MOV_RET rbx, r12d
    call rbx
    DUMP rax                            /* 3, 2, 1 */
    dec r12d
    jnz loop

    /* Unmap, map a fresh page at the same address, new code. */
    mov eax, __NR_munmap
    mov rdi, rbx
    mov esi, 4096
    syscall
    mov eax, __NR_mmap
    mov rdi, rbx
    mov esi, 4096
    mov edx, PROT_RWX
    mov r10d, MAP_FIXED_PRIVATE_ANON
    mov r8, -1
    xor r9d, r9d
    syscall
    EMIT_MOV_RET rbx, 0x42
    call rbx
    DUMP rax                            /* 0x42 */
    EXIT 0

    .section .note.GNU-stack, "", @progbits
