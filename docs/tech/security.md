# Security: threat model and isolation

**Decision:** ADR-0027 (isolation with namespaces, seccomp and limits, on request with `--sandbox`). **State as of 2026-10-13:** namespaces, file system, network, processes, capabilities, seccomp and limits done, and each one proved by `cargo xtask sandbox-check` (35 checks). As long as a control has no test that proves it, it is marked ⚠️ here; **armor with `--sandbox` must not be considered stronger than this table says**.

## What is protected
- The **files of the user** who launches armor: reading (confidentiality) and writing (integrity).
- The **network**: outgoing connections, listening ports.
- The user's **other processes**: signals, `ptrace`, memory.
- The machine's **resources**: memory, processes, open files, disk space, CPU time.
- The **kernel**: the reachable syscall surface.

## Who attacks
The **guest program**, an untrusted x86-64 binary. It fully controls:
- its own code, the syscalls and their arguments;
- the files that armor loads for it (ELF, libraries of the guest root);
- its own signals and its own threads.

It is assumed that it can exploit a bug in armor (decoder, JIT, syscall translation, loader) and **run arbitrary AArch64 code in armor's process**. For this reason a boundary entrusted to armor's code (for example a path filter in the syscalls) does not count as a security boundary.

## Boundaries
- **Without `--sandbox`** (the default, ADR-0027): **no boundary** beyond the user's permissions, as for a native program or for qemu-user. The permission table prevents the guest from touching armor's memory *through the interpreter and the JIT*, but that is correctness, not security: one bug is enough to get around it.
- **With `--sandbox`**: the boundary is the **kernel**, around the whole armor process (namespaces, seccomp, rlimit). It holds even with armor compromised, as long as the kernel is correct.
- **The JIT's cache** (ADR-0048, since 2026-10-20): translated host code read back from `~/.cache/armor/jit` (files of mode 0600). A process of the same user that can write it can make armor run code of its choice, as it could by replacing armor's executable: no new boundary is crossed without `--sandbox`. With `--sandbox` the cache is not used at all, so the sandboxed guest never runs host code from outside the run.
- **`--host-gl`** (ADR-0041, since 2026-10-15): the guest calls the host's libGL and GLX through armor's syscall 0x410000, with pointers the host library reads and writes directly, outside the permission table. Without `--sandbox` that changes nothing above (there is no boundary to cross); armor refuses `--host-gl` together with `--sandbox` (✅ by hand, 2026-10-15: "--host-gl and --sandbox do not go together"), because the sandbox has no X server and no GPU device, so the host's driver never runs inside the boundary.

## Outside the model
- Kernel bugs (user namespaces expose an additional part of them: it is the price of not asking for privileges).
- Side channels (timing, cache, Spectre).
- Denial of service beyond the configured limits (for example saturating the CPU within the time granted).
- The confidentiality of the data that the user themselves passes to the guest (arguments, environment variables, permitted files).

