/*
 * clc, stc, cmc, cld, std, observed through pushfq.
 */
#include "dump.h"

#define FLAGS_CF_DF 0x401

    .text
    .globl _start
_start:
    stc
    DUMP_FLAGS FLAGS_CF_DF
    cmc
    DUMP_FLAGS FLAGS_CF_DF
    cmc
    std
    DUMP_FLAGS FLAGS_CF_DF
    clc
    DUMP_FLAGS FLAGS_CF_DF
    cld
    DUMP_FLAGS FLAGS_CF_DF
    EXIT 0

    .section .note.GNU-stack, "", @progbits
