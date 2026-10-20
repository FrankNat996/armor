# 0047 — armor as the engine of Windows programs, behind Wine's WoW64 and ARM64EC interfaces

- **Status:** Accepted (2026-10-20)
- **Date:** 2026-10-20

## Context
Windows programs run under Hangover (ADR-0033): Wine built for ARM64, with an x86 emulator loaded by Wine as a DLL. Hangover 11.16 uses FEX for x86-64 programs (`libarm64ecfex.dll`) and box64 for 32-bit ones (`wowbox64.dll`). armor's engine never runs them: the gains of M11 reach Linux programs only. Item 8 of M11 asks for armor's engine there too.

How Wine loads an emulator, checked on 2026-10-20 in Wine's source (master `840a939e`), FEX's (`d7b8e149`) and box64's:
- **32-bit programs (WoW64).** `wow64.dll` loads the DLL named by `HKLM\Software\Microsoft\Wow64\x86` (default on ARM64: `xtajit.dll`, which Wine does not ship) and resolves its exports by name (`dlls/wow64/syscall.c`, `process_init`). Required: `BTCpuProcessInit`, `BTCpuGetBopCode`, `BTCpuSimulate`, `BTCpuGetContext`, `BTCpuSetContext`, `BTCpuResetToConsistentState`, `__wine_get_unix_opcode`. Optional: `BTCpuThreadInit`, `BTCpuFlushInstructionCache2`, `BTCpuNotifyMemoryAlloc`/`Free`/`Protect`/`Dirty`, `BTCpuNotifyMapViewOfSection`/`UnmapViewOfSection`, `BTCpuNotifyReadFile`, `BTCpuProcessTerm`, `BTCpuThreadTerm` and others. The guest state is a `WOW64_CONTEXT`; a guest syscall jumps to the address `BTCpuGetBopCode` returned, and the emulator calls `Wow64SystemServiceEx` itself; `NtFlushInstructionCache` reaches `BTCpuFlushInstructionCache2`.
- **64-bit programs (ARM64EC).** `ntdll` loads the DLL named by `HKLM\Software\Microsoft\Wow64\amd64` (default `xtajit64.dll`, a stub in Wine) and wires its exports `DispatchJump`, `RetToEntryThunk`, `ExitToX64`, `BeginSimulation` and `BTCpu64*` into ARM64EC's dispatch (`dlls/ntdll/signal_arm64ec.c`); the DLL must itself be an ARM64EC image. x64 and ARM64EC code call each other through Microsoft's thunks and the EC code bitmap; a guest `syscall` is raised as `STATUS_EMULATION_SYSCALL`. Wine 10.0 lists this interface as a highlight.
- **Both are Windows DLLs**, running inside the Windows process: FEX builds them with llvm-mingw, statically, without a C runtime, importing only `ntdll` (and `wow64.dll`), and marks them as Wine builtins. A Linux helper is reached through Wine's unix calls, when needed.
- **Rust:** `aarch64-pc-windows-gnullvm` is a Tier 2 target with host tools; `arm64ec-pc-windows-msvc` is Tier 2 without host tools, and there is no ARM64EC target for the GNU-LLVM toolchain. No emulator backend in Rust is known.
- box64's WoW64 backend is in use (Hangover's default for 32 bits); its ARM64EC one (`box64ec.dll`) is in progress, interpreter only (2026-10-05).

## Decision
armor's engine becomes a Wine CPU backend, in two stages, each opt-in until it passes the same checks as the engine it replaces, then the default:

1. **WoW64 first: `armor-wow64.dll`**, for 32-bit Windows programs. The interface is the smaller and the better known one (`BTCpu*`), armor already translates i386 code (ADR-0029, compatibility mode), and the slot is the one box64 fills today. A new crate builds a PE DLL for `aarch64-pc-windows-gnullvm`, `no_std` with its own small runtime, importing only `ntdll` and `wow64.dll`. It reuses `armor-x86`, `armor-cpu` (the interpreter) and `armor-jit`'s translator; what depends on Linux (the code buffer's `mmap`, the permission table, signals) gets a Windows side behind the same interfaces: `NtAllocateVirtualMemory` and `NtProtectVirtualMemory` for the code buffer, Wine's memory notifications for invalidating translations, Wine's exception dispatch for faults. Guest permissions are those of the process's own pages (Windows memory is managed by Wine, not by armor): no software table.
2. **ARM64EC next: `armor-arm64ec.dll`**, for x86-64 Windows programs, after a measurement of the toolchain (Rust's `arm64ec-pc-windows-msvc` with LLVM's `lld-link`, without MSVC) and of what the ARM64EC side needs beyond WoW64 (the thunks, the EC bitmap, `STATUS_EMULATION_SYSCALL`).
3. **How it is chosen:** `armor install` and `armor run` set the registry key (`Wow64\x86`, `Wow64\amd64`) in the application's prefix to armor's DLL, or to Hangover's, with a per-application profile setting (ADR-0028); `cargo xtask app-check` runs the Windows applications with either engine, and the matrix records both (ADR-0045).
4. **Verification:** the same i386 and x86-64 test programs built for Windows (mingw) run under both backends and compare their output; lockstep inside the DLL (JIT against interpreter) as on Linux; then the Windows applications of `app-check`.

## Alternatives considered
- **Keep FEX and box64 for Windows:** no work, but item 8's goal (Windows programs gain from armor's engine) is not met.
- **ARM64EC first:** most Windows programs and games are 64-bit, but the interface is larger, needs an ARM64EC image (no GNU-LLVM Rust target) and is still young in Wine; the WoW64 stage builds the Windows side of the engine on the simpler interface first.
- **The engine as a Linux process talking to Wine:** not what Wine's interfaces are; every guest call would cross processes.

## Consequences
- \+ Windows programs run on the same engine that `difftest` and lockstep check, and gain from M11.
- \+ The engine gets a second host environment, which keeps its Linux parts behind interfaces.
- − A new target and toolchain in the chain (`aarch64-pc-windows-gnullvm`, later ARM64EC), and Windows test programs built with mingw.
- − Inside Wine, armor no longer owns memory and faults: what `armor-mem` and `armor-linux` do for Linux programs, Wine does there; exactness depends on Wine's emulation of Windows, as for FEX and box64.
- − Months of work; the ARM64EC stage depends on a toolchain measurement not yet done.

## Implementation notes
- **Stage 1 (2026-10-20):** `armor-wow64.dll` (`wow64/`, `cargo xtask wow64`; `tech/jit.md`, "Inside Wine"). Two details differ from the plan above. The DLL is built for `aarch64-pc-windows-msvc` with LLVM's `lld-link` and import libraries from `llvm-dlltool`, without MSVC or a Windows SDK and without llvm-mingw: the same linker and kind of target the ARM64EC stage needs (`arm64ec-pc-windows-msvc`). The backend is chosen with Hangover's `HODLL` variable rather than the registry key: Hangover's `wow64.dll` reads `HODLL` (`tech/engines.md`, "How the emulator is chosen"), and `armor run` already sets it; the DLL is copied into the prefix's `system32`, where Wine finds it. The setting is `x86-engine = armor|fex` in the application's `app.conf` (or `ARMOR_WINDOWS_X86_ENGINE` for every application), and `cargo xtask app-check --x86-engine armor` checks the applications with it.
- **Default:** FEX stays the default while armor's backend is slower than FEX on the same program (7-Zip x86 on 12 MB: 16.5 s against 8.7 s, `tech/engines.md`), as the goal of M11 is the faster engine; armor's backend becomes the default once it passes `app-check` and is not slower.

