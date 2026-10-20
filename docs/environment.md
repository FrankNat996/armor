# Development environment

All the data below was **verified on 2026-10-12** with the commands shown, except where another date is given (versions rechecked on 2026-10-13). If it changes (updates, a different kernel), update the table and the date.

## Host
| Item | Value | How verified |
|---|---|---|
| Model | Raspberry Pi 5 Model B Rev 1.0 | `cat /proc/device-tree/model` |
| CPU | 4 ARM Cortex-A76 cores (implementer `0x41`, part `0xd0b`) | `/proc/cpuinfo`, `nproc` |
| CPU features | `fp asimd evtstrm aes pmull sha1 sha2 crc32 atomics fphp asimdhp cpuid asimdrdm lrcpc dcpop asimddp` | `/proc/cpuinfo` |
| RAM | 7.9 GiB | `free -h` |
| OS | Debian GNU/Linux 13 (trixie) | `/etc/os-release` |
| Kernel | until 2026-10-14 `6.18.50+rpt-rpi-2712`; since 2026-10-14 `6.18.50+rpt-rpi-v8` (below, "Kernel with 4K pages") | `uname -r` |
| **Page size** | **16384 (16K)** with `rpi-2712`; **4096 (4K)** with `rpi-v8` | `getconf PAGESIZE`; `CONFIG_ARM64_16K_PAGES=y` / `CONFIG_ARM64_4K_PAGES=y` |
| User VA | **47 bits** with `rpi-2712` (stack near `0x7fff_xxxx_xxxx`); **39 bits** with `rpi-v8` (stack near `0x7f_xxxx_xxxx`, 512 GiB per process) | `CONFIG_ARM64_VA_BITS` in `/boot/config-<version>`, `/proc/self/maps` (✅ 2026-10-14) |
| PA | 48 bits (`CONFIG_ARM64_PA_BITS=48`) | `/boot/config-$(uname -r)` |

Consequences for the project:
- The `atomics` (LSE) and `lrcpc` (LDAPR) features are needed by the JIT's memory model (ADR-0023). ✅ Verified on 2026-10-13 with a native program:
  - CPU part `0xd0b`, that is Cortex-A76;
  - **no `uscat`** (FEAT_LSE2): `LDAPR` and `STLR` on an unaligned address give SIGBUS, while normal unaligned `LDR` and `STR` work;
  - no `ilrcpc`: there are no sign-extending load-acquires (`LDAPURSB` and similar).
  Since 2026-10-14 the JIT and `cmpxchg16b` read them from `AT_HWCAP` and without them use the ARMv8.0 substitutes (`tech/jit.md`, "Every ARMv8.0"); `ARMOR_HOST_FEATURES=baseline` tests them here too. ⚠️ They have not yet been tested on a CPU that truly lacks LSE.
- There is no SVE and no hardware TSO mode.
- The JIT's flat permission table (ADR-0021) reserves 32 GiB of virtual space with `MAP_NORESERVE`: it is address space, not memory, but it requires the host not to forbid overcommit (✅ `overcommit_memory` = 0, 2026-10-12; `tech/memory.md`).
- The 47 bits of VA make identity mapping possible (`architecture.md`).

### Kernel with 4K pages
- `/boot` also contains `config-6.18.50+rpt-rpi-v8`, that is the `kernel8.img` variant with 4K pages.
- Until 2026-10-14 `/boot/firmware/config.txt` did not contain a `kernel=` line, so the default `2712` kernel with 16K pages was booted.
- **Since 2026-10-14 the Pi boots the 4K kernel** (for the ARM64 Steam client and FEX, ADR-0032): `/boot/firmware/config.txt` has `kernel=kernel8.img` under `[pi5]`, with a comment; the previous version is in `/boot/firmware/config.txt.before-4k`. To go back to 16K, remove the line and reboot. ✅ After the reboot `getconf PAGESIZE` gives 4096 and `uname -r` `6.18.50+rpt-rpi-v8`.
- There are initramfs images and modules for both kernels (`initramfs8`, `initramfs_2712`, `/lib/modules/6.18.50+rpt-rpi-{v8,2712}`).
- The 4K kernel has **39 bits of virtual addresses** (the 16K one 47): consequences in `tech/engines.md`, "On 4K pages".
- armor remains a 16K target (ADR-0005): its chain must also be repeated there from time to time, by switching kernel.

