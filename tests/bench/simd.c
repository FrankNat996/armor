/*
 * Benchmark (cargo xtask bench): SSE2 integer code. glibc's string
 * functions (memchr, memcmp, strlen, memcpy use SSE2) on 64 KiB buffers,
 * and a byte loop that gcc -O3 vectorizes with SSE2 (paddb, pxor, ...).
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define SIZE 65536
static unsigned char a[SIZE], b[SIZE], c[SIZE];
static char text[SIZE];

int main(void) {
    for (int i = 0; i < SIZE; i++) {
        a[i] = (unsigned char)(i * 7);
        b[i] = (unsigned char)(i * 13 + 1);
        text[i] = (char)('a' + i % 26);
    }
    text[SIZE - 1] = 0;
    uint64_t sum = 0;
    for (int round = 0; round < 4000; round++) {
        text[SIZE - 2 - round] = 0;
        sum += strlen(text);
        text[SIZE - 2 - round] = 'x';
        const unsigned char *hit = memchr(a, (round & 0xff) | 1, SIZE);
        sum += hit ? (uint64_t)(hit - a) : 0;
        sum += (uint64_t)(memcmp(a, b, SIZE) > 0);
        memcpy(c, a, SIZE);
        for (int i = 0; i < SIZE; i++)
            c[i] = (unsigned char)((c[i] ^ b[i]) + (unsigned char)round);
        sum += c[round];
    }
    printf("%llu\n", (unsigned long long)sum);
    return 0;
}
