/*
 * Helpers for guest test programs (docs/testing.md).
 *
 * DUMP reg          writes the 8 bytes of a 64-bit register to stdout.
 *                   Preserves every register and the flags.
 * DUMP_FLAGS mask   writes RFLAGS & mask (8 bytes) to stdout.
 *                   Preserves every register but CLOBBERS the flags.
 * DUMPX xmm         writes the 16 bytes of an xmm register to stdout.
 *                   Preserves every register and the flags.
 * SETX xmm, lo, hi  loads an xmm register with two 64-bit halves.
 *                   Preserves every other register and the flags.
 * EXIT code         exit_group(code).
 *
 * The helpers use only mov, lea, pushf, pop, and, syscall (plus movdqu for
 * the xmm ones): the basic/ and mov/ tests check those first.
 */
#include <asm/unistd_64.h>

    .intel_syntax noprefix

/* Status flags: CF PF AF ZF SF OF. */
#define FLAGS_STATUS 0x8d5
/* Status flags without AF, which logical instructions leave undefined. */
#define FLAGS_NO_AF 0x8c5

    .pushsection .bss
    .balign 16
dump_xmm:
    .zero 16
dump_save:
    .zero 48
dump_flags_rax:
    .zero 8
dump_value:
    .zero 8
    .popsection

/* write(1, &dump_value, 8), preserving the registers the syscall uses. */
.macro DUMP_WRITE_VALUE
    DUMP_WRITE dump_value, 8
.endm

/* write(1, &symbol, len), preserving every register. */
.macro DUMP_WRITE symbol, len
    mov [rip + dump_save + 0], rax
    mov [rip + dump_save + 8], rdi
    mov [rip + dump_save + 16], rsi
    mov [rip + dump_save + 24], rdx
    mov [rip + dump_save + 32], rcx
    mov [rip + dump_save + 40], r11
    mov eax, __NR_write
    mov edi, 1
    lea rsi, [rip + \symbol]
    mov edx, \len
    syscall
    mov rax, [rip + dump_save + 0]
    mov rdi, [rip + dump_save + 8]
    mov rsi, [rip + dump_save + 16]
    mov rdx, [rip + dump_save + 24]
    mov rcx, [rip + dump_save + 32]
    mov r11, [rip + dump_save + 40]
.endm

.macro DUMP reg
    mov [rip + dump_value], \reg
    DUMP_WRITE_VALUE
.endm

.macro DUMP_FLAGS mask
    mov [rip + dump_flags_rax], rax
    pushfq
    pop rax
    and rax, \mask
    mov [rip + dump_value], rax
    mov rax, [rip + dump_flags_rax]
    DUMP_WRITE_VALUE
.endm

.macro DUMPX xmm
    movdqu [rip + dump_xmm], \xmm
    DUMP_WRITE dump_xmm, 16
.endm

.macro SETX xmm, lo, hi
    mov [rip + dump_flags_rax], rax
    mov rax, \lo
    mov [rip + dump_xmm], rax
    mov rax, \hi
    mov [rip + dump_xmm + 8], rax
    movdqu \xmm, [rip + dump_xmm]
    mov rax, [rip + dump_flags_rax]
.endm

.macro EXIT code
    mov eax, __NR_exit_group
    mov edi, \code
    syscall
.endm
