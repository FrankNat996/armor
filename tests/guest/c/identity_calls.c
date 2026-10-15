/*
 * The calls of ADR-0044 that the host kernel does for the guest, first
 * group: user and group ids (calls that change nothing, so the result is
 * the same for root and for a user), setgroups (allowed or refused by
 * privilege), personality (ADDR_NO_RANDOMIZE, PER_LINUX32 with uname),
 * readahead, mlock2, kcmp, the pidfd calls, process_mrelease, unshare,
 * setns, vhangup. Prints no ids, pids or descriptors.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <unistd.h>

static void show(const char *what, long r) {
    if (r < 0) {
        printf("%-28s -1 %s\n", what, strerror(errno));
    } else {
        printf("%-28s %ld\n", what, r);
    }
}

/* 0 for success, else the errno: for calls whose result is a descriptor. */
static void ok(const char *what, long r) {
    printf("%-28s %s\n", what, r >= 0 ? "ok" : strerror(errno));
}

int main(void) {
    show("setresuid(-1,-1,-1)", syscall(SYS_setresuid, -1, -1, -1));
    show("setresgid(-1,-1,-1)", syscall(SYS_setresgid, -1, -1, -1));
    show("setreuid(-1,-1)", syscall(SYS_setreuid, -1, -1));
    show("setregid(-1,-1)", syscall(SYS_setregid, -1, -1));
    printf("%-28s %d\n", "setfsuid(-1) is euid", (uid_t)syscall(SYS_setfsuid, -1) == geteuid());
    printf("%-28s %d\n", "setfsgid(-1) is egid", (gid_t)syscall(SYS_setfsgid, -1) == getegid());
    gid_t groups[256];
    int n = getgroups(256, groups);
    long r = syscall(SYS_setgroups, n, groups);
    printf("%-28s %s\n", "setgroups(current)", r == 0 || errno == EPERM ? "0 or EPERM" : strerror(errno));

    unsigned long persona = syscall(SYS_personality, 0xffffffffUL);
    printf("%-28s %#lx\n", "personality", persona & 0xff);
    show("personality(+NORANDOM)", syscall(SYS_personality, persona | 0x0040000) & ~0x0040000L);
    printf("%-28s %#lx\n", "personality now", syscall(SYS_personality, 0xffffffffUL) & 0x0040000);
    syscall(SYS_personality, 0x0008UL);
    struct utsname names;
    uname(&names);
    printf("%-28s %s\n", "uname under PER_LINUX32", names.machine);
    syscall(SYS_personality, persona);
    uname(&names);
    printf("%-28s %s\n", "uname after", names.machine);

    char path[] = "/tmp/armor-ids-XXXXXX";
    int fd = mkstemp(path);
    write(fd, "readahead\n", 10);
    show("readahead", syscall(SYS_readahead, fd, 0L, 10L));
    static char page[4096] __attribute__((aligned(4096)));
    show("mlock2", syscall(SYS_mlock2, page, sizeof page, 0));
    show("munlock", munlock(page, sizeof page));
    show("mlock2 bad flags", syscall(SYS_mlock2, page, sizeof page, 8));
    int same = dup(fd);
    show("kcmp same file", syscall(SYS_kcmp, getpid(), getpid(), 0 /* KCMP_FILE */, fd, same));
    show("kcmp bad type", syscall(SYS_kcmp, getpid(), getpid(), 99, fd, same));

    int pidfd = syscall(SYS_pidfd_open, getpid(), 0);
    ok("pidfd_open", pidfd);
    show("pidfd_send_signal 0", syscall(SYS_pidfd_send_signal, pidfd, 0, NULL, 0));
    int copy = syscall(SYS_pidfd_getfd, pidfd, fd, 0);
    ok("pidfd_getfd", copy);
    show("process_mrelease alive", syscall(SYS_process_mrelease, pidfd, 0));
    show("pidfd_open bad flags", syscall(SYS_pidfd_open, getpid(), 1));

    show("unshare(0)", syscall(SYS_unshare, 0));
    show("setns(-1)", syscall(SYS_setns, -1, 0));
    show("vhangup", syscall(SYS_vhangup));
    unlink(path);
    return 0;
}
