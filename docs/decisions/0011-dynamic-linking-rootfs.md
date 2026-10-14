# 0011 — Dynamic linking: the real x86-64 `ld.so` from a guest rootfs

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
Almost all distribution binaries are dynamic and PIE: it is the gap that blocks the most real applications (initial audit of 2026-10-12, §6; replaced by `analysis.md` on 2026-10-14, the text remains in the repository history). The question asked how to run them:
- with the real x86-64 dynamic loader (`ld-linux-x86-64.so.2`) and the real x86-64 libraries, taken from a guest rootfs;
- or by replacing the libraries with the host's native ones (wrapping/thunks).

Measurements of 2026-10-12:
- `qemu-x86_64 -L /usr/x86_64-linux-gnu` runs a dynamic hello using the x86-64 loader and `libc.so.6` that `libc6-amd64-cross` has already installed on the host;
- `ld.so` maps the libraries with file `mmap` at offsets that are multiples of 4K but not of 16K.

## Decision
- armor runs dynamic programs by loading the loader named in `PT_INTERP` **from the guest rootfs**. The loader then loads the x86-64 libraries from there, as on a real x86-64 system. armor does not reimplement the dynamic linker.
- The first rootfs is the cross-compiler's (`/usr/x86_64-linux-gnu`). How to make the root configurable (option, environment variable, profiles) is decided when path resolution is implemented, documenting it in `architecture.md`.
- PIE executables (`ET_DYN`) are loaded at a base address chosen by armor.
- Host libraries (thunks) remain excluded until there is a case with a verified ABI (M7, guiding document §3D).
- Milestone M5 is brought forward, together with the "memory" part of M3 (file `mmap` with 4K offsets on 16K hosts), ahead of threads, signals (M3) and floating point (M4).

## Alternatives considered
- **Wrapping host libraries (box64 style ⚠️):** better performance, but every library requires bindings with a verified ABI. The guiding document forbids linking by symbol name without verification.
- **Original order M3 → M4 → M5:** threads and floating point are useful, but without dynamic linking almost no real binary starts.

## Consequences
- \+ Behaviour identical to an x86-64 system: same glibc, same symbol resolution, same `LD_*`.
- \+ Comparable with `qemu-x86_64 -L <same root>` in `difftest`.
- − An x86-64 rootfs is needed on the host. The cross-compiler's contains only glibc and a few libraries: programs with more dependencies will need a richer rootfs (to be decided). Decided with ADR-0014 (pinned Debian packages, `fetch-rootfs`).
- − All library code runs in the interpreter, so it is slow until the JIT (M6).
- To be designed: how guest paths (`/lib64/...`, `/etc/ld.so.cache`) are resolved in the rootfs without hiding the host files the program must see. Designed with ADR-0013 (resolution like `qemu -L`).
