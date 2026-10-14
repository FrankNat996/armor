/*
 * mremap: shrinking, growing in place or by moving (MREMAP_MAYMOVE),
 * contents kept, the errors of a few invalid calls, and realloc of a
 * large block (glibc uses mremap for blocks it got from mmap). Addresses
 * are not printed: they differ between emulators.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

static unsigned long checksum(const unsigned char *p, size_t n) {
  unsigned long sum = 0;
  for (size_t i = 0; i < n; i++) sum = sum * 31 + p[i];
  return sum;
}

int main(void) {
  size_t page = 4096;
  unsigned char *p = mmap(NULL, 8 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (p == MAP_FAILED) return 1;
  for (size_t i = 0; i < 8 * page; i++) p[i] = (unsigned char)(i * 7);
  unsigned long before = checksum(p, 4 * page);

  /* Shrink in place. */
  unsigned char *q = mremap(p, 8 * page, 4 * page, 0);
  printf("shrink: same address %d, checksum kept %d\n", q == p, checksum(q, 4 * page) == before);

  /* Grow without moving: may fail with ENOMEM, or succeed in place. */
  unsigned char *r = mremap(q, 4 * page, 6 * page, 0);
  if (r == MAP_FAILED)
    printf("grow in place: ENOMEM %d\n", errno == ENOMEM);
  else
    printf("grow in place: same address %d\n", r == q);
  if (r != MAP_FAILED) q = r;

  /* Grow, allowed to move: contents kept, new part zero and writable. */
  size_t old = r == MAP_FAILED ? 4 * page : 6 * page;
  unsigned char *s = mremap(q, old, 64 * page, MREMAP_MAYMOVE);
  if (s == MAP_FAILED) return 2;
  int zero = 1;
  for (size_t i = old; i < 64 * page; i++) zero &= s[i] == 0;
  s[64 * page - 1] = 1;
  printf("grow and move: checksum kept %d, new part zero %d\n", checksum(s, 4 * page) == before, zero);

  /* Errors: unaligned address, zero new size. The result first: the
     order in which printf's arguments are evaluated is unspecified. */
  errno = 0;
  int failed = mremap(s + 1, page, 2 * page, MREMAP_MAYMOVE) == MAP_FAILED;
  printf("unaligned: %d EINVAL %d\n", failed, errno == EINVAL);
  /* A zero size fails; Linux says EINVAL, qemu-x86_64 ENOMEM (a known
     discrepancy, docs/testing.md): only the failure is printed. */
  failed = mremap(s, page, 0, MREMAP_MAYMOVE) == MAP_FAILED;
  printf("zero size: %d\n", failed);
  munmap(s, 64 * page);

  /* realloc of large blocks. */
  char *big = malloc(1 << 20);
  memset(big, 'a', 1 << 20);
  for (int round = 2; round <= 8; round *= 2) {
    big = realloc(big, (size_t)round << 20);
    memset(big + ((size_t)(round / 2) << 20), 'a' + round, (size_t)(round / 2) << 20);
  }
  printf("realloc: checksum %lu\n", checksum((unsigned char *)big, 8 << 20));
  free(big);
  return 0;
}
