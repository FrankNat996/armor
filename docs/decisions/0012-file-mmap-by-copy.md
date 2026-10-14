# 0012 — File `mmap`: copy into anonymous memory

- **Status:** Accepted (2026-10-12). Superseded by 0022 for `MAP_SHARED` (2026-10-13); for `MAP_PRIVATE` it remains valid.
- **Date:** 2026-10-12
- Answers the file-mapping part of how 4K guest pages work on 16K hosts (ADR-0005).

## Context
The x86-64 dynamic loader maps libraries with file `mmap` at offsets and addresses that are multiples of 4K but not of 16K (measured on 2026-10-12 with `qemu-x86_64 -strace` on a dynamic hello: offsets `0x28000`, `0x18d000`, `0x1e2000`; addresses such as `0x…a279d000`). The host has 16K pages, so a host `mmap` of the file at that offset and at that address is impossible. Moreover, the guest's permissions already live in the 4K table (ADR-0009) and host pages are always RW.

## Decision
- A `MAP_PRIVATE` file mapping becomes anonymous memory (with `map_fixed` or `map_anywhere`, as for anonymous mappings) into which armor copies the file with `pread`, in chunks.
- The bytes past the end of the file stay zero.
- *Superseded by ADR-0022 (2026-10-13) for `MAP_SHARED`, which is now a real host shared mapping; `MAP_PRIVATE` remains a copy as here.* The rest of this paragraph describes 2026-10-12, before ADR-0022: `MAP_SHARED` on files was not supported (`Unsupported`, exit 125); addition of the same day (coreutils): without `PROT_WRITE` a shared mapping was read as a private copy. Since ADR-0022 every `MAP_SHARED`, including on files and read-only, is a host shared mapping.
- `MAP_DENYWRITE` and `MAP_EXECUTABLE` are accepted and ignored, as the kernel does.

## Alternatives considered
- **Host `mmap` of the file when offset and address are aligned to 16K, copy in the other cases:** shares the pages with the page cache, but the measured cases are not aligned. It can be added later as an optimisation.
- **Realigning the address:** impossible, because `ld.so` uses `MAP_FIXED` at computed addresses.

## Consequences
- \+ Simple, independent of the host page size, consistent with the permission table.
- − Every process copies the libraries it uses: more memory and start-up time. To be measured with large programs.
- − Pages entirely past the end of the file read as zero, whereas the kernel would raise SIGBUS on access. This matters only for programs that read past the end of a mapped file.
- − A later change to the file is not seen in the mapping. For `MAP_PRIVATE` the behaviour is unspecified by POSIX anyway.
- Left open: `MAP_SHARED` and JIT memory. *Closed on 2026-10-13: `MAP_SHARED` with ADR-0022; JIT memory with ADR-0021, because the JIT checks 4K permissions in software.*
