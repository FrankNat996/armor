/*
 * i386 stack operations (ADR-0029): 4-byte push and pop of registers,
 * immediates and memory, pop into memory addressed through esp, pushfd
 * and popfd, call and ret (with an immediate), enter (levels 0, 1, 3) and
 * leave. esp itself differs between runs, so the test prints values and
 * esp differences only.
 */
#include "dump32.h"

    .data
slot:
    .long 0x11223344
popped:
    .long 0
saved_esp:
    .long 0

    .text
    .globl _start
_start:
    mov [saved_esp], esp

    /* push imm8 (sign-extended to 32 bits), imm32, register, memory. */
    push -2
    push 0x12345678
    mov eax, 0xcafef00d
    push eax
    push dword ptr [slot]
    mov ebx, [saved_esp]
    sub ebx, esp                /* 16: four 4-byte pushes */
    DUMP ebx
    pop ecx
    DUMP ecx                    /* 0x11223344 */
    pop ecx
    DUMP ecx                    /* 0xcafef00d */
    pop dword ptr [popped]
    mov ecx, [popped]
    DUMP ecx                    /* 0x12345678 */
    /* pop into [esp]: the address uses the incremented esp. */
    push 0x55
    push 0x66
    pop dword ptr [esp]
    pop ecx
    DUMP ecx                    /* 0x66 */
    pop ecx
    DUMP ecx                    /* -2 */
    mov ebx, [saved_esp]
    sub ebx, esp
    DUMP ebx                    /* 0 */

    /* pushfd / popfd: CF and ZF through the stack. */
    stc
    pushfd
    pop eax
    and eax, 0x41               /* CF, ZF */
    DUMP eax
    push 0x40                   /* ZF only */
    popfd
    DUMP_FLAGS FLAGS_STATUS

    /* call and ret, ret imm16 popping the arguments. */
    push 3
    push 4
    call add_args
    DUMP eax                    /* 7 */
    mov ebx, [saved_esp]
    sub ebx, esp
    DUMP ebx                    /* 0: ret 8 released the arguments */

    /* The return address is the instruction after the call. */
    call where
after_call:
    sub eax, offset after_call
    DUMP eax                    /* 0 */

    /* enter / leave. */
    mov ebp, 0x1000
    enter 12, 0
    mov eax, [saved_esp]
    sub eax, ebp                /* 4: old ebp pushed */
    DUMP eax
    mov eax, ebp
    sub eax, esp                /* 12 allocated */
    DUMP eax
    mov eax, [ebp]
    DUMP eax                    /* 0x1000 */
    leave
    DUMP ebp                    /* 0x1000 */
    mov ebx, [saved_esp]
    sub ebx, esp
    DUMP ebx                    /* 0 */

    /* enter with nesting levels copies the frame pointers. */
    mov ebp, esp
    sub esp, 16
    mov dword ptr [ebp - 4], 0xaaaa
    mov dword ptr [ebp - 8], 0xbbbb
    mov dword ptr [ebp - 12], 0xcccc
    mov edi, esp
    enter 4, 3
    mov eax, edi
    sub eax, esp                /* 4 (ebp) + 8 (2 links) + 4 (frame) + 4 */
    DUMP eax
    mov eax, [ebp - 4]
    DUMP eax                    /* 0xaaaa */
    mov eax, [ebp - 8]
    DUMP eax                    /* 0xbbbb */
    lea eax, [ebp]
    sub eax, [ebp - 12]
    DUMP eax                    /* 0: the new frame's own pointer */
    leave
    add esp, 16

    EXIT 0

add_args:
    mov eax, [esp + 4]
    add eax, [esp + 8]
    ret 8

where:
    mov eax, [esp]
    ret

    .section .note.GNU-stack, "", @progbits
