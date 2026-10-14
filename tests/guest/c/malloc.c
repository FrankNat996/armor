/* malloc from brk and mmap, memset, calloc, free. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(void) {
  char *small = malloc(32);
  strcpy(small, "small block");
  char *big = malloc(1 << 20);
  memset(big, 'x', 1 << 20);
  size_t n = 0;
  for (size_t i = 0; i < (1 << 20); i++) n += big[i] == 'x';
  int *arr = calloc(1000, sizeof(int));
  for (int i = 0; i < 1000; i++) arr[i] = i * i;
  long sum = 0;
  for (int i = 0; i < 1000; i++) sum += arr[i];
  printf("%s %zu %ld %zu\n", small, n, sum, strlen(small));
  free(small); free(big); free(arr);
  return 0;
}