## Planned controls (`--sandbox`, ADR-0027)
| Control | Mechanism | State |
|---|---|---|
| Reduced file system | mount namespace with a new root (tmpfs): guest root, program, armor binary, `/lib`, `/usr/lib` (the AArch64 libraries to relaunch itself on `execve`) and `--allow-ro` read-only; current directory and `--allow` writable; `/dev/{null,zero,full,random,urandom,tty}`; a new `/proc`; an empty `/tmp`. Everything else on the host does not exist | ✅ `sandbox-check`: a file outside the permitted paths is ENOENT, `--allow-ro` is readable and gives EROFS on write, the current directory is writable; under armor and from native code; mutation (read-only ignored) caught |
| No network | network namespace with only `lo`, down, unless `--allow-net` | ✅ `sandbox-check`: connection to a listening port on the host → ENETUNREACH (succeeds with `--allow-net` and without sandbox); mutation caught |
| Isolated processes | PID namespace: the guest sees and reaches only the sandbox's processes (pid 1 is an init, armor is pid 2; ADR-0027, "Implementation") | ✅ `sandbox-check`: `kill(…, 0)` on a user's process outside → ESRCH; `/proc` shows at most 3 processes (outside, many more) |
| No real privilege | user namespace; the guest's user is mapped to the one who launches armor. Armor (pid 2) drops all the capabilities it has in the namespace (bounding, ambient, effective, permitted, inheritable sets) before the seccomp filter | ✅ `sandbox-check`: `mount` refused (EPERM from native code; under armor the syscall is not implemented and armor stops); `CapEff` 0 in `/proc/self/status` under armor (before the drop it was `000001ffffffffff`: the test showed it) and from native code |
| Kernel surface | seccomp on armor's process (pid 2) and on everything it starts, with no-new-privs (`sandbox::seccomp`): kills the process if a syscall is not AArch64; EPERM for `ptrace`, `process_vm_readv/writev`, `mount`, `umount2`, `pivot_root`, `chroot`, `unshare`, `setns`, `bpf`, `perf_event_open`, `keyctl`, `add_key`, `request_key`, `kexec_load`, `kexec_file_load`, `init_module`, `finit_module`, `delete_module`, `reboot`, `swapon`/`swapoff`, `acct`, `quotactl`, `open_by_handle_at`, `name_to_handle_at`, the new mount API (`fsopen` & co., `open_tree`, `move_mount`, `mount_setattr`), `userfaultfd`, `io_uring_setup`, `syslog`, and `clone` with namespace flags; ENOSYS for `clone3` (a filter does not read its flags; glibc falls back to `clone`) | ✅ `sandbox-check`: from native code `unshare(CLONE_NEWUSER)` and `clone(CLONE_NEWUSER)` → EPERM; without the filter they succeed (mutation caught). `ptrace` is refused even without the filter, by the kernel's rules: that check does not prove the filter. With the filter, threads, `fork`/`exec`, signals and dynamic python work |
| Resources | `--limit-files`, `--limit-processes` (processes and threads, counted per user namespace), `--limit-file-size`, `--limit-cpu` with `setrlimit`, applied before choosing the path (they also apply to a native program); `--limit-memory`: address space of the x86-64 guest, counted by armor on the host pages that its mappings occupy (ENOMEM beyond, like `RLIMIT_AS`). `RLIMIT_AS`/`RLIMIT_DATA` cannot be used: they would count the JIT's 32 GiB permission table | ✅ under armor and from native code: `--limit-files` (EMFILE), `--limit-file-size` (EFBIG with SIGXFSZ ignored), `--limit-processes` (EAGAIN from `fork`, in the sandbox), `--limit-cpu` (death by SIGXCPU; the hard limit is one second later, otherwise the kernel sends SIGKILL immediately). ✅ `--limit-memory` for the x86-64 guest (ENOMEM; unit test in `armor-mem`); **it is not a security boundary**: native code is not subject to it. Real errors found this way: the native program started before the limits; the JIT buffer (a `memfd` sized with `ftruncate`) fell under `RLIMIT_FSIZE` and armor died of SIGXFSZ |

Host facts it relies on (✅ Raspberry Pi 5, kernel 6.18, 2026-10-13):
- Landlock not available;
- unprivileged user, mount and network namespaces working (`unshare --user --map-root-user --mount --net`);
- seccomp with filters available.

## Other requests of the guiding document (§8)
- **Malformed inputs:** fuzzing of the decoder, the ELF parser and the syscall arguments (`cargo xtask fuzz`, `testing.md`) ✅ since 2026-10-13; it found panics and had them fixed.
- **Logs without secrets:** the diagnostics (`--diagnose`, ADR-0028) report only the program's path, syscall numbers, signal names, armor's messages and counters; never arguments, environment variables, file contents or guest memory ✅ (`diagnose_reports_the_run_without_guest_data`: a recognisable argument and variable do not appear in the report). armor's messages can contain guest addresses (rip of a fault).
- **Loaded libraries:** armor does not load host libraries on behalf of the guest (no thunks). The libraries of the guest root are guest code, as untrusted as the program.

## Usage
```
armor --sandbox [--allow PATH]... [--allow-ro PATH]... [--allow-net] program [args]
```
Or in the profile (ADR-0028): `sandbox = yes`, `allow = PATH`, `allow-ro = PATH`, `allow-net = yes`. A native program or a native script interpreter inside the sandbox must be in a permitted path. The file of `--diagnose=FILE` is created beforehand and made writable. If unprivileged user namespaces are disabled, `--sandbox` fails with a message; it does not open silently. ✅ That is the case on Ubuntu 24.04, whose AppArmor restricts unprivileged user namespaces (`kernel.apparmor_restrict_unprivileged_userns = 1`): armor stops with `--sandbox: /proc/self/setgroups: Permission denied` (2026-10-15, Ubuntu 24.04 with its 64K kernel in a virtual machine, `testing.md`); with the restriction lifted (`sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`) every `sandbox-check` probe passes there. ⚠️ An AppArmor profile for armor that allows user namespaces (`userns,`), shipped with the package, would make `--sandbox` work there without lifting the restriction for everyone: not done, to try on Ubuntu.

## How it is verified
For each control, two tests:
1. from an x86-64 program **under armor**;
2. from a **native AArch64** program launched in the same isolation, which stands in for an already compromised armor.

The second is the one that counts for the boundary. A control becomes ✅ only with both.

`cargo xtask sandbox-check` (`xtask/src/sandbox_check.rs`, probe `tests/sandbox/probe.c` compiled for x86-64 and for AArch64) does both, and repeats every control **without** the sandbox, where it must succeed: the proof that the probe would see an escape. It is in the verification chain (`testing.md`).
