/*
 * Instructions a user-mode program may not run, or runs rarely: what each
 * one raises (the signal number), and the CS and SS selectors a 64-bit
 * Linux process sees. Port I/O, cli/sti, int n and hlt raise #GP
 * (SIGSEGV); int1 and int3 raise SIGTRAP.
 */
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>

static sigjmp_buf back;
static volatile sig_atomic_t caught;

static volatile sig_atomic_t traps;

static void count_trap(int sig) {
    (void)sig;
    traps++;
}

static void handler(int sig) {
    caught = sig;
    siglongjmp(back, 1);
}

/* Runs one instruction sequence (AT&T syntax) and prints the signal it
 * raised, 0 for none. */
#define TRY(name, code)                                                        \
    do {                                                                       \
        caught = 0;                                                            \
        if (sigsetjmp(back, 1) == 0)                                           \
            __asm__ volatile(code ::: "memory", "rax", "rdx", "rsi", "rdi");   \
        printf("%-10s signal %d\n", name, (int)caught);                        \
    } while (0)

char io_buf[16];

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = handler;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGTRAP, &sa, NULL);
    sigaction(SIGILL, &sa, NULL);

    TRY("in", "xor %%edx, %%edx; inb %%dx, %%al");
    TRY("in imm", "inb $0x60, %%al");
    TRY("out", "xor %%edx, %%edx; outb %%al, %%dx");
    TRY("out imm", "outl %%eax, $0x80");
    TRY("insb", "lea io_buf(%%rip), %%rdi; xor %%edx, %%edx; insb");
    TRY("outsb", "lea io_buf(%%rip), %%rsi; xor %%edx, %%edx; outsb");
    TRY("cli", "cli");
    TRY("sti", "sti");
    TRY("hlt", "hlt");
    TRY("int 0x10", "int $0x10");
    TRY("int 3 (CD)", ".byte 0xcd, 0x03");
    TRY("int3 (CC)", "int3");
    TRY("int1", ".byte 0xf1");

    /* DS and ES are left out: qemu reports 0x2b, which may not be what
     * Linux sets (docs/tech/x86-64-encoding.md). */
    unsigned short cs, ss;
    unsigned long cs64;
    /* A selector stored to memory is 16 bits: the guard stays intact. */
    static struct {
        unsigned short selector, guard;
    } in_memory = {0xffff, 0xabcd};
    __asm__("mov %%cs, %0" : "=r"(cs));
    __asm__("mov %%ss, %0" : "=r"(ss));
    __asm__("movq $-1, %0; mov %%cs, %0" : "=&r"(cs64));
    __asm__("mov %%ss, %0" : "=m"(in_memory.selector) : : "memory");
    printf("cs %#x ss %#x cs in a 64-bit register %#lx ss in memory %#x guard %#x\n", cs, ss,
           cs64, in_memory.selector, in_memory.guard);

    /* Breakpoints are traps: a handler that returns resumes after the
     * instruction, whose length depends on the encoding. */
    sa.sa_handler = count_trap;
    sigaction(SIGTRAP, &sa, NULL);
    long after = 0;
    __asm__ volatile("int3; addq $1, %0; .byte 0xcd, 0x03; addq $10, %0; .byte 0xf1; addq $100, %0"
                     : "+r"(after));
    printf("traps %d, code after them %ld\n", (int)traps, after);
    return 0;
}
