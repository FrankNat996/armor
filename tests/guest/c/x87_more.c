/*
 * The x87 instructions added in M4: the constants (fldpi, fldl2e, fldl2t,
 * fldlg2, fldln2) in every rounding mode, fxam on every class and on an
 * empty register, frndint, fscale, fxtract, fprem and fprem1 (with
 * exponent differences beyond 64, where the reduction is partial),
 * fincstp/fdecstp, ffreep, fnstenv/fldenv, fnsave/frstor and fisttp. One line per
 * result, with the 10-byte values in hex. Status words are masked like in
 * c/x87: no DE (qemu never sets it) and no C1 after rounding (qemu does
 * not implement the round-up indicator); see docs/testing.md.
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef struct {
    uint64_t mant;
    uint16_t sexp;
} __attribute__((packed)) f80;

static f80 make(int sign, unsigned exp, uint64_t mant) {
    f80 v = {mant, (uint16_t)((sign ? 0x8000 : 0) | exp)};
    return v;
}

#define ONE_EXP 0x3fff
static f80 values[24];
static int nvalues;

static void add_value(f80 v) { values[nvalues++] = v; }

static void print80(const char *label, f80 v) {
    printf("%s %04x%016llx", label, v.sexp, (unsigned long long)v.mant);
}

static void fpu_init(unsigned rc) {
    uint16_t cw = (uint16_t)(0x037f | (rc << 10));
    __asm__ volatile("fninit\n\tfldcw %0" : : "m"(cw));
}

static uint16_t status(void) {
    uint16_t sw;
    __asm__ volatile("fnstsw %0" : "=m"(sw));
    return sw;
}

/* One operand on ST(0): run `insn`, print ST(0) and the status word
 * without DE and C1. */
