/*
 * Signals and threads (milestone M3): a signal sent to one thread
 * (pthread_kill) runs the handler on that thread; a signal sent to the
 * process goes to the thread that does not block it; masks are per
 * thread, so a blocked signal stays pending on its thread only.
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static atomic_int handler_tid;
static atomic_int ready, go, finished;

static void record(int sig) {
    (void)sig;
    atomic_store(&handler_tid, gettid());
}

static void wait_for(atomic_int *flag, int value) {
    while (atomic_load(flag) < value)
        usleep(1000);
}

static void *target(void *arg) {
    atomic_int *tid = arg;
    atomic_store(tid, gettid());
    /* Phase 1: unblocked, wait for the handler to run here. */
    atomic_store(&ready, 1);
    wait_for(&go, 1);
    /* Phase 2: block SIGUSR1, receive it pending, then unblock. */
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    pthread_sigmask(SIG_BLOCK, &set, NULL);
    atomic_store(&ready, 2);
    wait_for(&go, 2);
    sigset_t pending;
    sigpending(&pending);
    printf("pending on target while blocked: %d\n", sigismember(&pending, SIGUSR1));
    atomic_store(&handler_tid, 0);
    pthread_sigmask(SIG_UNBLOCK, &set, NULL);
    printf("handler after unblock on target: %d\n", atomic_load(&handler_tid) == gettid());
    atomic_store(&ready, 3);
    wait_for(&go, 3);
    atomic_store(&finished, 1);
    return NULL;
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = record;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);
    sigaction(SIGUSR2, &sa, NULL);

    atomic_int tid = 0;
    pthread_t thread;
    pthread_create(&thread, NULL, target, &tid);
    wait_for(&ready, 1);

    /* A signal to one thread runs its handler on that thread. */
    pthread_kill(thread, SIGUSR1);
    while (atomic_load(&handler_tid) == 0)
        usleep(1000);
    printf("pthread_kill handled by target: %d\n", atomic_load(&handler_tid) == atomic_load(&tid));

    /* A process signal goes to a thread that does not block it. */
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR2);
    pthread_sigmask(SIG_BLOCK, &set, NULL);
    atomic_store(&handler_tid, 0);
    kill(getpid(), SIGUSR2);
    while (atomic_load(&handler_tid) == 0)
        usleep(1000);
    printf("kill handled by the unblocked thread: %d\n", atomic_load(&handler_tid) == atomic_load(&tid));

    /* Per-thread masks: pending on the target only. */
    atomic_store(&go, 1);
    wait_for(&ready, 2);
    pthread_kill(thread, SIGUSR1);
    sigset_t pending;
    sigpending(&pending);
    printf("pending on main: %d\n", sigismember(&pending, SIGUSR1));
    atomic_store(&go, 2);
    wait_for(&ready, 3);
    atomic_store(&go, 3);
    pthread_join(thread, NULL);
    printf("joined: %d\n", atomic_load(&finished));
    return 0;
}
