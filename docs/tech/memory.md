# Memory: address space, pages, memory model, JIT code

**Legend:** ✅ verified (with a note on how) · ⚠️ to verify.

## Address space
- Host: **47-bit** user VA ✅ (`CONFIG_ARM64_VA_BITS=47` in `/boot/config-6.18.50+rpt-rpi-2712`; the stack is near `0x7fff_xxxx_xxxx` in `/proc/self/maps`).
- x86_64: user space is the canonical lower half, `0x0000_0000_0000_0000`–`0x0000_7FFF_FFFF_FFFF` (47 bits) ⚠️ (SDM Vol. 1, canonical addresses).
- So **identity mapping** (guest address = host address) is possible (`architecture.md`).
- **48-bit host** (✅ 2026-10-15, Ubuntu 24.04's kernel with 64K pages, `6.8.0-147-generic-64k`, in a KVM virtual machine on the Pi 5): the host placed armor's first mappings above 2^47, out of an x86-64 guest's reach, and armor stopped ("guest page … beyond 47 bits"). A new `AddressSpace` now asks the host for one page first (`host_places_above`); when it lands above 2^47, the address space starts with 2^47 − 4096 as its ceiling (`set_ceiling`, as for i386 with 4 GiB; x86-64 Linux never gives the last page below 2^47 to user space, and armor's syscalls refuse it as EFAULT, `USER_END`): mappings whose place armor chooses are placed top-down below it. A first version used 2^47: the guest stack then ended in that last page, and `readv` into the stack failed with EFAULT. On the Pi's kernels (39 and 47 bits) nothing changes. ✅ `difftest` in that virtual machine (`testing.md`, "Other page sizes").
- **39-bit host** (✅ 2026-10-14, the Pi's 4K kernel, `CONFIG_ARM64_VA_BITS=39` in `/boot/config-6.18.50+rpt-rpi-v8`; many 4K kernels use it): a process has 512 GiB instead of 128 TiB. Identity mapping still works, in the smaller space: armor lets the kernel choose addresses when it can (a PIE goes to `PIE_BASE` only if it is there, otherwise wherever the kernel puts it, `loader.rs`), and `difftest` stays at 397/397 (lockstep included). The unit tests of `armor-mem` and `armor-jit` used fixed bases above 2^39 (`0x2000_0000_0000`, `0x2400_0000_0000`): they are now below (`0x20_0000_0000`, `0x30_0000_0000`). ⚠️ A guest request with `MAP_FIXED` above 512 GiB fails with ENOMEM where x86-64 would succeed: to be measured on real programs (Wine, JITs that tag pointers in the high bits).

Problem to handle: the guest and armor **share the same address space**. armor's mappings (binary, heap, host thread stacks, libraries) can collide with the addresses the guest asks for with `MAP_FIXED` or with the segments of `ET_EXEC` binaries, which start at `0x400000` ✅ (`readelf -l` on the test binaries and on a static glibc hello, see below).
Options to evaluate:
- `MAP_FIXED_NOREPLACE` for the guest's mappings;
- reserving regions for the guest in advance;
- tracking all host regions.

Measurements on the host ✅ (2026-10-12, `/proc/self/maps`, `/proc/sys/vm/mmap_min_addr`):
- the host's PIE executables, so `armor` too, are loaded around `0x5555_xxxx_xxxx`;
- `mmap_min_addr` = 4096;
- x86_64 `ET_EXEC` binaries at `0x400000` do not collide with armor's binary.
- ⚠️ Still to measure: where the host places `mmap` calls without an address (armor's heap, thread stacks): they can end up where a guest asks for `MAP_FIXED`.

## 4K pages in the guest on 16K hosts (ADR-0005)
The host has **16K** pages ✅ (`getconf PAGESIZE` = 16384). The guest must see 4K pages.

### Where the guest assumes 4K
- `AT_PAGESZ` in the auxv: armor passes **4096**.
- Alignment of addresses and lengths in `mmap`/`munmap`/`mprotect`/`madvise`.
- File offsets in `mmap`: multiples of 4K, not necessarily of 16K.
- Adjacent ELF `PT_LOAD` segments with different permissions in the same 16K page. ✅ **It already happens in the simplest binaries** (measured with `x86_64-linux-gnu-readelf -lW`, binutils 2.44):

  | Binary | `PT_LOAD` segments (address, permissions), all with `p_align` 0x1000 |
  |---|---|
  | `basic/exit42` (`-nostdlib -static -no-pie`) | `0x400000` R, `0x401000` R E |
  | `basic/hello` | `0x400000` R, `0x401000` R E, `0x402000` R |
  | C hello with static glibc | `0x400000` R, `0x401000` R E, `0x477000` R, `0x49fe48` RW |

  In the assembly tests **all** segments fall in the host page `0x400000`–`0x403fff`, with different permissions. In the glibc case, `0x477000` and `0x49fe48` are not 16K-aligned.
  **Consequence: the M1 loader must already handle several segments with different permissions in the same host page.**
- Stack guard pages (4K `PROT_NONE` pages).

### Candidate strategies (to decide)
1. **4K permission table** managed by `armor-mem`. The 16K host page gets the **union** of the permissions of its 4 subpages.
   - With the interpreter the guest code is never executed by the host, so in M1 it is enough for host pages to be readable and writable by armor. The guest's permissions live only in the table. This is the minimal proposal for M1.
   - The interpreter checks permissions in software, so it is slow but exact.
   - The JIT relies on the host and handles the exceptions. *Superseded by ADR-0021:* the JIT also checks permissions in software, on the flat table (`tech/jit.md`).
2. **File mappings with an offset not aligned to 16K:** they are read with `read` into anonymous memory instead of using `mmap`. ✅ Implemented for `MAP_PRIVATE` (ADR-0012): see below.
   - Page sharing is lost.
   - **With `MAP_SHARED` coherence with the file breaks:** handled separately, with real host shared mappings (ADR-0022, below).
3. **Small anonymous mappings:** allocate 16K regions and keep track of the subpages in use.
4. **Before choosing**, study how box64 and FEX tackle the problem. ⚠️ Not verified: FEX is said to have historically required a 4K host.

The available 4K kernel (`environment.md`) can be used to compare results, but it does not replace 16K support.

### M1 implementation (`armor-mem`, ADR-0009)
- `AddressSpace::map_fixed(addr, len, prot)`:
  - `addr` aligned to 4K, length rounded up to 4K;
  - maps the missing host pages (16K) with `MAP_FIXED_NOREPLACE`, always RW;
  - records `prot` for each 4K page and zeroes the host pages already in use (new ones are already zero).
  - If the host already uses that range it returns `AddressInUse`: verified by the test `mapping_over_host_memory_in_use_fails`.
- `map_anywhere(len, prot)`: address chosen by the host, aligned to 16K. Used for the stack.
- **Reservations** (since 2026-10-15): a guest mapping with `PROT_NONE` gets its host pages with `MAP_NORESERVE` (`reserve_flag`, in `map_fixed` and `map_anywhere`). Host pages are always RW, and the kernel charges a private RW mapping to its overcommit accounting, which refused V8's 32 GiB reservation (the "cage" of each Chromium renderer: `mmap` ENOMEM on the host, and Spotify's renderers ended); natively a `PROT_NONE` private mapping is not charged. Difference: when the guest later makes such pages writable, the kernel still does not charge them (native Linux would, and could refuse the `mprotect` with ENOMEM). ✅ Test `reserve_large` (32 GiB `PROT_NONE` with and without `MAP_NORESERVE`, 1 MiB of it made RW and used, four times): identical to qemu and to the Xeon; without the change every reservation failed under armor.
- `write_privileged`: writes ignoring the guest's permissions. The loader needs it, for read-only segments and for the initial stack.
- `GuestMemory::{read, write, fetch}`:
  - check the table page by page and report the first faulting byte;
  - `fetch` stops at the first non-executable page.
- `unmap(addr, len)` (for `munmap` and `brk`): removes the guest's pages; host pages with no guest pages left go back to the host with `munmap`, one call per contiguous run (since 2026-10-13; before, one per host page: 512 for a thread's 8 MB stack).
- `discard(addr, len)` (for `madvise(MADV_DONTNEED)`): the pages go back to zero; whole host pages go to the host's `madvise`, the other pieces are zeroed.
- `protect(addr, len, prot)` (for `mprotect`): fails without changing anything if a page is not mapped.
- `any_mapped(addr, len)` (for `brk`): whether a page of the range is mapped. `brk` does not grow over another mapping, like Linux (since 2026-10-13; before, it overwrote it).
- **Cost proportional to the mapped memory, not to the range** (since 2026-10-13, found by the syscall fuzzer, `testing.md`):
  - `map_fixed`, `unmap`, `protect`, `discard` and `any_mapped` visit only the host pages of the address space that fall in the range (`host_pages.range`). Guest pages exist only inside them. Before, a `munmap` or a `brk` of 2^46 bytes took 2^34 steps;
  - `map_fixed` zeroes only the host pages already present: those just mapped are already zero and stay untouched, so a large sparse mapping takes no memory until the guest uses it. Before, the zeroing touched the whole range;
  - ✅ tests `huge_ranges_cost_only_the_mapped_pages` (2^46 bytes in under a second) and `brk_does_not_grow_over_a_mapping`.
