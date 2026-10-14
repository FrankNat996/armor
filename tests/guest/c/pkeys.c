/*
 * Memory protection keys, which Chromium probes at start (a Spotify build
 * did): pkey_alloc fails with EINVAL where the CPU has no PKU (armor's
 * CPUID model, like the hardware oracle), EINVAL for bad flags or rights;
 * pkey_free of a key never allocated is EINVAL; pkey_mprotect with key -1
 * is mprotect, with another key EINVAL. Called through syscall(2), since
 * not every libc wraps them.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EINVAL: return "EINVAL";
    case ENOSPC: return "ENOSPC";
    case ENOSYS: return "ENOSYS";
    default: return "other";
    }
}

static void show(const char *what, long result) {
    printf("%s: %ld %s\n", what, result < 0 ? -1L : result, name(result < 0 ? errno : 0));
}

int main(void) {
    show("pkey_alloc", syscall(SYS_pkey_alloc, 0, 0));
    show("pkey_alloc bad flags", syscall(SYS_pkey_alloc, 1, 0));
    show("pkey_alloc bad rights", syscall(SYS_pkey_alloc, 0, 8));
    show("pkey_free 1", syscall(SYS_pkey_free, 1));
    char *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    page[0] = 7;
    show("pkey_mprotect key -1", syscall(SYS_pkey_mprotect, page, 4096, PROT_READ, -1));
    printf("  still readable: %d\n", page[0]);
    show("pkey_mprotect key 1", syscall(SYS_pkey_mprotect, page, 4096, PROT_READ, 1));
    show("pkey_mprotect unaligned", syscall(SYS_pkey_mprotect, page + 1, 4096, PROT_READ, -1));
    return 0;
}
