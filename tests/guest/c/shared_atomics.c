/*
 * LOCK instructions on memory shared between processes (ADR-0022): three
 * children update the same shared page at the same time, with lock xadd,
 * a lock cmpxchg loop, a spinlock built on xchg guarding a plain counter,
 * lock or and lock bts, at 8, 16, 32 and 64 bits, and lock cmpxchg8b and
 * lock cmpxchg16b loops on 64- and 128-bit counters (the 128-bit one
 * starts just below 2^64, so that increments carry into the high half).
 * Every update must survive: the processes run in parallel on the host.
 */
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHILDREN 3
#define ITERATIONS 100000

struct shared {
    uint64_t xadd;
    uint64_t cas;
    uint64_t guarded;
    uint32_t lock;
    uint32_t sum32;
    uint16_t sum16;
    uint8_t sum8;
    uint64_t bits;
    uint64_t wide8;
    uint64_t pad;
    unsigned __int128 wide16 __attribute__((aligned(16)));
};

/* lock cmpxchg8b: *p = *p + 1, retried until no other process interferes. */
static void increment8b(uint64_t *p) {
    uint64_t old = __atomic_load_n(p, __ATOMIC_RELAXED);
    for (;;) {
        uint32_t lo = (uint32_t)old, hi = (uint32_t)(old >> 32);
        uint64_t new = old + 1;
        char ok;
        __asm__ volatile("lock cmpxchg8b %1; sete %0"
                         : "=q"(ok), "+m"(*p), "+a"(lo), "+d"(hi)
                         : "b"((uint32_t)new), "c"((uint32_t)(new >> 32))
                         : "memory", "cc");
        if (ok)
            return;
        old = (uint64_t)hi << 32 | lo;
    }
}

/* lock cmpxchg16b: the same on 128 bits. */
static void increment16b(unsigned __int128 *p) {
    uint64_t lo = (uint64_t)*p, hi = (uint64_t)(*p >> 64);
    for (;;) {
        unsigned __int128 new = ((unsigned __int128)hi << 64 | lo) + 1;
        char ok;
        __asm__ volatile("lock cmpxchg16b %1; sete %0"
                         : "=q"(ok), "+m"(*p), "+a"(lo), "+d"(hi)
                         : "b"((uint64_t)new), "c"((uint64_t)(new >> 64))
                         : "memory", "cc");
        if (ok)
            return;
    }
}

static void work(struct shared *s, int id) {
    for (int i = 0; i < ITERATIONS; i++) {
        __atomic_fetch_add(&s->xadd, 1, __ATOMIC_SEQ_CST);
        uint64_t old = __atomic_load_n(&s->cas, __ATOMIC_RELAXED);
        while (!__atomic_compare_exchange_n(&s->cas, &old, old + 1, 1, __ATOMIC_SEQ_CST,
                                            __ATOMIC_RELAXED))
            ;
        while (__atomic_exchange_n(&s->lock, 1, __ATOMIC_ACQUIRE))
            ;
        s->guarded++;
        __atomic_store_n(&s->lock, 0, __ATOMIC_RELEASE);
        __atomic_fetch_add(&s->sum32, 3, __ATOMIC_SEQ_CST);
        __atomic_fetch_add(&s->sum16, 5, __ATOMIC_SEQ_CST);
        __atomic_fetch_add(&s->sum8, 7, __ATOMIC_SEQ_CST);
        increment8b(&s->wide8);
        increment16b(&s->wide16);
    }
    __atomic_fetch_or(&s->bits, 1ull << id, __ATOMIC_SEQ_CST);
    /* lock bts on a bit of its own. */
    __asm__ volatile("lock btsq %1, %0" : "+m"(s->bits) : "r"((uint64_t)(32 + id)) : "memory");
}

int main(void) {
    struct shared *s = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (s == MAP_FAILED)
        return 1;
    s->wide16 = (unsigned __int128)0xfffffffffffe0000ull;
    for (int id = 0; id < CHILDREN; id++) {
        if (fork() == 0) {
            work(s, id);
            _exit(0);
        }
    }
    for (int id = 0; id < CHILDREN; id++)
        wait(NULL);
    const uint64_t n = (uint64_t)CHILDREN * ITERATIONS;
    printf("xadd %llu cas %llu guarded %llu (expected %llu)\n", (unsigned long long)s->xadd,
           (unsigned long long)s->cas, (unsigned long long)s->guarded, (unsigned long long)n);
    printf("sum32 %u sum16 %u sum8 %u (expected %u %u %u)\n", s->sum32, s->sum16, s->sum8,
           (uint32_t)(3 * n), (uint16_t)(5 * n), (uint8_t)(7 * n));
    printf("bits %#llx\n", (unsigned long long)s->bits);
    printf("cmpxchg8b %llu cmpxchg16b %#llx:%016llx\n", (unsigned long long)s->wide8,
           (unsigned long long)(s->wide16 >> 64), (unsigned long long)s->wide16);
    return 0;
}