- Page roundings are checked: a range that leaves the 64-bit space gives `MapError::OutOfRange` (→ ENOMEM/EINVAL for the guest), never an overflow (test `huge_ranges_are_errors_not_panics`).
- `map_anywhere` records for the guest only the requested 4K pages, not the whole last 16K host page: an access past the end of the mapping faults as on the kernel.
- Write-only and execute-only pages are readable, as on x86. ⚠️ This is a remembered fact: x86 page tables without protection keys have no read bit. To verify in SDM Vol. 3, chapter on paging.
- Host pages stay mapped as long as they contain at least one guest page (`unmap`, above), and all of them until the `Drop` of the `AddressSpace`.
- `map_fixed` inside a range already mapped by the host (for example the `ld.so` reservation, below) maps nothing new: it only changes the table and zeroes.
- Known difference from the kernel ⚠️: if a segment has no bss, the kernel maps the whole last page of the file, so the bytes past `p_filesz` contain the rest of the file. armor instead leaves them at zero. It matters only for programs that read past the end of their own segment.

### File mappings (ADR-0012)
- `MAP_PRIVATE` file `mmap`: armor maps anonymous memory, as for `MAP_ANONYMOUS`, and fills it with `pread` from the file starting at the offset. Bytes past the end of the file stay at zero.
- Measured pattern ✅ (2026-10-12, `qemu-x86_64 -strace` on a dynamic hello with glibc 2.41): `ld.so` reserves the whole library with `mmap(NULL, 2055800, PROT_READ, MAP_PRIVATE|MAP_DENYWRITE, fd, 0)`, then remaps each segment with `MAP_FIXED` inside the reservation, at offsets like `0x18d000` and `0x1e2000` (multiples of 4K, not of 16K).
- Verified by `difftest` on the `c-dyn/*` programs.
- Known differences from the kernel:
  - a page entirely past the end of the file reads as zero instead of raising SIGBUS;
  - the mapping is a copy, so later changes to the file are not seen;
  - until 2026-10-12 `MAP_SHARED` on a file without `PROT_WRITE` was a private copy, and writable `MAP_SHARED` was not supported. Since 2026-10-13 every `MAP_SHARED` is a real shared mapping (below).

