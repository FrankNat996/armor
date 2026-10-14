# 0032 — Integrate the existing components: armor as glue, test bench and engine where needed

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- Wider context: M9 (`tech/steam.md`), ADR-0029, ADR-0031.

## Context
The goal: an application that runs x86 software (Steam, Linux and Windows games) on **any** ARM64 CPU, simple to use, and that becomes a reference for development on ARM. Until today M9 planned to emulate everything with armor, including the Steam client.

Facts that emerged on 2026-10-14 (sources in `tech/steam.md`):
- Valve builds a native ARM64 Steam client for Linux (manifest `steam_client_linuxarm64`, the Steam Frame's); officially it is not supported outside its hardware, but third-party projects install it on other ARM64 systems.
- Proton has ARM64 builds: native ARM64 Wine, native DXVK and vkd3d towards Vulkan, and the games' x86 code translated by FEX through Wine's WoW64/ARM64EC interface (Proton 11 includes FEX).
- Mature user-mode emulators exist, both MIT-licensed: FEX (sponsored by Valve) and box64, which calls the host's native libraries in place of the emulated x86 ones (GL, Vulkan, SDL, …). box64's 32-bit mode (BOX32) is "experimental, do not use" in v0.4.4; box86 requires an armhf userspace, which many recent ARM64 CPUs do not have. box64's changelog says Wine does not work with 16K pages.
- armor already runs: x86-64 and i386 directly on AArch64 (without armhf), 4K guest pages on 16K hosts, exact 80-bit x87, the Steam i386 client up to the complete update (stage 3b). It has a decoder verified against objdump, `difftest` against qemu, a hardware oracle (Xeon) and lockstep between JIT and interpreter.

## Decision
1. **Layers, each with the best component that exists:**
   - **Steam client:** Valve's native ARM64 one; the i386 client under armor remains as a fallback;
   - **Windows games:** Proton ARM64 with FEX as translator; armor may become an alternative engine behind the same WoW64/ARM64EC interface, if measurements show gaps (pages, x87, CPUs without LSE/RCpc);
   - **x86 Linux programs and games:** armor, later with direct calls to the host's native libraries (Vulkan, GL, SDL, X11, audio) for performance;
   - **graphics:** the host's Mesa (Turnip, PanVK, v3dv, Honeykrisp…). No GPU emulation.
2. **armor is the glue:** a single command that recognises what is being launched (x86-64 ELF, i386, Windows PE, Steam client) and chooses the engine and the configuration; the per-application profiles (ADR-0028) say which engine to use where needed.
3. **armor is the test bench:** `difftest`, the hardware oracle and the compatibility matrix measure every engine (armor, FEX, box64) on the same cases, and the matrix says which one works where, CPU by CPU. This is the reference for development on ARM.
4. **"Any ARM64 CPU"** means: baseline ARMv8.0 (armor must choose at run time between LSE/`LDAPR` and their substitutes), 4K, 16K or 64K pages, no armhf userspace. Graphics depends on the host's Vulkan driver: this is stated, not promised.
5. **Measure first:** FEX and box64 are built from the official releases (box64 v0.4.4, FEX-2610) in `~/src`, outside the repository, and tried on this Pi 5 (16K) with armor's test programs, before building on them.

## Licenses
FEX and box64 are MIT, compatible with `MIT OR Apache-2.0` (ADR-0006): they can be used as components and, keeping their notice, their code can be reused. Wine (LGPL-2.1) and Proton are used as separate components. GPL code (qemu, and other GPL projects) stays excluded from armor.

## Alternatives considered
- **Continuing to emulate everything with armor:** possible, but it would redo years of work already done (ARM64 Wine, the JIT for Windows games), and would not give a result in useful time.
- **A fork of box64 or FEX:** it would inherit their limits (16K, armhf, 32 bits) without the test bench, and armor would disappear.
- **Only a launcher on top of the existing projects:** simple, but without an engine of its own the gaps (16K, ARMv8.0 CPUs, i386 without armhf) are not covered and nothing is verified.

## Consequences
- \+ A usable result sooner: native Steam and Proton ARM64 already exist.
- \+ The work done stays: decoder, interpreter, JIT, x86-64 and i386 syscalls, oracles and tests become the Linux engine and the test bench.
- − We depend on external projects and their versions: they must be pinned like the Debian roots (ADR-0014).
- − Stages 3–7 of M9 change (`roadmap.md`).
- The external projects used as components are named in the documentation (they are used as components, not only cited as examples).
