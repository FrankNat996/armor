# 0007 — Project name: armor

- **Status:** Accepted (2026-10-12)
- **Date:** 2026-10-12

## Context
The initial working name was `transmute`, which coincides with Rust's `std::mem::transmute`: a problem for searches and a possible source of confusion.

## Decision
The project is called **`armor`**.
- Repository: https://github.com/FrankNat996/armor
- Binary: `armor`
- Workspace crates: prefix `armor-` (`architecture.md`)

## Consequences
- The local development directory is still called `transmute`. Renaming it changes the local paths of the development tools.
- ⚠️ Before publishing crates or packages, it must be verified that the name is free on crates.io and that no relevant projects with the same name exist.
