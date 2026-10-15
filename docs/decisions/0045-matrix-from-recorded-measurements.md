# 0045 — The compatibility matrix generated from recorded measurements

- **Status:** Accepted (2026-10-15)
- **Date:** 2026-10-15

## Context
Roadmap M10 asks for a compatibility matrix produced by the measurements (application × engine × CPU), not written by hand, with the result, the engine, the date and the command that reproduces each entry. Until now `COMPATIBILITY_MATRIX.md` was prose written after each measurement: precise, but nothing ties a sentence to a run, and nothing says when a number went stale.

## Decision
- **Results files:** `docs/results/<machine>.tsv`, one per machine, committed. A machine is a CPU, a page size, a system and a place (native, KVM virtual machine, container); its file starts with those facts as read from it (`lscpu`, `getconf PAGESIZE`, `uname -r`, `/etc/os-release`). Each result is one tab-separated line: kind (suite, application, performance), name, engine, reference, result, date, commit, command. A new run of the same kind, name, engine and reference replaces the line.
- **Producers:** the commands that measure write their own lines when given `--report`: `difftest` (each mode: qemu, saved results, the hardware oracle, the native oracle; each engine), `distro-check`, `bench`, and a new `app-check`, which installs pinned real applications into a throwaway data directory, runs them under Xvfb and records what it could check (a window, an archive identical to a native build). The GitHub runner's chain writes its file as a build artifact, which is downloaded and committed; CI never pushes.
- **By hand:** what cannot run unattended (Steam and its games, sign-ins) is in `docs/results/by-hand.tsv`, with the same fields and a date, and is rendered in its own section.
- **Rendering:** `cargo xtask matrix` writes the generated sections of `COMPATIBILITY_MATRIX.md` and a summary table in `README.md` between markers, like `progress` and `coverage`; `--check` fails when they are stale, and the chain runs it. The prose notes per application stay, below the generated tables, as the history behind them.

## Alternatives considered
- **JSON results:** a parser and a writer in xtask, which has no dependencies; one line per result diffs better.
- **Results only as CI artifacts:** they expire and depend on GitHub; committed files keep the history with the code.
- **Keeping the matrix by hand with dates:** the date would say when someone wrote, not what was run.

## Consequences
- \+ Every generated entry has its command and the commit it measured; a stale entry shows its date.
- − Recording needs `--report` runs on each machine, and the virtual machines and containers of M9 to be started again for theirs.
- − Applications that need an account or a store stay by hand.
