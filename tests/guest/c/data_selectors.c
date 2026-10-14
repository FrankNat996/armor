/*
 * The data segment selectors a 64-bit Linux process sees: DS, ES, FS and GS
 * are all 0 (the null selector; FS and GS bases come from arch_prctl).
 * Crash reporters save them with the context (Chromium's did). qemu-x86_64
 * shows 0x2b in DS and ES, so this test runs against the hardware oracle.
 */
#include <stdio.h>

int main(void) {
    unsigned short ds, es, fs, gs;
    struct {
        unsigned short selector, guard;
    } in_memory = {0xffff, 0xabcd};
    unsigned long wide = ~0ul;
    __asm__("mov %%ds, %0" : "=r"(ds));
    __asm__("mov %%es, %0" : "=r"(es));
    __asm__("mov %%fs, %0" : "=r"(fs));
    __asm__("mov %%gs, %0" : "=r"(gs));
    __asm__("mov %%gs, %0" : "=m"(in_memory.selector) : : "memory");
    __asm__("mov %%ds, %0" : "+r"(wide));
    printf("ds %#x es %#x fs %#x gs %#x gs in memory %#x guard %#x ds in a 64-bit register %#lx\n",
           ds, es, fs, gs, in_memory.selector, in_memory.guard, wide);
    return 0;
}
