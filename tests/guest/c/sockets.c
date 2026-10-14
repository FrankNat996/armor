/*
 * Sockets on the local machine only: a socket pair passing data and a
 * descriptor (sendmsg/recvmsg with SCM_RIGHTS), TCP on the loopback
 * (bind, listen, getsockname, connect, accept, send, recv, shutdown,
 * getpeername), UDP (sendto, recvfrom with the sender's address), socket
 * options (SO_RCVTIMEO, a timeout that expires, SO_REUSEADDR, SO_TYPE)
 * and poll. The same source runs as x86-64 and i386, whose msghdr,
 * cmsghdr and timeval differ (ADR-0029). Prints no addresses or ports.
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/uio.h>
#include <unistd.h>

static int pass_descriptor(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        perror("socketpair");
        return 1;
    }
    int pipefd[2];
    if (pipe(pipefd) != 0) {
        return 1;
    }
    /* Two iovecs and one descriptor. */
    char a[] = "hello ", b[] = "descriptor";
    struct iovec out[2] = {{a, 6}, {b, 10}};
    union {
        struct cmsghdr header;
        char bytes[CMSG_SPACE(sizeof(int))];
    } control;
    memset(&control, 0, sizeof control);
    struct msghdr message = {.msg_iov = out, .msg_iovlen = 2, .msg_control = control.bytes,
                             .msg_controllen = sizeof control.bytes};
    struct cmsghdr *c = CMSG_FIRSTHDR(&message);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &pipefd[1], sizeof(int));
    printf("sendmsg: %zd\n", sendmsg(sv[0], &message, 0));

    char x[8], y[16];
    struct iovec in[2] = {{x, 8}, {y, 16}};
    union {
        struct cmsghdr header;
        char bytes[CMSG_SPACE(sizeof(int))];
    } received;
    struct msghdr back = {.msg_iov = in, .msg_iovlen = 2, .msg_control = received.bytes,
                          .msg_controllen = sizeof received.bytes};
    ssize_t n = recvmsg(sv[1], &back, 0);
    printf("recvmsg: %zd '%.8s' '%.8s' flags %d\n", n, x, y, back.msg_flags);
    struct cmsghdr *r = CMSG_FIRSTHDR(&back);
    int got = -1;
    if (r && r->cmsg_type == SCM_RIGHTS) {
        memcpy(&got, CMSG_DATA(r), sizeof(int));
        printf("control: level %d type %d len ok %d\n", r->cmsg_level, r->cmsg_type,
               r->cmsg_len == CMSG_LEN(sizeof(int)));
    }
    /* The passed descriptor writes into the same pipe. */
    if (got >= 0 && write(got, "through", 7) == 7) {
        char buf[8] = {0};
        printf("pipe: %zd '%s'\n", read(pipefd[0], buf, 7), buf);
    }
    /* Not enough room for the descriptor: MSG_CTRUNC. */
    sendmsg(sv[0], &message, 0);
    char small[4];
    struct msghdr cut = {.msg_iov = in, .msg_iovlen = 2, .msg_control = small, .msg_controllen = 2};
    n = recvmsg(sv[1], &cut, 0);
    printf("truncated control: %zd ctrunc %d\n", n, (cut.msg_flags & MSG_CTRUNC) != 0);

    return 0;
}

static int tcp(void) {
    int server = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
    int one = 1;
    printf("SO_REUSEADDR: %d\n", setsockopt(server, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one));
    struct sockaddr_in address = {.sin_family = AF_INET, .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    printf("bind: %d\n", bind(server, (struct sockaddr *)&address, sizeof address));
    printf("listen: %d\n", listen(server, 4));
    socklen_t len = sizeof address;
    int r = getsockname(server, (struct sockaddr *)&address, &len);
    /* Each call comes before the printf that shows its results: the
     * order in which arguments are evaluated is unspecified. */
    printf("getsockname: %d len %u port set %d\n", r, len, address.sin_port != 0);
    int client = socket(AF_INET, SOCK_STREAM, 0);
    printf("connect: %d\n", connect(client, (struct sockaddr *)&address, sizeof address));
    struct sockaddr_in peer;
    len = sizeof peer;
    int accepted = accept(server, (struct sockaddr *)&peer, &len);
    printf("accept: %d len %u loopback %d\n", accepted >= 0, len,
           peer.sin_addr.s_addr == htonl(INADDR_LOOPBACK));
    printf("send: %zd\n", send(client, "ping over tcp", 13, 0));
    char buf[32] = {0};
    printf("recv: %zd '%s'\n", recv(accepted, buf, sizeof buf, 0), buf);
    struct pollfd p = {.fd = accepted, .events = POLLIN};
    printf("poll before: %d\n", poll(&p, 1, 0));
    printf("shutdown: %d\n", shutdown(client, SHUT_WR));
    r = poll(&p, 1, 1000);
    printf("poll after: %d revents %#x\n", r, p.revents & (POLLIN | POLLHUP));
    printf("recv at eof: %zd\n", recv(accepted, buf, sizeof buf, 0));
    len = sizeof peer;
    r = getpeername(client, (struct sockaddr *)&peer, &len);
    printf("getpeername: %d family %d\n", r, peer.sin_family);
    int type = 0;
    len = sizeof type;
    r = getsockopt(client, SOL_SOCKET, SO_TYPE, &type, &len);
    printf("SO_TYPE: %d %d len %u\n", r, type, len);
    close(client);
    close(accepted);
    close(server);
    return 0;
}

static int udp(void) {
    int a = socket(AF_INET, SOCK_DGRAM, 0), b = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in address = {.sin_family = AF_INET, .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    bind(b, (struct sockaddr *)&address, sizeof address);
    socklen_t len = sizeof address;
    getsockname(b, (struct sockaddr *)&address, &len);
    printf("sendto: %zd\n", sendto(a, "datagram", 8, 0, (struct sockaddr *)&address, sizeof address));
    char buf[16] = {0};
    struct sockaddr_in from;
    len = sizeof from;
    ssize_t got = recvfrom(b, buf, sizeof buf, 0, (struct sockaddr *)&from, &len);
    printf("recvfrom: %zd '%s' family %d len %u\n", got, buf, from.sin_family, len);
    /* A receive timeout of 0.2 s that expires. */
    struct timeval timeout = {.tv_sec = 0, .tv_usec = 200000};
    printf("SO_RCVTIMEO: %d\n", setsockopt(b, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout));
    /* Not read back with getsockopt: qemu-user does not implement it. */
    errno = 0;
    ssize_t n = recv(b, buf, sizeof buf, 0);
    printf("timed out: %zd %s\n", n, errno == EAGAIN || errno == EWOULDBLOCK ? "EAGAIN" : strerror(errno));
    close(a);
    close(b);
    return 0;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    return pass_descriptor() | tcp() | udp();
}
