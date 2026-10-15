# Translation of x86_64 → aarch64 syscalls

**Legend:**
- ✅ = verified on the host on 2026-10-12:
  - aarch64 side: with a test program compiled with the host headers (`/usr/include/asm/unistd.h`, `<sys/stat.h>`, `<asm/stat.h>`, `<sys/epoll.h>`);
  - x86_64 side: with `/usr/x86_64-linux-gnu/include/asm/unistd_64.h` and with `offsetof` programs compiled by `x86_64-linux-gnu-gcc -static` and **run under `qemu-x86_64`**;
- ⚠️ = written from memory, with an indication of how to verify it.

## Calling convention
Registers: ✅ from `man 2 syscall` ("Arch/ABI" tables).

| | x86_64 | aarch64 |
|---|---|---|
| Instruction | `syscall` | `svc #0` |
| Number | `rax` | `w8` (`x8`) |
| Arguments 1–6 | `rdi, rsi, rdx, r10, r8, r9` | `x0`–`x5` |
| Return | `rax` (second value in `rdx`) | `x0` (second value in `x1`) |
| Clobber | `rcx` (← RIP), `r11` (← RFLAGS) ✅ SDM | — |

- The x32 ABI shares the x86_64 table and marks its syscalls with `__X32_SYSCALL_BIT` (✅ `man 2 syscall`). x32 is out of scope: armor must reject the numbers with that bit set.
- On both architectures raw syscalls return `-errno` (from −4095 to −1) on error. ⚠️ It is not written in `man 2 syscall`: to verify with `MAX_ERRNO` in `include/linux/err.h` of the Linux sources.
- ✅ The errno numbering matches: both `/usr/x86_64-linux-gnu/include/asm/errno.h` and `/usr/include/aarch64-linux-gnu/asm/errno.h` contain only `#include <asm-generic/errno.h>`.
- armor sets `rcx` and `r11` as the SDM requires ✅ (`RCX := RIP`, `R11 := RFLAGS`, extract on felixcloutier.com). **qemu-x86_64 does not** and leaves them unchanged (`testing.md`, known discrepancies).

## Implemented syscalls (`armor_linux::syscall`)
General rule: guest buffers always go through the checked path (`GuestMemory`). armor copies into host buffers and never passes a guest pointer to the kernel. Invalid address → `-EFAULT`.

**Guest paths** (M5, ADR-0013): the syscalls marked "root" resolve absolute paths in the guest root (`ARMOR_ROOT`, `crates/armor-linux/src/root.rs`). `/p` becomes `<root>/p` if it exists, otherwise it stays `/p`, like `qemu-x86_64 -L`. Relative paths do not change. Since 2026-10-14, `/proc`, `/sys`, `/dev`, `/run` and `/tmp` (whole path components: `/procedures` is not affected) always come from the host, even when the root has such a directory: Debian packages unpacked into a root bring an empty `/proc`, and Chromium's sandbox (Spotify) then missed `/proc/self/task` ✅ (unit test `kernel_and_runtime_directories_stay_on_the_host`; Spotify, by hand).

The "Since" column gives the engine milestone (M1–M5) in which the syscall arrived, or the date for those that arrived later.

