/*
 * i386 threads and processes (ADR-0029): what is different from x86_64.
 * - pthread_create: clone with CLONE_SETTLS and a struct user_desc (the
 *   TLS entry glibc already set with set_thread_area, new base), argument
 *   order flags, stack, parent_tid, tls, child_tid; each thread sees its
 *   own __thread variables through gs;
 * - fork, vfork and wait4 with the 32-bit struct rusage; waitpid;
 * - execve with 4-byte argv and envp pointers.
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

static __thread int mine = 100;
static int shared;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;

static void *worker(void *arg) {
    int id = (int)(long)arg;
    mine += id;
    for (int i = 0; i < 1000; i++) {
        pthread_mutex_lock(&lock);
        shared++;
        pthread_mutex_unlock(&lock);
    }
    return (void *)(long)(mine * 10);
}

int main(int argc, char **argv) {
    if (argc > 1) {
        printf("exec: argc %d argv1 %s env %s\n", argc, argv[1], getenv("ARMOR_T32"));
        return 7;
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    pthread_t t[4];
    for (long i = 0; i < 4; i++) {
        pthread_create(&t[i], NULL, worker, (void *)(i + 1));
    }
    for (int i = 0; i < 4; i++) {
        void *r;
        pthread_join(t[i], &r);
        printf("thread %d: %ld\n", i, (long)r);
    }
    printf("main tls %d shared %d\n", mine, shared);

    pid_t pid = fork();
    if (pid == 0) {
        mine = 5;
        _exit(mine + 1);
    }
    int status;
    struct rusage usage;
    memset(&usage, 0xff, sizeof usage);
    pid_t got = wait4(pid, &status, 0, &usage);
    printf("fork: waited %d status %d main tls %d rusage sane %d\n", got == pid,
           WEXITSTATUS(status), mine, usage.ru_utime.tv_usec >= 0 && usage.ru_utime.tv_usec < 1000000);

    pid = vfork();
    if (pid == 0) {
        char *args[] = {argv[0], "child", NULL};
        /* The environment plus one variable: the oracle's own variables
         * (QEMU_LD_PREFIX) must reach the child. */
        extern char **environ;
        int n = 0;
        while (environ[n]) {
            n++;
        }
        char **env = calloc(n + 2, sizeof *env);
        memcpy(env, environ, n * sizeof *env);
        env[n] = "ARMOR_T32=yes";
        execve("/proc/self/exe", args, env);
        _exit(99);
    }
    waitpid(pid, &status, 0);
    printf("vfork+execve: status %d\n", WEXITSTATUS(status));
    return 0;
}
