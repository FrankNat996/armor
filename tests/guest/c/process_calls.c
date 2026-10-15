/*
 * The process, message and wait calls of ADR-0044: sendmmsg and recvmmsg
 * (MSG_WAITFORONE, nothing pending), execveat of the program's own file
 * through a descriptor (fexecve's way) and its errors, openat2 (resolve
 * flags, sizes), process_vm_readv/writev on this process, sched_getattr
 * and sched_setattr, futex_waitv, futex_wait and futex_wake. Prints no
 * pids, descriptors or times.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/futex.h>
#include <linux/openat2.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void show(const char *what, long r) {
    if (r < 0) {
        printf("%-30s -1 %s\n", what, strerror(errno));
    } else {
        printf("%-30s %ld\n", what, r);
    }
}

struct waitv {
    uint64_t val;
    uint64_t uaddr;
    uint32_t flags;
    uint32_t reserved;
};

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "child") == 0) {
        printf("child ran through execveat\n");
        return 0;
    }
    int s[2];
    socketpair(AF_UNIX, SOCK_DGRAM, 0, s);
    char one[] = "one", two[] = "second", three[] = "3";
    struct iovec iov[3] = {{one, 3}, {two, 6}, {three, 1}};
    struct mmsghdr out[3];
    memset(out, 0, sizeof out);
    for (int i = 0; i < 3; i++) {
        out[i].msg_hdr.msg_iov = &iov[i];
        out[i].msg_hdr.msg_iovlen = 1;
    }
    show("sendmmsg", sendmmsg(s[0], out, 3, 0));
    printf("%-30s %u %u %u\n", "lengths sent", out[0].msg_len, out[1].msg_len, out[2].msg_len);
    char bufs[4][16];
    struct iovec in_iov[4];
    struct mmsghdr in[4];
    memset(in, 0, sizeof in);
    for (int i = 0; i < 4; i++) {
        in_iov[i].iov_base = bufs[i];
        in_iov[i].iov_len = sizeof bufs[i];
        in[i].msg_hdr.msg_iov = &in_iov[i];
        in[i].msg_hdr.msg_iovlen = 1;
    }
    show("recvmmsg WAITFORONE", recvmmsg(s[1], in, 4, MSG_WAITFORONE, NULL));
    printf("%-30s %u %.6s %u\n", "received", in[0].msg_len, bufs[1], in[2].msg_len);
    struct timespec t = {0, 1000000};
    show("recvmmsg nothing", recvmmsg(s[1], in, 4, MSG_DONTWAIT, &t));

    pid_t child = fork();
    if (child == 0) {
        int self = open("/proc/self/exe", O_RDONLY | O_CLOEXEC);
        char *args[] = {argv[0], "child", NULL};
        syscall(SYS_execveat, self, "", args, environ, AT_EMPTY_PATH);
        printf("execveat failed: %s\n", strerror(errno));
        _exit(1);
    }
    int status;
    waitpid(child, &status, 0);
    printf("%-30s %d\n", "child exit", WEXITSTATUS(status));
    char *none[] = {NULL};
    show("execveat empty path", syscall(SYS_execveat, AT_FDCWD, "", none, none, 0));
    show("execveat bad flags", syscall(SYS_execveat, AT_FDCWD, "/", none, none, 1));

    char name[] = "armor-openat2-XXXXXX";
    close(mkstemp(name));
    symlink(name, "armor-openat2-link");
    struct open_how how = {.flags = O_RDONLY | O_CLOEXEC, .resolve = RESOLVE_NO_SYMLINKS};
    long fd = syscall(SYS_openat2, AT_FDCWD, name, &how, sizeof how);
    printf("%-30s %s\n", "openat2", fd >= 0 ? "ok" : strerror(errno));
    show("openat2 symlink", syscall(SYS_openat2, AT_FDCWD, "armor-openat2-link", &how, sizeof how));
    show("openat2 size 8", syscall(SYS_openat2, AT_FDCWD, name, &how, 8));
    char bigger[40] = {0};
    memcpy(bigger, &how, sizeof how);
    bigger[30] = 1;
    show("openat2 nonzero tail", syscall(SYS_openat2, AT_FDCWD, name, bigger, sizeof bigger));
    struct open_how beneath = {.flags = O_RDONLY, .resolve = RESOLVE_BENEATH};
    show("openat2 beneath ..", syscall(SYS_openat2, AT_FDCWD, "../x", &beneath, sizeof beneath));
    unlink("armor-openat2-link");
    unlink(name);

    char from[] = "process_vm", to[16] = {0};
    struct iovec local = {to, 10}, remote = {from, 10};
    show("process_vm_readv", process_vm_readv(getpid(), &local, 1, &remote, 1, 0));
    printf("%-30s %s\n", "read", to);
    char new_text[] = "PROCESS_VM";
    struct iovec src = {new_text, 10}, dst = {from, 10};
    show("process_vm_writev", process_vm_writev(getpid(), &src, 1, &dst, 1, 0));
    printf("%-30s %s\n", "now", from);
    show("process_vm_readv flags", process_vm_readv(getpid(), &local, 1, &remote, 1, 1));

    unsigned char attr[56] = {0};
    show("sched_getattr", syscall(SYS_sched_getattr, 0, attr, sizeof attr, 0));
    printf("%-30s %u %u\n", "size policy", *(uint32_t *)attr, *(uint32_t *)(attr + 4));
    show("sched_setattr same", syscall(SYS_sched_setattr, 0, attr, 0));
    show("sched_getattr small", syscall(SYS_sched_getattr, 0, attr, 8, 0));

    static uint32_t word = 5;
    struct waitv w = {.val = 6, .uaddr = (uintptr_t)&word, .flags = 2 /* FUTEX2_SIZE_U32 */ | 128};
    show("futex_waitv changed", syscall(SYS_futex_waitv, &w, 1, 0, NULL, CLOCK_MONOTONIC));
    w.val = 5;
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    now.tv_nsec += 2000000;
    if (now.tv_nsec >= 1000000000) {
        now.tv_sec++;
        now.tv_nsec -= 1000000000;
    }
    show("futex_waitv timeout", syscall(SYS_futex_waitv, &w, 1, 0, &now, CLOCK_MONOTONIC));
    show("futex_waitv none", syscall(SYS_futex_waitv, &w, 0, 0, NULL, CLOCK_MONOTONIC));
    show("futex_wake", syscall(454, &word, ~0UL, 1, 2 | 128));
    show("futex_wait changed", syscall(455, &word, 6, ~0UL, 2 | 128, NULL, CLOCK_MONOTONIC));
    return 0;
}
