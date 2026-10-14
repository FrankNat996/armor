/*
 * Unmasked SSE floating-point exceptions (#XM, SIGFPE): for each case,
 * MXCSR is loaded with some exceptions unmasked and one instruction runs
 * on xmm0 and xmm1. Printed: whether SIGFPE arrived, its si_code, whether
 * si_addr and the saved rip are the faulting instruction, MXCSR and xmm0
 * as saved in the signal frame (was the destination written?), and MXCSR
 * after siglongjmp out of the handler. Without a signal: MXCSR and xmm0
 * after the instruction. Last, the floating-point state a handler starts
 * with (Linux gives it the initial one: xmm zeroed, MXCSR 0x1f80, x87
 * reset) and the state after sigreturn (the interrupted one).
 */
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ucontext.h>

static sigjmp_buf jump;
static volatile int got_signal, code;
static volatile uintptr_t addr, rip;
static volatile unsigned frame_mxcsr;
static volatile uint64_t frame_xmm0[2];
/* Set by the asm right before the instruction. */
uintptr_t sse_exceptions_at;
const long long sse_exceptions_big = 0x7fffffffffffffffll;

static void handler(int sig, siginfo_t *si, void *context) {
    ucontext_t *uc = context;
    got_signal = sig;
    code = si->si_code;
    addr = (uintptr_t)si->si_addr;
    rip = (uintptr_t)uc->uc_mcontext.gregs[REG_RIP];
    frame_mxcsr = uc->uc_mcontext.fpregs->mxcsr;
    memcpy((void *)frame_xmm0, &uc->uc_mcontext.fpregs->_xmm[0], 16);
    siglongjmp(jump, 1);
}

static unsigned read_mxcsr(void) {
    unsigned m;
    __asm__ volatile("stmxcsr %0" : "=m"(m));
    return m;
}

/*
 * xmm0 = a, xmm1 = b (16 bytes each); MXCSR = control; `insn` (AT&T:
 * source first); then MXCSR and xmm0 are read back and MXCSR reset.
 */
#define CASE(name, control, insn, a0, a1, b0, b1)                             \
    do {                                                                      \
        static uint64_t a[2], b[2], out[2];                                   \
        static unsigned m, after, dflt = 0x1f80;                              \
        a[0] = a0, a[1] = a1, b[0] = b0, b[1] = b1, m = control;              \
        got_signal = 0;                                                       \
        if (!sigsetjmp(jump, 1)) {                                            \
            __asm__ volatile("movupd %[a], %%xmm0\n\t"                        \
                             "movupd %[b], %%xmm1\n\t"                        \
                             "ldmxcsr %[m]\n\t"                               \
                             "lea 1f(%%rip), %%rax\n\t"                       \
                             "mov %%rax, sse_exceptions_at(%%rip)\n"          \
                             "1:\t" insn "\n\t"                               \
                             "stmxcsr %[after]\n\t"                           \
                             "ldmxcsr %[dflt]\n\t"                            \
                             "movupd %%xmm0, %[out]"                          \
                             : [out] "=m"(out), [after] "=m"(after)           \
                             : [a] "m"(a), [b] "m"(b), [m] "m"(m),            \
                               [dflt] "m"(dflt)                               \
                             : "rax", "xmm0", "xmm1", "memory");              \
            printf("%-28s no signal, mxcsr %04x, xmm0 %016llx %016llx\n",     \
                   name, after, (unsigned long long)out[1],                   \
                   (unsigned long long)out[0]);                               \
        } else {                                                              \
            printf("%-28s signal %d code %d addr %s rip %s, frame mxcsr "     \
                   "%04x, xmm0 %016llx %016llx; after %04x\n",                \
                   name, got_signal, code,                                    \
                   addr == sse_exceptions_at ? "ok" : "WRONG",                \
                   rip == sse_exceptions_at ? "ok" : "WRONG", frame_mxcsr,    \
                   (unsigned long long)frame_xmm0[1],                         \
                   (unsigned long long)frame_xmm0[0], read_mxcsr());          \
            __asm__ volatile("ldmxcsr %0" ::"m"(dflt));                       \
        }                                                                     \
    } while (0)

