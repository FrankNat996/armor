# 0038 — Steam installed by armor: Valve's ARM64 client from Valve's manifest

- **Status:** Accepted (2026-10-15)
- **Date:** 2026-10-15
- Context: M8 (`roadmap.md`), ADR-0032 (the best existing component for each layer), ADR-0033 (`armor install`), ADR-0034 (ease for everyone), `tech/steam.md`.

## Context
Valve's native ARM64 Steam client runs on the Raspberry Pi 5 with the 4K kernel (`tech/steam.md`), but on 2026-10-14 it was downloaded and started by hand with a script outside the repository. M8 asks for it to be installed by armor. The client updates itself: whatever version armor fetches is replaced within a minute of the first start, and Valve's server keeps only current packages.

## Decision
1. **`armor install steam`** (and "Install Steam" in the window) installs the client as an application of kind `steam` in `$XDG_DATA_HOME/armor/apps/Steam/`: the client in `Steam/`, its own `HOME` in `home/` (with the `~/.steam/steam`, `root` and `bin64` links Valve's `steam.sh` would make), a menu entry "Steam" that runs `armor run Steam`.
2. **Source and verification:** Valve's manifest `https://client-update.steamstatic.com/steam_client_linuxarm64`, read over HTTPS and not pinned (the client replaces itself anyway); the three packages that start it (`steam_linuxarm64`, `bins_linuxarm64`, `bins_linuxarm64_linuxarm64`) are downloaded from the same server and checked against the size and SHA-256 the manifest gives, then unpacked with `unzip` (the programs made executable: the zips carry no permissions). Packages are cached in `$XDG_DATA_HOME/armor/cache/steam/`.
3. **Host requirements, checked before downloading:** 4K pages (else a message with the Raspberry Pi's `kernel8.img` line); the three host libraries the client loads (GTK 2, IBus, PipeWire): armor's package recommends them, and if they are missing `armor install steam` stops with the `sudo apt install` line. armor itself never needs root.
4. **Running:** `armor run Steam` starts `steamrtarm64/steam` with that `HOME` (and the desktop's `XAUTHORITY` from the real home), starts it again when it exits with 42 (an update done, as `steam.sh` does; at most 5 times), and reports failures like any application.

## Alternatives considered
- **Valve's `steam` Debian package (`steam-launcher`):** it is for x86 (`i386` bootstrap) and installs system-wide with root.
- **Pinning the client's packages:** they disappear from Valve's server at the next client release and the client updates itself on its first start; pinning would only make the installation break sooner.
- **The user's real `HOME`:** the client would write `~/.steam` and `~/.local/share/Steam` outside armor's directory, and `armor remove` could not remove it cleanly.

## Consequences
- \+ Steam from the menu or the window, no script, no root; Windows games through the Proton the client installs (with Valve's FEX layer), x86 Linux games under armor.
- \+ `armor remove Steam` removes the client and its games completely.
- − The trust is Valve's HTTPS server and manifest, as for the client's own updates; there is no armor-side pin.
- − Games downloaded by Steam live inside the application's directory (they can be large).