#define UNARY(name, insn)                                                        \
    static void name(f80 a, unsigned rc) {                                       \
        f80 r;                                                                   \
        fpu_init(rc);                                                            \
        __asm__ volatile("fldt %1\n\t" insn "\n\tfstpt %0" : "=m"(r) : "m"(a));  \
        printf(#name " rc%u ", rc);                                              \
        print80("", a);                                                          \
        print80(" ->", r);                                                       \
        printf(" sw %04x\n", status() & 0x45fd);                                 \
    }
UNARY(frndint, "frndint")

static void fscale(f80 a, f80 b, unsigned rc) {
    f80 r;
    fpu_init(rc);
    __asm__ volatile("fldt %2\n\tfldt %1\n\tfscale\n\tfstpt %0\n\tfstp %%st(0)"
                     : "=m"(r)
                     : "m"(a), "m"(b));
    printf("fscale rc%u", rc);
    print80(" ", a);
    print80(" ", b);
    print80(" ->", r);
    printf(" sw %04x\n", status() & 0x45fd);
}

static void fxtract(f80 a) {
    f80 sig, exp;
    fpu_init(0);
    __asm__ volatile("fldt %2\n\tfxtract\n\tfstpt %0\n\tfstpt %1" : "=m"(sig), "=m"(exp) : "m"(a));
    print80("fxtract", a);
    print80(" ->", exp);
    print80("", sig);
    printf(" sw %04x\n", status() & 0x45fd);
}

/* fprem / fprem1 until the reduction completes (C2 clear), at most 400
 * steps; every step's ST(0) and C3 C2 C1 C0. */
static void fprem(f80 a, f80 b, int ieee) {
    f80 r;
    uint16_t sw;
    int steps = 0;
    fpu_init(0);
    __asm__ volatile("fldt %0\n\tfldt %1" : : "m"(b), "m"(a));
    printf("%s", ieee ? "fprem1" : "fprem");
    print80(" ", a);
    print80(" ", b);
    do {
        if (ieee)
            __asm__ volatile("fprem1");
        else
            __asm__ volatile("fprem");
        sw = status();
        __asm__ volatile("fld %%st(0)\n\tfstpt %0" : "=m"(r));
        print80(" |", r);
        /* fprem1's quotient bits: qemu always clears them, the SDM sets
         * them (docs/testing.md); only C2 here. */
        printf(" %04x", sw & (ieee ? 0x043d : 0x473d));
    } while ((sw & 0x400) && ++steps < 400);
    printf("\n");
    __asm__ volatile("fstp %st(0)\n\tfstp %st(0)");
}

static void fxam(f80 a, int empty) {
    uint16_t sw;
    fpu_init(0);
    if (empty)
        __asm__ volatile("fldt %0\n\tffree %%st(0)" : : "m"(a));
    else
        __asm__ volatile("fldt %0" : : "m"(a));
    __asm__ volatile("fxam\n\tfnstsw %0" : "=m"(sw));
    print80(empty ? "fxam empty" : "fxam", a);
    printf(" sw %04x\n", sw & 0x4700);
}

static void constants(unsigned rc) {
    f80 c[5];
    fpu_init(rc);
    __asm__ volatile("fldpi\n\tfstpt %0\n\tfldl2e\n\tfstpt %1\n\tfldl2t\n\tfstpt %2\n\t"
                     "fldlg2\n\tfstpt %3\n\tfldln2\n\tfstpt %4"
                     : "=m"(c[0]), "=m"(c[1]), "=m"(c[2]), "=m"(c[3]), "=m"(c[4]));
    printf("constants rc%u", rc);
    for (int i = 0; i < 5; i++)
        print80(" ", c[i]);
    printf(" sw %04x\n", status() & 0x45fd);
}

static void fisttp(f80 a) {
    int16_t w = 0x5555;
    int32_t d = 0x55555555;
    int64_t q = 0x5555555555555555;
    fpu_init(0);
    __asm__ volatile("fldt %3\n\tfisttps %0\n\tfldt %3\n\tfisttpl %1\n\tfldt %3\n\tfisttpll %2"
                     : "=m"(w), "=m"(d), "=m"(q)
                     : "m"(a));
    print80("fisttp", a);
    printf(" %04x %08x %016llx sw %04x\n", (uint16_t)w, (uint32_t)d, (unsigned long long)q,
           status() & 0x45fd);
}

/* The reserved halves of the first three words and everything from the
 * last-instruction pointer on (bytes 12..27): the hardware writes ffff in
 * the reserved fields and a FOP, qemu zeros, and the pointers are
 * addresses that differ between runs. Zeroed before printing; the full
 * format is checked against the hardware oracle (docs/testing.md). */
static void clear_pointers(unsigned char *env) {
    memset(env + 2, 0, 2);
    memset(env + 6, 0, 2);
    memset(env + 10, 0, 2);
    memset(env + 12, 0, 16);
}

static void environment(void) {
    unsigned char env[28], save[108];
    f80 two = make(0, ONE_EXP + 1, 1ull << 63);
    fpu_init(3);
    /* Registers of several classes: valid, zero, special. */
    __asm__ volatile("fldz\n\tfld1\n\tfldt %0\n\tfldpi\n\tfincstp\n\tfdecstp" : : "m"(two));
    __asm__ volatile("fnstenv %0" : "=m"(env));
    clear_pointers(env);
    printf("fnstenv");
    for (int i = 0; i < 28; i++)
        printf(" %02x", env[i]);
    printf("\n");
    /* fldenv: back, with the top of stack moved and a register emptied. */
    env[5] = (unsigned char)((env[5] & ~0x38) | (5 << 3));
    env[8] |= 3;
    __asm__ volatile("fldenv %0\n\tfnstenv %0" : "+m"(env));
    clear_pointers(env);
    printf("fldenv ");
    for (int i = 0; i < 28; i++)
        printf(" %02x", env[i]);
    printf("\n");
    fpu_init(1);
    __asm__ volatile("fld1\n\tfldt %0\n\tfldz\n\tfchs" : : "m"(two));
    __asm__ volatile("fnsave %0" : "=m"(save));
    clear_pointers(save);
    printf("fnsave ");
    for (int i = 0; i < 108; i++)
        printf(" %02x", save[i]);
    printf(" sw-after %04x\n", status());
    __asm__ volatile("frstor %0" : : "m"(save));
    __asm__ volatile("fnsave %0" : "=m"(save));
    clear_pointers(save);
    printf("frstor ");
    for (int i = 0; i < 108; i++)
        printf(" %02x", save[i]);
    printf("\n");
    /* ffreep st(1): frees ST(1), then pops ST(0) (two registers fewer). */
    fpu_init(0);
    __asm__ volatile("fld1\n\tfldz\n\tfldpi\n\t.byte 0xdf, 0xc1\n\tfnstenv %0" : "=m"(env));
    clear_pointers(env);
    printf("ffreep ");
    for (int i = 0; i < 28; i++)
        printf(" %02x", env[i]);
    printf("\n");
    /* fincstp / fdecstp move TOP and clear C1. */
    fpu_init(0);
    __asm__ volatile("fld1\n\tfincstp");
    printf("fincstp %04x", status());
    __asm__ volatile("fdecstp\n\tfdecstp");
    printf(" fdecstp %04x\n", status());
}

int main(void) {
    const uint64_t i = 1ull << 63;
    add_value(make(0, 0, 0));                         /* +0 */
    add_value(make(1, 0, 0));                         /* -0 */
    add_value(make(0, ONE_EXP, i));                   /* 1 */
    add_value(make(1, ONE_EXP + 1, i | (i >> 2)));    /* -2.5 */
    add_value(make(0, ONE_EXP + 1, i | (i >> 2)));    /* 2.5 */
    add_value(make(0, ONE_EXP - 1, i));               /* 0.5 */
    add_value(make(1, ONE_EXP - 1, i | (i >> 1)));    /* -0.75 */
    add_value(make(0, ONE_EXP + 62, i | 1));          /* 2^62 + 0.5ish */
    add_value(make(0, ONE_EXP + 63, i | 3));          /* integral, huge */
    add_value(make(0, ONE_EXP + 200, i | 12345));     /* big */
    add_value(make(1, ONE_EXP - 200, i | 777));       /* small */
    add_value(make(0, 0, 0x0000000012345678));        /* denormal */
    add_value(make(0, 0x7fff, i));                    /* +inf */
    add_value(make(1, 0x7fff, i));                    /* -inf */
    add_value(make(0, 0x7fff, i | (i >> 1) | 5));     /* QNaN */
    add_value(make(1, 0x7fff, i | 9));                /* SNaN */
    add_value(make(0, ONE_EXP, 0x4000000000000000));  /* unnormal */
    add_value(make(0, ONE_EXP + 3, i | (1ull << 60))); /* 9 */

    for (unsigned rc = 0; rc < 4; rc++)
        constants(rc);
    for (int k = 0; k < nvalues; k++) {
        fxam(values[k], 0);
        fxtract(values[k]);
        fisttp(values[k]);
        for (unsigned rc = 0; rc < 4; rc++)
            frndint(values[k], rc);
    }
    fxam(values[2], 1);
    fxam(values[3], 1);

    f80 scales[] = {make(0, 0, 0), make(1, ONE_EXP + 1, i | (i >> 2)), make(0, ONE_EXP + 3, i),
                    make(0, ONE_EXP + 14, i), make(1, ONE_EXP + 14, i), make(0, ONE_EXP + 40, i),
                    make(0, 0x7fff, i), make(1, 0x7fff, i), make(0, 0x7fff, i | (i >> 1))};
    for (int k = 0; k < nvalues; k++)
        for (unsigned s = 0; s < sizeof scales / sizeof scales[0]; s++) {
            /* Two NaNs: qemu returns ST(1)'s, the SDM's x87 rule the one
             * with the larger significand (docs/testing.md). */
            int nan = (values[k].sexp & 0x7fff) == 0x7fff && values[k].mant << 1 != 0;
            if (nan && s == sizeof scales / sizeof scales[0] - 1)
                continue;
            fscale(values[k], scales[s], (k + s) & 3);
        }

    f80 divisors[] = {make(0, ONE_EXP + 1, i | (i >> 1)), make(1, ONE_EXP - 3, i | 0x123456),
                      make(0, 0, 0), make(0, 0x7fff, i), make(0, ONE_EXP + 70, i | 99)};
    for (int k = 0; k < nvalues; k++)
        for (unsigned d = 0; d < sizeof divisors / sizeof divisors[0]; d++) {
            fprem(values[k], divisors[d], 0);
            fprem(values[k], divisors[d], 1);
        }
    /* Exponent differences of 64 and far beyond: partial reductions. */
    fprem(make(0, ONE_EXP + 64, i | 0xabcdef), make(0, ONE_EXP, i | 1), 0);
    fprem(make(0, ONE_EXP + 64, i | 0xabcdef), make(0, ONE_EXP, i | 1), 1);
    fprem(make(1, ONE_EXP + 1000, i | 0x13579bdf), make(0, ONE_EXP - 2, i | 0x2468), 0);
    fprem(make(0, ONE_EXP + 300, i | 0x13579bdf), make(1, ONE_EXP + 3, i | 0x2468), 1);

    environment();
    return 0;
}
