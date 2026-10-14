# 0033 — `armor install`: installing x86 Linux and Windows applications on ARM64

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- Wider context: ADR-0032 (armor as glue), ADR-0028 (per-application profiles), ADR-0014 and ADR-0031 (roots from pinned Debian packages), ADR-0027 (`--sandbox`).

## Context
On 2026-10-14

Today an x86 program is run with `armor [--root DIR] program`: one has to obtain by hand the x86 libraries it needs, a guest root and a way to launch it from the desktop. A Windows program needs Steam (Proton) as an intermediary.

Facts verified on 2026-10-14 on this Pi 5:
- the experimental root `target/guest/rootfs-games` (the test root plus 77 Debian amd64 packages, dependency closure computed from the index verified against `Release`, every package checked with SHA-256) is enough to run under armor the first real x86-64 game, Doki Doki Literature Club (`tech/steam.md`): the same mechanism can install an x86 `.deb` with its dependencies;
- the ARM64 Steam client installed "Proton Experimental (ARM64)" (`version`: `experimental-11.0-20261008-arm64`): native AArch64 Wine (`lib/wine/aarch64-unix`, `aarch64-windows`) with `libwow64fex.dll`, `libarm64ecfex.dll` and `xtajit64.dll` in `aarch64-windows`, that is FEX inside Wine through WoW64 (32-bit Windows programs) and ARM64EC (64-bit). It can be downloaded only through Steam;
- Debian trixie has `wine` 10.0 (`10.0~repack-6`) for arm64 (`apt-cache show`); it does not contain the FEX DLLs.

## Decision
armor also becomes an **application installer**, as an extension of point 2 of ADR-0032 (a single command that recognises what is being launched and chooses the engine).

1. **Commands:**
   - `armor install <file>`: recognises `.deb` (amd64, i386), AppImage, `.tar.*`/`.zip` archives with an x86 ELF, Windows `.exe` and `.msi`;
   - `armor list`, `armor run <app>`, `armor remove <app>`.
2. **One root per application, without privileges:** every application lives in `~/.local/share/armor/apps/<name>/` with its own guest root; the dependencies of a `.deb` come from the pinned Debian packages (index verified against `Release`, SHA-256 of every package, only extracted, as in ADR-0014 and ADR-0031); common libraries are shared between roots (links or a read-only base root). The host system is not touched: no host `dpkg`, no root.
3. **Desktop integration:** `armor install` writes a `.desktop` entry (name, icon, MIME types of the package) in `~/.local/share/applications` that launches `armor run <app>`; `armor remove` removes it together with the root.
4. **Per-application engine** (profiles of ADR-0028):
   - x86-64 and i386 Linux programs: armor (JIT); FEX as an alternative in the profile, where measured better;
   - Windows programs: native ARM64 Wine with FEX as x86 translator through WoW64/ARM64EC. One Wine prefix per application; the `.exe`/`.msi` installer runs there; the menu entries that Wine creates are rewritten to go through `armor run`. Where to get ARM64 Wine and the FEX DLLs is the first thing to measure (point 6).
5. **Security:** downloaded installers are untrusted code. `armor install --sandbox` (ADR-0027) runs scripts and installers isolated; the maintainer scripts of `.deb`s (`postinst`, …) are not run as root: they are read, and only the necessary ones are run under armor in the application's root.
6. **Stages** (milestone M10 in `roadmap.md`). First real cases (2026-10-14):
   - **7-Zip x64** for Windows: small, with a graphical interface, free (LGPL), x64 installers in `.exe` and in `.msi` (7-Zip download page, read on 2026-10-14), and an official ARM64 build to compare the results with (an archive created under FEX and one created natively);
   - then the **Epic Games Launcher**, a frequent request (x64 `.msi` installer, it does not exist for Linux);
   - **Spotify** for `.deb`s: its Debian repository publishes only amd64 (✅ 2026-10-14: `dists/testing/non-free/binary-amd64/Packages` exists, `binary-arm64/Packages` answers 404); it requires an account (a free one is enough).
   1. ARM64 Wine + FEX without Steam: 7-Zip x64 opens;
   2. `armor install` of an `.msi`/`.exe` in a per-application Wine prefix, with the menu entries: first the 7-Zip installers, then the Epic Games Launcher (it installs, it opens from the menu, the user signs in);
   3. a real x86-64 `.deb` that exists only for amd64, with its Debian dependencies, menu entry and `remove`;
   4. an x86-64 AppImage and a `.tar.*` archive (extracted, launched under armor);
   5. opening with a double click: MIME types for `.deb`, `.AppImage`, `.exe`, `.msi` associated with `armor install`; then a graphical interface.

