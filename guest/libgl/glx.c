/* armor's x86-64 libGL.so.1 (ADR-0041): GLX, by hand.
 *
 * With the host's OpenGL (armor --host-gl), each function passes its
 * arguments to armor, which calls GLX on its own connection to the X server
 * with the X ids the program's own libX11 created; the program's Display*
 * never leaves it. A visual crosses as its id, and comes back as the
 * program's own XVisualInfo (from its libX11's XGetVisualInfo, so that its
 * XFree frees it); configs and contexts are opaque values; an array of
 * configs is allocated here with malloc, so that XFree frees it.
 *
 * Without it (not under armor, or armor without the host's OpenGL), every
 * function is the guest's own libGL's (libGL.so.1.armor-guest).
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "armor_gl.h"

typedef int Bool;
typedef unsigned long XID;
typedef void Display;
typedef void *GLXContext;
typedef void *GLXFBConfig;

typedef struct {
    void *visual;
    unsigned long visualid;
    int screen;
    int depth;
    int c_class;
    unsigned long red_mask, green_mask, blue_mask;
    int colormap_size;
    int bits_per_rgb;
} XVisualInfo;

/* FNV-1a, 64 bits: a function's id from its name (as armor computes it). */
static uint64_t id_of(const char *name) {
    uint64_t hash = 0xcbf29ce484222325ULL;
    for (; *name; name++) {
        hash ^= (unsigned char)*name;
        hash *= 0x100000001b3ULL;
    }
    return hash;
}

int armor_gl_on_host(void) {
    static int on_host = -1;
    if (on_host < 0) {
        uint64_t s[16] = {0};
        on_host = armor_host_call(id_of("armorHostGL"), s) == 0;
    }
    return on_host;
}

void *armor_gl_guest(const char *name) {
    static void *guest;
    if (!guest) {
        guest = dlopen("libGL.so.1.armor-guest", RTLD_NOW | RTLD_GLOBAL);
    }
    return guest ? dlsym(guest, name) : NULL;
}

/* ARMOR_GL_TRACE=1: each GLX call on the host, with its first arguments
   and its result, on stderr. */
static int tracing(void) {
    static int trace = -1;
    if (trace < 0) {
        const char *value = getenv("ARMOR_GL_TRACE");
        trace = value && *value == '1';
    }
    return trace;
}

/* Calls GLX function `name` on the host with the slots `s`; slot 0 holds
   the result. */
static uint64_t host(const char *name, uint64_t *s) {
    uint64_t first = s[0], second = s[1];
    long status = armor_host_call(id_of(name), s);
    if (tracing()) {
        fprintf(stderr, "armor-gl: %s(%#llx, %#llx) = %#llx (%ld)\n", name,
                (unsigned long long)first, (unsigned long long)second,
                (unsigned long long)s[0], status);
    }
    return s[0];
}

/* Function `name` of the program's own libX11: in the global scope, or in
   the libX11 it loaded with RTLD_LOCAL (SDL does). */
static void *x11_symbol(const char *name) {
    void *symbol = dlsym(RTLD_DEFAULT, name);
    if (symbol) {
        return symbol;
    }
    static void *x11;
    if (!x11) {
        x11 = dlopen("libX11.so.6", RTLD_NOW | RTLD_NOLOAD);
    }
    if (!x11) {
        x11 = dlopen("libX11.so.6", RTLD_NOW);
    }
    return x11 ? dlsym(x11, name) : NULL;
}

/* The program's own XVisualInfo of visual `id` (NULL if none). */
static XVisualInfo *guest_visual(Display *dpy, unsigned long id, int screen) {
    static XVisualInfo *(*get)(Display *, long, XVisualInfo *, int *);
    if (!get) {
        get = (XVisualInfo * (*)(Display *, long, XVisualInfo *, int *))
            x11_symbol("XGetVisualInfo");
    }
    if (!get || !id) {
        return NULL;
    }
    XVisualInfo template;
    memset(&template, 0, sizeof template);
    template.visualid = id;
    template.screen = screen;
    int count = 0;
    /* VisualIDMask | VisualScreenMask */
    return get(dpy, 0x3, &template, &count);
}

/* The program's requests reach the server before armor's connection uses
   their ids (a window just created, for one). */
static void guest_sync(Display *dpy) {
    static int (*sync)(Display *, Bool);
    if (!sync) {
        sync = (int (*)(Display *, Bool))x11_symbol("XSync");
    }
    if (sync && dpy) {
        sync(dpy, 0);
    }
}

static __thread Display *current_display;

