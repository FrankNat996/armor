/*
 * The FPU state of an x86-64 signal frame with XSAVE declared (run with
 * `+xsave`, difftest's CASE_FEATURES; docs/tech/signals.md): uc_flags,
 * the frame's place below the interrupted stack, the software-reserved
 * bytes (_fpx_sw_bytes: magic1, extended_size, xfeatures, xstate_size),
 * MAGIC2 after the XSAVE area, XSTATE_BV, the saved xmm and MXCSR; then
 * what rt_sigreturn restores when the handler changes the image, clears
 * the SSE bit of XSTATE_BV, or damages the magic numbers. Left out because
 * qemu differs from Linux (docs/tech/signals.md): uc_flags' other bits,
 * the gap between siginfo and the FPU state, and an xfeatures without SSE.
 */
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/ucontext.h>
#include <unistd.h>

#define MAGIC1 0x46505853u
#define MAGIC2 0x46505845u

/* What the handler does to the frame before returning. */
enum change { NONE, WRITE_XMM, CLEAR_SSE_BV, NO_MAGIC1, NO_MAGIC2 };
static volatile enum change change;

static uint32_t u32(const unsigned char *p) {
    uint32_t v;
    memcpy(&v, p, 4);
    return v;
}

static uint64_t u64(const unsigned char *p) {
    uint64_t v;
    memcpy(&v, p, 8);
    return v;
}

static void handler(int sig, siginfo_t *si, void *ucv) {
    ucontext_t *uc = ucv;
    unsigned char *fp = (unsigned char *)uc->uc_mcontext.fpregs;
    uintptr_t rsp = uc->uc_mcontext.gregs[REG_RSP];
    uint32_t magic1 = u32(fp + 464), extended = u32(fp + 468);
    uint64_t xfeatures = u64(fp + 472);
    uint32_t xstate_size = u32(fp + 480);
    if (change == NONE) {
        printf("UC_FP_XSTATE %lu\n", uc->uc_flags & 1);
        printf("fpstate aligned to 64: %d\n", (int)((uintptr_t)fp % 64 == 0));
        printf("siginfo below fpstate: %d\n",
               (int)((uintptr_t)si + sizeof *si <= (uintptr_t)fp));
        printf("magic1 %#x extended_size %u xfeatures %#llx xstate_size %u\n", magic1, extended,
               (unsigned long long)xfeatures, xstate_size);
        if (magic1 == MAGIC1) {
            uintptr_t end = (uintptr_t)fp + extended;
            printf("magic2 %#x\n", u32(fp + xstate_size));
            printf("XSTATE_BV %#llx, header bytes 8..64 zero: %d\n",
                   (unsigned long long)u64(fp + 512),
                   (int)(u64(fp + 520) == 0 && u64(fp + 528) == 0 && u64(fp + 536) == 0));
            printf("red zone and area fit below rsp, gap under 64: %d\n",
                   (int)(end <= rsp - 128 && rsp - 128 - end < 64));
        }
        printf("saved mxcsr %#x xmm7 %#llx %#llx\n", u32(fp + 24),
               (unsigned long long)u64(fp + 160 + 7 * 16),
               (unsigned long long)u64(fp + 168 + 7 * 16));
    }
    switch (change) {
    case NONE:
        break;
    case WRITE_XMM:
        memset(fp + 160 + 7 * 16, 0x5a, 16);
        memcpy(fp + 24, &(uint32_t){0x1fa0}, 4);
        break;
    case CLEAR_SSE_BV:
        fp[512] &= ~2;
        break;
    case NO_MAGIC1:
        memset(fp + 464, 0, 4);
        memset(fp + 160 + 7 * 16, 0x77, 16);
        break;
    case NO_MAGIC2:
        if (magic1 == MAGIC1)
            memset(fp + xstate_size, 0, 4);
        memset(fp + 160 + 7 * 16, 0x66, 16);
        break;
    }
    (void)sig;
}

/*
 * Sends SIGUSR1 to this thread with xmm7 = 0x11..11 (and MXCSR 0x1f80):
 * the signal arrives as the syscall returns, so the frame holds that
 * state. Prints xmm7 and MXCSR after the handler's return.
 */
static void send(enum change what) {
    uint64_t in[2] = {0x1111111111111111ull, 0x2222222222222222ull}, out[2];
    uint32_t mxcsr = 0x1f80, after_mxcsr;
    long ret;
    change = what;
    __asm__ volatile("ldmxcsr %[m]\n\t"
                     "movdqu %[in], %%xmm7\n\t"
                     "syscall\n\t"
                     "movdqu %%xmm7, %[out]\n\t"
                     "stmxcsr %[after]"
                     : "=a"(ret), [out] "=m"(out), [after] "=m"(after_mxcsr)
                     : "a"(SYS_tgkill), "D"(getpid()), "S"(gettid()), "d"(SIGUSR1),
                       [in] "m"(in), [m] "m"(mxcsr)
                     : "rcx", "r11", "memory", "xmm7");
    printf("change %d: tgkill %ld, xmm7 %#llx %#llx, mxcsr %#x\n", what, ret,
           (unsigned long long)out[0], (unsigned long long)out[1], after_mxcsr);
}

int main(void) {
    struct sigaction sa = {.sa_sigaction = handler, .sa_flags = SA_SIGINFO};
    sigaction(SIGUSR1, &sa, NULL);
    send(NONE);
    send(WRITE_XMM);
    send(CLEAR_SSE_BV);
    send(NO_MAGIC1);
    send(NO_MAGIC2);
    return 0;
}
