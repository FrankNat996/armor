# 0019 — Processes: host `fork`, `execve` that relaunches armor

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
Shells, `make`, `system()` and `posix_spawn` create processes. Measurement of 2026-10-12 with `qemu -strace` on the `c/processes` test (glibc 2.41):
- `fork()` → `clone(CLONE_CHILD_SETTID|CLONE_CHILD_CLEARTID|SIGCHLD)`;
- `posix_spawn` and `system()` → `clone(CLONE_VM|CLONE_VFORK|SIGCHLD)` with a separate stack for the child, which then calls `execve`;
- `vfork`, `execve`, `wait4`, `getppid`.

On this host binfmt_misc registers qemu for x86-64 ELFs. An `execve` of an x86 binary passed as is to the kernel would therefore start under qemu, not under armor.

## Decision
- **`fork`, `vfork` and `clone` without `CLONE_THREAD` → host `fork()` of the whole of armor.** The guest's address space is armor's memory mapped identically, so the child has a copy of it at the same addresses, as after a real fork.
  - Only with a single-threaded guest: with several threads → "not supported" for now. The internal locks (GIL, page tables) could be held by threads that no longer exist in the child.
  - Exit signal: only `SIGCHLD`.
  - **`CLONE_VM|CLONE_VFORK` (`vfork`, `posix_spawn`) becomes a fork with copying, not with shared memory.** The parent stays stopped until the child runs `execve` or terminates, with an `O_CLOEXEC` pipe that the child closes at those two moments. Known difference: the child's writes are not seen in the parent. glibc uses them only to report an `execve` error in `posix_spawn`; with armor that error becomes a child that exits with 127.
- **`execve`:**
  - x86-64 ELF → host `execve` of armor itself, with `--root` (the guest root) and `--argv0` (the guest's `argv[0]`), plus `--jit` or `--lockstep` if active (since 2026-10-12, with the JIT), plus the program path resolved in the root;
  - `#!` scripts with an x86-64 interpreter → armor on the interpreter, with the arguments the kernel would build (interpreter, optional argument, script path, arguments);
  - everything else (native host binaries, scripts with a native interpreter) → host `execve` as is;
  - `/proc/self/exe` is the guest program, not armor.
- **`wait4`, `waitid`, `getppid`, process groups:** passed to the host. The exit status is already correct, because armor terminates with the guest's status or signal.
- New `armor` options: `--root DIR` (takes precedence over `ARMOR_ROOT`) and `--argv0 NAME`, also useful by hand.

## Alternatives considered
- **Leaving `execve` to the kernel and registering armor in binfmt_misc:** it is the system-wide solution (also for native programs that launch x86 binaries), but it requires privileges and changes the host. To be considered for the Compatibility Manager (M5).
- **`fork` emulated within a single process** (several address spaces in armor): much more complex and without advantages for an interpreter.
- **`vfork` with truly shared memory:** would require running the child as a thread on the same address space until `execve`, then a real fork at the moment of the exec. Possible later if a program depends on the sharing.

## Consequences
- \+ Process semantics of the real kernel: pids, signals, waits, inherited descriptors, `O_CLOEXEC`.
- \+ x86 child processes run under armor with the same root.
- − `fork` from a guest with several threads is not supported. *Update 2026-10-14:* now it is. The thread that calls `fork` first takes the locks the other threads could hold (page tables exclusively, `brk`, signal actions, the GIL with `ForkHooks`); the child keeps only that thread (test `fork_threads`, `tech/syscalls.md`).
- − A native host program that launches an x86 binary starts it under qemu (binfmt), not under armor.
- − The writes of a `vfork` child are not seen in the parent.
