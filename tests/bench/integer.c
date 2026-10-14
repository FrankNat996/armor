/*
 * Benchmark (cargo xtask bench, docs/tech/performance.md): integer code.
 * A sieve, a 64-bit hash mix with multiplications and shifts, divisions,
 * and an insertion-sorted table: branches, loads and stores, flags.
 * Prints a checksum so that the result can be compared with qemu.
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define SIEVE 2000000
static unsigned char composite[SIEVE];

int main(void) {
    uint64_t sum = 0;
    for (int round = 0; round < 3; round++) {
        memset(composite, 0, sizeof composite);
        for (long i = 2; i * i < SIEVE; i++)
            if (!composite[i])
                for (long j = i * i; j < SIEVE; j += i)
                    composite[j] = 1;
        for (long i = 2; i < SIEVE; i++)
            if (!composite[i])
                sum += (uint64_t)i;
    }

    uint64_t h = 0x9e3779b97f4a7c15u;
    for (uint32_t i = 0; i < 20000000; i++) {
        h ^= i;
        h *= 0xbf58476d1ce4e5b9u;
        h ^= h >> 31;
    }

    uint64_t q = 0;
    for (uint64_t i = 1; i < 3000000; i++)
        q += (h ^ i) / i + (h ^ i) % (i | 1);

    static int32_t table[4096];
    int n = 0;
    uint32_t x = 12345;
    for (int i = 0; i < 4096; i++) {
        x = x * 1103515245u + 12345u;
        int32_t v = (int32_t)(x >> 8);
        int j = n++;
        while (j > 0 && table[j - 1] > v) {
            table[j] = table[j - 1];
            j--;
        }
        table[j] = v;
    }
    printf("%llu %llu %llu %d %d\n", (unsigned long long)sum, (unsigned long long)h,
           (unsigned long long)q, table[0], table[4095]);
    return 0;
}
