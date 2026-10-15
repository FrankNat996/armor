# Analysis: what the goal needs

Written on 2026-10-14 with ADR-0034, which reordered the priorities. It starts from measured facts (the sources are in the documents cited) and says, for each milestone, what is missing, in what order and with what order of magnitude of work. The durations are estimates, not commitments. It replaces the initial audit of 2026-10-12, tied to the milestones of that time (the text remains in the repository history).

## 1. The goal, in one sentence
A single application that makes it simple for anyone with an ARM64 computer to use any x86 software — Linux programs, Windows programs and games — without knowing what happens underneath; and a measured reference for development on ARM.

## 2. Where we are (2026-10-14)
| Layer | What exists, measured | What is missing |
|---|---|---|
| Engine for x86 Linux programs (armor) | x86-64 and i386, static and dynamic, threads, signals, exact x87, SSE up to 4.2, JIT with the interpreter as backup; `difftest` 427/427 identical to qemu, 236/236 identical to a real Xeon; 203 real programs (coreutils, busybox, tools, interpreters); first game (Doki Doki Literature Club) up to the first scene (`COMPATIBILITY_MATRIX.md`) | performance (7-Zip Linux: 2.6× native, FEX 1.1×), real graphics and audio (today the x86 libraries in software), AVX |
| Windows programs | `armor install`/`run`/`list`/`remove` and `armor engine fetch` (Hangover 11.16: Wine ARM64X with FEX; DXVK 2.7.1); 7-Zip x64 from `.exe` and `.msi`, archives identical to the native ARM64 build (`tech/engines.md`) | automatic steps (Wine Mono, runtime components, renderer choice), real applications beyond 7-Zip; Epic stops at the sign-in page (later on 2026-10-14: Wine Mono and the renderer choice automatic, Notepad++ installs and opens; runtime components still missing) |
| Linux applications (added on 2026-10-14) | `armor install` of x86-64 `.deb`, AppImages and `.tar.*` archives with the Debian libraries they need, verified: XnView MP, KeePassXC and Sublime Text open from the menu | Chromium-based applications (Spotify stops when its renderers exit); Qt's OpenGL probe hangs under armor (software drawing for now); libraries shared between applications on disk |
| Steam and games | Valve's ARM64 Steam client on the Pi, with sign-in; Proton Experimental (ARM64) and the FEX layer installed by the client, not yet used | Steam installed by armor, a Windows game with Proton, x86 Linux games with GPU and audio |
| CPUs and systems | Pi 5 with the 4K kernel; armor's JIT also verified in baseline ARMv8.0 mode; armor engine also on 16K pages | no second CPU measured; on 16K pages Hangover, FEX and the ARM64 Steam client do not start; only Debian and Ubuntu |
| Reference | `difftest` against qemu, against the Xeon, `--lockstep`, `--engine box64|fex`, `bench` | matrix generated from the measurements, a readable page for developers |
| armor itself | it builds with Rust from source; since 2026-10-14 an arm64 `.deb` in the repository's releases (`v0.1.0`, then `v0.2.0`) | packages for other distributions |

## 3. What to do, in order of priority

