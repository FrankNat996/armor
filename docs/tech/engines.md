# x86 engines on ARM: armor, box64, FEX, Hangover, ARM64 Steam client — measurements

ADR-0032 (2026-10-14) plans to use the existing components where they are the best and to measure them all on the same cases: this document holds the measurements (the M10 test bench, `roadmap.md`) and those of the Windows programs (M7, ADR-0033, section "Windows programs"). Every statement is marked ✅ (verified, with how) or ⚠️ (to verify, with how). Host of all measurements: Raspberry Pi 5, Debian 13, kernel `6.18.50+rpt-rpi-2712` with **16K pages** (`environment.md`), except where stated otherwise.

## How to reproduce
The sources live outside the repository, in `~/src`, at the official releases (ADR-0032 §5); they must be pinned by tag and commit like the Debian roots (ADR-0014).

| Component | Source | Tag, commit | License | Build used |
|---|---|---|---|---|
| box64 | `https://github.com/ptitSeb/box64.git` → `~/src/box64` | `v0.4.4`, `2f130fa` | MIT (✅ `LICENSE`) | `cmake -DARM64=1 -DARM_DYNAREC=ON -DBOX32=ON -DCMAKE_BUILD_TYPE=RelWithDebInfo` (generic ARM64, not `RPI5ARM64`), in `~/src/box64/build`; binary `build/box64` |
| FEX | `https://github.com/FEX-Emu/FEX.git` → `~/src/FEX` | `FEX-2610`, `14c9268` | MIT (✅ `LICENSE`) | clang, `-DCMAKE_BUILD_TYPE=Release -DTUNE_CPU=generic -DTUNE_ARCH=armv8-a -DBUILD_TESTING=False -DENABLE_LTO=False`, in `~/src/FEX/build`; binaries in `build/Bin/` (the loader is called `FEX`) |
| Hangover (ARM64X Wine + FEX, Windows programs) | prebuilt release `https://github.com/AndreRH/hangover/releases`, not built here | `hangover-11.16`, file `hangover_11.16_debian13_trixie_arm64.tar` (SHA-256 below) | Wine LGPL-2.1, FEX DLLs and `wowbox64` MIT (✅ the packages' `copyright` files) | extracted by `armor engine fetch` into `~/.local/share/armor/engines/hangover-11.16` (section "Windows programs"; the measurements of 2026-10-14 used a copy extracted by hand, identical) |

✅ Tag and commit with `git describe --tags` and `git rev-parse --short HEAD`; options read from each build's `CMakeCache.txt` (2026-10-14).

**Comparison on the same cases** (`testing.md`, "Other engines on the same cases"):
```bash
ARMOR_BOX64=~/src/box64/build/box64 cargo xtask difftest --engine box64 [names…]
ARMOR_FEX=~/src/FEX/build/Bin/FEX   cargo xtask difftest --engine fex   [names…]
```
The reference is the same as armor's (qemu, or the Xeon with `--oracle`); the case's guest root becomes `BOX64_LD_LIBRARY_PATH` for box64 and `FEX_ROOTFS` for FEX. Cases that run only with `--oracle` are excluded, as for armor.

## box64 v0.4.4
- ✅ **BOX32 is experimental for box64 itself:** the option in `CMakeLists.txt` says `"Set to ON to add Linux 32bits support (experimental, do not use)"` (read on 2026-10-14).
- ⚠️ **Wine with 16K pages:** box64's changelog, under v0.2.8 (LoongArch backend, 16K pages by default), says that games with Wine do not work yet because of the 16K page limit. Not tested here with v0.4.4: to verify, run an x86-64 `wine` under box64 on this host.
- ✅ **i386 is not usable, with 16K or with 4K pages:** an i386 hello under box64 (BOX32) terminates with SIGSEGV on both of the Pi's kernels (16K and, since 2026-10-14, 4K): the problem does not depend on the pages; in the `difftest` cases many dynamic i386 programs stop at load time with `Global Symbol optarg not found, cannot apply R_386_GLOB_DAT` (i386 coreutils) and SIGSEGV (below, results).

### `difftest` results (✅ 2026-10-14)
`ARMOR_BOX64=~/src/box64/build/box64 cargo xtask difftest --engine box64`, Pi 5 with 16K pages, qemu reference (`qemu-x86_64` / `qemu-i386` 10.0.13), limit of 120 s per run. On the same 397 cases armor gives 397/397 with the interpreter, with the JIT and in lockstep.

| Group | box64 | armor |
|---|---|---|
| real x86-64 programs | **143/153**: coreutils 46/51, busybox 46/50, `tools` 32/33, `interpreters` 19/19 | 153/153 |
| x86-64 C test programs | **31/72**: `c/` 8/24, `c-pie/` 8/24, `c-dyn/` 15/24 | 72/72 |
| x86-64 assembly programs | **33/63** | 63/63 |
| i386 (BOX32) | **11/109**: `c32/` 0/18, `c32-pie/` 0/18, `c32-dyn/` 6/18, `coreutils32` 5/50, i386 assembly 0/5 | 109/109 |
| **total** | **218/397** | 397/397 |

x86-64 assembly programs by group: identical `faults` 6, `control` 6, `alu` 6, `basic` 5, `sse` 4, `mov` 4, `string` 1, `stack` 1; different `faults` 10, `sse` 9, `alu` 5, `basic` 3, `x87` 1, `stack` 1, `mmx` 1.

**Kinds of difference** (179 cases, counted from the run's list of failures):
| Difference | Cases |
|---|---|
| box64 terminates with SIGSEGV where qemu exits with 0 (77 i386 cases, 18 x86-64) | 95 |
| different stdout | 49 |
| does not terminate within the 120 s | 11 |
| no SIGSEGV where qemu gives one: write to read-only memory (`write_rodata`, `cmpxchg8b_rodata`), execution of data (`exec_data`, `exec_after_mprotect`), misaligned SSE, misaligned `cmpxchg16b`, `x87/stack` | 7 |
| no SIGFPE on division (`divide_error`, `divide_overflow`) | 2 |
| no SIGTRAP on `int3` | 1 |
| other exit statuses or signals (SIGABRT, also on `hlt`) | 14 |

The differences in the assembly programs fall into: memory protection faults (consistent with 4K guest pages that box64 does not distinguish inside a 16K host page), undefined flags (`alu/*`, `stack/enter_popf`), NaN propagation and MXCSR (`sse/*`), CPUID and auxv (`basic/cpuid`, `basic/auxv`: different by construction, each engine declares its own CPU).

**Reading:** box64 is strong on real x86-64 programs (143/153), partly thanks to the host's native libraries that it calls in place of the x86 ones; its i386 mode is not usable (with 4K pages too: section "On 4K pages") and on 16K pages memory protection faults are not reproduced. armor covers both. ⚠️ On a host with 4K pages box64's results may be different: to be redone there with the same command.

### Speed (✅ 2026-10-14)
`ARMOR_BOX64=~/src/box64/build/box64 cargo xtask bench --box64 --runs 3` (Pi 5, 16K pages; the workloads are those of `tech/performance.md`; an output different from qemu's or a failure is marked in the cell instead of stopping the measurement). Median of 3 runs:

| Workload | armor (JIT) | box64 | qemu | native |
|---|---|---|---|---|
| start-static | 7.0 ms | 21.6 ms | 17.6 ms | 0.3 ms |
| start-dynamic | 14.3 ms | 9.1 ms | 29.2 ms | 0.5 ms |
| integer | 305 ms | 160 ms | 335 ms | 116 ms |
| float | 268 ms | 131 ms | 720 ms | 50.8 ms |
| simd | 221 ms | 87 ms | 358 ms | 32.3 ms |
| syscall | 206 ms | 162 ms | 318 ms | 116 ms |
| memory | 218 ms | 201 ms | 343 ms | 102 ms |
| threads | 253 ms | fails (SIGABRT) | 287 ms | 93 ms |
| python-start | 229 ms | 92 ms | 469 ms | 13.3 ms |
| python-int | 1572 ms | 507 ms | 4935 ms | 223 ms |
| python-float | 1398 ms | 447 ms | 4322 ms | 177 ms |

**Reading:** on computation box64 is 2 to 3 times faster than armor (its JIT is mature; armor stays faster than qemu everywhere and beats box64 only on static startup), but on the workload with 4 threads box64 terminates with SIGABRT. The two measurements together (exactness in `difftest`, speed here) say: for x86-64 Linux programs where box64 works, it is the fastest engine; armor is the one that works where box64 does not (i386 and faults with 16K pages, threads in this workload) and the exactness reference. What follows for armor: its JIT has room for improvement (`tech/performance.md`), and armor will be able to choose the engine per program (M7).

## FEX-2610
- ✅ **FEX requires ARMv8.0 with FP and CRC32:** its `CMakeLists.txt` always adds `+crc` to the architecture (`-march=armv8-a+crc`); the generic build (`TUNE_CPU=generic`, `TUNE_ARCH=armv8-a`) compiles on this host (2026-10-14).
- ✅ **It does not run with 16K pages:** every program terminates immediately with `<jemalloc>: Unsupported system page size` (for example `~/src/FEX/build/Bin/FEX target/guest/c/hello`, rerun on 2026-10-14). `difftest --engine fex` therefore has no results on this host.
- ✅ **Without jemalloc it does not compile:** the configuration with `ENABLE_JEMALLOC_GLIBC_ALLOC=OFF` (`~/src/FEX/build-nojemalloc`) stops with compilation errors (tried on 2026-10-14); upstream does not support it.
- ✅ **On 4K pages it runs:** `difftest` results in the section "On 4K pages" (345/397).

## Valve's native ARM64 Steam client
- ✅ **Manifest:** `steam_client_linuxarm64`, platform `linuxarm64`, version `1791918634`, the same as the x86 client (downloaded on 2026-10-14).
- ✅ **Where the native binaries are:** in the package `bins_linuxarm64_linuxarm64`, directory `steamrtarm64/`: 43 aarch64 ELF files (counted with `file`), among them the `steam` client (~10 MB), `steamwebhelper`, `steamui.so`, `steamclient.so`. The bootstrap package `steam_linuxarm64` still contains the i386 bootstrapper, and `bins_linuxarm64` x86 binaries.
- ✅ **Linked for 4K pages:** `readelf -lW steamrtarm64/steam` shows two `LOAD` segments aligned to `0x1000`; the code ends at `0x949238` and the data starts at `0x94b080`, in the same 16K page (`0x948000`–`0x94c000`) with different permissions.
- ✅ **On the Pi 5's 16K kernel it terminates immediately with SIGSEGV** (run directly, 2026-10-14).
- ⚠️ **Support:** Valve declares Steam for Linux on ARM unsupported outside its own hardware. To be rechecked on Valve's support page before citing it in the README with a link.

## On 4K pages (kernel `rpi-v8`, since 2026-10-14)
The Pi 5 boots `kernel8.img` (`environment.md`): `getconf PAGESIZE` gives 4096 ✅.
- ✅ **39-bit address space:** `CONFIG_ARM64_VA_BITS=39` in `/boot/config-6.18.50+rpt-rpi-v8` (47 in the 16K `rpi-2712` kernel); a process's stack is around `0x7f_fc00_0000`. A process therefore has 512 GiB, while x86-64 gives 128 TiB. armor works (`difftest` 397/397): a PIE goes to `0x5555_5555_4000` if that address exists, otherwise where the kernel puts it (`loader.rs`); the JIT tests that used a fixed address at ~40 TB now stay below 2^39. ⚠️ An x86-64 program that asks for a fixed address beyond 512 GiB fails with ENOMEM: to be measured on real programs (Wine, JITs with tagged pointers).
- ✅ **FEX starts:** `FEX target/guest/c/hello` (with `FEX_ROOTFS=/usr/x86_64-linux-gnu`) and `c32/hello` (with `/usr/i686-linux-gnu`) print `hello from glibc`.

### FEX-2610: `difftest` results (✅ 2026-10-14)
`ARMOR_FEX=~/src/FEX/build/Bin/FEX cargo xtask difftest --engine fex`, the generic FEX-2610 build (above), Pi 5 with 4K pages, qemu reference, on the same 397 cases as box64 and armor (the set from before the cases added for Doki Doki Literature Club, `testing.md`). armor on this kernel: 397/397 with the interpreter, with the JIT and in lockstep.

| Group | FEX (4K) | box64 (16K, above) | armor |
|---|---|---|---|
| real x86-64 programs | **153/153**: coreutils 51/51, busybox 50/50, `tools` 33/33, `interpreters` 19/19 | 143/153 | 153/153 |
| x86-64 C test programs | **51/72**: `c/` 17/24, `c-pie/` 17/24, `c-dyn/` 17/24 | 31/72 | 72/72 |
| x86-64 assembly programs | **47/63** | 33/63 | 63/63 |
| i386 | **94/109**: `coreutils32` 50/50, `c32/` 13/18, `c32-pie/` 13/18, `c32-dyn/` 13/18, i386 assembly 5/5 | 11/109 | 109/109 |
| **total** | **345/397** | 218/397 | 397/397 |

box64 and FEX are measured on different pages: the comparison between the two is indicative. ⚠️ x86-64 box64 on 4K pages has not been redone (`difftest --engine box64` with this kernel).

**The 52 differences are all in the exactness tests**, none in the real programs:
- C programs (x86-64 and i386): x87 (`x87`, `x87_more`), asynchronous signals and signal frames (`signals`, `signals_async`, `signals32`), `privileged`, `writev`, `undeclared_extensions`, `legacy_syscalls`, `events`;
- assembly: `sse/*` 8 (NaN, MXCSR, packed and scalar floating point, `sse3`, `sse41`, `reciprocal`, `state`); `faults/*` 4 (division by zero and overflow without SIGFPE, misaligned SSE without SIGSEGV, misaligned `cmpxchg16b` with SIGBUS instead of SIGSEGV); `alu/bits`, `alu/legacy`, `stack/enter_popf`; `basic/cpuid` (different by construction: each engine declares its own CPU).

| Kind of difference | Cases |
|---|---|
| different stdout | 39 |
| FEX terminates with SIGSEGV where qemu exits with 0 | 9 |
| no SIGFPE on division | 2 |
| other exit statuses or signals | 2 |

**Reading:** on 4K pages FEX runs all the real programs among the cases, x86-64 and i386 (box64 on 16K 143/153 and 11/109): it is the engine already mature for those programs where the pages allow it. Where it does not match qemu, it is in the details that armor reproduces like the hardware (x87, MXCSR and NaN, signals, faults). ⚠️ FEX's speed not yet measured: `ARMOR_FEX=~/src/FEX/build/Bin/FEX cargo xtask bench --fex`.
- ✅ **box64 i386 still crashes:** `box64 target/guest/c32/hello` terminates with SIGSEGV here too.
- ✅ **The native ARM64 Steam client works up to the sign-in screen:** launched directly (`steamrtarm64/steam`, with a HOME and links of its own in a trial directory, removed on 2026-10-15), it downloads the ARM64 update (667 MB in less than a minute), extracts and installs it in about a minute and exits with 42 like the x86 client. On the next launch three host libraries were missing (`libgtk-x11-2.0.so.0`, `libibus-1.0.so.5`, `libpipewire-0.3.so.0`, found with `ldd` on the `steamrtarm64` binaries with `LD_LIBRARY_PATH` on its libraries): installed `libgtk2.0-0t64`, `libibus-1.0-5`, `libpipewire-0.3-0t64`. Then, without a display, it stops on `Unable to open X11 display`; with Xvfb (`:99`, 1280×800) the interface starts (`steamwebhelper`, Chromium) and shows the sign-in screen (captured with `xwd`). Non-blocking warnings: `steamrtarm32/gldriverquery` and `vulkandriverquery` are missing, and so is `steam-runtime-launcher-service` (provided by the runtime that `steam.sh` prepares, skipped here). Signing in requires a Steam account.
- ✅ **On the real desktop the user signs in and the library opens** (2026-10-14; the Pi's Wayland desktop with Xwayland, `environment.md`): launched by a trial script (HOME of the trial installation, `DISPLAY=:0`, and `XAUTHORITY` pointing to the `.Xauthority` of the real home, because HOME changes).

## Valve's FEX layer (since 2026-10-14)
After signing in, the ARM64 client installed by itself, in the library of the trial installation (`steamapps/common/`), three tools (✅ directory listing and files read, 2026-10-14):
- **`FEX-Emu`** (24 MB): `VERSIONS.txt` says `FEX-2607-76-g37265b1`, a Valve build different from the FEX-2610 built here above. Its `toolmanifest.vdf` declares it a compatibility **layer** (`"compatmanager_layer_name" "fex"`, `"filter_exclusive_priority" "2"`, `"commandline" "/fex-compat-tool %verb% --"`). Its `emulator.json` describes an **`emulator_v0`** interface: `argv` and `container_argv` `./usr/bin/FEX`, `main_argv` `./FEXCompatTool`, `server_argv` `./usr/bin/FEXServerManager`, `emulated_architectures` `x86_64-linux-gnu` and `i386-linux-gnu`, `required_architectures` `aarch64-linux-gnu`, `required_libraries` `libc.so.6` and `libstdc++.so.6`, environment `FEX_PORTABLE=1` and `FEX_ROOTFS` empty in the container. It is the official hook through which the Steam runtime runs x86 code: a possible way to put armor into Valve's chain. ⚠️ Not tested: to verify, prepare an `emulator.json` that points to armor and launch an x86 Linux game through the Steam Linux Runtime.
- **`SteamLinuxRuntime_4`** (661 MB), the Steam Linux Runtime 4.0.
- **Proton Experimental**, in two directories: `Proton - Experimental` (1.5 GB, `files/bin/wine` is an x86-64 ELF) and `Proton Experimental (ARM64)` (2.0 GB, `files/bin-arm64/wine` is an aarch64 ELF), verified with `file`.

The client uses Steam's generic compatibility tools mechanism (✅ strings `CCompatManager` and `CompatTool_LauncherService` in `steamclient.so`). In the trial installation's `compatibilitytools.d` there are two tools made by us, `armor` and `fex` (`compatibilitytool.vdf`, `toolmanifest.vdf` with `/run.sh %verb%`, and `run.sh`, which launches the game under armor or FEX with the root `target/guest/rootfs-games`). ⚠️ Not yet used through Steam: the client reads them only on restart; to verify, choose them in the properties of an x86 Linux game and look at their log.

## armor on every ARMv8.0
✅ The JIT chooses at runtime between LSE/`LDAPR` and their ARMv8.0 substitutes (`AT_HWCAP`; `ARMOR_HOST_FEATURES=baseline` to force them): verification in `tech/jit.md`, "Every ARMv8.0". ⚠️ A real CPU without the extensions is missing (Cortex-A53 or A72, for example a Pi 4): to verify, run `cargo xtask difftest` and `--lockstep` on that machine.

## Windows programs: Hangover (ARM64X Wine + FEX), since 2026-10-14
Measurements of 2026-10-14 (ADR-0033, "Measurements"; today an item of M7). Pi 5 with the **4K kernel** (`6.18.50+rpt-rpi-v8`), Wayland desktop with Xwayland (`environment.md`). The measurements were made with a copy of Hangover extracted by hand and with test prefixes (`test`, `test32`) in `target/apps/`, outside the repository and not installed in the system; the copy and the prefixes were removed on 2026-10-14, and today the engine is the one from `armor engine fetch` (`environment.md`).

### Which Wine
- ✅ **Debian's Wine runs only ARM64 Windows programs:** `wine`/`libwine` `10.0~repack-6` arm64 (`apt-get download`, not installed; removed on 2026-10-14): the DLLs in `aarch64-windows` are pure ARM64 (`llvm-readobj-19 --file-headers`: Machine `IMAGE_FILE_MACHINE_ARM64 (0xAA64)`, `CHPEMetadataPointer: 0x0`), `i386-windows` and `x86_64-windows` are empty.
- ✅ Steam's **Proton Experimental (ARM64)** (`experimental-11.0-20261008-arm64`) is built as ARM64X (Machine `0xA64E`, CHPE metadata present; same command), with 832 i386 PE files and 772 x86_64 ones and FEX as `libwow64fex.dll`/`libarm64ecfex.dll` and as `.so`. It can be obtained only through Steam.
- ✅ **Hangover 11.16** (https://github.com/AndreRH/hangover): ARM64X Wine with FEX as the emulator, packages for Debian 13 arm64. Release `hangover-11.16` (2026-08-30), file `hangover_11.16_debian13_trixie_arm64.tar`, 293,099,520 bytes, SHA-256 `b5493f5903ab3c05f78bf4c03b06ac0005eb26f732437748c1592fefa45e861a` (`sha256sum`, equal to the digest published by GitHub). It contains `hangover-wine_11.16~trixie_arm64.deb`, `hangover-libwow64fex`, `hangover-libarm64ecfex`, `hangover-wowbox64` (box64 v0.4.4 as a WoW64 DLL), `dxvk-v2.7.1.tar.gz`. Licenses from the `usr/share/doc/*/copyright` files: Wine LGPL-2.1; `libwow64fex`, `libarm64ecfex` and `wowbox64` MIT. Each `.deb` extracted with `dpkg-deb -x` (without root); `usr/bin/wine --version` gives `wine-11.16 (Hangover)`. `armor engine fetch` performs the same extraction (below, "What follows").
- ✅ **Host libraries:** `ldd` on Hangover's binaries finds missing only `libsane.so.1` (scanners) and `libxkbregistry.so.0` (Wine's Wayland driver); with X11 (Xwayland) they are not needed.

### How the emulator is chosen
✅ Read from the binaries with `strings` and tested:
- **x86-64 (ARM64EC):** `ntdll` uses the variable `HODLL64` (strings `libarm64ecfex.dll`, `xtajit64.dll`); when installing 7-Zip x64, without variables, Wine writes `starting FEX (Hangover 11.16) based libarm64ecfex.dll`.
- **32-bit x86 (WoW64):** `wow64.dll` uses `HODLL`, default `wowbox64.dll` (box64); `HODLL=libwow64fex.dll` chooses FEX.
- A prefix created when the WoW64 emulator DLL did not load has an empty `syswow64`, and 32-bit programs fail with `wine: could not load kernel32.dll, status c0000135` (that is the `test` prefix); created with `HODLL=libwow64fex.dll`, `syswow64` has 855 files (`test32`).
- An emulator DLL added later (wowbox64) reaches the prefix's `system32` only after `wineboot -u`; before that: `err:wow:load_64bit_module failed to load dll c0000135`.
- The first `wineboot` stopped on the Wine Mono installation dialog (`control.exe appwiz.cpl install_mono`) on the desktop; with `WINEDLLOVERRIDES='mscoree,mshtml='` the prefix is created in 19 s, without Mono or Gecko.

### 7-Zip 26.04
✅ Release 26.04 of `ip7z/7zip` on GitHub (2026-10-05): `7z2604-x64.exe`, `7z2604.exe` (32-bit), `7z2604-arm64.exe`, `7z2604-x64.msi`, `7z2604-linux-x64.tar.xz`, `7z2604-linux-arm64.tar.xz`, in `target/apps/downloads/7zip-26.04`; SHA-256 of the six files equal to the published digests.
- ✅ `wine 7z2604-x64.exe /S` installs in 5 s; the installed `7z.exe` is x86-64 (Machine `0x8664`). The 32-bit installer, with `HODLL=libwow64fex.dll`, installs into `Program Files (x86)` (`7z.exe` `0x14C`).
- ✅ **`7zFM.exe` (x64) opens on the Pi's real desktop:** capture with `grim`, a window with the toolbar and the file list.
- ✅ **Byte-for-byte identical archives.** Input: `input.bin` (4,000,000 random bytes from Python's `random.Random(42)`) and `input.txt` (an 18 MB text file, 17,998,039 bytes); options `7z a -t7z -mx=9 -mmt=1 -mtm=off -mtc=off -mta=off`. The Windows builds (x64 under FEX, 32-bit under FEX and under box64, native ARM64) give the same archive, SHA-256 `ca55cbdc2f1cbd9f3987ea85ad21ef60b7d4f2a08a7f558ce68cb1a3127aa67d`, 7,653,232 bytes, which extracts back to the inputs (`cmp`). The Linux `7zz` builds (x64 under every engine and native ARM64) all give `7fd6e4c45a6c21af2b577e98ee7ee7f0e3b378165a5b4d3458f29796fafb57b9`, 7,653,234 bytes: an archive different from the Windows builds' one, the same across engines.
- ✅ **Times** (wall-clock time, a single thread, the second of two runs where two were made; not a median as in `bench`):

| Build | Engine | s |
|---|---|---|
| Windows x64 | Hangover, FEX ARM64EC | 24.3 |
| 32-bit Windows x86 | Hangover, FEX WoW64 (`HODLL=libwow64fex.dll`) | 30.8 |
| 32-bit Windows x86 | Hangover, box64 WoW64 (`HODLL=wowbox64.dll`, box64 v0.4.4 with dynarec) | 28.3 |
| Windows ARM64 | Hangover's Wine, native | 21.9 |
| Linux x64 `7zz` | armor `--jit --root target/guest/rootfs-games` | 55.4 |
| Linux x64 `7zz` | FEX-2610 (`FEX_ROOTFS` = the same root) | 23.5 |
| Linux x64 `7zz` | box64 v0.4.4 | 25.1 |
| Linux x64 `7zz` | `qemu-x86_64` 10.0.13 | 93.9 |
| Linux ARM64 `7zz` | native | 21.0 |

**Reading:** on this compression workload FEX costs about 12% more than native (Linux), box64 19%, armor 2.6 times native (0.6 times qemu's time). Under Wine FEX ARM64EC (24.3 s) is close to the Windows ARM64 build (21.9 s). The test root `target/guest/rootfs` does not have `libstdc++.so.6`, which `7zz` asks for; `rootfs-games` does.

### How to reproduce
```bash
armor engine fetch                                   # Hangover and DXVK in ~/.local/share/armor/engines/
H=~/.local/share/armor/engines/wine/usr
D=$PWD/target/apps/downloads/7zip-26.04
P=$(mktemp -d)                                       # the test prefixes (those of 2026-10-14 were removed)
# 64-bit prefix; without Mono and Gecko
WINEPREFIX=$P/test WINEDLLOVERRIDES='mscoree,mshtml=' $H/bin/wine wineboot
WINEPREFIX=$P/test $H/bin/wine $D/7z2604-x64.exe /S
WINEPREFIX=$P/test $H/bin/wine 'C:\Program Files\7-Zip\7zFM.exe'      # on the desktop (DISPLAY=:0)
# prefix for 32-bit programs: create it with FEX as the WoW64 emulator
HODLL=libwow64fex.dll WINEPREFIX=$P/test32 WINEDLLOVERRIDES='mscoree,mshtml=' $H/bin/wine wineboot
HODLL=libwow64fex.dll WINEPREFIX=$P/test32 $H/bin/wine $D/7z2604.exe /S
# box64 as the WoW64 emulator: first wineboot -u, then HODLL=wowbox64.dll (the default)
HODLL=wowbox64.dll WINEPREFIX=$P/test32 $H/bin/wine wineboot -u
# the archive, with each build (Windows: wine …\7z.exe; Linux: 7zz under the engine)
7z a -t7z -mx=9 -mmt=1 -mtm=off -mtc=off -mta=off <archive>.7z input.bin input.txt
```
With `armor install` (2026-10-14) the same installation of 7-Zip x64 is done with `armor install --name 7zip 7z2604-x64.exe` (or `7z2604-x64.msi`): a prefix in `~/.local/share/armor/apps/7zip`, the entries in the desktop menu, `7zFM.exe` opened from the menu ✅ (`gio launch` of the entry, 2026-10-14).
⚠️ The order of the files on the command line and the exact paths used for each build were not recorded; to verify, redo the archive with the line above and compare the SHA-256 with those in this section. The test scripts are not in the repository.

### On 16K pages (2026-10-15)
Measured in the 16K virtual machine of `testing.md` ("Other page sizes": Debian 13, kernel `6.12.111+deb13-arm64-16k`), with `armor engine fetch` and Xvfb:
- ✅ **Hangover's FEX runs there, unlike FEX for Linux:** Wine warns `You are running with the wrong page size, expect problems!` (and points to Hangover's wiki), then works. FEX inside Wine is a DLL (`libarm64ecfex.dll`, `libwow64fex.dll`) that does not use jemalloc, which is what stops FEX for Linux on 16K pages (above).
- ✅ **7-Zip 26.04 x64:** its installer (`7z2604-x64.exe /S`) installs it; `7z a -t7z -mx=9 -mmt=1` on the same files gives an archive byte-for-byte identical (SHA-256 `5c95bd10…`) to the native Windows ARM64 build's (`7z2604-arm64.exe`, installed in the same prefix), and `7z t` says `Everything is Ok`.
- ✅ **Notepad++ 8.9.8.1:** `armor install npp.8.9.8.1.Installer.x64.exe` (its installer is 32-bit, so through WoW64 and `libwow64fex.dll`) creates the menu entry, and `armor run Notepad++` opens its window (screen capture of the Xvfb display).
- So Windows programs are not limited to 4K pages; Valve's ARM64 Steam client still is (below, "Valve's native ARM64 Steam client").

### Epic Games Launcher (2026-10-14)
Installed with `armor install` (prefix in `~/.local/share/armor/apps/epic`) on the Pi with 4K pages; measured on 2026-10-14.
- ✅ **Installation:** the installer downloaded from Epic (`EpicGamesLauncherInstaller.msi`, that is `EpicInstaller-20.3.3.msi`, SHA-256 in ADR-0033) installs in 39 s; the launcher updates itself from 20.3.3 to 20.3.5 (671.7 MB, after a click on its update button).
- ✅ **Direct3D:** with wined3d on OpenGL the launcher closed immediately (no Direct3D 11 feature level on the Pi's GPU); DXVK 2.7.1 discards the V3DV driver because `multiDrawIndirect` is missing, so the llvmpipe software renderer is needed (`env = DXVK_FILTER_DEVICE_NAME=llvmpipe` in `app.conf`, written by hand).
- ✅ **ARM64EC DXVK versus x64:** 20.3.5 stopped with EXCEPTION_ILLEGAL_INSTRUCTION; with `WINEDEBUG=+seh` the address is at offset 0x6594 of DXVK's ARM64EC `dxgi.dll` with an x86 context (`rip`, `cs=0033`): Epic's x86-64 code jumps directly into ARM64EC code, which FEX decodes as x86. With the x64 build of DXVK (translated by FEX) it gets further: this is why `armor install` copies DXVK's `x64/` and `x32/`.
- ✅ **Steps done by hand:** Epic Online Services was missing, and its MSI asks for .NET (`SFXCA: Failed to load mscoree.dll`); meanwhile Epic's crash handler wrote about 1,700 report folders in a minute. Installed in the prefix Wine Mono 11.3.0 ARM64 (`wine-mono-11.3.0-arm64.msi` from dl.winehq.org, SHA-256 `b0c47d37…120e0f`, equal to the one inside Wine 11.16's `appwiz.cpl`), then `EpicOnlineServicesInstaller.exe /upgrade productid=EpicGamesLauncher minversion=5.3.0 /quiet`: "Success". `app.conf` with `env = WINEDLLOVERRIDES=mshtml=;d3d8,d3d9,d3d10core,d3d11,dxgi=n,b`. These steps become automatic in M7.
- ✅ **Where it stops:** the launcher stays open (90 s observed, no new reports), CEF 90 starts (`EpicWebHelper`) and the sign-in window opens but stays at the skeleton: the process that draws `epicgames.com/id/login` terminates ("Render process terminated (ProcessCrashed)"); with `+seh` a thread fails executing a non-executable address (`c0000005`, `info[0]=8`). ⚠️ That it is the JavaScript JIT (V8) under FEX inside Wine is a hypothesis: to verify, use CEF without JIT or without GPU, a more recent FEX build, or the same launcher under Steam's ARM64 Proton.

### Runtime components and failure messages (2026-10-14)
- ✅ **Wine's own Visual C++ runtime:** Hangover 11.16's prefix has Wine's built-in `msvcp140*.dll`, `vcruntime140*.dll`, `ucrtbase.dll`, `concrt140.dll`, `vcomp140.dll`, `mscoree.dll` and the older `msvcp/msvcr 60–120` in `system32` (listed in the Notepad++ prefix), but **no MFC** (`mfc*.dll`).
- ✅ **Microsoft's redistributable under Hangover:** `VC_redist.x64.exe` 14.44.35211 (25,635,768 bytes, SHA-256 `cc0ff0eb…b713b`, the same as in its versioned URL on `download.visualstudio.microsoft.com`, where `aka.ms/vs/17/release/vc_redist.x64.exe` pointed on 2026-10-14) installs with `/install /quiet /norestart` in a new prefix in 2 s, exit status 0, and adds MFC (`mfc140u.dll` and the others; the files are ARM64 PE, ARM64X as the redistributable installs on an ARM64 Windows) and sets `HKLM\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64` (`Installed` 1, `Version` v14.44.35211.00). `VC_redist.x86.exe` (13,953,392 bytes, `0c09f261…ed64`) installs too.
- ✅ **A program that imports MFC:** a test program built on the Pi with `clang --target=x86_64-pc-windows-msvc` and `lld-link`, importing `mfc140u.dll` (ordinal 256, never called), stops in a prefix without the redistributable (`err:module:import_dll Library mfc140u.dll … not found`, exit status 53) and starts (its exit status, 7) after it, with x64 code under FEX loading the ARM64X MFC. `armor run` does this by itself: it reads the loader's message, fetches the pinned redistributables, installs them in the prefix and opens the program again (4 s with the files in the cache).
- ✅ **No real program so far needed it:** WinDirStat 2.9.3, an MFC program, links MFC statically (its imports: `KERNEL32`, `USER32`, `GDI32`, `ADVAPI32`, `SHELL32`, `ole32`, `Cabinet`, `COMCTL32`, `gdiplus`, `UxTheme`, `dwmapi`, `ntdll`); 7-Zip, Notepad++ and KeePass need only Wine's runtime. ⚠️ A real program that imports MFC or another library of the redistributable without bringing it is still to be found and tried.
- ✅ **.NET Framework:** KeePass 2.61.1 (a .NET Framework WinForms program; installer `KeePass-2.61.1-Setup.exe`, SHA-256 `9923a8b2…d38d` as published on keepass.info) installs with `armor install … -- /VERYSILENT /SUPPRESSMSGBOXES /NORESTART` in 28 s and, opened from its menu entry, runs on Wine Mono 11.3.0 and shows its first-start dialog (screenshot). armor does not install Microsoft's .NET Framework. ⚠️ WPF programs not tried (Wine Mono's WPF is limited).
- ✅ **Messages:** with `WINEDEBUG=-all,err+module` Wine prints the loader's missing-library errors; a crash prints `wine: Unhandled page fault on write access to … at address …` and opens Wine's own "Program Error" dialog; `armor run` turns these into one line on standard error and, when started from the menu, a desktop notification (`gdbus` call to `org.freedesktop.Notifications`, shown by the Pi's panel), and keeps the program's messages in `last-run.log` (tried with the test programs above and with the menu entry, 2026-10-14).

### A game from itch.io with Steam's ARM64 Proton (2026-10-15)
- ✅ **Download without an account:** for a free game, the itch.io page's own requests (`POST /<game>/download_url`, then `POST /<game>/file/<upload id>?source=game_download&after_download_lightbox=1&as_props=1` with the page's `csrf_token`) give a signed URL valid for 60 s. Friday Night Funkin' (`ninja-muffin24.itch.io/funkin`): `funkin-windows-64bit.zip`, 838,984,632 bytes, SHA-256 `3b6e4d82951c7d122bccec0a0d226fc4559ccb5437192272d0dc0f6a237dfe1a`; a portable game (`Funkin.exe`, x86-64, GUI subsystem; libVLC; 800 MB of assets).
- ✅ **Installed by `armor install` of the zip** in 22 s, with Steam's "Proton Experimental (ARM64)": prefix made by `wineboot`, the game in `C:\Games\Funkin`, one menu entry with the program's icon.
- ✅ **The Pi's OpenGL version:** started as it is, the game shows "GLSL 1.50 is not supported. Supported versions are: 1.10, 1.20, 1.30, 1.40, 1.00 ES, and 3.00 ES" for its first vertex shader (Wine's OpenGL is the host's Mesa 26.2.2, v3d, OpenGL 3.1). With `MESA_GL_VERSION_OVERRIDE=3.3` and `MESA_GLSL_VERSION_OVERRIDE=330` it compiles its shaders and draws the title screen, the menus and a song correctly (screenshots); armor now writes the two variables, with the reason, for every Windows application on a host whose GPUs are all `v3d`.
- ✅ **Started through Wine's `start.exe /wait`**, a console window opened next to the game, with the game's log in it (`start.exe` is a console program, the game a GUI one): `armor run` now starts a program given by its path directly, in its folder.
- ✅ **Without Steam (Hangover):** in a separate data directory (`XDG_DATA_HOME`, the same engines), so that Steam's Proton is not found, `armor install --name SevenExe 7z2604-x64.exe -- /S` (30 s, entries from Wine's shortcuts) and `armor install` of a zip of 7-Zip's installed files (31 s, entries `7zFM` and `7zG` only: `7z.exe` is a console program) both work, and `7zFM` opens from its entry (screenshot).
- ✅ **Played from the menu entry** (`gio launch` of armor's `.desktop` file): title, main menu, Free Play, the tutorial song with its notes and music (peak 28514 of 32767 recorded from the sink's monitor), keyboard input; about 40% CPU and 810 MB for `Funkin.exe`, observed with `top` during the song.

### Epic Games Launcher with Steam's ARM64 Proton (2026-10-15)
- ✅ `armor install --name Epic-Proton EpicInstaller-20.3.3.msi -- /qn` (the same installer, SHA-256 `d55d7971…0337`) installs it with "Proton Experimental (ARM64)" in 1 min 54 s; opened from its menu entry, it updates itself (`LogSelfUpdateService: Installer complete. Success:1`), then restarts through its `EpicGamesUpdater` service, which reaches `SERVICE_RUNNING` and starts nothing more.
- ✅ **Direct3D under Proton on the Pi:** Proton's DXVK (v3.1.1-67) lists "V3D 7.1.7.0 (V3DV Mesa 26.2.2)" ("Skipping: Device does not support required feature 'multiDrawIndirect'") and llvmpipe ("Skipping: Software driver"), then "No adapters found": unlike DXVK 2.7.1 under Hangover, it does not take llvmpipe, so armor's `DXVK_FILTER_DEVICE_NAME=llvmpipe` has no effect under Proton. ⚠️ `PROTON_USE_WINED3D=1` (Direct3D on OpenGL) is the candidate for Direct3D programs on the Pi: to measure on a Direct3D game before armor sets it.
- ✅ The updated launcher started directly (`armor run Epic-Proton 'C:\…\EpicGamesLauncher.exe'`) writes no log; with `PROTON_USE_WINED3D=1` it starts its Slate Direct3D renderer and stops with `EXCEPTION_ACCESS_VIOLATION reading address 0x0000000000000000` inside `EpicGamesLauncher.exe` (its own log). Not followed further: it is inside Epic's code and Wine, outside armor.

## What follows
- **With the 4K kernel** (since 2026-10-14) FEX and Valve's native client start (section "On 4K pages"): FEX is identical to qemu on all the real programs among the cases, the ARM64 client signs in and installs by itself the FEX layer, the Steam Linux Runtime and Proton. On a 4K system Valve's chain therefore already covers the Steam client and its tools ✅; games through Valve's FEX layer or ARM64 Proton ⚠️ have not been run yet (M8). The price is that kernel's 39-bit address space.
- **16K pages are the main gap.** With the Pi's 16K kernel Valve's native client and FEX do not start, and box64 handles neither i386 nor protection faults; armor runs (ADR-0005). ARM64 Proton depends on FEX, so it too needs 4K pages ⚠️ (to verify: ARM64 Proton has not been run yet, neither on 4K nor on 16K; M8). The way for 16K systems is to be chosen in M9.
- **Options for 4K pages on the Pi** (chosen on 2026-10-14: the first, the second only if really necessary):
  1. **4K kernel on the Pi:** done. ✅ `kernel8.img` is selected in `config.txt` since 2026-10-14 (`environment.md`); it applies to the whole system;
  2. **microVM with a 4K kernel via KVM:** ✅ `/dev/kvm` exists on this host. Only the x86 part would run with 4K pages, the rest of the system would stay at 16K. ⚠️ It is the solution used on other 16K hosts (for example Apple Silicon with Asahi): to be verified on their documentation before citing it as a precedent. ⚠️ Costs (graphics and audio towards the guest, startup) to be measured with a prototype.
- **Windows programs (2026-10-14):** ✅ Hangover 11.16 (ARM64X Wine with FEX) runs 7-Zip for Windows x64 and 32-bit on the Pi with 4K pages, with archives identical to those of the Windows ARM64 build, without Steam; Debian's Wine is not enough (section "Windows programs"). Hangover became the component for Windows on 2026-10-14 (ADR-0033, "Consequences"); ✅ `armor engine fetch` downloads it (build for the host's distribution, pinned SHA-256) and extracts it into `$XDG_DATA_HOME/armor/engines/`: tested on 2026-10-14 in an empty folder, 1 minute, files identical to the extraction done by hand (`diff -rq`). ⚠️ Not tested with 16K pages: to verify, redo the commands of the section with the `rpi-2712` kernel (FEX does not start there, so a failure is expected).
- **armor remains the engine for what the others do not cover** (16K, where FEX and Valve's client do not start; i386 without armhf; ARMv8.0 CPUs), the exactness reference (397/397 against FEX's 345) and the test bench that measures all the others. Doki Doki Literature Club, the first x86 game tested, runs under armor (`tech/steam.md`).
