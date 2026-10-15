/*
 * Linux AIO with the raw syscalls (ADR-0044): io_setup (and a context id
 * that is not zero), io_submit of a pwrite, a pread and a preadv,
 * io_getevents and io_pgetevents without a mask, the results and
 * aio_data, an iocb whose buffer is not mapped (EFAULT), io_cancel of a
 * finished request, io_destroy and a destroyed context.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/aio_abi.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

static void show(const char *what, long r) {
    if (r < 0) {
        printf("%-26s -1 %s\n", what, strerror(errno));
    } else {
        printf("%-26s %ld\n", what, r);
    }
}

static long submit(aio_context_t ctx, struct iocb *cb) {
    struct iocb *list[1] = {cb};
    return syscall(SYS_io_submit, ctx, 1, list);
}

int main(void) {
    char path[] = "armor-aio-XXXXXX";
    int fd = mkstemp(path);
    aio_context_t ctx = 0;
    show("io_setup", syscall(SYS_io_setup, 8, &ctx));
    aio_context_t used = 1;
    show("io_setup nonzero id", syscall(SYS_io_setup, 8, &used));

    static char text[] = "hello aio";
    struct iocb w = {.aio_data = 11, .aio_lio_opcode = IOCB_CMD_PWRITE, .aio_fildes = fd,
                     .aio_buf = (uintptr_t)text, .aio_nbytes = 9, .aio_offset = 0};
    show("submit pwrite", submit(ctx, &w));
    struct io_event ev[4];
    show("io_getevents", syscall(SYS_io_getevents, ctx, 1, 4, ev, NULL));
    printf("%-26s %llu %lld\n", "data res", (unsigned long long)ev[0].data, (long long)ev[0].res);

    static char got[16];
    struct iocb r = {.aio_data = 22, .aio_lio_opcode = IOCB_CMD_PREAD, .aio_fildes = fd,
                     .aio_buf = (uintptr_t)got, .aio_nbytes = 5, .aio_offset = 4};
    show("submit pread", submit(ctx, &r));
    show("io_pgetevents", syscall(SYS_io_pgetevents, ctx, 1, 4, ev, NULL, NULL));
    printf("%-26s %llu %lld %s\n", "data res text", (unsigned long long)ev[0].data, (long long)ev[0].res, got);

    static char a[3], b[4];
    struct iovec v[2] = {{a, 2}, {b, 3}};
    struct iocb rv = {.aio_data = 33, .aio_lio_opcode = IOCB_CMD_PREADV, .aio_fildes = fd,
                      .aio_buf = (uintptr_t)v, .aio_nbytes = 2, .aio_offset = 0};
    show("submit preadv", submit(ctx, &rv));
    show("io_getevents", syscall(SYS_io_getevents, ctx, 1, 4, ev, NULL));
    printf("%-26s %lld %s %s\n", "res parts", (long long)ev[0].res, a, b);

    char *gone = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    munmap(gone, 4096);
    struct iocb bad = {.aio_lio_opcode = IOCB_CMD_PREAD, .aio_fildes = fd,
                       .aio_buf = (uintptr_t)gone, .aio_nbytes = 4};
    long rbad = submit(ctx, &bad);
    if (rbad == 1) {
        /* The kernel takes it and reports the fault in the event. */
        syscall(SYS_io_getevents, ctx, 1, 1, ev, NULL);
        printf("%-26s %lld\n", "unmapped buffer", (long long)ev[0].res);
    } else {
        show("unmapped buffer", rbad);
    }
    show("io_cancel finished", syscall(SYS_io_cancel, ctx, &w, &ev[0]));
    show("io_getevents min > nr", syscall(SYS_io_getevents, ctx, 4, 2, ev, NULL));
    show("io_destroy", syscall(SYS_io_destroy, ctx));
    show("io_getevents destroyed", syscall(SYS_io_getevents, ctx, 0, 1, ev, NULL));
    unlink(path);
    return 0;
}