### Shared mappings (ADR-0022, since 2026-10-13)
- **`map_shared(fixed, len, prot, source, writable)`:**
  - 16K host pages belonging entirely to the mapping, mapped `MAP_SHARED` by the host, anonymous or from the file (`SharedSource`). No private guest page lives in a shared host page (`Tables::shared`, with the host's write permission);
  - without `MAP_FIXED` the address has the same remainder modulo 16K as the file offset, so it works with any offset that is a multiple of 4K;
  - with `MAP_FIXED` the same remainder is required, and the host pages touched must not contain other guest pages outside the range (not even a `PROT_NONE` reservation). Otherwise `MapError::SharedPlacement`, which becomes "not supported" with the reason;
  - free host pages are mapped with `MAP_FIXED_NOREPLACE`, those already owned by the `AddressSpace` are replaced in place with `MAP_FIXED`. No `munmap` first: another `mmap` by armor could grab the address in the meantime. This race was shown by a test that failed one time in five, in the replacement of a shared page with a private one (`make_private`).
- **Permissions:** the host maps as writable only the pages of a file opened for reading and writing. `mmap` or `mprotect` with `PROT_WRITE` on a file opened read-only → EACCES, like Linux. `PROT_EXEC` on shared memory → not supported: writes through another mapping or from another process would not invalidate the translated code.
- **Other operations:**
  - `discard` (`MADV_DONTNEED`) leaves shared pages as they are, because in Linux they reread the file or the shared memory;
  - `sync` does `msync` on the host pages;
  - `any_shared` tells `mremap` that a range is shared. Shrinking unmaps the tail; until 2026-10-14 growing was not supported. Since 2026-10-14 (Spotify) **`remap_shared`** grows it with the host's own `mremap`, so it stays the same memory (the same file pages or the same shared anonymous memory): in place when the host pages after it are free and not in armor's tables (`mremap` without `MREMAP_MAYMOVE`, which the host only extends into free pages); otherwise, if the guest allows a move, armor places an inaccessible placeholder where it would put any new mapping (below the ceiling for an i386 guest) and the host's `mremap` with `MREMAP_MAYMOVE | MREMAP_FIXED` moves the mapping over it; without the move → ENOMEM. The tables follow (the guest pages keep their permissions, the host pages stay shared) and translated code is invalidated (`code_changed`: the code may have moved or grown). The range must start and end on host page boundaries and hold only shared pages with one protection, else `SharedPlacement` ("unsupported" with the reason). ✅ unit test `shared_file_mappings_grow` (in place and moved: the bytes and the file stay the same) and `mremap_shared` (file and memfd, a `fork` child sees the grown mapping, ENOMEM without `MREMAP_MAYMOVE`; x86-64 and i386, identical to qemu and to the Xeon, 2026-10-14). ⚠️ All on the 4K kernel, where host and guest pages coincide; on 16K the boundary rule refuses ranges not aligned to 16K: from the code, not measured;
  - since 2026-10-15 `remap_shared` also grows a shared mapping that is smaller than a host page or does not end on a host page boundary (on 16K and 64K hosts, `c/mremap_shared` stopped there): the mapping's host pages are its own, so growing inside its last host page only gives the guest the pages it asks for (the guest pages there must be unmapped), and growing beyond extends or moves the host mapping from the start of its first host page, keeping the address's offset inside it. ✅ unit test `shared_mappings_smaller_than_a_host_page_grow`, and `c/mremap_shared` in all six variants on 16K and 64K hosts (`testing.md`, "Other page sizes");
  - a private mapping over shared host pages replaces them if no other guest page uses them.
- **Sharing with children:** the host's `fork`, which keeps `MAP_SHARED` pages shared (✅ `c/shared_memory`, also with `shm_open`).
- **System V shared memory** (since 2026-10-14, x86_64 `shmat`, for X11's MIT-SHM): `AddressSpace::attach_sysv` has the host attach the whole segment (the host's `shmat`), so it stays the same memory for the other processes; the host pages join the shared ones, the guest pages take the permissions from `SHM_RDONLY`/`SHM_EXEC`, and a writable attach turns on concurrent mode. Without an address armor chooses the place as for `mmap`; with an address, it must be on a host page boundary (on a 16K host an address aligned only to 4K is EINVAL, where x86 Linux accepts it: from the code) and the guest pages already there are replaced. Detaching is `unmap`: the host's `munmap` of an attach detaches it ⚠️ (remembered Linux behaviour; `sysv_shm` tests `shmdt` and the attach count with `IPC_STAT` ✅ against qemu and the Xeon, but on 4K pages).

### Atomic instructions and shared memory (since 2026-10-13)
- With shared memory, other processes (other armor instances, in parallel) write the same memory: a process's GIL is no longer enough to make `LOCK` instructions atomic.
- **Interpreter:** `cmpxchg8b` and `cmpxchg16b` are always an 8- and 16-byte host compare-and-swap (`atomic_update`, `compare_exchange_16` with `CASPAL`); before 2026-10-13 (evening) they were two loads and a store, a gap found in a review of the work (✅ `c/shared_atomics` with 64- and 128-bit counters; mutation 5 out of 5). `LOCK` instructions and `xchg` with memory go through `GuestMemory::atomic_update`, which in `armor-mem` is a compare-and-swap on host memory (`host_atomic_update`) if the operand is aligned to its size. A misaligned operand ("split lock") stays a read and a write: x86 makes it atomic, armor does not ⚠️ (rare case).
- **JIT:** outside concurrent mode it translates `LOCK` as ordinary read-modify-writes (`tech/jit.md`, increment 16). In concurrent mode (below: from the first writable shared mapping or the second thread, `MemView::is_concurrent`) the code generation advances and the JIT translates `LOCK` and `xchg` with memory into LSE atomics (`LDADDAL`, `SWPAL`, `CASAL`…, ADR-0023 step 4, `tech/jit.md`) if the operand is aligned; the other cases go to the interpreter.
- **Concurrent mode (ADR-0023, step 1 of 2026-10-13):**
  - it turns on at the first writable shared mapping or at the first extra thread (`AddressSpace::enter_concurrent_mode`), and advances the code generation;
  - the interpreter puts an acquire barrier after each guest load and a release barrier before each store (`MemView::read`/`write`): x86's TSO ordering;
  - the `mfence`, `lfence`, `sfence` barriers are full host barriers.
  - the JIT, from step 2, orders its loads and stores with `LDAPR`/`STLR` (`tech/jit.md`, "Memory ordering in concurrent mode").
- **Ordering of ordinary loads and stores across processes:** in concurrent mode it follows TSO as across threads (below, "Memory model"). Until step 2 of ADR-0023 it was a limit ⚠️.
- **Lockstep:** an instruction that reads or writes shared memory cannot be executed twice or compared, because another process can change it in between, and undoing a write would overwrite that process's write. So lockstep executes it only once with the interpreter, without comparison (`lockstep.rs`, `MemView::in_shared`).
- ✅ `c/shared_atomics`: three processes update the same shared page in parallel (`lock xadd`, `lock cmpxchg` loop, spinlock with `xchg`, `lock or`, `lock bts`, at 8–64 bits). Exact values with both engines. With the JIT without the lock the spinlock stays locked forever, and with the non-atomic update increments are lost (mutations, 5 runs out of 5).

### Loading ELF files (`loader.rs`)
- `ET_EXEC`: segments at the file's addresses, as before.
- Executable `ET_DYN` (PIE): reserves the whole range of the segments at `PIE_BASE` = `0x5555_5555_4000`, the `ELF_ET_DYN_BASE` of the x86-64 kernel without randomisation. ✅ It is also where qemu-x86_64 puts it: the first `brk(NULL)` of a dynamic hello under qemu is `0x555555559000`, the end of the executable. If the host already uses that address, armor falls back to an address chosen by the host.
- Interpreter (`ld.so`): reserves the range where the host chooses (`map_anywhere`, `PROT_NONE`), then maps the segments with `map_fixed` inside the reservation. The distances between segments stay those of the file; the holes stay inaccessible to the guest.
- `brk` starts at the end of the executable, PIEs included.
- **Where the files come from:** program, interpreter and libraries are looked up in the guest root (ADR-0013, `architecture.md`); since 2026-10-14 `/proc`, `/sys`, `/dev`, `/run` and `/tmp` always come from the host, even when the root has them ✅ (unit test `kernel_and_runtime_directories_stay_on_the_host`; `tech/syscalls.md`, "Guest paths").
- **Seeing the mappings** (since 2026-10-14): private file mappings are copies (ADR-0012), so the host's `/proc/<pid>/maps` does not show which file an address belongs to. With `ARMOR_TRACE_MAPS=1` armor prints on stderr every guest file mapping (`armor: map <start>-<end> <file> offset .. prot ..`) and where the loader put the program and the interpreter (`armor: map program … at base …`, `armor: map interpreter … at base …`) ✅ (used on Spotify, 2026-10-14).

### 32-bit guests (ADR-0029, since 2026-10-14)
- An i386 program sees only the low 4 GiB, so **every mapping whose address armor chooses must stay below 4 GiB**: stack, interpreter, `mmap` without an address, `mremap` that moves, `MAP_SHARED`. The loader calls `AddressSpace::set_ceiling(I386_CEILING)` before mapping anything; from then on `map_anywhere` and `map_shared` without an address search **top-down** below the ceiling for the first free range: they skip the host pages already in armor's tables (since 2026-10-21 jumping below the lowest page of armor's in the window tried, which every window ending above it holds, instead of below the highest: the old step moved down one page at a time through every mapping above, 11% of `bench`'s `memory` workload; the range found is the same), then try with `MAP_FIXED_NOREPLACE`, going down one host page if the kernel answers EEXIST (host memory armor does not know about). It does not go below `0x10000` (default `vm.mmap_min_addr`). ✅ unit test `mappings_stay_below_the_ceiling` (mappings below the ceiling, top-down, never overlapping, reuse of a freed range, `MAP_SHARED` too).
- `I386_CEILING` = `0xffff_e000`, the end of the user space of a 32-bit process on a 64-bit kernel (`IA32_PAGE_OFFSET`) ⚠️ remembered from the kernel source; to be measured by printing the stack address of an i386 program on the Xeon.
- i386 PIE at `I386_PIE_BASE` = `0x400000` ⚠️ (`ELF_ET_DYN_BASE` of a 32-bit process, remembered); ✅ `qemu-i386 -strace` of a static-pie hello: the first `brk(NULL)` is `0x4a9000`, the end of the image loaded at `0x400000`.
- ✅ That the host has no mappings below 4 GiB is measured (ADR-0029, "Context", 2026-10-13, with the 16K kernel; with the 4K one the i386 cases of `difftest` pass, 2026-10-14): if that changed, the guest's mappings would move down below the host's (EEXIST), without touching them.
- The CPU's address arithmetic in 32-bit mode truncates to 32 bits after adding the segment base: `gs:[-8]` with base `B` is `B + 0xfffffff8` modulo 2^32, that is `B - 8` ✅ (`i386/basic/tls`, against the Xeon).

### Memory shared between threads (ADR-0018)
- `AddressSpace` is shared by all guest threads: all methods take `&self` and the tables (permissions per guest page, host pages) sit behind an `RwLock` once a second thread may use them (`TableLock`, since 2026-10-21). Before that, with one guest thread, the tables are borrowed without the lock and without atomic operations: the borrows are counted as `RefCell` counts them (with plain loads and stores, in every build), and a borrow for writing while another one lives panics, where the lock would deadlock. `clone` with `CLONE_THREAD` calls `AddressSpace::share_between_threads` before the new thread exists (it asserts that no borrow lives across it, and turns concurrent mode on); from then on every borrow takes the lock. Concurrent mode alone, from a writable shared mapping, keeps the tables unlocked: another process cannot reach them. ✅ A pair of read-lock operations cost 20 ns on the Pi 5 (a test program, 10 million pairs, with or without LSE atomics inline), and a syscall took two (the block's `view`, the syscall's own access to its buffer). Tests: `page_tables_alone_then_locked` (reads inside a view, writes, an exclusive borrow, then three threads mapping and reading), `page_tables_shared_while_viewed_panic`, `page_tables_written_while_viewed_panic`.
- Guest accesses copy the bytes **under the read lock**, so no thread can unmap host pages during the copy. Races between guest threads on the bytes themselves belong to the guest, as on real hardware.
- Measured cost (2026-10-12, release): the `c/malloc` test, single-threaded, went from 0.296 s to 0.357 s because of the read lock on every access. Fixed the same day (ADR-0020): the interpreter takes the lock once per block of instructions with `AddressSpace::view()`, which also has a small permission TLB; `c/malloc` drops to 0.119 s (`tech/performance.md`).
- **Flat permission table** (since 2026-10-12, ADR-0021): one byte per 4K guest page (`MAPPED`, the R, W, X bits and derived bits for the JIT: "readable" and "writable not executable" since increment 11, the same for this page and the next since increment 18, `tech/jit.md`) in an array of 2^35 bytes reserved with `MAP_NORESERVE`; it replaces the `BTreeMap` (same operations: `get`, `insert`, `remove`). Host pages stay in a `BTreeSet`. A mapping beyond 47 bits → `MapError::OutOfRange`.
- **Code generation:** `AddressSpace` increments a counter on every write to an executable page and on every change of mapping or permissions that touches executable pages. The decoded instruction cache (`armor-cpu`) discards entries from earlier generations (ADR-0020).
- Writes made by syscalls (for example the `read` buffer, the `CLONE_CHILD_CLEARTID` tid) happen in parallel with the other threads, like the kernel's writes to user memory. **Never a non-atomic read-modify-write in a syscall:** a `futex` check that reread and rewrote the word lost mutex updates. It was found by `c/threads`, which hung one time in 20 (fixed: only the page's permissions are checked).

