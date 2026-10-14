/*
 * Benchmark (cargo xtask bench): memory management. malloc and free of
 * many sizes, large memset and memcpy, and mmap/munmap with every page
 * touched (new pages, page-table updates).
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

int main(void) {
    uint64_t sum = 0;
    static void *slots[1024];
    uint32_t x = 1;
    for (int i = 0; i < 400000; i++) {
        x = x * 1103515245u + 12345u;
        int s = (int)(x >> 22);
        free(slots[s]);
        size_t size = 16 + (x >> 8) % 2000;
        slots[s] = malloc(size);
        ((unsigned char *)slots[s])[0] = (unsigned char)i;
        sum += size;
    }
    for (int i = 0; i < 1024; i++)
        free(slots[i]);

    size_t big = 8 << 20;
    unsigned char *p = malloc(big), *q = malloc(big);
    for (int round = 0; round < 20; round++) {
        memset(p, round, big);
        memcpy(q, p, big);
        sum += q[round * 4096];
    }
    free(p);
    free(q);

    for (int round = 0; round < 300; round++) {
        size_t len = 1 << 20;
        unsigned char *m = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (m == MAP_FAILED)
            return 1;
        for (size_t off = 0; off < len; off += 4096)
            m[off] = (unsigned char)off;
        sum += m[4096];
        munmap(m, len);
    }
    printf("%llu\n", (unsigned long long)sum);
    return 0;
}
