/*
 * i386 TLS (ADR-0029), as glibc sets it up: set_thread_area with entry
 * -1 picks a free GDT entry (12 on Linux) and writes its number back; gs
 * then holds its selector and gs-relative accesses reach the base, also
 * with negative offsets (base + 0xfffffffc wraps at 4 GiB).
 * get_thread_area reads the entry back. mov eax, gs reads the selector.
 */
#include "dump32.h"

    .data
    .balign 16
block:
    .long 0x11111111, 0x22222222, 0x33333333, 0x44444444
desc:
    .long -1                    /* entry_number: any free */
    .long block + 8             /* base_addr */
    .long 0xfffff               /* limit, in pages */
    .long 0x51                  /* seg_32bit, limit_in_pages, useable */
readback:
    .long 0, 0, 0, 0

    .text
    .globl _start
_start:
    mov eax, __NR_set_thread_area
    mov ebx, offset desc
    int 0x80
    DUMP eax                    /* 0 */
    mov eax, [desc]
    DUMP eax                    /* 12 */

    /* gs = (entry << 3) | 3 */
    mov eax, [desc]
    shl eax, 3
    or eax, 3
    mov gs, ax
    xor ecx, ecx
    mov ecx, gs
    DUMP ecx                    /* 0x63 */
    mov eax, gs:[0]
    DUMP eax                    /* 0x33333333 */
    mov eax, gs:[4]
    DUMP eax                    /* 0x44444444 */
    mov eax, gs:[-8]
    DUMP eax                    /* 0x11111111 */
    mov dword ptr gs:[-4], 0x55555555
    mov eax, [block + 4]
    DUMP eax                    /* 0x55555555 */

    /* get_thread_area. */
    mov eax, [desc]
    mov [readback], eax
    mov eax, __NR_get_thread_area
    mov ebx, offset readback
    int 0x80
    DUMP eax                    /* 0 */
    mov eax, [readback + 4]
    sub eax, offset block
    DUMP eax                    /* 8 */
    mov eax, [readback + 8]
    DUMP eax                    /* 0xfffff */
    mov eax, [readback + 12]
    and eax, 0x7f
    DUMP eax                    /* 0x51 */

    /* An entry out of range: EINVAL. */
    mov dword ptr [desc], 20
    mov eax, __NR_set_thread_area
    mov ebx, offset desc
    int 0x80
    DUMP eax                    /* -22 */

    EXIT 0

    .section .note.GNU-stack, "", @progbits
