# 0039 — armor updates itself from its releases

- **Status:** Accepted (2026-10-15)
- **Date:** 2026-10-15
- Context: ADR-0034 (ease for everyone), ADR-0035 (the window), M7's release packages.

## Context
armor is installed from the `.deb` of its GitHub releases. Without an update mechanism a user keeps the version first installed (with 0.2.0 installed, the window's new menu never arrived).

## Decision
1. **`armor update`** reads the repository's list of releases (`api.github.com/repos/FrankNat996/armor/releases`), takes the newest non-draft one with an `armor_<version>_arm64.deb` asset, **pre-releases included**, and compares its version with the installed package's (`dpkg-query`, `dpkg --compare-versions`). If newer, it downloads the package and checks its size and the SHA-256 that GitHub publishes for the asset (`digest`), then installs it with `pkexec apt-get install` (the desktop's password dialog); without `pkexec` it says which `sudo apt install` to run. `armor update --check [--porcelain]` only checks. armor not installed from its package (built from source) is not updated.
2. **The window** checks at most once a day when it opens (a time stamp in `$XDG_CACHE_HOME/armor/update-checked`), silently; if a newer armor exists it shows a notice with **Update**; nothing is installed without that click and the password. Its menu has **Check for updates**, which says the result either way. After the update the notice offers **Restart** (the new window).
3. Installed applications are never touched by an update.

## Alternatives considered
- **Fully automatic updates:** they would still need the administrator's password (or a privileged service), and would change armor without the user knowing. Not chosen.
- **Only a command:** the user would have to remember it. Not chosen.
- **Stable releases only:** every release so far is a pre-release; nobody would get an update. Not chosen.

## Consequences
- \+ Users get new versions with one click; the package is verified like the engine.
- − The window contacts GitHub once a day (no data beyond the request itself). The trust is GitHub's HTTPS and its asset digest.
- − Updating needs the administrator's password, as installing armor did.
