/*
 * Threads (milestone M3): pthread_create and pthread_join, thread-local
 * storage, a mutex-protected counter, atomic read-modify-write from many
 * threads (lock xadd, lock cmpxchg), a condition variable handing work
 * between threads, a spinlock built on plain stores and atomic exchange
 * (which needs x86's ordering of ordinary loads and stores), and
 * pthread_exit with a value. The output does not depend on scheduling.
 */
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>

#define THREADS 8
#define ITERATIONS 2000

static __thread int tls_value = 7;
static long mutex_counter;
static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
static atomic_long atomic_counter;
static atomic_long cas_counter;

/* A test-and-set spinlock protecting a plain counter. */
static atomic_int spin;
static long spin_counter;

static void *worker(void *arg) {
    intptr_t id = (intptr_t)arg;
    tls_value += (int)id;
    for (int i = 0; i < ITERATIONS; i++) {
        pthread_mutex_lock(&mutex);
        mutex_counter++;
        pthread_mutex_unlock(&mutex);

        atomic_fetch_add(&atomic_counter, 1);

        long old = atomic_load(&cas_counter);
        while (!atomic_compare_exchange_weak(&cas_counter, &old, old + 2))
            ;

        while (atomic_exchange_explicit(&spin, 1, memory_order_acquire))
            ;
        spin_counter++;
        atomic_store_explicit(&spin, 0, memory_order_release);
    }
    /* Each thread sees its own copy of the thread-local variable. */
    return (void *)(intptr_t)(tls_value == 7 + id ? 1 : 0);
}

/* Producer and consumer through a condition variable. */
static pthread_mutex_t queue_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t queue_cond = PTHREAD_COND_INITIALIZER;
static int queue[16], queue_len, done;

static void *producer(void *arg) {
    (void)arg;
    for (int i = 1; i <= 100; i++) {
        pthread_mutex_lock(&queue_lock);
        while (queue_len == 16)
            pthread_cond_wait(&queue_cond, &queue_lock);
        queue[queue_len++] = i;
        pthread_cond_broadcast(&queue_cond);
        pthread_mutex_unlock(&queue_lock);
    }
    pthread_mutex_lock(&queue_lock);
    done = 1;
    pthread_cond_broadcast(&queue_cond);
    pthread_mutex_unlock(&queue_lock);
    return NULL;
}

static void *consumer(void *arg) {
    long sum = 0;
    (void)arg;
    for (;;) {
        pthread_mutex_lock(&queue_lock);
        while (queue_len == 0 && !done)
            pthread_cond_wait(&queue_cond, &queue_lock);
        if (queue_len == 0 && done) {
            pthread_mutex_unlock(&queue_lock);
            break;
        }
        sum += queue[--queue_len];
        pthread_cond_broadcast(&queue_cond);
        pthread_mutex_unlock(&queue_lock);
    }
    pthread_exit((void *)sum);
}

int main(void) {
    pthread_t threads[THREADS];
    for (intptr_t i = 0; i < THREADS; i++)
        if (pthread_create(&threads[i], NULL, worker, (void *)i) != 0)
            return 2;
    int tls_ok = 0;
    for (int i = 0; i < THREADS; i++) {
        void *result;
        pthread_join(threads[i], &result);
        tls_ok += (int)(intptr_t)result;
    }
    printf("threads %d, thread-local ok in %d, main tls %d\n", THREADS, tls_ok, tls_value);
    printf("mutex counter %ld (expected %d)\n", mutex_counter, THREADS * ITERATIONS);
    printf("atomic counter %ld\n", (long)atomic_load(&atomic_counter));
    printf("cas counter %ld\n", (long)atomic_load(&cas_counter));
    printf("spinlock counter %ld\n", spin_counter);

    pthread_t prod, cons[3];
    pthread_create(&prod, NULL, producer, NULL);
    for (int i = 0; i < 3; i++)
        pthread_create(&cons[i], NULL, consumer, NULL);
    pthread_join(prod, NULL);
    long total = 0;
    for (int i = 0; i < 3; i++) {
        void *sum;
        pthread_join(cons[i], &sum);
        total += (long)sum;
    }
    printf("producer/consumer sum %ld (expected 5050)\n", total);
    return 0;
}
