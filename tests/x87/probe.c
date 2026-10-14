/* Raw x87 transcendental results for deterministic pseudo-random inputs:
 * one line per case, "op inputs... result", in hex. Built for x86-64 and
 * run on real hardware and under armor; the distance between the two, in
 * units in the last place, is in docs/tech/floating-point.md (ADR-0030). */
#include <stdio.h>
#include <string.h>
#include <stdint.h>
typedef long double ld;
static uint64_t s = 0x9e3779b97f4a7c15ULL;
static uint64_t next(void) { s ^= s << 13; s ^= s >> 7; s ^= s << 17; return s; }
static ld make(int emin, int emax, int sign) {
    unsigned char b[10];
    uint64_t m = next() | 1ULL << 63;
    int e = emin + (int)(next() % (uint64_t)(emax - emin + 1));
    uint16_t hi = (uint16_t)(e + 16383) | (sign < 0 ? 0x8000 : sign > 0 ? 0 : (next() & 1) << 15);
    memcpy(b, &m, 8); memcpy(b + 8, &hi, 2);
    ld x; memcpy(&x, b, 10); return x;
}
static void hex(ld x) { unsigned char b[10]; memcpy(b, &x, 10); printf(" %04x%016llx", *(uint16_t *)(b + 8), *(unsigned long long *)b); }
int main(void) {
    for (int i = 0; i < 20000; i++) {
        ld x = make(-40, -1, 0), y = make(-20, 20, 0), xl = make(-2000, 2000, 1), xp = make(-60, -3, 0);
        ld ay = make(-60, 60, 0), ax = make(-60, 60, 0), t = make(-20, 20, 0), big = make(40, 62, 0), r;
        r = x; __asm__ volatile("f2xm1" : "+t"(r)); printf("f2xm1"); hex(x); hex(r); printf("\n");
        __asm__ volatile("fyl2x" : "=t"(r) : "0"(xl), "u"(y) : "st(1)"); printf("fyl2x"); hex(y); hex(xl); hex(r); printf("\n");
        __asm__ volatile("fyl2xp1" : "=t"(r) : "0"(xp), "u"(y) : "st(1)"); printf("fyl2xp1"); hex(y); hex(xp); hex(r); printf("\n");
        __asm__ volatile("fpatan" : "=t"(r) : "0"(ax), "u"(ay) : "st(1)"); printf("fpatan"); hex(ay); hex(ax); hex(r); printf("\n");
        r = t; __asm__ volatile("fsin" : "+t"(r)); printf("fsin"); hex(t); hex(r); printf("\n");
        r = t; __asm__ volatile("fcos" : "+t"(r)); printf("fcos"); hex(t); hex(r); printf("\n");
        { ld one; __asm__ volatile("fptan" : "=t"(one), "=u"(r) : "0"(t)); printf("fptan"); hex(t); hex(r); printf("\n"); }
        r = big; __asm__ volatile("fsin" : "+t"(r)); printf("fsin-big"); hex(big); hex(r); printf("\n");
    }
    return 0;
}
