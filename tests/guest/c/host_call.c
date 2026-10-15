/*
 * A call into the host's own libraries (ADR-0041): the arguments in
 * sixteen 8-byte slots, syscall 0x410000 with the function's id. Under
 * armor the host's libm computes ldexp; anywhere else the syscall fails
 * with ENOSYS and the program computes it itself, so the output is the
 * same everywhere. ARMOR_TEST_SHOW_PATH=1 also says which path was taken.
 */
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#define ARMOR_HOST_CALL 0x410000L
#define HOST_LDEXP 1L

/* x * 2^e by repeated exact doubling or halving (the fallback). */
static double local_ldexp(double x, int e) {
    for (; e > 0; e--) {
        x *= 2.0;
    }
    for (; e < 0; e++) {
        x /= 2.0;
    }
    return x;
}

static int on_host;

static double call_ldexp(double x, int e) {
    uint64_t slots[16] = {0};
    memcpy(&slots[0], &x, sizeof x);
    slots[1] = (uint64_t)(int64_t)e;
    if (syscall(ARMOR_HOST_CALL, HOST_LDEXP, slots) == 0) {
        double result;
        memcpy(&result, &slots[0], sizeof result);
        on_host = 1;
        return result;
    }
    return local_ldexp(x, e);
}

int main(void) {
    static const double values[] = {0.75, -3.5, 1e-300, 123456.789};
    static const int exponents[] = {-3, 10, 600, -40};
    for (int i = 0; i < 4; i++) {
        printf("ldexp(%a, %d) = %a\n", values[i], exponents[i],
               call_ldexp(values[i], exponents[i]));
    }
    /* An id armor does not know: ENOSYS, as from any other kernel. */
    uint64_t slots[16] = {0};
    long r = syscall(ARMOR_HOST_CALL, 9999L, slots);
    printf("unknown id: %ld %s\n", r, r == -1 && errno == ENOSYS ? "ENOSYS" : "other");
    const char *show = getenv("ARMOR_TEST_SHOW_PATH");
    if (show && *show == '1') {
        printf("path: %s\n", on_host ? "host" : "guest");
    }
    return 0;
}
