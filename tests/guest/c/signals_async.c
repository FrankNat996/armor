/*
 * Asynchronous signals (milestone M3): a timer interrupting a busy loop
 * (delivery between instructions), pause, a blocking read interrupted
 * with and without SA_RESTART, sigsuspend, and a handler on an alternate
 * stack.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <unistd.h>

static volatile sig_atomic_t alarms;
static int pipe_fds[2];
static char alt_stack[64 * 1024];
static volatile int on_alt_stack;

static void on_alarm(int sig) {
    (void)sig;
    alarms++;
}

/* Feeds the pipe so that a restarted read finds data. */
static void on_alarm_feed(int sig) {
    (void)sig;
    alarms++;
    char byte = 'x';
    if (write(pipe_fds[1], &byte, 1) != 1)
        _exit(3);
}

static void on_alt(int sig) {
    (void)sig;
    char local;
    uintptr_t at = (uintptr_t)&local;
    on_alt_stack = at >= (uintptr_t)alt_stack && at < (uintptr_t)alt_stack + sizeof alt_stack;
}

static void install(int sig, void (*handler)(int), int flags) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = handler;
    sa.sa_flags = flags;
    sigemptyset(&sa.sa_mask);
    sigaction(sig, &sa, NULL);
}

static void arm_timer(long usec) {
    struct itimerval t = {{0, 0}, {0, usec}};
    setitimer(ITIMER_REAL, &t, NULL);
}

int main(void) {
    /* A busy loop that only ends when the handler has run. */
    install(SIGALRM, on_alarm, 0);
    arm_timer(20000);
    unsigned long spins = 0;
    while (alarms == 0)
        spins++;
    printf("busy loop: alarms %d\n", (int)alarms);

    /* pause returns -1 EINTR after the handler. */
    arm_timer(20000);
    int r = pause();
    printf("pause: %d %s alarms %d\n", r, r < 0 && errno == EINTR ? "EINTR" : "?", (int)alarms);

    /* A blocking read: EINTR without SA_RESTART... */
    if (pipe(pipe_fds) != 0)
        return 2;
    char byte;
    arm_timer(20000);
    ssize_t n = read(pipe_fds[0], &byte, 1);
    printf("read: %zd %s\n", n, n < 0 && errno == EINTR ? "EINTR" : "?");

    /* ...restarted with it: the handler writes the byte the read gets. */
    install(SIGALRM, on_alarm_feed, SA_RESTART);
    arm_timer(20000);
    n = read(pipe_fds[0], &byte, 1);
    printf("restarted read: %zd '%c'\n", n, n == 1 ? byte : '?');

    /* sigsuspend: SIGALRM blocked, the timer fires while suspended with an
       empty mask, and the old mask (SIGALRM blocked) is back afterwards. */
    install(SIGALRM, on_alarm, 0);
    sigset_t block, empty, now;
    sigemptyset(&block);
    sigaddset(&block, SIGALRM);
    sigemptyset(&empty);
    sigprocmask(SIG_BLOCK, &block, NULL);
    int before = alarms;
    arm_timer(20000);
    r = sigsuspend(&empty);
    sigprocmask(SIG_BLOCK, NULL, &now);
    printf("sigsuspend: %d %s handled %d, still blocked %d\n", r,
           r < 0 && errno == EINTR ? "EINTR" : "?", alarms - before, sigismember(&now, SIGALRM));
    sigprocmask(SIG_UNBLOCK, &block, NULL);

    /* A handler on the alternate stack. */
    stack_t ss = {.ss_sp = alt_stack, .ss_size = sizeof alt_stack, .ss_flags = 0};
    if (sigaltstack(&ss, NULL) != 0)
        return 4;
    install(SIGUSR1, on_alt, SA_ONSTACK);
    raise(SIGUSR1);
    printf("alternate stack: %d\n", on_alt_stack);
    stack_t old;
    sigaltstack(NULL, &old);
    printf("sigaltstack: flags %d size %zu\n", old.ss_flags, old.ss_size);
    return 0;
}
