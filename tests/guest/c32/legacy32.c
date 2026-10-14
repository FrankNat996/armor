/*
 * Old i386 calls the Steam client's crash handler makes, by number:
 * sigprocmask with the 32-bit old_sigset_t (SIG_SETMASK keeps the
 * blocked signals above 32; a bad `how` is EINVAL), and prctl
 * PR_GET_DUMPABLE / PR_SET_DUMPABLE. qemu-i386 clears the signals above
 * 32 on SIG_SETMASK: compared with the hardware only (docs/testing.md).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

static uint64_t rt_mask(void) {
    uint64_t mask = 0;
    syscall(SYS_rt_sigprocmask, SIG_BLOCK, NULL, &mask, 8);
    return mask;
}

int main(void) {
    uint32_t set = 1u << (SIGUSR1 - 1), old = 0;
    printf("block: %ld\n", syscall(SYS_sigprocmask, SIG_BLOCK, &set, &old));
    printf("  old %#x now %#llx\n", old, (unsigned long long)rt_mask());
    /* Signal 40 blocked through rt_sigprocmask stays blocked. */
    uint64_t high = 1ull << (40 - 1);
    syscall(SYS_rt_sigprocmask, SIG_BLOCK, &high, NULL, 8);
    set = 1u << (SIGUSR2 - 1);
    printf("setmask: %ld\n", syscall(SYS_sigprocmask, SIG_SETMASK, &set, &old));
    printf("  old %#x now %#llx\n", old, (unsigned long long)rt_mask());
    printf("unblock: %ld\n", syscall(SYS_sigprocmask, SIG_UNBLOCK, &set, NULL));
    printf("  now %#llx\n", (unsigned long long)rt_mask());
    long r = syscall(SYS_sigprocmask, 7, &set, &old);
    printf("bad how: %ld %s\n", r, r < 0 && errno == EINVAL ? "EINVAL" : "other");
    old = 0xdead;
    r = syscall(SYS_sigprocmask, 7, NULL, &old);
    printf("query only: %ld old %#x\n", r, old);

    printf("dumpable: %d\n", prctl(PR_GET_DUMPABLE));
    printf("set 0: %d\n", prctl(PR_SET_DUMPABLE, 0));
    printf("dumpable: %d\n", prctl(PR_GET_DUMPABLE));
    printf("set 1: %d\n", prctl(PR_SET_DUMPABLE, 1));
    printf("dumpable: %d\n", prctl(PR_GET_DUMPABLE));
    r = prctl(PR_SET_DUMPABLE, 5);
    printf("set 5: %ld %s\n", r, r < 0 && errno == EINVAL ? "EINVAL" : "other");
    return 0;
}
