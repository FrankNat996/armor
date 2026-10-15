/*
 * signalfd (ADR-0044): SIGUSR1 blocked and raised, then read from a
 * signalfd as a struct signalfd_siginfo (signo, code); poll sees it
 * readable first; with nothing pending a non-blocking read is EAGAIN; the
 * old signalfd call; a mask of the wrong size and bad flags are EINVAL;
 * changing the mask of an existing signalfd.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/signalfd.h>
#include <sys/syscall.h>
#include <unistd.h>

static void show(const char *what, long r) {
    if (r < 0) {
        printf("%-26s -1 %s\n", what, strerror(errno));
    } else {
        printf("%-26s %ld\n", what, r);
    }
}

int main(void) {
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    sigprocmask(SIG_BLOCK, &set, NULL);
    int fd = signalfd(-1, &set, SFD_NONBLOCK | SFD_CLOEXEC);
    printf("%-26s %s\n", "signalfd", fd >= 0 ? "ok" : strerror(errno));
    struct signalfd_siginfo info;
    show("read, nothing pending", read(fd, &info, sizeof info));
    raise(SIGUSR1);
    struct pollfd p = {fd, POLLIN, 0};
    show("poll", poll(&p, 1, 1000));
    printf("%-26s %d\n", "revents POLLIN", (p.revents & POLLIN) != 0);
    show("read", read(fd, &info, sizeof info));
    printf("%-26s %u %d\n", "signo code", info.ssi_signo, info.ssi_code);

    /* Add SIGUSR2 to the same signalfd. */
    sigaddset(&set, SIGUSR2);
    sigprocmask(SIG_BLOCK, &set, NULL);
    show("signalfd update", signalfd(fd, &set, 0) == fd ? 0 : -1);
    kill(getpid(), SIGUSR2);
    show("read SIGUSR2", read(fd, &info, sizeof info));
    printf("%-26s %u %d\n", "signo code", info.ssi_signo, info.ssi_code);

    long old = syscall(SYS_signalfd, -1, &set, 8);
    printf("%-26s %s\n", "old signalfd", old >= 0 ? "ok" : strerror(errno));
    show("mask size 4", syscall(SYS_signalfd4, -1, &set, 4, 0));
    show("bad flags", syscall(SYS_signalfd4, -1, &set, 8, 1));
    return 0;
}
