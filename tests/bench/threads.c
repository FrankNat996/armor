/*
 * Benchmark (cargo xtask bench): four threads, each with its own integer
 * work (it scales with real parallelism), then all four incrementing
 * a mutex-protected counter and an atomic one (contention).
 */
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>

#define THREADS 4
static uint64_t results[THREADS];
static long counter;
static atomic_long atomic_counter;
static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;

static void *worker(void *arg) {
    intptr_t id = (intptr_t)arg;
    uint64_t h = (uint64_t)id + 1;
    for (int i = 0; i < 15000000; i++) {
        h ^= (uint64_t)i;
        h *= 0xbf58476d1ce4e5b9u;
        h ^= h >> 29;
    }
    results[id] = h;
    for (int i = 0; i < 100000; i++) {
        pthread_mutex_lock(&mutex);
        counter++;
        pthread_mutex_unlock(&mutex);
        atomic_fetch_add(&atomic_counter, 1);
    }
    return NULL;
}

int main(void) {
    pthread_t threads[THREADS];
    for (intptr_t i = 0; i < THREADS; i++)
        pthread_create(&threads[i], NULL, worker, (void *)i);
    uint64_t sum = 0;
    for (int i = 0; i < THREADS; i++) {
        pthread_join(threads[i], NULL);
        sum ^= results[i];
    }
    printf("%llu %ld %ld\n", (unsigned long long)sum, counter, atomic_load(&atomic_counter));
    return 0;
}
