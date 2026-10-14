/*
 * System V shared memory, as X11's MIT-SHM extension uses it (a Steam
 * game's libX11 asked for it): shmget, shmat at an address the kernel
 * picks, writes seen by a forked child that attaches the same segment and
 * by the parent after the child writes, IPC_STAT (size, attachments),
 * shmat at a given address (SHM_RND rounds it down; without SHM_REMAP an
 * address already mapped is EINVAL), SHM_RDONLY, shmdt (EINVAL where no
 * attachment starts), IPC_RMID. Prints no ids or addresses.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/mman.h>
#include <sys/shm.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EINVAL: return "EINVAL";
    case EIDRM: return "EIDRM";
    case ENOENT: return "ENOENT";
    default: return "other";
    }
}

static long show(const char *what, long result) {
    printf("%s: %ld %s\n", what, result < 0 ? -1 : 0, name(result < 0 ? errno : 0));
    return result;
}

int main(void) {
    int id = shmget(IPC_PRIVATE, 3 * 4096, IPC_CREAT | 0600);
    show("shmget", id);
    char *p = shmat(id, NULL, 0);
    show("shmat", p == (void *)-1 ? -1 : 0);
    strcpy(p, "parent wrote");
    strcpy(p + 2 * 4096, "last page");
    struct shmid_ds ds;
    show("ipc_stat", shmctl(id, IPC_STAT, &ds));
    printf("  size %zu attached %lu\n", (size_t)ds.shm_segsz, (unsigned long)ds.shm_nattch);

    pid_t child = fork();
    if (child == 0) {
        char *q = shmat(id, NULL, 0);
        int ok = q != (void *)-1 && strcmp(q, "parent wrote") == 0 && strcmp(q + 8192, "last page") == 0;
        strcpy(q + 100, "child wrote");
        shmdt(q);
        _exit(ok ? 0 : 1);
    }
    int status = 0;
    waitpid(child, &status, 0);
    printf("child saw the parent's writes: %d\n", WEXITSTATUS(status) == 0);
    printf("parent sees: %s\n", p + 100);

    /* A given address: rounded down with SHM_RND; taken without REMAP. */
    char *area = mmap(NULL, 8 * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    char *base = area + 4 * 4096;
    munmap(base, 4 * 4096);
    char *again = shmat(id, base + 100, SHM_RND | SHM_RDONLY);
    printf("shmat rounded down: %d\n", again == base);
    printf("  same contents: %d\n", again != (void *)-1 && strcmp(again, "parent wrote") == 0);
    show("shmat over a mapping", (long)shmat(id, area, 0));
    show("shmat unaligned", (long)shmat(id, base + 100, 0));
    show("shmdt", shmdt(again));
    show("shmdt nothing there", shmdt(base));

    show("ipc_rmid", shmctl(id, IPC_RMID, NULL));
    show("still attached", strcmp(p, "parent wrote") == 0 ? 0 : -1);
    show("shmdt", shmdt(p));
    show("ipc_stat removed", shmctl(id, IPC_STAT, &ds));
    return 0;
}
