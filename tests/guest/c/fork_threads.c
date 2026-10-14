/*
 * fork from a process with several threads running, as Python programs
 * (the Ren'Py engine of a Steam game) do: from the main thread and from a
 * worker, while other threads keep allocating and locking. As in Linux,
 * the child has only the forking thread: it allocates, takes a mutex the
 * parent's threads use, starts a thread of its own, runs a command with
 * popen and exits; the parent's threads go on. The mutex is made safe
 * across fork with pthread_atfork, as a correct program must: without it
 * the child could inherit it locked by a thread it does not have, on
 * Linux too. The same source runs as x86-64 and i386.
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static atomic_int stop;
static long counter;

/* Busy threads: allocate, free and lock all the time. */
static void *busy(void *arg) {
    (void)arg;
    while (!atomic_load(&stop)) {
        char *p = malloc(64 + (counter & 255));
        memset(p, 1, 64);
        pthread_mutex_lock(&lock);
        counter++;
        pthread_mutex_unlock(&lock);
        free(p);
    }
    return NULL;
}

static void lock_before_fork(void) {
    pthread_mutex_lock(&lock);
}

static void unlock_after_fork(void) {
    pthread_mutex_unlock(&lock);
}

static void *child_thread(void *arg) {
    *(int *)arg = 42;
    return NULL;
}

/* What the child does: everything a single-threaded process may do. */
static int in_child(void) {
    char *p = malloc(1 << 20);
    if (!p) {
        return 1;
    }
    memset(p, 2, 1 << 20);
    free(p);
    pthread_mutex_lock(&lock);
    pthread_mutex_unlock(&lock);
    int value = 0;
    pthread_t t;
    if (pthread_create(&t, NULL, child_thread, &value) != 0 || pthread_join(t, NULL) != 0) {
        return 2;
    }
    if (value != 42) {
        return 3;
    }
    FILE *f = popen("echo from popen", "r");
    char line[64] = {0};
    if (!f || !fgets(line, sizeof line, f) || pclose(f) != 0) {
        return 4;
    }
    return strcmp(line, "from popen\n") == 0 ? 7 : 5;
}

static int fork_and_wait(void) {
    pid_t pid = fork();
    if (pid < 0) {
        return -1;
    }
    if (pid == 0) {
        _exit(in_child());
    }
    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        return -2;
    }
    return WIFEXITED(status) ? WEXITSTATUS(status) : 100 + WTERMSIG(status);
}

static void *forking_worker(void *arg) {
    *(int *)arg = fork_and_wait();
    return NULL;
}

int main(void) {
    pthread_atfork(lock_before_fork, unlock_after_fork, unlock_after_fork);
    pthread_t threads[3];
    for (int i = 0; i < 3; i++) {
        pthread_create(&threads[i], NULL, busy, NULL);
    }
    for (int round = 0; round < 5; round++) {
        printf("fork from main, round %d: child exit %d\n", round, fork_and_wait());
    }
    int from_worker = 0;
    pthread_t worker;
    pthread_create(&worker, NULL, forking_worker, &from_worker);
    pthread_join(worker, NULL);
    printf("fork from a worker: child exit %d\n", from_worker);
    atomic_store(&stop, 1);
    for (int i = 0; i < 3; i++) {
        pthread_join(threads[i], NULL);
    }
    printf("busy threads ran: %d\n", counter > 0);
    return 0;
}