To verify before building (⚠️):
- whether Debian's Wine 10.0 loads `libwow64fex.dll`/`libarm64ecfex.dll` without Proton's changes (in Wine the WoW64 translator is chosen with a registry key under `HKLM\Software\Microsoft\Wow64\x86`, default `xtajit.dll`): to be verified by installing `wine` and the DLLs built from FEX's sources (MIT) and launching an x86 `.exe`;
  ✅ 2026-10-14, answer **no** for x86 programs: Debian's package has no x86 or ARM64X DLLs ("Measurements", point 1). In Hangover the emulator is chosen with the environment variables `HODLL` (WoW64) and `HODLL64` (ARM64EC) (point 3);
- the Epic Games Launcher under Wine: it is a Chromium (CEF) application, 64-bit (✅ 2026-10-14: the installer downloaded from Epic is `EpicInstaller-20.3.3.msi`, 264 MB, and its MSI summary says `Template: x64`, read with `file`), so it goes through FEX's ARM64EC path and not through WoW64; if it does not open, where it stops is measured (and the same installer is tried under x86 Wine on an x86 machine as a reference);
- for x86-64 programs (7-Zip x64, Epic) Wine must be built with ARM64EC support (system DLLs in ARM64X format) and load `libarm64ecfex.dll` as emulator: to be verified in Debian's package (format of the DLLs in `aarch64-windows`) before counting on it;
  ✅ 2026-10-14: in Debian's package the DLLs are pure ARM64, without ARM64EC ("Measurements", point 1); Hangover 11.16 has the support and loads `libarm64ecfex.dll` (points 2 and 4);
- alternatively, using the Proton ARM64 already present in the Steam library when it is there, without redistributing it;
  ✅ 2026-10-14 in part: Proton Experimental (ARM64) is built ARM64X with FEX inside ("Measurements", point 1), so it remains a possible alternative; not yet tried outside Steam;
- the AppImage of stage 4: it must exist only for x86 and have a license that allows installing it this way (to be chosen and checked on its site).

## Alternatives considered
- **Host multiarch and `binfmt_misc`** (`dpkg --add-architecture amd64`, `apt install package:amd64`, armor registered as the interpreter of x86 ELFs): transparent and already known to users, but it requires root, mixes x86 libraries into the host system, and a host update can break them. It remains a possible complement later (registering armor in `binfmt_misc` for those who want it), not the main model.
- **Containers (Flatpak, Docker with amd64 images):** they cover only what is packaged that way, and add one more layer between the user and the program.
- **Continuing with Steam as an intermediary for Windows programs:** it works, but it is inconvenient, and it ties every program to a running Steam client.
- **Whole x86 Wine under armor or FEX:** possible, but it also translates Wine itself; native ARM64 Wine translates only the Windows program's code and is the path Valve and Wine are following.

## Consequences
- \+ An everyday use of armor for anyone with an ARM64 computer: installing an x86 program with one command or a double click.
- \+ Per-application roots reuse what already exists (verified Debian closure, profiles, `--sandbox`) and do not touch the system.
- − More disk space (one root per application, partly shared).
- − For Windows we depend on ARM64 Wine and on FEX, and on their versions: they must be pinned and measured like the other engines (`tech/engines.md`).
- Provenance of Wine and FEX, decided on 2026-10-14: **Hangover** (ARM64X Wine with FEX and box64; Wine LGPL-2.1, FEX and box64 MIT, DXVK zlib), chosen after the measurements ("Measurements"). armor does not redistribute it: `armor engine fetch` downloads the Hangover build for the host's distribution (from `/etc/os-release`), pinned by URL, size and SHA-256 in the code (`crates/armor/src/engines.rs`), and extracts its packages without installing them into `$XDG_DATA_HOME/armor/engines/`.
- Open: the final name of the commands.

## Measurements (2026-10-14)
Stage 1 of M10, on the Pi 5 with the 4K kernel (`rpi-v8`). Method, commands and full table in `tech/engines.md`, "Windows programs". Everything outside the repository, in `target/apps/` (`environment.md`); nothing installed in the system.

