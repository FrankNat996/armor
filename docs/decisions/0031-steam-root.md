# 0031 — Guest root for Steam: amd64 and i386 in the same tree

- **Status:** Superseded by 0034 (2026-10-14). It was Proposed: the Steam client in use is Valve's native ARM64 one, and the i386 client under armor remains only as a fallback on 16K systems (M9 of the new roadmap).
- **Date:** 2026-10-14
- Wider context: M9, stage 3 (`tech/steam.md`); extends ADR-0014.

## Context
`steam.sh` is a 64-bit bash script: it calls `uname`, `which`, `realpath`, `md5sum`, `tar`, `ldd` (the `libc-bin` script, which launches the loader matching the binary's class), and the runtime it invokes (`setup.sh`, `run.sh`) also uses `ldconfig`, `file`, `find`, `readlink`; then it runs the i386 client `ubuntu12_32/steam`, which asks for `ld-linux.so.2` and the i386 glibc. Today armor has two separate roots: `rootfs` (amd64, ADR-0014) and `rootfs-i386` (ADR-0029, with the i386 coreutils in their `/usr/bin` paths). A process and its children see a single root (`--root`, ADR-0013).

Measured on 2026-10-14:
- the client, launched directly in the i386 root with the links that `steam.sh` prepares for it, completes the update under armor (`tech/steam.md`, stage 3b): it needs only the i386 glibc;
- compared with `rootfs`, `steam.sh` and the runtime lack `bash`, `libc-bin` (`ldd`, `ldconfig`), `file` (with `libmagic1t64` and `libmagic-mgc`) and `base-files` (a dependency of `bash`, it brings `/etc/os-release`, which `steam.sh` reads); `debianutils` (`which`) is already there. Closure computed on the trixie amd64 Packages index downloaded on 2026-10-14 and verified against `Release` (dated 2026-09-12): the common versions coincide with those already pinned;
- on the i386 side the closure of `libc6` (`libc6`, `libgcc-s1`, `gcc-14-base`) is enough, already pinned for `rootfs-i386`. The client's other libraries (X11, gnutls, curl, …) are brought by the Steam runtime downloaded with the update.

## Decision
- `cargo xtask fetch-rootfs --steam` builds `target/guest/rootfs-steam` as a multiarch Debian root: all the amd64 packages of `rootfs`, plus `bash`, `libc-bin`, `file`, `libmagic1t64`, `libmagic-mgc`, `base-files`, plus the i386 library packages (the closure of `libc6:i386`), in the same tree. The i386 libraries are in `/usr/lib/i386-linux-gnu` and the loader in `/usr/lib/ld-linux.so.2`, the amd64 ones in `/usr/lib/x86_64-linux-gnu`: they do not overlap. No programs are taken from the i386 packages (no i386 coreutils: they would go on top of the amd64 ones).
- Same rules as ADR-0014: packages pinned by path and SHA-256, only `data.tar` extracted, no maintainer script run, unified /usr links.
- Steam itself does not go into the root: the bootstrap and what it downloads are in a separate directory under `target/` (untrusted data: never run outside armor), with its own HOME.

## Alternatives considered
- **Two roots and a change of root at `execve` by ELF class:** armor would have to choose the root from the launched binary; it complicates the resolution of shared paths (the same `/home`, the same Steam files) and corresponds to no real system.
- **Launching only the client, without `steam.sh`:** enough for the update (stage 3b), but the restart after the update (code 42), the runtime and its environment variables are handled by `steam.sh`.
- **The Steam runtime as root:** it brings libraries, not the basic tools (`bash`, coreutils) and not glibc.

## Consequences
- \+ A tree like that of an amd64 Debian with `dpkg --add-architecture i386`: the shape that `steam.sh` and the runtime expect.
- \+ The test roots (`rootfs`, `rootfs-i386`) stay as they were.
- − About 40 MB more under `target/guest`.
- − Like the other roots, it is not an installed system: no `/etc/ld.so.cache` (`setup.sh`'s `ldconfig -p` finds nothing) ⚠️ to see whether the runtime needs it.
