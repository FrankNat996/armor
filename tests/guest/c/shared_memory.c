/*
 * MAP_SHARED (ADR-0022): anonymous shared memory seen by a child after
 * fork (and private memory not), a file mapped shared and written through
 * memory and through write(2), two mappings of the same file, an offset
 * that is 4K- but not 16K-aligned, MAP_FIXED, msync, mprotect and EACCES
 * on read-only files, madvise(MADV_DONTNEED) keeping shared contents,
 * and shm_open.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define PAGE 4096

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EACCES: return "EACCES";
    case EINVAL: return "EINVAL";
    case ENOMEM: return "ENOMEM";
    case EEXIST: return "EEXIST";
    default: return "other";
    }
}

static void check(const char *label, long r) {
    printf("%-34s %s\n", label, r < 0 ? name(errno) : "ok");
}

int main(void) {
    /* Anonymous shared and private memory across fork. */
    int *shared = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    int *private = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    check("mmap shared anonymous", shared == MAP_FAILED ? -1 : 0);
    shared[0] = 1;
    private[0] = 1;
    pid_t pid = fork();
    if (pid == 0) {
        shared[0] = 42;
        shared[100] = 7;
        private[0] = 42;
        _exit(0);
    }
    waitpid(pid, NULL, 0);
    printf("after fork: shared %d %d, private %d\n", shared[0], shared[100], private[0]);
    munmap(shared, PAGE);

    /* A file, mapped shared. */
    char path[] = "/tmp/armor-shared-XXXXXX";
    int fd = mkstemp(path);
    check("ftruncate", ftruncate(fd, 5 * PAGE));
    char *file = mmap(NULL, 3 * PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    check("mmap shared file", file == MAP_FAILED ? -1 : 0);
    memcpy(file + PAGE + 10, "through memory", 14);
    check("msync", msync(file, 3 * PAGE, MS_SYNC));
    char buf[32] = {0};
    pread(fd, buf, 14, PAGE + 10);
    printf("file after a memory write: %s\n", buf);
    pwrite(fd, "through write", 13, 2 * PAGE + 5);
    printf("memory after write(2): %.13s\n", file + 2 * PAGE + 5);

    /* A second mapping of the same file, at a 4K (not 16K) offset. */
    char *alias = mmap(NULL, 2 * PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, PAGE);
    check("mmap at offset 4096", alias == MAP_FAILED ? -1 : 0);
    printf("alias sees: %.14s\n", alias + 10);
    alias[PAGE + 5] = 'T';
    printf("first mapping sees: %.13s\n", file + 2 * PAGE + 5);

    /* MAP_FIXED inside a reserved range, over whole 64 KiB blocks (ADR-0022:
     * on a host with 16K or 64K pages a shared mapping cannot share a host
     * page with other guest pages, the reservation's included). */
    long block = 65536;
    char *area = mmap(NULL, 3 * block, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    char *aligned = (char *)(((unsigned long)area + block) & ~(block - 1));
    char *fixed = mmap(aligned, block, PROT_READ, MAP_SHARED | MAP_FIXED, fd, 0);
    check("MAP_FIXED shared file", fixed == aligned ? 0 : -1);
    printf("fixed sees: %.14s\n", fixed + PAGE + 10);
    check("mprotect of the reservation", mprotect(aligned + block, 4 * PAGE, PROT_READ | PROT_WRITE));
    aligned[block] = 1;
    munmap(area, 3 * block);

    /* MADV_DONTNEED keeps what the file holds. */
    check("madvise DONTNEED", madvise(file, 3 * PAGE, MADV_DONTNEED));
    printf("after DONTNEED: %.14s\n", file + PAGE + 10);

    /* Read-only files: no writable shared mapping, no mprotect to write. */
    int ro = open(path, O_RDONLY);
    void *w = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, ro, 0);
    check("mmap write on read-only file", w == MAP_FAILED ? -1 : 0);
    char *r = mmap(NULL, PAGE, PROT_READ, MAP_SHARED, ro, PAGE);
    check("mmap read-only shared", r == MAP_FAILED ? -1 : 0);
    printf("read-only mapping sees: %.14s\n", r + 10);
    check("mprotect write on it", mprotect(r, PAGE, PROT_READ | PROT_WRITE));
    pwrite(fd, "changed", 7, PAGE + 10);
    printf("read-only mapping after write(2): %.14s\n", r + 10);
    close(ro);

    check("msync unaligned", msync(file + 1, PAGE, MS_SYNC));
    check("msync both flags", msync(file, PAGE, MS_SYNC | MS_ASYNC));
    munmap(file, 3 * PAGE);
    /* Flags are checked before the range, as on Linux. */
    check("msync unmapped, both flags", msync(file, PAGE, MS_SYNC | MS_ASYNC));
    check("msync unmapped", msync(file, PAGE, MS_SYNC));
    munmap(alias, 2 * PAGE);
    munmap(r, PAGE);
    close(fd);
    unlink(path);

    /* shm_open: /dev/shm, shared with a child. */
    char shm_name[64];
    snprintf(shm_name, sizeof shm_name, "/armor-test-%d", (int)getpid());
    int shm = shm_open(shm_name, O_CREAT | O_RDWR, 0600);
    check("shm_open", shm);
    ftruncate(shm, PAGE);
    long *counter = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, shm, 0);
    *counter = 5;
    pid = fork();
    if (pid == 0) {
        *counter += 10;
        _exit(0);
    }
    waitpid(pid, NULL, 0);
    printf("shm counter: %ld\n", *counter);
    shm_unlink(shm_name);
    return 0;
}
