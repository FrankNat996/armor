/*
 * close_range(2): a range closed, a range marked close-on-exec, the
 * errors, and a child that closes every descriptor above 2 and goes on
 * (Spotify's child does it before execve, to open the browser). The
 * number is 436 on x86-64 and i386 alike.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define CLOSE_RANGE_NR 436
#define CLOSE_RANGE_CLOEXEC (1U << 2)

static long close_range_(unsigned first, unsigned last, unsigned flags) {
    return syscall(CLOSE_RANGE_NR, first, last, flags);
}

static void show(const char *what) {
    printf("%s:", what);
    for (int fd = 10; fd <= 16; fd++) {
        int flags = fcntl(fd, F_GETFD);
        printf(" %d=%s", fd, flags < 0 ? "closed" : (flags & FD_CLOEXEC) ? "cloexec" : "open");
    }
    printf("\n");
}

int main(void) {
    for (int fd = 10; fd <= 16; fd++) {
        dup2(1, fd);
    }
    show("start");
    printf("close 11..13: %ld\n", close_range_(11, 13, 0));
    show("after");
    printf("cloexec 15..~0: %ld\n", close_range_(15, ~0U, CLOSE_RANGE_CLOEXEC));
    show("after");
    long r = close_range_(14, 12, 0);
    printf("first > last: %ld %s\n", r, r < 0 ? strerror(errno) : "");
    r = close_range_(10, 10, 1U << 7);
    printf("unknown flag: %ld %s\n", r, r < 0 ? strerror(errno) : "");
    fflush(stdout);

    pid_t child = fork();
    if (child == 0) {
        long c = close_range_(3, ~0U, 0);
        int still = 0;
        for (int fd = 3; fd <= 16; fd++) {
            still += fcntl(fd, F_GETFD) >= 0;
        }
        char line[64];
        int n = snprintf(line, sizeof line, "child: %ld, %d open above 2\n", c, still);
        write(1, line, n);
        _exit(0);
    }
    int status;
    waitpid(child, &status, 0);
    printf("child exit %d\n", WIFEXITED(status) ? WEXITSTATUS(status) : -1);
    show("parent");
    return 0;
}
