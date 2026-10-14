/*
 * capget and capset, which Chromium's sandbox uses to drop its
 * capabilities (a Spotify build did): version 3 with two records, version
 * 1 with one, an unknown version (EINVAL, and the kernel's preferred
 * version written back), a NULL data pointer, and capset setting what
 * capget returned. Prints no capability bits, which depend on the user.
 */
#include <errno.h>
#include <linux/capability.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EINVAL: return "EINVAL";
    case EPERM: return "EPERM";
    case EFAULT: return "EFAULT";
    default: return "other";
    }
}

static void show(const char *what, long r) {
    printf("%s: %ld %s\n", what, r < 0 ? -1L : 0L, name(r < 0 ? errno : 0));
}

int main(void) {
    struct __user_cap_header_struct header = {_LINUX_CAPABILITY_VERSION_3, 0};
    struct __user_cap_data_struct data[2] = {{0}};
    show("capget v3", syscall(SYS_capget, &header, data));
    show("capset v3 same", syscall(SYS_capset, &header, data));
    header.version = _LINUX_CAPABILITY_VERSION_1;
    show("capget v1", syscall(SYS_capget, &header, data));
    header.version = 0x12345678;
    show("capget bad version", syscall(SYS_capget, &header, data));
    printf("  version written back %#x\n", header.version);
    header.version = 0x12345678;
    show("capget bad version, no data", syscall(SYS_capget, &header, NULL));
    printf("  version written back %#x\n", header.version);
    header.version = _LINUX_CAPABILITY_VERSION_3;
    show("capget bad pointer", syscall(SYS_capget, &header, (void *)8));
    return 0;
}
