/*
 * Processes (milestone M3): fork with exit status and with death by a
 * signal, a pipe between parent and child, getppid, vfork, execv of the
 * program itself, posix_spawn, waitpid with WNOHANG, and system() (which
 * runs the host's /bin/sh). Children re-run this program with an argument
 * that selects their role.
 */
#define _GNU_SOURCE
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static void report(const char *what, int status) {
    if (WIFEXITED(status))
        printf("%s: exit %d\n", what, WEXITSTATUS(status));
    else if (WIFSIGNALED(status))
        printf("%s: signal %d\n", what, WTERMSIG(status));
    else
        printf("%s: status %#x\n", what, status);
}

int main(int argc, char **argv) {
    if (argc > 1) {
        /* Child roles, after exec. */
        if (strcmp(argv[1], "exec") == 0) {
            printf("exec'd child: argv[0] %s, %d arguments\n",
                   strrchr(argv[0], '/') ? strrchr(argv[0], '/') + 1 : argv[0], argc);
            return 21;
        }
        if (strcmp(argv[1], "spawn") == 0)
            return 22;
        return 2;
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    int status;

    pid_t child = fork();
    if (child == 0)
        _exit(5);
    waitpid(child, &status, 0);
    report("fork", status);

    child = fork();
    if (child == 0) {
        raise(SIGTERM);
        _exit(1);
    }
    waitpid(child, &status, 0);
    report("fork killed", status);

    /* Parent and child talk through a pipe; the child checks getppid. */
    int fds[2];
    if (pipe(fds) != 0)
        return 3;
    pid_t parent = getpid();
    child = fork();
    if (child == 0) {
        close(fds[0]);
        char msg[64];
        int n = snprintf(msg, sizeof msg, "ppid ok %d", getppid() == parent);
        if (write(fds[1], msg, n) != n)
            _exit(4);
        _exit(0);
    }
    close(fds[1]);
    char buf[64] = {0};
    ssize_t n = read(fds[0], buf, sizeof buf - 1);
    close(fds[0]);
    waitpid(child, &status, 0);
    printf("pipe: %zd bytes \"%s\"\n", n, buf);

    child = vfork();
    if (child == 0)
        _exit(6);
    waitpid(child, &status, 0);
    report("vfork", status);

    child = fork();
    if (child == 0) {
        char *args[] = {argv[0], "exec", "x", NULL};
        execv("/proc/self/exe", args);
        _exit(98);
    }
    waitpid(child, &status, 0);
    report("execv", status);

    char *spawn_args[] = {argv[0], "spawn", NULL};
    if (posix_spawn(&child, "/proc/self/exe", NULL, NULL, spawn_args, environ) != 0)
        return 5;
    waitpid(child, &status, 0);
    report("posix_spawn", status);

    /* WNOHANG before the child exits, then a blocking wait. */
    if (pipe(fds) != 0)
        return 6;
    child = fork();
    if (child == 0) {
        char c;
        close(fds[1]);
        if (read(fds[0], &c, 1) != 1)
            _exit(7);
        _exit(8);
    }
    close(fds[0]);
    printf("wnohang: %d\n", waitpid(child, &status, WNOHANG));
    if (write(fds[1], "x", 1) != 1)
        return 7;
    waitpid(child, &status, 0);
    report("after wnohang", status);

    report("system", system("exit 3"));
    return 0;
}
