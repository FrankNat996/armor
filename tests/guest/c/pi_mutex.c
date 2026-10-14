/*
 * Priority-inheritance mutexes (PTHREAD_PRIO_INHERIT): glibc locks and
 * unlocks them with FUTEX_LOCK_PI/FUTEX_UNLOCK_PI when contended, and a
 * condition variable waiting on one uses FUTEX_WAIT_REQUEUE_PI and
 * FUTEX_CMP_REQUEUE_PI. Four threads share a counter under such a mutex,
 * trylock sees it busy, and a waiter is signalled. A Steam game's audio
 * library asked for them. The same source runs as x86-64 and i386.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdio.h>

static pthread_mutex_t lock;
static pthread_cond_t ready = PTHREAD_COND_INITIALIZER;
static long counter;
static int flag;

static void *add(void *arg) {
    (void)arg;
    for (int i = 0; i < 20000; i++) {
        pthread_mutex_lock(&lock);
        counter++;
        pthread_mutex_unlock(&lock);
    }
    return NULL;
}

static void *waiter(void *arg) {
    (void)arg;
    pthread_mutex_lock(&lock);
    while (!flag) {
        pthread_cond_wait(&ready, &lock);
    }
    pthread_mutex_unlock(&lock);
    return NULL;
}

static void *try_it(void *arg) {
    *(int *)arg = pthread_mutex_trylock(&lock);
    return NULL;
}

int main(void) {
    pthread_mutexattr_t attr;
    pthread_mutexattr_init(&attr);
    printf("protocol: %d\n", pthread_mutexattr_setprotocol(&attr, PTHREAD_PRIO_INHERIT));
    pthread_mutex_init(&lock, &attr);
    pthread_t t[4];
    for (int i = 0; i < 4; i++) {
        pthread_create(&t[i], NULL, add, NULL);
    }
    for (int i = 0; i < 4; i++) {
        pthread_join(t[i], NULL);
    }
    printf("counter: %ld\n", counter);

    pthread_mutex_lock(&lock);
    int busy = -1;
    pthread_t other;
    pthread_create(&other, NULL, try_it, &busy);
    pthread_join(other, NULL);
    printf("trylock while held: %s\n", busy == EBUSY ? "EBUSY" : "other");
    pthread_mutex_unlock(&lock);

    pthread_t w;
    pthread_create(&w, NULL, waiter, NULL);
    pthread_mutex_lock(&lock);
    flag = 1;
    pthread_cond_signal(&ready);
    pthread_mutex_unlock(&lock);
    pthread_join(w, NULL);
    printf("waiter woke: %d\n", flag);
    return 0;
}