#define ONE 0x3ff0000000000000ull
#define TWO 0x4000000000000000ull
#define THREE 0x4008000000000000ull
#define HALF 0x3fe0000000000000ull
#define M1 0xbff0000000000000ull
#define QNAN 0x7ff8000000000123ull
#define SNAN 0x7ff0000000000456ull
#define MAX 0x7fefffffffffffffull
#define MIN 0x0010000000000000ull
#define DEN 0x0000000000000001ull
#define BIG 0x41e65a0bc0000000ull /* 3e9 */
#define X 0x1111111111111111ull   /* upper lane, must survive */

/* MXCSR with these exceptions unmasked (the rest masked). */
#define UNMASK(bits) (0x1f80u & ~(bits))
#define IM 0x080u
#define DM 0x100u
#define ZM 0x200u
#define OM 0x400u
#define UM 0x800u
#define PM 0x1000u

static volatile uint64_t entry_xmm5[2];
static volatile unsigned entry_mxcsr;
static volatile unsigned short entry_cw, entry_sw, entry_tw;

static void entry_state(int sig) {
    unsigned char env[28];
    (void)sig;
    __asm__ volatile("movupd %%xmm5, %0" : "=m"(entry_xmm5));
    __asm__ volatile("stmxcsr %0" : "=m"(entry_mxcsr));
    __asm__ volatile("fnstenv %0" : "=m"(env));
    entry_cw = env[0] | env[1] << 8;
    entry_sw = env[4] | env[5] << 8;
    entry_tw = env[8] | env[9] << 8;
    /* fnstenv masks every x87 exception: load the control word back. */
    __asm__ volatile("fldcw %0" ::"m"(entry_cw));
}