1. **Debian's Wine is not enough for x86 programs.** `wine`/`libwine` `10.0~repack-6` arm64, downloaded with `apt-get download` and not installed: the DLLs in `aarch64-windows` are pure ARM64 (`llvm-readobj-19 --file-headers`: Machine `IMAGE_FILE_MACHINE_ARM64 (0xAA64)`, `CHPEMetadataPointer: 0x0`), and `i386-windows` and `x86_64-windows` are empty: it runs only ARM64 Windows programs. For comparison, Steam's Proton Experimental (ARM64) (`experimental-11.0-20261008-arm64`) is built ARM64X (Machine `0xA64E`, CHPE metadata present), with 832 i386 PE files and 772 x86_64 ones, and FEX both as `libwow64fex.dll`/`libarm64ecfex.dll` and as `.so`.
2. **Hangover 11.16** (https://github.com/AndreRH/hangover: ARM64X Wine with FEX) is the measured source of ARM64X Wine + FEX outside Steam. Release `hangover-11.16` (2026-08-30), file `hangover_11.16_debian13_trixie_arm64.tar` (293 099 520 bytes), SHA-256 `b5493f5903ab3c05f78bf4c03b06ac0005eb26f732437748c1592fefa45e861a`, equal to the one published by GitHub. It contains `hangover-wine_11.16~trixie_arm64.deb`, `hangover-libwow64fex`, `hangover-libarm64ecfex`, `hangover-wowbox64` (box64 v0.4.4 as a WoW64 DLL) and `dxvk-v2.7.1.tar.gz`. Licenses from the packages' `copyright` files: Wine LGPL-2.1, the FEX DLLs and `wowbox64` MIT. Extracted with `dpkg-deb -x` (without root) into `target/apps/engines/hangover-11.16`, it runs from there (`usr/bin/wine --version`: `wine-11.16 (Hangover)`). According to `ldd`, the host lacks only `libsane.so.1` (scanners) and `libxkbregistry.so.0` (Wayland driver), optional with X11.
3. **How the emulator is chosen** (strings of the binaries, `strings`): for x86-64 programs (ARM64EC) `ntdll` uses the variable `HODLL64` (strings `libarm64ecfex.dll`, `xtajit64.dll`); for 32-bit x86 programs (WoW64) `wow64.dll` uses `HODLL`, default `wowbox64.dll`; with `HODLL=libwow64fex.dll` FEX is used. Lessons:
   - a prefix created when the WoW64 emulator DLL did not load has an empty `syswow64` and 32-bit programs fail (`wine: could not load kernel32.dll, status c0000135`); created with `HODLL=libwow64fex.dll`, `syswow64` has 855 files;
   - an emulator DLL added later (wowbox64) reaches the prefix's `system32` only after `wineboot -u` (before: `err:wow:load_64bit_module failed to load dll c0000135`);
   - the first `wineboot` stopped on Wine Mono's installation dialog (`control.exe appwiz.cpl install_mono`) on the desktop; with `WINEDLLOVERRIDES='mscoree,mshtml='` the prefix is created in 19 s, without Mono or Gecko.
4. **7-Zip 26.04** (2026-10-05, release 26.04 of `ip7z/7zip` on GitHub; SHA-256 of the six files equal to the published ones): `7z2604-x64.exe`, `7z2604.exe` (32-bit), `7z2604-arm64.exe`, `7z2604-x64.msi`, `7z2604-linux-x64.tar.xz`, `7z2604-linux-arm64.tar.xz`.
   - `wine 7z2604-x64.exe /S` installs in 5 s (`starting FEX (Hangover 11.16) based libarm64ecfex.dll`); the installed `7z.exe` is x86-64 (`0x8664`). The 32-bit installer, with `HODLL=libwow64fex.dll`, installs into `Program Files (x86)` (`7z.exe` `0x14C`).
   - **The `7zFM.exe` (x64) file manager opens on the Pi's real desktop** (capture with `grim`: window with toolbar and file list): the criterion of stage 1.
   - **Identical archives:** same input (4 000 000 random bytes from Python's `random.Random(42)` and an 18 MB text file), `7z a -t7z -mx=9 -mmt=1 -mtm=off -mtc=off -mta=off`. The Windows builds (x64 under FEX, 32-bit under FEX and under box64, native ARM64) all give the same archive, SHA-256 `ca55cbdc…aa67d` (7 653 232 bytes), which extracts to the inputs (`cmp`); the Linux `7zz` builds (x64 under armor, FEX, box64 and qemu; native ARM64) all give `7fd6e4c4…fb57b9`.
   - **Times** (a single thread; table in `tech/engines.md`): Windows x64 under FEX 24.3 s, Windows ARM64 in Hangover's Wine (native) 21.9 s; Linux ARM64 `7zz` native 21.0 s, x64 under FEX 23.5 s, box64 25.1 s, armor 55.4 s, qemu 93.9 s. On this workload FEX costs about 12% more than native, box64 19%, armor 2.6 times native (0.6 times qemu's time).
5. **The Epic Games Launcher installer** (`EpicInstaller-20.3.3.msi`, 264 237 056 bytes, SHA-256 `d55d79710edfeaa107a62f2ea007bec1eeeac8b715a43c2ba3858eec7bc60337`, `Template: x64`) is downloaded into `target/apps/downloads/epic` (saved as `EpicGamesLauncherInstaller.msi`), not yet run (stage 2).
