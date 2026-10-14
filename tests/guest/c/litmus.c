/*
 * Memory-model litmus tests (ADR-0023): two threads walk arrays of
 * independent locations at the same time, so that their accesses to
 * location i race; the program counts the outcomes x86-TSO forbids.
 *   MP:        x=1; y=1           ||  r1=y; r2=x           forbidden r1=1, r2=0
 *   SB+mfence: x=1; mfence; r1=y  ||  y=1; mfence; r2=x    forbidden r1=0, r2=0
 *   SB+lock:   the same with `lock addl $0, (%rsp)`, the usual cheaper
 *              full barrier on x86 (armor's JIT: an LSE atomic, ADR-0023)
 * Expected under qemu-x86_64 and armor: 0 for all three. Built natively for
 * AArch64 the same source shows MP outcomes, and with -DNO_FENCE SB ones:
 * that is how the test was checked to see weak ordering (docs/testing.md).
 */
#include <pthread.h>
#include <stdio.h>
#include <string.h>

#define SIZE 4096
#define BATCHES 40

#define BARRIER() __asm__ volatile("" ::: "memory")
#if defined(NO_FENCE)
#define MFENCE() BARRIER()
#elif defined(__x86_64__)
#define MFENCE() __asm__ volatile("mfence" ::: "memory")
#else
#define MFENCE() __asm__ volatile("dmb ish" ::: "memory")
#endif
#if defined(NO_FENCE)
#define LOCKED() BARRIER()
#elif defined(__x86_64__)
#define LOCKED() __asm__ volatile("lock addl $0, (%%rsp)" ::: "memory", "cc")
#else
#define LOCKED() __atomic_fetch_add(&lock_word, 0, __ATOMIC_SEQ_CST)
static int lock_word;
#endif

/* Each location on its own cache line. */
struct cell {
    volatile int v;
    char pad[60];
};
static struct cell x[SIZE], y[SIZE];
static int r1[SIZE], r2[SIZE];
static volatile int go, arrived, finished;
static int test;

/* The full barrier of the SB test being run. */
static void fence(void) {
    if (test == 1)
        MFENCE();
    else
        LOCKED();
}

static void sync_start(int batch) {
    __atomic_fetch_add(&arrived, 1, __ATOMIC_ACQ_REL);
    while (__atomic_load_n(&arrived, __ATOMIC_ACQUIRE) < 2 * batch)
        ;
}

static void *second(void *arg) {
    (void)arg;
    for (int batch = 1; batch <= BATCHES; batch++) {
        sync_start(batch);
        if (test == 0) {
            for (int i = 0; i < SIZE; i++) {
                r1[i] = y[i].v;
                BARRIER();
                r2[i] = x[i].v;
            }
        } else {
            for (int i = 0; i < SIZE; i++) {
                y[i].v = 1;
                fence();
                r2[i] = x[i].v;
            }
        }
        __atomic_store_n(&finished, batch, __ATOMIC_RELEASE);
        while (__atomic_load_n(&go, __ATOMIC_ACQUIRE) != batch)
            ;
    }
    return NULL;
}

int main(void) {
    static const char *names[] = {"MP", "SB+mfence", "SB+lock"};
    for (test = 0; test < 3; test++) {
        long forbidden = 0;
        go = arrived = finished = 0;
        pthread_t t;
        pthread_create(&t, NULL, second, NULL);
        for (int batch = 1; batch <= BATCHES; batch++) {
            for (int i = 0; i < SIZE; i++)
                x[i].v = y[i].v = 0;
            sync_start(batch);
            if (test == 0) {
                for (int i = 0; i < SIZE; i++) {
                    x[i].v = 1;
                    BARRIER();
                    y[i].v = 1;
                }
            } else {
                for (int i = 0; i < SIZE; i++) {
                    x[i].v = 1;
                    fence();
                    r1[i] = y[i].v;
                }
            }
            /* The other thread's results are complete once it says so. */
            while (__atomic_load_n(&finished, __ATOMIC_ACQUIRE) != batch)
                ;
            for (int i = 0; i < SIZE; i++) {
                if (test == 0)
                    forbidden += r1[i] == 1 && r2[i] == 0;
                else
                    forbidden += r1[i] == 0 && r2[i] == 0;
            }
            __atomic_store_n(&go, batch, __ATOMIC_RELEASE);
        }
        pthread_join(t, NULL);
        printf("%-10s forbidden %ld\n", names[test], forbidden);
    }
    return 0;
}
