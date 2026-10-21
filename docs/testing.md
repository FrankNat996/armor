# Test strategy

**Status** (2026-10-14). ADR-0004 accepted. Implemented:
- Rust unit tests (226 in the chain of 2026-10-14 on 4K pages, plus `dump_translation`, a tool ignored by `cargo test`: `tech/performance.md`), among them the comparison of the JIT with the interpreter on random programs and the fuzzing tests;
- guest programs in assembly (dumps of GPR and xmm registers, flag masks) and C programs with static, static-pie and dynamic glibc;
- automatic armor/qemu comparison (`cargo xtask difftest`): **397 cases, all green** with the interpreter, with the JIT and in lockstep in the chain of 2026-10-14 (on 16K pages and then on 4K pages), plus 37 only with `--oracle` (`HARDWARE_ONLY`, below); `--oracle` against the Xeon: 216 static cases out of 216. Then, still on 2026-10-14 (Doki Doki Literature Club, `tech/steam.md`), the C sources `fork_threads`, `memfd`, `pi_mutex` (x86-64 and i386), `sysv_shm` and `drm_version` (x86-64) added 24 cases, each verified identical to qemu (and to the Xeon where it runs), `bad_ioctl` 3 cases only with `--oracle`, and `sched` (x86-64 and i386) another 6. Full chain of 2026-10-14, on the 4K kernel: `difftest` **427 cases out of 427** with the interpreter and with the JIT, 427 out of 427 in lockstep, `--oracle` against the Xeon 236 out of 236, 226 Rust tests. In total: 72 assembly programs (66 x86-64, 6 i386), 34 C sources in `tests/guest/c/` in 3 x86-64 variants (22 also in 3 i386 variants) and 7 i386-only in `tests/guest/c32/`, 50 busybox cases, 51 coreutils and 51 i386 coreutils, 33 `tools`, 19 interpreters. Later on 2026-10-14 (Spotify, `tech/syscalls.md`): the assembly test `stack/push_pop16` and the C sources `basic_calls`, `capabilities`, `mremap_shared`, `ppoll_mask`, `sigtimedwait`, `sync_misc`, `timerfd` (`mremap_shared` and `ppoll_mask` also for i386), each run by hand with `cargo xtask run` and identical to qemu with the interpreter and the JIT and to the Xeon (`push_pop16` in lockstep too; `ppoll_mask` in `c/`, `c32/` and `c-dyn/`), ⚠️ the other variants compiled and awaiting the full chain, and `pkeys` and `data_selectors`, only with `--oracle` (table below): from the layout, 28 more `difftest` cases and 6 more only with `--oracle`; ⚠️ the totals are to be read from the next full chain. Now 73 assembly programs (67 x86-64, 6 i386) and 43 C sources in `tests/guest/c/` (24 also for i386);
- comparison of the decoder with objdump on real binaries (`cargo xtask decoder-check`) and of the AArch64 encoder with the GNU assembler (`cargo xtask encoder-check`);
- long fuzzing (`cargo xtask fuzz`) and benchmarks (`cargo xtask bench`, `tech/performance.md`).

Not implemented yet: golden files, random instruction sequences compared with qemu (today only with the interpreter).

## Verification chain
It runs in full before every commit, and every step must end with result 0:
```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo xtask difftest && cargo xtask difftest --jit && cargo xtask decoder-check target/guest/c/* target/guest/busybox/busybox target/guest/rootfs/usr/bin/ls target/guest/rootfs/usr/lib/x86_64-linux-gnu/libm.so.6 target/guest/rootfs/usr/lib/x86_64-linux-gnu/libcrypto.so.3 /usr/i686-linux-gnu/lib/libc.so.6 /usr/i686-linux-gnu/lib/libm.so.6 && cargo xtask encoder-check && cargo xtask gl-gen --check && cargo xtask matrix --check && cargo xtask progress --check && cargo xtask coverage --check && cargo xtask sandbox-check && (cd gui && cargo fmt --check && cargo clippy --target-dir ../target/gui --all-targets -- -D warnings && cargo test --target-dir ../target/gui) && (cd wow64 && cargo fmt --check && cargo clippy --release --target-dir ../target/wow64-build -- -D warnings) && cargo xtask wow64
```
When the hardware oracle is available, `cargo xtask difftest --oracle` is added (ADR-0025).

## Other engines on the same cases (ADR-0032, since 2026-10-14)
`cargo xtask difftest --engine box64 [names…]` and `cargo xtask difftest --engine fex [names…]` run the `difftest` cases under box64 or FEX instead of armor, comparing them with the same reference (qemu, or the Xeon with `--oracle`). The binaries come from `ARMOR_BOX64` and `ARMOR_FEX` (otherwise `box64` and `FEX` in the PATH); the case's guest root becomes `BOX64_LD_LIBRARY_PATH` (its x86 and i386 library directories) for box64 and `FEX_ROOTFS` for FEX. It serves to measure every engine on the same tests and to write the compatibility matrix per engine and per CPU. With `--report` the result goes into the machine's results file like armor's (ADR-0045). Results on the Pi 5 with 4K pages, 2026-10-20: armor 470/470, FEX-2610 412/470, box64 v0.4.4 254/470 (`tech/engines.md`).

## Paths for every ARMv8.0 (ADR-0032)
`ARMOR_HOST_FEATURES=baseline` makes the JIT use only ARMv8.0 instructions (no LSE, no `LDAPR`: `tech/jit.md`, "Every ARMv8.0"), even on a CPU that has them. With `cargo xtask difftest` and `--lockstep` it verifies the paths of older CPUs (Cortex-A53, A72) on this machine.

## Principle
Every guest behaviour is verified **differentially**: the same x86_64 binary runs under an oracle and under armor, and the results are compared.
The test of every instruction is written **together** with the instruction, not afterwards.

## Oracles
| Oracle | What it verifies | Tool |
|---|---|---|
| Decoder | instruction length, mnemonic, operands | `x86_64-linux-gnu-objdump -d` (unit tests and `decoder-check`) |
| Execution | registers, flags, memory, output, exit code | `qemu-x86_64 -cpu qemu64,-svm,+ssse3,+sse4.1,+sse4.2,+popcnt` (`tech/cpuid.md`) |
| i386 execution (ADR-0029) | as above, for 32-bit programs | `qemu-i386 -cpu qemu32,+sse3,+ssse3,+sse4.1,+sse4.2,+popcnt`, chosen by `difftest` and `run` when the binary is an ELF32 `EM_386` |

**qemu CPU model:** `difftest` and `run` always launch qemu with the CPU of the `QEMU_CPU` constant (`xtask/src/main.rs`; `QEMU_I386_CPU` for `qemu-i386`, which has no `qemu64` because it does not emulate long mode). It is the same one that armor declares through CPUID (ADR-0010, `tech/cpuid.md`), so glibc chooses the same variants of its functions under both. Features armor declares only on request (`tech/avx.md`) are added to both sides: those in `ARMOR_CPU_FEATURES`, if set for `cargo xtask` (for example `ARMOR_CPU_FEATURES=+xsave cargo xtask difftest` runs every case with XSAVE declared under qemu and armor), and those a case needs, listed in `CASE_FEATURES` (`xtask/src/difftest.rs`; the cases under `xsave/` run with `+xsave`, those under `avx/` with `+xsave,+avx,+avx2`, those under `avx1/` with `+xsave,+avx`, those under `bmi/` with `+bmi1,+bmi2,+abm,+movbe`).

**Guest root:** every execution of `difftest` and `run` uses the same root on both sides: `-L /usr/x86_64-linux-gnu` for qemu, `ARMOR_ROOT=/usr/x86_64-linux-gnu` for armor (constant `GUEST_ROOT`, ADR-0013). Dynamic programs take `ld.so` and `libc.so.6` from there. The `c32-dyn/` programs use `/usr/i686-linux-gnu` (`I386_GUEST_ROOT`, since 2026-10-14), with `ld-linux.so.2` and the i386 glibc.

A third, optional oracle: **real hardware**, that is the same execution tests on an x86_64 PC, if available.

**qemu is a reference, not the truth.** If armor and qemu disagree, the real hardware decides when it is available (below), otherwise the Intel SDM (`references.md`). The cases in which qemu is wrong are listed below.