#define GUEST(type, name, params, ...)                                        \
    do {                                                                      \
        static type(*guest) params;                                           \
        if (!guest) guest = (type(*) params)armor_gl_guest(name);             \
        return guest ? guest(__VA_ARGS__) : (type)0;                          \
    } while (0)

#define GUEST_VOID(name, params, ...)                                         \
    do {                                                                      \
        static void(*guest) params;                                           \
        if (!guest) guest = (void(*) params)armor_gl_guest(name);             \
        if (guest) guest(__VA_ARGS__);                                        \
        return;                                                               \
    } while (0)

#define P(v) ((uint64_t)(uintptr_t)(v))
#define I(v) ((uint64_t)(int64_t)(v))

ARMOR_EXPORT Bool glXQueryExtension(Display *dpy, int *error_base, int *event_base) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {P(error_base), P(event_base)};
        return (Bool)host("glXQueryExtension", s);
    }
    GUEST(Bool, "glXQueryExtension", (Display *, int *, int *), dpy, error_base, event_base);
}

ARMOR_EXPORT Bool glXQueryVersion(Display *dpy, int *major, int *minor) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {P(major), P(minor)};
        return (Bool)host("glXQueryVersion", s);
    }
    GUEST(Bool, "glXQueryVersion", (Display *, int *, int *), dpy, major, minor);
}

ARMOR_EXPORT const char *glXQueryExtensionsString(Display *dpy, int screen) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {I(screen)};
        return (const char *)(uintptr_t)host("glXQueryExtensionsString", s);
    }
    GUEST(const char *, "glXQueryExtensionsString", (Display *, int), dpy, screen);
}

ARMOR_EXPORT const char *glXGetClientString(Display *dpy, int name) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {I(name)};
        return (const char *)(uintptr_t)host("glXGetClientString", s);
    }
    GUEST(const char *, "glXGetClientString", (Display *, int), dpy, name);
}

ARMOR_EXPORT const char *glXQueryServerString(Display *dpy, int screen, int name) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {I(screen), I(name)};
        return (const char *)(uintptr_t)host("glXQueryServerString", s);
    }
    GUEST(const char *, "glXQueryServerString", (Display *, int, int), dpy, screen, name);
}

ARMOR_EXPORT XVisualInfo *glXChooseVisual(Display *dpy, int screen, int *attributes) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {I(screen), P(attributes)};
        return guest_visual(dpy, (unsigned long)host("glXChooseVisual", s), screen);
    }
    GUEST(XVisualInfo *, "glXChooseVisual", (Display *, int, int *), dpy, screen, attributes);
}

ARMOR_EXPORT int glXGetConfig(Display *dpy, XVisualInfo *visual, int attribute, int *value) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {visual ? visual->visualid : 0, I(visual ? visual->screen : 0),
                          I(attribute), P(value)};
        return (int)host("glXGetConfig", s);
    }
    GUEST(int, "glXGetConfig", (Display *, XVisualInfo *, int, int *), dpy, visual, attribute, value);
}

ARMOR_EXPORT GLXContext glXCreateContext(Display *dpy, XVisualInfo *visual, GLXContext share,
                                         Bool direct) {
    if (armor_gl_on_host()) {
        guest_sync(dpy);
        uint64_t s[16] = {visual ? visual->visualid : 0, I(visual ? visual->screen : 0),
                          P(share), I(direct)};
        return (GLXContext)(uintptr_t)host("glXCreateContext", s);
    }
    GUEST(GLXContext, "glXCreateContext", (Display *, XVisualInfo *, GLXContext, Bool), dpy,
          visual, share, direct);
}

ARMOR_EXPORT void glXDestroyContext(Display *dpy, GLXContext context) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {P(context)};
        host("glXDestroyContext", s);
        return;
    }
    GUEST_VOID("glXDestroyContext", (Display *, GLXContext), dpy, context);
}

ARMOR_EXPORT Bool glXMakeCurrent(Display *dpy, XID drawable, GLXContext context) {
    if (armor_gl_on_host()) {
        guest_sync(dpy);
        uint64_t s[16] = {drawable, P(context)};
        Bool ok = (Bool)host("glXMakeCurrent", s);
        if (ok) {
            current_display = context ? dpy : NULL;
        }
        return ok;
    }
    GUEST(Bool, "glXMakeCurrent", (Display *, XID, GLXContext), dpy, drawable, context);
}

