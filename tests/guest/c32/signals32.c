/*
 * i386 signals (ADR-0029): what the 32-bit frames carry and restore.
 * - a handler with SA_SIGINFO (rt frame): siginfo of kill and of a fault,
 *   ucontext registers (eip, esp, eax), changed registers taken back;
 * - a handler without SA_SIGINFO (the old frame, sigreturn);
 * - the signal mask during and after a handler, sa_mask, SA_RESETHAND,
 *   SA_NODEFER;
 * - faults: SIGSEGV with si_addr, skipping the faulting instruction by
 *   moving eip; SIGFPE from a division by zero; SIGILL from ud2 (trapno,
 *   err and the selectors are in signal_context.c: qemu-i386 differs);
 * - the alternate stack; siglongjmp out of a handler;
 * - the x87 and SSE state: changed in the handler, restored after it.
 * Prints only values that do not depend on the machine.
 */
#define _GNU_SOURCE
/* SSE2 for the xmm0 checks (i686 code has none by default). */
#pragma GCC target("sse2")
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ucontext.h>
#include <unistd.h>

static volatile int count;
static volatile int last_code;
static volatile int mask_had_usr2;
static sigjmp_buf jump;

static int blocked(int sig) {
    sigset_t set;
    sigprocmask(SIG_BLOCK, NULL, &set);
    return sigismember(&set, sig);
}

static void info_handler(int sig, siginfo_t *info, void *context) {
    ucontext_t *uc = context;
    count++;
    last_code = info->si_code;
    printf("info: sig %d signo %d code %d pid ok %d self blocked %d\n", sig,
           info->si_signo, info->si_code, info->si_pid == getpid(),
           blocked(sig));
    /* Not uc_flags: qemu-i386 sets UC_FP_XSTATE (signal_context.c). */
    printf("  uc: eax %d, esp in frame %d\n",
           (int)uc->uc_mcontext.gregs[REG_EAX],
           uc->uc_mcontext.gregs[REG_ESP] > (greg_t)(uintptr_t)&uc);
    mask_had_usr2 = sigismember(&uc->uc_sigmask, SIGUSR2);
}

static void plain_handler(int sig) {
    count++;
    printf("plain: sig %d, usr2 blocked %d\n", sig, blocked(SIGUSR2));
}

static void segv_handler(int sig, siginfo_t *info, void *context) {
    ucontext_t *uc = context;
    printf("segv: code %d addr %#lx\n", info->si_code,
           (unsigned long)info->si_addr);
    /* Skip the faulting `movl $7, (%eax)` (6 bytes) and set eax. */
    uc->uc_mcontext.gregs[REG_EIP] += 6;
    uc->uc_mcontext.gregs[REG_EAX] = 1234;
}

static void fpe_handler(int sig, siginfo_t *info, void *context) {
    printf("fpe: code %d\n", info->si_code);
    siglongjmp(jump, 1);
}

static void ill_handler(int sig, siginfo_t *info, void *context) {
    ucontext_t *uc = context;
    printf("ill: code %d\n", info->si_code);
    uc->uc_mcontext.gregs[REG_EIP] += 2; /* ud2 */
}

static char alt[65536];

static void alt_handler(int sig) {
    char here;
    printf("alt: on the alternate stack %d\n",
           &here >= alt && &here < alt + sizeof alt);
    stack_t now;
    sigaltstack(NULL, &now);
    printf("alt: SS_ONSTACK %d\n", (now.ss_flags & SS_ONSTACK) != 0);
}

static volatile double fp_result;

static void fp_handler(int sig) {
    /* Change the x87 control word and xmm registers inside. */
    unsigned short cw = 0x0c7f;
    __asm__ volatile("fldcw %0" ::"m"(cw));
    fp_result = 1.0 / 3.0;
    __asm__ volatile("pxor %%xmm0, %%xmm0" ::: "xmm0");
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);

    /* SA_SIGINFO, kill to self, sa_mask with SIGUSR2. */
    sa.sa_sigaction = info_handler;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaddset(&sa.sa_mask, SIGUSR2);
    sigaction(SIGUSR1, &sa, NULL);
    kill(getpid(), SIGUSR1);
    printf("after: count %d, usr1 blocked %d, usr2 blocked %d, uc mask had usr2 %d\n",
           count, blocked(SIGUSR1), blocked(SIGUSR2), mask_had_usr2);
    raise(SIGUSR1);
    printf("raise code %d\n", last_code);

    /* Without SA_SIGINFO: the old frame. SA_RESETHAND. */
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = plain_handler;
    sa.sa_flags = SA_RESETHAND;
    sigaction(SIGUSR2, &sa, NULL);
    raise(SIGUSR2);
    struct sigaction now;
    sigaction(SIGUSR2, NULL, &now);
    printf("reset to default %d\n", now.sa_handler == SIG_DFL);

    /* A pending blocked signal arrives when unblocked. */
    sa.sa_handler = plain_handler;
    sa.sa_flags = 0;
    sigaction(SIGUSR2, &sa, NULL);
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR2);
    sigprocmask(SIG_BLOCK, &set, NULL);
    raise(SIGUSR2);
    sigset_t pending;
    sigpending(&pending);
    printf("pending %d count %d\n", sigismember(&pending, SIGUSR2), count);
    sigprocmask(SIG_UNBLOCK, &set, NULL);
    printf("delivered count %d\n", count);

    /* SIGSEGV: skip the faulting store. */
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = segv_handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    int eax;
    __asm__ volatile("movl $16, %%eax\n\t"
                     "movl $7, (%%eax)\n\t"
                     : "=a"(eax)::"memory");
    printf("after segv: eax %d\n", eax);

    /* SIGFPE and siglongjmp. */
    sa.sa_sigaction = fpe_handler;
    sigaction(SIGFPE, &sa, NULL);
    if (sigsetjmp(jump, 1) == 0) {
        int quotient, zero = 0;
        __asm__ volatile("cltd\n\t"
                         "idivl %1"
                         : "=a"(quotient)
                         : "r"(zero), "a"(1)
                         : "edx");
        printf("no fault: %d\n", quotient);
    } else {
        printf("back from fpe, fpe blocked %d\n", blocked(SIGFPE));
    }

    /* SIGILL from ud2. */
    sa.sa_sigaction = ill_handler;
    sigaction(SIGILL, &sa, NULL);
    __asm__ volatile("ud2");
    printf("after ud2\n");

    /* Alternate stack. */
    stack_t ss = {.ss_sp = alt, .ss_size = sizeof alt, .ss_flags = 0};
    sigaltstack(&ss, NULL);
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = alt_handler;
    sa.sa_flags = SA_ONSTACK;
    sigaction(SIGUSR1, &sa, NULL);
    raise(SIGUSR1);

    /* FPU state restored after a handler. */
    sa.sa_handler = fp_handler;
    sa.sa_flags = 0;
    sigaction(SIGUSR1, &sa, NULL);
    unsigned short before, after;
    __asm__ volatile("fnstcw %0" : "=m"(before));
    double kept;
    __asm__ volatile("movsd %1, %%xmm0\n\t"
                     "int $0x80\n\t" /* getpid, harmless */
                     : "=m"(kept)
                     : "m"((double){2.5}), "a"(20)
                     : "xmm0");
    raise(SIGUSR1);
    __asm__ volatile("fnstcw %0" : "=m"(after));
    __asm__ volatile("movsd %%xmm0, %0" : "=m"(kept));
    printf("fpu: cw %#x -> %#x, xmm0 %g, handler result %.6f\n", before,
           after, kept, fp_result);
    return 0;
}
