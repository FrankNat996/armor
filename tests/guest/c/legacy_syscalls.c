/*
 * x86-64 syscalls that AArch64 does not have (docs/tech/syscalls.md),
 * called by number so that glibc cannot pick a newer one: file and
 * directory calls, stat/lstat (struct stat differs between the two),
 * the old getdents format, select, alarm, epoll (struct epoll_event is
 * packed on x86-64), eventfd, inotify, and the obsolete calls Linux
 * answers with a fixed error (iopl and ioperm: ENOSYS, as from
 * qemu-x86_64; armor never grants port I/O). Prints results and errno names, never
 * inode numbers, times or the temporary directory's name.
 */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <unistd.h>
#include <utime.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case ENOENT: return "ENOENT";
    case EEXIST: return "EEXIST";
    case ENOTDIR: return "ENOTDIR";
    case EISDIR: return "EISDIR";
    case ENOTEMPTY: return "ENOTEMPTY";
    case EINVAL: return "EINVAL";
    case ENOSYS: return "ENOSYS";
    case EPERM: return "EPERM";
    case EBADF: return "EBADF";
    case EFAULT: return "EFAULT";
    case EACCES: return "EACCES";
    default: return "other";
    }
}

/* Prints "label: result errno-name". */
static long show(const char *label, long r) {
    printf("%-22s %ld %s\n", label, r, name(r < 0 ? errno : 0));
    return r;
}

static void show_stat(const char *label, const struct stat *st) {
    printf("%-22s mode %o nlink %lu size %ld blksize-ok %d\n", label, st->st_mode,
           (unsigned long)st->st_nlink, (long)st->st_size, st->st_blksize > 0);
}

/* struct epoll_event as the x86-64 kernel sees it: 12 bytes, packed. */
struct packed_event {
    uint32_t events;
    uint64_t data;
} __attribute__((packed));

static volatile sig_atomic_t alarms;
static void on_alarm(int sig) {
    (void)sig;
    alarms++;
}