ARMOR_EXPORT Bool glXMakeContextCurrent(Display *dpy, XID draw, XID read, GLXContext context) {
    if (armor_gl_on_host()) {
        guest_sync(dpy);
        uint64_t s[16] = {draw, read, P(context)};
        Bool ok = (Bool)host("glXMakeContextCurrent", s);
        if (ok) {
            current_display = context ? dpy : NULL;
        }
        return ok;
    }
    GUEST(Bool, "glXMakeContextCurrent", (Display *, XID, XID, GLXContext), dpy, draw, read,
          context);
}

ARMOR_EXPORT void glXSwapBuffers(Display *dpy, XID drawable) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {drawable};
        host("glXSwapBuffers", s);
        return;
    }
    GUEST_VOID("glXSwapBuffers", (Display *, XID), dpy, drawable);
}

ARMOR_EXPORT Bool glXIsDirect(Display *dpy, GLXContext context) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {P(context)};
        return (Bool)host("glXIsDirect", s);
    }
    GUEST(Bool, "glXIsDirect", (Display *, GLXContext), dpy, context);
}

ARMOR_EXPORT void glXQueryDrawable(Display *dpy, XID drawable, int attribute,
                                   unsigned int *value) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {drawable, I(attribute), P(value)};
        host("glXQueryDrawable", s);
        return;
    }
    GUEST_VOID("glXQueryDrawable", (Display *, XID, int, unsigned int *), dpy, drawable,
               attribute, value);
}

/* An array of configs from the host, in memory allocated here. */
static GLXFBConfig *host_configs(const char *name, int screen, const int *attributes,
                                 int *count) {
    enum { CAPACITY = 1024 };
    GLXFBConfig *configs = malloc(CAPACITY * sizeof *configs);
    int n = 0;
    if (!configs) {
        if (count) *count = 0;
        return NULL;
    }
    uint64_t s[16] = {I(screen), P(attributes), P(&n), P(configs), CAPACITY};
    host(name, s);
    if (count) {
        *count = n;
    }
    if (n == 0) {
        free(configs);
        return NULL;
    }
    return configs;
}

ARMOR_EXPORT GLXFBConfig *glXChooseFBConfig(Display *dpy, int screen, const int *attributes,
                                            int *count) {
    if (armor_gl_on_host()) {
        return host_configs("glXChooseFBConfig", screen, attributes, count);
    }
    GUEST(GLXFBConfig *, "glXChooseFBConfig", (Display *, int, const int *, int *), dpy, screen,
          attributes, count);
}

ARMOR_EXPORT GLXFBConfig *glXGetFBConfigs(Display *dpy, int screen, int *count) {
    if (armor_gl_on_host()) {
        return host_configs("glXGetFBConfigs", screen, NULL, count);
    }
    GUEST(GLXFBConfig *, "glXGetFBConfigs", (Display *, int, int *), dpy, screen, count);
}

ARMOR_EXPORT int glXGetFBConfigAttrib(Display *dpy, GLXFBConfig config, int attribute,
                                      int *value) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {P(config), I(attribute), P(value)};
        return (int)host("glXGetFBConfigAttrib", s);
    }
    GUEST(int, "glXGetFBConfigAttrib", (Display *, GLXFBConfig, int, int *), dpy, config,
          attribute, value);
}

ARMOR_EXPORT XVisualInfo *glXGetVisualFromFBConfig(Display *dpy, GLXFBConfig config) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {P(config)};
        unsigned long id = (unsigned long)host("glXGetVisualFromFBConfig", s);
        /* The visual's screen: the default one (configs of other screens
           are rare; XGetVisualInfo then finds none). */
        return guest_visual(dpy, id, 0);
    }
    GUEST(XVisualInfo *, "glXGetVisualFromFBConfig", (Display *, GLXFBConfig), dpy, config);
}

ARMOR_EXPORT GLXContext glXCreateNewContext(Display *dpy, GLXFBConfig config, int render_type,
                                            GLXContext share, Bool direct) {
    if (armor_gl_on_host()) {
        guest_sync(dpy);
        uint64_t s[16] = {P(config), I(render_type), P(share), I(direct)};
        return (GLXContext)(uintptr_t)host("glXCreateNewContext", s);
    }
    GUEST(GLXContext, "glXCreateNewContext", (Display *, GLXFBConfig, int, GLXContext, Bool), dpy,
          config, render_type, share, direct);
}

ARMOR_EXPORT GLXContext glXCreateContextAttribsARB(Display *dpy, GLXFBConfig config,
                                                   GLXContext share, Bool direct,
                                                   const int *attributes) {
    if (armor_gl_on_host()) {
        guest_sync(dpy);
        uint64_t s[16] = {P(config), P(share), I(direct), P(attributes)};
        return (GLXContext)(uintptr_t)host("glXCreateContextAttribsARB", s);
    }
    GUEST(GLXContext, "glXCreateContextAttribsARB",
          (Display *, GLXFBConfig, GLXContext, Bool, const int *), dpy, config, share, direct,
          attributes);
}

