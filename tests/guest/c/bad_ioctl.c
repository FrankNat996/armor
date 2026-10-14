/*
 * An ioctl request armor does not know, on a descriptor that is not open:
 * Linux answers EBADF before looking at the request (checked natively on
 * the AArch64 host and on the x86-64 oracle, 2026-10-14); qemu answers
 * ENOTTY, so this runs against the hardware only (docs/testing.md). Mesa
 * makes such a call while probing the GPU.
 */
#include <errno.h>
#include <stdio.h>
#include <sys/ioctl.h>

int main(void) {
    errno = 0;
    int r = ioctl(-1, 0xc0106443UL, (void *)0);
    printf("unknown ioctl on -1: %d %s\n", r, errno == EBADF ? "EBADF" : "other");
    return 0;
}
