/*
 * Message queues (ADR-0044). System V: msgget (private), msgsnd of two
 * types, msgrcv by type and the first, a short buffer (E2BIG) and
 * MSG_NOERROR, IPC_NOWAIT on an empty queue, IPC_STAT (count, bytes),
 * IPC_RMID, msgrcv after removal. POSIX: mq_open with attributes,
 * mq_getsetattr, mq_timedsend with priorities, mq_timedreceive in
 * priority order, a timeout on an empty queue, a short buffer, mq_notify
 * with SIGEV_NONE twice (EBUSY), mq_unlink. Prints no ids.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/msg.h>
#include <time.h>
#include <unistd.h>

static void show(const char *what, long r) {
    if (r < 0) {
        printf("%-28s -1 %s\n", what, strerror(errno));
    } else {
        printf("%-28s %ld\n", what, r);
    }
}

struct message {
    long type;
    char text[32];
};

int main(void) {
    int id = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    printf("%-28s %s\n", "msgget", id >= 0 ? "ok" : strerror(errno));
    struct message m = {1, "first"};
    show("msgsnd type 1", msgsnd(id, &m, 6, 0));
    m.type = 2;
    strcpy(m.text, "second message");
    show("msgsnd type 2", msgsnd(id, &m, 15, 0));
    struct msqid_ds ds;
    show("IPC_STAT", msgctl(id, IPC_STAT, &ds));
    printf("%-28s %lu %lu\n", "qnum cbytes", (unsigned long)ds.msg_qnum, (unsigned long)ds.__msg_cbytes);
    struct message got = {0};
    show("msgrcv type 2 short", msgrcv(id, &got, 4, 2, 0));
    show("msgrcv type 2 NOERROR", msgrcv(id, &got, 4, 2, MSG_NOERROR));
    printf("%-28s %ld %.4s\n", "got", got.type, got.text);
    memset(&got, 0, sizeof got);
    show("msgrcv first", msgrcv(id, &got, sizeof got.text, 0, 0));
    printf("%-28s %ld %s\n", "got", got.type, got.text);
    show("msgrcv empty NOWAIT", msgrcv(id, &got, sizeof got.text, 0, IPC_NOWAIT));
    show("IPC_RMID", msgctl(id, IPC_RMID, NULL));
    show("msgrcv removed", msgrcv(id, &got, sizeof got.text, 0, IPC_NOWAIT));

    char name[64];
    snprintf(name, sizeof name, "/armor-mq-%d", getpid());
    struct mq_attr attr = {.mq_maxmsg = 4, .mq_msgsize = 32};
    mqd_t q = mq_open(name, O_CREAT | O_EXCL | O_RDWR, 0600, &attr);
    printf("%-28s %s\n", "mq_open", q != (mqd_t)-1 ? "ok" : strerror(errno));
    struct mq_attr now;
    show("mq_getattr", mq_getattr(q, &now));
    printf("%-28s %ld %ld %ld %ld\n", "attr", now.mq_flags, now.mq_maxmsg, now.mq_msgsize, now.mq_curmsgs);
    show("send low", mq_send(q, "low", 3, 1));
    show("send high", mq_send(q, "high", 4, 9));
    char buf[64] = {0};
    unsigned prio = 0;
    show("receive", mq_receive(q, buf, sizeof buf, &prio));
    printf("%-28s %s %u\n", "first", buf, prio);
    memset(buf, 0, sizeof buf);
    show("receive", mq_receive(q, buf, sizeof buf, &prio));
    printf("%-28s %s %u\n", "second", buf, prio);
    show("receive short buffer", mq_receive(q, buf, 8, &prio));
    struct timespec soon;
    clock_gettime(CLOCK_REALTIME, &soon);
    soon.tv_nsec += 5000000;
    if (soon.tv_nsec >= 1000000000) {
        soon.tv_sec++;
        soon.tv_nsec -= 1000000000;
    }
    show("timedreceive empty", mq_timedreceive(q, buf, sizeof buf, &prio, &soon));
    struct sigevent ev = {.sigev_notify = SIGEV_NONE};
    show("mq_notify", mq_notify(q, &ev));
    show("mq_notify again", mq_notify(q, &ev));
    show("mq_notify remove", mq_notify(q, NULL));
    struct mq_attr nonblock = {.mq_flags = O_NONBLOCK};
    show("mq_setattr", mq_setattr(q, &nonblock, &now));
    show("receive nonblock", mq_receive(q, buf, sizeof buf, &prio));
    show("mq_unlink", mq_unlink(name));
    show("mq_unlink again", mq_unlink(name));
    return 0;
}
