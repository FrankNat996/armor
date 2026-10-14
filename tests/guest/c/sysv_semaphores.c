/*
 * System V semaphores through glibc: a private set of three, SETVAL,
 * GETVAL, SETALL/GETALL, semop (also several operations at once and
 * IPC_NOWAIT), semtimedop with a timeout that expires, GETPID, GETNCNT
 * with a waiting child, and IPC_RMID. The Steam client uses them (with
 * the i386 `ipc` call). The same source runs as x86-64 and i386.
 * Prints no ids or pids.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/sem.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

union semun {
    int val;
    struct semid_ds *buf;
    unsigned short *array;
};

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EAGAIN: return "EAGAIN";
    case EINVAL: return "EINVAL";
    case EIDRM: return "EIDRM";
    case E2BIG: return "E2BIG";
    case ERANGE: return "ERANGE";
    default: return "other";
    }
}

static int show(const char *what, int result) {
    printf("%s: %d %s\n", what, result, name(result < 0 ? errno : 0));
    return result;
}

int main(void) {
    int id = semget(IPC_PRIVATE, 3, IPC_CREAT | 0600);
    if (id < 0) {
        perror("semget");
        return 1;
    }
    show("setval", semctl(id, 0, SETVAL, (union semun){.val = 2}));
    show("getval", semctl(id, 0, GETVAL));
    show("setval too big", semctl(id, 0, SETVAL, (union semun){.val = 40000}));
    unsigned short all[3] = {5, 6, 7};
    show("setall", semctl(id, 0, SETALL, (union semun){.array = all}));
    memset(all, 0, sizeof all);
    show("getall", semctl(id, 0, GETALL, (union semun){.array = all}));
    printf("  values %u %u %u\n", all[0], all[1], all[2]);

    struct sembuf two[2] = {{0, -5, 0}, {2, 1, 0}};
    show("semop two", semop(id, two, 2));
    printf("  values %d %d\n", semctl(id, 0, GETVAL), semctl(id, 2, GETVAL));
    struct sembuf take = {0, -1, IPC_NOWAIT};
    show("semop nowait", semop(id, &take, 1));
    take.sem_flg = 0;
    struct timespec ts = {0, 20000000};
    show("semtimedop expires", semtimedop(id, &take, 1, &ts));
    show("semop no operations", semop(id, &take, 0));
    printf("getpid is self: %d\n", semctl(id, 2, GETPID) == getpid());

    /* A child waits on semaphore 1 (6 - 7 < 0); the parent sees it in
     * GETNCNT, then releases it. */
    show("setval 1", semctl(id, 1, SETVAL, (union semun){.val = 0}));
    pid_t child = fork();
    if (child == 0) {
        struct sembuf wait = {1, -1, 0};
        _exit(semop(id, &wait, 1) == 0 ? 0 : 1);
    }
    int waiting = 0;
    for (int i = 0; i < 200 && waiting == 0; i++) {
        waiting = semctl(id, 1, GETNCNT);
        usleep(5000);
    }
    printf("child waiting: %d\n", waiting);
    struct sembuf post = {1, 1, 0};
    show("semop post", semop(id, &post, 1));
    int status = 0;
    waitpid(child, &status, 0);
    printf("child exit: %d\n", WEXITSTATUS(status));
    printf("getpid is child: %d\n", semctl(id, 1, GETPID) == child);

    show("rmid", semctl(id, 0, IPC_RMID));
    show("getval removed", semctl(id, 0, GETVAL));
    return 0;
}