### Hardware oracle (ADR-0025, since 2026-10-13)
A real x86-64 machine, reached via SSH: a Proxmox container on an **Intel Xeon X5675** (Westmere, SSE up to SSE4.2, no AVX), Ubuntu 24.04, 4K pages. The container is temporary: the verifications made stay here, dated.
- `ARMOR_ORACLE=user@host ARMOR_ORACLE_KEY=<key> cargo xtask difftest --oracle [names…]` compares armor (interpreter and JIT) with the machine instead of with qemu (`xtask/src/oracle.rs`). The programs are compiled on the Pi and copied with `scp` into `/tmp/armor-oracle`, with their name (some tests print it, busybox picks the applet by it); exit status and signals go through a marker on stderr.
- **Only static programs** (asm, `c/`, `c-pie/`, busybox): the dynamic ones depend on the Debian root; the cases with closed stdout do not cross `ssh`.
- **Cases skipped because they depend on the machine** (`ENVIRONMENT_DEPENDENT` in `difftest.rs`): `basic/cpuid` (another CPU), `sse/reciprocal` (approximations differ between processors), `busybox/ls`, `ls-long`, `pwd`, `id` (different files, owners, directory and user), `c/legacy_syscalls` (`iopl` depends on privileges and kernel configuration: EPERM there, ENOSYS under armor and qemu, both Linux answers); since 2026-10-14 `c/drm_version` and `c-pie/drm_version` (they ask the machine's GPU for the driver: the oracle has none; they are compared with qemu on the Pi, where armor and qemu see the same GPU).
- **Result** (2026-10-13): **136 static cases out of 136 identical to the hardware**, with the interpreter and with the JIT (8 skipped, above; after increment 5 of M4, with `sse/mxcsr_flags` and `sse/mxcsr_hardware`). On the first run it was 131 out of 142: the other differences came from the environment or the tool, plus the fixes below. Fixes that the hardware imposed on armor: C0/C1/C3 in a partial `fprem`, the reserved fields and the last-instruction pointers of the x87 environment, the 6 padding bytes after each register in `fxsave`.
- **Hardware-only cases** (`HARDWARE_ONLY` in `difftest.rs`): where qemu is wrong in a known way, `difftest` without `--oracle` skips them with a message. Today `sse/mxcsr_hardware` (DE, NaN propagation, FZ and DAZ in SSE), `sse/sse42_hardware` (64-bit lengths of `pcmpestri`), since 2026-10-14 `i386/basic/tls` (`qemu-i386` has the GDT of a 32-bit kernel: `set_thread_area` chooses entry 6, the Xeon and armor entry 12), `c32/signal_context` in the three variants (`qemu-i386` does not set trapno, has the FPU header of a 32-bit kernel and declares XSAVE), `c32/shared_memory` in the three variants (`qemu-i386` segfaults), `x87_transcendental` in the six variants (qemu computes the x87 transcendentals differently from the hardware, ADR-0030), `mmx/x87_state` (qemu leaves at 0 the exponent of a written MMX register), `c32/uname_machine` and `coreutils32/uname` (qemu-i386 says `i686`, a 64-bit kernel `x86_64` ✅ Xeon). Since 2026-10-14 also `bad_ioctl` (`c/` and `c-pie/`; the `c-dyn` variant does not run on the oracle: an unknown `ioctl` request on a descriptor that is not open: Linux, on the AArch64 host and on the Xeon, answers EBADF, qemu ENOTTY; table below), `c32/legacy32` in the three variants (`qemu-i386`, in the old `sigprocmask` with `SIG_SETMASK`, clears the blocked signals above 32; the Xeon and armor keep them ✅) and `scm_rights_truncated` in the six variants: two descriptors passed with `SCM_RIGHTS` to a control buffer that holds only one; the Xeon (and armor) delivers the first, sets `MSG_CTRUNC` and closes the second, qemu-x86_64 and qemu-i386 deliver the first without `MSG_CTRUNC` and leave the second open ✅ (Xeon, `c/` and `c32/`). An entry `*/name` applies to a C source in all its variants (`*/name@i386` only to the i386 ones: `hardware_only` in `difftest.rs`) and `c32/mremap`, `c32-pie/mremap`, `c32-dyn/mremap` (`qemu-i386` crashes: `page_find_range_empty: Assertion 'len != 0' failed`; the dynamic variant does not run on the oracle, so it is compared with nothing), and `c/sse_exceptions` in the three variants (unmasked SSE exceptions and FPU state on entry to a handler; the `c-dyn` variant does not run on the oracle, so today it is compared with nothing: it stays compiled like the others). Later on 2026-10-14 also `pkeys` (qemu-x86_64 answers ENOSYS to the memory-protection-key syscalls; Linux x86 on a CPU without PKU, like the Xeon, EINVAL) and `data_selectors` (reading `ds`, `es`, `fs`, `gs` with `mov r, sreg` in 64-bit mode: 0 on the Xeon, 0x2b in `ds` and `es` under qemu-x86_64), both x86-64 only, in the three variants (`c-dyn` does not run on the oracle). Table below. They make comparable with the hardware what with qemu would be left to the unit tests only.
- Probes outside the tests (in the temporary directory, for the cases that the tests mask for qemu): MXCSR flags (now in the `sse/mxcsr_*` tests), C1, x87 DE, `fprem1`, `fscale` with two NaNs, the raw x87 environment. Results in the table below, with "✅ Xeon X5675".

### Known discrepancies of qemu-x86_64 10.0.13
Verified on 2026-10-12. The reference is the SDM extract on felixcloutier.com. On these cases armor follows the SDM, so they cannot be compared with `difftest`.

| Case | SDM | qemu | armor | Where it is tested |
|---|---|---|---|---|
| `SYSCALL`: `rcx`/`r11` | `RCX := RIP` (next instruction), `R11 := RFLAGS` ✅ | leaves them unchanged | as the SDM | `armor-cpu` unit test (`hello_sequence_reaches_syscall_with_arguments`) |
| `INTO` (CE), `AAM` (D4), `AAD` (D5) | "Invalid" in 64-bit mode, #UD → SIGILL ✅ (✅ Xeon X5675: SIGILL) | executes them | SIGILL | `armor-x86` unit test (`errors`) |
| `SALC` (D6) | not documented by Intel; ✅ Xeon X5675: SIGILL | does not give SIGILL | SIGILL | `armor-x86` unit test (`errors`) |
| `LDMXCSR`/`FXRSTOR` with a reserved MXCSR bit | #GP ✅ ("For an attempt to set reserved bits in MXCSR", LDMXCSR entry, felixcloutier, 2026-10-12) | accepts it | #GP → SIGSEGV | `armor-cpu` unit test (`ldmxcsr_with_a_reserved_bit_is_a_general_protection_fault`) |
| SSE operations with **two NaNs** | the first source operand always wins ✅ (SDM vol. 1 §4.8.3.5, cited in qemu commit 62d39b28; ✅ Xeon X5675, `addsd` with QNaN/QNaN, SNaN/QNaN, QNaN/SNaN and different signs) | applies the x87 rule (QNaN before SNaN, then the larger significand); it is a known TODO in the qemu source | as the SDM | `armor-cpu` unit test (`nans_follow_the_x86_rules`); `sse/scalar_float` uses only pairs on which the two rules agree |
| x87: **stack faults** (empty register, full stack) | IE, SF, C1 and indefinite result ✅ Xeon X5675 (`fadd` on an empty stack: SW `0041`; nine `fld`: SW `3a41`; ST(0) indefinite) | does not detect them: computes with the register's content | as the SDM | `armor-cpu` unit test (`x87_stack_underflow_and_overflow_give_the_indefinite`) |
| x87: **DE** flag (denormal operand) | set ✅ (FLD entry: "#D Source operand is a denormal value"; ✅ Xeon X5675: `fld` of a denormal double and sum with an 80-bit denormal) | never set | as the SDM | unit test `x87_denormal_operands_set_de`; `c/x87` masks DE |
| SSE: **unmasked exceptions** in MXCSR | #XM → SIGFPE, destination not written, flags according to the two phases (before and after the computation) ✅ Xeon X5675 | ignores the masks: no #XM, result written | as the hardware (ADR-0024) | `c/sse_exceptions`, only with `--oracle` |
| SSE: **FZ** on a result that rounds to the smallest normal; **DAZ** in `min`/`max` | not flushed (tininess after rounding); `min` returns zero instead of the denormal ✅ Xeon X5675 | flushes the result with UE; returns the denormal | as the hardware | `sse/mxcsr_hardware`, only with `--oracle` |
| `pcmpestri`/`pcmpestrm` with **REX.W** | lengths from all of `rax`/`rdx` (2^32 + 3 counts as 16 elements) ✅ Xeon X5675 | reads only `eax`/`edx` | as the hardware | `sse/sse42_hardware`, only with `--oracle` |
| FPU state on **entry to a signal handler** | initial: xmm zeroed, MXCSR 1f80, x87 reset ✅ Xeon X5675 | that of the interrupted code | as the hardware (Linux) | `c/sse_exceptions`, only with `--oracle` |
| SSE: **DE** flag in MXCSR (denormal operand) | set ✅ Xeon X5675 (2026-10-13: `denormal + 1`, `denormal × 1`) | never set | as the hardware, in both engines (ADR-0024) | `sse/mxcsr_hardware`, only with `--oracle` (`HARDWARE_ONLY`); unit tests `flags_follow_the_hardware` (`armor-cpu`) and `float_operations_on_special_values` (`armor-jit`) |
| `mremap` with new size 0 | EINVAL ✅ (host kernel, native C program, 2026-10-13) | ENOMEM | EINVAL | `armor-linux` unit test (`mremap_errors_follow_linux`); `c/mremap` prints only the failure |
| `mremap` of an unmapped range | EFAULT ✅ (host kernel, as above) | EINVAL | EFAULT | `armor-linux` unit test (`mremap_errors_follow_linux`) |
| `mremap` and different protections or growth inside a 16K page | with 4K pages: EFAULT on mappings of a different kind, ENOMEM if it cannot grow in place without `MREMAP_MAYMOVE` ✅ (the oracle's x86 kernel, 4K pages, 2026-10-13: armor identical on all five cases of the test; the 16K host cannot show it) | as the 16K kernel: succeeds | follows 4K pages (ADR-0005) | `armor-linux` unit test (`mremap_errors_follow_linux`) |
| x87: **C1** after a rounded operation | 1 if rounded up ✅ Xeon X5675 (2026-10-13: `1/3` and `1/1.5`, rounded up, give C1 = 1) | always 0 | as the hardware | probe on the oracle; `c/x87` and `x87/stack` mask C1 for qemu |
| `ioctl` with an **unknown request on a descriptor that is not open** | EBADF: the descriptor is checked before the request ✅ (native program on the AArch64 host and on the Xeon X5675, 2026-10-14) | ENOTTY | EBADF (since 2026-10-14; Mesa makes such a call while looking for the GPU) | `c/bad_ioctl`, `c-pie/`, only with `--oracle` |
| `pkey_alloc`, `pkey_free`, `pkey_mprotect` with a key other than −1 | EINVAL on a CPU without PKU; `pkey_mprotect` with key −1 = `mprotect` ✅ Xeon X5675 (2026-10-14) | ENOSYS | as the hardware (armor's CPU model has no PKU) | `c/pkeys`, `c-pie/`, only with `--oracle` |
| `mov r, ds`/`es`/`fs`/`gs` in **64-bit mode** | 0 (Linux loads the null selector) ✅ Xeon X5675 (2026-10-14) | 0x2b in `ds` and `es` | 0 | `c/data_selectors`, `c-pie/`, only with `--oracle` |
| `recvmsg`: **`SCM_RIGHTS` that does not fit** in the control buffer | the descriptors that fit, `MSG_CTRUNC`, the others closed ✅ Xeon X5675 (2026-10-14, x86-64 and i386) | no `MSG_CTRUNC`, the others stay open (qemu-i386 too) | as the hardware | `c/scm_rights_truncated` and `c32/…`, only with `--oracle`; unit test `i386_descriptors_that_do_not_fit_are_left_out` |
| i386 `sigprocmask` (126) with **`SIG_SETMASK`** | changes only the first 32 signals, the other blocked ones stay ✅ Xeon X5675 (2026-10-14) | qemu-i386 also clears those above 32 | as the hardware | `c32/legacy32`, only with `--oracle` |
| x87: `fprem1`, quotient bits | C0, C3, C1 = Q2, Q1, Q0 of the rounded quotient ✅ (FPREM1 entry, pseudocode; ✅ Xeon X5675, four cases) | always 0 | as the SDM and the hardware | unit test `fprem1_reports_the_rounded_quotient`; `c/x87_more` prints only C2 for `fprem1` |
| x87: `fscale` with two NaNs | the x87 rule for two NaN operands: the larger significand wins ✅ Xeon X5675 (in both orders) | the NaN of ST(1) | as the hardware | unit test `fscale_with_two_nans_follows_the_x87_rule`; `c/x87_more` skips that case |
| x87: environment of `fnstenv`/`fnsave` | ✅ Xeon X5675: `ffff` in the reserved halves of the control, status and tag words and after FDS; FIP and FDP (low 32 bits), FCS `0x33`, FDS `0x2b`, FOP of the last non-control instruction | writes 0 in the reserved fields and in FOP, keeps FIP and FDP | as the hardware (since 2026-10-13; before, 0 everywhere) | probe on the oracle (byte by byte); `c/x87_more` clears those fields for qemu |
| `FXSAVE`: the 6 bytes after each x87 register | ✅ Xeon X5675: written as zero | leaves them as they were | zero, as the hardware (since 2026-10-13) | unit test `fxsave_zeroes_the_register_padding`; `sse/state` clears them for qemu |
| `FXSAVE`: FOP, FIP, FDP | fields of the saved image ✅ (FXSAVE entry) | FOP unchanged, pointers 0 | 0: armor's CPUID says AMD (ADR-0010), and on AMD `fxsave` saves them only with a pending exception ⚠️ (remembered); in `fnstenv`/`fnsave` yes (above) | `sse/state` clears FOP before the dump |
| process name (`/proc/self/comm`, `PR_GET_NAME`) | a native execution shows the program's name | `qemu-x86_64` | the guest program's name | not compared |

### Cases in which armor and qemu agree, verified on the hardware
| Case | armor and qemu | Hardware | Test |
|---|---|---|---|
| `cmpxchg r32, r32` when the comparison fails | the destination register is not written: its high 32 bits stay | ✅ Xeon X5675 (2026-10-13): the same. The doubt was that the rewrite of the destination described by the SDM would clear the high 32 bits | `alu/exchange` (also with `--oracle`) |

## Test levels
1. **Rust unit tests** (`cargo test`): decoder (bytes → `Instruction`, with the bytes taken from objdump), ALU flags, interpreter on fake memory, ELF parsing, `armor-mem` mappings, `armor inspect` verdicts on synthetic files. They do not need qemu. In addition, `crates/armor/tests/inspect.rs` launches the real `armor` binary (`armor inspect`: output, exit code, errors).
   **Guest addresses = host addresses, and the tests run in parallel** in the same process: a test that frees host pages and then expects to find them free again can see them taken by another test. Until 2026-10-13 `brk_does_not_grow_over_a_mapping` failed this way about once in two runs; now it keeps occupied the host pages it needs. The `armor-mem` tests use fixed zones, different for each test (`test_addr`).
2. **Assembly programs** (`tests/guest/asm/`): a static program without libc that:
   - prepares the registers;
   - executes the instructions to test;
   - writes to stdout the value of the registers and flags with the macros of `tests/guest/include/dump.h`;
   - exits.
   The same binary runs under qemu and under armor.
3. **C programs** (`tests/guest/c/`): they execute real glibc, including the SSE2 string functions. Each source is compiled in three variants (`C_VARIANTS` in `xtask/src/main.rs`):
   - `c/<name>`: `-static -O2 -Wall` (`ET_EXEC`);
   - `c-pie/<name>`: `-static-pie -O2 -Wall` (`ET_DYN` without interpreter);
   - `c-dyn/<name>`: `-O2 -Wall`, the toolchain default: dynamic PIE with `ld.so` and `libc.so.6` from the guest root.
4. **Real Debian binaries** (busybox, coreutils, `tools`, interpreters): below.
5. **Random programs** against the interpreter, for the JIT (below). The same sequences against qemu do not exist yet.

### What `difftest` compares
- **Two armor engines** (since 2026-10-12, ADR-0021 §9): every case runs once under qemu and twice under armor, with `--interpreter` and with `--jit` (the engine always by name: armor's default is the JIT, ADR-0028); it passes only if both executions match qemu. A failure says which engine (`FAIL name (jit): …`). `--interpreter` or `--jit` as the first argument run only one; `--lockstep` runs armor in lockstep, every translated instruction checked against the interpreter (`tech/jit.md`). A full `difftest --lockstep` takes about 2 minutes (2026-10-13); the slowest case, the python traceback, 19 s. Instructions that touch memory shared with other processes (ADR-0022) are not compared by the lockstep: it executes them once with the interpreter (`tech/memory.md`).
- **stdout**, byte by byte.
- **Termination:** exit code or signal number. A guest fault must make armor terminate with the same signal (SIGSEGV, SIGILL) with which it terminates under qemu.
- **stderr is not compared:** qemu and armor print different diagnostics. On failure armor's stderr is shown.
- **Child processes under qemu:** on this host binfmt_misc starts x86-64 children under a new qemu. `difftest` passes it the same CPU and the same root with `QEMU_CPU` and `QEMU_LD_PREFIX`. Without them, a dynamic child relaunched with `execve` does not find `ld.so` (`c-dyn/processes` exited with 255 under qemu).
- **Arguments with spaces** in the case files: in double or single quotes, without escapes; each kind can contain the other (`sh: sh -c "echo a b"`, `gawk: gawk '{ printf "%d\n", NR }'`).
- **Time limit:** every execution runs under `timeout` (120 s): a stuck guest (for example a lost wake-up between threads) makes the case fail instead of stopping `difftest`. A timeout on either side is a failure.
- **Threads:** the execution order changes from one execution to the next, so the tests with threads have an output that does not depend on scheduling. A single execution proves little: after every change to threads, repeat them many times (for example 100 executions of `c/threads` in release).
- **`fork` with threads** (`fork_threads`, 2026-10-14): the test itself must be a correct program. The first version took, in the child, a mutex that a parent thread, absent in the child, could hold at the moment of the `fork`: it sometimes hung on Linux too. Now the mutex goes through `pthread_atfork`, as a real program must do.
- **Closed stdout:** the cases of `busybox.txt` and `coreutils.txt` whose name ends with `-closed-stdout` run with stdout connected to a pipe already closed for reading, like `program | head -0`. They verify the reaction to EPIPE/SIGPIPE.

On a difference, `difftest` shows the first differing byte and the nearby bytes aligned to 8, since the dumps are 8-byte values.

### Dump macros (`tests/guest/include/dump.h`)
| Macro | Effect |
|---|---|
| `DUMP reg` | writes the 8 bytes of a 64-bit register; preserves registers and flags |
| `DUMP_FLAGS mask` | writes `RFLAGS & mask`; preserves the registers but **modifies the flags** |
| `DUMPX xmm` | writes the 16 bytes of an xmm register; preserves registers and flags |
| `SETX xmm, lo, hi` | loads an xmm register with two 64-bit halves; preserves the other registers and the flags |
| `EXIT code` | `exit_group(code)` |
| `FLAGS_STATUS` (0x8d5) | mask of the 6 status flags |
| `FLAGS_NO_AF` (0x8c5) | without AF, for the logical instructions in which AF is undefined |

The macros use only `mov`, `lea`, `pushf`, `pop`, `and` and `syscall` (plus `movdqu` for the xmm ones).

**Bootstrap:** `basic/exit42` and `basic/hello` verify `mov`, `lea`, `xor` and `syscall` without dumps, using only the exit code and stdout. The macros rest on these instructions.

### Mutation testing
To check that the tests do not pass by chance, a bug is deliberately introduced into the interpreter and one verifies that `difftest` finds it. It must be repeated when a family of instructions is added.

| Date | Bug introduced | Result |
|---|---|---|
| 2026-10-12 (M1) | AF never set by ADD | `alu/add_sub_cmp` and `alu/adc_sbb` fail |
| 2026-10-12 (M1) | 32-bit writes without clearing the high 32 bits | 10 programs out of 20 fail |
| 2026-10-12 (M2) | unsigned `pcmpgt` | `sse/integer` fails |
| 2026-10-12 (M2) | wrong OF of `ror` by 1 | **not detected** at first: two cases added to `alu/shifts`, now it fails |
| 2026-10-12 (M2) | quotient and remainder of 8-bit `div` swapped | `alu/mul_div` fails |
| 2026-10-12 (M2) | DF ignored by the string instructions | `string/ops` fails |
| 2026-10-12 (M2) | unsigned offset of `bt*` in memory | `alu/bits` fails |
| 2026-10-12 (M2) | a CPUID value changed | `basic/cpuid` fails |
| 2026-10-12 (M2) | 32-bit `cmov` without extension when the condition is false | `control/cmov_setcc` fails |
| 2026-10-12 (M2) | `O_*` flags of `openat` not translated | `busybox/ls` and `busybox/find` fail |
| 2026-10-15 (lockstep) | the translation of `movsx` from memory as a zero extension | `--lockstep`: `mov/extend`, `c/strings`, `c/printf_int`, `busybox/sort`, `coreutils/sort` fail (after lockstep learned to skip memory changed by other writers, `tech/jit.md`) |

## AArch64 encoder of the JIT
- `cargo xtask encoder-check` (`xtask/src/encoder_check.rs`): the samples of `armor_jit::check` (instruction emitted by the encoder, plus the same operand as assembly text) are assembled with the host's `as` and `objcopy`. The words must match.
- If the assembler emits a different number of words (an expanded alias), the command assembles the samples one by one to find the one responsible.
- It is in the verification chain (above).

## OpenGL through the host (ADR-0041, since 2026-10-15)
- `cargo xtask gl-gen --check` (`xtask/src/glgen.rs`, in the verification chain): the generated OpenGL table (`crates/armor-linux/src/host/gl_table.rs`) and stubs (`guest/libgl/gl_stubs.c`) must equal what Khronos' `gl.xml` (`third_party/khronos/`) gives; it also lists the functions left out and why.
- `cargo xtask gl-check` (`xtask/src/gl.rs`): builds armor's x86-64 `libGL.so.1` and the programs of `tests/guest/gl/`, and runs each one three times in the games root (`cargo xtask fetch-rootfs --games`): under `qemu-x86_64` and under armor with the guest's software Mesa (llvmpipe), and under `armor --host-gl` with the host's OpenGL. The programs draw only what every conforming renderer draws the same way (pixel-aligned rectangles and quads, flat colours, colour factors 0 and 1, `GL_NEAREST` textures with no pixel centre on a texel edge) and print a checksum of the pixels read back with `glReadPixels`: the three outputs must be equal. It needs an X display (`DISPLAY`), so it is not in `difftest` nor in the chain; it runs after every change to the OpenGL path. ✅ 2026-10-15, Pi 5 (renderer of `--host-gl`: "V3D 7.1.7.0"): `gl_rects` and `gl_shader` equal.

| Program | What it covers |
|---|---|
| `gl_rects` | GLX visual, context and window from the program's own `libX11`; `glClear` with scissors; `glReadPixels` into the program's memory |
| `gl_shader` | OpenGL 2 the way games use it: functions from `glXGetProcAddressARB`, GLSL compiled and linked, vertex attributes read from the program's memory, uniforms, a texture uploaded with `glTexImage2D` |

## Fuzzing (M3)
In-tree fuzzing tests, without dependencies (since 2026-10-13), on the components that read guest input. They are normal tests: `cargo test` runs them briefly with a fixed seed; `cargo xtask fuzz` (`xtask/src/fuzz.rs`) runs them in release with 1 million iterations each (`--iterations`) and a new seed, which it prints so that the run can be repeated (`--seed`). Generator: xorshift64, like the JIT's random tests.

| Test | Crate | Input | Properties checked |
|---|---|---|---|
| `fuzz_decoder` | `armor-x86` | random bytes and "instruction-shaped" bytes (prefixes, `0F`/`0F38`/`0F3A`/VEX/EVEX/x87 escapes, opcodes, ModRM, immediates) | no panic; length from 1 to 15 and not beyond the given bytes; the same instruction from its own bytes alone; "truncated" for every shorter prefix; never "truncated" with 15 bytes or more |
| `fuzz_parser` | `armor-elf` | a valid dynamic executable (`PT_LOAD`, `PT_INTERP`, `PT_DYNAMIC`) with changed bytes, fields at extreme values, truncated or lengthened file | no panic in `ident`, `parse_any`, `parse` and in every accessor (`file_bytes`, `interpreter_path`, `needed_libraries`, `phdr_address`) |
| `fuzz_syscall_arguments` | `armor-linux` | 36 syscalls that touch only the guest address space or read host state, with pointers at the edges of writable, read-only and inaccessible mappings, unmapped or non-canonical addresses, extreme sizes and integers | no panic and no hang; in `rax` a result or a `-errno` |

Excluded from the syscall fuzz, because with random arguments they would act outside the test: writes to files and descriptors, signals, processes, blocking calls, process state used by the test (umask, fcntl, prctl, limits).

**Verification of the tests** (mutations): caught a read without bounds check and the 15-byte limit removed in the decoder, the check of the `PT_LOAD` data removed in the parser.

**What it found** (2026-10-13), on the first run of the syscall fuzz:
- **`brk` with a huge address hung armor.** `map_fixed` looked for the missing host pages one by one over the whole range. `unmap`, `protect` and `discard` did the same, so a `munmap` of 2^46 bytes was 2^34 steps.
- **Fix** (`tech/memory.md`): now they visit only the host pages that exist, and `map_fixed` clears only the host pages already in use.
- **Second finding, rereading the code:** `brk` could grow over another mapping and overwrite it. Now it refuses, like Linux.
- New tests: `huge_ranges_cost_only_the_mapped_pages` and `brk_does_not_grow_over_a_mapping`.

## Memory model litmus tests (ADR-0023)
`c/litmus`: two threads walk together through arrays of independent locations, one per cache line, so that their accesses to location i cross. The program counts the outcomes that x86-TSO forbids:
- **MP** (message passing): `x=1; y=1` against `r1=y; r2=x`; forbidden `r1=1, r2=0`;
- **SB+mfence** (store buffering with a barrier): `x=1; mfence; r1=y` against `y=1; mfence; r2=x`; forbidden `r1=r2=0`;
- **SB+lock** (since 2026-10-13, step 4 of ADR-0023): the same with `lock addl $0, (%rsp)` instead of `mfence`, the cheapest full barrier on x86, which the JIT translates with an LSE atomic.

Expected: 0 for all three, as under qemu-x86_64.

**Sensitivity verified** (2026-10-13). The same source compiled for AArch64 and run natively gives:
- MP: from 4 to 40 forbidden outcomes per execution;
- SB with `-DNO_FENCE`: from 88 to 256;
- SB with `dmb ish`: 0;
- SB+lock (natively a `SEQ_CST` `__atomic_fetch_add`): 0; with `-DNO_FENCE` from 6 to 55.

A first version, which synchronised the threads at every round, gave 0 natively too, so it saw nothing: it was rewritten this way.

Since step 3 of ADR-0023 (the GIL removed) it is the TSO regression test:
- ✅ 0 forbidden outcomes with both engines, in 50 executions each;
- ✅ the JIT with the ordering removed but the threads parallel gives forbidden MP outcomes in 5 executions out of 5: the test sees the error in armor itself;
- ✅ the JIT with LSE atomics without acquire/release (`LDADD` instead of `LDADDAL`) gives forbidden SB+lock outcomes in 5 executions out of 5 (from 20 to 305).

In lockstep the threads run one at a time (GIL), so there the test does not see the ordering.

## Benchmarks
`cargo xtask bench` (`xtask/src/bench.rs`) is not a test, but it also checks the outputs: armor, qemu and native must print the same thing, otherwise it stops. Programs in `tests/bench/`, description and results in `tech/performance.md`.

## Differential tests of the JIT
`crates/armor-jit/src/tests.rs`, inside `cargo test`. The oracle is the interpreter, which in turn is compared with qemu by `difftest`.

**The comparison (`Guest::compare`).** The same code runs from the same initial state:
- under the interpreter;
- under the JIT;
- in lockstep.

Registers, xmm, `rip`, `rflags`, memory and the error (fault, #DE…) must match.

**The test memory:**
- two data pages;
- a `PROT_NONE` page;
- a read-only page;
- the stack;
- the code.

**The tests:**
- `random_code_matches_the_interpreter`: 3,000 random programs of 1 to 24 instructions; `random_code_matches_the_interpreter_in_concurrent_mode` another 1,500 in concurrent mode (ADR-0023), where the 8–64-bit `LOCK` instructions, often on aligned operands, go through the LSE atomics; the generator covers all the translated forms and some left to the interpreter; it uses limit values for carry, overflow and sign; it makes accesses across a page boundary, on `PROT_NONE`, on read-only memory and beyond the mapping;
- targeted tests:
  - all conditions in all flag states;
  - every operation with lazy flags × size × condition;
  - chained loops and budget;
  - indirect jumps with flags that cross a `ret`;
  - code rewritten after translation;
  - faults in the middle of a block;
  - null pointers;
  - edge cases of division;
  - checks that follow the changes of the base and of `rsp`, at the edges of the stack and of the mappings;
  - floating point on all pairs of special values (signed zeros, infinities, quiet and signalling NaNs, denormals);
  - dead flags not computed (`dead_flags_are_not_computed`), LSE atomics in concurrent mode (`concurrent_mode_emits_lse_atomics`).

Every JIT increment has its own mutation tests (`tech/jit.md`). When a mutation is not caught, the missing test is added before going on.

## Undefined flags
Many instructions leave some flags **architecturally undefined**: qemu, the hardware and armor may legitimately produce different values.
Every test compares RFLAGS through a **mask** that contains only the flags defined for that instruction (`FLAGS_STATUS`, `FLAGS_NO_AF`, …). Examples and sources in `tech/x86-64-flags.md`.

## Native oracle (ADR-0044, since 2026-10-15)
For syscalls that armor hands to the host kernel (keys, NUMA policies, file handles, the administrators' calls), qemu often has no implementation and the Xeon's container filters some of them. **`difftest --native`** runs the C tests listed in `NATIVE_ORACLE` (`difftest.rs`; today `system_calls`) built for AArch64 with the host's compiler (`target/guest/native/`), on this machine, as the reference: the same kernel answers the native program and armor's guest. Those cases run only with `--native` (the other modes skip them), and the chain runs it. Also new on 2026-10-15, compared with the Xeon only (`HARDWARE_ONLY`, qemu lacks the calls): `identity_calls`, `signalfd`, `process_calls`, `message_queues`, `linux_aio`, `seccomp`; `xattr_splice` with qemu as well.

## Other page sizes (roadmap M9, since 2026-10-15)
armor runs on hosts with 4K, 16K and 64K pages. The Pi 5 boots one kernel at a time, so the other sizes are measured in KVM virtual machines on it (QEMU 10.0.13, `-cpu host`, 4 CPUs, 3 GiB):
- **16K:** Debian 13's cloud image (`debian-13-genericcloud-arm64.qcow2`, SHA-512 checked against Debian's list) with Debian's own 16K kernel, `linux-image-arm64-16k` (`6.12.111+deb13-arm64-16k`, `getconf PAGESIZE` = 16384);
- **64K:** Ubuntu 24.04's cloud image (`noble-server-cloudimg-arm64.img`, SHA-256 checked) with `linux-generic-64k` (`6.8.0-147-generic-64k`, 65536, 48-bit addresses).

cloud-init installs the kernel and the build tools; the repository is shared read-only with 9p and copied in, the guest programs and roots of this machine with it.

On those hosts qemu-user itself is not a reliable reference: on 16K `qemu-i386` crashes or fails `mmap` in 6 cases; on 64K `qemu-x86_64` 8.2 crashes in dozens (grep, gawk, xz). So **`difftest --save DIR`** keeps qemu's result of every case (stdout and how the process ended) on the 4K host, where qemu matches the Xeon, and **`difftest --expected DIR`** compares armor with those saved results instead of running qemu; the cases whose output depends on the machine (user, directory, files of `/etc`, time zone, GPU: `OTHER_MACHINE` in `difftest.rs`) are skipped. The results, 2026-10-15:

| Host | Rust tests | `difftest` (interpreter and JIT) | `--lockstep` | ARMv8.0 paths (`ARMOR_HOST_FEATURES=baseline`) | `sandbox-check` |
|---|---|---|---|---|---|
| Pi 5, 4K (the reference) | 267 | 467/467 against qemu (results saved with `--save`) | 467/467 | 467/467 | ✅ |
| Debian 13, 16K | 267 | 454/454 against the saved results (13 machine-dependent cases skipped) | 454/454 | 454/454 | ✅ |
| Ubuntu 24.04, 64K | 267 | 454/454 against the saved results | 454/454 | 454/454 | ✅ after lifting Ubuntu's AppArmor restriction on user namespaces (`tech/security.md`) |

Since ADR-0045 each virtual machine records its runs: inside it, `cargo xtask difftest --expected DIR --report`, the same with `--lockstep` and with `ARMOR_HOST_FEATURES=baseline`, on a copy of the repository with its `.git` (the commit goes into every line); its results file is then copied into `docs/results/` and rendered in the compatibility matrix (✅ 2026-10-15, commit 97f001d: 457/457 in the three modes on 16K and on 64K, with 470 cases of which 13 depend on the machine).

Against qemu run on the same host, before the saved results existed: on 16K 461/467, the other 6 being qemu-i386's own failures (armor's output there equals the 4K run's in all three engines); on 64K 400/467, almost all qemu-x86_64 8.2 crashing.

What these hosts found in armor, fixed on 2026-10-15: shared mappings smaller than a host page could not grow (`mremap`, `tech/memory.md`); on the 48-bit host armor placed memory above 2^47 and then the guest stack in the last page below it (`tech/memory.md`, "Address space"). Three tests laid shared and private pages out in 4K or 16K steps, which on bigger host pages share a host page, the case ADR-0022 leaves unsupported; `mremap_shared`, `shared_memory` and `sysv_shm` now use 64 KiB blocks for those cases.

## Other CPUs (roadmap M9, since 2026-10-15)
**`.github/workflows/arm64.yml`** runs the chain at every push to `main` on GitHub's ARM64 Linux runner (free for public repositories): fmt, clippy, the Rust tests, `fetch-busybox`, `fetch-rootfs` (x86-64 and i386), `build-guest`, `difftest` with the interpreter and the JIT, in lockstep and on the ARMv8.0 paths, `encoder-check`, `sandbox-check`, and the build of `armor-wow64.dll` (`cargo xtask wow64`, with the `aarch64-pc-windows-msvc` target and Debian's `lld` and `llvm`). They run in a privileged Debian 13 container (`.github/ci-chain.sh`), as a normal user, with the development machine's tools (gcc, glibc, qemu 10.0.13 with qemu's `binfmt.d` entries registered), so that the CPU is what differs: Ubuntu's qemu 8.2 gave another result for `alu/double_shifts` than qemu 10 and the Xeon, which armor matches on both CPUs. The first runs found two things, fixed: armor's core dumps (its whole address space) streamed for minutes to the runner's `systemd-coredump`, so `faults/*` and `writev` outlasted the time limit (`tech/signals.md`); without binfmt in the container qemu could not run the x86 programs `processes` and `threads32` start.

✅ 2026-10-15, run 38756588654: Neoverse-N2 (ARMv9.0 with SVE2, LSE2, BF16; 4 cores), 4K pages, kernel `6.17.0-1022-azure`: 268 Rust tests, `difftest` 467/467 with every engine and on the ARMv8.0 paths, `encoder-check` 0 mismatches, `sandbox-check` 0 failed, in 12 minutes.

Since ADR-0045 the runner's chain records its `difftest` results with `--report` and the workflow publishes the results file as the `results` artifact, downloaded with `gh run download <run> -n results` and committed by hand (CI never pushes). ✅ 2026-10-15, run 38798303497 (commit 49c201c): 470/470 with the interpreter and the JIT, in lockstep and on the ARMv8.0 paths, 3/3 against the native builds; the file is `docs/results/neoverse-n2-4k-debian-gnu-linux-13-container.tsv`.

## Other distributions (ADR-0043, since 2026-10-15)
**`cargo xtask distro-check`** builds the package and its archive (`cargo xtask package`) and, in podman containers on this machine, installs the archive with its `install.sh` and uses armor as a user would: Debian's Python (x86-64) and coreutils' `sort` (i386) must print what they print here, `armor install` of Debian's `hello` `.deb` (pinned SHA-256) takes its libraries from Debian's archive with the keyring armor carries, and `armor engine fetch` downloads the Windows engine, whose Wine runs `cmd /c ver`. Fedora 42 comes from Fedora's registry; Arch Linux ARM from its official root archive, checked against its build key's fingerprint before `podman import`. 2026-10-15: every check passes on both.

## Real applications (ADR-0045, since 2026-10-15)
**`cargo xtask app-check [--report] [--x86-engine armor|fex] [id…]`** (`xtask/src/app_check.rs`) installs real applications as a user would and checks them without anyone at the screen. Each installer is pinned by SHA-256 (downloaded into `target/apps/downloads/` when missing; Friday Night Funkin' has no public URL and is skipped when its zip is absent). Each application gets a throwaway data directory (`target/app-check/<id>/`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `TMPDIR`, with the user's Windows engine linked in, never modified), `armor install`, then `armor run` on its own Xvfb display (`:91`, 1280×800):
- **level 1** when a window whose title names the application appears (`xwininfo -root -tree`): Notepad++, KeePass 2 (.NET), Friday Night Funkin', XnView MP (`.deb`), KeePassXC (AppImage), Spotify (`.deb`, Chromium);
- **level 3** for 7-Zip: the `.7z` archive it makes of `tests/guest/` with fixed options is byte-for-byte the one the native ARM64 build makes of the same files (the Linux `7zz` under armor; the Windows x64 and x86 builds under Hangover against the ARM64 Windows build).

The JIT's cache of translations (ADR-0048) is never the user's in the checks: `difftest` gives armor `target/difftest/jit-cache` (kept across runs; the chain runs `difftest --jit` a second time right after the full `difftest`, so that every JIT case runs once from a cold cache and once from a warm one), `app-check` a throwaway `XDG_CACHE_HOME`, `bench` its own (`target/bench/jit-cache`, emptied at the start; its "first run" column empties `target/bench/jit-cache-cold` before every run).

Windows applications run their 32-bit x86 code (32-bit programs, and the 32-bit installers of most 64-bit ones) under FEX's WoW64 backend, as `armor run` does by default; with **`--x86-engine armor`** under armor's own (`armor-wow64.dll`, ADR-0047, built first with `cargo xtask wow64`), recorded as an engine of its own. As armor falls back to FEX when its backend is missing, the check then fails unless the backend's line at start (`armor-wow64 <version>: started`) is in the log.

Every process left in the data directory is stopped afterwards (found by its environment, `/proc/<pid>/environ`), and the directory removed. What needs an account or a desktop session (Steam, Proton games, sign-ins, Sublime Text, whose window appears on the desktop but not under Xvfb) is checked by hand and recorded in `docs/results/by-hand.tsv`.

## Recorded results and the matrix (ADR-0045, since 2026-10-15)
`difftest`, `app-check`, `distro-check` and `bench` take **`--report`**: each result becomes one line of `docs/results/<machine>.tsv` (`xtask/src/results.rs`), the file of the machine the command ran on. The machine is named from what it reports: CPU (`lscpu`), page size, system (`/etc/os-release`) and place (native, KVM virtual machine or container: the GitHub runner's chain runs in one), and the file starts with those facts and the kernel. A line has eight tab-separated fields: kind (`suite`, `application`, `performance`), name, engine, reference (qemu, saved results, the Xeon, the native build), result, date, commit (with `+` when the tree had changes) and the command that reproduces it. A new run of the same kind, name, engine and reference replaces its line. The GitHub runner's chain records its own results and publishes the file as the `results` artifact; it is downloaded and committed by hand (CI never pushes).

**`cargo xtask matrix`** writes the generated tables of `COMPATIBILITY_MATRIX.md` (between `<!-- matrix:start -->` and `<!-- matrix:end -->`: machines, suites per machine and engine, applications per engine and machine, performance, results by hand) and, in `README.md`, the summary per machine (between `matrix` markers) and the speed table of each machine with `bench` results, as multiples of the native time with the fastest translator in bold (between `bench` markers); `--check` fails when they differ from the results files, and the chain runs it.

## Golden files
By default `difftest` **runs qemu at every test**: it is the simplest solution. Since 2026-10-15 `--save` and `--expected` keep and use qemu's results in a directory (above, "Other page sizes"); they are not versioned in the repository.

## Commands
```bash
cargo test --workspace                     # Rust unit tests (they do not need qemu)
cargo xtask build-guest                    # compiles all the guest programs
cargo xtask run basic/hello [args…]        # runs a program under qemu-x86_64
cargo xtask run i386/basic/hello           # … an i386 program under qemu-i386
cargo xtask run --armor basic/hello        # … or under armor (compiled on the fly)
cargo xtask run --jit basic/hello          # … or under armor with the JIT
cargo xtask difftest                       # compares armor (interpreter and JIT) and qemu on all programs
cargo xtask difftest alu/logic mov/sizes   # … or only on the ones given
cargo xtask difftest --jit c/malloc        # … with a single engine (--interpreter, --jit or --lockstep)
ARMOR_ORACLE=root@host ARMOR_ORACLE_KEY=~/.ssh/key cargo xtask difftest --oracle   # against the x86-64 hardware (ADR-0025)
cargo xtask run --lockstep c/hello         # the JIT checked instruction by instruction by the interpreter
cargo xtask decoder-check target/guest/c/hello   # decoder against objdump on a real binary
cargo xtask encoder-check                  # AArch64 encoder of the JIT against the host's GNU assembler
cargo xtask gl-gen [--check]               # OpenGL table and stubs from Khronos' gl.xml (ADR-0041)
cargo xtask gl-check                       # OpenGL under qemu, armor and armor --host-gl: same pixels (needs DISPLAY)
cargo xtask fetch-busybox                  # once: Debian's busybox for difftest
cargo xtask fetch-rootfs                   # once: x86-64 root with coreutils and the other Debian programs
cargo xtask fetch-rootfs --i386            # once: i386 root with coreutils (ADR-0029)
cargo xtask fetch-rootfs --games           # once: x86-64 root with Mesa, X11, SDL2, sound, libstdc++ (games, gl-check)
cargo xtask progress                       # roadmap percentages in the README
cargo xtask package                        # target/package/armor_<version>_arm64.deb (M7)
cargo xtask coverage                       # coverage badges in the README (conventions.md)
cargo xtask fuzz                           # long fuzzing of the decoder, the ELF parser and the syscall arguments
cargo xtask bench                          # benchmarks against qemu and native (tech/performance.md)
cargo xtask app-check [id…]                # real applications installed and checked under Xvfb (ADR-0045)
cargo xtask matrix [--check]               # compatibility matrix from docs/results/ (ADR-0045)
cargo xtask difftest --report              # … any of difftest, app-check, distro-check, bench: record the result
# x87 transcendentals against an independent reference (ADR-0030):
ARMOR_X87_CASES=/tmp/x87-cases.txt cargo test --release -p armor-cpu write_cases -- --ignored
python3 -I tests/x87/reference.py /tmp/x87-cases.txt   # "N cases, 0 mismatches"
# … and against the hardware: tests/x87/probe.c compiled for x86-64, run on the machine and under armor
```
`decoder-check` decodes every instruction that objdump finds in the executable sections and compares the length. It fails if a length differs or if armor declares invalid an instruction that objdump decodes (except `ud0`/`ud1`/`ud2`, which exist to raise #UD). Two ways in which objdump splits or joins instructions are not differences (since 2026-10-13, `same_split`, test `objdump_splits_are_not_mismatches`): a REX prefix followed by another prefix, which objdump shows on its own (`rex.RXB`) and the CPU joins to the next instruction (20 cases in `libcrypto.so.3`), and `fwait` joined to the next x87 instruction (`fstcw` = `fwait` + `fnstcw`, 13 cases in `libm.so.6`). With these rules `libm.so.6` and `libcrypto.so.3` are in the verification chain: 0 differences. It prints the coverage and the most frequent instructions not yet supported (how many: `DECODER_CHECK_LIMIT`, default 25). Since 2026-10-13 it checks the **length-only decoder** (`armor_x86::length`) in the same way on every instruction, verifies that it matches `decode` where the latter succeeds, and counts the instructions per extension (`tech/x86-64-encoding.md`).
Result as of 2026-10-12 on the 5 C programs (~114,000 instructions each, almost all from glibc): **95.4% decoded, 0 different lengths**. The rest are almost only AVX, AVX-512 and SSE4, which glibc does not use with our CPUID.
The code is in `xtask/src/main.rs`, `xtask/src/difftest.rs` and `xtask/src/decoder_check.rs`. The armor binary used is `target/difftest/armor`, rebuilt by `difftest` and by `run --armor` with the `difftest` cargo profile: optimised like release, but with debug assertions and overflow checks, so a guest input that overflows an armor arithmetic operation stays a panic. With the debug build python took more than 120 s for a traceback; with this profile the whole `difftest` took about 1 min 30 s, and 43 s after the instruction cache (2026-10-12). With the two engines (interpreter and JIT) and 217 cases it took 51 s, 49 s after increments 9–15 of the JIT (2026-10-13); with more cases it takes longer (the whole chain takes about 30 minutes in October 2026).

## Layout
```
tests/guest/include/dump.h          # shared macros (DUMP, DUMPX, DUMP_FLAGS, SETX, EXIT)
tests/guest/asm/<group>/<name>.S    # assembly programs; the name is "<group>/<name>"
tests/guest/c/<name>.c              # C programs; the names are "c/<name>", "c-pie/<name>", "c-dyn/<name>"
tests/guest/include/dump32.h        # macros for the i386 tests (DUMP, DUMP_FLAGS, DUMP_WRITE, EXIT with int 0x80)
tests/guest/asm32/<group>/<name>.S  # i386 assembly programs (ADR-0029); the name is "i386/<group>/<name>"
tests/guest/c32/<name>.c            # i386-only C programs; the names are "c32/<name>", "c32-pie/<name>", "c32-dyn/<name>"
target/guest/asm/<group>/<name>     # compiled executables (ignored by git)
target/guest/c/<name>, target/guest/c-pie/<name>, target/guest/c-dyn/<name>
target/guest/i386/…, target/guest/c32/<name>, target/guest/c32-pie/<name>, target/guest/c32-dyn/<name>
tests/guest/gl/<name>.c             # OpenGL programs for gl-check (dynamic, run in the games root)
guest/libgl/                        # armor's x86-64 libGL.so.1 (gl_stubs.c generated, glx.c), built into target/guest/libgl/
```

| Group | Programs | What they cover |
|---|---|---|
| `basic` | `exit42`, `hello`, `initial_state`, `auxv`, `syscall_regs`, `write_errors`, `cpuid`, `prefixes` | process start, auxv, syscalls, CPUID, prefixes and NOP |
| `mov` | `sizes`, `memory`, `lea`, `extend` | operand sizes, addressing, `lea`, movzx/movsx/cbw… |
| `alu` | `add_sub_cmp`, `adc_sbb`, `logic`, `unary`, `mul_div`, `shifts`, `double_shifts`, `exchange`, `bits`, `cmpxchg_wide`, `legacy` | arithmetic, logic, mul/div, shifts/rotations, `shld`/`shrd`, xchg/cmpxchg/xadd with lock, bit test/scan, bswap, `cmpxchg8b`/`16b`, `lahf`/`sahf`, `xlat`, `mov` with an absolute address |
| `control` | `jcc`, `call_ret`, `cmov_setcc`, `indirect`, `flags`, `self_modifying` | jumps, direct and indirect calls, jump tables, `ret imm`, `leave`, `loop`, cmov/setcc, CF/DF flags |
| `stack` | `push_pop`, `enter_popf`, `push_pop16` (since 2026-10-14) | `push`/`pop`, immediates, `push rsp`, `pushfq`; `enter` at every level, `popf` (modifiable bits); 16-bit `push`/`pop` with the `66` prefix (registers, memory, immediates; `pop` of a 16-bit register keeps the upper bits) |
| `string` | `ops` | movs/stos/lods/cmps/scas, rep/repe/repne, DF=1, overlapping copies |
| `mmx` (since 2026-10-14) | `ops`, `x87_state` | MMX: the SSE2 and SSSE3 integer operations on `mm`, `pshufw`, `pinsrw`/`pextrw`, `pmovmskb`, `psadbw`, `maskmovq`, `movntq`, conversions with xmm; the x87 state after MMX and `emms` |
| `x87` | `stack` | stack registers, integers in memory, comparisons, `fcmov`, control word, `fxsave` of a live x87 state |
| `sse` | `moves`, `integer`, `integer_more`, `state`, `scalar_float`, `packed_float`, `reciprocal` | packed floating point (arithmetic, `cmpps/pd`, conversions) and `rcp`/`rsqrt` (separately: approximations differ between processors); saturating sums, `pavg`, 16-bit multiplications, `pmaddwd`, `psadbw`, shifts with the count in xmm, `maskmovdqu`, `clflush`; xmm moves, SSE2 per-lane arithmetic and logic, shifts, unpack, pack with saturation; `fxsave`/`fxrstor`/`ldmxcsr`/`stmxcsr`; scalar floating point (`tech/floating-point.md`) |
| `faults` | `write_rodata`, `read_null`, `exec_data`, `exec_unmapped`, `exec_after_mprotect`, `invalid_opcode`, `stack_after_write`, `divide_error`, `divide_overflow`, `misaligned_sse`, `hlt`, `ud2`, `int3`, `lock_register`, `cmpxchg16b_misaligned`, `cmpxchg8b_rodata` | SIGSEGV, SIGILL, SIGFPE, SIGTRAP |
| `c`, `c-pie`, `c-dyn` | `hello`, `printf_int`, `malloc`, `strings`, `compute`, `x87`, `signals`, `signals_async`, `threads`, `threads_signals`, `processes`, `mremap`, `privileged`, `legacy_syscalls`, `shared_memory`, `shared_atomics`, `litmus`, `writev`, `x87_more`, then `sse_exceptions`, `undeclared_extensions`, `x87_transcendental`, `sockets`, `events`, `scm_rights_truncated`, `sysv_semaphores`, `file_locks` (with record locks since 2026-10-14), and since 2026-10-14 for Doki Doki Literature Club `fork_threads`, `memfd`, `pi_mutex`, `sysv_shm`, `drm_version`, `bad_ioctl`, `sched`, and for Spotify `basic_calls`, `capabilities`, `data_selectors`, `mremap_shared`, `pkeys`, `ppoll_mask`, `sigtimedwait`, `sync_misc`, `timerfd`, and since 2026-10-15 `reserve_large` | static, static-pie and dynamic glibc: stdio, malloc/brk/mmap, SSE2 string functions, arithmetic, qsort, printf; for `c-dyn` also `ld.so` (relocations, lazy symbol resolution with `fxsave`); signals: handlers, mask, faults as signals, timers, interrupted syscalls, alternate stack (`tech/signals.md`); threads: mutexes, atomics, spinlocks, condition variables, TLS, per-thread signals; processes: `fork`, `vfork`, `execv`, `posix_spawn`, `waitpid`, pipes, `system()`; `mremap`; privileged instructions and `int n` as signals, `cs`/`ss` selectors; the syscalls absent on AArch64 called by number (files, `stat`, `getdents`, `select`, `epoll`, `eventfd`, `inotify`, `alarm`, obsolete ones); `MAP_SHARED` and `LOCK` instructions across processes in parallel; `writev` and glibc's fatal message; x87 instructions added in M4 (constants, `fxam`, `frndint`, `fscale`, `fxtract`, `fprem`/`fprem1`, environment, `fnsave`/`frstor`, `fisttp`, `ffreep`); `fork` from a process with several threads; `memfd_create` and seals; priority-inheritance mutexes (PI futexes); System V shared memory (`shmget`/`shmat`, as in MIT-SHM); `DRM_IOCTL_VERSION` on the host GPU; scheduling policy and `clock_getres`; vectored I/O, `waitid`, `getitimer`, `nanosleep`, rlimits; capabilities; `timerfd`; `rt_sigtimedwait` and queued signals; `ppoll` with a signal mask; `fsync` and the other sync calls, `mincore`, `mlock`, `membarrier`, `ioprio`; growing a shared mapping (file and memfd) with `mremap`; memory protection keys and the data segment selectors (only with `--oracle`); 32 GiB `PROT_NONE` reservations like V8's |
| `i386` (since 2026-10-14) | `basic/hello`, `basic/tls`, `stack/push_pop`, `control/branches`, `string/ops`, `mmx/bignum` | 32-bit code (ADR-0029): `int 0x80`, 4-byte stack, `enter`/`leave`, jumps and tables, `loop`/`jecxz` with `ecx`, one-byte `inc`/`dec`, string instructions with `esi`/`edi`/`ecx`, `xlat`, `set_thread_area`/`get_thread_area` and `gs` |
| `c32`, `c32-pie`, `c32-dyn` (since 2026-10-14) | `i386_abi`, `signals32`, `signal_context`, `threads32` (in `tests/guest/c32/`) and, from `tests/guest/c/`, `hello`, `malloc`, `mremap`, `printf_int`, `processes`, `shared_memory`, `signals_async`, `strings`, `threads`, `threads_signals`, `writev`, `x87_more`, `x87_transcendental`, `sockets`, `events`, `scm_rights_truncated`, `sysv_semaphores`, `file_locks`, and since 2026-10-14 `fork_threads`, `memfd`, `pi_mutex`, `sched`, then `mremap_shared` and `ppoll_mask` (list `I386_SHARED_C_TESTS` in `xtask/src/main.rs`; also `files32`, `legacy32`, `uname_machine` in `tests/guest/c32/`) | i386 threads (`clone` with `user_desc`, per-thread TLS), `fork`, `vfork`, `execve`, `wait4`, shared memory; i386 signals (rt and old frame, faults, alternate stack, FPU state, `sigcontext` and FSAVE header against the Xeon); static, static-pie and dynamic i386 glibc (`c32-dyn`: `ld-linux.so.2` and the `libc6-i386-cross` libraries from the root `/usr/i686-linux-gnu`, constant `I386_GUEST_ROOT`): start with TLS, stdio, malloc, strings, x87; 64-bit integers, `double` on the x87, syscalls with 32-bit structures. The other sources of `c/` do not compile for i386 (`__int128`, 64-bit assembly, `REG_RIP`) |
| `busybox` | 50 cases in `tests/guest/busybox.txt` | a real Debian binary: files, directories, stdin, hashes, sorting, text, the shell (pipelines, command substitution, loops); also a nonexistent applet |
| `interpreters` | 19 cases in `tests/guest/interpreters.txt` | perl 5.40 (`-e`, `-n`, regexes, hashes, `sprintf`, `system`) and python 3.13 minimal (maths, big integers, f-strings, exception with traceback, `os.system`) |
| `tools` | 33 cases in `tests/guest/tools.txt` | grep (also `-P` with libpcre2), sed, gawk (also floating point and `-M` with libmpfr/libgmp), find, xargs; gzip, xz, bzip2, tar, diff, cmp, make and dash in pipelines with `sh` |
| `coreutils` | 51 cases in `tests/guest/coreutils.txt` | real dynamic Debian binaries with their libraries: files, hashes (also SHA-512 and BLAKE2), `sort` and `tail` (SSE floating point), `seq` and `numfmt` (x87), `factor` with libgmp, `ls -l` (users, xattr), `date`, `id`; programs that launch others (`env`, `timeout`, `nice`, `nohup`); a nonexistent file; closed stdout |

## busybox
- **Binary:** Debian package `busybox-static` 1:1.37.0-6+b9 amd64 (SHA-256 pinned in `xtask/src/busybox.rs`), downloaded by `cargo xtask fetch-busybox` into `target/guest/busybox/busybox`. It is not in the repository. `difftest` fails with a hint if it is missing.
- **Cases:** `tests/guest/busybox.txt`, one line `name: arguments` per case, arguments separated by spaces; an argument with spaces goes in double or single quotes (above, "What `difftest` compares"). The name in `difftest` is `busybox/<name>`.
- **Environment:** every case runs in `tests/guest/data/` with `fruits.txt` as stdin. Same environment and same directory for qemu and armor, so the output that depends on the system (e.g. `ls /etc/apt`, `id -u`) is comparable too.
- **Floating point:** `seq` (2), `awk` (arithmetic without maths functions, absent from this build) and `sleep` use SSE and x87.
- **Decoder:** `decoder-check` on busybox: 97.9% of 414,343 instructions decoded, 0 lengths different from objdump (2026-10-12).

## coreutils (dynamic)
- **Root:** `cargo xtask fetch-rootfs` builds `target/guest/rootfs` from 39 pinned Debian trixie amd64 packages (`coreutils` 9.7-3, `grep` 3.11-4, `sed` 4.9-2+deb13u1, `gawk` 1:5.2.1-2+b1, `findutils` 4.10.0-3, `perl-base` 5.40.1-6+deb13u1, `python3.13-minimal` 3.13.5-2+deb13u5, and since 2026-10-13 `gzip`, `xz-utils`, `bzip2`, `tar`, `diffutils`, `make`, `dash`, with their dependencies, including `libc6` 2.41-12+deb13u4; ADR-0014). It is not in the repository. `difftest` fails with a hint if it is missing.
- **Cases:** `tests/guest/coreutils.txt`, `tests/guest/tools.txt` (grep, sed, gawk, findutils, compression, archives, diff, make, dash) and `tests/guest/interpreters.txt` (perl, python), same format as `busybox.txt`; the first argument is the program in the root's `/usr/bin`. The names in `difftest` are `coreutils/<name>`, `tools/<name>` and `interpreters/<name>`.
- **i386** (since 2026-10-14): `cargo xtask fetch-rootfs --i386` builds `target/guest/rootfs-i386` from 15 Debian trixie i386 packages (coreutils 9.7-3 and its closure, the same packages and versions as amd64, `PACKAGES_I386` in `rootfs.rs`, hashes from the `Packages` index of 2026-10-14 verified against `Release`). The same cases of `coreutils.txt` run there as `coreutils32/…`, with `qemu-i386` as the oracle: 50 out of 50 identical; `coreutils32/uname` only with `--oracle`, which however does not run it (root), because qemu-i386 answers `i686` and a 64-bit kernel `x86_64` (`c32/uname_machine` verifies it on the Xeon).
- **Games root** (since 2026-10-15): `cargo xtask fetch-rootfs --games` builds `target/guest/rootfs-games` from the packages of `target/guest/rootfs` plus 84 more (`PACKAGES_GAMES` in `rootfs.rs`: the closure of Mesa's OpenGL, EGL and Vulkan libraries, X11, SDL2, ALSA, PulseAudio and `libstdc++6`, Debian trixie amd64, hashes from the `Packages` index of 2026-10-14 verified against `Release`). It is the root of `gl-check` and of the games tried by hand (`tech/steam.md`); no `difftest` case uses it.
- **Environment:** as for busybox (`tests/guest/data/`, `fruits.txt` as stdin), but with the `fetch-rootfs` root for qemu (`-L`) and for armor (`ARMOR_ROOT`). Locale, `/etc/passwd` and time zones come from the host, the same for both.
- **Decoder:** `decoder-check` on Debian's `ls`, `sort`, `sha512sum` and `libc.so.6`: 0 lengths different from objdump (2026-10-12).

## How to write an assembly test
- **Assembler: gas**, invoked through `x86_64-linux-gnu-gcc` with the options `-nostdlib -static -no-pie -I tests/guest/include`. The result is an `ET_EXEC` executable at `0x400000`.
  Why gas and not nasm: the `.S` files go through the C preprocessor, so the syscall numbers come directly from `<asm/unistd_64.h>`, without magic numbers.
- `#include "dump.h"` at the start: it enables `.intel_syntax noprefix` and defines the macros.
- C-style comments (`/* … */`): `#` at the start of a line is a preprocessor directive.
- Every file ends with `.section .note.GNU-stack, "", @progbits`, which avoids the linker warning about an executable stack.
- `{disp32} jcc label` forces the form with a 32-bit offset.
- After writing a test, check the encoding produced with `x86_64-linux-gnu-objdump -d -M intel target/guest/asm/<name>`.
- Do not print values that depend on the host (stack addresses, `AT_RANDOM`, environment variables).
- The core dumps of the fault tests are not written, with `ulimit -c 0` (the default on this host, `environment.md`).

## Rules
- A new instruction is "done" only with the differential test green and the flag mask documented.
- A bug found first becomes a failing test, then it is fixed.
- Unimplemented instructions produce an explicit error (bytes and RIP), never silently wrong behaviour.
