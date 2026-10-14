/*
 * Signal delivery to guest handlers (milestone M3): sigaction with and
 * without SA_SIGINFO, raise and kill, the signal mask (a blocked signal
 * stays pending until unblocked), SA_RESETHAND, SA_NODEFER, a handler that
 * modifies the interrupted registers through ucontext, siglongjmp out of a
 * handler, and synchronous signals from faults (SIGSEGV with si_addr,
 * SIGFPE, SIGILL, SIGTRAP) handled by the program.
 */
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ucontext.h>
#include <unistd.h>

static volatile sig_atomic_t count;
static volatile int last_signal;
static volatile int last_code;
static volatile uintptr_t last_addr;
static volatile int depth, max_depth, calls;
static sigjmp_buf jump;

static void plain(int sig) {
    count++;
    last_signal = sig;
}

static void info(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    count++;
    last_signal = sig;
    last_code = si->si_code;
    last_addr = (uintptr_t)si->si_addr;
}

/* Raises its own signal until it has run three times. */
static void nested(int sig) {
    depth++;
    calls++;
    if (depth > max_depth)
        max_depth = depth;
    if (calls < 3)
        raise(sig);
    depth--;
}

static void jump_out(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    last_signal = sig;
    last_code = si->si_code;
    last_addr = (uintptr_t)si->si_addr;
    siglongjmp(jump, 1);
}

/* Skips the faulting 2-byte `ud2` by moving rip in the saved context. */
static void skip_ud2(int sig, siginfo_t *si, void *ucv) {
    ucontext_t *uc = ucv;
    last_signal = sig;
    last_code = si->si_code;
    uc->uc_mcontext.gregs[REG_RIP] += 2;
    uc->uc_mcontext.gregs[REG_RAX] = 0x1234;
}

static void install(int sig, void (*handler)(int), int flags) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = handler;
    sa.sa_flags = flags;
    sigemptyset(&sa.sa_mask);
    sigaction(sig, &sa, NULL);
}

static void install_info(int sig, void (*handler)(int, siginfo_t *, void *), int flags) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO | flags;
    sigemptyset(&sa.sa_mask);
    sigaction(sig, &sa, NULL);
}

int main(void) {
    /* A plain handler, through raise (tgkill) and kill. */
    install(SIGUSR1, plain, 0);
    raise(SIGUSR1);
    kill(getpid(), SIGUSR1);
    printf("plain: count %d signal %d\n", (int)count, last_signal);

    /* SA_SIGINFO: si_code SI_TKILL for raise, SI_USER for kill. */
    install_info(SIGUSR2, info, 0);
    raise(SIGUSR2);
    printf("info raise: signal %d code %d\n", last_signal, last_code);
    kill(getpid(), SIGUSR2);
    printf("info kill: signal %d code %d\n", last_signal, last_code);

    /* Blocked: pending until unblocked. */
    sigset_t set, old;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    count = 0;
    sigprocmask(SIG_BLOCK, &set, &old);
    raise(SIGUSR1);
    sigset_t pending;
    sigpending(&pending);
    printf("blocked: count %d pending %d\n", (int)count, sigismember(&pending, SIGUSR1));
    sigprocmask(SIG_SETMASK, &old, NULL);
    printf("unblocked: count %d\n", (int)count);

    /* The handler runs with its own signal blocked, unless SA_NODEFER. */
    install(SIGUSR1, nested, 0);
    depth = max_depth = calls = 0;
    raise(SIGUSR1);
    printf("deferred: calls %d max depth %d\n", calls, max_depth);
    install(SIGUSR1, nested, SA_NODEFER);
    depth = max_depth = calls = 0;
    raise(SIGUSR1);
    printf("nodefer: calls %d max depth %d\n", calls, max_depth);

    /* SA_RESETHAND: the second signal has the default action (ignored for SIGCHLD). */
    install(SIGCHLD, plain, SA_RESETHAND);
    count = 0;
    raise(SIGCHLD);
    raise(SIGCHLD);
    printf("resethand: count %d\n", (int)count);

    /* Ignored signals do nothing. */
    install(SIGUSR1, SIG_IGN, 0);
    raise(SIGUSR1);
    printf("ignored: still running\n");

    /* A handler changing rip and rax in the saved context. */
    install_info(SIGILL, skip_ud2, 0);
    uint64_t rax;
    __asm__ volatile("mov $1, %%eax\n\tud2\n\tmov %%rax, %0" : "=r"(rax) : : "rax");
    printf("sigill: signal %d code %d rax %#lx\n", last_signal, last_code, (unsigned long)rax);

    /* Synchronous faults, left with siglongjmp. */
    install_info(SIGSEGV, jump_out, 0);
    volatile int *volatile bad = (volatile int *)16;
    if (sigsetjmp(jump, 1) == 0)
        *bad = 1;
    printf("sigsegv: signal %d code %d addr %#lx\n", last_signal, last_code, (unsigned long)last_addr);

    install_info(SIGFPE, jump_out, 0);
    if (sigsetjmp(jump, 1) == 0) {
        volatile int zero = 0;
        volatile int x = 7 / zero;
        (void)x;
    }
    printf("sigfpe: signal %d code %d\n", last_signal, last_code);

    install_info(SIGTRAP, jump_out, 0);
    if (sigsetjmp(jump, 1) == 0)
        __asm__ volatile("int3");
    printf("sigtrap: signal %d code %d\n", last_signal, last_code);

    /* The mask was restored by siglongjmp: SIGSEGV works a second time. */
    bad = (volatile int *)32;
    if (sigsetjmp(jump, 1) == 0)
        *bad = 1;
    printf("sigsegv again: addr %#lx\n", (unsigned long)last_addr);
    return 0;
}
