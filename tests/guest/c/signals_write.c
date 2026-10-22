/*
 * A blocking write interrupted by a signal: to a full pipe, a timer's
 * SIGALRM without SA_RESTART (write returns EINTR) and with it (write
 * restarts, and succeeds once the handler made room). Also the simple
 * syscalls armor's translated code makes itself (getpid, getppid,
 * gettid, the ids, lseek, sched_yield, write, clock_gettime,
 * gettimeofday), checked against what they mean rather than their values,
 * and writes and time structures in memory the call may not use.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t alarms;
static int pipe_fds[2];

static void on_alarm(int sig) {
    (void)sig;
    alarms++;
}

/* Makes room in the pipe, so that a restarted write succeeds: a pipe
   frees its buffer a page at a time. */
static void on_alarm_drain(int sig) {
    (void)sig;
    alarms++;
    static char page[8192];
    if (read(pipe_fds[0], page, sizeof page) < 4096)
        _exit(3);
}

static void install(int sig, void (*handler)(int), int flags) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = handler;
    sa.sa_flags = flags;
    sigemptyset(&sa.sa_mask);
    sigaction(sig, &sa, NULL);
}

static void arm_timer(long usec) {
    struct itimerval t = {{0, 0}, {0, usec}};
    setitimer(ITIMER_REAL, &t, NULL);
}

/* Fills the pipe without blocking, then makes it blocking again. */
static long fill_pipe(void) {
    int flags = fcntl(pipe_fds[1], F_GETFL);
    fcntl(pipe_fds[1], F_SETFL, flags | O_NONBLOCK);
    char block[4096];
    memset(block, 'p', sizeof block);
    long total = 0;
    for (;;) {
        ssize_t n = write(pipe_fds[1], block, sizeof block);
        if (n > 0) {
            total += n;
            continue;
        }
        n = write(pipe_fds[1], block, 1);
        if (n == 1) {
            total++;
            continue;
        }
        break;
    }
    fcntl(pipe_fds[1], F_SETFL, flags);
    return total;
}

int main(void) {
    pid_t pid = getpid();
    printf("getpid: %s\n", pid == (pid_t)syscall(SYS_getpid) && pid > 0 ? "same" : "differs");
    printf("gettid: %s\n", (pid_t)syscall(SYS_gettid) == pid ? "the process's" : "another");
    printf("getppid: %s\n", getppid() > 0 && getppid() != pid ? "a parent" : "?");
    printf("ids: %s\n",
           getuid() == (uid_t)syscall(SYS_getuid) && geteuid() == (uid_t)syscall(SYS_geteuid) &&
                   getgid() == (gid_t)syscall(SYS_getgid) &&
                   getegid() == (gid_t)syscall(SYS_getegid)
               ? "consistent"
               : "inconsistent");
    printf("sched_yield: %d\n", sched_yield());

    if (pipe(pipe_fds) != 0)
        return 1;
    /* lseek on a pipe: ESPIPE; on a file: positions. */
    errno = 0;
    long at = (long)lseek(pipe_fds[0], 0, SEEK_CUR);
    printf("lseek pipe: %ld %s\n", at, errno == ESPIPE ? "ESPIPE" : "?");
    FILE *tmp = tmpfile();
    if (tmp == NULL)
        return 1;
    int fd = fileno(tmp);
    printf("write file: %ld\n", (long)write(fd, "abcdef", 6));
    at = (long)lseek(fd, 0, SEEK_CUR);
    printf("lseek file: %ld %ld\n", at, (long)lseek(fd, 2, SEEK_SET));
    fclose(tmp);

    /* A buffer that is not mapped: EFAULT, nothing written. */
    char *gone = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    munmap(gone, 4096);
    errno = 0;
    long n = write(pipe_fds[1], gone, 16);
    printf("write unmapped: %ld %s\n", n, errno == EFAULT ? "EFAULT" : "?");
    /* Its first byte readable, the rest not: what the kernel writes. */
    char *half = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    memset(half, 'h', 8192);
    mprotect(half + 4096, 4096, PROT_NONE);
    errno = 0;
    n = write(pipe_fds[1], half + 4090, 16);
    printf("write across: %ld\n", n);
    char sink[64];
    while (n > 0)
        n -= read(pipe_fds[0], sink, n < 64 ? n : 64);

    /* The clocks: monotonic does not go back; a read-only page, or a
       structure running into an inaccessible page, is EFAULT. */
    long rc;
    struct timespec t1, t2;
    int r1 = clock_gettime(CLOCK_MONOTONIC, &t1);
    int r2 = clock_gettime(CLOCK_MONOTONIC, &t2);
    printf("clock_gettime: %d %d %s\n", r1, r2,
           t2.tv_sec > t1.tv_sec || (t2.tv_sec == t1.tv_sec && t2.tv_nsec >= t1.tv_nsec)
               ? "monotonic"
               : "back");
    struct timeval tv;
    int rt = gettimeofday(&tv, NULL);
    printf("gettimeofday: %d %s\n", rt, tv.tv_sec > 1000000000 ? "now" : "?");
    /* A page of code that has run (armor translated it): x86 writes there. */
    unsigned char *rwx =
        mmap(NULL, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    rwx[0] = 0xc3; /* ret */
    ((void (*)(void))rwx)();
    rc = syscall(SYS_clock_gettime, CLOCK_MONOTONIC, rwx + 64);
    ((void (*)(void))rwx)();
    printf("clock_gettime into code: %ld\n", rc);
    char *ro = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    mprotect(ro, 4096, PROT_READ);
    mprotect(ro + 4096, 4096, PROT_NONE);
    errno = 0;
    rc = syscall(SYS_clock_gettime, CLOCK_REALTIME, ro);
    printf("clock_gettime read-only: %ld %s\n", rc, errno == EFAULT ? "EFAULT" : "?");
    errno = 0;
    rc = syscall(SYS_gettimeofday, ro, NULL);
    printf("gettimeofday read-only: %ld %s\n", rc, errno == EFAULT ? "EFAULT" : "?");
    char *edge = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    mprotect(edge + 4096, 4096, PROT_NONE);
    errno = 0;
    rc = syscall(SYS_clock_gettime, CLOCK_REALTIME, edge + 4088);
    printf("clock_gettime across: %ld %s\n", rc, errno == EFAULT ? "EFAULT" : "?");

    /* A full pipe: the write blocks until the timer's signal. */
    long filled = fill_pipe();
    printf("pipe filled: %s\n", filled > 0 ? "yes" : "no");
    install(SIGALRM, on_alarm, 0);
    arm_timer(30000);
    errno = 0;
    n = write(pipe_fds[1], "w", 1);
    printf("write, no SA_RESTART: %ld %s alarms %d\n", n, n < 0 && errno == EINTR ? "EINTR" : "?",
           (int)alarms);

    install(SIGALRM, on_alarm_drain, SA_RESTART);
    arm_timer(30000);
    errno = 0;
    n = write(pipe_fds[1], "w", 1);
    printf("write, SA_RESTART: %ld alarms %d\n", n, (int)alarms);
    return 0;
}
