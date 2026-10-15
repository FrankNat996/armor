# 0043 — Other distributions: one archive of armor, Debian's libraries and Hangover's Debian build everywhere

- **Status:** Accepted (2026-10-15)
- **Date:** 2026-10-15

## Context
Roadmap M9 asks armor to work beyond Debian and Ubuntu (Fedora, Arch and similar). Hangover publishes builds only for Debian and Ubuntu, and armor itself comes as a `.deb`. What depends on the host's distribution today:
- **armor's package:** a `.deb`, installed with apt; `armor update` installs the next one with apt.
- **Linux applications** (`.deb`, AppImage, `.tar.*`, ADR-0033): their x86-64 libraries come from the amd64 archive of the host's own distribution (`debs::archive_for`), checked with `gpgv` and the archive keyring in `/usr/share/keyrings/`. Any other host is refused.
- **The Windows engine** (`armor engine fetch`): the Hangover build for the host's Debian or Ubuntu release (`engines::build_for`). Any other host is refused.

Measured on 2026-10-15 in containers (podman, on the Pi 5) of Fedora 42 (glibc 2.41) and Arch Linux ARM (glibc 2.43; its official root, signature checked):
- ✅ armor's binary from the `.deb` runs unchanged: Debian's Python 3.13 (x86-64) and coreutils' `sort` (i386) give the same output as on the Pi's Debian. It needs glibc 2.39 or later (`objdump -T`: highest symbol version `GLIBC_2.39`), so it does not run on Ubuntu 22.04 (2.35), although the package only depends on `libc6`.
- ✅ Hangover 11.16's Debian 13 build starts Wine on both (`wine cmd /c ver` prints the version), with the host's own X11, FreeType, GnuTLS and Vulkan libraries. Its media part wants `libavutil.so.59`; Arch has a newer FFmpeg, so video in Windows programs may not play there (not tried).
- The x86 libraries of Linux applications live in the application's own root, not on the host: the host's distribution only decides which archive armor reads.

## Decision
- **Linux applications on any distribution:** on a host that is neither Debian nor Ubuntu, armor takes the libraries from **Debian stable's** amd64 archive (today trixie). armor carries Debian's archive keyring itself (`share/armor/keyrings/debian-archive-keyring.gpg`, copied from Debian's `debian-archive-keyring` package when armor is built) and uses it when the host has none; `gpgv` stays a requirement (Fedora's `gnupg2`, Arch's `gnupg`).
- **The Windows engine on any distribution:** on such a host, Hangover's **Debian 13** build, with the same checks; missing host libraries are named as today.
- **armor as an archive:** next to the `.deb`, each release carries `armor_<version>_arm64.tar.gz` with the same files (`bin/`, `lib/armor/`, `share/`) and an `install.sh` that installs into `~/.local` without root, with the menu entries and the double-click handler; `uninstall.sh` removes them. `armor update` updates such an installation from the archive (checked against GitHub's SHA-256, as the `.deb`).
- **The `.deb` declares what it needs:** `libc6 (>= 2.39)`.
- **Verified by a command:** `cargo xtask distro-check` runs, in podman containers of Fedora and Arch Linux ARM, the archive's `install.sh`, x86-64 and i386 programs under armor, `armor install` of an x86-64 `.deb`, and `armor engine fetch` with `wine cmd /c ver`. Its results go into the compatibility matrix.

## Alternatives considered
- **Native packages for each distribution (RPM, PKGBUILD):** better integrated, but one more format to build and test per distribution; the archive covers every one, and native packages can follow on request.
- **The host distribution's own x86-64 libraries (Fedora's x86_64 repository, Arch's x86_64 one):** Arch Linux ARM has no x86_64 repository, and every distribution would need its own resolver and signature check; Debian's archive is already resolved and verified by armor.
- **Building Hangover for Fedora and Arch:** a long build per distribution and release; the Debian build already starts on both.
- **Flatpak:** armor runs other programs and needs the host's paths and namespaces; a sandboxed runtime makes that harder, not easier.

## Consequences
- \+ armor installs and works on Fedora, Arch and others with one archive; Linux applications are the same everywhere, since their libraries come from the same archive.
- − armor's archive carries a copy of Debian's keyring, refreshed when armor is built; a key rotation needs a new armor release (the `.deb` keeps using the host's keyring package).
- − On distributions with a newer FFmpeg, Windows video may not play until Hangover's Debian build follows.
- Open: glibc older than 2.39 (Ubuntu 22.04): building armor against an older glibc, measured separately.
