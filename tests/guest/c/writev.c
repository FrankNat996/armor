/*
 * writev: segments in order (an empty one among them) to stdout and to a
 * pipe, the errors of invalid calls, and glibc's fatal error path: a child
 * frees a block twice, glibc writes its message with writev and aborts.
 * Before armor had writev that child ended with armor's exit code 125
 * instead of SIGABRT.
 */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

static void result(const char *what, ssize_t r) {
  if (r < 0)
    printf("%-24s -1 %s\n", what, strerror(errno));
  else
    printf("%-24s %zd\n", what, r);
}

int main(void) {
  char a[] = "segments ", b[] = "in ", c[] = "order\n";
  struct iovec v[4] = {{a, strlen(a)}, {NULL, 0}, {b, strlen(b)}, {c, strlen(c)}};
  fflush(stdout);
  result("stdout", writev(1, v, 4));
  result("no segments", writev(1, v, 0));
  result("1025 segments", writev(1, v, 1025));
  result("bad vector", writev(1, (struct iovec *)8, 1));
  result("bad descriptor", writev(99, v, 4));
  struct iovec negative[2] = {{a, 1}, {b, (size_t)-1}};
  result("negative length", writev(1, negative, 2));

  int p[2];
  if (pipe(p) != 0)
    return 1;
  result("pipe", writev(p[1], v, 4));
  char buf[64];
  ssize_t n = read(p[0], buf, sizeof buf);
  printf("read back %zd: %.*s", n, (int)n, buf);

  /* glibc's fatal message: "free(): double free detected in tcache 2". */
  fflush(stdout);
  pid_t child = fork();
  if (child == 0) {
    char *block = malloc(32);
    char *volatile again = block;
    free(block);
    free(again);
    return 0;
  }
  int status;
  waitpid(child, &status, 0);
  if (WIFSIGNALED(status))
    printf("double free: signal %d\n", WTERMSIG(status));
  else
    printf("double free: exit %d\n", WEXITSTATUS(status));
  return 0;
}
