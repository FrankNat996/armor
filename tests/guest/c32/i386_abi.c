/*
 * i386 ABI under armor (ADR-0029): what a 32-bit glibc program relies on.
 * - 64-bit integers on 32-bit registers (adc, sbb, the libgcc division
 *   helpers), and the x87 floating point the i386 ABI uses for double;
 * - syscalls whose structures differ from x86_64: _llseek (a 64-bit offset
 *   in two halves), ugetrlimit (32-bit struct rlimit), clock_gettime64,
 *   gettimeofday (32-bit struct timeval), writev (32-bit struct iovec);
 * - thread-local storage through gs (set_thread_area at startup);
 * - the stack: arguments on it, 16-byte alignment kept by glibc.
 * Prints only values that do not depend on the machine.
 */
#define _LARGEFILE64_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/time.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

static __thread int tls_counter = 41;

static uint64_t mix(uint64_t a, uint64_t b) {
    return (a * b) ^ (a / (b | 1)) ^ (a % (b | 3)) ^ (a << 13) ^ (b >> 7);
}

static int64_t smix(int64_t a, int64_t b) {
    return a / (b | 1) + a % (b | 1) - (a >> 3);
}

static double series(int n) {
    double sum = 0;
    for (int i = 1; i <= n; i++) {
        sum += 1.0 / ((double)i * i);
    }
    return sum;
}

int main(void) {
    printf("sizes: int %zu long %zu pointer %zu long long %zu\n", sizeof(int),
           sizeof(long), sizeof(void *), sizeof(long long));

    uint64_t h = 0x123456789abcdefULL;
    for (int i = 0; i < 1000; i++) {
        h = mix(h, h ^ (uint64_t)i * 0x9e3779b97f4a7c15ULL);
    }
    printf("u64 mix: %016llx\n", (unsigned long long)h);
    int64_t s = -0x7654321012345678LL;
    for (int i = 1; i < 100; i++) {
        s = smix(s, (int64_t)i * -977) * 3 + i;
    }
    printf("i64 mix: %lld\n", (long long)s);
    printf("x87 double: %.15f %.6e\n", series(1000), 1.0 / 3.0 * 1e300);

    tls_counter++;
    printf("tls: %d\n", tls_counter);

    /* _llseek past 4 GiB in a sparse file. mkstemp64: with O_LARGEFILE,
     * without which the kernel refuses writes beyond 2 GiB - 1 (EFBIG) to
     * a 32-bit process, which armor does not reproduce (docs/tech/syscalls.md). */
    char name[] = "/tmp/armor-i386-XXXXXX";
    int fd = mkstemp64(name);
    if (fd < 0) {
        perror("mkstemp");
        return 1;
    }
    unlink(name);
    off64_t big = lseek64(fd, 5LL << 30, SEEK_SET);
    printf("lseek64: %lld\n", (long long)big);
    if (write(fd, "x", 1) != 1) {
        perror("write");
    }
    printf("end: %lld\n", (long long)lseek64(fd, 0, SEEK_END));
    errno = 0;
    off_t small = lseek(fd, 0, SEEK_CUR);
    printf("32-bit lseek beyond 2 GiB: %ld %s\n", (long)small,
           errno == EOVERFLOW ? "EOVERFLOW" : strerror(errno));
    close(fd);

    struct rlimit limit;
    printf("getrlimit: %d\n", getrlimit(RLIMIT_NOFILE, &limit));
    printf("nofile soft <= hard: %d\n", limit.rlim_cur <= limit.rlim_max);

    struct timespec ts;
    struct timeval tv;
    printf("clock_gettime: %d\n", clock_gettime(CLOCK_MONOTONIC, &ts));
    printf("gettimeofday: %d\n", gettimeofday(&tv, NULL));
    printf("plausible: %d %d\n", ts.tv_nsec >= 0 && ts.tv_nsec < 1000000000,
           tv.tv_sec > 1600000000);

    struct iovec parts[3] = {
        {"wri", 3},
        {"tev", 3},
        {" ok\n", 4},
    };
    fflush(stdout);
    printf("writev: %zd\n", writev(1, parts, 3));

    char *buffer = malloc(1 << 20);
    memset(buffer, 0x5a, 1 << 20);
    unsigned sum = 0;
    for (int i = 0; i < 1 << 20; i += 4096) {
        sum += (unsigned char)buffer[i];
    }
    printf("malloc 1 MiB: %u\n", sum);
    free(buffer);
    return 0;
}
