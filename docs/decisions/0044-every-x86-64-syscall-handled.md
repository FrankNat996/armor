# 0044 — Every x86-64 syscall handled: as the kernel would answer

- **Status:** Accepted (2026-10-15)
- **Date:** 2026-10-15

## Context
armor handled 247 of the 375 x86-64 syscalls of the kernel headers. Syscalls entered when a real program needed one, with a C test against qemu and the Xeon; any other stopped armor with "syscall N is not supported yet" and exit code 125. A program that probes an optional feature (io_uring, a new futex call, `mseal`) therefore stopped under armor, where on a kernel without that feature it gets an error and carries on.

The conventions forbid a silently wrong result. An error that a real kernel gives in a real configuration is not wrong: a kernel built without io_uring answers ENOSYS, an unprivileged process asking to mount answers EPERM.

## Decision
Every x86-64 syscall gets an answer, in one of four ways, each with a C test in `tests/guest/c/` compared with qemu and, where it runs, with the Xeon:
1. **Done by the host kernel** for the guest, with the arguments converted as for the syscalls before (strings and structures copied, paths resolved in the guest root, `O_*` flags translated): user and group ids (`setresuid` & co., `setgroups`), `personality`, extended attributes (12 calls), `readahead`, `copy_file_range`, `splice`, `tee`, `vmsplice`, `mlock2`, `kcmp`, the `pidfd_*` calls, `process_vm_readv`/`writev`, `process_madvise`, `process_mrelease`, `sched_setattr`/`getattr`, `unshare`, `setns`, `execveat`, `openat2`, `sendmmsg`, `recvmmsg`, `futex_waitv` and the futex2 calls (the futex rule of ADR-0018: words checked, then the guest's addresses), System V message queues and POSIX message queues, Linux AIO (`io_setup` & co.), `signalfd` (the guest's mask is mirrored into the host's, so the host's signalfd sees the signals the guest blocks), and the calls of administrators — `mount` and the new mount API, `umount2`, `pivot_root`, `chroot`, `swapon`, `swapoff`, `reboot`, `sethostname`, `setdomainname`, kernel modules, `kexec_*`, `acct`, `quotactl`, `settimeofday`, `clock_settime`, `adjtimex`, `clock_adjtime`, `syslog`, `vhangup` — which an unprivileged process sees refused with EPERM, as it would natively, and which a privileged one performs on the host, as it would natively. Also `statmount`, `listmount`, `cachestat`, `fanotify_*`, `name_to_handle_at`, `open_by_handle_at`, the `lsm_*` calls, the key calls (`add_key`, `request_key`, `keyctl` for its operations with plain arguments, strings and buffers), the NUMA calls, `perf_event_open` and `bpf`.
2. **Emulated by armor** where the host cannot do it for the guest: `seccomp` (and `prctl(PR_SET_SECCOMP)`): the guest's classic BPF filters run in armor on every guest syscall, with the x86-64 (or i386) numbers and `AUDIT_ARCH`, since a filter given to the host would see AArch64 numbers and armor's own syscalls.
3. **Answered as a real kernel configuration answers**, with the configuration named in `tech/syscalls.md`: io_uring (ENOSYS, a kernel built without it, as ChromeOS and Android are), `userfaultfd` and `memfd_secret` (ENOSYS, built without them or not enabled, the default), `mseal` (ENOSYS, a kernel before 6.10), `map_shadow_stack` (EOPNOTSUPP, a CPU without user shadow stacks, as armor's CPUID says), `ptrace` (EPERM, Yama's `ptrace_scope` 3: armor cannot give a debugger the guest's registers), `get_robust_list` (ENOSYS, like `set_robust_list` before), `remap_file_pages` (ENOSYS: the guest's mappings are armor's), `lookup_dcookie` and `nfsservctl` (ENOSYS, removed from Linux), `restart_syscall` (EINTR: armor never sets up a restart block). `--diagnose` lists each ENOSYS a program received.
4. **Still an explicit stop** for an operation of a handled syscall that needs work armor has not done (a `keyctl` operation with nested structures, an XFS quota command), with the message naming it, as today for `ioctl` requests and `fcntl` commands.

The i386 table gets the same answers for the same calls where its structures are the same; the others follow when i386 programs need them.

## Alternatives considered
- **ENOSYS for every syscall armor does not implement:** simple, but many of them have a real answer the host gives for free, and ENOSYS would hide work armor could do correctly.
- **Passing a guest's seccomp filter to the host:** the filter would judge armor's AArch64 syscalls by x86-64 numbers.
- **Emulating ptrace:** a debugger under armor would need the guest's registers and memory of another armor process through a protocol of armor's own; large, and a real configuration (Yama 3) already answers EPERM.

## Consequences
- \+ No x86-64 syscall stops armor any more; the coverage badge counts every syscall of the headers.
- \+ Programs that probe optional features take the path they take on a kernel without them.
- − Privileged calls do on the host what they would do natively: a guest run as root that mounts or reboots does mount or reboot.
- − Linux AIO and the futex calls write the guest's memory from the kernel, past armor's page permissions, as `futex` already does (ADR-0018).
