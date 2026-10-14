/*
 * Differential test of the x87 FPU (ADR-0016): deterministic pseudo-random
 * 80-bit operands plus edge cases (zeros, infinities, quiet and signalling
 * NaNs, denormals, pseudo-denormals, unnormals and other non-canonical
 * encodings), every operation in every rounding mode. Each result is
 * written as raw bytes: the 10-byte value (or the integer, or the float)
 * followed by the defined bits of the status word; compares write the
 * status word or the flags as their value. Run with `-v` for one readable line per result.
 *
 * Every operation starts from fninit, so results do not depend on each
 * other. The control word has all exceptions masked, the rounding control
 * under test, and 64-bit precision, or 24 and 53 bits for the arithmetic.
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef struct {
    uint64_t mant;
    uint16_t sexp;
} __attribute__((packed)) f80;

static int verbose;
static unsigned char out[64];
static size_t out_len;

static void emit(const char *what, unsigned rc, const void *bytes, size_t n, uint16_t sw) {
    out_len = 0;
    memcpy(out, bytes, n);
    out_len = n;
    /*
     * Only the exception flags and SF: the condition codes are undefined
     * after arithmetic (C0, C2, C3) or not implemented by qemu (C1, the
     * round-up indicator); compares report theirs as the value instead.
     * Nor DE (denormal operand), which qemu never sets (docs/testing.md).
     */
    sw &= 0x007d;
    memcpy(out + out_len, &sw, 2);
    out_len += 2;
    if (verbose) {
        printf("%-8s rc%u ", what, rc);
        for (size_t i = n; i-- > 0;)
            printf("%02x", ((const unsigned char *)bytes)[i]);
        printf(" sw %04x\n", sw);
    } else {
        fwrite(out, 1, out_len, stdout);
    }
}

static uint64_t state = 0x9e3779b97f4a7c15ull;
static uint64_t next(void) {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    return state;
}

static f80 make(int sign, unsigned exp, uint64_t mant) {
    f80 v = {mant, (uint16_t)((sign ? 0x8000 : 0) | exp)};
    return v;
}

/* An operand: mostly normal numbers near 1, some anywhere, and edge cases. */
static f80 operand(void) {
    uint64_t r = next();
    int sign = (int)(r & 1);
    uint64_t m = next();
    switch ((r >> 1) % 16) {
    case 0: return make(sign, 0, 0);                                /* zero */
    case 1: return make(sign, 0x7fff, 0x8000000000000000ull);       /* infinity */
    case 2: return make(sign, 0x7fff, 0xc000000000000000ull | (m >> 2)); /* QNaN */
    case 3: return make(sign, 0x7fff, 0x8000000000000000ull | ((m >> 2) | 1)); /* SNaN */
    case 4: return make(sign, 0, m >> (1 + (r >> 8) % 63));         /* denormal */
    case 5: return make(sign, (unsigned)((r >> 8) % 64), m | 0x8000000000000000ull); /* tiny */
    case 6: return make(sign, 0x7fff - 1 - (unsigned)((r >> 8) % 64), m | 0x8000000000000000ull); /* huge */
    case 7: {
        /* Non-canonical: pseudo-denormal, unnormal, pseudo-infinity/NaN. */
        switch ((r >> 8) % 3) {
        case 0: return make(sign, 0, m | 0x8000000000000000ull);
        case 1: return make(sign, 1 + (unsigned)((r >> 10) % 0x7ffe), m >> 1);
        default: return make(sign, 0x7fff, m >> 1);
        }
    }
    case 8: return make(sign, 0x3fff + (unsigned)((r >> 8) % 64), (m | 0x8000000000000000ull) & ~0xffffffffffull); /* integer-ish */
    case 9: return make(sign, (unsigned)((r >> 8) % 0x7fff), m | 0x8000000000000000ull); /* any normal */
    default: return make(sign, 0x3fff - 20 + (unsigned)((r >> 8) % 41), m | 0x8000000000000000ull);
    }
}

/* Rounding control `rc`, 64-bit precision unless `precision_control`
   (bits 8-9: 0 = 24 bits, 2 = 53 bits) is given through cw_pc. */
static unsigned precision_control = 3;
static uint16_t cw_for(unsigned rc) {
    return (uint16_t)(0x007f | (precision_control << 8) | (rc << 10));
}