### Host protection (ADR-0046, since 2026-10-20)
On hosts with 4K pages, `armor` (with the JIT) calls `AddressSpace::enable_host_protection` before loading the program; `ARMOR_PERMISSIONS=table` keeps the table checks, to compare the two. Hosts with 16K and 64K pages, and the unit tests, keep ADR-0009 alone.
- **The guest's region:** from `0x10000` to 2^47 less a page on hosts with 48 bits or more, to the lower half on smaller ones (2^38 with the Pi's 39 bits), reserved with one inaccessible `MAP_NORESERVE` mapping (it costs no memory). If anything of the host's is already there the reservation fails and host protection stays off. The ceiling becomes the region's end, so every mapping whose place armor chooses goes inside it, top-down; armor's own later mappings, placed by the kernel, land above it. A guest mapping inside the region first removes the reservation where it goes (`unreserve`, then the host's `MAP_FIXED_NOREPLACE`, `mremap` in place or `shmat` as before), and a released host page goes back into the reservation (`release`) instead of being unmapped; a failed mapping puts the reservation back (`reserve`). A guest `MAP_FIXED` beyond the region fails with ENOMEM.
- **Host protections follow the guest's** (`apply_protection`, after every change of the table; `host_prot_of`): inaccessible for a guest page that allows no access, read-write for a writable page that is not executable (and not a shared page the host maps read-only), read-only otherwise: a write to code faults, so that it reaches the interpreter, which drops translations. armor's own accesses that the host protection would refuse (the loader writing a read-only segment, `mremap`'s copy, the interpreter writing code) make the pages accessible for the copy (`with_access`, one thread at a time). A 64 KiB guard is reserved above the region (`REGION_GUARD`).
- ✅ `cat /proc/<pid>/maps` of a guest `sleep` under armor on the Pi: `00010000-3fff1c4000 ---p` (the reservation), the guest's mappings right below 2^38.
- ⚠️ Between `unreserve` and the host's mapping there is a window in which a mapping of armor's own, placed by the kernel, could land in the hole; the kernel places top-down below the region's end, far from the holes, so it is not expected: to be checked if a guest mapping ever fails with EEXIST.

## Memory model (relevant with multiple threads)
- **Today (ADR-0023, since 2026-10-13):** with a single thread and no shared memory nothing has changed. From the second thread, or from the first writable shared mapping, the process is in **concurrent mode**:
  - guest threads execute code in parallel, without a GIL (which stays for `--lockstep` and with `ARMOR_GIL=1`);
  - accesses are ordered as in TSO: `LDAPR`/`STLR` in the JIT, barriers in the interpreter;
  - `LOCK` and `xchg` with memory are host atomics.
- ✅ Verified with the litmus tests (`c/litmus`, `testing.md`): 0 forbidden results with both engines. The JIT without ordering but with parallel threads instead gives forbidden MP results in 5 runs out of 5 (15 to 20 per run): the litmus tests see the error in armor itself.
  - The litmus tests cannot show that the interpreter's barriers are necessary: without them they still give 0, because at every instruction the interpreter already does a load-acquire (the code generation). They stay, because the architecture requires them.
- ✅ 50 runs per engine of `c/threads`, `c/threads_signals`, `c/shared_atomics`, `c/litmus` and `c/processes`, all identical to qemu (2026-10-13).
- Known limits: a misaligned `LOCK` is not atomic (⚠️, ADR-0023). A block already executing when another thread modifies the code finishes with the old code, and the new code applies from the next block.
- x86 has a **TSO** model ⚠️ (SDM Vol. 3, chapter "Memory Ordering"):
  - stores are not reordered with other stores;
  - loads are not reordered with other loads;
  - a load can be moved ahead of an earlier store to a different address.
- ARM has a weaker model. The Cortex-A76 does **not** have a hardware TSO mode.
- Features available on the host ✅ (`/proc/cpuinfo`):
  - `atomics` (LSE: `CAS`, `LDADD`, `SWP`, …);
  - `lrcpc` (`LDAPR`).

Consequences:
- **The interpreter too** is subject to reorderings, if it performs the guest's loads and stores with ordinary Rust accesses while other guest threads run in parallel. Decided with ADR-0023: in concurrent mode an acquire barrier after each guest load and a release barrier before each store (`MemView::read`/`write`).
- **JIT:** `LDAPR` for all loads and `STLR` for all stores in concurrent mode (ADR-0023, done). The faster heuristics, which order only the accesses that other threads can see, are postponed (ADR-0023, "Alternatives considered").
- `LOCK` instructions: host atomics in the interpreter, LSE atomics in the JIT (above). ARM atomics require aligned accesses, while x86 accepts `LOCK` on misaligned data too (split lock): armor executes a split lock as a read plus a write, not atomic ⚠️ (ADR-0023).

## JIT code memory (M6)
- **W^X:** the code is written to an RW page which is then made RX with `mprotect`. Alternatively a double mapping is used (the same memory mapped RW and RX at different addresses). Double mapping chosen (`tech/jit.md`, increment 2).
- **Instruction cache coherence:** on AArch64, after writing code the sequence `DC CVAU` → `DSB ISH` → `IC IVAU` → `DSB ISH` → `ISB` is needed (or `__clear_cache`). Line sizes are read from `CTR_EL0`; the `IDC`/`DIC` bits can make some steps unnecessary.
- **Self-modifying code (SMC):** *superseded by ADR-0021:* writes to executable pages go to the JIT's slow path, without host protection (`tech/jit.md`). The original idea was:
  - after translation the guest's code pages are write-protected;
  - a write raises a fault that invalidates the translated blocks;
  - with 16K hosts the granularity is 16K, so there will be spurious invalidations.

### Writers before readers on the page tables ✅
Guest threads take the page tables for reading once per block of instructions (`AddressSpace::view`); changes (mmap, mprotect, a fork) take them for writing. A busy guest thread kept the host's read-write lock from a waiting writer in practice: `c/fork_threads` hung with the interpreter in 7–11 runs out of 30 (2026-10-14). `view` now waits, briefly and at most 10,000 yields, while `writers_waiting` is not zero; measured after the change: 0 hangs in 100 interpreter runs and 30 JIT runs.
