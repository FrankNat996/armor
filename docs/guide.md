# Testing x86 software on ARM64 with armor: a guide for developers

For someone who has an x86 program (their own, or one they depend on) and wants to know whether and how well it runs on an ARM64 Linux machine, and how to show it reproducibly. Every command below exists today; the details are in the documents each step links.

## 1. Look at the file
```bash
armor inspect ./program
```
It reads the file without running it: format (ELF, PE, script, AppImage), architecture, static or dynamic, the libraries it needs, the instruction-set extensions its code uses (from a length decoder over the code), and a verdict: whether armor can load it, and if not, what to do (`architecture.md`, `inspect.rs`).

## 2. Run it, with a report
```bash
armor --diagnose ./program args        # or --diagnose=report.txt
```
armor picks the path by itself (x86-64 and i386 under its engine, AArch64 programs and scripts natively, ADR-0028). At the end the report lists the path taken, how the program ended, the syscalls answered with ENOSYS, faults, the instructions armor stopped on, and suggestions; it contains no arguments, environment or data of the program. When armor itself cannot go on, it says why on stderr and exits with 125 (`conventions.md`).

A program that needs x86 libraries runs with a guest root: `armor --root DIR ./program` (the root of `cargo xtask fetch-rootfs`, or the one `armor install` makes for an application). Per-program settings go into a profile (`~/.config/armor/profiles/<name>.conf`: engine, root, environment, sandbox, limits; ADR-0028).

## 3. Install it like a user
```bash
armor install file.exe|file.msi|file.zip|file.deb|file.AppImage|file.tar.xz
armor list
armor run <application>                # its messages go to <app>/last-run.log
armor remove <application>
```
Windows programs get their own Wine prefix (Hangover, or Steam's ARM64 Proton when Steam is installed); Linux packages get a root with the Debian libraries they need, verified as apt does (ADR-0033, ADR-0040, ADR-0043). When a program fails to start, `armor run` explains it in one line and keeps the details in `last-run.log`.

## 4. Compare it with a reference
armor's correctness is shown by running the same x86 program twice and comparing standard output and exit status (`testing.md`):
- `cargo xtask difftest`: against `qemu-x86_64` on the same machine, with armor's interpreter and JIT; `--lockstep` checks every translated instruction against the interpreter;
- `--oracle`: against a real x86-64 machine over SSH (`ARMOR_ORACLE`, `ARMOR_ORACLE_KEY`; ADR-0025);
- `--native`: against the same C source built for AArch64 and run natively, for syscalls the host kernel answers for both (ADR-0044);
- `--save DIR` / `--expected DIR`: keep qemu's results on one machine and compare another machine's armor with them, where qemu-user itself fails (16K and 64K pages).

To add a case: a C file in `tests/guest/c/` (built in three x86-64 variants, and in three i386 ones if it is listed in `I386_SHARED_C_TESTS`), an assembly file in `tests/guest/asm/`, or a line in the busybox, coreutils, tools or interpreters lists. If qemu is wrong for it, list it in `HARDWARE_ONLY` with the reason; if only the native oracle can judge it, in `NATIVE_ORACLE`. Print only what does not depend on the machine (no pids, addresses, user names).

## 5. Check a real application unattended
```bash
cargo xtask app-check [id...]
```
installs pinned real applications (each installer checked by SHA-256) into a throwaway data directory and runs them under Xvfb: level 1 when the application's window appears, level 3 when its output is byte-for-byte that of a native build (7-Zip's archives). `--x86-engine armor` runs the 32-bit code of Windows applications under armor's own WoW64 backend instead of FEX, `--x64-engine armor` their x86-64 code under armor's own ARM64EC emulator (ADR-0047). To add an application, add an entry to `APPS` in `xtask/src/app_check.rs`: installer URL and SHA-256, installer arguments, and the check.

## 6. Record the result
Add `--report` to `difftest`, `app-check`, `distro-check` or `bench`: the result goes into `docs/results/<machine>.tsv` with the machine (CPU, page size, system, native or virtual), the date, the commit and the command. `cargo xtask matrix` renders the compatibility matrix and the README's summary from those files (ADR-0045). What cannot run unattended (an account, a store) goes into `docs/results/by-hand.tsv`, with how and when it was checked.

## 7. Other machines
- **Another CPU:** the GitHub ARM64 runner runs the chain at every push (`.github/workflows/arm64.yml`) and publishes its results file as an artifact.
- **16K and 64K pages:** KVM virtual machines with Debian's 16K kernel and Ubuntu's 64K kernel (`testing.md`, "Other page sizes").
- **Other distributions:** `cargo xtask distro-check` installs armor's archive in Fedora and Arch Linux ARM containers (ADR-0043).

## 8. Speed
`cargo xtask bench [--box64] [--fex]` times armor's JIT (with its cache of translations warm, and on a first run with the cache empty, ADR-0048), qemu, the native build and, if given, box64 and FEX on the same workloads; `ARMOR_JIT_STATS=interpreted` lists what the JIT left to the interpreter, by kind; `perf` profiles armor itself (`tech/performance.md`).

## 9. Reporting a problem
Include armor's version (`armor --version`; `armor update --check` also says whether a newer one is out), the output of `armor inspect` on the program, the `--diagnose` report or the application's `last-run.log`, and the machine (CPU, page size, distribution). armor's own stop messages name what is missing ("syscall N", "ioctl request 0x…", an instruction): that line is the most useful one.
