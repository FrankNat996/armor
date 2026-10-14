# 0040 — Windows programs run with Steam's ARM64 Proton when Steam is installed

- **Status:** Accepted (2026-10-15)
- **Date:** 2026-10-15
- Context: ADR-0033 (`armor install`, Hangover for Windows programs), ADR-0038 (Steam installed by armor), `tech/steam.md` (the first game with ARM64 Proton).

## Context
Windows programs run today with Hangover 11.16 (Wine ARM64X with FEX), downloaded by `armor engine fetch`, with DXVK and Wine Mono added by armor. On 2026-10-15 Valve's "Proton Experimental (ARM64)", installed by the Steam client that armor installs, ran Doki Doki Literature Club with the Pi's GPU and sound; started by hand outside Steam (its ARM64 Steam Linux Runtime 4 container, `_v2-entry-point`, then `proton waitforexitandrun`), it installed Notepad++ silently in 11 s (27 s with Hangover) and opened it. Proton carries DXVK, vkd3d-proton, Wine Mono and Gecko, and Valve's fixes for thousands of games. It is distributed only through Steam.

## Decision
1. **When the Steam client installed by armor has "Proton Experimental (ARM64)" and the ARM64 Steam Linux Runtime 4**, `armor install` of a Windows program uses them: the program's prefix is `<app>/proton/pfx` (`STEAM_COMPAT_DATA_PATH=<app>/proton`), and `armor install`/`armor run` start `SteamLinuxRuntime_4-arm64/_v2-entry-point --verb=waitforexitandrun -- <Proton>/proton waitforexitandrun <program>`, with `STEAM_COMPAT_CLIENT_INSTALL_PATH` pointing to the client. `app.conf` records `engine = proton`. Steam does not need to be running.
2. **Otherwise Hangover**, as today (`engine = hangover`, the default for applications installed before).
3. **Menu entries from the prefix's shortcuts, read by armor:** Proton does not run Wine's menu builder, so armor reads the `.lnk` files under the prefix's Start Menu itself (target, arguments, icon location) and extracts the icons from the programs (PE resources), for both engines.
4. **Without Steam:** building Proton ARM64 from Valve's sources is to be measured before it is planned; until then, Hangover.

## Alternatives considered
- **Hangover only:** works without Steam, but without Proton's game fixes and with DXVK and Wine Mono added by hand.
- **Redistributing Proton's ARM64 build with armor:** it comes from Steam's content, under Steam's terms; not done.

## Consequences
- \+ Windows programs and games get Valve's maintained stack, with the Pi's GPU through the host's Mesa and sound through PipeWire.
- − It depends on the Steam client being installed and up to date (Proton updates with it); a Proton update can change a program's behaviour.
- − Like Hangover, it needs 4K pages.
