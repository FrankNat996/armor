/*
 * Helpers for i386 guest test programs (ADR-0029, docs/testing.md): the
 * 32-bit counterparts of dump.h, with syscalls through int 0x80.
 *
 * DUMP reg          writes the 4 bytes of a 32-bit register to stdout.
 *                   Preserves every register and the flags.
 * DUMP_FLAGS mask   writes EFLAGS & mask (4 bytes) to stdout.
 *                   Preserves every register but CLOBBERS the flags.
 * EXIT code         exit_group(code).
 *
 * The helpers use only mov, pushf, pop, and, int 0x80: the basic/ tests
 * check those first.
 */
#include <asm/unistd_32.h>

    .intel_syntax noprefix

/* Status flags: CF PF AF ZF SF OF. */
#define FLAGS_STATUS 0x8d5

    .pushsection .bss
    .balign 16
dump_save:
    .zero 16
dump_flags_eax:
    .zero 4
dump_value:
    .zero 4
    .popsection

/* write(1, &symbol, len), preserving every register. */
.macro DUMP_WRITE symbol, len
    mov [dump_save + 0], eax
    mov [dump_save + 4], ebx
    mov [dump_save + 8], ecx
    mov [dump_save + 12], edx
    mov eax, __NR_write
    mov ebx, 1
    mov ecx, offset \symbol
    mov edx, \len
    int 0x80
    mov eax, [dump_save + 0]
    mov ebx, [dump_save + 4]
    mov ecx, [dump_save + 8]
    mov edx, [dump_save + 12]
.endm

.macro DUMP reg
    mov [dump_value], \reg
    DUMP_WRITE dump_value, 4
.endm

.macro DUMP_FLAGS mask
    mov [dump_flags_eax], eax
    pushfd
    pop eax
    and eax, \mask
    mov [dump_value], eax
    mov eax, [dump_flags_eax]
    DUMP_WRITE dump_value, 4
.endm

.macro EXIT code
    mov eax, __NR_exit_group
    mov ebx, \code
    int 0x80
.endm
