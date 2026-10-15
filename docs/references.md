# References

⚠️ **The URLs were not opened when this list was written** (2026-10-12): they are written from memory. At the first consultation, verify them and remove this warning.

## Architecture manuals
| Document | Use | URL |
|---|---|---|
| Intel 64 and IA-32 Architectures Software Developer's Manual (SDM) | Final reference for semantics, encoding and flags | https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html |
| AMD64 Architecture Programmer's Manual | Second reference, useful where Intel and AMD differ | AMD website (search for "AMD64 Architecture Programmer's Manual") |
| x86 instruction reference (extract of the SDM) | Quick lookup by instruction | https://www.felixcloutier.com/x86/ |
| sandpile.org | Opcode tables and encoding details | https://www.sandpile.org/ |
| System V AMD64 psABI | Calling conventions, initial stack, auxv | https://gitlab.com/x86-psABIs/x86-64-ABI |
| Arm Architecture Reference Manual (A-profile) | AArch64 semantics, memory model | https://developer.arm.com/documentation/ddi0487/latest |

## Graphics
| Resource | Use |
|---|---|
| Khronos OpenGL registry, `xml/gl.xml` and `xml/glx.xml` — https://github.com/KhronosGroup/OpenGL-Registry | Apache-2.0 (✅ SPDX line in each file's header, 2026-10-15). Copied unchanged into `third_party/khronos/` (commit and SHA-256 in its `README.md`); `cargo xtask gl-gen` generates from them armor's OpenGL table and the x86-64 `libGL.so.1` stubs (ADR-0041), which the package ships with this notice in its `copyright` file |
| OpenGL and GLX specifications — https://registry.khronos.org/OpenGL/ | Semantics of the functions armor passes to the host (which pointers they read or write, what they return) |

## Linux kernel
| Resource | Use |
|---|---|
| `arch/x86/entry/syscalls/syscall_64.tbl` (Linux sources) | x86_64 syscall numbers |
| `include/uapi/asm-generic/unistd.h` (Linux sources) | aarch64 syscall numbers. On the host: `/usr/include/asm-generic/unistd.h` ✅ |
| x86_64 headers after installing `libc6-dev-amd64-cross` | Struct layouts and syscall numbers: `/usr/x86_64-linux-gnu/include/asm/unistd_64.h` and neighbours (`environment.md`; used by `cargo xtask coverage`) |
| man-pages (`man 2 <syscall>`) | Syscall semantics |

## Similar projects
| Project | License | Permitted use |
|---|---|---|
| box64 — https://github.com/ptitSeb/box64 | MIT (✅ `LICENSE` of v0.4.4, 2026-10-14) | Read it; engine measured on the same cases (ADR-0032, `tech/engines.md`). If code is reused, attribute it. |
| FEX-Emu — https://github.com/FEX-Emu/FEX | MIT (✅ `LICENSE` of FEX-2610, 2026-10-14) | Read it; component (inside Proton ARM64) and measured engine (ADR-0032, `tech/engines.md`). If code is reused, attribute it. |
| Hangover — https://github.com/AndreRH/hangover | Wine LGPL-2.1, FEX DLLs and `wowbox64` MIT (✅ `copyright` files of the Hangover 11.16 packages, 2026-10-14) | Run it as the component for Windows programs (Wine ARM64X + FEX), measured on 2026-10-14 (ADR-0033, `tech/engines.md`); `armor engine fetch` downloads it (it is not redistributed with armor). LGPL code: do not copy it into armor. |
| DXVK — https://github.com/doitsujin/dxvk | zlib (✅ repository license read from the GitHub API, 2026-10-14) | Direct3D 8–11 on Vulkan in the prefixes of `armor install`; build 2.7.1 comes from the Hangover release (`armor engine fetch`), and its x64 and x32 DLLs go into the prefixes (the ARM64EC one stopped the Epic Games Launcher: `tech/engines.md`). |
| qemu — https://gitlab.com/qemu-project/qemu | **GPL-2.0** ⚠️ | **Only run it as an oracle and read it to understand. Do not copy code**, otherwise armor would have to become GPL. Useful parts: `target/i386` (semantics), `linux-user` (syscall translation). |

## Rust crates (ADR-0008: only `libc` adopted; licenses to verify on adoption)
| Crate | Possible use |
|---|---|
| `libc` | **Adopted** (ADR-0008): host syscalls and structs |
| `rustix` | Rejected (ADR-0008) |
| `object` / `goblin` | Not used: ELF parser written by us (ADR-0008), which also handles PIE and `PT_INTERP` (M5). |
| `dynasmrt` (dynasm-rs) | Not used: AArch64 encoder written by us, verified against the GNU assembler by `cargo xtask encoder-check` (ADR-0021 §8) |
