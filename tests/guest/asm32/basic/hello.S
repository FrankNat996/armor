/*
 * i386: prints "hello" with write(2) through int 0x80 (ebx, ecx, edx),
 * then exit_group(7). The first i386 test (ADR-0029): ELF32 loading,
 * compatibility-mode decoding, absolute 32-bit addresses.
 */
#include <asm/unistd_32.h>

    .intel_syntax noprefix

    .section .rodata
msg:
    .ascii "hello\n"
    msg_len = . - msg

    .text
    .globl _start
_start:
    mov eax, __NR_write
    mov ebx, 1                  /* stdout */
    mov ecx, offset msg
    mov edx, msg_len
    int 0x80

    mov eax, __NR_exit_group
    mov ebx, 7
    int 0x80

    .section .note.GNU-stack, "", @progbits
