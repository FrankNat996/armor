# 0035 — The window: a native GTK 4 application, outside the engine's workspace

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- Context: M7 (`roadmap.md`), ADR-0033 (`armor install`), ADR-0034 (ease for everyone). Amends ADR-0008 for this one program.

## Context
M7 asks for a minimal window: the installed applications, install, open, remove. Until now armor has been a command-line program with one dependency, `libc` (ADR-0008), and the verification chain builds and tests the whole workspace. A native GTK window needs the GTK 4 Rust bindings (`gtk4` and the crates under it) and the GTK development files on the machine that builds it (`libgtk-4-dev`).

## Decision
1. **A separate program, `armor-gui`,** in its own Cargo workspace (`gui/`), not a member of the engine's workspace. The engine crates keep the rule of ADR-0008 (only `libc`); `cargo build`, `cargo test --workspace` and the verification chain do not need GTK.
2. **The window drives the `armor` command** and does not duplicate its logic: it reads `armor list --porcelain` (one line per application or menu entry, tab-separated, a stable format for programs), and runs `armor install`, `armor run`, `armor remove` and `armor engine fetch`, showing their output.
3. **Contents:** the installed applications with their menu entries (Open), Remove with a confirmation, Install (a file chooser for `.exe` and `.msi`), and the state of the Windows engine with a button to fetch it.
4. **Dependencies:** `gtk4` from crates.io, pinned in `gui/Cargo.lock`; GTK 4 from the distribution (the Raspberry Pi's desktop already has `libgtk-4-1`; building needs `libgtk-4-dev`). Licences: gtk-rs is MIT, GTK 4 LGPL-2.1+, linked dynamically.
5. **Packaging:** `cargo xtask package` builds `armor-gui` too and adds it to the package, with a menu entry "armor" (System).

**Note (2026-10-14):** the window was redone, more modern and responsive, within this decision: still `gtk4` only (no libadwaita, which the Raspberry Pi's system does not carry), with its own CSS; cards with icons, search, a progress panel, drag and drop, and refresh when the applications change (`architecture.md`). Point 3 above describes the first version; the file chooser now also offers `.deb`, AppImages and `.tar.*` archives.

## Alternatives considered
- **A local web page served by armor:** no new dependency, but it opens in a browser instead of looking like a desktop application. Not chosen.
- **zenity dialogs:** very simple, but limited (no list with actions) and dependent on zenity being installed. Not chosen.
- **GTK inside the `armor` binary:** the engine would carry GTK and its dependencies, and every build and test of the engine would need the GTK development files. Rejected: the window is a separate program.

## Consequences
- \+ A native window on the Raspberry Pi desktop (GTK is its toolkit), without changing the engine.
- \+ The window and the command line behave the same, because the window runs the command.
- − A second workspace to build and to keep formatted and lint-free (`cargo xtask package` builds it; its checks are added to the verification chain).
- − Building the package needs `libgtk-4-dev` and network access for the crates the first time.
