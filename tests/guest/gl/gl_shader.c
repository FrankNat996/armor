/*
 * OpenGL 2 the way games use it (ADR-0041): a GLSL program, vertex
 * attributes read from the program's own memory, a uniform, and a texture
 * uploaded with glTexImage2D and sampled with GL_NEAREST. A 64x64 window
 * gets a 2x2 checker texture on a pixel-aligned quad (16..48), tinted by a
 * uniform colour; no pixel centre falls on a texel or triangle edge, so
 * every conforming renderer gives the same pixels and the checksum is the
 * same with the guest's Mesa and with the host's GPU (armor --host-gl).
 * Needs an X display ($DISPLAY).
 */
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>

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

/* Declares `name` as a function pointer of the given type, loaded from
   `lib`. */
#define LOAD(lib, name, ...)                                                   \
    __typeof__(__VA_ARGS__) name = (__typeof__(__VA_ARGS__))dlsym(lib, #name); \
    if (!name) {                                                               \
        fprintf(stderr, "missing %s\n", #name);                                \
        return 2;                                                              \
    }

static const char *vertex_source =
    "#version 120\n"
    "attribute vec2 position;\n"
    "attribute vec2 coord;\n"
    "varying vec2 v_coord;\n"
    "void main() {\n"
    "    v_coord = coord;\n"
    "    gl_Position = vec4(position, 0.0, 1.0);\n"
    "}\n";

static const char *fragment_source =
    "#version 120\n"
    "uniform sampler2D image;\n"
    "uniform vec4 tint;\n"
    "varying vec2 v_coord;\n"
    "void main() {\n"
    "    gl_FragColor = texture2D(image, v_coord) * tint;\n"
    "}\n";

int main(void) {
    void *x11 = dlopen("libX11.so.6", RTLD_NOW | RTLD_GLOBAL);
    void *gl = dlopen("libGL.so.1", RTLD_NOW | RTLD_GLOBAL);
    if (!x11 || !gl) {
        fprintf(stderr, "cannot load libX11 or libGL: %s\n", dlerror());
        return 2;
    }
    LOAD(x11, XOpenDisplay, Display *(*)(const char *))
    LOAD(x11, XDefaultScreen, int (*)(Display *))
    LOAD(x11, XRootWindow, XID (*)(Display *, int))
    LOAD(x11, XCreateColormap, XID (*)(Display *, XID, void *, int))
    LOAD(x11, XCreateWindow,
         XID (*)(Display *, XID, int, int, unsigned, unsigned, unsigned, int, unsigned, void *,
                 unsigned long, XSetWindowAttributes *))
    LOAD(x11, XMapWindow, int (*)(Display *, XID))
    LOAD(x11, XSync, int (*)(Display *, int))
    LOAD(x11, XDestroyWindow, int (*)(Display *, XID))
    LOAD(x11, XCloseDisplay, int (*)(Display *))
    LOAD(x11, XFree, int (*)(void *))
    LOAD(gl, glXChooseVisual, XVisualInfo *(*)(Display *, int, int *))
    LOAD(gl, glXCreateContext, void *(*)(Display *, XVisualInfo *, void *, int))
    LOAD(gl, glXMakeCurrent, int (*)(Display *, XID, void *))
    LOAD(gl, glXDestroyContext, void (*)(Display *, void *))
    LOAD(gl, glXGetProcAddressARB, void *(*)(const char *))
    LOAD(gl, glGetString, const unsigned char *(*)(unsigned))
    LOAD(gl, glViewport, void (*)(int, int, int, int))
    LOAD(gl, glClearColor, void (*)(float, float, float, float))
    LOAD(gl, glClear, void (*)(unsigned))
    LOAD(gl, glFinish, void (*)(void))
    LOAD(gl, glReadPixels, void (*)(int, int, int, int, unsigned, unsigned, void *))
    LOAD(gl, glGetError, unsigned (*)(void))
    LOAD(gl, glGenTextures, void (*)(int, unsigned *))
    LOAD(gl, glBindTexture, void (*)(unsigned, unsigned))
    LOAD(gl, glTexParameteri, void (*)(unsigned, unsigned, int))
    LOAD(gl, glPixelStorei, void (*)(unsigned, int))
    LOAD(gl, glTexImage2D,
         void (*)(unsigned, int, int, int, int, int, unsigned, unsigned, const void *))
    LOAD(gl, glDrawArrays, void (*)(unsigned, int, int))
    /* The OpenGL 2 functions, through glXGetProcAddressARB as games do. */
    unsigned (*glCreateShader)(unsigned) = glXGetProcAddressARB("glCreateShader");
    void (*glShaderSource)(unsigned, int, const char *const *, const int *) =
        glXGetProcAddressARB("glShaderSource");
    void (*glCompileShader)(unsigned) = glXGetProcAddressARB("glCompileShader");
    void (*glGetShaderiv)(unsigned, unsigned, int *) = glXGetProcAddressARB("glGetShaderiv");
    void (*glGetShaderInfoLog)(unsigned, int, int *, char *) =
        glXGetProcAddressARB("glGetShaderInfoLog");
    unsigned (*glCreateProgram)(void) = glXGetProcAddressARB("glCreateProgram");
    void (*glAttachShader)(unsigned, unsigned) = glXGetProcAddressARB("glAttachShader");
    void (*glBindAttribLocation)(unsigned, unsigned, const char *) =
        glXGetProcAddressARB("glBindAttribLocation");
    void (*glLinkProgram)(unsigned) = glXGetProcAddressARB("glLinkProgram");
    void (*glGetProgramiv)(unsigned, unsigned, int *) = glXGetProcAddressARB("glGetProgramiv");
    void (*glUseProgram)(unsigned) = glXGetProcAddressARB("glUseProgram");
    int (*glGetUniformLocation)(unsigned, const char *) =
        glXGetProcAddressARB("glGetUniformLocation");
    void (*glUniform1i)(int, int) = glXGetProcAddressARB("glUniform1i");
    void (*glUniform4f)(int, float, float, float, float) = glXGetProcAddressARB("glUniform4f");
    void (*glVertexAttribPointer)(unsigned, int, unsigned, unsigned char, int, const void *) =
        glXGetProcAddressARB("glVertexAttribPointer");
    void (*glEnableVertexAttribArray)(unsigned) =
        glXGetProcAddressARB("glEnableVertexAttribArray");
    if (!glCreateShader || !glShaderSource || !glCompileShader || !glGetShaderiv ||
        !glGetShaderInfoLog || !glCreateProgram || !glAttachShader || !glBindAttribLocation ||
        !glLinkProgram || !glGetProgramiv || !glUseProgram || !glGetUniformLocation ||
        !glUniform1i || !glUniform4f || !glVertexAttribPointer || !glEnableVertexAttribArray) {
        fprintf(stderr, "glXGetProcAddressARB returned null\n");
        return 2;
    }

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

    const char *sources[2] = {vertex_source, fragment_source};
    const unsigned kinds[2] = {0x8B31 /* GL_VERTEX_SHADER */, 0x8B30 /* GL_FRAGMENT_SHADER */};
    unsigned program = glCreateProgram();
    for (int i = 0; i < 2; i++) {
        unsigned shader = glCreateShader(kinds[i]);
        glShaderSource(shader, 1, &sources[i], NULL);
        glCompileShader(shader);
        int ok = 0;
        glGetShaderiv(shader, 0x8B81 /* GL_COMPILE_STATUS */, &ok);
        if (!ok) {
            char log[1024] = "";
            glGetShaderInfoLog(shader, sizeof log, NULL, log);
            fprintf(stderr, "shader %d: %s\n", i, log);
            return 1;
        }
        glAttachShader(program, shader);
    }
    glBindAttribLocation(program, 0, "position");
    glBindAttribLocation(program, 1, "coord");
    glLinkProgram(program);
    int linked = 0;
    glGetProgramiv(program, 0x8B82 /* GL_LINK_STATUS */, &linked);
    if (!linked) {
        fprintf(stderr, "link failed\n");
        return 1;
    }
    glUseProgram(program);

    /* 2x2 RGBA texture: red, green / blue, white. */
    static const unsigned char texels[16] = {255, 0,   0,   255, 0,   255, 0,   255,
                                             0,   0,   255, 255, 255, 255, 255, 255};
    unsigned texture = 0;
    glGenTextures(1, &texture);
    glBindTexture(0x0DE1 /* GL_TEXTURE_2D */, texture);
    glPixelStorei(0x0CF5 /* GL_UNPACK_ALIGNMENT */, 1);
    glTexParameteri(0x0DE1, 0x2801 /* MIN_FILTER */, 0x2600 /* GL_NEAREST */);
    glTexParameteri(0x0DE1, 0x2800 /* MAG_FILTER */, 0x2600);
    glTexImage2D(0x0DE1, 0, 0x1908 /* GL_RGBA */, 2, 2, 0, 0x1908, 0x1401 /* UNSIGNED_BYTE */,
                 texels);
    glUniform1i(glGetUniformLocation(program, "image"), 0);
    glUniform4f(glGetUniformLocation(program, "tint"), 0.0f, 1.0f, 1.0f, 1.0f);

    /* A quad from pixel 16 to 48 in both directions, as two triangles:
       x, y in clip space (-0.5..0.5), then the texture coordinates. */
    static const float vertices[] = {
        -0.5f, -0.5f, 0.0f, 0.0f, 0.5f,  -0.5f, 1.0f, 0.0f, 0.5f,  0.5f, 1.0f, 1.0f,
        -0.5f, -0.5f, 0.0f, 0.0f, 0.5f,  0.5f,  1.0f, 1.0f, -0.5f, 0.5f, 0.0f, 1.0f,
    };
    glVertexAttribPointer(0, 2, 0x1406 /* GL_FLOAT */, 0, 4 * sizeof(float), vertices);
    glVertexAttribPointer(1, 2, 0x1406, 0, 4 * sizeof(float), vertices + 2);
    glEnableVertexAttribArray(0);
    glEnableVertexAttribArray(1);

    glViewport(0, 0, 64, 64);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(0x4000); /* GL_COLOR_BUFFER_BIT */
    glDrawArrays(0x0004 /* GL_TRIANGLES */, 0, 6);
    glFinish();

    static unsigned char pixels[64 * 64 * 4];
    glReadPixels(0, 0, 64, 64, 0x1908, 0x1401, pixels);
    uint64_t hash = 0xcbf29ce484222325ULL;
    for (size_t i = 0; i < sizeof pixels; i++) {
        hash = (hash ^ pixels[i]) * 0x100000001b3ULL;
    }
    static const int probes[][2] = {{20, 20}, {40, 20}, {20, 40}, {40, 40}, {8, 8}};
    for (int i = 0; i < 5; i++) {
        const unsigned char *p = &pixels[(probes[i][1] * 64 + probes[i][0]) * 4];
        printf("pixel (%d,%d) = %u %u %u %u\n", probes[i][0], probes[i][1], p[0], p[1], p[2],
               p[3]);
    }
    printf("checksum = %016llx\n", (unsigned long long)hash);
    printf("gl error = %u\n", glGetError());
    glXMakeCurrent(dpy, 0, NULL);
    glXDestroyContext(dpy, context);
    XFree(visual);
    XDestroyWindow(dpy, window);
    XCloseDisplay(dpy);
    return 0;
}
