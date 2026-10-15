/*
 * OpenGL through GLX on an X window (ADR-0041): a 64x64 window cleared to
 * one colour, three axis-aligned rectangles of flat colour drawn with the
 * scissor test, read back with glReadPixels. Every conforming renderer
 * gives the same pixels, so the checksum is the same with the guest's
 * software Mesa (llvmpipe, under qemu or armor) and with the host's GPU
 * (armor --host-gl); the renderer's name differs, and is printed on
 * stderr. Xlib and GL are loaded with dlopen, so no X or GL headers are
 * needed to build it. Needs an X display ($DISPLAY).
 */
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef void Display;
typedef unsigned long XID;
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
typedef struct {
    XID background_pixmap;
    unsigned long background_pixel;
    XID border_pixmap;
    unsigned long border_pixel;
    int bit_gravity, win_gravity, backing_store;
    unsigned long backing_planes, backing_pixel;
    int save_under;
    long event_mask, do_not_propagate_mask;
    int override_redirect;
    XID colormap;
    XID cursor;
} XSetWindowAttributes;

#define LOAD(lib, name) void *name##_p = dlsym(lib, #name); if (!name##_p) { fprintf(stderr, "missing %s\n", #name); return 2; }

int main(void) {
    void *x11 = dlopen("libX11.so.6", RTLD_NOW | RTLD_GLOBAL);
    void *gl = dlopen("libGL.so.1", RTLD_NOW | RTLD_GLOBAL);
    if (!x11 || !gl) {
        fprintf(stderr, "cannot load libX11 or libGL: %s\n", dlerror());
        return 2;
    }
    LOAD(x11, XOpenDisplay) LOAD(x11, XDefaultScreen) LOAD(x11, XRootWindow)
    LOAD(x11, XCreateColormap) LOAD(x11, XCreateWindow) LOAD(x11, XMapWindow) LOAD(x11, XSync)
    LOAD(x11, XDestroyWindow) LOAD(x11, XCloseDisplay) LOAD(x11, XFree)
    LOAD(gl, glXChooseVisual) LOAD(gl, glXCreateContext) LOAD(gl, glXMakeCurrent)
    LOAD(gl, glXDestroyContext) LOAD(gl, glGetString) LOAD(gl, glViewport) LOAD(gl, glClearColor)
    LOAD(gl, glClear) LOAD(gl, glEnable) LOAD(gl, glScissor) LOAD(gl, glFinish)
    LOAD(gl, glReadPixels) LOAD(gl, glGetError)

    Display *(*XOpenDisplay)(const char *) = XOpenDisplay_p;
    int (*XDefaultScreen)(Display *) = XDefaultScreen_p;
    XID (*XRootWindow)(Display *, int) = XRootWindow_p;
    XID (*XCreateColormap)(Display *, XID, void *, int) = XCreateColormap_p;
    XID (*XCreateWindow)(Display *, XID, int, int, unsigned, unsigned, unsigned, int, unsigned,
                         void *, unsigned long, XSetWindowAttributes *) = XCreateWindow_p;
    int (*XMapWindow)(Display *, XID) = XMapWindow_p;
    int (*XSync)(Display *, int) = XSync_p;
    int (*XDestroyWindow)(Display *, XID) = XDestroyWindow_p;
    int (*XCloseDisplay)(Display *) = XCloseDisplay_p;
    int (*XFree)(void *) = XFree_p;
    XVisualInfo *(*glXChooseVisual)(Display *, int, int *) = glXChooseVisual_p;
    void *(*glXCreateContext)(Display *, XVisualInfo *, void *, int) = glXCreateContext_p;
    int (*glXMakeCurrent)(Display *, XID, void *) = glXMakeCurrent_p;
    void (*glXDestroyContext)(Display *, void *) = glXDestroyContext_p;
    const unsigned char *(*glGetString)(unsigned) = glGetString_p;
    void (*glViewport)(int, int, int, int) = glViewport_p;
    void (*glClearColor)(float, float, float, float) = glClearColor_p;
    void (*glClear)(unsigned) = glClear_p;
    void (*glEnable)(unsigned) = glEnable_p;
    void (*glScissor)(int, int, int, int) = glScissor_p;
    void (*glFinish)(void) = glFinish_p;
    void (*glReadPixels)(int, int, int, int, unsigned, unsigned, void *) = glReadPixels_p;
    unsigned (*glGetError)(void) = glGetError_p;

    Display *dpy = XOpenDisplay(NULL);
    if (!dpy) {
        fprintf(stderr, "no X display\n");
        return 2;
    }
    int screen = XDefaultScreen(dpy);
    /* GLX_RGBA, GLX_RED_SIZE 8, GLX_GREEN_SIZE 8, GLX_BLUE_SIZE 8, GLX_DOUBLEBUFFER */
    int attributes[] = {4, 8, 8, 9, 8, 10, 8, 5, 0};
    XVisualInfo *visual = glXChooseVisual(dpy, screen, attributes);
    if (!visual) {
        fprintf(stderr, "no visual\n");
        return 2;
    }
    XID root = XRootWindow(dpy, screen);
    XSetWindowAttributes window_attributes = {0};
    window_attributes.colormap = XCreateColormap(dpy, root, visual->visual, 0);
    /* CWColormap | CWBorderPixel */
    XID window = XCreateWindow(dpy, root, 0, 0, 64, 64, 0, visual->depth, 1, visual->visual,
                               (1L << 13) | (1L << 3), &window_attributes);
    XMapWindow(dpy, window);
    XSync(dpy, 0);
    void *context = glXCreateContext(dpy, visual, NULL, 1);
    if (!context || !glXMakeCurrent(dpy, window, context)) {
        fprintf(stderr, "no GL context\n");
        return 2;
    }
    fprintf(stderr, "renderer: %s\n", (const char *)glGetString(0x1F01));
    /* Draw into the back buffer and read it back (GL_BACK is the default
       read buffer of a double-buffered window). */
    glViewport(0, 0, 64, 64);
    glClearColor(0.0f, 0.0f, 1.0f, 1.0f);
    glClear(0x4000); /* GL_COLOR_BUFFER_BIT */
    glEnable(0x0C11); /* GL_SCISSOR_TEST */
    static const struct { int x, y, w, h; float r, g, b; } rects[] = {
        {4, 4, 20, 10, 1.0f, 0.0f, 0.0f},
        {30, 8, 12, 40, 0.0f, 1.0f, 0.0f},
        {10, 40, 50, 6, 1.0f, 1.0f, 1.0f},
    };
    for (int i = 0; i < 3; i++) {
        glScissor(rects[i].x, rects[i].y, rects[i].w, rects[i].h);
        glClearColor(rects[i].r, rects[i].g, rects[i].b, 1.0f);
        glClear(0x4000);
    }
    glFinish();
    static unsigned char pixels[64 * 64 * 4];
    glReadPixels(0, 0, 64, 64, 0x1908 /* GL_RGBA */, 0x1401 /* GL_UNSIGNED_BYTE */, pixels);
    uint64_t hash = 0xcbf29ce484222325ULL;
    for (size_t i = 0; i < sizeof pixels; i++) {
        hash = (hash ^ pixels[i]) * 0x100000001b3ULL;
    }
    printf("pixel (10,6) = %u %u %u %u\n", pixels[(6 * 64 + 10) * 4], pixels[(6 * 64 + 10) * 4 + 1],
           pixels[(6 * 64 + 10) * 4 + 2], pixels[(6 * 64 + 10) * 4 + 3]);
    printf("pixel (0,0) = %u %u %u %u\n", pixels[0], pixels[1], pixels[2], pixels[3]);
    printf("checksum = %016llx\n", (unsigned long long)hash);
    printf("gl error = %u\n", glGetError());
    glXMakeCurrent(dpy, 0, NULL);
    glXDestroyContext(dpy, context);
    XFree(visual);
    XDestroyWindow(dpy, window);
    XCloseDisplay(dpy);
    return 0;
}
