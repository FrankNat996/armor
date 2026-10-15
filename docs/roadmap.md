# Roadmap

**Goal** (ADR-0034, 2026-10-14): a single application that makes it simple for anyone with an ARM64 computer to use any x86 software — Linux programs, Windows programs and games — without knowing what happens underneath; and a measured reference for development on ARM. Priorities, in order: ease for everyone, any x86 program, any ARM64 CPU, the best existing component for each layer, reproducible measurements. What it takes to get there: `analysis.md`.

**State:** M0–M6 (the engine) completed on 2026-10-12 and 2026-10-13. Since 2026-10-14 the open milestones are, in order of priority, **M7** (the installer for everyone), **M8** (games and Steam), **M9** (every ARM64), **M10** (the reference).

Milestones M7–M10 were rewritten on 2026-10-14 (ADR-0034). Where the previous items ended up, for reading the earlier ADRs:

| Before 2026-10-14 | Now |
|---|---|
| M7 Desktop, multimedia and Windows | M7 (Windows programs), M8 (graphics, audio, games) |
| M8 New targets | i386: done (M7); "every ARM64": M9; other architectures: Beyond |
| M9 stages 1–2 (i386) | done, item of M7 |
| M9 stage 3 (i386 Steam client under armor, `steam.sh`) | M9, as a fallback on 16K systems |
| M9 stage 4 (measuring box64 and FEX) | M10 |
| M9 stage 5 (Valve's ARM64 Steam client) | done, item of M8 |
| M9 stages 6 and 8 (ARM64 Proton, native host libraries) | M8 |
| M9 stage 7 (every ARMv8.0) | M9 |
| M9 stage 9 (the single command) and M10 stages 1–5 (`armor install`) | M7 |

Durations are orders of magnitude, not commitments.

Each milestone closes only when its completion criterion passes **and** the documentation is up to date.

The top-level checkboxes of each `## M<n>` section generate the percentages in the README. After changing them run `cargo xtask progress` (details in `conventions.md`).

---

## M0 — Infrastructure (days) — ✅ completed 2026-10-12
- [x] `git init`, license (ADR-0006), `.gitignore`
- [x] Toolchain installed (`environment.md` § Installed toolchain)
- [x] Cargo workspace with the planned crates, still empty (`architecture.md`). `armor-jit` is postponed to M6.
- [x] Harness that builds and runs x86_64 binaries with `qemu-x86_64`: `cargo xtask build-guest` and `cargo xtask run <name>` (`testing.md`)

**Completion criterion:** `cargo test` passes and the harness runs a static x86_64 binary with `qemu-x86_64`, capturing stdout and exit code.

## M1 — Static hello world (days) — ✅ completed 2026-10-12
- [x] ELF64 loader for static non-PIE executables (`ET_EXEC`), with **several segments with different permissions in the same 16K host page** (this already happens in `basic/exit42`, see `tech/memory.md`): 4K permission table in `armor-mem` (minimal strategy)
- [x] Guest CPU state: 16 GPRs, RIP, RFLAGS, FS/GS base
- [x] Initial guest stack: argc, argv, envp, auxv
- [x] Decoder and interpreter for a minimal subset: `mov`, `lea`, `add`/`sub`, `cmp`, `test`, `jmp`/`jcc`, `call`/`ret`, `push`/`pop`, `syscall`
  - Encodings used by the current acceptance tests (✅ from `objdump -d`): `B8+r imm32` (`mov r32, imm32`), `48 8D /r` with RIP-relative ModRM (`lea r64, [rip+disp32]`), `31 /r` (`xor r32, r32`), `0F 05` (`syscall`)
- [x] Syscalls `write`, `exit`, `exit_group`

- [x] `cargo xtask` also runs the guest programs under armor and compares stdout and exit code with qemu (not stderr: qemu's and armor's diagnostics differ, `testing.md`)

**Criterion:** `basic/exit42` and `basic/hello` (`tests/guest/asm/`) produce under armor the same stdout and the same exit code obtained with `qemu-x86_64`.
✅ Reached on 2026-10-12: `cargo xtask difftest` passes on these two and on 18 other programs that cover the whole subset (`testing.md`).

## M2 — Complete decoder and static programs with glibc (weeks) — ✅ completed 2026-10-12
Measured starting point (2026-10-12): a C hello with static glibc stopped on `67 e8` (`addr32 call`) during glibc startup.
The original items "complete decoder" and "complete integer ALU" were split on 2026-10-12: the part glibc needs is done, the rest is not.

- [x] Decoder: legacy prefixes (including `67`, `F0`, `F2`/`F3`), REX, ModRM/SIB, RIP-relative, 1-byte and `0F` maps for integer and SSE2 (`tech/x86-64-encoding.md`)
- [x] Integer ALU and string instructions used by glibc: unary, mul/div, shifts and rotations, extensions, cmov/setcc, xchg/cmpxchg/xadd, bit test and bit scan, bswap, `rep movs/stos/lods/cmps/scas`. Lazy flags only if performance requires them.
- [x] SSE2 subset used by glibc: 128/64/32-bit moves, per-lane arithmetic and comparisons, logic, shifts, unpack, `pshufd`, `pmovmskb`, `pmuludq` (measured with `qemu -d in_asm`, `tech/cpuid.md`)
- [x] `CPUID` that declares only the implemented features (ADR-0010, `tech/cpuid.md`)
- [x] Syscalls required by static glibc, measured with `qemu -strace`: `brk`, `mmap`/`munmap`/`mprotect`, `arch_prctl`, `set_tid_address`, `set_robust_list`, `rseq`, `prlimit64`, `readlinkat`, `getrandom`, `fstat`/`newfstatat`, `ioctl` (TCGETS, TIOCGWINSZ) (`tech/syscalls.md`)
- [x] Decoder tests compared against `x86_64-linux-gnu-objdump` (`cargo xtask decoder-check`)
- [x] Static busybox: basic applets. The Debian `busybox-static` binary ran with 38 cases identical to qemu (`tests/guest/busybox.txt`) on 2026-10-12; `seq` and `awk` arrived with scalar floating point (ADR-0015), and today there are 50 cases (`COMPATIBILITY_MATRIX.md`).

**Criterion:** static C programs compiled with `x86_64-linux-gnu-gcc -static` (hello, integer `printf`, `malloc`) give the same output they give under qemu. Then: basic applets of static busybox.
✅ Reached on 2026-10-12: 5 C programs (`tests/guest/c/`) and 38 busybox applet runs identical to qemu.
The two items still open were moved on 2026-10-12: `0F38`/`0F3A` maps and VEX to M4, remaining integer instructions to M3.

## M3 — Real processes: threads, signals, memory (weeks–months) — ✅ completed 2026-10-13
- [x] `clone` and threads (1:1 model, `architecture.md`), `futex`, TLS (FS base via `arch_prctl` and `CLONE_SETTLS`) (2026-10-12, ADR-0018: `c/threads` and `c/threads_signals` identical to qemu, 100 runs out of 100)
- [x] Processes: `fork`, `vfork`, `execve` (which relaunches armor), `wait4` (2026-10-12, ADR-0019: `c/processes`, the busybox shell, `env`/`timeout`/`nice`/`nohup` from coreutils identical to qemu)
- [x] Signals: `rt_sigaction`, building the x86_64 signal frame on the guest stack, `rt_sigreturn` (2026-10-12, ADR-0017: `c/signals` and `c/signals_async` identical to qemu)
- [x] 4K guest memory on a 16K host (ADR-0005, `tech/memory.md`). Done: permission table (ADR-0009), `mmap` of `MAP_PRIVATE` files by copy (ADR-0012) (2026-10-12); then (2026-10-13: `MAP_SHARED` with ADR-0022, atomic `LOCK` instructions across processes; the JIT's memory was already solved by ADR-0021, which checks 4K permissions in software)
- [x] Translation of the syscalls missing on aarch64 and of the structs with a different layout (`tech/syscalls.md`) (2026-10-13: all 57 missing on AArch64 handled except `signalfd` and the write side of `modify_ldt`, with a reason; `struct stat` and `epoll_event` converted, the other structs measured identical; test `c/legacy_syscalls`)
- [x] Remaining integer instructions, as a program needs them: `cmpxchg8b`/`16b`, `enter`, `popf`, `lahf`/`sahf`, `mov` with a 64-bit absolute address, … (added on 2026-10-12: `rdtsc`, `fxsave`/`fxrstor`, `ldmxcsr`/`stmxcsr` for `ld.so`; `shld`/`shrd` for `sha512sum`) (moved from M2) (2026-10-13: all those listed done, plus `xlat`, `int n`/`int1`, reading `cs`/`ss`; privileged ones give #GP as in Linux. Remaining, with a reason in `tech/x86-64-encoding.md`: `int 0x80`, segment registers beyond `cs`/`ss`, rare 16-bit forms)
- [x] Fuzzing of the components that read guest input: decoder, ELF parser, syscall arguments (guiding document, §8) (2026-10-13: in-tree tests without dependencies, `cargo xtask fuzz`; it found `brk`/`munmap` hanging armor on huge ranges, `testing.md`)

**Criterion:** static multi-threaded programs and programs with signal handlers pass the differential tests.
✅ Reached on 2026-10-13: `c/threads`, `c/threads_signals`, `c/signals`, `c/signals_async`, `c/processes`, `c/shared_memory`, `c/shared_atomics` identical to qemu with the interpreter, with the JIT and in lockstep.

## M4 — Floating point and SIMD (months) — ✅ completed 2026-10-13
- [x] Complete SSE/SSE2, then SSE3/SSSE3/SSE4.1/SSE4.2. Done on 2026-10-12: scalar SSE/SSE2 (ADR-0015, `tech/floating-point.md`); on 2026-10-13 the packed forms and the missing integer SSE2 instructions (MMX excluded), then SSE3, SSSE3, SSE4.1, SSE4.2 and POPCNT, each declared in CPUID and verified against qemu and an Intel Xeon X5675 (`sse/sse3`, `ssse3`, `sse41`, `sse42`)
- [x] Decoder: `0F38`/`0F3A` maps (SSSE3, SSE4) and VEX decoding (AVX) (moved from M2). On 2026-10-13 the maps with SSSE3, SSE4.1 and SSE4.2 (`maps.rs`); VEX and EVEX decoded for length and extension (`length.rs`), not for operands: armor does not execute them, and its CPUID does not declare AVX
- [x] Length-only decoder for all of x86-64 (VEX/EVEX/x87 too), so that `armor inspect` can list the instructions a program uses (2026-10-13: `length.rs`, 11.3 million instructions of the guest root with objdump's length; `inspect` lists the extensions)
- [x] x87 with 80-bit precision (2026-10-12, ADR-0016: own implementation, identical to qemu on the 27,200 results of `c/x87`; transcendentals excluded until a program uses them)
- [x] MXCSR, rounding modes, FP exceptions (2026-10-13, ADR-0024: SSE in software in the interpreter, flags from the host FPSR corrected in the JIT, four roundings, FZ, DAZ, #XM → SIGFPE; identical to an Intel Xeon X5675 in `sse/mxcsr_*` and `c/sse_exceptions`, `tech/floating-point.md`)

**Criterion:** differential FP test suite green.

✅ Reached on 2026-10-13: `difftest` green with the interpreter, with the JIT and in lockstep on `sse/*` (scalar, packed, SSE3–SSE4.2, the three MXCSR tests), `x87/*`, `c/x87`, `c/x87_more` and the real programs with floating point (python `math` and complex numbers, `sort -g`, `awk`); in addition `difftest --oracle` identical to an Intel Xeon X5675, including the cases where qemu is wrong (`sse/mxcsr_hardware`, `sse/sse42_hardware`, `c/sse_exceptions`). Left out, by choice: x87 transcendentals (until a program uses them), MMX, AVX execution.

## M5 — Dynamic linking (months) — ✅ completed 2026-10-13
- [x] Dynamic linking, a root or wrapped libraries: a root with the real x86-64 `ld.so` (ADR-0011)
- [x] Support for `PT_INTERP` and `ET_DYN` (PIE) (2026-10-12: `c-pie/*` and `c-dyn/*` identical to qemu)
- [x] Handling of guest rootfs paths (2026-10-12, ADR-0013: `ARMOR_ROOT`, resolution like `qemu -L`)
- [x] Reproducible guest root from pinned Debian amd64 packages and coreutils in `difftest` (2026-10-12, ADR-0014: 46 cases identical to qemu)
- [x] Detection: `armor inspect <file>` with recognised / loadable verdict, linking and required libraries (2026-10-12)
- [x] Compatibility Manager: automatic choice of the execution path, per-application profiles, diagnosis with suggestions (guiding document, §5; ADR-0028: JIT by default, native for AArch64, scripts, refusals with a reason; profiles in `~/.config/armor/profiles/`; `--diagnose`; done on 2026-10-13)
- [x] Threat model and isolation of the guest environment (file system, network, resources) (guiding document, §8; ADR-0027: `--sandbox` with user, mount and network namespaces, seccomp and limits, also tested from native code; model in `tech/security.md`; done on 2026-10-13: `cargo xtask sandbox-check` 27/27, 35 checks, all tested)

**Criterion:** Debian amd64 dynamic binaries (for example coreutils) work. As of 2026-10-12: 46 coreutils cases identical to qemu, including the floating-point ones (`sort`, `tail`, `seq`, `numfmt`).

## M6 — AArch64 JIT (months) — ✅ completed 2026-10-13
- [x] Basic-block translation, block cache, invalidation: first translator with `--jit` (2026-10-12, `tech/jit.md`). Invalidation is global by generation; per-page invalidation will come later.
- [x] Memory model: x86 TSO on a weakly ordered ARM (`tech/memory.md`) (2026-10-13, ADR-0023: threads in parallel, `LDAPR`/`STLR` and barriers in concurrent mode, atomic `LOCK`; litmus tests `c/litmus`)
- [x] Self-modifying code detection: writes to executable pages go to the interpreter, which invalidates (2026-10-12)
- [x] W^X and instruction cache flush: double mapping of shared memory (a `memfd` until 2026-10-13), `ic ivau` according to `CTR_EL0` (2026-10-12)
- [x] The interpreter stays as fallback and as internal oracle: fallback for untranslated instructions, oracle with `--lockstep` (2026-10-12)
- [x] Reproducible benchmarks: translation cost, integer, floating point, SIMD, syscalls, memory, startup; comparison with qemu and, where possible, with native (guiding document, §6) (2026-10-13: `cargo xtask bench`, first measurement in `tech/performance.md`)

**Criterion:** all differential tests also pass in JIT mode. Benchmarks documented.
✅ Reached on 2026-10-13: `difftest` 255/255 with the interpreter, with the JIT and in lockstep; `cargo xtask bench` with the measurements in `tech/performance.md`; parallel threads with TSO verified by the litmus tests.

## M7 — The installer for everyone: any x86 program (priorities 1 and 2) ✅
An x86 program for Linux or for Windows installs with one command or a double click, without root, shows up in the desktop menu and opens (ADR-0033, ADR-0034). Every step done by hand today becomes automatic.
- [x] The engine also runs 32-bit x86 programs (i386), without an armhf userspace (ADR-0029, 2026-10-14: static and dynamic programs and i386 coreutils identical to `qemu-i386` and to a real x86 CPU, with the JIT too)
- [x] `armor install`/`run`/`list`/`remove` for Windows programs: one Wine prefix per application in `~/.local/share/armor/apps/`, menu entries with icons, the prefix's Desktop separate from the user's, x64 DXVK, per-application settings (`app.conf`) (2026-10-14: 7-Zip x64 from `.exe` and from `.msi`, opened from the desktop menu; the archive created by 7-Zip x64 under FEX is identical to the one from the native ARM64 build)
- [x] `armor engine fetch`: Hangover (ARM64X Wine with FEX and box64) and DXVK downloaded for the host's distribution, verified with pinned size and SHA-256, extracted without root (2026-10-14: Debian 12 and 13, Ubuntu 22.04–26.04; tested in an empty folder)
- [x] No manual steps in the Windows engine: Wine Mono installed by armor when a program asks for it (verified with the hash that Wine itself carries), the GPU recognised (software renderer only where the Vulkan driver is not enough), the common runtime components. First test bench: the steps done by hand for the Epic Games Launcher on 2026-10-14 (Wine Mono, Epic Online Services, llvmpipe filter). 2026-10-14: Wine Mono 11.3.0 fetched with the engine (pinned) and installed in every new prefix; DXVK's renderer chosen from the host GPU's kernel driver (llvmpipe for `v3d`, with the reason in `app.conf`); 7-Zip reinstalled with no manual step. An installer that fails without creating a menu entry (or is cancelled) leaves nothing behind (tried with a Windows program that exits with 3). The runtime components and the messages, first part of this item, are the next one (split on 2026-10-14)
- [x] Runtime components of Windows programs when a program needs them, and a clear message when an installed program fails (split from the item above on 2026-10-14; rewritten on 2026-10-14 to say what was measured). Messages: `armor run` waits for the program and explains in one line, on the terminal or in a desktop notification when started from the menu, a missing DLL, a crash, a fatal signal or armor's own stop, with the program's messages in the application's `last-run.log` (tried with test programs from the menu). Visual C++: Wine already has the 2015–2022 runtime except MFC; when a program cannot start for a library of that runtime, armor installs Microsoft's redistributable 14.44.35211 (x64 and x86, pinned) in its prefix and opens it again (verified with a test program that imports `mfc140u.dll`; no real program tried so far needed it: `tech/engines.md`). .NET Framework: Wine Mono, already installed in every prefix, runs KeePass 2.61.1 (WinForms); Microsoft's .NET Framework is not installed (WPF programs not tried). Not covered: a program that keeps running while it fails (Epic's crash reports)
- [x] Real Windows programs, measured in the matrix: a list of common applications (2026-10-14: Notepad++ installs with no manual step and opens from the menu; 7-Zip from `.exe` and `.msi`). The Epic Games Launcher, first part of this item, moved with Spotify to M8 (Chromium-based applications, ADR-0037)
- [x] x86-64 `.deb` with verified Debian dependencies, menu entry and `armor remove` (first case: Spotify, whose repository publishes only `binary-amd64`); the dependency computation, today a trial script, goes into armor with pinned indexes. 2026-10-14: `armor install file.deb` (`debs.rs`) resolves `Pre-Depends`/`Depends` against the host distribution's amd64 archive, verified as apt does (`InRelease` with `gpgv`, then SHA-256 of the index and of every package; the index is the live one, cached for a day, not pinned), unpacks without root or maintainer scripts into the application's root and creates the menu entries with icons. Spotify 1.3.4.258 installs with 176 trixie packages (49 s, 10 s with the cache) and starts, but stops when its renderer processes exit (`COMPATIBILITY_MATRIX.md`, level 1; Spotify usable is now in M8). Later on 2026-10-14: `dash`, `bash` and `coreutils` always added; the libraries the package's own files need (`DT_NEEDED`) found in the archive's verified `Contents` index; XnView MP 1.12.2 (a Qt application) installs with 260 packages and opens from the menu. The indexes are the live ones verified with the archive's signature, as apt does, not pinned (ADR-0036)
- [x] x86-64 AppImages and `.tar.*` archives (2026-10-14: `armor install` recognises AppImages of type 2, installs the system libraries they expect from the verified Debian archive and runs their own runtime under armor to unpack them; the unpacking first stopped after 11 files because armor's `read` returned at most 64 KiB, now fixed. KeePassXC 2.7.12 (AppImage) and Sublime Text 4200 (`.tar.xz`, unpacked into `/opt/sublime_text`) install and open from the menu; the 7-Zip `.tar.xz` installs as a command-line program, without a menu entry)
- [x] Double click and interface: MIME types of `.deb`, `.AppImage`, `.exe`, `.msi` associated with `armor install`; then a minimal window (installed applications, install, remove). 2026-10-14: the package's entry "Install with armor" is the desktop's handler for `.exe` (`application/vnd.microsoft.portable-executable`) and `.msi` (`application/x-msi`): a double click opens a terminal window with the progress and the installer's own window (tried with `gio open` on 7-Zip's `.msi`). The window `armor-gui` (GTK 4, ADR-0035), from the menu entry "armor": the installed applications with buttons for their menu entries, Remove with a confirmation, Install (file chooser), and a notice with a button when the Windows engine is missing; it runs the `armor` command (`armor list --porcelain`). Later on 2026-10-14: the entry also handles `.deb` (`application/vnd.debian.binary-package`) and AppImages (`application/vnd.appimage`, `application/x-iso9660-appimage`), and the window's file chooser offers them; with the package installed, a double click (`gio open`) on XnView MP's `.deb` installs it in a terminal window; a double click on KeePassXC's AppImage installs it too
- [x] armor installs without compiling it: release binaries and an arm64 `.deb` package, with three-line instructions (2026-10-14: `cargo xtask package` builds `armor_<version>_arm64.deb`, about 1 MB: `/usr/bin/armor`, the window `/usr/bin/armor-gui` with the menu entry "armor", the "Install with armor" entry, the licenses; installed with `apt install ./armor_…deb` on the Pi. The first public release, `v0.1.0` (2026-10-14), carries the package; the README's installation is three commands)

**Criterion:** on a Raspberry Pi 5 with a freshly installed system, someone who does not know the details installs armor, then with a double click a real x86 `.exe`, `.msi`, `.deb` and AppImage, and opens them from the menu; each case is in the compatibility matrix.

**Completed on 2026-10-14.** The criterion was checked, including the double click on `.exe` files. Measured here as well: armor's package from the release installed with apt; a double click (`gio open`) installs a `.msi` (7-Zip), a `.deb` (XnView MP) and an AppImage (KeePassXC); all open from the menu and are in the matrix.

## M8 — Games and Steam (priority 2)
x86 Linux games run under armor with the host's GPU and audio; Windows games with ARM64 Proton (Steam) or with `armor install`.
- [x] Valve's native ARM64 Steam client on the Pi 5: it updates itself, the user signs in and the library opens (2026-10-14; requires 4K pages, the Pi boots its 4K kernel)
- [x] First x86 Linux game under armor: Doki Doki Literature Club (Ren'Py) reaches the first scene on the Pi's desktop (2026-10-14; without audio, software rendering)
- [x] Steam installed by armor (`armor install` of the ARM64 client with the host libraries it asks for and the launcher), not by hand (2026-10-15, ADR-0038: `armor install steam` or "Install Steam" in the window fetches the client's packages from Valve's manifest, checked by SHA-256; the host libraries come with armor's package as recommendations, else a message with the `apt` line; `armor run Steam` restarts the client after its update; tried up to the sign-in window, `tech/steam.md`)
- [x] Real graphics and audio for x86 Linux programs under armor: direct calls to the host's native libraries (OpenGL/Vulkan, SDL, X11, audio) instead of the x86 copies in software (first case: Doki Doki Literature Club with audio; its x86 library root pinned with hashes like `fetch-rootfs`) (2026-10-15, ADR-0041: with `--host-gl`, or `host-gl = yes` in a profile or `app.conf`, OpenGL and GLX go to the host's libGL through armor's x86-64 `libGL.so.1` of stubs generated from Khronos' `gl.xml` (2965 functions); `cargo xtask gl-check` gives the same pixels under qemu, armor and `armor --host-gl`; Doki Doki Literature Club draws on the Pi's GPU (V3D 7.1) with its music and is playable through its first scene; its root is `cargo xtask fetch-rootfs --games`, 84 pinned packages. Sound goes through the x86 PulseAudio client to the host's PipeWire socket; X11 and SDL stay the x86 copies, which talk to the X server over its socket. Not yet: Vulkan, EGL, the 20 OpenGL functions that map driver memory or take callbacks, and `--host-gl` as the default)
- [x] A simple Windows game with Steam's ARM64 Proton, with the limits of the Pi's GPU measured (v3dv) (2026-10-15: Doki Doki Literature Club's Windows build with "Proton Experimental (ARM64)", in Valve's ARM64 Steam Linux Runtime 4 container, from the Steam installed by armor: it opens and draws on the Pi's GPU (Ren'Py reports Broadcom V3D 7.1, OpenGL 3.1, Mesa 26.2.2), and its sound reaches PipeWire. GPU limits: OpenGL 3.1 on V3D 7.1; Vulkan (v3dv) lacks `multiDrawIndirect`, so DXVK 2.x, which Direct3D 10/11 games need, does not accept it (measured with the Epic Games Launcher, `tech/engines.md`); Direct3D games are still to try)
- [x] Windows games without Steam with `armor install` (stores like Epic or GOG), after M7 (2026-10-15: `armor install` runs Windows installers with Steam's ARM64 Proton when Steam is installed, ADR-0040: Notepad++ and 7-Zip install and open with it. Then a game from itch.io: Friday Night Funkin' for Windows, a portable zip, installed with `armor install funkin-windows-64bit.zip` (zips are new: unpacked into the prefix, a menu entry per program with its icon) and played from its menu entry with sound, after armor learned to report OpenGL 3.3 on the Pi's GPU (`MESA_GL_VERSION_OVERRIDE`, written with its reason). Epic needs its launcher's sign-in, the Chromium item below; GOG's installers need an account, not tried)
- [ ] Chromium-based applications (moved from M7 on 2026-10-14, ADR-0037): the Epic Games Launcher up to sign-in (2026-10-14: it installs, updates itself and opens the sign-in window, but the login page is not drawn: the Chromium process that draws it terminates under FEX) and Spotify usable (2026-10-14: it installs and starts; its renderer processes exit with status 0, then the main process stops on purpose)

**Criterion:** an x86 Linux game with GPU and audio and a Windows game with Proton open and are playable on the Pi 5; the GPU limits are measured and written down.

## M9 — Every ARM64 (priority 3)
Where external components do not reach, armor is the engine or the solution.
- [x] 4K guest pages on 16K hosts in the armor engine (ADR-0005, since the first milestones)
- [x] armor's JIT on every ARMv8.0: LSE atomics and `LDAPR` chosen at runtime, with substitutes where they are missing (2026-10-14: verified on the Pi 5 by forcing the baseline, `ARMOR_HOST_FEATURES=baseline`)
- [ ] A matrix of real CPUs: at least a Pi 4 (ARMv8.0 without LSE), the Pi 5 and another CPU, with `difftest`, `--lockstep` and the M7 applications
- [ ] Systems with 16K and 64K pages, where Hangover, FEX and the ARM64 Steam client do not start: measure and choose between a 4K kernel documented per distribution, a 4K micro-VM, armor as the engine behind Wine's WoW64/ARM64EC interface (big work), the i386 Steam client under armor as a fallback (it already updates itself, without a display)
- [ ] Other distributions beyond Debian and Ubuntu (Hangover publishes only for these): Fedora, Arch and similar

**Criterion:** the compatibility matrix shows, for at least three CPUs and two page sizes, what works and with which engine.

## M10 — The reference (priority 5, always active)
Whoever develops or uses software on ARM finds on the project page, for an application or a CPU, what works, with which engine and how to reproduce it.
- [x] Test bench: `difftest` against qemu and against a real x86 CPU (Xeon X5675), JIT lockstep, the same cases under box64 and FEX (`--engine`), `bench` with the box64 and FEX columns (2026-10-14: armor 427/427, FEX 345/397 on 4K pages, box64 218/397 on 16K pages)
- [ ] The compatibility matrix generated from the measurements (application × engine × CPU), not written by hand, and prominent on the project page
- [ ] Performance: FEX and box64 measured on 4K pages; the armor engine closer to FEX on Linux programs (today 2.6× native time on 7-Zip, FEX 1.1×)
- [ ] A guide for developers: how to test an application on ARM with `armor inspect`, `--diagnose`, `difftest` and the matrix

**Criterion:** for each application and CPU of the matrix there is the result, the engine, the date and the command to reproduce it.

## Beyond
Other guest architectures (beyond x86) or hosts (beyond ARM64), AVX and AVX-512: only at the request of real applications.
