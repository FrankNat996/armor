/*
 * Code that ran once and was then unmapped must not run again: the call
 * after munmap is an execute fault (SIGSEGV), even for an interpreter that
 * caches decoded instructions.
 */
#include "dump.h"

    .text
    .globl _start
_start:
    mov eax, __NR_mmap
    xor edi, edi
    mov esi, 4096
    mov edx, 7                          /* PROT_READ | PROT_WRITE | PROT_EXEC */
    mov r10d, 0x22                      /* MAP_PRIVATE | MAP_ANONYMOUS */
    mov r8, -1
    xor r9d, r9d
    syscall
    mov rbx, rax
    mov byte ptr [rbx], 0xb8            /* mov eax, 5 ; ret */
    mov dword ptr [rbx + 1], 5
    mov byte ptr [rbx + 5], 0xc3
    call rbx
    DUMP rax                            /* 5 */
    mov eax, __NR_munmap
    mov rdi, rbx
    mov esi, 4096
    syscall
    call rbx                            /* SIGSEGV */
    EXIT 0

    .section .note.GNU-stack, "", @progbits
