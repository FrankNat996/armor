# 0037 — Chromium-based applications move from M7 to M8

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- Context: ADR-0034 (priorities), `roadmap.md` (M7, M8).

## Context
M7 (the installer for everyone) had two Chromium-based applications in its items: the Epic Games Launcher up to sign-in (Windows; its sign-in page is not drawn because the Chromium process that draws it terminates under FEX) and Spotify usable (x86-64 `.deb`; its renderer processes exit with status 0 under armor, then the main process stops on purpose). Both install and start; both fail inside Chromium's multi-process model, partly in external components (FEX, CEF), work whose length is not known. Every other part of M7 does not depend on them.

## Decision
1. The item "Chromium-based applications (the Epic Games Launcher up to sign-in, Spotify usable)" moves to **M8 — Games and Steam**: Epic is a game store, and launchers built on Chromium/CEF are a problem games share.
2. They stay in the compatibility matrix at level 1 with what was measured.

## Alternatives considered
- **Keep them in M7:** M7 would stay open on work that is mostly outside armor, while the rest of it (runtime components, release) is done. Not chosen.
- **Drop them from the roadmap:** they would remain only as known cases. Not chosen: they are common applications the goal (ADR-0034) covers.

## Consequences
- \+ M7 closes when its own work (the runtime components of Windows programs) is done.
- − The README and the matrix keep saying that Chromium-based applications do not work yet.
