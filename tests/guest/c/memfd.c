/*
 * memfd_create: an anonymous file in memory, as graphics and audio
 * libraries use for shared buffers (a Steam game's SDL asked for it):
 * write, size, a shared mapping that sees the writes and whose stores the
 * file sees, seals (F_ADD_SEALS, F_GET_SEALS) that then forbid writing
 * and growing, and the errors. The same source runs as x86-64 and i386.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EPERM: return "EPERM";
    case EINVAL: return "EINVAL";
    case EFAULT: return "EFAULT";
    default: return "other";
    }
}

static long show(const char *what, long result) {
    printf("%s: %ld %s\n", what, result, name(result < 0 ? errno : 0));
    return result;
}

int main(void) {
    int fd = memfd_create("armor-test", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    show("memfd_create", fd >= 0 ? 0 : -1);
    printf("cloexec: %d\n", (fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0);
    show("write", write(fd, "hello memfd", 11));
    show("ftruncate", ftruncate(fd, 8192));
    struct stat st;
    fstat(fd, &st);
    printf("size: %lld\n", (long long)st.st_size);
    char *map = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    show("mmap shared", map == MAP_FAILED ? -1 : 0);
    if (map == MAP_FAILED) {
        return 1;
    }
    printf("mapped: %.11s\n", map);
    memcpy(map + 4096, "via the mapping", 15);
    char back[16] = {0};
    pread(fd, back, 15, 4096);
    printf("file sees: %s\n", back);
    munmap(map, 8192);

    show("get seals", fcntl(fd, F_GET_SEALS));
    show("add write and grow seals", fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE | F_SEAL_GROW));
    show("get seals", fcntl(fd, F_GET_SEALS));
    show("write sealed", write(fd, "x", 1));
    show("grow sealed", ftruncate(fd, 16384));
    show("seal seal", fcntl(fd, F_ADD_SEALS, F_SEAL_SEAL));
    show("add after seal", fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK));
    close(fd);

    int plain = memfd_create("no-seals", 0);
    show("seal without ALLOW_SEALING", fcntl(plain, F_ADD_SEALS, F_SEAL_WRITE));
    close(plain);
    show("bad flags", memfd_create("bad", 0x1000));
    show("bad name", memfd_create((const char *)8, 0));
    return 0;
}
