# Vision

## The goal (ADR-0034, 2026-10-14)
A single application that makes it simple for anyone with an ARM64 computer to use any x86 software — Linux programs, Windows programs and games — without knowing what happens underneath; and for armor to become a measured reference for development on ARM.

The project was born on 2026-10-12 as a user-mode x86-64 → AArch64 binary translator, and since that same day it follows a guiding document (not published) that asks for a compatibility layer in which the user installs and runs applications built for other architectures without knowing about CPUs, ABIs or emulation. ADR-0032 (integrate the existing components), ADR-0033 (`armor install`) and ADR-0034 (priorities) make it today's goal.

## The priorities, in order
1. **Ease for everyone.** One command or a double click: the program installs, appears in the menu, opens. No prefixes, environment variables, DXVK or hashes to manage by hand; armor itself also installs without compiling it.
2. **Any x86 program:** Linux (`.deb`, AppImage, archives), Windows (`.exe`, `.msi`), Steam and games.
3. **Any ARM64 CPU:** baseline ARMv8.0, 4K, 16K or 64K pages, without an armhf userspace. Where the external components do not reach, armor is the engine or the solution.
4. **The best existing component** for every layer, instead of rewriting it (ADR-0032).
5. **A measured reference:** no compatibility claimed without proof (`COMPATIBILITY_MATRIX.md`).

What is missing and in what order: `roadmap.md` (milestones M7–M10) and `analysis.md`.

## armor's roles
- **Engine for x86 Linux programs** (x86-64 and i386). armor loads an x86 Linux ELF and runs it on a Linux AArch64 host, translating the x86 instructions and the x86 Linux system calls into their equivalents on the host. It does not emulate a whole PC: no BIOS, no guest kernel, no peripherals; the kernel stays the host's. Later it will call the host's native libraries directly (graphics, audio).
- **Glue around the external components.** For Windows programs armor downloads and verifies Hangover (Wine ARM64X with FEX) and DXVK (`armor engine fetch`) and prepares a prefix per application with the menu entries (`armor install`). For Steam, Valve's native ARM64 client and Proton ARM64. For graphics, the host's Mesa drivers.
- **Test bench.** `difftest` compares armor, box64 and FEX with qemu and with a real x86 CPU on the same cases (`testing.md`, `tech/engines.md`); the compatibility matrix says what works, with which engine and on which CPU.

## Engine principles
1. **Verifiable correctness.** Every implemented instruction has differential tests against an oracle (`qemu-x86_64`, a real x86 CPU; `testing.md`).
2. **Understandable errors.** `armor ./program args`, with no mandatory environment variables; when armor stops it says why and what to do (`--diagnose`, ADR-0028), instead of a crash.
3. **Every ARM64 CPU.** The engine handles 16K pages (ADR-0005) and chooses at runtime between the extensions after ARMv8.0 and their substitutes (`tech/jit.md`, "Every ARMv8.0"). Since 2026-10-14 the development Pi boots the 4K kernel, because the external components require it (`environment.md`); 16K and 64K systems are an item of M9.
4. **Understandable and documented code**, so that others can contribute.
5. **Performance after correctness** (JIT since 2026-10-13, `tech/performance.md`).

## Non-goals
- Full-system emulation (BIOS, guest kernel, peripherals).
- Declaring armor a secure sandbox without a threat model and verification (`tech/security.md`).
- Emulating the GPU: graphics go through the host's drivers, and depend on them.
- Redoing what other projects already do well (Wine ARM64, FEX, DXVK, the ARM64 Steam client): they are components to measure and integrate, not competitors (ADR-0032). armor does not redistribute them: it downloads them from the source.

## Similar projects
box64, FEX-Emu, qemu-user, Hangover, Rosetta 2 (proprietary). FEX (inside Hangover and Proton ARM64), Hangover and Valve's ARM64 Steam client are components of the application; box64 is an engine measured on the same cases (`tech/engines.md`). Links and licenses in `references.md`.
