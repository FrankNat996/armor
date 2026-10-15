/*
 * seccomp (ADR-0044), each case in a child since filters stay: the mode
 * through prctl, the action and size queries, a filter refused without
 * no_new_privs only for unprivileged callers (so set first), an invalid
 * program; then filters on the x86-64 numbers and AUDIT_ARCH_X86_64:
 * SECCOMP_RET_ERRNO, SECCOMP_RET_TRAP (SIGSYS with si_code, si_errno,
 * si_syscall, si_arch, the number in rax, and the handler's result),
 * SECCOMP_RET_KILL_PROCESS, the strictest of two filters, TSYNC to
 * another thread, and a filter that survives execve. Strict mode, which a
 * container with its own filter refuses, is checked by the run_paths
 * test of armor (crates/armor/tests/run_paths.rs).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <pthread.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

static void show(const char *what, long r) {
    if (r < 0) {
        printf("%-28s -1 %s\n", what, strerror(errno));
    } else {
        printf("%-28s %ld\n", what, r);
    }
    fflush(stdout);
}

/* A filter: on x86-64, syscall `nr` gets `action`; everything else runs. */
static long install(int nr, unsigned action, unsigned flags) {
    struct sock_filter code[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, nr, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, action),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = {sizeof code / sizeof code[0], code};
    return syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, flags, &prog);
}

static void on_sigsys(int signo, siginfo_t *info, void *context) {
    ucontext_t *uc = context;
    printf("SIGSYS %d code %d errno %d syscall %d arch %#x rax %lld\n", signo, info->si_code,
           info->si_errno, info->si_syscall, info->si_arch, (long long)uc->uc_mcontext.gregs[REG_RAX]);
    uc->uc_mcontext.gregs[REG_RAX] = 42;
}

static void *thread_getpid(void *arg) {
    (void)arg;
    show("thread getpid", syscall(SYS_getpid) > 0 ? 0 : -1);
    return NULL;
}

static void wait_child(const char *what, pid_t child) {
    int status;
    waitpid(child, &status, 0);
    if (WIFSIGNALED(status)) {
        printf("%-28s killed by %d\n", what, WTERMSIG(status));
    } else {
        printf("%-28s exit %d\n", what, WEXITSTATUS(status));
    }
    fflush(stdout);
}

int main(int argc, char **argv) {
    if (argc > 1) {
        /* Started by execve under the filter of the last case. */
        show("after execve getppid", syscall(SYS_getppid) > 0 ? 0 : -1);
        show("after execve mode", prctl(PR_GET_SECCOMP));
        return 0;
    }
    /* 0, or 2 where the test runs inside a container that has a filter. */
    long mode = prctl(PR_GET_SECCOMP);
    printf("%-28s %s\n", "mode at start", mode == 0 || mode == 2 ? "0 or 2" : strerror(errno));
    unsigned action = SECCOMP_RET_LOG;
    show("action LOG available", syscall(SYS_seccomp, SECCOMP_GET_ACTION_AVAIL, 0, &action));
    action = 0x12340000;
    show("action unknown", syscall(SYS_seccomp, SECCOMP_GET_ACTION_AVAIL, 0, &action));
    struct seccomp_notif_sizes sizes;
    show("notif sizes", syscall(SYS_seccomp, SECCOMP_GET_NOTIF_SIZES, 0, &sizes));
    printf("%-28s %u %u %u\n", "sizes", sizes.seccomp_notif, sizes.seccomp_notif_resp, sizes.seccomp_data);
    show("bad operation", syscall(SYS_seccomp, 99, 0, NULL));
    prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    struct sock_filter bad[] = {BPF_STMT(BPF_LD | BPF_W | BPF_ABS, 64)};
    struct sock_fprog bad_prog = {1, bad};
    show("invalid program", syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &bad_prog));
    show("unknown flag", install(SYS_getpid, SECCOMP_RET_ALLOW, 1u << 20));

    pid_t child = fork();
    if (child == 0) {
        show("install errno", install(SYS_getpid, SECCOMP_RET_ERRNO | EPERM, 0));
        show("getpid", syscall(SYS_getpid));
        show("mode now", prctl(PR_GET_SECCOMP));
        show("strict after filter", prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT));
        /* The strictest of two filters wins. */
        install(SYS_getpid, SECCOMP_RET_ERRNO | EACCES, 0);
        show("getpid two filters", syscall(SYS_getpid));
        _exit(0);
    }
    wait_child("errno child", child);

    child = fork();
    if (child == 0) {
        struct sigaction sa = {.sa_sigaction = on_sigsys, .sa_flags = SA_SIGINFO};
        sigaction(SIGSYS, &sa, NULL);
        show("install trap", install(SYS_getuid, SECCOMP_RET_TRAP | 7, 0));
        show("getuid trapped", syscall(SYS_getuid));
        _exit(0);
    }
    wait_child("trap child", child);

    child = fork();
    if (child == 0) {
        install(SYS_getppid, SECCOMP_RET_KILL_PROCESS, 0);
        syscall(SYS_getppid);
        _exit(0);
    }
    wait_child("kill child", child);

    child = fork();
    if (child == 0) {
        install(SYS_getuid, SECCOMP_RET_TRAP, 0);
        sigset_t set;
        sigemptyset(&set);
        sigaddset(&set, SIGSYS);
        sigprocmask(SIG_BLOCK, &set, NULL);
        syscall(SYS_getuid);
        _exit(0);
    }
    wait_child("trap with SIGSYS blocked", child);


    child = fork();
    if (child == 0) {
        pthread_t t;
        show("install tsync", install(SYS_getpid, SECCOMP_RET_ERRNO | EPERM, SECCOMP_FILTER_FLAG_TSYNC));
        pthread_create(&t, NULL, thread_getpid, NULL);
        pthread_join(t, NULL);
        _exit(0);
    }
    wait_child("tsync child", child);

    child = fork();
    if (child == 0) {
        install(SYS_getppid, SECCOMP_RET_ERRNO | EPERM, 0);
        char *args[] = {argv[0], "exec", NULL};
        execv("/proc/self/exe", args);
        _exit(1);
    }
    wait_child("execve child", child);
    return 0;
}
