/*
 * The system's own calls of ADR-0044, as a user and as root alike: calls
 * that only an administrator may make print "ok or refused" (EPERM,
 * EACCES, or another error for root), print only that they answered, so
 * that the output does not depend on who runs the test; the
 * rest print their result. Nothing here changes the machine: every call
 * an administrator could make has a missing path or an invalid argument. Keys (add_key, keyctl read, describe, search,
 * revoke, request_key), NUMA (get_mempolicy, set_mempolicy, mbind,
 * move_pages), file handles, open_tree, statmount and listmount sizes,
 * lsm_list_modules, cachestat, perf_event_open, bpf, fanotify, mount and
 * the new mount API, chroot, pivot_root, swap, reboot, names, modules,
 * kexec, acct, quotactl, the clock, syslog. Prints no ids or names of the
 * machine.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/keyctl.h>
#include <linux/perf_event.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/quota.h>
#include <sys/syscall.h>
#include <sys/timex.h>
#include <time.h>
#include <unistd.h>

static void show(const char *what, long r) {
    if (r < 0) {
        printf("%-30s -1 %s\n", what, strerror(errno));
    } else {
        printf("%-30s %ld\n", what, r);
    }
}

/* For calls whose result depends on privilege or on the machine: only
   an answer that would mean the call did not reach the kernel properly
   (EFAULT, ENOSYS) is printed. */
static void privileged(const char *what, long r) {
    int bad = r < 0 && (errno == EFAULT || errno == ENOSYS);
    printf("%-30s %s\n", what, bad ? strerror(errno) : "answered");
}

