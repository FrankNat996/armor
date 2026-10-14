/*
 * DRM_IOCTL_VERSION on the host's GPU render node, as Mesa asks it which
 * driver to load (a Steam game's libGL did): a name buffer too short (the
 * kernel fills what fits and writes back the full length), then one large
 * enough. The output depends on the machine's GPU: compared with qemu on
 * the same machine, not with the hardware oracle (no GPU there).
 */
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

struct drm_version {
    int major, minor, patch;
    size_t name_len;
    char *name;
    size_t date_len;
    char *date;
    size_t desc_len;
    char *desc;
};

#define DRM_IOCTL_VERSION 0xc0406400UL

int main(void) {
    int fd = open("/dev/dri/renderD128", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        printf("no render node\n");
    } else {
        char tiny[2] = {'x', 'x'};
        struct drm_version v = {.name_len = sizeof tiny, .name = tiny};
        int r = ioctl(fd, DRM_IOCTL_VERSION, &v);
        printf("short buffer: %d, full length %zu, first byte copied %d\n", r, v.name_len,
               tiny[0] != 'x');
        char name[64] = {0}, date[64] = {0}, desc[128] = {0};
        struct drm_version full = {.name_len = sizeof name - 1, .name = name,
                                   .date_len = sizeof date - 1, .date = date,
                                   .desc_len = sizeof desc - 1, .desc = desc};
        r = ioctl(fd, DRM_IOCTL_VERSION, &full);
        printf("version: %d, name %s (length %zu), date and description non-empty %d %d\n", r,
               name, full.name_len, full.date_len > 0, full.desc_len > 0);
        close(fd);
    }
    return 0;
}
