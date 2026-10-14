# 0006 — License: MIT OR Apache-2.0

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
The project aims to be useful to the community: it must be possible to reuse it, include it in distributions and integrate it into other tools. Constraint: it must not contain GPL code (see qemu in `references.md`).

## Decision
The project uses the dual license **`MIT OR Apache-2.0`**, at the choice of whoever reuses it. The texts are in `LICENSE-MIT` and `LICENSE-APACHE` at the root of the repository (copied from GitHub's licenses API).
The `Cargo.toml` files state `license = "MIT OR Apache-2.0"` (in `[workspace.package]`, inherited by the crates).

## Alternatives considered
- **MIT only:** simpler, but without Apache-2.0's explicit patent clause.
- **GPL-3.0:** copyleft. Discarded in favour of maximum reusability.
- **Deciding later:** without a license nobody can legally reuse the code.

## Consequences
- \+ It is the same convention as the Rust ecosystem, so compatible with almost all dependencies.
- − Code cannot be copied from GPL projects (qemu). MIT code (box64, FEX) can be reused with attribution.
- ⚠️ Future dependencies must be checked for license compatibility (`references.md`).
