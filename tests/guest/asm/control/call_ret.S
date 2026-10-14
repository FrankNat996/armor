/*
 * call pushes the return address and ret pops it; nested calls keep the
 * stack balanced; REX.W overrides an operand-size prefix on call.
 */
#include "dump.h"

    .text
    .globl _start
_start:
    mov rbp, rsp
    call return_address
after_first:
    lea rcx, [rip + after_first]
    sub rax, rcx
    DUMP rax                            /* 0 */
    mov rax, rbp
    sub rax, rsp
    DUMP rax                            /* 0: balanced */

    call nested
    DUMP rax                            /* 0 */
    DUMP rbx                            /* preserved */
    mov rax, rbp
    sub rax, rsp
    DUMP rax                            /* 0 */

    mov rax, 0
    call count_down
    DUMP rax                            /* 5 */

    /* 66 66 REX.W call: REX.W overrides the operand-size prefix (the
       padding compilers put around __tls_get_addr calls). */
    .byte 0x66, 0x66, 0x48, 0xe8
    .long return_address - 1f
1:
    lea rcx, [rip + 1b]
    sub rax, rcx
    DUMP rax                            /* 0 */
    mov rax, rbp
    sub rax, rsp
    DUMP rax                            /* 0 */
    EXIT 0

/* Returns its own return address in rax. */
return_address:
    mov rax, [rsp]
    ret

/* Calls return_address and checks the address it got. */
nested:
    push rbx
    mov rbx, 0x5555
    call return_address
after_nested:
    lea rcx, [rip + after_nested]
    sub rax, rcx
    pop rbx
    ret

/* Recursion: adds 1 to rax five times. */
count_down:
    add rax, 1
    cmp rax, 5
    je 1f
    call count_down
1:
    ret

    .section .note.GNU-stack, "", @progbits
