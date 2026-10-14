/*
 * Two descriptors passed with SCM_RIGHTS to a receiver with room for
 * exactly one (CMSG_LEN, not CMSG_SPACE, which holds two on x86-64):
 * Linux passes the first, sets MSG_CTRUNC and releases the second, so the
 * lowest free descriptor is right after the first. qemu passes one
 * without MSG_CTRUNC and leaves the second open: compared with the
 * hardware only (docs/testing.md). The i386 control messages are
 * converted by armor (ADR-0029).
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

int main(void) {
    int sv[2], pipefd[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0 || pipe(pipefd) != 0) {
        perror("setup");
        return 1;
    }
    char text[] = "two";
    struct iovec out = {text, 3};
    union {
        struct cmsghdr header;
        char bytes[CMSG_SPACE(2 * sizeof(int))];
    } two;
    memset(&two, 0, sizeof two);
    struct msghdr pair = {.msg_iov = &out, .msg_iovlen = 1, .msg_control = two.bytes,
                          .msg_controllen = sizeof two.bytes};
    struct cmsghdr *c = CMSG_FIRSTHDR(&pair);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(2 * sizeof(int));
    memcpy(CMSG_DATA(c), pipefd, sizeof pipefd);
    printf("sendmsg: %zd\n", sendmsg(sv[0], &pair, 0));

    char buf[8];
    struct iovec in = {buf, sizeof buf};
    union {
        struct cmsghdr header;
        char bytes[CMSG_SPACE(2 * sizeof(int))];
    } one;
    struct msghdr room = {.msg_iov = &in, .msg_iovlen = 1, .msg_control = one.bytes,
                          .msg_controllen = CMSG_LEN(sizeof(int))};
    ssize_t n = recvmsg(sv[1], &room, 0);
    printf("recvmsg: %zd ctrunc %d\n", n, (room.msg_flags & MSG_CTRUNC) != 0);
    struct cmsghdr *r = CMSG_FIRSTHDR(&room);
    if (r && r->cmsg_type == SCM_RIGHTS) {
        int first = -1;
        memcpy(&first, CMSG_DATA(r), sizeof(int));
        int next = dup(0);
        printf("len ok %d controllen ok %d second released %d\n",
               r->cmsg_len == CMSG_LEN(sizeof(int)),
               room.msg_controllen == CMSG_LEN(sizeof(int)), next == first + 1);
    } else {
        printf("no descriptor\n");
    }
    return 0;
}
