/*
 * Scheduling policy and parameters (sched_getscheduler, sched_setscheduler,
 * sched_getparam, sched_setparam, the priority ranges, sched_rr_get_interval)
 * and clock_getres, as a Steam game's Python runtime asked for them. Only
 * changes an unprivileged process may make (SCHED_BATCH and back), so the
 * output is the same as root on the hardware oracle. The same source runs as
 * x86-64 and i386.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdio.h>
#include <time.h>

static const char *name(int e) {
    switch (e) {
    case 0: return "ok";
    case EINVAL: return "EINVAL";
    case ESRCH: return "ESRCH";
    case EPERM: return "EPERM";
    case EFAULT: return "EFAULT";
    default: return "other";
    }
}

static int show(const char *what, int result) {
    printf("%s: %d %s\n", what, result, name(result < 0 ? errno : 0));
    return result;
}

int main(void) {
    show("getscheduler", sched_getscheduler(0));
    struct sched_param p = {.sched_priority = -1};
    show("getparam", sched_getparam(0, &p));
    printf("  priority %d\n", p.sched_priority);

    show("fifo max", sched_get_priority_max(SCHED_FIFO));
    show("fifo min", sched_get_priority_min(SCHED_FIFO));
    show("rr max", sched_get_priority_max(SCHED_RR));
    show("other max", sched_get_priority_max(SCHED_OTHER));
    show("batch min", sched_get_priority_min(SCHED_BATCH));
    show("bad policy max", sched_get_priority_max(42));

    struct sched_param zero = {.sched_priority = 0};
    show("set batch", sched_setscheduler(0, SCHED_BATCH, &zero));
    show("getscheduler", sched_getscheduler(0));
    show("set other", sched_setscheduler(0, SCHED_OTHER, &zero));
    show("getscheduler", sched_getscheduler(0));
    struct sched_param one = {.sched_priority = 1};
    show("setparam priority 1 on other", sched_setparam(0, &one));
    show("setparam 0", sched_setparam(0, &zero));
    show("setscheduler bad policy", sched_setscheduler(0, 42, &zero));
    show("getparam bad pid", sched_getparam(-1, &p));
    show("getscheduler no such pid", sched_getscheduler(0x3fffffff));
    show("getparam bad pointer", sched_getparam(0, (struct sched_param *)8));

    struct timespec ts = {.tv_sec = -1, .tv_nsec = -1};
    show("rr_get_interval", sched_rr_get_interval(0, &ts));
    printf("  seconds %lld, nanoseconds in range %d\n", (long long)ts.tv_sec,
           ts.tv_nsec >= 0 && ts.tv_nsec < 1000000000);
    show("rr_get_interval no such pid", sched_rr_get_interval(0x3fffffff, &ts));

    struct timespec res = {.tv_sec = -1, .tv_nsec = -1};
    show("clock_getres realtime", clock_getres(CLOCK_REALTIME, &res));
    printf("  %lld s %ld ns\n", (long long)res.tv_sec, res.tv_nsec);
    show("clock_getres monotonic", clock_getres(CLOCK_MONOTONIC, &res));
    printf("  %lld s %ld ns\n", (long long)res.tv_sec, res.tv_nsec);
    show("clock_getres no result", clock_getres(CLOCK_MONOTONIC, NULL));
    show("clock_getres bad clock", clock_getres(1234, &res));
    return 0;
}