int main(void) {
    long key = syscall(SYS_add_key, "user", "armor-test", "payload", 7, KEY_SPEC_PROCESS_KEYRING);
    printf("%-30s %s\n", "add_key", key > 0 ? "ok" : strerror(errno));
    char buf[256] = {0};
    show("keyctl read", syscall(SYS_keyctl, KEYCTL_READ, key, buf, sizeof buf));
    printf("%-30s %s\n", "payload", buf);
    show("keyctl read size", syscall(SYS_keyctl, KEYCTL_READ, key, NULL, 0));
    long n = syscall(SYS_keyctl, KEYCTL_DESCRIBE, key, buf, sizeof buf);
    char *last = strrchr(buf, ';');
    printf("%-30s %ld %.*s %s\n", "describe", n, (int)strcspn(buf, ";"), buf, last ? last + 1 : "?");
    long found = syscall(SYS_keyctl, KEYCTL_SEARCH, KEY_SPEC_PROCESS_KEYRING, "user", "armor-test", 0);
    printf("%-30s %d\n", "search finds it", found == key);
    show("revoke", syscall(SYS_keyctl, KEYCTL_REVOKE, key));
    show("read revoked", syscall(SYS_keyctl, KEYCTL_READ, key, buf, sizeof buf));
    show("request missing", syscall(SYS_request_key, "user", "armor-none", NULL, 0));
    show("keyctl bad op", syscall(SYS_keyctl, 9999, 0, 0, 0, 0));

    int mode = -1;
    unsigned long mask[16] = {0};
    show("get_mempolicy", syscall(SYS_get_mempolicy, &mode, mask, 1024, NULL, 0));
    printf("%-30s %d\n", "mode", mode);
    show("set_mempolicy default", syscall(SYS_set_mempolicy, 0, NULL, 0));
    char *page = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    page[0] = 1;
    show("mbind default", syscall(SYS_mbind, page, 65536, 0, NULL, 0, 0));
    void *pages[1] = {page};
    int status = -99;
    show("move_pages query", syscall(SYS_move_pages, 0, 1, pages, NULL, &status, 0));
    printf("%-30s %d\n", "node valid", status >= 0);

    char name[] = "armor-handle-XXXXXX";
    int fd = mkstemp(name);
    struct {
        unsigned int bytes;
        int type;
        unsigned char h[128];
    } handle = {.bytes = 128};
    int mount_id;
    show("name_to_handle_at", syscall(SYS_name_to_handle_at, AT_FDCWD, name, &handle, &mount_id, 0));
    unsigned int need = handle.bytes;
    handle.bytes = 0;
    show("name_to_handle_at no room", syscall(SYS_name_to_handle_at, AT_FDCWD, name, &handle, &mount_id, 0));
    printf("%-30s %d\n", "size told", handle.bytes == need);
    handle.bytes = need;
    privileged("open_by_handle_at", syscall(SYS_open_by_handle_at, AT_FDCWD, &handle, O_RDONLY));
    long tree = syscall(SYS_open_tree, AT_FDCWD, ".", 0x80000 /* OPEN_TREE_CLOEXEC */);
    printf("%-30s %s\n", "open_tree", tree >= 0 ? "ok" : strerror(errno));
    show("mount_setattr size 8", syscall(442, AT_FDCWD, ".", 0, buf, 8));
    show("mount_setattr size 8192", syscall(442, AT_FDCWD, ".", 0, buf, 8192));
    uint64_t range[2] = {0, 0};
    uint64_t cstat[5] = {0};
    write(fd, "cached", 6);
    show("cachestat", syscall(451, fd, range, cstat, 0));
    show("cachestat bad flags", syscall(451, fd, range, cstat, 1));
    uint32_t request[6] = {24, 0};
    show("listmount small request", syscall(458, request, cstat, 0, 0) < 0 && errno == EINVAL ? 0 : -1);
    uint32_t size = 0;
    long modules = syscall(461, NULL, &size, 0);
    printf("%-30s %s\n", "lsm_list_modules size", modules < 0 && errno == E2BIG && size > 0 ? "told" : strerror(errno));

    struct perf_event_attr attr = {.type = PERF_TYPE_SOFTWARE, .size = sizeof attr,
                                   .config = PERF_COUNT_SW_TASK_CLOCK, .disabled = 1,
                                   .exclude_kernel = 1, .exclude_hv = 1};
    long perf = syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0);
    privileged("perf_event_open", perf);
    attr.size = 8;
    show("perf_event_open size 8", syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0));
    privileged("bpf map create", syscall(SYS_bpf, 0, buf, 64));
    privileged("fanotify_init", syscall(SYS_fanotify_init, 0, O_RDONLY));
    privileged("fsopen tmpfs", syscall(SYS_fsopen, "tmpfs", 0));
    privileged("mount", syscall(SYS_mount, "none", "/armor-none", "tmpfs", 0, "size=1m"));
    privileged("umount2", syscall(SYS_umount2, "/armor-none", 0));
    privileged("chroot", syscall(SYS_chroot, "/armor-none"));
    privileged("pivot_root", syscall(SYS_pivot_root, "/armor-none", "/armor-none"));
    privileged("swapoff", syscall(SYS_swapoff, "/armor-none"));
    privileged("reboot bad magic", syscall(SYS_reboot, 0, 0, 0, NULL));
    privileged("sethostname too long", syscall(SYS_sethostname, buf, 200));
    privileged("delete_module", syscall(SYS_delete_module, "armor_none", 0));
    privileged("acct", syscall(SYS_acct, "/armor-none"));
    privileged("kexec_file_load", syscall(SYS_kexec_file_load, -1, -1, 0, NULL, 0));
    privileged("quotactl getfmt", syscall(SYS_quotactl, QCMD(Q_GETFMT, USRQUOTA), "/armor-none", 0, buf));
    struct timex tx = {0};
    long state = syscall(SYS_adjtimex, &tx);
    printf("%-30s %s\n", "adjtimex read", state >= 0 ? "ok" : strerror(errno));
    struct timespec invalid = {1, 2000000000};
    show("clock_settime invalid", syscall(SYS_clock_settime, CLOCK_REALTIME, &invalid));
    privileged("syslog size", syscall(SYS_syslog, 10, NULL, 0));
    unlink(name);
    return 0;
}
