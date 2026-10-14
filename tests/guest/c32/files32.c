/*
 * i386 file syscalls called by number, so that glibc cannot pick newer
 * ones: stat64, lstat64, fstat64 and fstatat64 (the 96-byte struct
 * stat64, compared field by field with statx), link, rename, renameat2,
 * symlink, chmod, fchmodat, mknod (a fifo), mknodat, mkdirat, chown32 and
 * lchown32 to oneself, utimensat with 32-bit and 64-bit timespecs. The
 * Steam client calls several of them. Prints no inode numbers, devices,
 * times or the temporary directory's name: only comparisons.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/sysmacros.h>
#include <unistd.h>

/* The kernel's i386 struct stat64 (asm/stat.h). */
struct kstat64 {
    unsigned long long st_dev;
    unsigned char pad0[4];
    unsigned long small_ino;
    unsigned int st_mode;
    unsigned int st_nlink;
    unsigned long st_uid;
    unsigned long st_gid;
    unsigned long long st_rdev;
    unsigned char pad3[4];
    long long st_size;
    unsigned long st_blksize;
    unsigned long long st_blocks;
    unsigned long atime, atime_nsec, mtime, mtime_nsec, ctime, ctime_nsec;
    unsigned long long st_ino;
};

/* The kernel's 32-bit timespec of utimensat (not glibc's). */
struct ts32 {
    int32_t sec, nsec;
};
struct ts64 {
    int64_t sec, nsec;
};

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case ENOENT: return "ENOENT";
    case EEXIST: return "EEXIST";
    case EINVAL: return "EINVAL";
    case EBADF: return "EBADF";
    default: return "other";
    }
}

static long show(const char *what, long result) {
    printf("%s: %ld %s\n", what, result, name(result < 0 ? errno : 0));
    return result;
}

/* Field-by-field comparison with statx of the same path. */
static void compare(const char *what, const struct kstat64 *k, const char *path, int flags) {
    struct statx x;
    if (statx(AT_FDCWD, path, flags, STATX_BASIC_STATS, &x) != 0) {
        printf("%s: statx failed\n", what);
        return;
    }
    printf("%s: size %lld mode %o nlink %u; dev %d ino %d small ino %d uid %d gid %d rdev %d "
           "blocks %d blksize %d atime %d mtime %d ctime %d\n",
           what, k->st_size, k->st_mode, k->st_nlink,
           k->st_dev == makedev(x.stx_dev_major, x.stx_dev_minor), k->st_ino == x.stx_ino,
           k->small_ino == (unsigned long)x.stx_ino, k->st_uid == x.stx_uid,
           k->st_gid == x.stx_gid,
           k->st_rdev == makedev(x.stx_rdev_major, x.stx_rdev_minor),
           k->st_blocks == x.stx_blocks, k->st_blksize == x.stx_blksize,
           k->atime == (unsigned long)x.stx_atime.tv_sec && k->atime_nsec == x.stx_atime.tv_nsec,
           k->mtime == (unsigned long)x.stx_mtime.tv_sec && k->mtime_nsec == x.stx_mtime.tv_nsec,
           k->ctime == (unsigned long)x.stx_ctime.tv_sec && k->ctime_nsec == x.stx_ctime.tv_nsec);
}

