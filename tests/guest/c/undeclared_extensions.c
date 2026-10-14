/*
 * Instructions of extensions armor's CPUID does not report raise #UD
 * (SIGILL, ILL_ILLOPN, at the instruction), as on a CPU without them
 * (ADR-0026): AVX (VEX), AVX-512 (EVEX), SHA, XOP, 3DNow!, VIA PadLock.
 * The hardware oracle (an Intel Xeon X5675) has none of them either.
 */
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ucontext.h>

static sigjmp_buf jump;
static volatile int got, code;
static volatile uintptr_t addr, rip;
uintptr_t undeclared_at;

static void handler(int sig, siginfo_t *si, void *context) {
    ucontext_t *uc = context;
    got = sig;
    code = si->si_code;
    addr = (uintptr_t)si->si_addr;
    rip = (uintptr_t)uc->uc_mcontext.gregs[REG_RIP];
    siglongjmp(jump, 1);
}

/* The bytes of one instruction, after recording its address. */
#define TRY(name, bytes)                                                      \
    do {                                                                      \
        got = 0;                                                              \
        if (!sigsetjmp(jump, 1)) {                                            \
            __asm__ volatile("lea 1f(%%rip), %%rax\n\t"                       \
                             "mov %%rax, undeclared_at(%%rip)\n"              \
                             "1:\t.byte " bytes "\n\t" ::                     \
                                 : "rax", "rcx", "memory");                   \
            printf("%-14s executed\n", name);                                 \
        } else {                                                              \
            printf("%-14s signal %d code %d addr %s rip %s\n", name, got,     \
                   code, addr == undeclared_at ? "ok" : "WRONG",              \
                   rip == undeclared_at ? "ok" : "WRONG");                    \
        }                                                                     \
    } while (0)

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGILL, &sa, NULL);

    TRY("vzeroupper", "0xc5, 0xf8, 0x77");
    TRY("vpxor ymm", "0xc5, 0xfd, 0xef, 0xc0");
    TRY("vpcmpeqb", "0xc5, 0xfd, 0x74, 0xc1");
    TRY("andn (BMI1)", "0xc4, 0xe2, 0x70, 0xf2, 0xc1");
    TRY("vmovdqu64 zmm", "0x62, 0xf1, 0xfe, 0x48, 0x6f, 0xc1");
    TRY("sha256rnds2", "0x0f, 0x38, 0xcb, 0xca");
    TRY("vprotd (XOP)", "0x8f, 0xe8, 0x78, 0xc2, 0xc1, 0x03");
    TRY("pfadd (3DNow)", "0x0f, 0x0f, 0xc1, 0x9e");
    TRY("xsha256", "0xf3, 0x0f, 0xa6, 0xd0");
    puts("done");
    return 0;
}
