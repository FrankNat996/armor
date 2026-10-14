/*
 * uname of an i386 process on a 64-bit kernel: the machine is "x86_64"
 * (without setarch's PER_LINUX32 personality), as armor answers.
 * qemu-i386 says "i686", like a 32-bit kernel: --oracle only.
 */
#include <stdio.h>
#include <sys/utsname.h>

int main(void) {
    struct utsname names;
    if (uname(&names) != 0) {
        perror("uname");
        return 1;
    }
    printf("%s\n", names.machine);
    return 0;
}
