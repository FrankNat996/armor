/*
 * Calls Chromium makes that hand plain values to the kernel (a Spotify
 * build did): fsync, fdatasync, syncfs, sync, sync_file_range on a file;
 * getrusage and times (their values vary: only results and sanity);
 * getcpu; mincore (one byte per 4K page, ENOMEM past a mapping, EINVAL
 * unaligned); mlock and munlock; membarrier's query; ioprio_get. Prints no
 * value that depends on the machine.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/times.h>
#include <unistd.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EINVAL: return "EINVAL";
    case ENOMEM: return "ENOMEM";
    case EBADF: return "EBADF";
    case EPERM: return "EPERM";
    default: return "other";
    }
}

static void show(const char *what, long result) {
    printf("%s: %ld %s\n", what, result < 0 ? -1L : 0L, name(result < 0 ? errno : 0));
}

int main(void) {
    char path[] = "/tmp/armor-sync-XXXXXX";
    int fd = mkstemp(path);
    if (write(fd, "data", 4) != 4) {
        return 1;
    }
    show("fsync", fsync(fd));
    show("fdatasync", fdatasync(fd));
    show("syncfs", syncfs(fd));
    sync();
    show("sync_file_range", sync_file_range(fd, 0, 4, SYNC_FILE_RANGE_WRITE));
    show("fsync bad fd", fsync(-1));
    close(fd);
    unlink(path);

    struct rusage usage;
    show("getrusage self", getrusage(RUSAGE_SELF, &usage));
    printf("  max rss positive: %d\n", usage.ru_maxrss > 0);
    show("getrusage bad who", getrusage(42, &usage));
    struct tms tms;
    clock_t ticks = times(&tms);
    printf("times: positive %d\n", ticks > 0);

    unsigned cpu = 99999, node = 99999;
    show("getcpu", syscall(SYS_getcpu, &cpu, &node, NULL));
    printf("  cpu below the count: %d\n", cpu < (unsigned)sysconf(_SC_NPROCESSORS_CONF));

    char *pages = mmap(NULL, 3 * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    pages[0] = 1;
    pages[2 * 4096] = 1;
    unsigned char vec[3] = {9, 9, 9};
    show("mincore", mincore(pages, 3 * 4096, vec));
    printf("  touched pages resident: %d %d\n", vec[0] & 1, vec[2] & 1);
    show("mincore unaligned", mincore(pages + 1, 4096, vec));
    munmap(pages + 2 * 4096, 4096);
    show("mincore past the mapping", mincore(pages, 3 * 4096, vec));
    show("mlock", mlock(pages, 4096));
    show("munlock", munlock(pages, 4096));
    long commands = syscall(SYS_membarrier, 0, 0, 0);
    printf("membarrier query: %d\n", commands >= 0);
    show("ioprio_get", syscall(SYS_ioprio_get, 1, 0));
    return 0;
}