## Installed toolchain (2026-10-12)
Installed with:
```bash
sudo apt install gcc-x86-64-linux-gnu libc6-dev-amd64-cross binutils-x86-64-linux-gnu \
                 nasm qemu-user gdb-multiarch rustup
rustup default stable        # as a user, not with sudo
# since 2026-10-13, for 32-bit guests (ADR-0029):
sudo apt install gcc-i686-linux-gnu libc6-dev-i386-cross
```

| Tool | Version | Path / notes |
|---|---|---|
| `rustc` / `cargo` | 1.99.0 (stable, via rustup) | `/usr/bin/cargo` and `/usr/bin/rustc` are rustup proxies (Debian package `rustup` 1.27.1). Toolchain `stable-aarch64-unknown-linux-gnu`. |
| `clippy` / `rustfmt` | 0.1.99 / 1.10.0 | Included in rustup's default profile; also required by `rust-toolchain.toml` |
| `x86_64-linux-gnu-gcc` | 14.2.0 | Cross-compiler for the test programs and for armor's x86-64 `libGL.so.1` (ADR-0041; also needed by `cargo xtask package`, which ships it) |
| x86_64 glibc (`libc6-dev-amd64-cross`) | 2.41 | Sysroot in `/usr/x86_64-linux-gnu/` |
| x86_64 kernel headers | — | `/usr/x86_64-linux-gnu/include/asm/unistd_64.h`, `/usr/x86_64-linux-gnu/include/asm/stat.h` |
| `x86_64-linux-gnu-objdump` | binutils 2.44 | Decoder oracle |
| Host `as`, `objcopy` | binutils 2.44 | Oracle of the JIT's AArch64 encoder (`cargo xtask encoder-check`) |
| `timeout` (host coreutils) | — | Maximum time of each run in `difftest` (`testing.md`) |
| `qemu-x86_64` | 10.0.13 | `/usr/bin/qemu-x86_64` (package `qemu-user`), execution oracle |
| `i686-linux-gnu-gcc` | 14.2.0 | Cross-compiler for the 32-bit test programs (since 2026-10-13, ADR-0029) |
| i386 glibc (`libc6-dev-i386-cross`) | 2.41 | Sysroot in `/usr/i686-linux-gnu/`; `libc.so.6` is in the `decoder-check` chain |
| `qemu-i386` | 10.0.13 | Execution oracle for 32-bit guests (same `qemu-user` package) |
| `nasm` | 2.16.03 | Installed but not used for now: the tests use gas (`testing.md`) |
| `gdb-multiarch` | 16.3 | Guest debugging (for example with `qemu-x86_64 -g <port>`) |

How verified: `--version` of each tool, `dpkg -L qemu-user`, `dpkg -L linux-libc-dev-amd64-cross`, `rustup show active-toolchain`.

Installed on 2026-10-14 (ADR-0032): `clang`, `lld`, `llvm` 19, `cmake` 3.31.6, `ninja-build` 1.12.1 and `python3-pkg-resources` (to build FEX and box64); `libgtk2.0-0t64`, `libibus-1.0-5`, `libpipewire-0.3-0t64` (host libraries that the ARM64 Steam client asks for); `xvfb`, `x11-apps` (`xwd`), `netpbm` (virtual display and captures; `cargo xtask app-check` runs the applications on an Xvfb display and finds their windows with `xwininfo`, from `x11-utils`, already present with the desktop); then the desktop, `rpi-connect` and `grim` (above, "Desktop and remote screen"). box64 and FEX are built in `~/src` (versions and options in `tech/engines.md`), not installed in the system: no binfmt and no files in `/etc`.

Installed on 2026-10-15 (roadmap M9): `qemu-system-arm` 10.0.13 and `qemu-efi-aarch64` (KVM virtual machines with 16K and 64K pages, in `target/vm/`, `testing.md` "Other page sizes"), `cloud-image-utils` (their cloud-init seeds), `podman` 5.4.2 (containers of Fedora 42 and Arch Linux ARM for `cargo xtask distro-check`, ADR-0043; the downloads are in `target/distros/`).