int main(void) {
    char dir[] = "/tmp/armor-files32-XXXXXX";
    if (!mkdtemp(dir) || chdir(dir) != 0) {
        perror("mkdtemp");
        return 1;
    }
    printf("sizeof stat64: %zu\n", sizeof(struct kstat64));
    int fd = open("f", O_CREAT | O_WRONLY, 0640);
    if (fd < 0 || write(fd, "twelve bytes", 12) != 12) {
        return 1;
    }
    struct kstat64 k;
    memset(&k, 0, sizeof k);
    show("stat64", syscall(SYS_stat64, "f", &k));
    compare("  stat64", &k, "f", 0);
    show("fstat64", syscall(SYS_fstat64, fd, &k));
    compare("  fstat64", &k, "f", 0);
    show("fstat64 bad descriptor", syscall(SYS_fstat64, -1, &k));
    show("stat64 missing", syscall(SYS_stat64, "missing", &k));

    show("symlink", syscall(SYS_symlink, "f", "s"));
    show("symlink again", syscall(SYS_symlink, "f", "s"));
    show("lstat64", syscall(SYS_lstat64, "s", &k));
    compare("  lstat64", &k, "s", AT_SYMLINK_NOFOLLOW);
    printf("  is link %d\n", S_ISLNK(k.st_mode));
    show("fstatat64 nofollow", syscall(SYS_fstatat64, AT_FDCWD, "s", &k, AT_SYMLINK_NOFOLLOW));
    printf("  is link %d\n", S_ISLNK(k.st_mode));
    show("fstatat64 follow", syscall(SYS_fstatat64, AT_FDCWD, "s", &k, 0));
    printf("  is link %d size %lld\n", S_ISLNK(k.st_mode), k.st_size);

    show("link", syscall(SYS_link, "f", "g"));
    syscall(SYS_stat64, "f", &k);
    printf("  nlink %u\n", k.st_nlink);
    show("rename", syscall(SYS_rename, "g", "h"));
    show("rename missing", syscall(SYS_rename, "g", "i"));
    show("renameat2 noreplace", syscall(SYS_renameat2, AT_FDCWD, "h", AT_FDCWD, "f",
                                        RENAME_NOREPLACE));
    show("chmod", syscall(SYS_chmod, "f", 0600));
    syscall(SYS_stat64, "f", &k);
    printf("  mode %o\n", k.st_mode);
    show("fchmodat", syscall(SYS_fchmodat, AT_FDCWD, "f", 0644));
    syscall(SYS_stat64, "f", &k);
    printf("  mode %o\n", k.st_mode);
    show("chown32 to self", syscall(SYS_chown32, "f", getuid(), getgid()));
    show("lchown32 to self", syscall(SYS_lchown32, "s", getuid(), getgid()));
    show("chown32 unchanged", syscall(SYS_chown32, "f", -1, -1));

    show("mknod fifo", syscall(SYS_mknod, "p", S_IFIFO | 0600, 0));
    syscall(SYS_lstat64, "p", &k);
    printf("  is fifo %d\n", S_ISFIFO(k.st_mode));
    show("mkdirat", syscall(SYS_mkdirat, AT_FDCWD, "d", 0750));
    show("mknodat fifo", syscall(SYS_mknodat, AT_FDCWD, "d/q", S_IFIFO | 0600, 0));

    struct ts32 t32[2] = {{1000000000, 5}, {1100000000, 7}};
    show("utimensat", syscall(SYS_utimensat, AT_FDCWD, "f", t32, 0));
    syscall(SYS_stat64, "f", &k);
    printf("  atime %lu.%lu mtime %lu.%lu\n", k.atime, k.atime_nsec, k.mtime, k.mtime_nsec);
    struct ts32 omit[2] = {{0, UTIME_OMIT}, {1200000000, 9}};
    show("utimensat omit", syscall(SYS_utimensat, AT_FDCWD, "f", omit, 0));
    syscall(SYS_stat64, "f", &k);
    printf("  atime %lu.%lu mtime %lu.%lu\n", k.atime, k.atime_nsec, k.mtime, k.mtime_nsec);
    struct ts32 bad[2] = {{0, 1000000000}, {0, 0}};
    show("utimensat bad nanoseconds", syscall(SYS_utimensat, AT_FDCWD, "f", bad, 0));
    struct ts64 t64[2] = {{1300000000, 11}, {1400000000, 13}};
    show("utimensat_time64", syscall(SYS_utimensat_time64, AT_FDCWD, "f", t64, 0));
    syscall(SYS_stat64, "f", &k);
    printf("  atime %lu.%lu mtime %lu.%lu\n", k.atime, k.atime_nsec, k.mtime, k.mtime_nsec);

    unlink("d/q");
    rmdir("d");
    unlink("p");
    unlink("h");
    unlink("s");
    unlink("f");
    chdir("/");
    show("rmdir", rmdir(dir));
    return 0;
}
