# 0036 — Linux applications: Debian's live indexes, verified by signature, not pinned

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- Context: ADR-0033 (`armor install`), ADR-0034 (ease for everyone), ADR-0014 (the test roots, pinned).

## Context
`armor install` of an x86-64 `.deb`, AppImage or `.tar.*` archive downloads the Debian (or Ubuntu) amd64 packages the application needs (`debs.rs`). M7's `.deb` item first asked for pinned indexes, for reproducible installations. Today armor takes the archive's current `InRelease`, verifies it with `gpgv` and the archive keyring, checks `Packages.xz` and `Contents-amd64` against the SHA-256 in that signed file and every package against the index, and keeps the index for a day.

## Decision
1. **The live indexes, verified by signature**, as apt does: no pinned date.
2. Reproducibility stays where it is needed for measurements: the test roots of `fetch-rootfs` are pinned (ADR-0014), and the compatibility matrix records the versions of what was tested.

## Alternatives considered
- **Indexes pinned through `snapshot.debian.org`:** the same installation every time, but slower, not always reachable, and without security updates until the date is moved by hand. Not chosen.
- **Both, with an option to pin a date:** more code and more tests for a need nobody has yet. Not chosen.

## Consequences
- \+ Applications get the distribution's current libraries, security updates included, with the same checks as apt.
- − Two installations a month apart can get different library versions: a problem that appears with one version may not appear with another. The matrix records the versions tested.
