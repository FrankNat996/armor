# Performance

Measurements are **release builds** (`cargo build --release`) on the Raspberry Pi 5, median of 3 runs, except where stated. The profiler is `perf` 6.18 (Debian package `linux-perf`, installed on 2026-10-12).

## Reproducible benchmarks: `cargo xtask bench`
The command (`xtask/src/bench.rs`, since 2026-10-13, an M6 item) measures a fixed set of programs:
- the startup, static and dynamic, of a hello world;
- C programs in `tests/bench/`, one per category: integers, scalar floating point, integer SSE2, syscalls, memory, threads;
- python startup and two python loops, one on integers and one on floating point.

How it works:
- **Engines.** Each program runs under `armor --jit`, qemu-x86_64 (with the tests' CPU model) and natively. The native one is the same C source compiled by the host's gcc, or the host's python 3.13, the same version as the one in the guest root. `--interpreter` adds the interpreter's column (slow: up to 17 s per program).
- **Rounds.** In each round the engines alternate; the table gives the median and, in brackets, minimum and maximum.
- **Checked outputs.** All engines must print the same thing, otherwise the command stops. This is why the native build uses `-ffp-contract=off`: gcc on AArch64 fuses multiplications and additions (FMA) by default, and `float` gave a different result (Mandelbrot 25,425 interior points instead of 25,488).
- **Measured time.** It runs from the start of the process to its exit. No `timeout` around the measured process: a watchdog thread kills it after 300 s.
- **Translation cost.** It comes from armor itself: with `ARMOR_JIT_STATS` set, armor prints at exit the translated blocks, guest instructions, generated AArch64 instructions and time spent translating (`armor_jit::stats`, all threads together, only the measured process and not the children after `exec`). With `ARMOR_JIT_STATS=interpreted` (since 2026-10-14) it also counts the instructions that the engine leaves to the interpreter, by type (the name of the operation and, for SSE and x87, that of the inner operation), and prints the 25 most frequent types at exit and every 2^24 interpreted instructions: it helps choose what to translate next. It slows things down (every interpreted instruction is decoded twice), so `cargo xtask bench` does not use it.
- **Environment.** At the top the command prints the date, machine, kernel, page size, armor commit (with a warning if there are uncommitted changes) and the versions of qemu and of the compilers.
- **Binary.** It always measures the freshly built release binary. This makes the two measurement errors on this page impossible: a binary with leftover instrumentation, a wrong input file.

### First measurement (2026-10-13), 5 rounds
Commit `39ea8b8` plus the command itself and the JIT statistics; no change to the generated code.
```
date: 2026-10-13
host: Raspberry Pi 5 Model B Rev 1.0, Cortex-A76, 4 CPUs, page size 16384
kernel: 6.18.50+rpt-rpi-2712
armor: 39ea8b8 (uncommitted changes), release build
qemu: qemu-x86_64 version 10.0.13 (Debian 1:10.0.13+ds-0+deb13u1)
guest gcc: 14.2.0
host gcc: 14.2.0

Median of 5 runs, min–max in brackets. armor/qemu < 1: armor is faster.

| Benchmark | armor --jit | qemu-x86_64 | native | armor/qemu | translating (jit) |
|---|---|---|---|---|---|
| start-static | 6.7 ms (6.6–6.7) | 17.5 ms (17.4–17.7) | 0.3 ms (0.3–0.4) | 0.38 | 3.5 ms (52%) |
| start-dynamic | 13.3 ms (13.2–14.5) | 28.8 ms (28.6–29.8) | 0.7 ms (0.5–1.4) | 0.46 | 7.6 ms (57%) |
| integer | 400 ms (398–400) | 330 ms (328–333) | 116 ms (116–117) | 1.21 | 3.9 ms (1%) |
| float | 230 ms (225–230) | 718 ms (715–720) | 50.8 ms (50.8–52.7) | 0.32 | 4.4 ms (2%) |
| simd | 226 ms (225–233) | 348 ms (347–353) | 32.3 ms (32.2–32.4) | 0.65 | 4.0 ms (2%) |
| syscall | 480 ms (479–481) | 320 ms (318–321) | 116 ms (116–118) | 1.50 | 3.8 ms (1%) |
| memory | 254 ms (243–273) | 334 ms (332–342) | 104 ms (102–104) | 0.76 | 4.7 ms (2%) |
| threads | 933 ms (898–960) | 345 ms (322–408) | 112 ms (97–119) | 2.71 | 7.1 ms (1%) |
| python-start | 231 ms (226–236) | 466 ms (464–471) | 13.3 ms (13.1–14.5) | 0.50 | 96.6 ms (42%) |
| python-int | 1815 ms (1748–2033) | 4831 ms (4809–5057) | 225 ms (218–255) | 0.38 | 101.8 ms (6%) |
| python-float | 1493 ms (1471–1595) | 4339 ms (4252–4491) | 181 ms (172–189) | 0.34 | 102.0 ms (7%) |

- `start-static`: startup: static glibc hello world
- `start-dynamic`: startup: dynamic glibc hello world (ld.so, libc.so.6)
- `integer`: integer code: sieve, hash mixing, division, sorting
- `float`: scalar SSE double precision: Mandelbrot, sqrt, division
- `simd`: SSE2 integer: glibc string functions, vectorized byte loop
- `syscall`: syscall round trips: getppid, write to /dev/null
- `memory`: malloc/free, memset/memcpy, mmap/munmap
- `threads`: 4 threads: independent work, then a mutex and an atomic
- `python-start`: python startup (`-c pass`): code run a few times, translation cost dominates
- `python-int`: python loop, integers (1M iterations)
- `python-float`: python loop, floats (1M iterations)
```
What it says:
- **armor faster than qemu** on startup (0.38–0.50), floating point (0.32), integer SSE2 (0.65), memory (0.76), python (0.34–0.50).
- **armor slower than qemu** in three cases:
  - `integer` (1.21);
  - `syscall` (1.50): every syscall leaves the translated code, goes through the engine, releases and reacquires the GIL;
  - `threads` (2.71): qemu runs the four threads in parallel, armor one at a time (ADR-0018). This is the case that M6's parallel threads must solve (done: section "Parallel threads" further below, 933 → 546 ms).
- **Translation:** it accounts for more than half of the startup of the C programs, 42% of python's startup (97 ms) and 1–7% in long programs.
- **Distance from native:** from 3.5 times (`integer`) to 8 times (python). The startups are not comparable: natively there is nothing to translate or to load.

## How to take a profile
```bash
CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release    # symbols for perf, without touching Cargo.toml
cd tests/guest/data
ARMOR_ROOT=$PWD/../../../target/guest/rootfs perf record -F 999 -g -o /tmp/perf.data \
    ../../../target/release/armor ../../../target/guest/rootfs/usr/bin/python3.13 -c 'import sys; sys.exit(int("x"))'
perf report -i /tmp/perf.data --no-children --percent-limit 1.5 --stdio
```
`perf_event_paranoid` is 2 on the host: you can profile your own processes in user space.

## Interpreter: what it cost and what changed (2026-10-12)
Profile of the traceback of a python exception (the heaviest case in the tests):

| Function | Before | After |
|---|---|---|
| fetching and decoding instructions (`fetch`, `decode`, `modrm`) | 47% | ~15% (the cache misses) |
| page table locking (`ldadd`/`cas` atomics) | 16% | no longer visible |
| execution (`execute`, `run`, memory accesses) | the rest | almost all |

Changes (ADR-0020):
1. **Decoded instruction cache** (`crates/armor-cpu/src/icache.rs`): 32,768 direct-mapped entries per thread (72 bytes each, 2.4 MB). An entry is valid only for the "code generation" in which it was decoded. The generation changes at every write to an executable page and at every mapping, unmapping or permission change that touches executable pages (`armor-mem`).
   - With 4,096 entries python took 5.2 s, with 16,384 4.46 s, with 32,768 4.1 s, with 65,536 4.09 s.
   - At first the generation changed at every `mmap`: python does many of them, and the cache was emptied constantly (6.6 s). Now it changes only if executable pages are involved.
2. **One page table lock per block of instructions**: `AddressSpace::view()` takes the read lock for a whole block of `Cpu::run` (10,000 instructions). Syscalls run outside the block, so an `mmap` from another thread waits at most until the end of the block.
3. **Software TLB in the view** (64 entries of per-page permissions): while the view exists the tables do not change, so the TLB never needs invalidating. Replaced shortly afterwards by the flat table (point 4).
4. **Flat permission table** (ADR-0021, JIT increment 0): one byte per guest page in a 32 GiB array reserved with `MAP_NORESERVE`, in place of the `BTreeMap`. A lookup is a single load; in python's profile the memory access functions no longer appear as separate entries.

Correctness check: `control/self_modifying` (code written, executed, rewritten, remapped), `faults/exec_unmapped` and `faults/exec_after_mprotect` (cached code that must no longer run). Each one fails with the corresponding mutation test (write without invalidation, `munmap` without invalidation, `mprotect` without invalidation).

## Measurements

| Program | Before (2026-10-12) | After | qemu-x86_64 |
|---|---|---|---|
| python 3.13: traceback of an exception | 13 s | 4.1 s | 0.8 s |
| coreutils `sha512sum` (libcrypto startup) | 0.41 s | 0.135 s | 0.048 s |
| coreutils `ls -l` | 0.085 s | 0.042 s | 0.065 s |
| `c/malloc` | 0.357 s | 0.119 s | 0.024 s |
| `c/threads`, 20,000 iterations, 8 threads | 1.9 s | 0.52 s | 0.39 s |
| `cargo xtask difftest` in full (`difftest` profile) | 1 min 32 s | 43 s | — |

After the flat table (same day): python 3.76 s, `sha512sum` 0.130 s, `ls -l` 0.039 s, `malloc` 0.118 s, `c/threads` 20,000 iterations 0.49 s. Python's profile: `execute` 29%, `run` (with the instruction cache lookup) 29%, decoding 4%: what remains is interpretation, the JIT's job.

## JIT, first translator (2026-10-12)
The same measurements with `--jit` (increment 3 of ADR-0021, `tech/jit.md`): guest registers in memory, flags always computed, no chaining.

| Program | Interpreter | `--jit` | qemu-x86_64 |
|---|---|---|---|
| python 3.13: traceback of an exception | 3.76 s | 1.14 s | 0.8 s |
| coreutils `sha512sum` | 0.130 s | 0.066 s | 0.048 s |
| coreutils `ls -l` | 0.039 s | 0.038 s | 0.065 s |
| `c/malloc` | 0.117 s | 0.125 s | 0.024 s |

- **`c/malloc` is slower with the JIT.** It spends its time in SSE `memset`/`memcpy`, which the JIT does not translate: short blocks alternating with interpreted instructions, and each switch costs a call and a block lookup. The first version looked blocks up in a `HashMap` with SipHash and took 0.167 s. With a multiplicative hasher it went down to 0.139 s, then to 0.125 s with a direct-mapped cache in front of the table. The rest is recovered by translating the SSE instructions.
- **python:** 1.59 s with the first version, 1.26 s with the hasher, 1.14 s with the direct-mapped cache.

## JIT with chaining (2026-10-12)
Blocks jump directly to the next one when the destination is known (`tech/jit.md`, increment 5).

| Program | Interpreter | `--jit` before chaining | `--jit` with chaining | qemu-x86_64 |
|---|---|---|---|---|
| python 3.13: traceback of an exception | 3.76 s | 1.14 s | 0.95 s | 0.8 s |
| coreutils `sha512sum` | 0.130 s | 0.066 s | 0.056 s | 0.048 s |
| coreutils `ls -l` | 0.039 s | 0.038 s | 0.039 s | 0.065 s |
| `c/malloc` | 0.117 s | 0.125 s | 0.131 s | 0.024 s |
| `c/threads` (2,000 iterations, 8 threads) | 0.115 s | — | 0.079 s | — |

`c/malloc` does not improve: it spends its time in interpreted SSE instructions, and the budget check on entry to each block costs two instructions. The missing measurements of `c/threads` and qemu were not taken.

## JIT with more translated instructions (2026-10-13)
**What was left to the interpreter.** Before choosing, temporary instrumentation (removed after the measurement) counted the instructions left to the interpreter:
- **python (6.6 million):** 64-bit shifts 2.0 M; 8-bit `test` 1.6 M; SSE 1.3 M (`Mov128` 0.7 M); 8-bit ALU 0.46 M; `movzx ecx, ah` 0.24 M (the bytecode dispatch); then 8/16-bit `mov`, `cdqe`/`cqo`, `setcc`, `imul`, `cmov`;
- **`c/malloc` (2.2 million):** almost only SSE: `Unpack` 0.9 M, `Mov128` 0.6 M, per-lane operations 0.6 M.

Translated the forms in `tech/jit.md`, increment 6. All the measurements in this table were taken at the same time, qemu included:

| Program | Interpreter | `--jit` with chaining | `--jit`, increment 6 | qemu-x86_64 |
|---|---|---|---|---|
| python 3.13: traceback of an exception | 3.76 s | 0.95 s | **0.51 s** | 0.79 s |
| coreutils `sha512sum` | 0.130 s | 0.056 s | **0.042 s** | 0.051 s |
| coreutils `ls -l` | 0.039 s | 0.039 s | **0.035 s** | 0.064 s |
| `c/malloc` | 0.117 s | 0.131 s | 0.106 s | 0.023 s |
| `c/threads` (2,000 iterations, 8 threads) | 0.115 s | 0.079 s | 0.073 s | 0.040 s |

On python, `sha512sum` and `ls -l`, armor with `--jit` is now faster than qemu-x86_64. `c/malloc` stays 4.6 times slower: its SSE instructions (`punpck*`, per-lane operations) still go to the interpreter. `c/threads` is dominated by futex and the GIL.

## Possible next steps (2026-10-12)
Written before M6 and superseded: the JIT steps listed here are done (chaining, lazy flags, registers in host registers, translated SSE: `tech/jit.md`). Today's next steps are in `roadmap.md`.
- The rest of the time is in execution: flags always computed (even when nobody reads them), operands read through generic `match`es, a 64-byte `Instruction` copied at every step. The classic solutions (lazy flags, specialised handlers per instruction) make sense only if the JIT (M6) is late.
- The JIT (M6) removes interpretation: after the first translator, the measurable steps are chaining, live flags, registers in host registers and translated SSE instructions (`tech/jit.md`).

## JIT with integer SSE2 (2026-10-13)
The integer SSE2 operations are translated with NEON and with the general-purpose registers (`tech/jit.md`, increment 7). qemu as in the previous section, measured the same day.

| Program | `--jit`, increment 6 | `--jit`, increment 7 | qemu-x86_64 |
|---|---|---|---|
| python 3.13: traceback of an exception | 0.51 s | **0.485 s** | 0.79 s |
| coreutils `sha512sum` | 0.042 s | **0.035 s** | 0.051 s |
| coreutils `ls -l` | 0.035 s | **0.031 s** | 0.064 s |
| `c/malloc` | 0.106 s | **0.010 s** | 0.023 s |
| `c/threads` (2,000 iterations, 8 threads) | 0.073 s | 0.073 s | 0.040 s |

`c/malloc` spent its time in glibc's SSE `memset`/`memcpy`/`strlen`: translated, it runs ten times faster, and faster than qemu. `c/threads` stays slower than qemu, dominated by futex and the GIL.

## Threads (2026-10-13)
`c/threads` was the only measured program still slower under armor than under qemu (0.073 versus 0.040 s with `--jit`). Profile and `strace -c`:
- **`madvise(MADV_DONTNEED)` 12% of the time:** glibc calls it on the 8 MB stacks of the threads that finish. armor emulated it by zeroing one 4K page at a time with `write_privileged`, so two locks per page and memory made resident. Now a single lock, the host's `madvise` on the whole host pages and zeroing of only the partial pieces (`AddressSpace::discard`);
- **6,200 `munmap`s:** the stacks were returned to the host one 16K page at a time. Now one call per contiguous range: 61.

| `c/threads` (2,000 iterations, 8 threads) | Before | After | qemu-x86_64 |
|---|---|---|---|
| interpreter | 0.115 s | 0.084 s | — |
| `--jit` | 0.073 s | **0.040 s** | 0.040 s |

What remains in the profile is scattered: host futexes, block translation (each thread has its own cache), insertion of the host pages into the `BTreeSet`. A block cache shared between threads would be the next step, to be measured.

## Indirect jumps in the JIT (2026-10-13)
`ret` and jumps through a register look up the next block in a table instead of returning to the engine (`tech/jit.md`, increment 8). Before and after measured together, median of 3: the "before" binary is the previous commit, built in a separate worktree.

| Program | Before | After | qemu-x86_64 |
|---|---|---|---|
| python 3.13: loop of 3 million iterations (`s += i*i % 7`) | 10.53 s | **9.29 s** | 13.9 s |
| python 3.13: traceback of an exception | 0.477 s | **0.458 s** | 0.79 s |

`sha512sum`, `ls -l`, `c/malloc` and `c/threads` do not change measurably. The python loop is the first steady-state benchmark: about 3 µs per iteration under both emulators, so there is still a lot to understand about where the time goes.

## Profile of the python loop (2026-10-13)
Loop `for i in range(1000000): s += i*i % 7` with `--jit`, `perf record` at 4,999 Hz:
- **83% of the time in the generated code** (`memfd:armor-jit`), 12% in armor (engine, interpreter), 5% in libc. The bottleneck is the quality of the translation, not the engine.

To get inside the generated code, temporary instrumentation (removed after the measurement; the files went back to being identical to the commit) wrote, for each block, the emitted words and the offset of each guest instruction. Each `perf script` sample was attributed to the host instruction hit (disassembled with `objdump`) and to its role:

| Role of the host instruction | Samples |
|---|---|
| flag computation (`cset`, `orr`, parity table, `bic`/`orr` on `rflags`) | 31.8% |
| permission check (`tst`, `and`, `lsr`, `ldrb` from the table, comparisons) | 25.7% |
| jumps (exits, chaining, `br` of the indirect jumps) | 14.2% |
| guest memory accesses (the real work) | 13.1% |
| guest registers read and written in the `Cpu` | 7.5% |
| rest (operations, addresses, budget) | ~8% |

Per guest instruction the time is scattered (no instruction above 0.7%): `mov` with memory 22%, `push`/`pop` 17%, ALU instructions that write flags about 30%, `jcc` 10%.

**Consequences:**
- static register mapping (ADR-0021 §4) would be worth at most 7.5%;
- the candidates are flag computation (today complete at every instruction that writes them, even when the next one overwrites them) and the permission check (9 instructions per access).

## Lazy flags (2026-10-13)
`tech/jit.md`, increment 9. Before and after measured together (`--jit`, median of 3, the "before" built in a worktree from the previous commit):

| Program | Before | After |
|---|---|---|
| python, loop of 1 million iterations | 3.17 s | 3.00 s |
| python, traceback | 0.458 s | 0.463 s |
| `sha512sum` | 0.036 s | 0.035 s |
| `c/malloc` | 0.010 s | 0.010 s |
| `c/threads` | 0.042 s | 0.044 s |

The gain (−5% on the loop) is close to the estimate made before writing the code (about 7%): most of the computed flags are really needed by the jump at the end of the block.

## Flag table (2026-10-13)
`tech/jit.md`, increment 10. Python loop of 1 million iterations, 7 alternating runs per binary (the "before" is the commit of increment 9):
- before: 2990 3034 3134 **3143** 3203 3206 3274 ms;
- after: 2934 2939 2951 **2957** 2996 3004 3082 ms.

Median −6%. Between runs of the same binary the noise is about ±4%: gains of this size need more alternating runs, not the median of 3. Traceback, `sha512sum`, `c/malloc`, `c/threads` unchanged within the noise.

## Shorter permission check (2026-10-13)
`tech/jit.md`, increment 11. Python loop, 7 alternating runs per binary:
- before: 2908 2914 2931 **3111** 3218 3240 3568 ms;
- after: 2786 2860 2926 **2931** 2967 3043 3129 ms.

Median −6%. The same "before" binary in the increment 10 series had a median of 3.06 s: the gain is between 3% and 6%, at the edge of the noise. The other programs are unchanged.

## Lazy flags across blocks (2026-10-13)
`tech/jit.md`, increment 12. Profile redone before starting (same temporary instrumentation, removed afterwards): flags ~30%, jumps 26% (including the permissions' `tbz`s), permission check 14%, work and other 12%, registers in the `Cpu` 9%, memory accesses 6%. Python loop, two series of 7 alternating runs:
- before: medians 2879 and 3006 ms;
- after: medians 2578 and 2759 ms (−8–10%).

The first version (a routine with comparisons in a row) improved nothing; with the jump table it did. Traceback 0.442 → 0.419 s; the other programs unchanged.

## Discarded attempt: cache of the last checked page (2026-10-13)
After increment 12 the profile gave 41% of the time to the permission check (including the `tbz`s). Attempt: two registers with the last page found readable and the last one found writable. An access entirely inside that page was checked with `sub`, `cmp`, `ccmp`, `b.eq` (no load); the others did the full check out of line and updated the remembered page.

Correct (tests, `difftest` and lockstep green, mutations caught), but slower: python loop 2.58 → 3.27 s (+25%), traceback 0.41 → 0.48 s. python constantly alternates stack, heap objects and constants, so one page per access type is not enough and almost every access paid for the out-of-line path, more expensive than before. Code reverted.

Two tests born here remain: a read-only page in the memory of the random tests, and reads and writes near address 0 (`null_pointer_accesses_fault`).

**Reading of the profile to revisit.** With the guest registers in the `Cpu`, every dependency between instructions goes through a `str` and an `ldr` from the same address (store-to-load forwarding, a few cycles). `perf` attributes the wait to the instruction that consumes the value: often it is the permission check, which immediately uses the address just loaded. The 7.5% attributed to "registers in the `Cpu`" therefore underestimates the cost of the provisional choice; static register mapping (ADR-0021 §4) is the next candidate.

## Guest registers in host registers (2026-10-13)
`tech/jit.md`, increment 13. Register use weighted by time in the python loop, measured before choosing: rsp 22%, rax 21%, rbx 9%, rdx 8%, rbp 8%, rcx 7%, r12 6%, rdi 4%, r15 4%, rsi 3%, r13 3%, r14 2%, r8–r11 less than 1% each. Python loop, two series of 7 alternating runs:
- before: medians 2553 and 2649 ms;
- after: medians 2320 and 2278 ms (−9% and −14%).

Traceback 0.409 → 0.429 s: each pass through the engine now costs about 30 more instructions between prologue and epilogue, and the traceback makes many of them (interpreted instructions). The other programs unchanged within the noise.

## `div` and narrow shifts translated (2026-10-13)
`tech/jit.md`, increment 14. Instructions left to the interpreter, counted beforehand with temporary instrumentation (removed afterwards):
- python loop, 300,000 iterations: 943,000 instructions, of which 64-bit `div` 535,000 (the `% 7`) and 16-bit shifts 305,000;
- traceback: 159,000, of which 8-bit and 16-bit shifts 93,000, `bsf`/`bsr` 23,000, `bt*` 14,000, `bswap` 9,500.

Python loop, 7 alternating runs: median 2272 → 1983 ms (−13%). Traceback 0.434 → 0.416 s.

## `bsf`/`bsr`, `bt*`, `bswap` (2026-10-13)
`tech/jit.md`, increment 15. In the traceback they were about 46,000 interpreted instructions out of 159,000. Traceback, 5 alternating runs: median 0.419 → 0.419 s; python loop 2.010 → 2.014 s. No measurable gain: these instructions cost little compared with the rest. The increment widens the translator's coverage, not the speed of these programs.

## Summary after increment 15 (2026-10-13)
Python loop of 1 million iterations, armor `--jit` and qemu-x86_64 measured together, 5 alternating runs: armor 2027 2044 **2153** 2181 2295 ms; qemu 4816 4890 **4928** 5088 5101 ms. armor is about 2.3 times faster than qemu on this loop.

A measurement error, corrected before writing these numbers: a first series (armor 2.5–2.8 s) had been taken with the release binary still built with the profile instrumentation. The patch had been removed from the sources but the binary had not been rebuilt. After every profile with instrumentation, rebuild before measuring.

## Discarded attempt: one check per group of accesses (2026-10-13)
**Estimate.** Before writing the code, on the same profile of the python loop (temporary instrumentation, then removed and the binary rebuilt): the permission check was 39% of the samples in the generated code. Only 23% of those checks concerned accesses covered by an earlier one in the same block (same base with the same value, `rsp` moved by `push`/`pop` included, within 2 KB, same type): about 9% of the time in the generated code.

**The attempt.**
- A first pass recorded the accesses of each block; those with the same base and the same type formed a group.
- The first access of the group checked the range of all of them, if it fit in one page, and recorded it in a bit of `x18`.
- The following accesses cost one `tbz`. If the range did not pass, each one did its own check out of line, so as to avoid pathological cases.

Correct: tests, `difftest` with both engines and in lockstep, six mutations caught after adding a targeted test.

**Measurement**, against the previous commit:

| Program | Before | After |
|---|---|---|
| python loop | 2.00 s | 2.00 s |
| traceback | 0.413 s | 0.481 s |
| `sha512sum` | 0.034 s | 0.043 s |
| `c/malloc` | 0.010 s | 0.012 s |
| `c/threads` | 0.039 s | 0.049 s |

No gain on the loop, and 16–26% worse on the programs that translate a lot: the first pass translated each block twice. A lighter analysis would have to reproduce exactly the order of the emitted checks, and a mistake there would skip the wrong check. Code reverted.

The test born here remains (`checks_follow_base_and_stack_changes`): base changed between two accesses, `push`/`pop` at the edges of the stack, `rsp` moved by a `sub`, accesses straddling valid pages and a block followed by another one that reaches the guard page.

Conclusion for now: the cost of the permission checks remains; other ideas (known 32-bit stack addresses, checks hoisted out of loops) require broader analyses and must be evaluated with the same method.

## Instructions with `LOCK`, `xchg`, one-operand `mul` (2026-10-13)
`tech/jit.md`, increment 16. Instructions left to the interpreter, counted beforehand on the python loop and traceback, `ls -l`, `c/malloc`, `c/threads` and perl: 127,000 in all, of which 32-bit `xchg` 35,000, `lock add` 18,000, `lock cmpxchg` 35,000, one-operand `mul` 17,000, SSE (floating point) 11,500, strings 4,000.

Measurements, before and after together (median of 3, python loop of 7):
- `c/threads` 0.041 → **0.033 s** (qemu 0.040: now faster);
- python loop 2.05 → 2.13 s, within the noise (±4%);
- traceback 0.414 → 0.413 s;
- `sha512sum` 0.032 → 0.032 s;
- `c/malloc` unchanged.

**Correction of a measurement error.** The `sha512sum` measurements in this document used `sha512sum stdin.txt`, a file that does not exist in the data directory (the tests' file is `fruits.txt`). They therefore measured the loading of libcrypto plus an error, not a hash. They remain a valid comparison between the engines on the same work, but not a hash. With `fruits.txt` (2026-10-13): armor `--jit` 0.032 s, interpreted 0.130 s, qemu 0.047 s. The file is small, so it is still almost only the startup that counts.

## Scalar SSE floating point in the JIT (2026-10-13)
`tech/jit.md`, increment 17. Floating-point python loop (`for i in range(1000000): s += i*0.5/3.0`), 5 alternating runs:
- before: 1943 1959 **1977** 2019 2024 ms;
- after: 1605 1641 **1679** 1730 1730 ms (−15%);
- qemu-x86_64: 4224–4298 ms;
- identical result under armor and qemu.

The other programs are unchanged (integer loop 2.00 → 2.02 s, traceback 0.417 → 0.411 s).

## Permission check with the "next page too" bit (2026-10-13)
`tech/jit.md`, increment 18.

**Estimate of the maximum.** Before looking for a new idea, the value of the whole check was measured: an experimental binary (never committed) that, with an environment variable, emitted no permission check at all. Python loop 1991 → 1493 ms. The check is therefore worth at most 25%, and that is the ceiling for any idea on this front.

**The idea.** In the fast path of a multi-byte access there were three instructions (`and`, `cmp`, `b.hi`) only to rule out a page crossing, a rare case. A bit in the table that says "this page and the next one have the same permission" makes the access legal even when it crosses the boundary. The crossing test remains only out of line, when the bit is missing. The fast path goes from 7 to 5 instructions, without block analysis and without extra translation cost.

**Measurement**, before and after together:
- python loop, 7 alternating runs: before 1987 2040 2054 **2057** 2084 2205 2254 ms; after 1759 1822 1843 **1861** 1894 2105 2306 ms (−9.5%);
- `bench.sh` (median of 3): loop 2015 → 1855 ms, traceback 410 → 404 ms, `sha512sum` 32 → 32 ms, `c/malloc` 10 → 10 ms, `c/threads` 36 → 37 ms (within the noise).

Against qemu, 10 alternating runs: armor `--jit` median 1.86 s (1.75–2.14), qemu-x86_64 4.92 s (4.76–5.09).

About 10 of the 25 possible points are recovered. The rest is in the `tst`/`b.ne` on the 47-bit limit, in the table read and in the `tbz`: removing them requires checks eliminated entirely, not shortened.

## Parallel threads (ADR-0023, 2026-10-13)
`cargo xtask bench threads integer syscall python-int start-dynamic`, 5 rounds, after step 3 (the GIL removed):

| Benchmark | Before (GIL) | After | qemu | native |
|---|---|---|---|---|
| `threads` | 933 ms | **546 ms** | 331 ms | 116 ms |
| `integer` | 400 ms | 400 ms | 335 ms | 117 ms |
| `syscall` | 480 ms | 480 ms | 322 ms | 116 ms |
| `python-int` | 1815 ms | 1827 ms | 4951 ms | 220 ms |
| `start-dynamic` | 13.3 ms | 13.4 ms | 28.9 ms | 0.5 ms |

- Single-threaded programs do not change: the concurrent mode switches on only with the second thread or with shared memory.
- `threads` is 1.7 times faster, but stays 1.65 times slower than qemu: in the part with the mutex and the atomic, every `LOCK` instruction leaves the translated code and goes through the interpreter. LSE atomics in the JIT (step 4 of ADR-0023) are the next candidate (done: next section).
- `c/threads`: 34 ms before step 1, 44 ms at steps 1 and 2 (with the GIL and the `LOCK`s in the interpreter), 37 ms after step 3.

## LSE atomics in the JIT (ADR-0023 step 4, 2026-10-13)
`cargo xtask bench --runs 5 threads integer syscall python-int start-dynamic`, on an idle machine, commit `0f2ed00` plus step 4:

| Benchmark | Step 3 | Step 4 | qemu | native | armor/qemu |
|---|---|---|---|---|---|
| `threads` | 546 ms | **428 ms** (427–429) | 290 ms | 92.7 ms | 1.48 |
| `integer` | 400 ms | 398 ms | 333 ms | 116 ms | 1.20 |
| `syscall` | 480 ms | 481 ms | 317 ms | 116 ms | 1.52 |
| `python-int` | 1827 ms | 1867 ms | 4895 ms | 232 ms | 0.38 |
| `start-dynamic` | 13.4 ms | 13.3 ms | 28.7 ms | 0.5 ms | 0.46 |

- `threads` drops by 22%: the `LOCK` instructions of the mutex and of the atomic counter no longer leave the translated code. qemu measures 290 ms in this round (331 in the step 3 round): the ratio goes from 1.65 to 1.48.
- The others do not change (within the noise): a single-threaded program never enters concurrent mode.
- **Where the rest goes** (`perf record` of `armor --jit` on `threads.x86_64`): 91% of the time in the translated code, almost all in a single loop, the independent computation of each thread (multiplications, xors and shifts on registers); 2% in the page table lock (`RwLock::write_contended`). It is the same gap as `integer` (1.20), not a cost of the threads.

## Dead flags (JIT increment 19, 2026-10-13)
**How it was found.** The profile of `bench threads` (`perf record` on `armor --jit target/bench/threads.x86_64`) put 91% of the time in the translated code, almost all in each thread's compute loop. To read that code:
```bash
x86_64-linux-gnu-objdump -d target/bench/threads.x86_64 --start-address=0x401800 --stop-address=0x40181e   # the loop's bytes
ARMOR_DUMP_HEX=4831d0… ARMOR_DUMP_RIP=401800 ARMOR_DUMP_OUT=/tmp/blocco.bin \
  cargo test -p armor-jit dump_translation -- --ignored                    # translates the block
objdump -D -b binary -m aarch64 /tmp/blocco.bin                             # the AArch64 code
```
The hot addresses of `perf report` (`memfd:armor-jit`, offset in the buffer) fell right after the reads of the flag table. The code showed about 83 AArch64 instructions for 8 x86 instructions, almost half of them for flags never read (`tech/jit.md`, "Dead flags").

`cargo xtask bench --runs 5`, all programs, on an idle machine; commit `748304e` plus increment 19:

| Benchmark | Before | After | qemu | native | armor/qemu |
|---|---|---|---|---|---|
| `start-static` | 6.7 ms | 6.9 ms | 17.7 ms | 0.3 ms | 0.39 |
| `start-dynamic` | 13.3 ms | 13.5 ms | 28.9 ms | 0.5 ms | 0.47 |
| `integer` | 398 ms | **318 ms** | 333 ms | 116 ms | 0.96 |
| `float` | 230 ms | 231 ms | 719 ms | 50.8 ms | 0.32 |
| `simd` | 226 ms | 221 ms | 348 ms | 32.5 ms | 0.63 |
| `syscall` | 481 ms | 480 ms | 318 ms | 116 ms | 1.51 |
| `memory` | 254 ms | 235 ms | 335 ms | 102 ms | 0.70 |
| `threads` | 428 ms | **253 ms** | 291 ms | 93.6 ms | 0.87 |
| `python-start` | 231 ms | 228 ms | 467 ms | 13.2 ms | 0.49 |
| `python-int` | 1867 ms | **1739 ms** | 4907 ms | 224 ms | 0.35 |
| `python-float` | 1493 ms | **1408 ms** | 4283 ms | 178 ms | 0.33 |

"Before" comes from the latest measurement of each program: the first measurement of 2026-10-13 for `start-static`, `float`, `simd`, `memory`, `python-start`, `python-float`; the LSE atomics (above) for the others. Differences below 3% are within the noise. `memory` (254 → 235 ms) already had a wide spread in the first measurement (243–273): without a direct comparison it is not attributed to this increment.
- `integer` and `threads` are now faster than qemu. Only `syscall` stays slower (1.51). Syscalls are not translated: each one leaves the translated code and goes through the engine and armor's Linux layer. Where the time goes is not measured yet.
- The python loops gain 6–7%: there the flags weighed less (10% in the profile of increment 15) and the largest cost remains the permission check.

## The syscall path (2026-10-13)
**Profile** of `bench syscall` (300,000 `getppid`s and 300,000 one-byte `write`s to `/dev/null`), `perf record` and `strace -c` on `armor --jit`:
- 42% of the time in the kernel, and about 25% in atomic functions (`__aarch64_cas4_acq`, `ldadd`, `swp`) of locks and counters;
- `strace -c`: for each guest syscall armor made **three** to the host: the real syscall, a `futex` and a `getpid` (600,032 `futex` and 600,033 `getpid` for 600,000 guest syscalls).

**Causes**, found in the code:
- **`futex`:** the `run_thread` loop took the GIL even before the concurrent mode, when there is a single guest thread. Releasing it calls `Condvar::notify_all`, which makes a `futex` syscall even with nobody waiting. The GIL was taken at every return to the engine: at every syscall and every ~10,000 instructions;
- **`getpid`:** the JIT engine compared the pid at every `run` to notice a `fork` (`std::process::id()` is a real syscall).

**Fixes:**
- the GIL is taken only where ADR-0023 wants it: in lockstep and with `ARMOR_GIL=1`. Before the concurrent mode there is a single thread: the second one switches it on before starting;
- a `fork` is recognised from a counter incremented in the child by a `pthread_atfork` handler (`tech/jit.md`, "Fork").

After: a single host syscall per guest syscall (`strace -c`). Verification: `difftest` with both engines and in lockstep; tests with threads and shared memory 20 out of 20 per engine and 5 out of 5 with `ARMOR_GIL=1`; two mutations of the fork detection caught (`c/processes` and `busybox/sh-pipeline` with `--jit` end with SIGSEGV).

`cargo xtask bench --runs 5`, all programs, on an idle machine, commit `de5b945` plus these fixes:

| Benchmark | Before (increment 19) | After | qemu | native | armor/qemu |
|---|---|---|---|---|---|
| `start-static` | 6.9 ms | 6.9 ms | 17.7 ms | 0.3 ms | 0.39 |
| `start-dynamic` | 13.5 ms | 13.5 ms | 28.7 ms | 0.5 ms | 0.47 |
| `integer` | 318 ms | 302 ms | 332 ms | 116 ms | 0.91 |
| `float` | 231 ms | 223 ms | 718 ms | 50.8 ms | 0.31 |
| `simd` | 221 ms | 210 ms | 348 ms | 32.5 ms | 0.60 |
| `syscall` | 480 ms | **201 ms** | 317 ms | 116 ms | 0.63 |
| `memory` | 235 ms | 228 ms | 332 ms | 102 ms | 0.69 |
| `threads` | 253 ms | 249 ms | 290 ms | 93.4 ms | 0.86 |
| `python-start` | 228 ms | 228 ms | 466 ms | 13.2 ms | 0.49 |
| `python-int` | 1739 ms | **1570 ms** | 4923 ms | 225 ms | 0.32 |
| `python-float` | 1408 ms | **1333 ms** | 4285 ms | 181 ms | 0.31 |

- `syscall` goes from 1.51 to 0.63 times qemu. The cost that remains beyond native is about 0.14 µs per syscall (against 0.6 before).
- Programs without many syscalls improve too (`python-int` −10%, `integer` −5%): the GIL was taken at every return to the engine, not only at syscalls.
- armor with `--jit` is now faster than qemu on all 11 programs.
- `threads` has, in both measurements, one run much faster than the others (163–168 ms against ~250): not understood yet, to be looked at if `threads` becomes a goal.

## Software SSE in the interpreter (ADR-0024, 2026-10-13)
The interpreter computes SSE floating point with the integer engine of `f80.rs` instead of Rust's `f32`/`f64`, to have exact MXCSR flags (`tech/floating-point.md`). The JIT translates the same scalar instructions with the host's instructions as before. `cargo xtask bench --runs 5` (the interpreter with `--interpreter --runs 3`), commit `35b7810` against the changes:

| Benchmark | Before | After | qemu |
|---|---|---|---|
| `float`, `--jit` | 222 ms | 223 ms | 717 ms |
| `python-float`, `--jit` | 1345 ms | 1323 ms | 4367 ms |
| `float`, interpreter | 4574 ms | 11381 ms | 715 ms |

- With the JIT no measurable difference: the hot loops are translated.
- The pure interpreter is 2.5 times slower on SSE floating point, as foreseen by ADR-0024. It matters for the instructions that the JIT leaves to the interpreter: the packed forms, and NaN results.

## MXCSR flags in the JIT (ADR-0024, JIT increment 20, 2026-10-13)
The translated code checks for denormal operands (DE) and corrects underflow at the boundaries (`tech/jit.md`, "MXCSR flags"). `cargo xtask bench --runs 5`, commit `a224111` against the changes:

| Variant | `float` | `python-float` |
|---|---|---|
| before (no flags in the JIT) | 223 ms | 1323 ms |
| FPSR saved with `mrs` before every `mul`/`div` and conversion, restored before the slow path (discarded) | 374 ms | 1356 ms |
| the same without `mrs` (only for measuring) | 275 ms | — |
| the same without the denormal checks (only for measuring) | 305 ms | — |
| UFC corrected out of line, no `mrs` in the fast path | 275 ms | 1354 ms |
| plus: denormals with the continuation out of line | 276 ms | — |
| plus: bits of the xmm operands read with `ldr` instead of `fmov` (version kept) | **267 ms** | — |
| without the denormal checks (only for measuring) | 235 ms | — |

- `mrs fpsr` waits for the in-flight operations to finish: in the Mandelbrot loop it alone cost about 100 ms. The underflow check costs about 12, the denormal check about 30.
- `float` stays at 0.37 times qemu (718 ms); `python-float` does not change: there floating point is a small part of the time.

## Comparison with box64 (2026-10-14)
`cargo xtask bench --box64` (and `--fex`) adds the column of another engine, from the binary in `ARMOR_BOX64` (`ARMOR_FEX`); an output different from qemu's or a failure is marked in the cell. First measurement (Pi 5, 16K): box64 v0.4.4 2 to 3 times faster than armor on computation (python-int 507 ms against 1572 ms, simd 87 against 221), armor faster on static startup, box64 fails on the `threads` workload. Table and reading in `tech/engines.md`.
