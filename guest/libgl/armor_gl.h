/* armor's x86-64 libGL.so.1 (ADR-0041): what the generated OpenGL stubs
   (gl_stubs.c) and the GLX functions (glx.c) share. */
#ifndef ARMOR_GL_H
#define ARMOR_GL_H

#include <stdint.h>

#define ARMOR_EXPORT __attribute__((visibility("default")))

/* armor's host-call syscall: function id in rdi, sixteen 8-byte slots in
   rsi; 0 when armor made the call, -ENOSYS anywhere else. */
#define ARMOR_HOST_CALL 0x410000L

static inline long armor_host_call(uint64_t id, uint64_t *slots) {
    long result;
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(ARMOR_HOST_CALL), "D"(id), "S"(slots)
                     : "rcx", "r11", "memory");
    return result;
}

static inline void armor_gl_call(uint64_t id, uint64_t *slots) {
    armor_host_call(id, slots);
}

/* Whether armor runs this program with the host's OpenGL (asked once). */
int armor_gl_on_host(void);

/* The guest's own libGL function `name` (libGL.so.1.armor-guest), for when
   armor does not provide the host's OpenGL. */
void *armor_gl_guest(const char *name);

#endif
