/*
 * Probe for `cargo xtask sandbox-check` (ADR-0027, docs/tech/security.md).
 * Built twice from this source: for x86-64, run as a guest under armor,
 * and for AArch64, run natively inside the same sandbox, where it stands
 * for armor already taken over by the guest. Each run does one check and
 * prints "ok" or the errno name; xtask compares with what the sandbox
 * must allow.
 *
 *   probe read PATH | write PATH | connect PORT | kill PID | processes
 *   probe ptrace | mount | unshare | clone-namespace | open-files N
 *   probe allocate MB | write-size BYTES | fork N | spin SECONDS
 *   probe capabilities
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/ptrace.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int result(int ok) {
    if (ok) {
        puts("ok");
    } else {
        const char *name = strerrorname_np(errno);
        puts(name ? name : "unknown");
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        return 2;
    }
    const char *check = argv[1];
    if (!strcmp(check, "read") && argc == 3) {
        int fd = open(argv[2], O_RDONLY);
        return result(fd >= 0);
    }
    if (!strcmp(check, "write") && argc == 3) {
        int fd = open(argv[2], O_WRONLY | O_CREAT | O_TRUNC, 0600);
        return result(fd >= 0 && write(fd, "x", 1) == 1);
    }
    if (!strcmp(check, "connect") && argc == 3) {
        int s = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in addr = {.sin_family = AF_INET, .sin_port = htons(atoi(argv[2]))};
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        return result(s >= 0 && connect(s, (struct sockaddr *)&addr, sizeof addr) == 0);
    }
    if (!strcmp(check, "kill") && argc == 3) {
        return result(kill(atoi(argv[2]), 0) == 0);
    }
    if (!strcmp(check, "processes")) {
        /* The processes this one can see, by their /proc entries. */
        DIR *dir = opendir("/proc");
        int count = 0;
        struct dirent *e;
        while (dir && (e = readdir(dir))) {
            count += e->d_name[0] >= '1' && e->d_name[0] <= '9';
        }
        printf("%d\n", count);
        return 0;
    }
    if (!strcmp(check, "ptrace")) {
        return result(ptrace(PTRACE_ATTACH, getppid(), 0, 0) == 0);
    }
    if (!strcmp(check, "mount")) {
        return result(mount("none", "/tmp", "tmpfs", 0, 0) == 0);
    }
    if (!strcmp(check, "unshare")) {
        return result(unshare(CLONE_NEWUSER) == 0);
    }
    if (!strcmp(check, "clone-namespace")) {
        /* A child in a new user namespace, through clone rather than
         * unshare. */
        long pid = syscall(SYS_clone, CLONE_NEWUSER | SIGCHLD, 0, 0, 0, 0);
        if (pid == 0) {
            _exit(0);
        }
        return result(pid > 0);
    }
    if (!strcmp(check, "write-size") && argc == 3) {
        /* SIGXFSZ ignored: past the file size limit, write fails with
         * EFBIG instead of killing the probe. */
        signal(SIGXFSZ, SIG_IGN);
        static char block[1024];
        int fd = open("big", O_WRONLY | O_CREAT | O_TRUNC, 0600);
        long total = atol(argv[2]);
        for (long done = 0; fd >= 0 && done < total; done += sizeof block) {
            if (write(fd, block, sizeof block) != sizeof block) {
                unlink("big");
                return result(0);
            }
        }
        unlink("big");
        return result(fd >= 0);
    }
    if (!strcmp(check, "fork") && argc == 3) {
        /* Children that wait, until fork fails or N of them run. */
        int n = atoi(argv[2]), made = 0, ok = 1;
        pid_t children[256];
        for (; made < n && made < 256; made++) {
            pid_t pid = fork();
            if (pid == 0) {
                pause();
                _exit(0);
            }
            if (pid < 0) {
                ok = 0;
                break;
            }
            children[made] = pid;
        }
        int saved = errno;
        for (int i = 0; i < made; i++) {
            kill(children[i], SIGKILL);
            waitpid(children[i], 0, 0);
        }
        errno = saved;
        return result(ok);
    }
    if (!strcmp(check, "spin") && argc == 3) {
        /* CPU time, not wall time: killed by SIGXCPU past the limit. */
        double seconds = atof(argv[2]);
        volatile unsigned long x = 0;
        while ((double)clock() / CLOCKS_PER_SEC < seconds) {
            x++;
        }
        return result(1);
    }
    if (!strcmp(check, "capabilities")) {
        FILE *f = fopen("/proc/self/status", "r");
        char line[256];
        while (f && fgets(line, sizeof line, f)) {
            if (!strncmp(line, "CapEff:", 7)) {
                printf("%s", line + 8);
                return 0;
            }
        }
        return result(0);
    }
    if (!strcmp(check, "allocate") && argc == 3) {
        size_t len = (size_t)atoi(argv[2]) << 20;
        void *p = mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        return result(p != MAP_FAILED);
    }
    if (!strcmp(check, "open-files") && argc == 3) {
        int n = atoi(argv[2]);
        for (int i = 0; i < n; i++) {
            if (open("/dev/null", O_RDONLY) < 0) {
                return result(0);
            }
        }
        return result(1);
    }
    return 2;
}
