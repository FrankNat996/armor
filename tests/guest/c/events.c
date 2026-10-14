/*
 * Waiting for descriptors through glibc: select (a descriptor above 64,
 * so the set spans several longs, and the time left written back),
 * pselect, poll, ppoll, epoll (data round trip, EPOLL_CTL_DEL with no
 * event, errors) and eventfd (counter and semaphore modes). The same
 * source runs as x86-64 and i386, whose fd_set longs and timeval differ
 * (ADR-0029). Prints no times other than an expired timeout's zero.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/select.h>
#include <unistd.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EINVAL: return "EINVAL";
    case EAGAIN: return "EAGAIN";
    case EBADF: return "EBADF";
    case ENOENT: return "ENOENT";
    case EEXIST: return "EEXIST";
    default: return "other";
    }
}

static int show(const char *what, int result) {
    printf("%s: %d %s\n", what, result, name(result < 0 ? errno : 0));
    return result;
}

static void selects(void) {
    int p[2];
    if (pipe(p) != 0) {
        return;
    }
    /* A read end above 64: the set covers three i386 longs. */
    int high = dup2(p[0], 70);
    fd_set rd;
    FD_ZERO(&rd);
    FD_SET(high, &rd);
    struct timeval tv = {0, 20000};
    show("select empty", select(high + 1, &rd, NULL, NULL, &tv));
    printf("  time left %ld.%06ld, still set %d\n", (long)tv.tv_sec, (long)tv.tv_usec,
           FD_ISSET(high, &rd));
    if (write(p[1], "x", 1) != 1) {
        return;
    }
    FD_ZERO(&rd);
    FD_SET(high, &rd);
    FD_SET(p[0], &rd);
    fd_set wr;
    FD_ZERO(&wr);
    FD_SET(p[1], &wr);
    tv = (struct timeval){5, 0};
    show("select ready", select(high + 1, &rd, &wr, NULL, &tv));
    printf("  read %d %d write %d\n", FD_ISSET(p[0], &rd), FD_ISSET(high, &rd),
           FD_ISSET(p[1], &wr));
    /* Microseconds over a million carry into seconds. */
    tv = (struct timeval){0, 1500000};
    FD_ZERO(&rd);
    FD_SET(high, &rd);
    show("select carried usec", select(high + 1, &rd, NULL, NULL, &tv));
    tv = (struct timeval){-1, 0};
    show("select negative", select(high + 1, &rd, NULL, NULL, &tv));
    FD_ZERO(&rd);
    FD_SET(69, &rd);
    show("select bad descriptor", select(high + 1, &rd, NULL, NULL, &(struct timeval){0, 0}));

    struct timespec ts = {0, 0};
    FD_ZERO(&rd);
    FD_SET(high, &rd);
    show("pselect ready", pselect(high + 1, &rd, NULL, NULL, &ts, NULL));
    char c;
    if (read(p[0], &c, 1) != 1) {
        return;
    }
    FD_ZERO(&rd);
    FD_SET(high, &rd);
    ts = (struct timespec){0, 10000000};
    show("pselect empty", pselect(high + 1, &rd, NULL, NULL, &ts, NULL));
    ts = (struct timespec){0, 1000000000};
    show("pselect bad nanoseconds", pselect(high + 1, &rd, NULL, NULL, &ts, NULL));
    close(high);
    close(p[0]);
    close(p[1]);
}

static void polls(void) {
    int p[2];
    if (pipe(p) != 0) {
        return;
    }
    struct pollfd fds[3] = {{p[0], POLLIN, 0}, {p[1], POLLOUT, 0}, {99, POLLIN, 0}};
    show("poll", poll(fds, 3, 0));
    printf("  revents %#x %#x %#x\n", fds[0].revents, fds[1].revents, fds[2].revents);
    show("poll empty", poll(fds, 1, 10));
    if (write(p[1], "y", 1) != 1) {
        return;
    }
    struct timespec ts = {0, 10000000};
    show("ppoll ready", ppoll(fds, 1, &ts, NULL));
    printf("  revents %#x\n", fds[0].revents);
    show("ppoll no timeout", ppoll(fds, 1, NULL, NULL));
    char c;
    if (read(p[0], &c, 1) != 1) {
        return;
    }
    ts = (struct timespec){0, 10000000};
    show("ppoll empty", ppoll(fds, 1, &ts, NULL));
    printf("  time left %ld.%09ld\n", (long)ts.tv_sec, ts.tv_nsec);
    ts = (struct timespec){-1, 0};
    show("ppoll negative", ppoll(fds, 1, &ts, NULL));
    close(p[1]);
    show("poll hangup", poll(fds, 1, 0));
    printf("  revents %#x\n", fds[0].revents);
    close(p[0]);
}

static void epolls(void) {
    int p[2];
    if (pipe(p) != 0) {
        return;
    }
    show("epoll_create 0", epoll_create(0));
    int ep = epoll_create1(EPOLL_CLOEXEC);
    show("epoll_create1", ep >= 0 ? 0 : -1);
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 0x1122334455667788ULL};
    show("epoll_ctl add", epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &ev));
    show("epoll_ctl add again", epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &ev));
    struct epoll_event out[4];
    memset(out, 0, sizeof out);
    show("epoll_wait empty", epoll_wait(ep, out, 4, 10));
    if (write(p[1], "z", 1) != 1) {
        return;
    }
    show("epoll_wait", epoll_wait(ep, out, 4, 1000));
    printf("  events %#x data %#llx\n", out[0].events, (unsigned long long)out[0].data.u64);
    show("epoll_pwait", epoll_pwait(ep, out, 4, 0, NULL));
    show("epoll_wait no room", epoll_wait(ep, out, 0, 0));
    ev.data.fd = 7;
    ev.events = EPOLLIN | EPOLLOUT;
    show("epoll_ctl add write end", epoll_ctl(ep, EPOLL_CTL_ADD, p[1], &ev));
    memset(out, 0, sizeof out);
    int n = show("epoll_wait both", epoll_wait(ep, out, 4, 0));
    for (int i = 0; i < n; i++) {
        printf("  events %#x\n", out[i].events);
    }
    show("epoll_ctl del", epoll_ctl(ep, EPOLL_CTL_DEL, p[1], NULL));
    show("epoll_ctl del missing", epoll_ctl(ep, EPOLL_CTL_DEL, p[1], NULL));
    show("epoll_wait after del", epoll_wait(ep, out, 4, 0));
    close(ep);
    close(p[0]);
    close(p[1]);
}

static void eventfds(void) {
    int fd = eventfd(3, EFD_NONBLOCK | EFD_CLOEXEC);
    uint64_t value = 0;
    ssize_t got = read(fd, &value, 8);
    printf("eventfd read: %zd value %llu\n", got, (unsigned long long)value);
    show("eventfd read again", (int)read(fd, &value, 8));
    value = 5;
    printf("eventfd write: %zd\n", write(fd, &value, 8));
    struct pollfd pfd = {fd, POLLIN, 0};
    show("eventfd poll", poll(&pfd, 1, 0));
    close(fd);
    fd = eventfd(2, EFD_SEMAPHORE | EFD_NONBLOCK);
    for (int i = 0; i < 3; i++) {
        value = 0;
        ssize_t r = read(fd, &value, 8);
        printf("semaphore read: %zd value %llu %s\n", r, (unsigned long long)value,
               name(r < 0 ? errno : 0));
    }
    close(fd);
    show("eventfd bad flags", eventfd(0, 2));
}

int main(void) {
    selects();
    polls();
    epolls();
    eventfds();
    return 0;
}
