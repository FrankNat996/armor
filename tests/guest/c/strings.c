/*
 * glibc string and memory functions over many lengths and alignments.
 * Their SSE2 implementations use most of the SSE2 subset armor supports.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned long hash;

static void mix(unsigned long value) { hash = hash * 1000003UL ^ value; }

int main(void) {
  static char a[512 + 64], b[512 + 64];
  for (int len = 0; len < 300; len += (len < 40 ? 1 : 7)) {
    for (int align = 0; align < 16; align += 3) {
      char *s = a + align, *t = b + (15 - align);
      for (int i = 0; i < len; i++) s[i] = 'a' + (i * 7 + align) % 26;
      s[len] = 0;
      mix(strlen(s));
      mix(strchr(s, 'q') ? (unsigned long)(strchr(s, 'q') - s) : 999);
      mix(strrchr(s, 'e') ? (unsigned long)(strrchr(s, 'e') - s) : 999);
      mix(memchr(s, 'z', len) ? (unsigned long)((char *)memchr(s, 'z', len) - s) : 999);
      memcpy(t, s, len + 1);
      mix(memcmp(s, t, len + 1) == 0);
      mix(strcmp(s, t) == 0);
      if (len > 3) {
        t[len / 2] ^= 1;
        mix(memcmp(s, t, len) > 0);
        mix(strncmp(s, t, len / 2) == 0);
        mix(strcmp(s, t) < 0);
      }
      memmove(s + 1, s, len);
      mix((unsigned char)s[len]);
      memset(t, 'x', len);
      mix(strspn(t, "x"));
      strcpy(t, "needle");
      mix(strstr(s, "abc") ? 1 : 0);
    }
  }
  char buf[64];
  strcpy(buf, "hello");
  strcat(buf, ", ");
  strcat(buf, "world");
  printf("%s %zu %016lx\n", buf, strlen(buf), hash);
  return 0;
}
