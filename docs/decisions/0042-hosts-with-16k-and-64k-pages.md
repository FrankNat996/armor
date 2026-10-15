# 0042 — Hosts with 16K and 64K pages: armor and Hangover as they are, Steam on a 4K kernel

- **Status:** Proposed
- **Date:** 2026-10-15

## Context
Roadmap M9 asks what to do on ARM64 systems whose kernel uses 16K or 64K pages (Apple Silicon with Asahi Linux, some servers, the Raspberry Pi 5's default kernel), where Hangover, FEX and Valve's ARM64 Steam client were believed not to start. The candidates: a 4K kernel documented per distribution, a 4K micro-VM, armor as the engine behind Wine's WoW64/ARM64EC interface, the i386 Steam client under armor.

Measured on 2026-10-15 in KVM virtual machines on the Pi 5 (`testing.md`, "Other page sizes"):
- **armor's engine** runs on both: Debian 13 with its 16K kernel, Ubuntu 24.04 with its 64K kernel. Two bugs in armor appeared there and were fixed (growing shared mappings smaller than a host page; memory placed above 2^47 on a 48-bit host); then every `difftest` case gives the same result as on the 4K host, where it matches qemu and the Xeon.
- **Windows programs (Hangover 11.16)** run on 16K pages: Wine warns about the page size and works; 7-Zip x64 installs and its archive is byte-for-byte identical to the native ARM64 build's; Notepad++ installs with `armor install` and opens. FEX inside Wine is a DLL without jemalloc, the part that stops FEX for Linux on 16K pages (`tech/engines.md`).
- **Valve's ARM64 Steam client** needs 4K pages: its segments are 4K-aligned and it ends with SIGSEGV on the Pi's 16K kernel (`tech/engines.md`, measured 2026-10-14). Proton runs inside the client.

## Decision
- **Linux x86 programs:** armor as it is, on 4K, 16K and 64K pages. The `difftest` results on each page size are in the compatibility matrix, reproduced with the virtual machines of `testing.md` and `difftest --expected`.
- **Windows programs:** Hangover as it is, on every page size where it is measured to work (16K today); armor does not refuse them.
- **Steam:** a kernel with 4K pages. Where the hardware has one (the Raspberry Pi 5's `kernel8.img`; Debian's and Ubuntu's generic arm64 kernels use 4K pages), `armor install steam` says how to switch, as today. On hardware with no 4K kernel (Apple Silicon), a 4K micro-VM (`muvm`, which Fedora Asahi Remix uses for Steam) is the route, to measure when such a machine is available.
- **Not now:** armor as the engine behind Wine's WoW64/ARM64EC interface (Hangover's FEX already runs on 16K pages) and the i386 Steam client under armor (the micro-VM is the existing component for the same case).

## Alternatives considered
- **Requiring a 4K kernel for everything:** simpler to explain, but it would refuse what works: armor's engine and Hangover run on 16K pages.
- **armor as the WoW64/ARM64EC engine:** the largest piece of work of the candidates; it would replace FEX inside Wine where FEX already works.
- **The i386 Steam client under armor:** it already updates itself without a display, but the 32-bit client is on its way out and would need its own graphics path; a 4K micro-VM runs the ARM64 client unchanged.

## Consequences
- \+ Linux x86 programs and Windows programs work on 16K-page systems without changing the kernel; the README no longer asks for 4K pages for Windows programs.
- − Steam stays limited to 4K kernels until the micro-VM route is measured on hardware without one.
- Open: Hangover on 64K pages (to measure), and the micro-VM on Apple Silicon.
