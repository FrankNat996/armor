/*
 * ppoll with a signal mask (Chromium waits with one): SIGUSR1 blocked and
 * raised before the call; ppoll with an empty mask lets the handler run
 * and returns EINTR at once, and afterwards SIGUSR1 is blocked again; with
 * SIGUSR1 in the mask the call times out and the signal stays pending. A
 * bad mask size is EINVAL. Then SIOCGIFNAME for index 1 (the loopback
 * interface). The same source runs as x86-64 and i386.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <net/if.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t handled;

static void on_usr1(int signal) { handled += signal == SIGUSR1; }

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EINTR: return "EINTR";
    case EINVAL: return "EINVAL";
    default: return "other";
    }
}

int main(void) {
    struct sigaction action = {.sa_handler = on_usr1};
    sigaction(SIGUSR1, &action, NULL);
    sigset_t usr1, empty, now;
    sigemptyset(&usr1);
    sigaddset(&usr1, SIGUSR1);
    sigemptyset(&empty);
    sigprocmask(SIG_BLOCK, &usr1, NULL);
    raise(SIGUSR1);
    printf("handled before: %d\n", handled);

    int p[2];
    if (pipe(p) != 0) {
        return 1;
    }
    struct pollfd fd = {.fd = p[0], .events = POLLIN};
    struct timespec second = {1, 0};
    int r = ppoll(&fd, 1, &second, &empty);
    printf("ppoll empty mask: %d %s, handled %d\n", r, name(r < 0 ? errno : 0), handled);
    sigprocmask(SIG_BLOCK, NULL, &now);
    printf("usr1 blocked again: %d\n", sigismember(&now, SIGUSR1));

    raise(SIGUSR1);
    struct timespec short_wait = {0, 20 * 1000 * 1000};
    r = ppoll(&fd, 1, &short_wait, &usr1);
    sigset_t pending;
    sigpending(&pending);
    printf("ppoll usr1 mask: %d %s, handled %d, pending %d\n", r, name(r < 0 ? errno : 0), handled,
           sigismember(&pending, SIGUSR1));
    r = syscall(SYS_ppoll, &fd, 1, &short_wait, &empty, 4);
    printf("ppoll mask size 4: %d %s\n", r < 0 ? -1 : r, name(r < 0 ? errno : 0));
    sigprocmask(SIG_UNBLOCK, &usr1, NULL);
    printf("handled at the end: %d\n", handled);

    int s = socket(AF_INET, SOCK_DGRAM, 0);
    struct ifreq ifr;
    memset(&ifr, 0, sizeof ifr);
    ifr.ifr_ifindex = 1;
    r = ioctl(s, SIOCGIFNAME, &ifr);
    printf("SIOCGIFNAME 1: %d %s\n", r, r == 0 ? ifr.ifr_name : name(errno));
    return 0;
}