ARMOR_EXPORT void glXSwapIntervalEXT(Display *dpy, XID drawable, int interval) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {drawable, I(interval)};
        host("glXSwapIntervalEXT", s);
        return;
    }
    GUEST_VOID("glXSwapIntervalEXT", (Display *, XID, int), dpy, drawable, interval);
}

ARMOR_EXPORT int glXSwapIntervalMESA(unsigned int interval) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {interval};
        return (int)host("glXSwapIntervalMESA", s);
    }
    GUEST(int, "glXSwapIntervalMESA", (unsigned int), interval);
}

ARMOR_EXPORT int glXGetSwapIntervalMESA(void) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {0};
        return (int)host("glXGetSwapIntervalMESA", s);
    }
    GUEST(int, "glXGetSwapIntervalMESA", (void));
}

ARMOR_EXPORT int glXSwapIntervalSGI(int interval) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {I(interval)};
        return (int)host("glXSwapIntervalSGI", s);
    }
    GUEST(int, "glXSwapIntervalSGI", (int), interval);
}

ARMOR_EXPORT void glXWaitGL(void) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {0};
        host("glXWaitGL", s);
        return;
    }
    GUEST_VOID("glXWaitGL", (void));
}

ARMOR_EXPORT void glXWaitX(void) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {0};
        host("glXWaitX", s);
        return;
    }
    GUEST_VOID("glXWaitX", (void));
}

ARMOR_EXPORT XID glXCreateWindow(Display *dpy, GLXFBConfig config, XID window,
                                 const int *attributes) {
    if (armor_gl_on_host()) {
        guest_sync(dpy);
        uint64_t s[16] = {P(config), window, P(attributes)};
        return (XID)host("glXCreateWindow", s);
    }
    GUEST(XID, "glXCreateWindow", (Display *, GLXFBConfig, XID, const int *), dpy, config, window,
          attributes);
}

ARMOR_EXPORT void glXDestroyWindow(Display *dpy, XID window) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {window};
        host("glXDestroyWindow", s);
        return;
    }
    GUEST_VOID("glXDestroyWindow", (Display *, XID), dpy, window);
}

ARMOR_EXPORT GLXContext glXGetCurrentContext(void) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {0};
        return (GLXContext)(uintptr_t)host("glXGetCurrentContext", s);
    }
    GUEST(GLXContext, "glXGetCurrentContext", (void));
}

ARMOR_EXPORT XID glXGetCurrentDrawable(void) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {0};
        return (XID)host("glXGetCurrentDrawable", s);
    }
    GUEST(XID, "glXGetCurrentDrawable", (void));
}

ARMOR_EXPORT XID glXGetCurrentReadDrawable(void) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {0};
        return (XID)host("glXGetCurrentDrawable", s);
    }
    GUEST(XID, "glXGetCurrentReadDrawable", (void));
}

ARMOR_EXPORT Display *glXGetCurrentDisplay(void) {
    if (armor_gl_on_host()) {
        return current_display;
    }
    GUEST(Display *, "glXGetCurrentDisplay", (void));
}

ARMOR_EXPORT int glXQueryContext(Display *dpy, GLXContext context, int attribute, int *value) {
    if (armor_gl_on_host()) {
        uint64_t s[16] = {P(context), I(attribute), P(value)};
        return (int)host("glXQueryContext", s);
    }
    GUEST(int, "glXQueryContext", (Display *, GLXContext, int, int *), dpy, context, attribute,
          value);
}

/* This library's own function `name`: the stubs, for glXGetProcAddress. */
static void *own(const char *name) {
    static void *self;
    if (!self) {
        Dl_info info;
        if (dladdr((void *)own, &info) && info.dli_fname) {
            self = dlopen(info.dli_fname, RTLD_NOW | RTLD_NOLOAD);
        }
    }
    return self ? dlsym(self, name) : NULL;
}

typedef void (*proc)(void);

ARMOR_EXPORT proc glXGetProcAddressARB(const unsigned char *name) {
    if (armor_gl_on_host()) {
        return (proc)own((const char *)name);
    }
    GUEST(proc, "glXGetProcAddressARB", (const unsigned char *), name);
}

ARMOR_EXPORT proc glXGetProcAddress(const unsigned char *name) {
    return glXGetProcAddressARB(name);
}
