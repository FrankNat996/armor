/*
 * BMI1, BMI2, LZCNT and MOVBE as the compiler emits them for ordinary C
 * (functions built with `target("bmi,bmi2,lzcnt,movbe")`, as with
 * `-march=x86-64-v3`): shifts by a variable count (shlx, shrx, sarx),
 * `~a & b` (andn), the lowest set bit (blsi, blsr, blsmsk), bit counts
 * (tzcnt, lzcnt), masks (bzhi), wide multiplication (mulx), byte-swapped
 * loads (movbe), and the intrinsics for bextr, pdep and pext. Run with
 * those features declared (difftest's CASE_FEATURES; docs/tech/avx.md).
 */
#include <stdint.h>
#include <stdio.h>
#include <x86intrin.h>

#define BMI __attribute__((target("bmi,bmi2,lzcnt,movbe"), noinline))

static volatile uint64_t inputs[] = {
    0, 1, 2, 3, 0x80, 0xff, 0x8000, 0x12345678, 0x80000000, 0xffffffff,
    0x100000000ull, 0x0123456789abcdefull, 0x8000000000000000ull, ~0ull,
};
#define N (sizeof inputs / sizeof inputs[0])

BMI static uint64_t shifts(uint64_t x, unsigned n) {
    int64_t s = (int64_t)x;
    uint32_t w = (uint32_t)x;
    return (x << (n & 63)) ^ (x >> (n & 63)) ^ (uint64_t)(s >> (n & 63)) ^ (w << (n & 31)) ^
           ((uint64_t)(w >> (n & 31)) << 7);
}

BMI static uint64_t lowest_bits(uint64_t x, uint64_t y) {
    return (~x & y) ^ (x & -x) ^ ((x & (x - 1)) << 1) ^ ((x ^ (x - 1)) >> 1) ^ (uint32_t)(~(uint32_t)x & (uint32_t)y);
}

BMI static unsigned counts(uint64_t x) {
    unsigned tz = x ? (unsigned)__builtin_ctzll(x) : 64;
    unsigned lz = x ? (unsigned)__builtin_clzll(x) : 64;
    unsigned tz32 = (uint32_t)x ? (unsigned)__builtin_ctz((uint32_t)x) : 32;
    return tz | lz << 8 | tz32 << 16 | (unsigned)_lzcnt_u32((uint32_t)x) << 24;
}

BMI static uint64_t masks(uint64_t x, unsigned n) {
    return _bzhi_u64(x, n) ^ (uint64_t)_bzhi_u32((uint32_t)x, n) << 3 ^
           _bextr_u64(x, n & 63, 13) ^ _pdep_u64(x, 0x00ff00f00f0f00ffull) ^ _pext_u64(x, 0xf0f0f0f00ff0ff00ull) ^
           (uint64_t)_pext_u32((uint32_t)x, 0x0f0ff0f0u) << 32;
}

BMI static uint64_t wide(uint64_t x, uint64_t y) {
    unsigned __int128 p = (unsigned __int128)x * y;
    unsigned long long high;
    uint64_t low = _mulx_u64(x, y, &high);
    return (uint64_t)(p >> 64) ^ low ^ high << 1;
}

BMI static uint64_t swapped(const volatile uint64_t *p) {
    return __builtin_bswap64(*p) ^ __builtin_bswap32(*(const volatile uint32_t *)p);
}

int main(void) {
    for (unsigned i = 0; i < N; i++) {
        uint64_t x = inputs[i], y = inputs[(i * 5 + 3) % N];
        printf("%2u shifts", i);
        for (unsigned n = 0; n < 70; n += 7)
            printf(" %llx", (unsigned long long)shifts(x, n));
        printf("\n   bits %llx counts %08x wide %llx swapped %llx\n   masks",
               (unsigned long long)lowest_bits(x, y), counts(x), (unsigned long long)wide(x, y),
               (unsigned long long)swapped(&inputs[i]));
        for (unsigned n = 0; n < 72; n += 9)
            printf(" %llx", (unsigned long long)masks(x, n));
        printf("\n");
    }
    return 0;
}
