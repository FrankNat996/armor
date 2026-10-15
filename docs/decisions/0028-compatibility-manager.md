# 0028 — Compatibility Manager: automatic path, profiles, diagnostics

- **Status:** Accepted (2026-10-13)
- **Date:** 2026-10-13

## Context
The guiding document (§5) asks that armor choose the execution path by itself, with per-application profiles, controlled fallbacks, a diagnostic mode and concrete suggestions when something does not work, without silent fallbacks that compromise correctness. Today:
- `armor inspect` analyses the file (format, architecture, linking, libraries, extensions);
- `armor <program>` runs an x86-64 with the interpreter, the JIT only with `--jit`;
- the other formats are rejected by the loader.

## Decision
- **Automatic path** of `armor <program>`:
  - **x86-64 Linux** → JIT (`--interpreter` for the interpreter, `--lockstep` as today). The JIT is as verified as the interpreter (`difftest`, lockstep, hardware oracle) and leaves to the interpreter, instruction by instruction, what it does not translate: it is a controlled fallback, not a silent one;
  - **AArch64 Linux** → run directly with `execve`, with a warning on stderr (inside `--sandbox`, in the same isolation);
  - **`#!` scripts** → the named interpreter, resolved as the kernel would; under armor if it is x86-64, native if it is AArch64;
  - **i386, Windows PE, other architectures** → rejected with the reason and what to do (the milestone, or the right tool).
- **Per-application profiles:** `~/.config/armor/profiles/<name>.conf` (`$XDG_CONFIG_HOME` if set), `key = value` lines, comments with `#`. The profile is chosen with `--profile NAME`, otherwise the one with the name of the program's file, if it exists. Keys:
  - engine;
  - guest root;
  - sandbox, allowed paths and network (ADR-0027);
  - resource limits;
  - environment variables to add or remove;
  - `argv0`;
  - diagnostics.

  Command-line options take precedence over the profile. An unknown key is an error, not an ignored option. Hand-written parser, no new dependency.
- **Diagnostics** (`--diagnose`, `--diagnose=FILE`): at the end a report with:
  - the chosen path and why, and the profile used;
  - the cause of the exit (exit code, signal);
  - the syscalls rejected with ENOSYS and the unsupported ones;
  - the unsupported or undeclared instructions;
  - the fault signals;
  - the JIT statistics;
  - a concrete suggestion for every problem found.

  The report contains no arguments, environment variables, file contents or guest data: only syscall names, mnemonics and numbers.
- **Suggestions even without `--diagnose`**, when armor stops: for every known cause (unsupported instruction, undeclared extension, library missing from the guest root, non-executable format) a "suggestion:" line with a concrete action.

## Update of 2026-10-15: scripts of the host system
A `#!` script in a system directory of the host (`/usr`, `/bin`, `/sbin`, `/lib`, `/lib64`, `/etc`) that the guest root does not override is the host's own program, written for the host's tools: it runs on the host with the host's interpreter, both as `armor <script>` and from a guest's `execve` (`process::is_host_system_script`). Before, its interpreter was resolved in the guest root: a game run with a root holding x86-64 `dash` and coreutils opened a web page through Debian's `/usr/bin/chromium` launcher, whose `uname -m` then said `x86_64`, and the launcher looked for SSE3 in the host's `/proc/cpuinfo` and refused to start Chromium. Scripts anywhere else (a game's launcher next to its files, a test) and those the root provides keep the root's interpreter. Test: `host_system_scripts_use_the_host_interpreter` (`crates/armor/tests/run_paths.rs`, fails without the rule). Not a `difftest` case: `qemu-x86_64 -L` resolves the interpreter in the root.

## Alternatives considered
- **Interpreter by default**: more conservative, but the JIT is as verified as the interpreter and much faster: for example the python traceback 0.41 s against 3.76 s, `bench float` 0.27 s against 11.4 s (`tech/performance.md`).
- **TOML with a dependency:** more expressive; the `key = value` format is enough for the planned keys, without new dependencies.
- **Profiles in the guest root:** they travel with the environment, but are not per user; can be added later as a second level.

## Consequences
- \+ `armor program` does the right thing for the formats it knows, and says why when it cannot.
- − The default changes (interpreter → JIT): `difftest` and `run` pass the engine explicitly for both.
- − A profile with a wrong key stops armor (intended: no silently ignored configuration).
