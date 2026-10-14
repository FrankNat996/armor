/*
 * Integer arithmetic through the compiler and glibc: division by constants
 * and variables, 128-bit products and quotients, bit tricks, qsort,
 * strtol and printf formats.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int compare(const void *x, const void *y) {
  long a = *(const long *)x, b = *(const long *)y;
  return (a > b) - (a < b);
}

int main(int argc, char **argv) {
  volatile long divisors[] = {1, -1, 3, -7, 10, 1000003, -2147483648L};
  long acc = 0;
  for (long n = -100000; n <= 100000; n += 997) {
    for (int i = 0; i < 7; i++) acc += n / divisors[i] + n % divisors[i];
    acc ^= (unsigned long)n / 13 + (unsigned long)n % 13;
  }
  unsigned __int128 big = (unsigned __int128)0xfedcba9876543210ULL * 0x123456789abcdefULL;
  unsigned long q = (unsigned long)(big / 0x1000000001ULL);
  unsigned x = 0x00f0f000;
  printf("acc=%ld q=%lx hi=%lx ctz=%d clz=%d pop=%d\n", acc, q, (unsigned long)(big >> 64),
         __builtin_ctz(x), __builtin_clz(x), __builtin_popcount(x));

  long values[64];
  unsigned long seed = 12345;
  for (int i = 0; i < 64; i++) {
    seed = seed * 6364136223846793005ULL + 1442695040888963407ULL;
    values[i] = (long)(seed >> 20) - (1L << 42);
  }
  qsort(values, 64, sizeof values[0], compare);
  printf("min=%ld max=%ld median=%ld\n", values[0], values[63], values[32]);

  printf("[%5d] [%-5d] [%+d] [%05x] [%o] [%llu] [%.3s] [%c] [%%]\n", 42, 42, 42, 255, 8,
         18446744073709551615ULL, "abcdef", 'Z');
  printf("strtol=%ld %ld %ld argc=%d\n", strtol("-12345", NULL, 10), strtol("ff", NULL, 16),
         strtol("0777", NULL, 0), argc);
  char out[32];
  int n = snprintf(out, sizeof out, "%d-%s", 7, "seven");
  printf("%s %d %d\n", out, n, (int)strlen(argv[0]) > 0);
  return 0;
}
