/*
 * sigtimedwait and sigwaitinfo (Chromium's crash handler waits for
 * signals this way, under a Spotify build): a blocked SIGUSR1 raised
 * before the call is taken with its siginfo (code SI_TKILL, our pid); a
 * signal sent by a child while the parent waits arrives (SI_USER, the
 * child's pid); an empty wait times out with EAGAIN; a bad timeout is
 * EINVAL; sigqueue and rt_tgsigqueueinfo deliver their values; nothing
 * reaches the handler.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t handled;

static void on_signal(int signal) { handled += signal; }

int main(void) {
    struct sigaction action = {.sa_handler = on_signal};
    sigaction(SIGUSR1, &action, NULL);
    sigaction(SIGUSR2, &action, NULL);
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    sigaddset(&set, SIGUSR2);
    sigprocmask(SIG_BLOCK, &set, NULL);

    raise(SIGUSR1);
    siginfo_t info;
    struct timespec second = {1, 0};
    int r = sigtimedwait(&set, &info, &second);
    printf("pending: %d, code SI_TKILL %d, our pid %d\n", r, info.si_code == SI_TKILL,
           info.si_pid == getpid());

    pid_t parent = getpid();
    pid_t child = fork();
    if (child == 0) {
        struct timespec nap = {0, 50 * 1000 * 1000};
        nanosleep(&nap, NULL);
        kill(parent, SIGUSR2);
        _exit(0);
    }
    struct timespec five = {5, 0};
    r = sigtimedwait(&set, &info, &five);
    printf("from the child: %d, code SI_USER %d, child's pid %d\n", r, info.si_code == SI_USER,
           info.si_pid == child);
    waitpid(child, NULL, 0);

    struct timespec short_wait = {0, 10 * 1000 * 1000};
    r = sigtimedwait(&set, &info, &short_wait);
    printf("nothing pending: %d %s\n", r, r < 0 && errno == EAGAIN ? "EAGAIN" : "other");
    struct timespec bad = {0, -1};
    r = sigtimedwait(&set, &info, &bad);
    printf("bad timeout: %d %s\n", r, r < 0 && errno == EINVAL ? "EINVAL" : "other");
    /* sigqueue (rt_sigqueueinfo) carries a value; rt_tgsigqueueinfo
     * targets a thread. */
    union sigval value = {.sival_int = 42};
    sigqueue(getpid(), SIGUSR1, value);
    r = sigtimedwait(&set, &info, &second);
    printf("sigqueue: %d, SI_QUEUE %d, value %d\n", r, info.si_code == SI_QUEUE, info.si_value.sival_int);
    siginfo_t sent = {0};
    sent.si_signo = SIGUSR2;
    sent.si_code = SI_QUEUE;
    sent.si_pid = getpid();
    sent.si_value.sival_int = 7;
    printf("rt_tgsigqueueinfo: %ld\n",
           syscall(SYS_rt_tgsigqueueinfo, getpid(), gettid(), SIGUSR2, &sent));
    r = sigtimedwait(&set, &info, &second);
    printf("  taken: %d, value %d\n", r, info.si_value.sival_int);
    raise(SIGUSR2);
    r = sigwaitinfo(&set, NULL);
    printf("sigwaitinfo without info: %d\n", r);
    printf("handled: %d\n", handled);
    return 0;
}