Added on 2026-10-20 (ADR-0047): the Rust target `aarch64-pc-windows-msvc` (`rustup target add`), for `armor-wow64.dll` (`cargo xtask wow64`), linked with `lld-link` and import libraries from `llvm-dlltool`, both from the `lld` and `llvm` 19 packages above (✅ `rustup target list --installed`; `/usr/bin/lld-link` → `lld-link-19`). No MSVC and no Windows SDK. Also `arm64ec-pc-windows-msvc`, for the measurement of ADR-0047's second stage (`docs/decisions/0047-armor-as-the-windows-engine.md`, "Implementation notes").

Not installed (available if needed): `qemu-user-static`, the Debian `rustc`/`cargo` 1.85.1 packages, Debian's `box64` 0.3.4 (older than the releases used).
Miri (dynamic checking of `unsafe` code) requires the nightly toolchain: not installed. `unsafe` code has existed since M1 (`armor-mem`, `armor-linux`, `armor-jit`); installing it is a choice still to make.

### Desktop and remote screen (since 2026-10-14)
To test the ARM64 Steam client and games on a real screen (desktop plus remote screen, since 2026-10-14). Versions ✅ with `dpkg-query -W`, 2026-10-14:
- **Desktop:** `rpd-wayland-core` 1.31 (the Raspberry Pi Wayland desktop), with `labwc` 0.20.1 (compositor), `lightdm` 1.32.0, `xwayland` 24.1.6 (X11 programs, including the Steam client, on display `:0`), `wayvnc` 0.10.1, `pipewire` 1.4.2. `raspi-config` chose labwc and automatic login to the desktop for the desktop user.
- **Raspberry Pi Connect:** full `rpi-connect` 2.13.0 (remote shell and screen sharing) in place of `rpi-connect-lite` (preinstalled, shell only; enabled on 2026-10-14, with systemd linger, `loginctl enable-linger <user>`). Screen sharing did not work at first because `rpi-connect` had been started as the lite version: solved by enabling `rpi-connect-wayvnc` and restarting `rpi-connect`; then `rpi-connect doctor` is all green ✅.
- **Screen captures:** `grim` 1.4.0 (Wayland).
- **The ARM64 Steam client on the desktop:** installed with `armor install steam` in `~/.local/share/armor/apps/Steam` and opened from its menu entry (`armor run Steam`: its own HOME, and `XAUTHORITY` pointing to the `.Xauthority` of the real home, which is needed because HOME changes).
- **Sound:** PipeWire, `pipewire-pulse` and WirePlumber enabled as user services since 2026-10-15 (they were installed but not started). No HDMI display is connected (the desktop is used through Raspberry Pi Connect), so the only output is PipeWire's null sink.

