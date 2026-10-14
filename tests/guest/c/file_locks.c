/*
 * flock between processes (exclusive, shared, LOCK_NB, unlock, the lock
 * released when the holder exits) and the FIONBIO and FIONREAD ioctls on
 * a pipe and a socket pair, and record locks (F_SETLK, F_GETLK, F_SETLKW)
 * between processes. The Steam client locks its files with flock and its
 * sockets with FIONBIO; Python programs use record locks. The same source
 * runs as x86-64 and i386 (whose struct flock differs).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/file.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EWOULDBLOCK: return "EWOULDBLOCK";
    case EINVAL: return "EINVAL";
    case EBADF: return "EBADF";
    case ENOTTY: return "ENOTTY";
    default: return "other";
    }
}

static int show(const char *what, int result) {
    printf("%s: %d %s\n", what, result, name(result < 0 ? errno : 0));
    return result;
}

/* flock on a fresh descriptor of the same file, in a child. */
static int try_in_child(const char *path, int operation) {
    pid_t child = fork();
    if (child == 0) {
        int fd = open(path, O_RDONLY);
        _exit(flock(fd, operation) == 0 ? 0 : (errno == EWOULDBLOCK ? 1 : 2));
    }
    int status = 0;
    waitpid(child, &status, 0);
    return WEXITSTATUS(status);
}

int main(void) {
    char path[] = "/tmp/armor-flock-XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) {
        perror("mkstemp");
        return 1;
    }
    show("lock exclusive", flock(fd, LOCK_EX));
    printf("child exclusive nb: %d\n", try_in_child(path, LOCK_EX | LOCK_NB));
    printf("child shared nb: %d\n", try_in_child(path, LOCK_SH | LOCK_NB));
    show("downgrade to shared", flock(fd, LOCK_SH));
    printf("child shared nb: %d\n", try_in_child(path, LOCK_SH | LOCK_NB));
    printf("child exclusive nb: %d\n", try_in_child(path, LOCK_EX | LOCK_NB));
    show("unlock", flock(fd, LOCK_UN));
    printf("child exclusive nb: %d\n", try_in_child(path, LOCK_EX | LOCK_NB));
    show("bad operation", flock(fd, 0));
    show("bad descriptor", flock(-1, LOCK_EX));

    /* A child takes the lock and exits: the lock goes with it. */
    pid_t child = fork();
    if (child == 0) {
        int own = open(path, O_RDONLY);
        flock(own, LOCK_EX);
        _exit(0);
    }
    waitpid(child, NULL, 0);
    show("after the holder exited", flock(fd, LOCK_EX | LOCK_NB));
    /* Record locks: bytes 10..19 write-locked here; a child sees the
     * lock with F_GETLK (and who holds it) and cannot take it. */
    struct flock fl = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 10, .l_len = 10};
    show("setlk write", fcntl(fd, F_SETLK, &fl));
    pid_t me = getpid();
    pid_t probe = fork();
    if (probe == 0) {
        int own = open(path, O_RDWR);
        struct flock q = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 15, .l_len = 1};
        int r = fcntl(own, F_GETLK, &q);
        int seen = r == 0 && q.l_type == F_WRLCK && q.l_start == 10 && q.l_len == 10 && q.l_pid == me;
        struct flock take = {.l_type = F_RDLCK, .l_whence = SEEK_SET, .l_start = 12, .l_len = 2};
        int blocked = fcntl(own, F_SETLK, &take) < 0 && (errno == EAGAIN || errno == EACCES);
        struct flock other = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 30, .l_len = 5};
        int free_range = fcntl(own, F_SETLK, &other) == 0;
        _exit(seen | blocked << 1 | free_range << 2);
    }
    int status = 0;
    waitpid(probe, &status, 0);
    printf("record lock seen, blocked, other range free: %d\n", WEXITSTATUS(status));
    fl.l_type = F_UNLCK;
    show("setlk unlock", fcntl(fd, F_SETLK, &fl));
    struct flock after = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    show("getlk after unlock", fcntl(fd, F_GETLK, &after));
    printf("  unlocked: %d\n", after.l_type == F_UNLCK);
    fl.l_type = F_WRLCK;
    show("setlkw", fcntl(fd, F_SETLKW, &fl));
    close(fd);
    unlink(path);

    int p[2];
    if (pipe(p) != 0) {
        return 1;
    }
    int on = 1;
    show("fionbio on", ioctl(p[0], FIONBIO, &on));
    char c;
    printf("read empty: %zd %s\n", read(p[0], &c, 1), name(errno));
    printf("flags nonblock: %d\n", (fcntl(p[0], F_GETFL) & O_NONBLOCK) != 0);
    int off = 0;
    show("fionbio off", ioctl(p[0], FIONBIO, &off));
    printf("flags nonblock: %d\n", (fcntl(p[0], F_GETFL) & O_NONBLOCK) != 0);
    if (write(p[1], "hello", 5) != 5) {
        return 1;
    }
    pid_t group = 0;
    show("tiocgpgrp on a pipe", ioctl(p[0], TIOCGPGRP, &group));
    show("tiocspgrp on a pipe", ioctl(p[0], TIOCSPGRP, &group));
    int pending = -1;
    show("fionread pipe", ioctl(p[0], FIONREAD, &pending));
    printf("  pending %d\n", pending);

    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        return 1;
    }
    if (write(sv[0], "abcdefgh", 8) != 8) {
        return 1;
    }
    pending = -1;
    show("fionread socket", ioctl(sv[1], FIONREAD, &pending));
    printf("  pending %d\n", pending);
    show("fionbio socket", ioctl(sv[1], FIONBIO, &on));
    char buf[16];
    printf("recv: %zd\n", recv(sv[1], buf, sizeof buf, 0));
    printf("recv empty: %zd %s\n", recv(sv[1], buf, sizeof buf, 0), name(errno));
    return 0;
}
