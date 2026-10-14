/*
 * Benchmark (cargo xtask bench): the cost of a syscall round trip.
 * getppid (no work in the kernel) and one-byte writes to /dev/null.
 */
#include <fcntl.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    long sum = 0;
    for (int i = 0; i < 300000; i++)
        sum += syscall(SYS_getppid) > 0;
    int fd = open("/dev/null", O_WRONLY);
    for (int i = 0; i < 300000; i++)
        sum += write(fd, "x", 1);
    close(fd);
    printf("%ld\n", sum);
    return 0;
}
