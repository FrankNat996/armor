/*
 * SSE4.2 and POPCNT: pcmpgtq; crc32 with 8-, 16-, 32- and 64-bit sources
 * (registers and memory, 32- and 64-bit destinations); popcnt (16, 32,
 * 64 bits, register and memory, flags); pcmpestri/pcmpestrm/pcmpistri/
 * pcmpistrm with every combination of element format, aggregation and
 * polarity (imm bits 0-5), both index and mask selections (bit 6), on
 * strings with and without nulls and explicit lengths that are short,
 * negative and past the end. Dumped: ecx or xmm0, and the flags.
 */
#include "dump.h"

#define A0 0x80ff7f0100fe0281
#define A1 0x7fff800000017ffe
#define B0 0x0102030485868788
#define B1 0xff00807f0f8e1d2c

.macro P op, a0, a1, b0, b1
    SETX xmm0, \a0, \a1
    SETX xmm1, \b0, \b1
    \op xmm0, xmm1
    DUMPX xmm0
.endm

/* pcmpXstrY xmm1, xmm2, imm for every imm in 0..0x7f, with xmm1 = a,
 * xmm2 = b, eax = la, edx = lb; dumps rcx or xmm0, and the flags. */
.macro STR op, a0, a1, b0, b1, la, lb, index
    .irp hi, 0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70
    .irp lo, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
    SETX xmm1, \a0, \a1
    SETX xmm2, \b0, \b1
    mov rax, \la
    mov rdx, \lb
    mov rcx, -1
    \op xmm1, xmm2, \hi + \lo
    .if \index
    DUMP rcx
    .else
    DUMPX xmm0
    .endif
    DUMP_FLAGS FLAGS_STATUS
    .endr
    .endr
.endm

/* "hello, world" and two needles, as little-endian quadwords. */
#define HELLO0  0x77202c6f6c6c6568
#define HELLO1  0x00000000646c726f
#define SET0    0x00000000002c6f77  /* "wo," */
#define RANGE0  0x000000005a417a61  /* "azAZ" */
#define WORDS0  0x0003fffe7fff8000
#define WORDS1  0x0000000100020005

    .data
    .balign 16
mem:
    .quad 0x0123456789abcdef, 0xfedcba9876543210
unaligned_text:
    .byte 0
    .ascii "hello, world"
    .byte 0, 0, 0, 0

    .text
    .globl _start
_start:
    P pcmpgtq, A0, A1, B0, B1
    P pcmpgtq, B0, B1, A0, A1
    P pcmpgtq, A0, A1, A0, A1
    P pcmpgtq, -1, 0x7fffffffffffffff, 0, 0x8000000000000000

    /* crc32. */
    mov rax, 0xffffffff
    mov rbx, 0x0123456789abcdef
    crc32 eax, bl
    DUMP rax
    crc32 eax, bx
    DUMP rax
    crc32 eax, ebx
    DUMP rax
    crc32 rax, rbx
    DUMP rax
    mov rax, -1
    crc32 rax, bl
    DUMP rax
    crc32 eax, byte ptr [rip + mem + 3]
    DUMP rax
    crc32 eax, word ptr [rip + mem + 1]
    DUMP rax
    crc32 eax, dword ptr [rip + mem + 5]
    DUMP rax
    crc32 rax, qword ptr [rip + mem + 7]
    DUMP rax

    /* popcnt. */
    .irp value, 0, 1, -1, 0x8000000000000000, 0x0123456789abcdef
    mov rbx, \value
    mov rax, -1
    popcnt rax, rbx
    DUMP rax
    DUMP_FLAGS FLAGS_STATUS
    mov rax, -1
    popcnt eax, ebx
    DUMP rax
    DUMP_FLAGS FLAGS_STATUS
    mov rax, -1
    popcnt ax, bx
    DUMP rax
    DUMP_FLAGS FLAGS_STATUS
    .endr
    mov rax, -1
    popcnt rax, qword ptr [rip + mem]
    DUMP rax
    popcnt ax, word ptr [rip + mem + 3]
    DUMP rax

    /* String compares. */
    STR pcmpistri, SET0, 0, HELLO0, HELLO1, 0, 0, 1
    STR pcmpistrm, RANGE0, 0, HELLO0, HELLO1, 0, 0, 0
    STR pcmpestri, HELLO0, HELLO1, SET0, 0, 12, 3, 1
    STR pcmpestrm, WORDS0, WORDS1, A0, A1, 5, -7, 0
    STR pcmpestri, A0, A1, B0, B1, 100, 0x80000000, 1
    STR pcmpistri, A0, A1, WORDS0, WORDS1, 0, 0, 1
    /* Empty operands: a needle of length 0, then a text of length 0. */
    STR pcmpestri, HELLO0, HELLO1, HELLO0, HELLO1, 0, 5, 1
    STR pcmpestrm, HELLO0, HELLO1, HELLO0, HELLO1, 4, 0, 0

    /* Memory operand, unaligned. pcmpestri with REX.W (64-bit lengths)
     * is in sse/sse42_hardware: qemu reads only eax and edx. */
    SETX xmm1, SET0, 0
    pcmpistri xmm1, [rip + unaligned_text + 1], 0x00
    DUMP rcx
    DUMP_FLAGS FLAGS_STATUS
    mov rax, 0x100000003
    mov rdx, -0x100000000
    pcmpestri xmm1, [rip + unaligned_text + 1], 0x00
    DUMP rcx
    DUMP_FLAGS FLAGS_STATUS

    EXIT 0
