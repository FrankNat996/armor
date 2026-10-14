# 0013 — Guest root: `ARMOR_ROOT`, with the same resolution as `qemu -L`

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12
- Completes ADR-0011, which deferred this choice to the implementation.

## Context
ADR-0011 loads the real x86-64 `ld.so` from a guest root. It must be decided how the root is chosen and how guest paths are resolved in it, without hiding the host files the program must see (current directory, `/proc`, `/etc/passwd`, …).

Measurement of 2026-10-12 (`strace -f` on the host of `qemu-x86_64 -L /usr/x86_64-linux-gnu` with a dynamic hello): for every absolute path `/p`, qemu tries `faccessat("<root>/p", F_OK)`. If it exists it uses that, otherwise the host path `/p`. For example `/lib/libc.so.6` becomes `/usr/x86_64-linux-gnu/lib/libc.so.6`, while `/etc/ld.so.cache`, absent from the root, remains the host's.

## Decision
- **Resolution:** the same as qemu's. An absolute path `/p` becomes `<root>/p` if it exists (following symbolic links), otherwise it stays `/p`. Relative paths never change.
  - It applies to the loader (`PT_INTERP`) and to the syscalls that receive a path: `open`, `openat`, `access`, `faccessat`, `faccessat2`, `newfstatat`, `readlinkat`.
- **Choice of the root:** the environment variable `ARMOR_ROOT`. Since 2026-10-12 also the option `--root DIR`, which takes precedence over it (used by `execve`, ADR-0019).
  - If it is empty: no root.
  - If it is not set: `/usr/x86_64-linux-gnu` (from `libc6-amd64-cross`) if it exists, otherwise no root.
  - It is an environment variable, like `QEMU_LD_PREFIX`, because the guest inherits it and passes it to child processes (`execve`, ADR-0019, with `--root`).
- `difftest` sets the same root on both sides: `-L` for qemu, `ARMOR_ROOT` for armor.

## Alternatives considered
- **Command-line option (`--root`):** does not reach child processes and complicates the `armor <program> [arguments]` syntax. It can be added later as a shortcut.
- **Exclusive root (chroot):** the program would not see the host files outside the root. It is useful for isolation (M5, threat model), but it is not the oracle's behaviour.
- **No default value:** more explicit, but `armor ./program` would fail for every dynamic binary.

## Consequences
- \+ Same behaviour as the oracle, so the differential tests stay meaningful.
- \+ Works without configuration on the development host.
- − A host file with the same name as one missing from the root is used: for example the host's `/etc/ld.so.cache` (AArch64), which the x86-64 `ld.so` ignores because the entries are not x86-64.
- − **The default value diverges from the oracle.** Without `ARMOR_ROOT`, armor uses `/usr/x86_64-linux-gnu` as the root for *every* program, even static ones: for example `armor ./busybox ls /lib` lists the cross-compiler's x86-64 libraries, not the host's `/lib`. qemu without `-L` (or `QEMU_LD_PREFIX`) has no root. `difftest` does not see this, because it sets the same root on both sides. To get the behaviour without a root: `ARMOR_ROOT=` (empty).
- − The guest sees `ARMOR_ROOT` in its own environment.
- − A root is not a sandbox: the guest still accesses the whole host (threat model in M5).
