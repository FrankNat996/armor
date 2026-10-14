/*
 * mremap growing a shared mapping of a file and of a memfd (Chromium grew
 * one, under a Spotify build): the file is extended with ftruncate, the
 * one-page mapping grown to three with MREMAP_MAYMOVE; the old bytes stay,
 * new writes reach the file (pread), and a child forked afterwards shares
 * the grown mapping. Without MREMAP_MAYMOVE and another mapping right after
 * it, growing fails with ENOMEM. The same source runs as x86-64 and i386.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static void grow(const char *what, int fd) {
    long page = 4096;
    if (ftruncate(fd, page) != 0) {
        return;
    }
    char *map = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    strcpy(map, "first page");
    if (ftruncate(fd, 3 * page) != 0) {
        return;
    }
    char *grown = mremap(map, page, 3 * page, MREMAP_MAYMOVE);
    printf("%s: mremap %s\n", what, grown == MAP_FAILED ? strerror(errno) : "ok");
    if (grown == MAP_FAILED) {
        return;
    }
    printf("  old bytes: %s\n", grown);
    strcpy(grown + 2 * page, "third page");
    char back[16] = {0};
    pread(fd, back, 10, 2 * page);
    printf("  file sees: %s\n", back);
    pid_t child = fork();
    if (child == 0) {
        strcpy(grown + page, "from child");
        _exit(0);
    }
    waitpid(child, NULL, 0);
    printf("  parent sees the child's write: %s\n", grown + page);
    munmap(grown, 3 * page);
}

int main(void) {
    char path[] = "/tmp/armor-mremap-XXXXXX";
    int fd = mkstemp(path);
    grow("file", fd);
    close(fd);
    unlink(path);
    int mfd = memfd_create("armor-mremap", 0);
    grow("memfd", mfd);

    /* No room after it and no MREMAP_MAYMOVE: ENOMEM. */
    char *area = mmap(NULL, 2 * 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    munmap(area, 4096);
    char *shared = mmap(area, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, mfd, 0);
    char *stuck = mremap(shared, 4096, 2 * 4096, 0);
    printf("grow into a mapping without MAYMOVE: %s\n", stuck == MAP_FAILED ? strerror(errno) : "ok");
    close(mfd);
    return 0;
}
