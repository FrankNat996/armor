/* printf of integers in several formats; exit status 3. */
#include <stdio.h>
int main(int argc, char **argv) {
  long values[] = {0, 1, -1, 42, 1234567890123L, -9876543210L};
  for (int i = 0; i < 6; i++) printf("%d: %ld %lx %lu\n", i, values[i], values[i], (unsigned long)values[i]);
  printf("argc=%d argv0_len_nonzero=%d\n", argc, argv[0][0] != 0);
  return 3;
}
