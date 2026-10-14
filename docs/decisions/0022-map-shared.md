# 0022 — `MAP_SHARED`: real host shared mappings, in whole host pages

- **Status:** Accepted (2026-10-13). Real host shared mappings when address and offset are compatible with 16K pages, and an explicit, documented error in the other cases; the details below must be revised if a real program contradicts them.
- **Date:** 2026-10-13

## Context
Until today:
- `MAP_SHARED` of a file without `PROT_WRITE` was a private copy (ADR-0012): correct only as long as the file does not change;
- writable `MAP_SHARED` and `MAP_SHARED | MAP_ANONYMOUS` were "not supported".

Many programs need them:
- memory shared between parent and children after `fork`;
- `shm_open`, which goes through `/dev/shm`;
- files updated through memory, like databases.

The constraint is the same as in ADR-0005 and ADR-0012: the guest sees 4K pages, the host uses 16K ones. A host page is shared or private as a whole, and a host file `mmap` wants an offset that is a multiple of 16K.

## Decision
- **Dedicated host pages, truly shared.**
  - Every shared mapping occupies 16K host pages entirely its own, mapped with the host's `MAP_SHARED` (anonymous, or from the guest's file).
  - Sharing with children after `fork` and coherence with the file are provided by the kernel.
  - The 4K pages at the edges of the host page that the guest did not ask for stay unmapped for it.
- **Without `MAP_FIXED` it always works.**
  - With an offset `o`, armor has the host map the file from `o` rounded down to 16K.
  - It gives the guest the address with the same remainder modulo 16K: `address ≡ o (mod 16K)`.
- **With `MAP_FIXED`** it works if:
  - for a file, `address ≡ offset (mod 16K)`;
  - the host pages touched do not contain, outside the requested range, other guest pages.

  A `PROT_NONE` reservation also counts as "another guest page": a shared `MAP_FIXED` inside a reservation must cover whole host pages. The guest pages inside the range are replaced, as in Linux. Otherwise armor stops with an explicit message ("not supported", exit 125): continuing would give the guest a mapping that is not the one it asked for.
- **Host permissions.**
  - The host pages of a shared mapping are writable only if the file is open for reading and writing (anonymous ones always).
  - `PROT_WRITE` on a file open read-only → EACCES, both for `mmap` and for `mprotect`, as in Linux.
  - The guest's permissions stay in the 4K table, as for the other pages (ADR-0009).
- **Not supported**, with the reason in the documents:
  - **executable shared mappings:** a write through another mapping of the same file, or from another process, does not go through armor's checks and would not invalidate the translated code (ADR-0020, ADR-0021);
  - **`mremap` of a shared mapping:** the copy it does today would break the sharing. *Update 2026-10-14:* a real program asked for it (Spotify): growing is now the host's own `mremap` of the same memory (`AddressSpace::remap_shared`, `tech/memory.md`), on host page boundaries; `MREMAP_FIXED` and `MREMAP_DONTUNMAP` stay unsupported.
- **The rest:**
  - `madvise(MADV_DONTNEED)` on shared pages does not zero them: in Linux it re-reads the content of the file or of the shared memory, which is what the guest already sees;
  - `msync` passes to the host on the host pages that cover the range;
  - a private mapping with `MAP_FIXED` over shared host pages replaces them if no other guest page uses them, otherwise it is "not supported";
  - `MAP_SHARED_VALIDATE` counts as `MAP_SHARED`, with unknown flags rejected.

## Alternatives considered
- **Copy with write-back** (`msync`/`munmap` write the file back): no coherence between processes nor with whoever writes the file while it is mapped.
- **4K host pages:** the Pi 5 kernel uses 16K pages (ADR-0005).
- **Moving nearby guest pages to make room:** addresses already given to the guest cannot change.

## Consequences
- \+ Memory shared between processes and files mapped for writing work as in Linux, with the kernel's coherence.
- \+ Read-only `MAP_SHARED` is no longer a copy: it sees changes to the file.
- − A shared mapping occupies at least one 16K host page, even if the guest asks for 4K.
- − Non-alignable `MAP_FIXED`, executable shared mappings and `mremap` of shared mappings stop with a message, until a real program asks for them (`mremap` growth: since 2026-10-14, when not on host page boundaries only).
- With parallel threads (M6) memory shared with other processes follows the same ordering rules as guest memory.