### M7 — The installer for everyone (the priority)
1. **armor installable without compiling it** (days). Release binaries for aarch64 and an arm64 `.deb` package that carries `armor` and the menu entry; `armor engine fetch` done at the first installation of a Windows program. Without this, "for everyone" is not true. Done on 2026-10-14: the repository is public and release `v0.1.0` carries `armor_0.1.0_arm64.deb`.
2. **No manual steps** (weeks). Generalise what was done by hand for Epic:
   - Wine Mono installed when a program's installer asks for .NET (`mscoree`), downloaded and verified with the hash that Wine carries in `appwiz.cpl` (done by hand on 2026-10-14);
   - the GPU recognised: today DXVK rejects the Pi's V3DV driver (`multiDrawIndirect` is missing) and the llvmpipe software renderer is needed, written by hand in `app.conf`; armor must try the host's Vulkan driver and choose on its own;
   - common runtime components of Windows programs (Visual C++ and similar) when an installer asks for them;
   - knowing when a program ends badly and saying so with an understandable message (today Epic wrote 1,700 crash reports per minute without the user seeing it).
   Done on 2026-10-14: Wine Mono and the renderer automatic; `armor run` explains a missing DLL, a crash or a fatal signal in a desktop notification; the Visual C++ runtime (MFC, the part Wine lacks) installed when a program needs it, verified with a test program (no real program tried needed it); KeePass 2 (.NET Framework) runs on Wine Mono. Not covered: a program that keeps running while it fails (Epic's crash reports).
3. **Real Windows programs** (weeks, ongoing). A list of 5–10 common applications, each in the matrix with its level. The Epic Games Launcher is on the list but does not block: its sign-in page (Chromium 90 inside the launcher) terminates under FEX with an execution on non-executable memory; it is a problem between Hangover, FEX and CEF, to report to the projects or to try again with new versions, not to solve in armor. Since 2026-10-14 the Chromium-based applications (Epic, Spotify) are in M8 (ADR-0037).
4. **x86-64 `.deb`** (weeks). Done on 2026-10-14 in armor (`debs.rs`): dependencies resolved against the host distribution's amd64 archive, the signed `InRelease` checked with `gpgv`, every package checked against the index's SHA-256, a shared download cache, one root per application. Missing: sharing the common libraries between roots on disk, and Spotify actually running (its renderer processes exit). Later on 2026-10-14: the libraries a package's own files need (`DT_NEEDED`) are found in the archive's `Contents` index; XnView MP installs and opens.
5. **AppImage and archives** (days). A type 2 AppImage is a squashfs file system: extract it and launch its `AppRun` under armor. Done on 2026-10-14: the AppImage's own runtime extracts it under armor (KeePassXC opens); `.tar.*` archives are unpacked into `/opt` of the application's root (Sublime Text opens).
6. **Double click and interface** (weeks). MIME types associated with `armor install`; then a minimal window. The choice of toolkit (GTK, a local web interface, …) is to be made with an ADR. Done on 2026-10-14: `armor-gui` in GTK 4 (ADR-0035); the double click covers `.exe`, `.msi`, `.deb` and AppImages.

### M8 — Games and Steam
Completed on 2026-10-15 (`roadmap.md`): items 1–3 below are done (`armor install steam`; Doki Doki Literature Club with Proton ARM64 and the GPU limits measured; OpenGL through the host's libGL, ADR-0041); item 4 is still open.
1. **Steam installed by armor** (days): Valve's ARM64 client with the three host libraries it asks for (GTK2, IBus, PipeWire) and the launcher, done by hand today.
2. **A Windows game with Proton ARM64** (days–weeks): Proton is already installed by the client; it needs to be tried and the limits of the Pi's GPU measured (v3dv, without `multiDrawIndirect`, which DXVK 2.x asks for).
3. **Real graphics and audio for x86 Linux programs under armor** (months). Today an x86 Linux game uses the x86 libraries (Mesa, SDL, ALSA) translated, hence software rendering and no GPU driver. The solution is to call the host's native libraries from the x86 functions (OpenGL/Vulkan, SDL, X11, audio), with the ABI verified. It is the largest piece of work of M8 and also the one that helps performance most.
4. **Chromium-based applications** (weeks, unknown; moved from M7 on 2026-10-14, ADR-0037; still open after M8 closed on 2026-10-15, `roadmap.md`): the Epic Games Launcher up to sign-in (its CEF renderer terminates under FEX) and Spotify (its renderer processes exit under armor). Partly in external components: measure, report, retry with new versions.

### M9 — Every ARM64
Measured on 2026-10-15 (`testing.md`, "Other page sizes" and "Other distributions"):
1. **A second and a third CPU:** GitHub's ARM64 Linux runner as the second real CPU, running the chain at every push (`.github/workflows/arm64.yml`); the ARMv8.0 paths of the JIT checked by forcing them (`ARMOR_HOST_FEATURES=baseline`) on every host. A real ARMv8.0 CPU without LSE (a Pi 4) is still to try.
2. **16K and 64K pages:** armor's engine runs on both (virtual machines on the Pi: every `difftest` case as on 4K, after two fixes); Hangover runs Windows programs on both (7-Zip x64 identical to its ARM64 build, Notepad++ on 16K); Valve's ARM64 Steam client needs 4K pages. ADR-0042: no new engine; Steam on a 4K kernel, a 4K micro-VM where the hardware has no 4K kernel.
3. **Other distributions:** done with ADR-0043: Debian's archive for Linux applications and Hangover's Debian 13 build on any distribution, armor as a `.tar.gz` with `install.sh`; `cargo xtask distro-check` passes on Fedora 42 and Arch Linux ARM.

### M10 — The reference (always active)
1. **Matrix generated from the measurements** (days): every row (application, engine, CPU, pages, date, command) produced by `difftest`, `bench` and the application tests, not written by hand.
2. **Performance of the armor engine** (months, ongoing): profiling of real programs with `perf` (`tech/performance.md`); the native calls of M8 count more than any JIT optimisation.
3. **Guide for developers** (days).

## 4. Risks
- **External components:** Hangover, FEX, DXVK and the ARM64 Steam client have their own bugs (Epic) and their own versions. They are pinned by hash, measured at every update, and nothing is declared that has not been proved.
- **16K pages:** the external components do not run there; until M9 chooses a route, on those systems only x86 Linux programs under armor work.
- **GPU:** graphics depend on the host's Vulkan driver. On the Pi 5 the V3DV driver is not enough for DXVK 2.x: Windows programs with Direct3D use the software renderer (slow).
- **Disk space:** the Windows engine takes 2.4 GB; every Wine prefix starts at about 2.3 GB (7-Zip installed), Epic's after the update takes 5.1 (measured with `du` on 2026-10-14). It must be shown to the user before installing, and the prefixes should share Wine's files instead of copying them.
- **Licenses:** Wine LGPL-2.1, FEX and box64 MIT, DXVK zlib; armor redistributes nothing, it downloads them from the source. GPL code (qemu) stays excluded from armor.
- **Security:** installers are untrusted code; today they run without isolation. `--sandbox` (ADR-0027) exists for Linux programs; for Wine it must be evaluated.

## 5. What was removed on 2026-10-14 and what was not
- **Removed:** the previous milestones M7–M10 (replaced, with the correspondence table in `roadmap.md`); the initial audit (`PROJECT_AUDIT.md`, replaced by this document); the superseded proposals of `tech/steam.md` (M9 stages rewritten on 2026-10-14, inventory of `steam.deb` for the bootstrap under armor); ADR-0031 marked "Superseded by 0034"; on the development machine, the prefixes and the test files outside the repository.
- **Kept, and why:** all the code and all the tests (they are the engine and the reference; no test is disabled); the ADRs (they are not deleted, they are superseded); i386 support (it serves "any x86 program"); `difftest --engine` and `bench` (they are M10).
