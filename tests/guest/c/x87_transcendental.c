/*
 * The x87 transcendental instructions (ADR-0030): f2xm1, fyl2x, fyl2xp1,
 * fpatan, fsin, fcos, fsincos, fptan. armor rounds them correctly; Intel
 * hardware errs by up to about one unit in the last place of the 64-bit
 * significand, so the results are printed rounded to double, where the two
 * agree (docs/tech/floating-point.md); and the exception flags (IE, DE,
 * ZE, OE, UE, PE) and C2, which do not depend on the last bit. The same
 * source runs as an x86-64 and an i386 program.
 */
#include <stdio.h>

typedef long double ld;

static unsigned short status(void) {
    unsigned short sw;
    __asm__ volatile("fnstsw %0" : "=m"(sw));
    return sw;
}

static void clear(void) { __asm__ volatile("fnclex"); }

/* C2 only for the trigonometric instructions: f2xm1 leaves it undefined. */
static void show(const char *what, ld x, ld r, unsigned short sw) {
    printf("%-8s %-12.6Lg -> %.17g  flags %02x", what, x, (double)r, sw & 0x3d);
    if (what[1] != '2') {
        printf(" c2 %d", (sw >> 10) & 1);
    }
    printf("\n");
}

static ld unary(const char *name, ld x) {
    ld r = x;
    clear();
    if (name[1] == '2') {
        __asm__ volatile("f2xm1" : "+t"(r));
    } else if (name[1] == 's' && name[4] == 0) {
        __asm__ volatile("fsin" : "+t"(r));
    } else {
        __asm__ volatile("fcos" : "+t"(r));
    }
    show(name, x, r, status());
    return r;
}

static void binary(const char *name, ld y, ld x) {
    ld r;
    clear();
    if (name[3] == '2' && name[5] == 0) {
        __asm__ volatile("fyl2x" : "=t"(r) : "0"(x), "u"(y) : "st(1)");
    } else if (name[3] == '2') {
        __asm__ volatile("fyl2xp1" : "=t"(r) : "0"(x), "u"(y) : "st(1)");
    } else {
        __asm__ volatile("fpatan" : "=t"(r) : "0"(x), "u"(y) : "st(1)");
    }
    unsigned short sw = status();
    printf("%-8s %-12.6Lg %-12.6Lg -> %.17g  flags %02x\n", name, y, x, (double)r, sw & 0x3d);
}

static void tan_and_sincos(ld x) {
    ld t, one, s, c;
    clear();
    __asm__ volatile("fptan" : "=t"(one), "=u"(t) : "0"(x));
    show("fptan", x, t, status());
    printf("         pushed %g\n", (double)one);
    clear();
    __asm__ volatile("fsincos" : "=t"(c), "=u"(s) : "0"(x));
    unsigned short sw = status();
    printf("fsincos  %-12.6Lg -> %.17g %.17g  flags %02x\n", x, (double)s, (double)c, sw & 0x3d);
}

int main(void) {
    const ld small[] = {0.5L, -0.5L, 0.25L, -0.999L, 0.001L, 1e-10L, -3e-20L, 0.75L};
    for (unsigned i = 0; i < sizeof small / sizeof *small; i++) {
        unary("f2xm1", small[i]);
    }
    /* Not near multiples of pi, nor large: there the hardware's 66-bit pi
     * shows (docs/tech/floating-point.md). */
    const ld angles[] = {0.5L, -1.0L, 2.0L, 10.0L, -100.5L, 1000.25L, 1e-5L};
    for (unsigned i = 0; i < sizeof angles / sizeof *angles; i++) {
        unary("fsin", angles[i]);
        unary("fcos", angles[i]);
        tan_and_sincos(angles[i]);
    }
    const ld ys[] = {1.0L, -2.5L, 0.75L};
    const ld xs[] = {3.0L, 0.1L, 1e10L, 1.5L, 1e-300L};
    for (unsigned i = 0; i < 3; i++) {
        for (unsigned j = 0; j < 5; j++) {
            binary("fyl2x", ys[i], xs[j]);
            binary("fpatan", ys[i], xs[j]);
            binary("fpatan", ys[i], -xs[j]);
        }
        binary("fyl2xp1", ys[i], 0.125L);
        binary("fyl2xp1", ys[i], -0.2L);
        binary("fyl2xp1", ys[i], 1e-15L);
    }
    /* Exact and special cases: flags tell them apart. */
    unary("f2xm1", 1.0L);
    unary("f2xm1", -1.0L);
    unary("f2xm1", 0.0L);
    unary("fsin", 0.0L);
    unary("fcos", 0.0L);
    unary("fsin", 1e19L); /* out of range: C2, unchanged */
    unary("fsin", 1.0L / 0.0L);
    binary("fyl2x", 3.0L, 8.0L);
    binary("fyl2x", 3.0L, 0.0L);
    binary("fyl2x", 1.0L, -1.0L);
    binary("fyl2x", -1.0L, 1.0L);
    binary("fpatan", 0.0L, -1.0L);
    binary("fpatan", -0.0L, 1.0L);
    binary("fpatan", 1.0L / 0.0L, -1.0L / 0.0L);
    binary("fyl2xp1", 2.0L, 1.0L);
    binary("fyl2xp1", 1.0L, 0.0L);
    /* A tiny result: UE with PE. */
    unary("fsin", 1e-4940L);
    unary("f2xm1", -1e-4940L);
    return 0;
}
