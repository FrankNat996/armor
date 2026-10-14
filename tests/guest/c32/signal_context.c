/*
 * i386 sigcontext details that only a real x86-64 kernel gives (ADR-0029;
 * qemu-i386 differs, so difftest compares this with --oracle only):
 * trapno and the page-fault error code of a fault (kept by the kernel
 * for later signals too), the segment selectors a 32-bit process has (cs
 * 0x23, ds/es/ss 0x2b, gs the TLS entry set by glibc, fs 0), uc_flags 0
 * (no XSAVE), and the FPU state of the frame: the FSAVE header converted
 * from FXSAVE and the software-reserved bytes of the FXSAVE image.
 */
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ucontext.h>

static void show(const char *what, ucontext_t *uc, siginfo_t *info) {
    greg_t *g = uc->uc_mcontext.gregs;
    printf("%s: signo %d code %d trapno %d err %d\n", what, info->si_signo,
           info->si_code, (int)g[REG_TRAPNO], (int)g[REG_ERR]);
    printf("  cs %#x ss %#x ds %#x es %#x fs %#x gs %#x\n",
           (unsigned)g[REG_CS] & 0xffff, (unsigned)g[REG_SS] & 0xffff,
           (unsigned)g[REG_DS] & 0xffff, (unsigned)g[REG_ES] & 0xffff,
           (unsigned)g[REG_FS] & 0xffff, (unsigned)g[REG_GS] & 0xffff);
}

/* The length of the instruction the next fault skips. */
static volatile int skip;

static void segv(int sig, siginfo_t *info, void *context) {
    ucontext_t *uc = context;
    show("segv", uc, info);
    uc->uc_mcontext.gregs[REG_EIP] += skip;
}

static void ill(int sig, siginfo_t *info, void *context) {
    ucontext_t *uc = context;
    show("ill", uc, info);
    uc->uc_mcontext.gregs[REG_EIP] += 2; /* ud2 */
}

static void usr1(int sig, siginfo_t *info, void *context) {
    ucontext_t *uc = context;
    show("usr1", uc, info);
    printf("  uc_flags %lu\n", uc->uc_flags);
    /* The FPU state: the legacy FSAVE header the kernel converts from
     * FXSAVE, then the FXSAVE image and its software-reserved bytes. */
    struct _libc_fpstate *fp = uc->uc_mcontext.fpregs;
    unsigned char *raw = (unsigned char *)fp;
    /* Not ipoff and dataoff: armor does not track the last x87
     * instruction (docs/tech/floating-point.md). */
    printf("  fp: cw %#lx sw %#lx tag %#lx cssel %#lx datasel %#lx\n", fp->cw,
           fp->sw, fp->tag, fp->cssel, fp->datasel);
    printf("  fp: status %#x magic %#x\n", *(unsigned short *)(raw + 108),
           *(unsigned short *)(raw + 110));
    unsigned char *fx = raw + 112;
    unsigned *sw = (unsigned *)(fx + 464);
    printf("  fx: fcw %#x mxcsr %#x sw_reserved %#x %u %#x%08x %u\n", *(unsigned short *)fx,
           *(unsigned *)(fx + 24), sw[0], sw[1], sw[3], sw[2], sw[4]);
    long double st0;
    memcpy(&st0, &fp->_st[0], 10);
    printf("  st0 %Lg\n", st0);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_flags = SA_SIGINFO;
    sa.sa_sigaction = segv;
    sigaction(SIGSEGV, &sa, NULL);
    sa.sa_sigaction = ill;
    sigaction(SIGILL, &sa, NULL);
    sa.sa_sigaction = usr1;
    sigaction(SIGUSR1, &sa, NULL);
    /* A write to an unmapped page (movl $7, (%eax): 6 bytes), a read of
     * one (movl (%eax), %eax: 2 bytes), ud2, and a signal. */
    skip = 6;
    __asm__ volatile("movl $16, %%eax\n\tmovl $7, (%%eax)" ::: "eax", "memory");
    skip = 2;
    int value;
    __asm__ volatile("movl $32, %%eax\n\tmovl (%%eax), %%eax" : "=a"(value)::"memory");
    __asm__ volatile("ud2");
    /* Two values on the x87 stack when the signal comes. */
    long double a = 1.5L, b = -2.25L;
    __asm__ volatile("fldt %0\n\tfldt %1" ::"m"(a), "m"(b));
    raise(SIGUSR1);
    __asm__ volatile("fstp %st(0)\n\tfstp %st(0)");
    return 0;
}