int main(void) {
    char dir[] = "/tmp/armor-legacy-XXXXXX";
    if (!mkdtemp(dir) || chdir(dir) != 0)
        return 1;

    /* Files and directories. */
    struct stat st;
    show("mkdir", syscall(SYS_mkdir, "d", 0750));
    show("mkdir again", syscall(SYS_mkdir, "d", 0750));
    long fd = show("creat", syscall(SYS_creat, "d/f", 0640));
    show("write", write((int)fd, "hello", 5));
    close((int)fd);
    fd = syscall(SYS_creat, "d/t", 0600);
    write((int)fd, "longer content", 14);
    close((int)fd);
    fd = show("creat existing", syscall(SYS_creat, "d/t", 0600));
    close((int)fd);
    syscall(SYS_stat, "d/t", &st);
    printf("  truncated to %ld\n", (long)st.st_size);
    unlink("d/t");
    show("link", syscall(SYS_link, "d/f", "d/g"));
    show("symlink", syscall(SYS_symlink, "f", "d/s"));
    show("stat", syscall(SYS_stat, "d/s", &st));
    show_stat("  stat of link", &st);
    show("lstat", syscall(SYS_lstat, "d/s", &st));
    show_stat("  lstat of link", &st);
    show("stat missing", syscall(SYS_stat, "d/missing", &st));
    show("chmod", syscall(SYS_chmod, "d/f", 0600));
    syscall(SYS_stat, "d/f", &st);
    show_stat("  after chmod", &st);
    show("chown to self", syscall(SYS_chown, "d/f", getuid(), getgid()));
    show("lchown to self", syscall(SYS_lchown, "d/s", getuid(), getgid()));
    /* A dangling link: lchown changes the link, chown follows it. */
    syscall(SYS_symlink, "nowhere", "d/dangling");
    show("lchown dangling", syscall(SYS_lchown, "d/dangling", getuid(), getgid()));
    show("chown dangling", syscall(SYS_chown, "d/dangling", getuid(), getgid()));
    unlink("d/dangling");
    struct utimbuf ut = {.actime = 1000000000, .modtime = 1234567890};
    show("utime", syscall(SYS_utime, "d/f", &ut));
    syscall(SYS_stat, "d/f", &st);
    printf("  mtime %ld atime %ld\n", (long)st.st_mtime, (long)st.st_atime);
    struct timeval tv[2] = {{1500000000, 250000}, {1600000000, 750000}};
    show("utimes", syscall(SYS_utimes, "d/f", tv));
    syscall(SYS_stat, "d/f", &st);
    printf("  mtime %ld.%09ld\n", (long)st.st_mtime, st.st_mtim.tv_nsec);
    show("futimesat", syscall(SYS_futimesat, AT_FDCWD, "d/g", NULL));
    show("mknod fifo", syscall(SYS_mknod, "d/p", S_IFIFO | 0600, 0));
    syscall(SYS_lstat, "d/p", &st);
    printf("  fifo %d\n", S_ISFIFO(st.st_mode));
    show("rename", syscall(SYS_rename, "d/g", "d/h"));
    show("rename missing", syscall(SYS_rename, "d/g", "d/i"));
    show("rmdir not empty", syscall(SYS_rmdir, "d"));

    /* The old getdents format: d_type is the last byte of each record. */
    int dfd = open("d", O_RDONLY | O_DIRECTORY);
    char buf[4096];
    long n = syscall(SYS_getdents, dfd, buf, sizeof buf);
    int entries = 0, regular = 0, links = 0, fifos = 0, names_ok = 0;
    for (long off = 0; off < n;) {
        unsigned short reclen = *(unsigned short *)(buf + off + 16);
        const char *nm = buf + off + 18;
        unsigned char type = (unsigned char)buf[off + reclen - 1];
        entries++;
        regular += type == DT_REG;
        links += type == DT_LNK;
        fifos += type == DT_FIFO;
        names_ok += strlen(nm) + 20 <= reclen && reclen % 8 == 0;
        off += reclen;
    }
    printf("getdents %d entries, %d regular, %d links, %d fifos, %d well-formed\n", entries,
           regular, links, fifos, names_ok);
    lseek(dfd, 0, SEEK_SET);
    show("getdents tiny buffer", syscall(SYS_getdents, dfd, buf, 8));
    close(dfd);

    show("unlink f", unlink("d/f"));
    unlink("d/h");
    unlink("d/s");
    unlink("d/p");
    show("rmdir", syscall(SYS_rmdir, "d"));

    /* select on a pipe: not ready, then ready; the timeout is updated. */
    int p[2];
    pipe(p);
    fd_set rd;
    FD_ZERO(&rd);
    FD_SET(p[0], &rd);
    struct timeval timeout = {0, 1000};
    show("select empty pipe", syscall(SYS_select, p[0] + 1, &rd, NULL, NULL, &timeout));
    printf("  timeout left %ld.%06ld\n", (long)timeout.tv_sec, (long)timeout.tv_usec);
    write(p[1], "x", 1);
    FD_ZERO(&rd);
    FD_SET(p[0], &rd);
    timeout.tv_sec = 5;
    timeout.tv_usec = 0;
    struct timeval negative = {0, -1};
    show("select negative", syscall(SYS_select, 0, NULL, NULL, NULL, &negative));
    show("select ready pipe", syscall(SYS_select, p[0] + 1, &rd, NULL, NULL, &timeout));
    printf("  readable %d\n", FD_ISSET(p[0], &rd));

    /* epoll with the packed x86-64 event layout. */
    show("epoll_create 0", syscall(SYS_epoll_create, 0));
    long ep = show("epoll_create", syscall(SYS_epoll_create, 1));
    struct packed_event ev = {.events = EPOLLIN, .data = 0x1122334455667788ull};
    show("epoll_ctl add", syscall(SYS_epoll_ctl, ep, EPOLL_CTL_ADD, p[0], &ev));
    struct packed_event out[4];
    memset(out, 0xee, sizeof out);
    show("epoll_wait", syscall(SYS_epoll_wait, ep, out, 4, 1000));
    printf("  events %#x data %#llx next %#x\n", out[0].events,
           (unsigned long long)out[0].data, out[1].events);
    show("epoll_pwait", syscall(SYS_epoll_pwait, ep, out, 4, 0, NULL, 8));
    show("epoll_wait 0 events", syscall(SYS_epoll_wait, ep, out, 0, 0));
    close((int)ep);

    /* eventfd and inotify. */
    long efd = show("eventfd", syscall(SYS_eventfd, 3));
    uint64_t count = 0;
    read((int)efd, &count, sizeof count);
    printf("  count %llu\n", (unsigned long long)count);
    close((int)efd);
    long ifd = show("inotify_init", syscall(SYS_inotify_init));
    show("inotify_add_watch", syscall(SYS_inotify_add_watch, ifd, ".", 0x100 /* IN_CREATE */) > 0);
    close(open("new", O_CREAT | O_WRONLY, 0600));
    char ibuf[256];
    long got = read((int)ifd, ibuf, sizeof ibuf);
    printf("  inotify event mask %#x name %s\n", got > 16 ? *(uint32_t *)(ibuf + 4) : 0,
           got > 16 ? ibuf + 16 : "-");
    unlink("new");
    close((int)ifd);

    /* alarm: the previous alarm's remaining seconds, rounded. */
    signal(SIGALRM, on_alarm);
    show("alarm 100", syscall(SYS_alarm, 100));
    show("alarm 0", syscall(SYS_alarm, 0));
    syscall(SYS_alarm, 1);
    pause();
    printf("  alarms %d\n", (int)alarms);

    /* Calls Linux answers with a fixed error. */
    show("create_module", syscall(SYS_create_module, 0, 0));
    show("tuxcall", syscall(SYS_tuxcall));
    show("epoll_wait_old", syscall(SYS_epoll_wait_old, 0, 0, 0));
    show("iopl", syscall(SYS_iopl, 3));
    show("ioperm", syscall(SYS_ioperm, 0x80, 1, 1));

    chdir("/");
    show("rmdir temp", syscall(SYS_rmdir, dir) == 0);
    return 0;
}