static void handler_entry_state(void) {
    static const uint64_t pattern[2] = {0x1234567812345678ull, 0x9abcdef09abcdef0ull};
    static const unsigned control = 0x9fc0;
    static const unsigned short cw = 0x27f;
    uint64_t after[2];
    unsigned mxcsr;
    unsigned short after_cw;
    __asm__ volatile("movupd %0, %%xmm5\n\t"
                     "ldmxcsr %1\n\t"
                     "fldcw %2\n\t"
                     "fld1\n\t"
                     "fld1" ::"m"(pattern),
                     "m"(control), "m"(cw)
                     : "xmm5");
    signal(SIGUSR1, entry_state);
    raise(SIGUSR1);
    __asm__ volatile("movupd %%xmm5, %0\n\t"
                     "stmxcsr %1\n\t"
                     "fnstcw %2\n\t"
                     "fstp %%st(0)\n\t"
                     "fstp %%st(0)"
                     : "=m"(after), "=m"(mxcsr), "=m"(after_cw));
    printf("handler entry: xmm5 %016llx %016llx, mxcsr %04x, x87 cw %04x sw %04x tw %04x\n",
           (unsigned long long)entry_xmm5[1], (unsigned long long)entry_xmm5[0], entry_mxcsr,
           entry_cw, entry_sw, entry_tw);
    printf("after sigreturn: xmm5 %016llx %016llx, mxcsr %04x, x87 cw %04x\n",
           (unsigned long long)after[1], (unsigned long long)after[0], mxcsr, after_cw);
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGFPE, &sa, NULL);

    CASE("divsd 1/0, ZE", UNMASK(ZM), "divsd %%xmm1, %%xmm0", ONE, X, 0, X);
    CASE("divsd 0/0, ZE only", UNMASK(ZM), "divsd %%xmm1, %%xmm0", 0, X, 0, X);
    CASE("divsd 0/0, IE", UNMASK(IM), "divsd %%xmm1, %%xmm0", 0, X, 0, X);
    CASE("sqrtsd -1, IE", UNMASK(IM), "sqrtsd %%xmm1, %%xmm0", ONE, X, M1, X);
    CASE("addsd snan, IE", UNMASK(IM), "addsd %%xmm1, %%xmm0", ONE, X, SNAN, X);
    CASE("addsd qnan, IE", UNMASK(IM), "addsd %%xmm1, %%xmm0", ONE, X, QNAN, X);
    CASE("minsd qnan, IE", UNMASK(IM), "minsd %%xmm1, %%xmm0", ONE, X, QNAN, X);
    CASE("comisd qnan, IE", UNMASK(IM), "comisd %%xmm1, %%xmm0", ONE, X, QNAN, X);
    CASE("ucomisd qnan, IE", UNMASK(IM), "ucomisd %%xmm1, %%xmm0", ONE, X, QNAN, X);
    CASE("addsd den, DE", UNMASK(DM), "addsd %%xmm1, %%xmm0", ONE, X, DEN, X);
    CASE("addsd den, PE", UNMASK(PM), "addsd %%xmm1, %%xmm0", ONE, X, DEN, X);
    CASE("mulsd max*2, OE", UNMASK(OM), "mulsd %%xmm1, %%xmm0", MAX, X, TWO, X);
    CASE("mulsd max*2, PE", UNMASK(PM), "mulsd %%xmm1, %%xmm0", MAX, X, TWO, X);
    CASE("divsd min/3, UE", UNMASK(UM), "divsd %%xmm1, %%xmm0", MIN, X, THREE, X);
    CASE("mulsd min/2 exact, UE", UNMASK(UM), "mulsd %%xmm1, %%xmm0", MIN, X, HALF, X);
    CASE("divsd 1/3, PE", UNMASK(PM), "divsd %%xmm1, %%xmm0", ONE, X, THREE, X);
    CASE("divsd min/3, all", 0x0000, "divsd %%xmm1, %%xmm0", MIN, X, THREE, X);
    CASE("divsd den/0, all", 0x0000, "divsd %%xmm1, %%xmm0", DEN, X, 0, X);
    CASE("divsd min/3, FZ, UE", 0x8000 | UNMASK(UM), "divsd %%xmm1, %%xmm0", MIN, X, THREE, X);
    CASE("addsd den, DAZ, DE", 0x0040 | UNMASK(DM), "addsd %%xmm1, %%xmm0", ONE, X, DEN, X);
    CASE("addsd 1+1, ZE pending", UNMASK(ZM) | 0x4, "addsd %%xmm1, %%xmm0", ONE, X, ONE, X);
    CASE("cvtsd2si 2.5, PE", UNMASK(PM), "cvtsd2si %%xmm1, %%rax", ONE, X, 0x4004000000000000ull, X);
    CASE("cvtsd2si 3e9 eax, IE", UNMASK(IM), "cvtsd2si %%xmm1, %%eax", ONE, X, BIG, X);
    CASE("cvtsd2ss max, OE", UNMASK(OM), "cvtsd2ss %%xmm1, %%xmm0", ONE, X, MAX, X);
    CASE("cvtsi2sd big, PE", UNMASK(PM), "cvtsi2sdq sse_exceptions_big(%%rip), %%xmm0", ONE, X, 0, 0);
    /* Packed: one lane unmasked-exceptional, the other masked-exceptional. */
    CASE("divpd [1/0, 0/0], ZE", UNMASK(ZM), "divpd %%xmm1, %%xmm0", ONE, 0, 0, 0);
    CASE("divpd [0/0, 1/0], ZE", UNMASK(ZM), "divpd %%xmm1, %%xmm0", 0, ONE, 0, 0);
    CASE("mulpd [max*2, 1/3], OE", UNMASK(OM), "mulpd %%xmm1, %%xmm0", MAX, ONE, TWO, THREE);
    CASE("addpd [1+den, 1+1], DE", UNMASK(DM), "addpd %%xmm1, %%xmm0", ONE, ONE, DEN, ONE);
    CASE("divpd [1/3, snan], PE", UNMASK(PM), "divpd %%xmm1, %%xmm0", ONE, ONE, THREE, SNAN);
    CASE("divpd [max/0.5, 1/3], OE", UNMASK(OM), "divpd %%xmm1, %%xmm0", MAX, ONE, HALF, THREE);
    CASE("divpd [1/3, min/2], UE", UNMASK(UM), "divpd %%xmm1, %%xmm0", ONE, MIN, THREE, TWO);
    CASE("divpd [min/2, 1/3], UE+PE", UNMASK(UM | PM), "divpd %%xmm1, %%xmm0", MIN, ONE, TWO, THREE);
    handler_entry_state();
    return 0;
}