### Windows engine and installed applications (since 2026-10-14)
For Windows programs (ADR-0033; measurements in `tech/engines.md`, "Windows programs"). No root, no `dpkg -i`, no files in `/etc`: everything in the user's home (✅ directory listing, 2026-10-14).
- `~/.local/share/armor/engines/` (`$XDG_DATA_HOME/armor/engines/`), written by `armor engine fetch`: `hangover-11.16/` (the extracted Hangover 11.16 `.deb` packages for Debian 13, 2.4 GB ✅ `du -sh`), `dxvk-v2.7.1/` (from the same release: `x64`, `x32`, `arm64ec`, `aarch64`), and the links `wine` → `hangover-11.16` and `dxvk` → `dxvk-v2.7.1`, which `armor install` and `armor run` use. The host lacks only `libsane.so.1` and `libxkbregistry.so.0`, optional with X11 (not installed).
- `~/.local/share/armor/apps/` (`$XDG_DATA_HOME/armor/apps/`), one folder per application installed with `armor install`: for a Windows program (`7zip`, `epic`; since 2026-10-14 Notepad++) `app.conf`, the Wine prefix (`prefix/`), Wine's shortcuts (`menu/`) and the list of entries (`entries`); for an x86-64 `.deb` (since 2026-10-14, Spotify) `app.conf` with `kind = linux`, the unpacked root (`root/`) and `entries`. The menu entries are `~/.local/share/applications/armor-<name>-<n>.desktop`.
- `~/.local/share/armor/cache/` (since 2026-10-14, `.deb` installs): `debs/`, the downloaded `.deb` packages shared by all applications (checked against the archive index's SHA-256), `index-<suite>-amd64/`, the archive's verified `InRelease` and `Packages.xz` (reused for a day), and `scratch/`, a working directory. Needs the host's `gpgv` and the archive keyring (`/usr/share/keyrings/debian-archive-keyring.gpg` on Debian).
- `target/apps/downloads/` (outside the repository, in `target/` like the guest roots): the installers used for the tests, `7zip-26.04/` (the six files of release 26.04, with `published.sha256`), `epic/` (`EpicGamesLauncherInstaller.msi`), `wine-mono-11.3.0/` (`wine-mono-11.3.0-arm64.msi`, installed by hand in Epic's prefix). SHA-256 in `tech/engines.md` and in ADR-0033.
- Removed on 2026-10-14: the copy of Hangover extracted by hand (`target/apps/engines/`), the test prefixes (`target/apps/prefixes/test`, `test32`), the measurement working files (`target/apps/work/`), the Hangover archive downloaded by hand and the Debian Wine packages downloaded to examine them.

### x86-64 hardware oracle (ADR-0025, since 2026-10-13)
A temporary Proxmox container: **Intel Xeon X5675** (Westmere, family 6 model 44; SSE up to SSE4.2, POPCNT, AES, PCLMULQDQ, no AVX), 4 cores, Ubuntu 24.04 (glibc 2.39), Proxmox kernel 7.0, 4K pages; no compiler ✅ (`/proc/cpuinfo`, `/etc/os-release`, 2026-10-13).
- Access with a dedicated key, `~/.ssh/armor_oracle` on the Pi (outside the repository), installed in the container's `authorized_keys`;
- `cargo xtask difftest --oracle` reads the destination from `ARMOR_ORACLE` and the key from `ARMOR_ORACLE_KEY`;
- when the container is deleted: also delete `~/.ssh/armor_oracle*`.

### busybox for the tests
`cargo xtask fetch-busybox` downloads the Debian `busybox-static` amd64 package with pinned version and SHA-256 (`xtask/src/busybox.rs`), and puts the binary in `target/guest/busybox/busybox`, which does not go into the repository. It uses `curl`, `sha256sum`, `ar` and `tar`, already present on the host (verified on 2026-10-12). It must be run once after cloning, and again after `cargo clean`.

`perf` (package `linux-perf` 6.18, installed on 2026-10-12) is used to profile armor: `tech/performance.md`.

`cargo xtask fetch-rootfs` does the same for the guest root with coreutils, grep, sed, gawk, findutils, perl, python, gzip, xz, bzip2, tar, diffutils, make and dash (39 amd64 packages; ADR-0014). Same tools, same rules. `cargo xtask fetch-rootfs --i386` (since 2026-10-14) builds `target/guest/rootfs-i386` with i386 coreutils and its 14 dependencies (the same packages and versions as amd64, ADR-0029).

## Host memory layout relevant to the guest
Verified on 2026-10-12:
- `vm.mmap_min_addr` = 4096 (`/proc/sys/vm/mmap_min_addr`).
- ASLR enabled (`/proc/sys/kernel/randomize_va_space` = 2).
- The host's PIE executables (including `armor`) are loaded around `0x5555_xxxx_xxxx` (`/proc/self/maps`). x86_64 `ET_EXEC` executables start at `0x400000`, so they do not collide (`tech/memory.md`).

## Git and GitHub
Verified on 2026-10-12:

| Item | Value | How verified |
|---|---|---|
| git | 2.47.3 | `git --version` |
| gh | 2.101.0, authenticated as `FrankNat996` (https protocol, scopes `gist`, `read:org`, `repo`) | `gh auth status` |
| Remote | `origin` → https://github.com/FrankNat996/armor (public since 2026-10-14) | `gh repo view` |
| Commit identity | `Francesco Natale <111684444+FrankNat996@users.noreply.github.com>`, set **only in the repository's local config** | `git config --local -l` |
| Main branch | `main` | |

- No global git identity is configured: in other repositories commits will require a separate configuration.
- Commits use GitHub's noreply address, so that no personal e-mail appears in them.

## Practical notes
- Parallel builds: `-j4` (4 cores). With 8 GB of RAM memory is not a problem.
- `ARMOR_TRACE_MAPS=1` (since 2026-10-14): armor prints every guest file mapping and where it loaded the program and the interpreter, to find which library an address belongs to (`tech/memory.md`).
- Core dumps: `ulimit -c` = 0 and `kernel.core_pattern` = `core` (verified on 2026-10-12), so tests that end with SIGSEGV leave no `core` files.
- No other real x86 hardware is available; one could act as a third oracle (`testing.md`).
