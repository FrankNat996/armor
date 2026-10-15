/* Large reservations, as V8 makes them for each Chromium renderer: 32 GiB
   of PROT_NONE address space (with and without MAP_NORESERVE), then a
   small part made read-write and used. A native kernel does not charge
   PROT_NONE private memory to its overcommit accounting, so both succeed
   on a machine with less memory than that. */
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define RESERVE (32ULL << 30)

static int try_reserve(const char *what, int flags) {
    char *p = mmap(NULL, RESERVE, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | flags, -1, 0);
    if (p == MAP_FAILED) {
        printf("%s: failed\n", what);
        return 1;
    }
    /* The middle of the reservation becomes usable memory. */
    char *middle = p + RESERVE / 2;
    if (mprotect(middle, 1 << 20, PROT_READ | PROT_WRITE) != 0) {
        printf("%s: mprotect failed\n", what);
        return 1;
    }
    memset(middle, 0x5a, 1 << 20);
    unsigned long sum = 0;
    for (int i = 0; i < 1 << 20; i += 4096) {
        sum += (unsigned char)middle[i];
    }
    printf("%s: reserved, 1 MiB used, sum %lu\n", what, sum);
    if (munmap(p, RESERVE) != 0) {
        printf("%s: munmap failed\n", what);
        return 1;
    }
    return 0;
}

int main(void) {
    int failed = try_reserve("PROT_NONE with MAP_NORESERVE", MAP_NORESERVE);
    failed |= try_reserve("PROT_NONE", 0);
    /* Twice more, to see that the address space came back. */
    failed |= try_reserve("again", MAP_NORESERVE);
    failed |= try_reserve("and again", 0);
    return failed;
}
