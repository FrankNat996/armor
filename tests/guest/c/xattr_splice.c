/*
 * Extended attributes and in-kernel transfers (ADR-0044): set, get (size
 * query, short buffer), list (user.* names only), the descriptor forms,
 * XATTR_CREATE and XATTR_REPLACE, remove, and the errors (missing, empty
 * or long names, a value too large, a missing file); then splice with an
 * offset, tee, vmsplice in both directions, vmsplice on a file, and
 * copy_file_range with offsets. Files are made in the working directory.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/xattr.h>
#include <unistd.h>

static void show(const char *what, long r) {
    if (r < 0) {
        printf("%-30s -1 %s\n", what, strerror(errno));
    } else {
        printf("%-30s %ld\n", what, r);
    }
}

int main(void) {
    char path[] = "armor-xattr-XXXXXX";
    int fd = mkstemp(path);
    long r = setxattr(path, "user.armor", "value", 5, 0);
    if (r < 0 && errno == ENOTSUP) {
        printf("user xattrs not supported here\n");
    }
    show("setxattr", r);
    show("getxattr size", getxattr(path, "user.armor", NULL, 0));
    char buf[256] = {0};
    show("getxattr", getxattr(path, "user.armor", buf, sizeof buf));
    printf("%-30s %s\n", "value", buf);
    show("getxattr short", getxattr(path, "user.armor", buf, 2));
    show("fsetxattr", fsetxattr(fd, "user.second", "2", 1, 0));
    char list[512];
    long n = listxattr(path, list, sizeof list);
    printf("%-30s", "listxattr user names");
    for (long i = 0; i < n; i += strlen(list + i) + 1) {
        if (strncmp(list + i, "user.", 5) == 0) {
            printf(" %s", list + i);
        }
    }
    printf("\n");
    show("flistxattr size >= 23", flistxattr(fd, NULL, 0) >= 23 ? 0 : -1);
    show("fgetxattr", fgetxattr(fd, "user.second", buf, sizeof buf));
    show("create existing", setxattr(path, "user.armor", "x", 1, XATTR_CREATE));
    show("replace missing", setxattr(path, "user.none", "x", 1, XATTR_REPLACE));
    show("removexattr", removexattr(path, "user.armor"));
    show("fremovexattr", fremovexattr(fd, "user.second"));
    show("getxattr removed", getxattr(path, "user.armor", buf, sizeof buf));
    show("lgetxattr missing", lgetxattr(path, "user.armor", buf, sizeof buf));
    show("empty name", setxattr(path, "", "x", 1, 0));
    char long_name[300];
    memset(long_name, 'a', sizeof long_name - 1);
    memcpy(long_name, "user.", 5);
    long_name[sizeof long_name - 1] = 0;
    show("long name", setxattr(path, long_name, "x", 1, 0));
    static char big[65537];
    show("value too large", setxattr(path, "user.big", big, sizeof big, 0));
    show("missing file", getxattr("armor-no-such-file", "user.armor", buf, sizeof buf));

    /* splice from the file into a pipe, at an offset. */
    write(fd, "0123456789abcdef", 16);
    int p[2], q[2];
    pipe(p);
    pipe(q);
    loff_t off = 4;
    show("splice", splice(fd, &off, p[1], NULL, 6, 0));
    printf("%-30s %lld\n", "offset after", (long long)off);
    show("tee", tee(p[0], q[1], 6, 0));
    char got[32] = {0};
    show("read pipe", read(p[0], got, sizeof got));
    printf("%-30s %s\n", "pipe holds", got);
    memset(got, 0, sizeof got);
    show("read tee copy", read(q[0], got, sizeof got));
    printf("%-30s %s\n", "copy holds", got);

    char a[] = "vm", b[] = "splice";
    struct iovec in[2] = {{a, 2}, {b, 6}};
    show("vmsplice to pipe", vmsplice(p[1], in, 2, 0));
    char x[3] = {0}, y[6] = {0};
    struct iovec out[2] = {{x, 2}, {y, 5}};
    show("vmsplice from pipe", vmsplice(p[0], out, 2, 0));
    printf("%-30s %s %s\n", "got", x, y);
    show("vmsplice nonblock empty", vmsplice(q[0], out, 2, SPLICE_F_NONBLOCK));
    show("vmsplice on a file", vmsplice(fd, in, 2, 0));

    char other[] = "armor-copy-XXXXXX";
    int fd2 = mkstemp(other);
    loff_t from = 10, to = 2;
    show("copy_file_range", copy_file_range(fd, &from, fd2, &to, 6, 0));
    printf("%-30s %lld %lld\n", "offsets after", (long long)from, (long long)to);
    memset(got, 0, sizeof got);
    show("pread copy", pread(fd2, got, sizeof got, 2));
    printf("%-30s %s\n", "copy", got);
    show("copy_file_range bad flags", copy_file_range(fd, NULL, fd2, NULL, 1, 1));
    unlink(path);
    unlink(other);
    return 0;
}
