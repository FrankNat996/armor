/*
 * Calls glibc itself seldom makes but programs do (Chromium, under a
 * Spotify build): sched_yield; nanosleep; getrlimit and setrlimit;
 * getitimer; waitid for an exited child; readv from a pipe; preadv,
 * pwritev and preadv2 at offsets of a file; readv into a read-only buffer
 * (EFAULT) and with a negative length (EINVAL).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sched.h>
#include <signal.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/time.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EINVAL: return "EINVAL";
    case EFAULT: return "EFAULT";
    case EPERM: return "EPERM";
    default: return "other";
    }
}

static void show(const char *what, long r) {
    printf("%s: %ld %s\n", what, r < 0 ? -1L : r, name(r < 0 ? errno : 0));
}

int main(void) {
    show("sched_yield", sched_yield());
    struct timespec nap = {0, 1000000}, left = {9, 9};
    show("nanosleep", nanosleep(&nap, &left));
    struct timespec bad = {0, 2000000000};
    show("nanosleep bad", nanosleep(&bad, NULL));

    struct rlimit files;
    show("getrlimit", getrlimit(RLIMIT_NOFILE, &files));
    struct rlimit lower = {files.rlim_cur > 64 ? 64 : files.rlim_cur, files.rlim_max};
    show("setrlimit lower soft", setrlimit(RLIMIT_NOFILE, &lower));
    struct rlimit back;
    getrlimit(RLIMIT_NOFILE, &back);
    printf("  soft now %llu\n", (unsigned long long)back.rlim_cur);
    show("getrlimit bad resource", getrlimit(99, &back));
    struct itimerval timer;
    show("getitimer", getitimer(ITIMER_REAL, &timer));
    printf("  disarmed %d\n", timer.it_value.tv_sec == 0 && timer.it_value.tv_usec == 0);

    pid_t child = fork();
    if (child == 0) {
        _exit(7);
    }
    siginfo_t info;
    memset(&info, 0, sizeof info);
    show("waitid", waitid(P_PID, child, &info, WEXITED));
    printf("  signo %d code %d status %d same pid %d\n", info.si_signo, info.si_code,
           info.si_status, info.si_pid == child);

    int p[2];
    if (pipe(p) != 0 || write(p[1], "abcdefgh", 8) != 8) {
        return 1;
    }
    char a[3] = {0}, b[6] = {0};
    struct iovec two[2] = {{a, 2}, {b, 5}};
    show("readv", readv(p[0], two, 2));
    printf("  %s %s\n", a, b);

    char path[] = "/tmp/armor-iov-XXXXXX";
    int fd = mkstemp(path);
    struct iovec out[2] = {{"hello ", 6}, {"world", 5}};
    show("pwritev at 4", pwritev(fd, out, 2, 4));
    char c[5] = {0}, d[7] = {0};
    struct iovec in[2] = {{c, 4}, {d, 6}};
    show("preadv at 5", preadv(fd, in, 2, 5));
    printf("  %s|%s\n", c, d);
    lseek(fd, 4, SEEK_SET);
    memset(c, 0, sizeof c);
    struct iovec one[1] = {{c, 4}};
    show("preadv2 at -1", preadv2(fd, one, 1, -1, 0));
    printf("  %s, position %ld\n", c, (long)lseek(fd, 0, SEEK_CUR));
    close(fd);
    unlink(path);

    char *ro = mmap(NULL, 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    write(p[1], "xyz", 3);
    struct iovec bad_buf[1] = {{ro, 3}};
    show("readv into read-only", readv(p[0], bad_buf, 1));
    struct iovec negative[1] = {{a, (size_t)-1}};
    show("readv negative length", readv(p[0], negative, 1));
    return 0;
}
