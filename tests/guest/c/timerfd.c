/*
 * timerfd (Chromium's message loops use one, under a Spotify build): a
 * monotonic timer armed for 20 ms reads 1 expiration; gettime of a
 * disarmed timer is zero; settime returns the old value; a non-blocking
 * read of an unexpired timer is EAGAIN; bad clock and flags are EINVAL;
 * TFD_CLOEXEC sets FD_CLOEXEC.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/timerfd.h>
#include <unistd.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EINVAL: return "EINVAL";
    case EAGAIN: return "EAGAIN";
    case EFAULT: return "EFAULT";
    default: return "other";
    }
}

int main(void) {
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC);
    printf("create: %d, cloexec %d\n", fd >= 0, (fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0);
    struct itimerspec value = {{0, 0}, {0, 20 * 1000 * 1000}}, old;
    printf("settime: %d\n", timerfd_settime(fd, 0, &value, &old));
    printf("  old value zero: %d\n", old.it_value.tv_sec == 0 && old.it_value.tv_nsec == 0);
    struct itimerspec now;
    timerfd_gettime(fd, &now);
    printf("  remaining positive and below 20 ms: %d\n",
           now.it_value.tv_sec == 0 && now.it_value.tv_nsec > 0 && now.it_value.tv_nsec <= 20000000);
    uint64_t expirations = 0;
    ssize_t n = read(fd, &expirations, sizeof expirations);
    printf("read: %zd, expirations %llu\n", n, (unsigned long long)expirations);
    timerfd_gettime(fd, &now);
    printf("disarmed: %d\n", now.it_value.tv_sec == 0 && now.it_value.tv_nsec == 0);

    int nb = timerfd_create(CLOCK_REALTIME, TFD_NONBLOCK);
    struct itimerspec far = {{0, 0}, {100, 0}};
    timerfd_settime(nb, 0, &far, NULL);
    n = read(nb, &expirations, sizeof expirations);
    printf("non-blocking read: %zd %s\n", n, name(n < 0 ? errno : 0));
    struct itimerspec bad = {{0, 0}, {0, 2000000000}};
    int r = timerfd_settime(nb, 0, &bad, NULL);
    printf("settime bad nanoseconds: %d %s\n", r, name(r < 0 ? errno : 0));
    r = timerfd_create(1234, 0);
    printf("create bad clock: %d %s\n", r, name(r < 0 ? errno : 0));
    r = timerfd_create(CLOCK_MONOTONIC, 0x40);
    printf("create bad flags: %d %s\n", r, name(r < 0 ? errno : 0));
    r = timerfd_gettime(nb, (struct itimerspec *)8);
    printf("gettime bad pointer: %d %s\n", r, name(r < 0 ? errno : 0));
    return 0;
}
