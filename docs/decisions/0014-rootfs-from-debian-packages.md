# 0014 — Guest root for the tests: pinned Debian amd64 packages

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
The cross-compiler's root (`/usr/x86_64-linux-gnu`) contains glibc and a few libraries. Debian's coreutils (the goal of M5) also depend on libacl, libattr, libcap, libgmp, libselinux, libssl, libsystemd and their dependencies. Real x86-64 libraries are needed, reproducible, without installing amd64 packages on the host.

## Decision
- `cargo xtask fetch-rootfs` downloads `coreutils` 9.7-3 amd64 and the closure of its dependencies (Pre-Depends and Depends, first alternative) from the Debian trixie Packages index of 2026-10-12: 15 packages, about 11 MB. Extended the same day to `grep`, `sed`, `gawk` and `findutils` (24 packages, about 15 MB), then to `perl-base` and `python3.13-minimal` (29 packages), with the same method. Extended on 2026-10-13 to `gzip`, `xz-utils`, `bzip2`, `tar`, `diffutils`, `make` and `dash` (39 packages), from that day's index.
- Every package is pinned by path and SHA-256 (`xtask/src/rootfs.rs`) and verified before extraction.
- Only the `data.tar` is extracted, with `ar` and `tar`, into `target/guest/rootfs`. Maintainer scripts never run and nothing from the package is executed outside the emulators.
- Debian's unified /usr links (`/bin`, `/sbin`, `/lib`, `/lib64` → `usr/...`), which do not come from the packages, are added.
- `difftest` runs the cases of `tests/guest/coreutils.txt` with this root both under qemu (`-L`) and under armor (`ARMOR_ROOT`).

## Alternatives considered
- **`debootstrap` or an amd64 Docker image:** a complete system, but much larger; it requires privileges or extra tools and runs scripts.
- **Host multiarch (`dpkg --add-architecture amd64`):** would install amd64 libraries into the system, outside the repository and without pinned versions.

## Consequences
- \+ Reproducible: same versions and same bytes on every machine.
- \+ No changes to the host system.
- − It is not an installed system: no `/etc/ld.so.cache`, no configuration. `ld.so` uses the default paths; the `/etc` files absent from the root come from the host (ADR-0013).
- − Debian removes superseded versions from the mirror: when a URL disappears, the list must be updated (or one switches to snapshot.debian.org).
- The locale and the data files (`/usr/lib/locale`, `/usr/share/zoneinfo`) come from the host, the same for qemu and armor.