| Syscall (x86_64 nr) | Since | Implementation |
|---|---|---|
| `write` (1) | M1 | in blocks of 64 KiB, at most `0x7ffff000` bytes (`MAX_RW_COUNT`); test `basic/write_errors` |
| `writev` (20) | M3 | since 2026-10-13. `struct iovec` the same on the two architectures (pointer and length). Errors in Linux's order ✅ (native C program on the host kernel, 2026-10-13): first the descriptor (EBADF, even if open read-only), then more than 1024 elements or a negative length (EINVAL), then the unreadable vector or a buffer beyond the x86_64 user space (EFAULT); total limited to `MAX_RW_COUNT`. The segments are gathered in order into a host buffer and leave with a single `write` every 64 KiB, so a pipe receives a short `writev` in a single write. An unreadable buffer stops the gathering at the first bad byte: what came before is written, as Linux does for a regular file, or EFAULT if there was nothing. **Known difference:** on a pipe Linux writes nothing and returns EFAULT ✅ (same program); armor writes the prefix. On `/dev/null` Linux does not read the buffers and returns the total; armor gives EFAULT. glibc uses it for fatal messages (`free(): double free detected…`). Tests `writev_follows_linux`, `c/writev` |
| `fstat` (5), `newfstatat` (262) | M2 | host `fstat`/`fstatat`, then conversion of `struct stat` from the aarch64 layout to the x86_64 one (below); `newfstatat`: root |
| `mmap` (9) | M2, files since M5 | private, anonymous or file-backed. Without `MAP_FIXED` the address is a hint that armor ignores (`map_anywhere`). With `MAP_FIXED` it uses `map_fixed`. **Files:** anonymous memory filled with `pread` (ADR-0012, `memory.md`); offset not a multiple of 4K → `-EINVAL`; invalid fd → `-EBADF`; fd open write-only → `-EACCES`. `MAP_NORESERVE`/`MAP_POPULATE`/`MAP_STACK`/`MAP_DENYWRITE`/`MAP_EXECUTABLE` are accepted without effect. `MAP_SHARED` (and `MAP_SHARED_VALIDATE`), anonymous or file-backed, since 2026-10-13: real shared mappings of the host (ADR-0022, `memory.md`). `PROT_WRITE` on a file open read-only → EACCES; shared `MAP_FIXED` not compatible with 16K pages and shared `PROT_EXEC` → "unsupported" with the reason. `MAP_FIXED_NOREPLACE` → EEXIST if any page of the range is mapped (before, it checked only the first). `MAP_GROWSDOWN` → "unsupported" (exit 125). |
| `msync` (26) | M3 | address not a multiple of 4K, unknown bits, `MS_SYNC` with `MS_ASYNC` → EINVAL (before the range, like Linux); range not entirely mapped → ENOMEM; then host `msync` on the host pages that cover it |
| `mprotect` on shared mappings | M3 | `PROT_WRITE` on a file open read-only → EACCES; `PROT_EXEC` → unsupported (ADR-0022) |
| `mremap` on shared mappings | M3, growth since 2026-10-14 | until 2026-10-14 unsupported (a copy would break the sharing). Shrinking unmaps the tail, as for private mappings. Growing (Spotify) is the host's own `mremap` of the same memory (`AddressSpace::remap_shared`, ADR-0022, `tech/memory.md`): in place if the pages after it are free, else, with `MREMAP_MAYMOVE`, moved below armor's ceiling; without `MREMAP_MAYMOVE` and no room → ENOMEM. The range must start and end on host page boundaries, otherwise → unsupported with the reason. ✅ test `mremap_shared` (file and memfd, a `fork` child sees the grown mapping, ENOMEM without `MREMAP_MAYMOVE`; x86-64 and i386, identical to qemu and to the Xeon, 2026-10-14, on the 4K kernel) and unit test `shared_file_mappings_grow`. ⚠️ On a 16K host only ranges on 16K boundaries can grow: from the code, not measured |
| `pwrite64` (18), `ftruncate` (77), `truncate` (76, root) | M3 | passed to the host (`pwrite64` in blocks like `write`) |
| `pread64` (17) | M5 | like `read`, at an offset |
| `access` (21), `faccessat` (269), `faccessat2` (439) | M5 | all with the host's `faccessat2` (AArch64 has no `access`); `AT_*` flags and modes identical; root |
| `mprotect` (10) | M2 | 4K permission table; unmapped page → `-ENOMEM` |
| `munmap` (11) | M2 | removes the guest pages; host pages left empty go back to the host |
| `brk` (12) | M2 | start = 4K-aligned end of the segments, without randomisation (like qemu); grows with `map_fixed`, shrinks with `unmap`. If it cannot, it returns the current brk, like the kernel. |
| `ioctl` (16) | M2 | `TCGETS` and `TIOCGWINSZ` (numbers and layout identical on the two architectures); since 2026-10-14 also `FIONBIO` (an `int` read from the guest) and `FIONREAD` (an `int` written), 0x5421 and 0x541b on x86_64, i386 and AArch64 ✅ (measured with the headers of the three); since 2026-10-14 (Mesa and SDL of a Steam game) also:<br>• `DRM_IOCTL_VERSION` (0xc0406400): Mesa asks the GPU node which driver to load. `struct drm_version` of 64 bytes (three `int`s, then three length-and-pointer pairs for the driver's name, date and description) the same on the two architectures ✅ (uapi `drm.h` of both, 2026-10-14); the strings go through host buffers of the length given by the guest (at most 4096 bytes each), and the full lengths written by the kernel go back to the guest, as in Linux. Test `c/drm_version` (against qemu on the same machine; it depends on the GPU, so not against the hardware oracle, which has none: `testing.md`);<br>• requests with a structure without pointers, the same on the two architectures ✅ (uapi headers of both, 2026-10-14), copied into a host buffer (`PLAIN_IOCTLS`): `UDMABUF_CREATE` (0x40187542, 24 bytes), `DMA_BUF_IOCTL_SYNC` (0x40086200, 8 bytes), `DMA_BUF_IOCTL_EXPORT_SYNC_FILE` (0xc0086202, 8 bytes, written back to the guest), `DMA_BUF_IOCTL_IMPORT_SYNC_FILE` (0x40086203, 8 bytes). ⚠️ No test exercises them: to verify with a program that creates a dma-buf from a memfd (`/dev/udmabuf`) under armor and under qemu;<br>• an unknown request on a descriptor that is not open → EBADF, like Linux, which checks the descriptor before the request ✅ (native program on the AArch64 host and on the Xeon, 2026-10-14; qemu answers ENOTTY: `testing.md`, discrepancies). Mesa makes such a call while looking for the GPU. Test `c/bad_ioctl`, only with `--oracle`;<br>• since 2026-10-14 (Chromium's network and cache code, in Spotify), also in `PLAIN_IOCTLS`: `SIOCGIFNAME` (0x8910, a 40-byte `struct ifreq`, the index at offset 16, written back), `SIOCGIWNAME` (0x8b01, a 32-byte `struct iwreq`, written back), `F2FS_IOC_GET_FEATURES` (0x8004f50c, a `u32`); numbers and sizes the same on x86_64 and AArch64 ✅ (`sizeof`/`offsetof` program compiled for both, x86_64 run under qemu, 2026-10-14). `SIOCETHTOOL` (0x8946, whose `ifreq` points to a command-specific structure) and the wireless-extension requests 0x8b00–0x8bff other than 0x8b01 (their `iwreq` carries pointers) → EOPNOTSUPP, as a driver without them; Chromium copes with the refusal (Spotify, by hand). ⚠️ No test exercises these five: to verify with a C program that calls them on a socket under armor, qemu and the Xeon.<br>• since 2026-10-14 (the AppImage runtime and shell scripts in AppImages), also in `PLAIN_IOCTLS`: `TIOCGPGRP` (0x540f, a `pid_t` written) and `TIOCSPGRP` (0x5410, a `pid_t` read), the same numbers on x86_64, i386 and AArch64 (`asm-generic/ioctls.h`); ✅ test `c/file_locks` (ENOTTY on a pipe), identical to qemu in `c`, `c32` and `c-dyn` and to the Xeon (2026-10-14).<br>The other requests → "unsupported". Test `c/file_locks` |
| `exit` (60) | M1, threads since M3 | terminates the thread: `CLONE_CHILD_CLEARTID` cleared and woken; the last thread terminates the process (`architecture.md`) |
| `exit_group` (231) | M1 | terminates the process |
| `clone` (56) | M3 | only POSIX threads: `CLONE_VM\|FS\|FILES\|SIGHAND\|THREAD` plus `SYSVSEM`, `SETTLS`, `PARENT_SETTID`, `CHILD_SETTID`, `CHILD_CLEARTID` (values the same on the two architectures). Other flags (fork) → "unsupported". Arguments in x86_64 order: flags, stack, parent_tid, child_tid, tls |
| `clone` without `CLONE_THREAD` (56), `fork` (57), `vfork` (58) | M3 | host `fork()` (ADR-0019): exit signal only `SIGCHLD`; `CLONE_CHILD_SETTID`, `CHILD_CLEARTID`, `PARENT_SETTID`, `SETTLS`; `CLONE_VM\|CLONE_VFORK` (`vfork`, `posix_spawn`) = fork with copy and parent stopped until `execve`/exit of the child. **From a guest with several threads** (since 2026-10-14; python, the Ren'Py engine of a Steam game): as in Linux the child has only the calling thread. Before the `fork` that thread takes the armor locks that the others might hold: the page tables in exclusive mode (which also stops the other threads at the end of their instruction block), `brk`, the signal actions; parent and child release them afterwards. The GIL (lockstep, `ARMOR_GIL=1`) goes through `ForkHooks`, registered by the front end: taken first, then the turn counter kept across the `fork`; the child skips the tickets of the threads it does not have. Test `fork_threads` (6 variants, x86-64 and i386: `fork` from the main thread and from another one while other threads allocate and take a mutex; in the child `malloc`, the mutex, a new thread, `popen`), ✅ identical to qemu with the interpreter, with the JIT, in lockstep, with `ARMOR_GIL=1` and to the Xeon (2026-10-14). The test protects its mutex with `pthread_atfork`, as a correct program must do: the first version took, in the child, a mutex that a vanished thread could hold, and hung on Linux too |
| `execve` (59) | M3 | x86-64 ELF and scripts with an x86-64 interpreter → armor relaunched (`--root`, `--argv0`); the rest → host `execve`. Arguments and environment read from the guest (at most 64 Ki strings of 128 KiB) |
| `wait4` (61) | M3 | passed to the host; status (`int`) and `struct rusage` (144 bytes) the same on the two architectures |
| `getppid` (110), `setpgid` (109), `getpgrp` (111), `setsid` (112), `getpgid` (121), `getsid` (124), `getpriority` (140), `setpriority` (141) | M3 | integers only, passed to the host (`getpgrp` = `getpgid(0)`, absent on AArch64) |
| `timer_create` (222), `timer_settime` (223), `timer_gettime` (224), `timer_getoverrun` (225), `timer_delete` (226) | M3 | `struct sigevent` (64 bytes) and `struct itimerspec` (32 bytes) the same; a `SIGEV_THREAD_ID` names a host tid, which is the guest's. Measured: coreutils `timeout` |
| `poll` (7) | M5 | absent on AArch64: host `ppoll` (milliseconds → `timespec`, negative = no limit); `struct pollfd` (8 bytes) and the `POLL*` bits the same, on i386 too. Measured: `xargs`; test `c/events` |
| `flock` (73) | 2026-10-14 | since 2026-10-14 (the Steam client locks its files): passed to the host, `LOCK_*` the same ✅; restarted after a handler with `SA_RESTART` as in Linux ⚠️ (remembered from the kernel source). Test `c/file_locks` (✅ Xeon) |
| `semget` (64), `semop` (65), `semctl` (66), `semtimedop` (220) | 2026-10-14 | since 2026-10-14 (`armor_linux::ipc`): host System V semaphores, shared with the other processes. `struct sembuf` (6 bytes) and constants the same on x86_64, i386 and AArch64 ✅. `semctl`: `IPC_RMID`, `GETPID`, `GETVAL`, `GETNCNT`, `GETZCNT`, `SETVAL`, `GETALL`, `SETALL`; the commands with a structure (`IPC_STAT`, `IPC_SET`, `IPC_INFO`, `SEM_INFO`, `SEM_STAT`) → unsupported. Test `c/sysv_semaphores` (✅ Xeon) |
| `shmget` (29), `shmat` (30), `shmctl` (31), `shmdt` (67) | 2026-10-14 | since 2026-10-14 (`armor_linux::ipc`; X11's MIT-SHM extension passes images to the X server this way): host System V shared memory. `shmat` reads the size with the host's `IPC_STAT` and has the host attach the whole segment (`AddressSpace::attach_sysv`), so it is the same memory that the other processes (the X server) see; with a given address, `SHM_RND` rounds it to `SHMLBA` (4096), without `SHM_RND` an address that is not a multiple is EINVAL, and without `SHM_REMAP` an address already mapped is EINVAL, as in Linux ✅ (`sysv_shm`, qemu and Xeon); `SHM_RDONLY` and `SHM_EXEC` pass to the host; a writable attachment puts the guest in concurrent mode (ADR-0023). `shmdt` on an address where no attachment starts → EINVAL; a `fork` child inherits the attachments. `shmctl`: `IPC_RMID`, `SHM_LOCK`, `SHM_UNLOCK` (integers); `IPC_STAT`, `IPC_SET`, `SHM_STAT`, `SHM_STAT_ANY` with the 112-byte `struct shmid_ds`, the same on x86_64 and AArch64 ✅ (`sizeof`/`offsetof`, 2026-10-14), copied as it is; `IPC_INFO` and `SHM_INFO` → unsupported. **Known limit** (from the code): with a given address, the host attaches only at a multiple of its page, so on a 16K host an address aligned to 4K but not to 16K is EINVAL, where x86 Linux accepts it. x86_64 only: the i386 `shmid_ds` is 84 bytes, not converted yet. Test `sysv_shm` (`c/`, `c-pie/`, `c-dyn/`: `shmat` where the kernel chooses, writes seen by a child that attaches the same segment and vice versa, `IPC_STAT`, `SHM_RND`, `SHM_REMAP`, `SHM_RDONLY`, `shmdt`, `IPC_RMID`), ✅ identical to qemu and to the Xeon (2026-10-14) |
| `memfd_create` (319) | 2026-10-14 | since 2026-10-14 (the SDL of a Steam game): the name read from the guest, the flags `MFD_CLOEXEC`, `MFD_ALLOW_SEALING`, `MFD_HUGETLB`, `MFD_NOEXEC_SEAL`, `MFD_EXEC` the same on x86_64, i386 and AArch64 ✅ (measured, 2026-10-14), passed to the host. Mapping it is a file `MAP_SHARED` (ADR-0022); the seals go through `fcntl` (above). Test `memfd` (6 variants: write, size, shared mapping in both directions, seals that then forbid writing and growth, errors), ✅ identical to qemu and to the Xeon (2026-10-14) |
| `clock_getres` (229) | 2026-10-14 | since 2026-10-14 (python of a Steam game): the host's, 16-byte `struct timespec` and clock ids the same (below, "Structs confirmed identical"); a null pointer only checks the id |
| `sched_setparam` (142), `sched_getparam` (143), `sched_setscheduler` (144), `sched_getscheduler` (145), `sched_get_priority_max` (146), `sched_get_priority_min` (147), `sched_rr_get_interval` (148) | 2026-10-14 | since 2026-10-14 (python of a Steam game): `struct sched_param` is a single `int` (✅ x86_64 and AArch64, "Structs confirmed identical", below; on i386 the same header) and the `SCHED_*` policies have the same numbers ✅ (`linux/sched.h` identical in the two sysroots, 2026-10-14); passed to the host with copies of the structure and of the `timespec` |
| `getresuid` (118), `getresgid` (120) | 2026-10-14 | since 2026-10-14: three 32-bit ids from the host, each written to its pointer |
| `sched_setaffinity` (203) | 2026-10-14 | since 2026-10-14: the CPU mask (bytes, the same on the two architectures) copied from the guest, at most 1024 bytes as for `sched_getaffinity` |
| `fallocate` (285) | 2026-10-14 | since 2026-10-14: integers, passed to the host. The values of `FALLOC_FL_*` are the same ✅ (`linux/falloc.h` of the two sysroots, 2026-10-14; the host's, more recent, adds `FALLOC_FL_WRITE_ZEROES` 0x80) |
| `pselect6` (270), `ppoll` (271) | 2026-10-14 | since 2026-10-14. Without a signal mask: host `pselect6`/`ppoll`, `timespec` checked by the host as by Linux (negative or nanoseconds ≥ 10⁹ → EINVAL) and remaining time written back. `pselect6` receives a pointer to the (mask, size) pair, which glibc passes even without a mask: with a non-null mask → unsupported (the guest's mask belongs to armor). Test `c/events` (✅ Xeon). **`ppoll` with a signal mask** since 2026-10-14 (Spotify), x86_64 and i386 (`ppoll`, `ppoll_time64`): as for `rt_sigsuspend`, the guest's mask becomes the given one for the wait (`Signals::begin_masked_wait`, `tech/signals.md`) and the host's `ppoll` gets the matching host mask, so switching masks and waiting is one step; a signal already deliverable under the new mask → EINTR at once, as on Linux; the mask is passed to the host as 8 bytes. ✅ test `ppoll_mask`, run in `c/`, `c32/` and `c-dyn/`: identical to qemu with the interpreter and the JIT and to the Xeon (2026-10-14); ⚠️ the other variants (`c-pie/`, `c32-pie/`, `c32-dyn/`) are compiled and await the full chain |
| `pkey_mprotect` (329), `pkey_alloc` (330), `pkey_free` (331) | 2026-10-14 | memory protection keys. armor's CPU model has no PKU, and Linux x86 then answers EINVAL to `pkey_alloc` and `pkey_free`; `pkey_mprotect` with key −1 is `mprotect`, any other key EINVAL ✅ (Xeon X5675, which has no PKU, 2026-10-14). qemu-x86_64 answers ENOSYS (`testing.md`, discrepancies): test `pkeys` (`c/`, `c-pie/`), only with `--oracle` |
| `fsync` (74), `fdatasync` (75), `syncfs` (306), `sync` (162), `sync_file_range` (277), `mlock` (149), `munlock` (150), `mlockall` (151), `munlockall` (152), `membarrier` (324), `ioprio_set` (251), `ioprio_get` (252) | 2026-10-14 | integers only, passed to the host. The ranges of `mlock`/`munlock` are guest addresses, which are host addresses (identity mapping): the kernel only walks its page tables. ✅ test `sync_misc`, identical to qemu and to the Xeon (2026-10-14) |
| `getrusage` (98), `times` (100), `getcpu` (309) | 2026-10-14 | `struct rusage` (144 bytes) and `struct tms` the same on the two architectures (below, "Structs confirmed identical"); `getcpu` writes the CPU and the node each to its pointer if set. ✅ test `sync_misc` |
| `mincore` (27) | 2026-10-14 | one byte per **guest** page: whether the host page holding it is resident; ENOMEM if a guest page in the range is not mapped; address not a multiple of 4K → EINVAL. ✅ test `sync_misc` |
| `capget` (125), `capset` (126) | 2026-10-14 | an 8-byte header (version, pid) and one 12-byte record (version 1) or two (versions 2 and 3), the same on x86_64 and AArch64 ✅ (`sizeof` program compiled for both, x86_64 run under qemu, 2026-10-14); the kernel writes its preferred version back into the header when it does not know the guest's. ✅ test `capabilities`, identical to qemu and to the Xeon |
| `timerfd_create` (283), `timerfd_settime` (286), `timerfd_gettime` (287) | 2026-10-14 | `struct itimerspec` (32 bytes) the same (below); flags passed to the host; the expirations are read with `read`. ✅ test `timerfd`, identical to qemu and to the Xeon |
| `sched_yield` (24), `nanosleep` (35), `getrlimit` (97), `setrlimit` (160), `getitimer` (36), `waitid` (247), `readv` (19), `preadv` (295), `pwritev` (296), `preadv2` (327), `pwritev2` (328) | 2026-10-14 | `nanosleep` is `clock_nanosleep` on `CLOCK_MONOTONIC`, as in Linux ⚠️ (remembered from the kernel source); `getrlimit`/`setrlimit` go through `prlimit64` on the calling process; `getitimer` with the 32-byte `itimerval`; `waitid` with the 128-byte `siginfo_t` and, if asked, the 144-byte `rusage`. The vectored calls check the guest's 16-byte `iovec`s as Linux does (more than `IOV_MAX` → EINVAL, a negative length → EINVAL, a buffer beyond user space → EFAULT), move the data through one host buffer (at most 1 MiB per call) and pass the offset and the `RWF_*` flags of `preadv2`/`pwritev2` (offset −1: the current position); a read stops at the first guest byte it cannot write and returns what it copied before (EFAULT if nothing). ✅ test `basic_calls`, identical to qemu and to the Xeon |
| `rt_sigtimedwait` (128), `rt_sigqueueinfo` (129), `rt_tgsigqueueinfo` (297) | 2026-10-14 | `rt_sigtimedwait` (`Signals::timed_wait`, `tech/signals.md`): 8-byte set (`sigsetsize` ≠ 8 → EINVAL), optional 16-byte `timespec`, the `siginfo_t` written if asked; EAGAIN at the timeout. `rt_sigqueueinfo`/`rt_tgsigqueueinfo`: the 128-byte `siginfo_t` copied as it is (same layout) to the host, with the host's pids and tids (they are the guest's). ✅ test `sigtimedwait`, identical to qemu and to the Xeon |
| `gettimeofday` (96) | M5 | `struct timeval` (16 bytes) and `struct timezone` (8 bytes) the same. Measured: `find` |
| `readlink` (89) | M5 | absent on AArch64: `readlinkat(AT_FDCWD, …)`, with the same handling of `/proc/self/exe`. Measured: perl |
| `chdir` (80, root), `fchdir` (81) | M5 | `chdir` resolves the path in the root like the other syscalls with paths (ADR-0013); `fchdir` passed to the host. Measured: `find` |
| `rt_sigsuspend` (130) | M3 | host `rt_sigsuspend` with the new mask, in a single step (no signal lost between the mask change and the wait); the handler's frame saves the previous mask (`tech/signals.md`) |
| `clone3` (435) | M3 | `-ENOSYS`: glibc falls back to `clone` ✅ (`c/threads` runs) |
| `umask` (95) | M5 | since 2026-10-13 (`make`, `tar`): forwarded to the host, which uses the mask for the files created by the guest; argument reduced to `0o777` |
| `unlink` (87) / `unlinkat` (263) | M5 | since 2026-10-13 (`make` deletes the temporary Makefile): host `unlinkat` with the path resolved in the guest root like the other syscalls; `AT_REMOVEDIR` has the same value on the two architectures |
| `landlock_create_ruleset` (444) / `landlock_add_rule` (445) / `landlock_restrict_self` (446) | M5 | since 2026-10-13: ENOSYS, like qemu-x86_64 (measured with `-strace` on `xz`, which then works without a sandbox) |
| `mremap` (25) | M5 | since 2026-10-13 (perl with a large hash, `realloc` of blocks from `mmap`). Shrinking: unmaps the tail. Growth: in place if the pages after it are free, otherwise with `MREMAP_MAYMOVE` new mapping, copy (also of unreadable pages) and unmapping of the old one. Like Linux, the old range must have a single protection (EFAULT). `MREMAP_FIXED`/`MREMAP_DONTUNMAP` → "unsupported". Shared mappings: their own row above (since 2026-10-14 they grow with the host's `mremap`). Verified errors: `testing.md` (qemu discrepancies). Tests: `c/mremap`, `interpreters/perl-big-hash` |
| `madvise` (28) | M3 | `MADV_DONTNEED` zeroes the range, like Linux for private anonymous memory (glibc frees thread stacks this way): the host pages entirely covered go to the host's `madvise(MADV_DONTNEED)`, which reclaims them and fills them with zeros on the next access; the partial pieces are zeroed (`AddressSpace::discard`, since 2026-10-13). With unmapped pages in the range, like Linux, it handles the mapped parts and returns ENOMEM. Known difference, also before: private file mappings (copies, ADR-0012) go back to zero instead of to the file's content. The other advice values → 0 |
| `arch_prctl` (158) | M2 | `ARCH_SET_FS/GS` set the guest's `fs_base`/`gs_base`; `ARCH_GET_FS/GS` write them to memory; other → `-EINVAL` |
| `set_tid_address` (218) | M2 | stores the address (`clear_child_tid`, cleared and woken when the thread exits, ADR-0018) and returns the host tid |
| `set_robust_list` (273), `rseq` (334) | M2 | `-ENOSYS`, **like qemu**; glibc works without them |
| `read` (0) | M2 | into a host buffer, then copied to the guest. The buffer is as large as the guest asks, up to 64 MiB per call (`READ_BUFFER`; until 2026-10-14 at most 64 KiB per call: a regular file then gave a short read, which Linux never does for one that is long enough, and the runtime of an AppImage, which reads its squashfs blocks with a single `read`, stopped unpacking after 11 files). ✅ KeePassXC's AppImage extracted under armor: the 284 files identical by SHA-256 to an extraction on the Xeon (2026-10-14). `readv`, `preadv`, `preadv2` share the same limit |
| `open` (2), `openat` (257) | M2 | path read from the guest; **`O_*` flags translated** (table below); `open` = `openat(AT_FDCWD, …)`; root. Since 2026-10-14 `/proc/self/exe` and `/proc/<own pid>/exe` open the **guest program**, as `readlink` reports it, not armor (the AppImage runtime opens itself this way to find its squashfs); on i386 too. ✅ KeePassXC's AppImage unpacks under armor (2026-10-14); ⚠️ no test in `tests/guest/` yet: to add a C program that opens `/proc/self/exe` and checks its ELF header and size against `stat` of `argv[0]`, under armor, qemu and the Xeon |
| `close` (3), `lseek` (8), `dup` (32), `dup3` (292) | M2 | passed to the host (integers only; `O_CLOEXEC` of `dup3` has the same value) |
| `dup2` (33) | M2 | absent on AArch64: `dup3(old, new, 0)`, but `dup2(fd, fd)` returns `fd` after checking that it is open |
| `fcntl` (72) | M2 | `F_DUPFD`, `F_DUPFD_CLOEXEC`, `F_GETFD`, `F_SETFD` passed to the host; `F_GETFL`/`F_SETFL` with translation of the `O_*` flags. Since 2026-10-14 (python of a Steam game): the record locks `F_GETLK`/`F_SETLK`/`F_SETLKW` (5–7) and the ones tied to the open file description `F_OFD_GETLK`/`F_OFD_SETLK`/`F_OFD_SETLKW` (36–38), with the 32-byte `struct flock`, the same on x86_64 and AArch64 ✅ (`sizeof`/`offsetof`, 2026-10-14), copied into a host buffer and written back to the guest after `F_GETLK`; the seals of a memfd `F_ADD_SEALS`/`F_GET_SEALS` (1033, 1034, integers). `fcntl` is restarted after a handler with `SA_RESTART`, like `flock` ⚠️ (rule remembered from the kernel source; no test interrupts an `F_SETLKW`). Other commands (leases, `F_SETOWN`, …) → "unsupported". Test `c/file_locks` (✅ qemu and Xeon) |
| `getdents64` (217) | M2 | `struct linux_dirent64` identical on the two architectures: records copied as they are |
| `sendfile` (40) | M2 | optional offset copied in and out |
| `uname` (63) | M2 | the host's, but with `machine` = `x86_64`, like an x86_64 kernel (and like qemu) |
| `getcwd` (79) | M2 | raw syscall: returns the length including the NUL |
| `time` (201) | M2 | absent on AArch64: host `time()` |
| `getuid`/`getgid`/`geteuid`/`getegid` (102, 104, 107, 108), `setuid`/`setgid` (105, 106) | M2 | raw host syscalls. As on the x86 kernel, `setuid`/`setgid` apply to the calling thread: it is the guest's glibc that repeats them on every thread (1:1 threads, ADR-0018) ⚠️ (reasoning, no test with several threads) |
| `getgroups` (115) | M2 | `gid_t` is u32 on both |
| `prctl` (157) | M2 | `PR_SET_NAME`/`PR_GET_NAME`; `PR_CAPBSET_READ` (capability numbers the same on the two architectures); since 2026-10-13 `PR_SET_NO_NEW_PRIVS`/`PR_GET_NO_NEW_PRIVS` (`xz`), forwarded to the host: they only limit what `execve` grants, and armor's is the host's. Since 2026-10-14 (Spotify) `PR_SET_VMA` (0x53564d41, names for anonymous memory) → EINVAL, as a kernel built without `CONFIG_ANON_VMA_NAME`: the names only appear in `/proc/<pid>/maps`, which armor does not show ⚠️ (no test: to verify with a C program under armor, qemu and the Xeon). Other options → "unsupported". At start-up armor gives its own process the name of the guest program, as `exec` would |
| `readlinkat` (267) | M2 | `/proc/self/exe` and `/proc/<pid>/exe` → absolute path of the **guest program** (like qemu), not of armor; other paths → host, resolved in the root |
| `prlimit64` (302) | M2 | passed to the host with local buffers (`struct rlimit64`: two u64, `RLIMIT_*` identical) |
| `getrandom` (318) | M2 | from the host, at most 64 KiB per call |
| `rt_sigaction` (13) | M3 | records the guest's action (x86_64 `struct sigaction`: handler, flags, restorer, 8-byte mask) and returns the previous one; `sigsetsize` ≠ 8, signal outside 1–64 or `SIGKILL`/`SIGSTOP` → `-EINVAL`. In the host: an armor handler for the guest's handlers, `SIG_DFL`/`SIG_IGN` the same for the others (`tech/signals.md`). At start-up armor resets SIGPIPE to the default (`reset_signal_dispositions`): the Rust runtime ignores it, while a freshly created process does not ✅ (`*-closed-stdout` cases of `difftest`) |
| `rt_sigprocmask` (14), `rt_sigpending` (127) | M3 | the guest's mask, mirrored in the host; `how` and 8-byte sets the same; pending = pending in the host plus those recorded by armor |
| `rt_sigreturn` (15) | M3 | restores registers, FPU state and mask from the frame (`tech/signals.md`); invalid frame → SIGSEGV |
| `sigaltstack` (131) | M3 | state in armor; x86_64 `stack_t` (sp, flags + 4 bytes of padding, size); `-EPERM` if in use, `-ENOMEM` below `MINSIGSTKSZ` (2048) |
| `getpid` (39), `gettid` (186) | M3 | the host's (the guest is armor's process) |
| `kill` (62), `tgkill` (234), `tkill` (200) | M3 | passed to the host; towards its own process the signals that armor reserves (SIGSEGV & co.) are recorded by armor |
| `setitimer` (38) | M3 | `struct itimerval` (32 bytes) the same; the SIGALRM comes from the host |
| `pause` (34) | M3 | AArch64 does not have it: libc's `pause()` |
| `pipe` (22), `pipe2` (293) | M3 | flags translated as for `open` |
| `sysinfo` (99), `statfs` (137, root), `fstatfs` (138), `statx` (332, root), `clock_gettime` (228) | M5 | structs identical on the two architectures (above): bytes copied as they are. `clock_gettime` arrives as a syscall because armor has no vDSO |
| `socket` (41), `connect` (42) | M5 | `socket` passed to the host (domains, types, `SOCK_NONBLOCK`/`SOCK_CLOEXEC` the same); `connect` copies the address from the guest (at most 128 bytes, otherwise `-EINVAL`). Measured: `ls -l` tries the nscd/systemd-userdb sockets |
| `accept` (43), `sendto` (44), `recvfrom` (45), `sendmsg` (46), `recvmsg` (47), `shutdown` (48), `bind` (49), `listen` (50), `getsockname` (51), `getpeername` (52), `socketpair` (53), `setsockopt` (54), `getsockopt` (55), `accept4` (288) | 2026-10-14 | since 2026-10-14 (`armor_linux::net`). Constants and structures the same on the two architectures (below): addresses copied into host buffers (at most 128 bytes, otherwise EINVAL) and written back truncated to the guest's capacity with the full length, like the kernel; data in a host buffer, at most 1 MiB per call (a longer transfer becomes short); `msghdr` read from the guest, its `iovec`s (at most 1024, otherwise EMSGSIZE; a length negative as a guest `long` → EINVAL, unit test `messages_with_extreme_lengths_are_checked`) gathered into a single buffer and scattered again on receive; control messages up to 4096 bytes, `SCM_RIGHTS` included. Tests `c/sockets` (socket pair with a passed descriptor, TCP and UDP on loopback, options, timeouts) and `c/scm_rights_truncated`, both ✅ Xeon |
| `llistxattr` (195, root) | M5 | host buffer, at most 64 KiB |
| `futex` (202) | M3 | **passed to the host with the guest address** (ADR-0018: the only exception to the pointer rule, the memory is armor's and mapped identically). First armor checks that the word is aligned to 4 (`-EINVAL`) and on a writable page (`-EFAULT`), without writing it. Timeout copied into a host buffer. `WAIT`, `WAKE`, `WAIT_BITSET`, `WAKE_BITSET`, `REQUEUE`, `CMP_REQUEUE`, `WAKE_OP`. Since 2026-10-14 (the audio library of a Steam game) also the priority-inheritance operations `LOCK_PI`, `LOCK_PI2`, `UNLOCK_PI`, `TRYLOCK_PI`, `WAIT_REQUEUE_PI`, `CMP_REQUEUE_PI`, passed to the host in the same way: the word contains the owner's TID, and the TID of a guest thread is that of its host thread (ADR-0018). Test `pi_mutex` (6 variants: `PTHREAD_PRIO_INHERIT` mutex contended by four threads, `trylock`, a condition variable on that mutex), ✅ identical to qemu, in lockstep and to the Xeon (2026-10-14). The robust-list operations are not needed (`set_robust_list` is ENOSYS). **Open issue (2026-10-14):** with Qt's OpenGL integration (KeePassXC, XnView MP), the main thread waits forever in `FUTEX_WAIT` on a non-private futex in anonymous memory next to Mesa's `libgallium` (a lock like Mesa's `simple_mtx`) that no thread wakes; deterministic, also with `ARMOR_GIL=1` and with `LP_NUM_THREADS=0`. ⚠️ Cause not known: to investigate with `ARMOR_TRACE_MAPS` and a trace of the futex word's writers, comparing with `qemu-x86_64`. Until then `armor install` writes `QT_XCB_GL_INTEGRATION=none` (with the reason) into the `app.conf` of Linux applications, and Qt draws in software |
| `sched_getaffinity` (204) | M5 | the host's mask |
| `clock_nanosleep` (230) | M4 | `struct timespec`, clock ids and `TIMER_ABSTIME` the same; if interrupted, the remaining time goes back to the guest. Measured: busybox `sleep` |
| `fadvise64` (221) | M5 | passed to the host (`POSIX_FADV_*` the same) |
| `stat` (4), `lstat` (6) | M3 | absent on AArch64: `newfstatat(AT_FDCWD, …, 0 / AT_SYMLINK_NOFOLLOW)` and the x86_64 `struct stat`; root |
| `mkdir` (83), `mkdirat` (258), `mknod` (133), `mknodat` (259), `rmdir` (84), `creat` (85) | M3 | `mkdirat`, `mknodat`, `unlinkat(AT_REMOVEDIR)`, `openat(O_CREAT\|O_WRONLY\|O_TRUNC)` with `AT_FDCWD` for the forms absent on AArch64; root |
| `rename` (82), `renameat` (264), `renameat2` (316), `link` (86), `linkat` (265), `symlink` (88), `symlinkat` (266) | M3 | host `renameat2`, `linkat`, `symlinkat`; both paths in the root, except the target of a symbolic link (it is its content) |
| `chmod` (90), `fchmod` (91), `fchmodat` (268), `fchmodat2` (452), `chown` (92), `fchown` (93), `lchown` (94), `fchownat` (260) | M3 | host `fchmodat` (or `fchmodat2` if there are flags, since Linux 6.6) and `fchownat`, `AT_SYMLINK_NOFOLLOW` for `lchown`; root |
| `utime` (132), `utimes` (235), `futimesat` (261), `utimensat` (280) | M3 | host `utimensat`: `struct utimbuf` and `struct timeval[2]` converted to `timespec[2]`; microseconds outside 0..10^6 → EINVAL like Linux; null path allowed (`utimensat` on the descriptor) |
| `getdents` (78) | M3 | absent on AArch64: host `getdents64` converted to the old format (`d_type` in the last byte of the record). The two formats round a record with an n-byte name to `n + 20` aligned to 8 (unit test), so positions and sizes stay the same |
| `select` (23) | M3 | absent on AArch64: host `pselect6` without a mask; `fd_set` sets the same; timeout normalised as in Linux and remaining time written back into the `timeval` |
| `epoll_create` (213), `epoll_create1` (291), `epoll_ctl` (233), `epoll_wait` (232), `epoll_pwait` (281), `epoll_pwait2` (441) | M3 | `struct epoll_event` converted (12 bytes packed on x86_64, 16 on AArch64); `size` ≤ 0 → EINVAL; `maxevents` between 1 and `INT_MAX / 12`; at most 1024 events per call. With a signal mask → unsupported (the guest's mask belongs to armor, not to the host) |
| `eventfd` (284), `eventfd2` (290), `inotify_init` (253), `inotify_init1` (294), `inotify_add_watch` (254, root), `inotify_rm_watch` (255) | M3 | the host's `2`/`1` forms; flags the same as the `O_*` ones; events read with `read` (layouts the same) |
| `alarm` (37) | M3 | absent on AArch64: glibc's `alarm()` (`setitimer`), which rounds the remainder like Linux |
| `modify_ldt` (154) | M3 | only reading (function 0): 0 bytes, the LDT is empty; writing an LDT → unsupported |
| obsolete or x86-only: `uselib` (134), `ustat` (136), `sysfs` (139), `_sysctl` (156), `iopl` (172), `ioperm` (173), `create_module` (174), `get_kernel_syms` (177), `query_module` (178), `getpmsg` (181), `putpmsg` (182), `afs_syscall` (183), `tuxcall` (184), `security` (185), `set_thread_area` (205), `get_thread_area` (211), `epoll_ctl_old` (214), `epoll_wait_old` (215), `vserver` (236), `uretprobe` (335) | M3 | `-ENOSYS`, like qemu-x86_64 (✅ measured, `c/legacy_syscalls` for some). Linux never implemented them or removed them; `iopl`/`ioperm` would give EPERM to a non-root process ⚠️ (remembered), but armor never grants port I/O; `set_thread_area` would be needed only with segment registers |
| `signalfd` (282), `signalfd4` (289) | — | **unsupported**: the guest's signals and its mask are handled by armor (ADR-0017), so a host `signalfd` would not see the same signals |
| x32 (bit `0x40000000`) | M1 | `-ENOSYS`, like a kernel without x32 |
| any other | — | armor prints `… is not supported yet` and exits with 125 (`conventions.md`) |

On 2026-10-14 two programs drove the additions. First Doki Doki Literature Club (`tech/steam.md`): the 80 different syscalls it makes under `qemu-x86_64 -strace`, compared with armor's table, then the requests found by running it under armor. `clock_getres` and the `sched_*` ones have the `sched` test (`c/` and `c32/`, in the six variants), identical to qemu and to the Xeon in the full chain of 2026-10-14. ⚠️ `getresuid`/`getresgid`, `sched_setaffinity` and `fallocate` do not have a test yet: to verify with a C program that calls them under armor and under qemu (and the Xeon).

Then Spotify (installed with `armor install` from its `.deb`, `COMPATIBILITY_MATRIX.md`): the requests it stopped on under armor, plus the neighbouring calls of the same families, each with a C test in `tests/guest/c/` (`pkeys`, `ppoll_mask`, `sync_misc`, `capabilities`, `timerfd`, `basic_calls`, `sigtimedwait`, `mremap_shared`; `testing.md`). **x86_64 syscalls handled: 246 of 375** (208 before; counted by `cargo xtask coverage`, README badge).

### armor's own syscall: calls into the host's libraries (ADR-0041, since 2026-10-15)
Syscall **0x410000** (outside Linux's x86_64 numbers and below the x32 range) is armor's: `rdi` is a function id, `rsi` the address of sixteen 8-byte slots holding the arguments (integers and pointers as they are, a `double` by its bits); armor calls the host function (`crates/armor-linux/src/host.rs`: a table of ids, libraries, symbols and C signatures; `dlopen`/`dlsym` once) on the guest thread's own host thread and writes the result into slot 0, returning 0. An unknown id, or a function the host lacks, gives `ENOSYS`, as the number does under any kernel or emulator, so a guest stub falls back to its own code. ✅ Test `host_call` (`c`, `c-pie`, `c-dyn`): `ldexp` from the host's `libm` under armor (interpreter and JIT) and the program's own exact fallback under qemu give the same output, including `ldexp(1e-300, 600)`; an unknown id is `ENOSYS` in both (`ARMOR_TEST_SHOW_PATH=1` prints which path was taken: `host` under armor, `guest` under qemu). It is the mechanism of ADR-0041.

The OpenGL functions use the same syscall (`architecture.md`, "Calls into the host's libraries"): the stub of each function in armor's x86-64 `libGL.so.1` puts its arguments into the slots (a `float` or `double` by its bits) and armor calls the host function with the C signature of the generated table (`host/gl_table.rs`). The answer to the probe id `armorHostGL` tells the stubs whether armor provides the host's OpenGL (only with `--host-gl`); without it, every stub calls the guest's own libGL. ✅ `cargo xtask gl-check` (2026-10-15, Pi 5): `gl_rects` (clears and scissors) and `gl_shader` (a GLSL program, vertex attributes from the program's memory, a uniform, a texture with `glTexImage2D`) give the same pixels under qemu (guest Mesa, llvmpipe), armor (the same) and `armor --host-gl` (the host's Mesa, renderer "V3D 7.1.7.0"): checksums `1067c578809623dd` and `b32739add746d525`.

### Constants and structs compared on the two architectures ✅
Verified on 2026-10-12 with a program compiled both for aarch64 (run natively) and for x86_64 (run under qemu). They are **identical**:
- `mmap` flags (`MAP_SHARED` 0x1, `MAP_PRIVATE` 0x2, `MAP_FIXED` 0x10, `MAP_ANONYMOUS` 0x20, `MAP_GROWSDOWN` 0x100, `MAP_NORESERVE` 0x4000, `MAP_POPULATE` 0x8000, `MAP_STACK` 0x20000, `MAP_FIXED_NOREPLACE` 0x100000, `MAP_DENYWRITE` 0x800, `MAP_EXECUTABLE` 0x1000);
- `PROT_*` (1, 2, 4);
- `RLIMIT_STACK`/`NOFILE`/`AS` (3, 7, 9) and `sizeof(struct rlimit)` = 16;
- the kernel's `struct termios` (36 bytes, `c_line` @16, `c_cc` @17, `NCCS` 19), `TCGETS` 0x5401, `TIOCGWINSZ` 0x5413, `struct winsize` 8 bytes;
- `AT_FDCWD` −100, `AT_SYMLINK_NOFOLLOW` 0x100, `AT_EMPTY_PATH` 0x1000, `GRND_NONBLOCK` 1, `GRND_RANDOM` 2.
- (M5, same method, 2026-10-12) `struct statfs` 120 bytes and `struct sysinfo` 112 bytes, with all offsets the same; `struct statx` 256 bytes; signal numbers (`SIGBUS` 7, `SIGUSR1` 10, `SIGUSR2` 12, `SIGCHLD` 17, `SIGCONT` 18, `SIGSTOP` 19, `SIGTSTP` 20, `SIGURG` 23, `SIGIO` 29, `SIGSYS` 31); `SA_*` flags (`SA_NOCLDSTOP` 1, `SA_NOCLDWAIT` 2, `SA_SIGINFO` 4, `SA_ONSTACK` 0x8000000, `SA_RESTART` 0x10000000, `SA_NODEFER` 0x40000000).
- `MAP_DENYWRITE` 0x800 and `MAP_EXECUTABLE` 0x1000: the same `asm-generic/mman.h` included by both architectures (verified in the headers).

`arch_prctl` codes (x86_64 header `asm/prctl.h`) ✅: `ARCH_SET_GS` 0x1001, `ARCH_SET_FS` 0x1002, `ARCH_GET_FS` 0x1003, `ARCH_GET_GS` 0x1004.

Also identical ✅ (same method, 2026-10-12): `struct utsname` (390 bytes, `machine` @260), `struct linux_dirent64` (`d_reclen` @16, `d_type` @18, `d_name` @19), `PR_SET_NAME` 15, `PR_GET_NAME` 16, `SEEK_*`, `F_GETFD`/`F_SETFD`/`F_GETFL`/`F_SETFL`/`F_DUPFD_CLOEXEC`.

### Different `O_*` flags: translated ✅
Verified in the kernel headers: x86_64 uses `asm-generic/fcntl.h`, AArch64 its own `asm/fcntl.h`. Only these four differ (`O_TMPFILE` includes them through `O_DIRECTORY`):

| Flag | x86_64 | AArch64 |
|---|---|---|
| `O_DIRECT` | 0o40000 | 0o200000 |
| `O_LARGEFILE` | 0o100000 | 0o400000 |
| `O_DIRECTORY` | 0o200000 | 0o40000 |
| `O_NOFOLLOW` | 0o400000 | 0o100000 |

`open_flags_to_host` / `open_flags_to_guest` (`syscall.rs`) swap them on the way in (`openat`, `F_SETFL`) and on the way out (`F_GETFL`). Without this translation `ls` and `find` fail (verified with a mutation test).

### Robustness
No guest input must make armor panic or hang (`conventions.md`). The fuzz test `fuzz_syscall_arguments` (`testing.md`, since 2026-10-13) calls 36 syscalls with extreme arguments and found `brk` and `munmap`, which hung armor on huge ranges (`tech/memory.md`). The test `extreme_arguments_do_not_panic` calls every implemented syscall with all arguments at −1. On its first run it found an overflow in `brk`, and checking the rest of the code, other unchecked page roundings in `armor-mem` and in the loader. Now they are all checked computations that return a normal error.

## Syscall numbers (first milestones)
| Syscall | aarch64 ✅ | x86_64 ✅ |
|---|---|---|
| `write` | 64 | 1 |
| `exit` | 93 | 60 |
| `exit_group` | 94 | 231 |
| `openat` | 56 | 257 |
| `mmap` | 222 | 9 |
| `mprotect` | 226 | 10 |
| `munmap` | 215 | 11 |
| `brk` | 214 | 12 |
| `clone` | 220 | 56 |
| `newfstatat` | 79 | 262 |
| `arch_prctl` | **absent** ✅ | 158 |

The full x86_64 table is in `/usr/x86_64-linux-gnu/include/asm/unistd_64.h` (and in the Linux sources, `arch/x86/entry/syscalls/syscall_64.tbl`). In the code it is better to generate it from that header than to transcribe it by hand. The assembly test programs include it directly (`testing.md`).

## Legacy x86_64 syscalls absent on aarch64
**Absence on aarch64:** ✅ verified (no matching `__NR_*` macro in the host headers).
**Status (2026-10-13):** ✅ computed from the headers (x86_64 `unistd_64.h` against the host's `asm/unistd.h`). Of 375 x86_64 syscalls, 57 do not exist on AArch64; armor handles all of them (table above), except:
- `signalfd`, unsupported with a reason;
- `modify_ldt` for writing.

The behaviour is compared with qemu by `c/legacy_syscalls`. The table below is the one proposed at the start, kept for history.

| x86_64 (nr ✅) | Proposed translation on aarch64 | Caution |
|---|---|---|
| `open` (2) | `openat(AT_FDCWD, …)` | |
| `stat` (4) / `lstat` (6) | `newfstatat(AT_FDCWD, path, buf, 0 / AT_SYMLINK_NOFOLLOW)` | different `stat` struct (below) |
| `access` (21) | `faccessat(AT_FDCWD, …)` | implemented (with `faccessat2`) |
| `mkdir` (83) / `unlink` (87) / `readlink` (89) | `mkdirat` / `unlinkat` / `readlinkat` with `AT_FDCWD` | |
| `rename` (82) | `renameat` (✅ present on aarch64) or `renameat2` | |
| `fork` (57) | `clone(SIGCHLD, …)` | |
| `vfork` (58) | to be designed | The child shares memory with the emulator: risky. It probably has to be implemented as `fork`. |
| `pipe` (22) | `pipe2(fds, 0)` | |
| `dup2` (33) | `dup3` | `dup2(fd, fd)` returns `fd`, while `dup3(fd, fd)` returns EINVAL: a special case is needed |
| `poll` (7) | `ppoll` | timeout in ms → `timespec` |
| `select` (23) | `pselect6` | `timeval` → `timespec`. Linux updates the remaining timeout. |
| `epoll_create` (213) | `epoll_create1(0)` | `size <= 0` → EINVAL |
| `epoll_wait` (232) | `epoll_pwait(…, NULL)` | different `epoll_event` struct (below) |
| `getdents` (78) | `getdents64` + conversion to the old `linux_dirent` format | ⚠️ format to verify |
| `time` (201) | `clock_gettime(CLOCK_REALTIME)` | |
| `alarm` (37) | `setitimer(ITIMER_REAL)` | ⚠️ to verify |
| `arch_prctl` (158) | **handled internally**: `ARCH_SET_FS/GET_FS/SET_GS/GET_GS` act on the guest's `fs_base`/`gs_base` | No host syscall |

The list is not exhaustive: compare the full x86_64 table with `asm-generic/unistd.h`.

## Structs with different layouts
### `struct stat`
**aarch64** ✅, 128 bytes. Verified with `offsetof` by compiling two separate programs: one with `<sys/stat.h>` (glibc) and one with `<asm/stat.h>` (kernel, the one that matters for `newfstatat`). The layouts match. In the kernel the times are separate fields `st_atime` + `st_atime_nsec`, with the same offsets.

| Field | Offset | Size |
|---|---|---|
| `st_dev` | 0 | 8 |
| `st_ino` | 8 | 8 |
| `st_mode` | 16 | 4 |
| `st_nlink` | 20 | 4 |
| `st_uid` | 24 | 4 |
| `st_gid` | 28 | 4 |
| `st_rdev` | 32 | 8 |
| *(padding)* | 40 | 8 |
| `st_size` | 48 | 8 |
| `st_blksize` | 56 | 4 |
| *(padding)* | 60 | 4 |
| `st_blocks` | 64 | 8 |
| `st_atim` | 72 | 16 |
| `st_mtim` | 88 | 16 |
| `st_ctim` | 104 | 16 |
| *(reserved)* | 120 | 8 |

**x86_64** ✅, 144 bytes. Verified with `offsetof` on `<sys/stat.h>` (glibc) and on `<asm/stat.h>` (kernel), compiled for x86_64 and run under qemu. The two layouts match.

| Field | Offset | Size |
|---|---|---|
| `st_dev` | 0 | 8 |
| `st_ino` | 8 | 8 |
| `st_nlink` | 16 | **8** |
| `st_mode` | 24 | 4 |
| `st_uid` | 28 | 4 |
| `st_gid` | 32 | 4 |
| *(padding)* | 36 | 4 |
| `st_rdev` | 40 | 8 |
| `st_size` | 48 | 8 |
| `st_blksize` | 56 | **8** |
| `st_blocks` | 64 | 8 |
| `st_atim` | 72 | 16 |
| `st_mtim` | 88 | 16 |
| `st_ctim` | 104 | 16 |
| *(reserved)* | 120 | 24 |

Main differences: order and size of `st_nlink`/`st_mode`, size of `st_blksize` and total size.

### `struct epoll_event`
- aarch64: **16 bytes**, alignment 8 ✅.
- x86_64: **12 bytes**, packed (alignment 1, `data` at offset 4) ✅.

A conversion is therefore needed for `epoll_ctl` and `epoll_wait`.

### To analyse (checklist)
- [x] `sigaction` (on x86_64 `SA_RESTORER` / `sa_restorer` is needed): x86_64 `struct sigaction` read and written by armor (`rt_sigaction` above, ADR-0017)
- [x] `siginfo_t`: 128 bytes on both (below); armor builds it for the guest (`tech/signals.md`)
- [x] `ucontext` / `sigcontext`: completely different; armor builds the x86_64 frame (`tech/signals.md`, ADR-0017)
- [x] `statfs`: identical (section "Constants and structs" above, 2026-10-12)
- [x] `msghdr` / `cmsghdr`: identical (below)
- [x] `termios`: identical (above); the other `ioctl`s are assessed case by case, when a program uses them
- [x] Structs confirmed identical ✅ (2026-10-13, `sizeof` and `offsetof` of every field, compiled for both architectures, x86_64 run under qemu):
  - `msghdr` (56 bytes), `mmsghdr` (64), `cmsghdr` (16), `iovec` (16);
  - `timespec`, `timeval`, `itimerval`, `itimerspec`, `timezone`, `rlimit`, `rusage` (144), `tms`;
  - `sockaddr` (16), `sockaddr_in` (16), `sockaddr_in6` (28), `sockaddr_un` (110), `sockaddr_storage` (128), `linger`, `ucred`;
  - `pollfd`, `flock` (32), `sched_param`, `utimbuf`, `f_owner_ex`, `siginfo_t` (128).

  The only different one among those measured is `epoll_event` (12 against 16 bytes), converted.
- [x] Since 2026-10-14, same method or uapi headers of the two architectures: `shmid_ds` (112 bytes), `drm_version` (64), the `struct flock` (32) also for the OFD commands, `udmabuf_create` (24), `dma_buf_sync` (8), `dma_buf_export_sync_file` and `dma_buf_import_sync_file` (8); `MFD_*` flags of `memfd_create`; later on 2026-10-14 (Spotify) `ifreq` (40 bytes, `ifr_ifindex` @16), `iwreq` (32), the capability header (8) and record (12), and the numbers of `SIOCGIFNAME`, `SIOCGIWNAME`, `SIOCETHTOOL`, `F2FS_IOC_GET_FEATURES`. The same. On i386 `struct flock64` (24 bytes) and the old `struct flock` (16) differ, converted (below), and `shmid_ds` (84 bytes), not done yet.

## i386 syscalls (`int 0x80`, ADR-0029, since 2026-10-14)
A 32-bit guest calls the kernel with `int 0x80`: number in `eax`, arguments in `ebx, ecx, edx, esi, edi, ebp`, result in `eax` ✅ (`man 2 syscall`, i386 table). Numbers from `/usr/i686-linux-gnu/include/asm/unistd_32.h` ✅. The module is `armor_linux::syscall::i386`: `syscall::dispatch` goes there when `Cpu::mode` is `Compat32`. The arguments are read as 32 bits (the high half of the registers is ignored), the signed ones (descriptors like `AT_FDCWD`, `lseek` offsets) sign-extended; the result goes into `eax` with the high half zero, and `syscall::result` reads it back sign-extended (for EINTR and ENOSYS in `main.rs`).

**Implemented (184, counted in `is_implemented` on 2026-10-14; 62 with the first static programs, 75 with signals, 88 with threads and processes, 116 for coreutils, 143 with networking and waits, 173 after the Steam client (written here as 172 because of a counting error), then 11 for Doki Doki Literature Club):** `exit`, `exit_group`, `read`, `write`, `writev`, `open`, `openat`, `close`, `dup`, `dup2`, `dup3`, `pipe`, `pipe2`, `fcntl`/`fcntl64` (since 2026-10-14 also the record locks, below), `lseek`, `_llseek`, `ioctl` (the same function as x86_64: `TCGETS`, `TIOCGWINSZ`, `FIONBIO`, `FIONREAD`; the requests without pointers and the EBADF rule of 2026-10-14 apply here too, ⚠️ not tested by an i386 test; `DRM_IOCTL_VERSION` only in the 64-byte x86_64 form), `getdents64`, `getcwd`, `chdir`, `access`, `faccessat`, `faccessat2`, `unlink`, `unlinkat`, `mkdir`, `rmdir`, `readlink`, `readlinkat`, `statx`, `brk`, `mmap2`, `munmap`, `mprotect`, `mremap` (the x86_64 function: since 2026-10-14 shared mappings grow too, ✅ `c32/mremap_shared`, identical to `qemu-i386` and to the Xeon), `madvise`, `uname`, `getpid`, `getppid`, `gettid`, `getuid32`, `getgid32`, `geteuid32`, `getegid32`, `kill`, `tgkill`, `sched_yield`, `set_tid_address`, `set_robust_list` and `rseq` (ENOSYS, as for x86_64), `set_thread_area`, `get_thread_area`, `rt_sigprocmask`, `ugetrlimit`, `prlimit64`, `getrandom`, `clock_gettime`, `clock_gettime64`, `clock_nanosleep_time64`, `futex_time64`, `gettimeofday`; for signals `rt_sigaction` (the 20-byte i386 `struct sigaction`: 32-bit handler, flags, restorer and 8-byte mask, converted into the x86_64 layout that `Signals` keeps), `rt_sigreturn`, `sigreturn`, `rt_sigpending`, `rt_sigsuspend`, `sigaltstack` (12-byte `stack_t`), `tkill`, `alarm`, `pause`, `setitimer` and `getitimer` (`itimerval` with 32-bit `timeval`), `pread64` and `pwrite64` (offset in two registers); for threads and processes `clone` with the i386 argument order (`flags, stack, parent_tid, tls, child_tid`, the kernel's CLONE_BACKWARDS) and with `CLONE_SETTLS` a `user_desc` that sets the TLS entry in the child only (entry −1 is EINVAL, as in `do_set_thread_area` without allocation), `fork`, `vfork`, `clone3` (ENOSYS, glibc falls back to `clone`), `execve` (4-byte `argv` and `envp` pointers), `wait4` and `waitpid` (72-byte `struct rusage`: two 32-bit `timeval`s and 14 `long`s), `futex` (240, with the 32-bit `timespec` widened for the host), `clock_nanosleep` (267) and `nanosleep` (162) with the 32-bit `timespec`, `ftruncate`, `ftruncate64`, `msync`; for i386 coreutils `fadvise64_64` (offset and length in two registers), `sched_getaffinity` (length a multiple of 4, rounded to 8 for the host), `statfs64`/`fstatfs64` (84-byte i386 structure ✅ measured with `sizeof`/`offsetof` under i686 glibc; the size passed must be that one, or EINVAL), `sysinfo` (64-byte structure with 32-bit `long`s; memory scaled by increasing `mem_unit` up to 4096 when it does not fit in 32 bits, like `compat_sysinfo` ⚠️ remembered), `timer_create` (i386 `sigevent` with a 4-byte `sigev_value`), `timer_settime`/`timer_gettime` (time64 too), `timer_getoverrun`, `timer_delete`, `llistxattr`, `prctl`, `socketcall` (102: calls 1–18, arguments read as 32-bit `long`s from the guest's vector; 19 and 20, `recvmmsg` and `sendmmsg`, stop armor) and the direct ones 359–373 (`socket`, `socketpair`, `bind`, `connect`, `listen`, `accept4`, `getsockopt`, `setsockopt`, `getsockname`, `getpeername`, `sendto`, `sendmsg`, `recvfrom`, `recvmsg`, `shutdown`), with the i386 structures converted (below), `poll`, `_newselect` (142, 32-bit `timeval`), `pselect6` (308) and `ppoll` (309) with the 32-bit `timespec`, `pselect6_time64` (413) and `ppoll_time64` (414) with the 64-bit one (since 2026-10-14 `ppoll` and `ppoll_time64` also with a signal mask, as on x86_64: ✅ `c32/ppoll_mask`, identical to `qemu-i386` and to the Xeon), `epoll_create`, `epoll_create1`, `epoll_ctl`, `epoll_wait`, `epoll_pwait`, `epoll_pwait2` (12-byte `struct epoll_event` like the x86_64 one: a `u64` on i386 is aligned to 4), `eventfd`, `eventfd2`; for the Steam client `stat64`, `lstat64`, `fstat64` and `fstatat64` (the 96-byte i386 `struct stat64` ✅ measured with `sizeof`/`offsetof` of the kernel header under i686 gcc: 64-bit device, inode truncated to 32 bits at 12 and full at 88, 32-bit times, like `cp_stat64` ⚠️ remembered), `link`, `linkat`, `mknod`, `mknodat`, `mkdirat`, `chmod`, `fchmod`, `fchmodat`, `fchmodat2`, `rename`, `renameat`, `renameat2`, `symlink`, `symlinkat`, `chown32`, `lchown32`, `fchown32`, `fchownat`, `utimensat` (with the 32-bit `timespec` widened) and `utimensat_time64`, `flock`, `ipc` (semaphores only: `SEMOP`, `SEMGET`, `SEMCTL` with the argument read from the pointer, `SEMTIMEDOP` with the 32-bit `timespec`; message queues and shared memory → unsupported), `semget`, `semctl`, `semtimedop_time64`, `setpgid`, `getpgid`, `getpgrp`, `setsid`, `getsid`, `getpriority`, `setpriority`, `umask`, `fchdir`, `fsync`, `fdatasync`; for Doki Doki Literature Club (2026-10-14) `memfd_create` (356, flags the same ✅), `clock_getres` (266, with the 32-bit `timespec`) and `clock_getres_time64` (406), `sched_setparam`, `sched_getparam`, `sched_setscheduler`, `sched_getscheduler` (154–157), `sched_get_priority_max` and `sched_get_priority_min` (159, 160), `sched_rr_get_interval` (161, 32-bit `timespec`) and `sched_rr_get_interval_time64` (423). Kept in sync with `dispatch` by the module's `implemented_list_matches_dispatch` test. The others stop armor with "i386 syscall N is not supported yet".

Structures:
- **the same as x86_64** ✅ (fixed-size fields or integers only): `statx`, `linux_dirent64`, `sigset_t` (8 bytes), `rlimit64`, the 64-bit `timespec` of the `*_time64` calls, `termios`, `winsize`, `utsname`; `O_*` flags (✅ i686 `asm-generic/fcntl.h`: the same values as x86_64, translated for AArch64 as always);
- **converted** ✅ (layout from i386 `man` pages/headers, tested by `c32/i386_abi` against qemu-i386 and the Xeon): `iovec` (4-byte pointer and length, length signed 32-bit), the `rlimit` of `ugetrlimit` (two 32-bit `long`s, values above 2^32 − 1 as infinity, `COMPAT_RLIM_INFINITY` ⚠️ remembered), 32-bit `timespec` and `timeval` (EOVERFLOW after 2038), `_llseek` (offset in two halves, 64-bit `loff_t` result), `lseek` (EOVERFLOW beyond 2 GiB − 1), `mmap2` (offset in units of 4096 bytes);
- **`user_desc`** of `set_thread_area`/`get_thread_area` (16 bytes: entry number, base, limit, flag bits): entry −1 = the first free one among 12, 13 and 14, written back ✅ Xeon (`i386/basic/tls`: 12, like armor; `qemu-i386` gives 6, the entries of a 32-bit kernel); an entry out of range is EINVAL ✅ Xeon; an all-zero or "empty" descriptor (`LDT_empty`) clears it; a segment that is not 32-bit or with `contents` > 1 is EINVAL ⚠️ (rule of `tls_desc_okay` remembered from the kernel source). If `fs` or `gs` already contain the entry's selector, the base changes at once, as when Linux reloads them.

**Large files without `O_LARGEFILE`** ✅ Xeon (2026-10-14): a 32-bit process that opens a file without `O_LARGEFILE` (glibc without `_FILE_OFFSET_BITS=64`, for example `mkstemp`) cannot write beyond 2 GiB − 1: the `write` at 5 GiB gives EFBIG (the kernel's `MAX_NON_LFS` limit), and opening a file larger than 2 GiB − 1 gives EOVERFLOW ⚠️ (from the kernel source, not measured). armor (like `qemu-i386`) runs as a 64-bit process, to which the kernel always adds `O_LARGEFILE`: there the write succeeds. **Known difference, not reproduced**: it would require remembering for every descriptor whether it was opened without `O_LARGEFILE`. `c32/i386_abi` uses `mkstemp64`.

Syscalls restartable after a handler with `SA_RESTART`: the same as x86_64 with the i386 numbers (`i386::restartable`), since 2026-10-14 also `fcntl` and `fcntl64` ⚠️ (as on x86_64).

**i386 record locks** ✅ (2026-10-14, `sizeof`/`offsetof` under i686 glibc; test `c32/file_locks` in the three variants, identical to `qemu-i386` and to the Xeon): two structures, converted into the host's 32-byte `struct flock` and back after `F_GETLK`:
- the 24-byte `struct flock64` (`l_type` and `l_whence` of 2 bytes, `l_start` @4, `l_len` @12, `l_pid` @20), with the commands `F_GETLK64`/`F_SETLK64`/`F_SETLKW64` (12–14) and with the OFD ones (36–38); it is the one glibc uses with `_FILE_OFFSET_BITS=64`;
- the old 16-byte `struct flock` (`l_start` @4, `l_len` @8, `l_pid` @12: 32-bit offsets, sign-extended), with `F_GETLK`/`F_SETLK`/`F_SETLKW` (5–7); if `F_GETLK` finds a lock that does not fit in 32 bits → EOVERFLOW ⚠️ (kernel rule remembered, not tested by a test).

**i386 networking and waits** ✅ (2026-10-14, `sizeof`/`offsetof` under i686, x86_64 and aarch64 glibc; then `c32/sockets`, `c32/events`, `c32/scm_rights_truncated` identical to the Xeon): the constants (domains, types, `SOCK_*`, `MSG_*`, `SOL_SOCKET`, `SO_*`, `SCM_*`) and `struct sockaddr*` are the same on the three architectures. Different on i386, converted by `net.rs` (`Word::W32`):
- the 28-byte `struct msghdr` (seven 4-byte fields) and the 8-byte `iovec`;
- `struct cmsghdr` with a 12-byte header and data aligned to 4 (16 and 8 on x86_64 and AArch64): control messages are converted in both directions. On receive the host writes into a buffer twice as large as the guest's, enough for every message that fits in the guest; if an `SCM_RIGHTS` does not fit, as in Linux the guest receives the descriptors that fit in the space, `MSG_CTRUNC`, and the others are closed (on x86_64 the host buffer is as large as the guest's and the host itself does the truncation); another message that does not fit is cut to the remaining space, with that length in `cmsg_len` ⚠️ (rule of `put_cmsg_compat` remembered from the kernel source, tested only by the unit test `i386_descriptors_that_do_not_fit_are_left_out`; to measure on the Xeon with `SCM_CREDENTIALS` and a short buffer);
- `SO_RCVTIMEO`/`SO_SNDTIMEO` (20 and 21, the "old" forms) with a `timeval` of 32-bit `long`s;
- `fd_set` of 32-bit `long`s: bit n is still in byte n / 8, but the set is a multiple of 4 bytes and not of 8;
- the `timeval` of `_newselect` and the `timespec` of `pselect6`/`ppoll` are 32-bit; the `_time64` versions use the 64-bit one.

i386 glibc 2.41 calls `pselect6` (308) for `select` and `pselect` and `ppoll` (309) for `ppoll`, with the 32-bit `timespec`, not the `_time64` versions ✅ (`qemu-i386 -strace` on `c32/events`: 7 `pselect6`, 4 `ppoll`; armor stopped on 308 before it existed). The `_time64` ones ⚠️ are needed, from glibc, only for times beyond 2038: implemented but not tested by a test yet.

Not yet: `recvmmsg`/`sendmmsg`, message queues and System V shared memory (with `ipc` or with the direct numbers; the i386 `shmid_ds` is 84 bytes), the signal masks of `pselect6` and `epoll_pwait` (`ppoll`'s since 2026-10-14), `personality` (so no `setarch i686`), and the calls that no measured i386 program has asked for (`tech/steam.md`).

`uname` ✅ (2026-10-14, Xeon): an i386 process on a 64-bit kernel sees `x86_64`, `i686` only with the PER_LINUX32 personality of `setarch`; armor gives `x86_64` (`c32/uname_machine`), `qemu-i386` `i686`.

## vDSO and vsyscall ⚠️
- The guest does not receive an x86_64 vDSO: armor does not put `AT_SYSINFO_EHDR` in the auxv. qemu-x86_64 instead provides it ✅ (auxv read under qemu on 2026-10-12). Without a vDSO glibc falls back to real syscalls ✅: `clock_gettime` reaches armor as a syscall (2026-10-13: an `strace` of armor while python 3.13 calls `time.monotonic()` shows the `clock_gettime` calls forwarded to the host).
- The old vsyscall page (`0xffffffffff600000`) is used only by very old binaries: out of scope for now.
