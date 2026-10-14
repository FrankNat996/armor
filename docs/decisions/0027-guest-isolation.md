# 0027 — Guest isolation: namespaces, seccomp and limits, on request

- **Status:** Accepted (2026-10-13)
- **Date:** 2026-10-13
- Threat model: `tech/security.md`.

## Context
The guiding document (§8) asks that guest code be considered untrusted, and forbids declaring armor a sandbox without a threat model and verification. Today the guest runs in armor's process and its syscalls reach the kernel with the user's permissions, as with qemu-user: no isolation of file system, network or resources (the initial audit of 2026-10-12 §7 (replaced by `analysis.md`)).

The guest controls its own code and can exploit an armor bug (decoder, JIT, syscall translation) to run arbitrary AArch64 code in armor's process. A boundary placed **inside** armor, for example a path filter in the syscall layer, falls with it. The boundary must be in the kernel, around the whole process.

Facts measured on the Raspberry Pi 5 (kernel 6.18, 2026-10-13):
- **Landlock is not there** (`CONFIG_SECURITY_LANDLOCK` off; `/sys/kernel/security/lsm` = `capability`);
- **unprivileged user namespaces: yes** (`unshare --user --map-root-user --mount --net`: mount of a tmpfs succeeded, in the network namespace there is only `lo`, down);
- **seccomp with filters: yes** (`CONFIG_SECCOMP_FILTER=y`).

## Decision
- **`armor --sandbox`** (or a profile, ADR-0028) isolates armor's process, and therefore the guest, with kernel mechanisms:
  - **user namespace**: no real privileges; the guest's user is mapped to the one that launches armor;
  - **mount namespace** with a new root: the guest sees only the guest root (read-only), the current directory, the paths given with `--allow` (and `--allow-ro`), plus the minimum armor itself needs to relaunch itself on `execve` (its binary and the host's AArch64 libraries, read-only) and a few devices (`/dev/null`, `/dev/zero`, `/dev/urandom`). All the rest of the host file system does not exist for the guest;
  - **network namespace**: no network (only `lo`), unless `--allow-net`;
  - **seccomp**: after setup, armor denies itself the syscalls that serve only to escape the isolation or to attack the kernel (`ptrace`, `mount`, `umount2`, `pivot_root`, `unshare`, `setns`, `bpf`, `perf_event_open`, `keyctl`, `kexec_*`, `*_module`, …). Exact list and reasons in `tech/security.md`;
  - **resource limits** with `setrlimit` where they are compatible with armor (open files, processes, file size, CPU time); the guest's memory with a limit counted by armor, which is not a security boundary and is documented as such (`RLIMIT_AS` and `RLIMIT_DATA` also count the 32 GiB permission table reserved by the JIT).
- **On request**: without `--sandbox` armor behaves as today, with the user's permissions.
- **Verification:** tests that try every control from the inside, both from an x86-64 program under armor and from a native AArch64 program launched in the same isolation, which represents an already compromised armor. A control without a test that proves it is not declared.

## Implementation (2026-10-13)
- **PID namespace and an init**, in addition to what is written above: without it, the guest would see in `/proc` the user's other processes (including their `environ`) and could send them signals (same uid). Pid 1 of a namespace is protected from signals without a handler, so it cannot be the guest: a supervisor stays outside (it forwards SIGTERM, SIGHUP, SIGUSR1/2, SIGCONT and ends like the guest, exit status or signal), pid 1 is a small init that reaps processes, and armor runs as pid 2. A new `/proc`, of the PID namespace; an empty `/tmp` (tmpfs).
- Verification: `cargo xtask sandbox-check` (`tech/security.md`).

## Alternatives considered
- **Controls inside armor** (path and network filters in the syscalls): simpler, but one armor bug is enough to escape. Discarded as a security boundary.
- **Landlock**: not available on this kernel. It can be added where it exists.
- **Isolation always on**: safer, but every current use would have to be configured; chosen: on request.
- **Only the threat model**, isolation later: postpones the problem.

## Consequences
- \+ The boundary is the kernel: it holds even if armor is compromised.
- \+ No privileges required (user namespaces).
- − It depends on unprivileged user namespaces, which some distributions restrict (for example AppArmor on Ubuntu): `--sandbox` fails with a clear message, it does not silently open up.
- − The guest sees a reduced file system: a program that looks for files outside the allowed paths finds them missing.
- − The memory limit is counted by armor and is not a security boundary.
