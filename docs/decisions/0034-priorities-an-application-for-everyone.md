# 0034 — Priorities: armor as an application for everyone

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- Takes up and orders: ADR-0032 (integrate the existing components), ADR-0033 (`armor install`). Supersedes ADR-0031.

## Context
In three days the project went from "x86-64 → AArch64 user-mode translator" to a broader goal, in successive pieces (guiding document, Steam, games, the installer). The roadmap had four open milestones that overlapped (M7, M8, M9, M10), and the last hours of work went to debugging a single application (the Epic launcher) inside external components. On 2026-10-14 the goal was restated as follows:

> Un'applicazione unica che renda semplice, per chiunque abbia un computer ARM64, usare qualsiasi software x86 — programmi Linux, programmi Windows e giochi — senza dover sapere cosa succede sotto; e che armor diventi un riferimento per lo sviluppo su ARM.
>
> (A single application that makes it simple, for anyone with an ARM64 computer, to use any x86 software — Linux programs, Windows programs and games — without having to know what happens underneath; and that armor becomes a reference for development on ARM.)

## Decision
The priorities, in this order, decide what is done first:
1. **Ease for everyone.** One command or a double click: the program installs, appears in the menu, opens. No prefixes, environment variables, DXVK or hashes to manage by hand: every step done by hand for an application becomes automatic in armor. armor itself must also install without compiling it.
2. **Any x86 program:** Linux (`.deb`, AppImage, archives), Windows (`.exe`, `.msi`), Steam and games.
3. **Any ARM64 CPU:** baseline ARMv8.0, 4K, 16K or 64K pages, without an armhf userspace. Where external components do not reach (16K pages, CPUs without LSE), armor is the engine or the solution.
4. **The best component that exists** for each layer (ADR-0032): Hangover (ARM64X Wine with FEX) and DXVK for Windows, Valve's ARM64 Steam client and Proton ARM64 for Steam, the armor engine for x86 Linux programs, the host's Mesa for graphics.
5. **A measured reference:** the compatibility matrix says what works, with which engine, on which CPU, with reproducible evidence (`difftest` against qemu and a real x86 CPU, `--engine`, `bench`). No compatibility declared without evidence.

The role of the armor engine, stated without ambiguity:
- for **x86 Linux** programs it is the engine (exact: 427 cases out of 427 identical to qemu; slower than FEX, 2.6× native against 1.1× on 7-Zip);
- for **Windows** programs armor today is the glue around Hangover; the armor engine is needed where Hangover and FEX do not start (16K pages), as future and large work;
- for all engines armor is the **test bench**.

The roadmap becomes (`roadmap.md`): M0–M6 stay as completed foundations; M7–M10 are replaced by M7 "The installer for everyone", M8 "Games and Steam", M9 "Any ARM64", M10 "The reference", in this order of priority. The correspondence table between the old and the new ones is at the top of `roadmap.md`. The analysis of what is needed is in `analysis.md`.

Debugging a single application inside external components (such as Epic's sign-in page) stops when it takes more than a few hours: what was measured is recorded, an item is opened in the analysis, and work returns to the priorities.

## What remains of ADR-0032 and ADR-0033
- ADR-0032: the layers and the components remain; the "single command" and the per-engine matrix become M7 and M10.
- ADR-0033: everything remains (one root or one prefix per application, without root; Hangover as the component for Windows, accepted on 2026-10-14). The steps done by hand for Epic (Wine Mono, Epic Online Services, per-application settings, x64 DXVK, llvmpipe filter) become automatic in M7.
- ADR-0031 (multiarch root for `steam.sh` under armor): superseded. The Steam client in use is the native ARM64 one; the i386 client under armor serves only as a fallback on 16K systems (M9), and there the root will be decided if it is needed.

## Alternatives considered
- **Keeping milestones M7–M10 and adding priorities on top:** four overlapping lists, with the same work written in several places; discarded: the roadmap was reworked as a whole and what no longer serves the goal was removed.
- **Making the armor engine the path for Windows too, right away** (armor behind WoW64/ARM64EC): years of work before a result, while Hangover already works on 4K systems; it stays in M9 for the systems where Hangover does not start.

## Consequences
- \+ A single list of priorities, verifiable on the project page.
- \+ The work already done stays: engine, tests, oracles, `armor install`, `armor engine fetch`.
- − The README percentages restart lower: the new milestones are bigger than the old ones.
- − The references to "M9 stage N" and "M10 stage N" in earlier ADRs must be read with the correspondence table in `roadmap.md`.
