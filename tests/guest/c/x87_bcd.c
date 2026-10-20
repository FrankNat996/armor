/*
 * x87 packed BCD: fbstp of integers, fractions (in every rounding mode),
 * zeros of both signs, the 18-digit limits and values past them, infinities
 * and NaNs (the BCD indefinite); fbld of digits, both signs and zero, and
 * back through fstpt. One line per result, with the 10 bytes in hex and the
 * status word masked like in c/x87 (no DE, no C1: qemu does not implement
 * the round-up indicator; docs/testing.md).
 */
#include <stdint.h>
#include <stdio.h>

typedef struct {
    uint64_t mant;
    uint16_t sexp;
} __attribute__((packed)) f80;

static void fpu_init(unsigned rc) {
    uint16_t cw = (uint16_t)(0x037f | (rc << 10));
    __asm__ volatile("fninit\n\tfldcw %0" : : "m"(cw));
}

static uint16_t status(void) {
    uint16_t sw;
    __asm__ volatile("fnstsw %0" : "=m"(sw));
    return sw;
}

static void print_bytes(const unsigned char *b) {
    for (int i = 9; i >= 0; i--)
        printf("%02x", b[i]);
}

static void store(const char *label, f80 a, unsigned rc) {
    unsigned char bcd[10];
    fpu_init(rc);
    __asm__ volatile("fldt %1\n\tfbstp %0" : "=m"(bcd) : "m"(a));
    printf("fbstp %s rc%u ", label, rc);
    print_bytes(bcd);
    printf(" sw %04x\n", status() & 0x45fd);
}

static void load(const unsigned char *bcd) {
    f80 r;
    fpu_init(0);
    __asm__ volatile("fbld %1\n\tfstpt %0" : "=m"(r) : "m"(*(const unsigned char(*)[10])bcd));
    printf("fbld ");
    print_bytes(bcd);
    printf(" -> %04x%016llx sw %04x\n", r.sexp, (unsigned long long)r.mant, status() & 0x45fd);
}

static f80 from_long_double(long double x) {
    f80 v;
    __builtin_memcpy(&v, &x, sizeof v);
    return v;
}

int main(void) {
    static const struct {
        const char *label;
        long double value;
    } values[] = {
        {"0", 0.0L},
        {"-0", -0.0L},
        {"1", 1.0L},
        {"-1", -1.0L},
        {"0.3", 0.3L},
        {"-0.3", -0.3L},
        {"0.5", 0.5L},
        {"1.5", 1.5L},
        {"2.5", 2.5L},
        {"-2.5", -2.5L},
        {"-2.7", -2.7L},
        {"1234567.89", 1234567.89L},
        {"-987654321012", -987654321012.0L},
        {"10^18-1", 999999999999999999.0L},
        {"-(10^18-1)", -999999999999999999.0L},
        {"10^18-0.5", 999999999999999999.5L},
        {"10^18", 1000000000000000000.0L},
        {"-10^18", -1000000000000000000.0L},
        {"2^63", 9223372036854775808.0L},
        {"1e30", 1e30L},
    };
    for (unsigned i = 0; i < sizeof values / sizeof values[0]; i++)
        for (unsigned rc = 0; rc < 4; rc++)
            store(values[i].label, from_long_double(values[i].value), rc);
    f80 inf = {0x8000000000000000ull, 0x7fff}, ninf = {0x8000000000000000ull, 0xffff};
    f80 qnan = {0xc000000000000000ull, 0x7fff}, snan = {0xa000000000000000ull, 0x7fff};
    store("inf", inf, 0);
    store("-inf", ninf, 0);
    store("qnan", qnan, 0);
    store("snan", snan, 0);

    static const unsigned char bcds[][10] = {
        {0, 0, 0, 0, 0, 0, 0, 0, 0, 0},
        {0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80},
        {0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0},
        {0x89, 0x67, 0x45, 0x23, 0x01, 0, 0, 0, 0, 0x80},
        {0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0},
        {0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x80},
        {0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0x76, 0},
    };
    for (unsigned i = 0; i < sizeof bcds / sizeof bcds[0]; i++)
        load(bcds[i]);
    return 0;
}