#define BINARY(name, insn)                                                       \
    static void name(f80 a, f80 b, unsigned rc) {                                \
        f80 r;                                                                   \
        uint16_t sw, cw = cw_for(rc);                                            \
        __asm__ volatile("fninit\n\tfldcw %4\n\tfldt %3\n\tfldt %2\n\t" insn     \
                         "\n\tfstpt %0\n\tfnstsw %1"                            \
                         : "=m"(r), "=m"(sw)                                     \
                         : "m"(a), "m"(b), "m"(cw)                               \
                         : "memory", "st", "st(1)");                             \
        __asm__ volatile("" ::: "memory");                                       \
        emit(#name, rc, &r, 10, sw);                                             \
    }

/*
 * st(0) = a, st(1) = b. Names are the Intel mnemonics (checked with
 * objdump -M intel); AT&T swaps fsubp/fsubrp and fdivp/fdivrp.
 */
BINARY(faddp, "faddp %%st, %%st(1)")    /* st(1) = b + a */
BINARY(fsubp, "fsubrp %%st, %%st(1)")   /* st(1) = b - a */
BINARY(fsubrp, "fsubp %%st, %%st(1)")   /* st(1) = a - b */
BINARY(fmulp, "fmulp %%st, %%st(1)")    /* st(1) = b * a */
BINARY(fdivp, "fdivrp %%st, %%st(1)")   /* st(1) = b / a */
BINARY(fdivrp, "fdivp %%st, %%st(1)")   /* st(1) = a / b */
BINARY(fadd_st, "fadd %%st(1), %%st\n\tfstp %%st(1)") /* st(0) = a + b */
BINARY(fsub_st, "fsub %%st(1), %%st\n\tfstp %%st(1)") /* st(0) = a - b */
BINARY(fdiv_st, "fdiv %%st(1), %%st\n\tfstp %%st(1)") /* st(0) = a / b */

static void fsqrt_(f80 a, unsigned rc) {
    f80 r;
    uint16_t sw, cw = cw_for(rc);
    __asm__ volatile("fninit\n\tfldcw %3\n\tfldt %2\n\tfsqrt\n\tfstpt %0\n\tfnstsw %1"
                     : "=m"(r), "=m"(sw) : "m"(a), "m"(cw) : "memory", "st");
    __asm__ volatile("" ::: "memory");
    emit("fsqrt", rc, &r, 10, sw);
}

static void compare(f80 a, f80 b) {
    uint16_t sw1, sw2;
    uint64_t flags1, flags2;
    __asm__ volatile("fninit\n\tfldt %2\n\tfldt %1\n\tfcomp %%st(1)\n\tfnstsw %0\n\tfstp %%st(0)"
                     : "=m"(sw1) : "m"(a), "m"(b) : "memory", "st");
    __asm__ volatile("fninit\n\tfldt %2\n\tfldt %1\n\tfucomp %%st(1)\n\tfnstsw %0\n\tfstp %%st(0)"
                     : "=m"(sw2) : "m"(a), "m"(b) : "memory", "st");
    /* C3, C2, C0, the exception flags but DE, and SF; C1 is undefined. */
    sw1 &= 0x457d;
    sw2 &= 0x457d;
    emit("fcom", 0, &sw1, 2, 0);
    emit("fucom", 0, &sw2, 2, 0);
    __asm__ volatile("fninit\n\tfldt %2\n\tfldt %1\n\tfcomip %%st(1), %%st\n\tpushfq\n\tpop %%rax\n\tmov %%rax, %0\n\tfstp %%st(0)"
                     : "=m"(flags1) : "m"(a), "m"(b) : "rax", "memory", "st", "cc");
    __asm__ volatile("fninit\n\tfldt %2\n\tfldt %1\n\tfucomip %%st(1), %%st\n\tpushfq\n\tpop %%rax\n\tmov %%rax, %0\n\tfstp %%st(0)"
                     : "=m"(flags2) : "m"(a), "m"(b) : "rax", "memory", "st", "cc");
    flags1 &= 0x8d5;
    flags2 &= 0x8d5;
    emit("fcomi", 0, &flags1, 2, 0);
    emit("fucomi", 0, &flags2, 2, 0);
}

static void to_int(f80 a, unsigned rc) {
    int64_t q;
    int32_t d;
    int16_t w;
    uint16_t sw1, sw2, sw3, cw = cw_for(rc);
    __asm__ volatile("fninit\n\tfldcw %3\n\tfldt %2\n\tfistpll %0\n\tfnstsw %1"
                     : "=m"(q), "=m"(sw1) : "m"(a), "m"(cw) : "memory", "st");
    __asm__ volatile("fninit\n\tfldcw %3\n\tfldt %2\n\tfistpl %0\n\tfnstsw %1"
                     : "=m"(d), "=m"(sw2) : "m"(a), "m"(cw) : "memory", "st");
    __asm__ volatile("fninit\n\tfldcw %3\n\tfldt %2\n\tfistps %0\n\tfnstsw %1"
                     : "=m"(w), "=m"(sw3) : "m"(a), "m"(cw) : "memory", "st");
    __asm__ volatile("" ::: "memory");
    emit("fistp64", rc, &q, 8, sw1);
    emit("fistp32", rc, &d, 4, sw2);
    emit("fistp16", rc, &w, 2, sw3);
}

static void to_float(f80 a, unsigned rc) {
    double dbl;
    float flt;
    uint16_t sw1, sw2, cw = cw_for(rc);
    __asm__ volatile("fninit\n\tfldcw %3\n\tfldt %2\n\tfstpl %0\n\tfnstsw %1"
                     : "=m"(dbl), "=m"(sw1) : "m"(a), "m"(cw) : "memory", "st");
    __asm__ volatile("fninit\n\tfldcw %3\n\tfldt %2\n\tfstps %0\n\tfnstsw %1"
                     : "=m"(flt), "=m"(sw2) : "m"(a), "m"(cw) : "memory", "st");
    __asm__ volatile("" ::: "memory");
    emit("fstp64", rc, &dbl, 8, sw1);
    emit("fstp32", rc, &flt, 4, sw2);
}

static void from_memory(uint64_t bits) {
    f80 r1, r2, r3, r4;
    uint16_t sw1, sw2, sw3, sw4;
    uint32_t bits32 = (uint32_t)bits;
    int16_t w = (int16_t)bits;
    int32_t d = (int32_t)bits;
    __asm__ volatile("fninit\n\tfldl %2\n\tfstpt %0\n\tfnstsw %1"
                     : "=m"(r1), "=m"(sw1) : "m"(bits) : "memory", "st");
    __asm__ volatile("fninit\n\tflds %2\n\tfstpt %0\n\tfnstsw %1"
                     : "=m"(r2), "=m"(sw2) : "m"(bits32) : "memory", "st");
    __asm__ volatile("fninit\n\tfildll %2\n\tfstpt %0\n\tfnstsw %1"
                     : "=m"(r3), "=m"(sw3) : "m"(bits) : "memory", "st");
    __asm__ volatile("fninit\n\tfildl %2\n\tfilds %3\n\tfaddp\n\tfstpt %0\n\tfnstsw %1"
                     : "=m"(r4), "=m"(sw4) : "m"(d), "m"(w) : "memory", "st", "st(1)");
    __asm__ volatile("" ::: "memory");
    emit("fld64", 0, &r1, 10, sw1);
    emit("fld32", 0, &r2, 10, sw2);
    emit("fild64", 0, &r3, 10, sw3);
    emit("fild32+16", 0, &r4, 10, sw4);
}

/* A double or float bit pattern: random, or one of the edge cases. */
static uint64_t memory_operand(void) {
    uint64_t r = next();
    switch (r % 8) {
    case 0: return 0x7ff0000000000001ull | (next() & 0x0007ffffffffffffull); /* SNaN */
    case 1: return 0x000fffffffffffffull & next();                         /* denormal */
    case 2: return 0x7f800001u | (uint32_t)(next() & 0x003fffff);          /* float SNaN */
    case 3: return (uint32_t)next() & 0x007fffff;                          /* float denormal */
    default: return next();
    }
}

/*
 * Stack faults (an empty register read, a push on a full stack) are not
 * tested here: qemu does not implement them (docs/testing.md); armor's
 * behaviour, from the SDM, is unit-tested in armor-cpu.
 */

int main(int argc, char **argv) {
    verbose = argc > 1 && strcmp(argv[1], "-v") == 0;
    for (int i = 0; i < 400; i++) {
        f80 a = operand();
        f80 b;
        switch (next() % 8) {
        case 0: b = a; break;                                            /* equal */
        case 1: b = make(a.sexp >> 15 ^ 1, a.sexp & 0x7fff, a.mant); break; /* opposite */
        case 2: b = make(a.sexp >> 15 ^ 1, a.sexp & 0x7fff, a.mant ^ (next() & 0xff)); break; /* close */
        default: b = operand();
        }
        for (unsigned rc = 0; rc < 4; rc++) {
            faddp(a, b, rc);
            fsubp(a, b, rc);
            fsubrp(a, b, rc);
            fmulp(a, b, rc);
            fdivp(a, b, rc);
            fdivrp(a, b, rc);
            fadd_st(a, b, rc);
            fsub_st(a, b, rc);
            fdiv_st(a, b, rc);
            fsqrt_(a, rc);
            to_int(a, rc);
            to_float(a, rc);
        }
        compare(a, b);
        from_memory(memory_operand());
        /* Reduced precision (python sets 53 bits): only arithmetic rounds. */
        for (unsigned pc = 0; pc <= 2; pc += 2) {
            precision_control = pc;
            for (unsigned rc = 0; rc < 4; rc++) {
                faddp(a, b, rc);
                fsubrp(a, b, rc);
                fmulp(a, b, rc);
                fdivrp(a, b, rc);
                fadd_st(a, b, rc);
                fsqrt_(a, rc);
            }
            precision_control = 3;
        }
    }
    fflush(stdout);
    return 0;
}
