# AArch64 JIT (M6)

State as of 2026-10-13: **design accepted** (ADR-0021). The JIT works (`armor --jit`) and is described as it is today in the section "How it is made today". Increments done, described further below in the order in which they were written:
- increment 0: flat permission table (python from 4.1 to 3.76 s, `tech/performance.md`);
- increment 1: AArch64 encoder;
- increment 2: code buffer and execution;
- increment 3: first translator, `--jit` option, `difftest` with both engines (python from 3.76 to 1.14 s);
- increment 4: lockstep (`--lockstep`);
- increment 5: chaining of direct jumps (python 1.14 → 0.95 s);
- increment 6: more translated instructions, that is 8/16 bit, shifts, `imul`, `cmov`/`setcc`, SSE moves (python 0.95 → 0.51 s, qemu 0.79 s);
- increment 7: integer SSE2 with NEON (`c/malloc` 0.106 → 0.010 s, qemu 0.023 s);
- increment 8: indirect jumps through a table, without going back to the engine (python loop −12%);
- increment 9: lazy flags inside the block (python loop −5%);
- increment 10: flags computed with a table indexed by NZCV and by the low byte of the result (python loop −6%);
- increment 11: shorter permission check, with derived bits in the page table (python loop −3–6%, at the edge of the noise);
- increment 12: `jcc` evaluated from the pending operands, and lazy flags across blocks too (python loop −8–10%);
- increment 13: 12 guest registers in host registers (ADR-0021 §4; python loop −9–14%);
- increment 14: 32/64-bit `div`/`idiv` and 8/16-bit shifts translated (python loop −13%);
- increment 15: `bsf`/`bsr`, `bt*`, `bswap` translated (no measurable gain on the test programs);
- increment 16: `xchg`, `cmpxchg`, `xadd`, `LOCK`, one-operand `mul`/`imul` translated (`c/threads` −20%);
- increment 17: scalar SSE floating point translated (floating-point python loop −15%);
- increment 18: "next page too" bit in the page table, full check only near the page boundary (python loop −8–10%);
- ADR-0023, steps 1–3: concurrent mode with ordered accesses (`LDAPR`/`STLR`), `mfence`/`lfence`/`sfence` barriers translated, guest threads in parallel (`bench threads` 933 → 546 ms; section "Memory ordering in concurrent mode");
- ADR-0023, step 4: LSE atomics for `LOCK` instructions in concurrent mode (`bench threads` 546 → 428 ms);
- increment 19: dead flags not computed, with a per-block liveness analysis (`bench integer` 398 → 318 ms, faster than qemu; `threads` 428 → 253 ms; python loop −7%);
- M11, 2026-10-20: liveness per flag, dead flags for every instruction, shifts and multiplications with deferred flags, only CF for `inc`/`dec`/`adc`/`sbb` (`python-float` −8%, section "Dead and deferred flags everywhere");
- M11, 2026-10-20: operations in place on mapped registers, addresses without copies (7-Zip −9%, section "Operations in place").

Next steps: `roadmap.md`. Permission checks remain the largest item of the generated code: without any check the python loop would be 25% faster; increment 18 recovers about 10% of that (`tech/performance.md`).

## How it is made today (2026-10-13)
Summary of the current code (`crates/armor-jit`). The per-increment sections, further below, each describe the code at that moment: registers and details there may be superseded by later increments.

**Modules:**
- `encoder.rs`: AArch64 assembler for the subset that is needed, verified by `cargo xtask encoder-check`;
- `buffer.rs`: executable memory (W^X with a double mapping);
- `translator.rs`: from a block of x86 instructions to AArch64 code; safe code, without `unsafe`;
- `engine.rs`: block cache, execution, chaining, fallback to the interpreter;
- `lockstep.rs`: the instruction-by-instruction oracle;
- `stats.rs`: translation counters for the whole process (blocks, guest instructions, AArch64 instructions, time), printed at exit when `ARMOR_JIT_STATS` is set. `cargo xtask bench` uses them (`tech/performance.md`). On python startup: 34,503 blocks, 146,657 guest instructions, 3.48 million AArch64 instructions (about 24 per guest instruction, prologues, exits and slow paths included), 100 ms (2026-10-13, after increment 19, which hardly changes this total: dead flags weigh in hot loops, not in the bulk of the code).

**Block.** At most 64 guest instructions; it ends at an unconditional jump, a call or a return, at the first untranslated instruction or at the limit. A conditional jump inside a block is a side exit (since 2026-10-15, below). The engine runs it through `enter_routine`, an `extern "C" fn(cpu, page_table, context, block) -> state` shared by every block of the buffer (since 2026-10-20; before, each block had its own prologue and epilogue, about 80 of the 183 AArch64 instructions of an average block on Python's startup):
- **prologue** (`enter_routine`, at the start of the buffer with the flag routine), executed only when the engine calls a block: saves `x19`–`x30` on the stack, loads the mapped guest registers, the xmm registers (`ldp q`), `rflags` and the budget, and branches to the block. `CHAINED_ENTRY` is 0: a block starts at its budget check, whether the engine or another block jumps there (96 bytes until 2026-10-20, past the block's own prologue);
- **body:** at the start it subtracts the block's instruction count from the budget, then the translated instructions;
- **exits:** towards a known address, a rewritable `b` (chaining); towards a computed address, the lookup in the indirect jump table; otherwise the epilogue. A conditional jump that is not the block's last instruction branches, when taken, to an out-of-line exit with the flags state of that point, and the block goes on;
- **epilogue** (`leave_routine`, shared; a block reaches it through `Context::leave`, two instructions, so that its code stays independent of where it is published), on the way to the engine: computes the flags still pending, writes registers, xmm registers, `rflags` and `rip` back into the `Cpu`, and returns the status (`Context::status`, recorded by one entry per status that the block's exits share): continue, interpret the instruction at `rip`, or continue and link the exit.

**Host registers:**

| Register | Use |
|---|---|
| `x0`, `x1`, `x2` | `Cpu`, page table, `Context` |
| `x22`–`x29` | rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi |
| `x8`, `x21` | r8, r9 (since 2026-10-20) |
| `x18`, `x30` | r10, r11 (since 2026-10-15) |
| `x3`, `x9`, `x16`, `x17` | r12, r13, r14, r15 |
| `x12`, `x13` | `rflags`, budget |
| `x19`, `x20` | lazy flags: operands (`FA`, `FB`); the code of the pending operation is `Context::kind` (in `x21` until 2026-10-20) |
| `x4`–`x7`, `x10`, `x11`, `x14`, `x15` | temporaries; `x7` is the guest address, `x4` also the exit `rip` |
| `v16`–`v31` | xmm0–xmm15 (since 2026-10-20), loaded by the prologue and stored by the epilogue |
| `v1`, `v2` | temporaries for SSE operations |
| `v3`, `v4` | the two MMX scratch registers inside one MMX instruction (the interpreter's `Cpu::mm_scratch`, which translated code leaves zero) |

`x18` is free for programs on Linux (the platform register only elsewhere). `x30` is the link register only for the one call blocks make, to the flag routine (generated code, which touches neither): the block keeps it on the stack around that `blr`.

**Lazy flags.**
- Instructions that write all the flags by themselves (`add`, `sub`, `cmp`, `and`, `or`, `xor`, `test`, `neg`), `inc`/`dec`, shifts of a 32- or 64-bit register and multiplications save only the operands (since 2026-10-20 also shifts and multiplications, whose kinds only the block itself knows: an exit computes them first).
- `jcc` after a known operation redoes the operation and jumps with the host condition.
- The other readers have the flags computed: inline when the operation is known, otherwise with the shared routine at the start of the buffer.
- On the exit to another block only the code in `KIND` remains; the epilogue towards the engine always computes.
- The computation uses `mrs nzcv` and a 16 KB table (`FLAG_TABLE`) for CF, PF, ZF, SF, OF, plus AF separately.
- **Dead flags** (increment 19, per flag since 2026-10-20): a backward pass over the block (`live_flags`) finds the flags nobody will see. An instruction whose flags are all dead does not compute them; one that rewrites them all without reading them does not first materialise the pending ones; `inc`, `dec`, `adc` and `sbb` take only the pending CF.

**Guest memory accesses:** address below 2^47, `ldrb` from the page table and `tbz` on the "readable" or "writable and not executable" bit. For multi-byte accesses the `tbz` looks at the bit that also holds for the next page: if it is missing, an out-of-line piece checks the page alone and the crossing. In every other case there is the **slow path**: the instruction goes to the interpreter, with the exact state from before the instruction (pending flags included). Writes to code always go that way, so old translations are thrown away.

**Engine**, one cache per guest thread:
- block table with a direct-mapped cache in front;
- indirect jump table (4,096 entries) read by the generated code;
- 64 MiB buffer, flushed together with the tables when the code generation changes or when it is full;
- a child after `fork` starts again with its own buffer;
- `Jit::run` executes about 10,000 instructions, then returns to `run_thread` (signals; GIL outside concurrent mode).
- In concurrent mode (ADR-0023) the engine translates with `Options::concurrent`: loads `LDAPR`, stores `STLR`, misaligned addresses out of line with a barrier, `DMB` around SSE accesses, `mfence` as `DMB ISH`; `LOCK` and `xchg` with memory as LSE atomics (`LDADDAL`, `SWPAL`, `CASAL`…) if the operand is aligned, otherwise to the interpreter (section "Memory ordering in concurrent mode").

**What it translates:**
- 8–64-bit integers: `mov`, extensions, `lea`, ALU, `test`, unary operations, shifts and rotations (not `rcl`/`rcr`), one-, two- and three-operand `imul`, `mul`, 32/64-bit `div`/`idiv`, `cmov`, `setcc`, `cbw`…`cqo`, `bsf`/`bsr`, `bt*`, `bswap`;
- 32/64-bit `xchg`, `cmpxchg`, `xadd` and the `LOCK` prefix on ALU and unary operations; the atomic forms (`LOCK` and `xchg` with memory) as ordinary read-modify-writes outside concurrent mode (increment 16), with LSE atomics inside it (ADR-0023, step 4), except `lock adc`/`sbb`/`neg`;
- stack and jumps: `push`/`pop`, jumps, direct and indirect `call`/`ret`;
- SSE2: moves and integer operations; since 2026-10-14 also `pshufb` and `palignr` (SSSE3) and MMX (below, "MMX and SSSE3");
- scalar SSE floating point: arithmetic, `min`/`max`, `sqrt`, comparisons, `cmpss`/`cmpsd`, conversions, `movmskps`/`movmskpd`, with the MXCSR exception flags (increment 20). Cases with NaN, out-of-range conversions and denormal operands go to the slow path (`tech/floating-point.md`).

Left to the interpreter:
- x87, and packed SSE floating point (`addps` & co., `cmpps`, packed conversions, SSE3), SSSE3 except `pshufb` and `palignr`, the MMX forms listed below, SSE4.1, SSE4.2, `popcnt`, `rcpss`/`rsqrtss`, and the integer SSE2 instructions added in M4 (saturating additions, `pavg`, `pmulhw`/`pmullw`, `pmaddwd`, `psadbw`, shifts with the count in xmm, `maskmovdqu`);
- string instructions;
- 8/16-bit `adc`/`sbb`, 8/16-bit `cmpxchg`/`xadd`/`xchg`, 8/16-bit `mul`/`div`, `LOCK` on `bt*`, `cmpxchg8b`/`cmpxchg16b`, `rcl`/`rcr`, 32/64-bit shifts on memory;
- in concurrent mode: `lock adc`/`sbb`/`neg` and every atomic instruction on a misaligned operand (split lock);
- syscalls.

Exact lists per increment further below.

**Verification:**
- `difftest` with both engines, with `--jit` and also in lockstep (`--lockstep`);
- JIT-versus-interpreter differential tests in `crates/armor-jit/src/tests.rs`: random programs, also run in lockstep, plus targeted tests;
- `encoder-check`;
- mutation tests for each increment (`testing.md`).

## Host facts
- ✅ `CONFIG_ARM64_VA_BITS=47`, 16K pages (`environment.md`).
- ✅ `overcommit_memory` = 0; a 32 GiB `mmap` with `MAP_NORESERVE` succeeds, and touching three sparse pages costs three pages (test program of 2026-10-12).
- ✅ CPU features (`/proc/cpuinfo`): `atomics` (LSE), `lrcpc`, `asimd`, `fp`, `crc32`, `aes`, `sha1`, `sha2`, `asimddp`.
- ✅ Native GNU objdump 2.44 (`/usr/bin/objdump`) to verify the encoder.

## Pieces and order of the increments
0. **Flat permission table** (`armor-mem`): one byte per guest page (R, W, X bits; the "page with translated code" bit planned here was not done, see "Self-modifying code" below), reserved with `MAP_NORESERVE`. It replaces the `BTreeMap` and the TLB in the interpreter's hot paths, which becomes faster even before the JIT. The `BTreeMap` stays only where iteration is needed (unmapping, `Drop`).
1. **AArch64 encoder** (crate `armor-jit`, `encoder.rs`) ✅ 2026-10-12: `movz`/`movn`/`movk` and 64-bit constants in at most 4 instructions, `add`/`sub`/`adds`/`subs` (immediate and shifted register), `adc`/`sbc`, `madd`/`msub`, `smulh`/`umulh`, `sdiv`/`udiv`, logical operations (shifted register, inverted too, and bitmask immediates), shifts (register and immediate, `ubfm`/`sbfm`, `ror`), `csel`/`csinc`/`csinv`/`csneg`/`cset`, loads and stores (8/16/32/64 bit, immediate or register offset, with sign extension), `ldp`/`stp` (offset, pre- and post-index), branches (`b`, `bl`, `b.cond`, `cbz`/`cbnz`, `tbz`/`tbnz`, `br`, `blr`, `ret`) with labels, `mrs`/`msr nzcv`, `nop`, `brk`.
   - **Verification:** `cargo xtask encoder-check` also writes each sample of `armor_jit::check` as assembly text, assembles it with `as` (the host's GNU 2.44) and compares the words. The text is built from the operands, not from the encoding, so a wrong bit shows up. 11,998 samples, 12,033 instructions, 0 differences; two errors introduced on purpose (in the bitmask immediate and in `brk`) give 960 and 1 differences.
   - `mov_imm64` chooses the sequence itself: its verification is a unit test that executes the sequence and compares the value.
2. **Code buffer** (`buffer.rs`) ✅ 2026-10-12: **double mapping** of shared memory, an RW view to write and an RX view to execute. Until 2026-10-13 it was a `memfd`; now it is an anonymous shared mapping, and the second view comes from `mremap` with old size 0, which maps the same pages again: the `ftruncate` of the `memfd` fell under `RLIMIT_FSIZE`, and with `--limit-file-size` the kernel killed armor with SIGXFSZ as soon as the JIT started (found by `sandbox-check`). No page is ever writable and executable at the same time, and publishing code does not need `mprotect`. Chosen without a measurement: it avoids a syscall per block, and `mprotect` on 16K pages shared by several blocks would require stopping whoever executes them.
   - Instruction cache coherence: `dc cvau` on the written lines (skipped if `CTR_EL0.IDC` = 1), `dsb ish`, `ic ivau` on the executable lines (skipped if `CTR_EL0.DIC` = 1), `dsb ish`, `isb`. ✅ On the Pi 5 `CTR_EL0` = `0x9444c004`: 64-byte lines, IDC 1, DIC 0, so only the instruction cache invalidation is needed.
   - Tests: generated code called from Rust (sum, loop with a backward jump, `cset` after a comparison, 64-bit constant) and a patch of already executed code, seen by the instruction cache.
3. **First translator** ✅ 2026-10-12 (`translator.rs`, `engine.rs`; details in the section "The first translator").
4. **Lockstep** ✅ 2026-10-12 (`lockstep.rs`, `--lockstep` option): instruction-by-instruction oracle against the interpreter (ADR-0021 §9.2), section "Lockstep". An option instead of the `ARMOR_LOCKSTEP` variable planned here: armor's environment variables reach the guest too.
5. **`difftest` with two engines** ✅ 2026-10-12, done together with step 3: each case runs under armor interpreted and with `--jit` (`testing.md`).
6. **Chaining** of direct jumps ✅ 2026-10-12 (section "Chaining"); indirect jumps through a table ✅ 2026-10-13 (section "Indirect jumps").
7. **Live flags** (backward pass per block): first in another form, as lazy flags ✅ 2026-10-13 (increments 9, 10, 12); then also in the form planned here, the per-block liveness analysis ✅ 2026-10-13 (increment 19, "Dead flags"). **Integer SSE** ✅ 2026-10-13 (increment 7). Scalar SSE floating point is translated since increment 17; x87 stays with the interpreter.
8. **Static registers** (ADR-0021 §4) ✅ 2026-10-13, for 12 registers out of 16 (increment 13).

## The first translator (increment 3, 2026-10-12)
Usage: `armor --jit <program>`. `execve` passes `--jit` on to children (field `Process::jit`); without the option armor interprets as before. *Since 2026-10-13 the JIT is the default engine (ADR-0028): `--interpreter` for the interpreter alone, and `execve` always passes the engine by name.*

**What it translates** (`translator::supported`), without `LOCK` and without the `67` prefix. Increment 3:
- `mov`, `movzx`/`movsx`/`movsxd`, `lea`;
- `add`/`or`/`adc`/`sbb`/`and`/`sub`/`xor`/`cmp`, `test`, `inc`/`dec`/`not`/`neg`, with register or memory destination;
- `push` (register, immediate, memory), `pop` of a register;
- `jmp`, `jcc`, `call`, `ret`, indirect `call`/`jmp`, `nop`.

Increment 6 (chosen by measuring what python and `c/malloc` left to the interpreter, `tech/performance.md`):
- `mov`, ALU (except `adc`/`sbb`), `test` and unary operations at **8 and 16 bits** too, AH–DH included;
- `movzx`/`movsx` from AH–DH;
- **shifts and rotations** `shl`/`shr`/`sar`/`rol`/`ror` of a 32/64-bit register, by immediate, by 1 and by `cl`;
- two- and three-operand **`imul`**;
- **`cmov`**, **`setcc`**;
- `cbw`/`cwde`/`cdqe`, `cwd`/`cdq`/`cqo`;
- **SSE moves:** `movaps`/`movups`/`movdqa`/`movdqu`, `movd`/`movq` to and from xmm.

Increment 7 (integer SSE2: everything that was left in `c/malloc`):
- with **NEON** (128 bits in `v0`–`v2`, loaded from and written back to the `Cpu` with `ldr`/`str q`):
  - per-lane operations `padd*`/`psub*`, `pcmpeq*`/`pcmpgt*`, `pminub`/`pmaxub`/`pminsw`/`pmaxsw`;
  - logic `pand`/`pandn`/`por`/`pxor` and `andps`…`xorps`;
  - `punpckl*`/`punpckh*` (`zip1`/`zip2`);
  - `packsswb`/`packssdw`/`packuswb` (`sqxtn`/`sqxtun` and the "2" forms);
  - per-lane shifts by immediate (`shl`/`ushr`/`sshr`; counts beyond the lane as on x86);
  - `pslldq`/`psrldq` (`ext` with a vector of zeros);
- with the **general-purpose registers**:
  - `pshufd`, `pshuflw`/`pshufhw`, `shufps`/`shufpd`;
  - `pmuludq`;
  - `pmovmskb`: for each half, `and` with 0x80…80, multiplication by 0x0002040810204081 and shift by 56. The sign bit of byte *i* ends up in bit 56 + *i*, and no partial product touches another one;
  - `pextrw`/`pinsrw`;
  - `movhps`/`movlps`, `movhlps`/`movlhps`, `movss`/`movsd`.

128-bit source operands in memory must be 16-aligned, as for the interpreter (#GP otherwise, from the slow path). The encoder's new NEON instructions are verified by `encoder-check` like the others (154 more samples, 0 differences; two mutations give 8 and 16 differences).

Increment 14 (chosen by counting the instructions left to the interpreter: in the python loop 64-bit `div` 57% and 16-bit shifts 32%; in the traceback 8/16-bit shifts 58%):
- **32- and 64-bit `div`/`idiv`:** a host division when the dividend fits in 64 bits. At 64 bits `rdx` must be zero (`div`) or equal to the sign of `rax` (`idiv`); at 32 bits `edx:eax` always fits in one register. Division by zero, `MIN / -1`, a quotient that does not fit the size and wider dividends go to the slow path: the interpreter raises #DE or computes the result. The remainder comes from `msub` (sign of the dividend, as on x86). The flags stay unchanged, as in the interpreter.
- **8- and 16-bit shifts and rotations**, on a register (by immediate, 1, `cl`) or memory (by immediate or 1; with `cl` a count of 0 must not write, so it goes to the interpreter). The value is kept extended in a 64-bit register, where the interpreter's formulas come out without special cases even for counts beyond the size.

Increment 15:
- 32/64-bit **`bsf`/`bsr`** with `rbit` + `clz` or `clz`. Only ZF changes; with a zero source the destination stays as it is, as in the interpreter.
- 32/64-bit **`bt`/`bts`/`btr`/`btc`**, with a register or immediate on a register, and with an immediate on memory; with a register on memory the address moves, so it goes to the interpreter. Only CF changes, inserted into `rflags` with `bfi`.
- 32/64-bit **`bswap`** with `rev`.

The three encoder instructions (`rbit`, `rev`, `clz`) are verified by `encoder-check`.

Increment 16 (chosen by counting the instructions left to the interpreter by python, coreutils, `c/malloc`, `c/threads` and perl: at the top `xchg`, `lock add`, `lock cmpxchg`, one-operand `mul`):
- **`LOCK` without atomics.** Only one guest thread executes code at a time (the GIL, ADR-0018), so, as long as no other process writes the same memory, an ordinary read-modify-write is exact. ADR-0021 §6 planned LSE atomics: this is an intermediate stage, to be revisited with parallelism. **Since 2026-10-13** (`MAP_SHARED`, ADR-0022, then the concurrent mode of ADR-0023): from the first writable shared mapping or the first extra thread, the JIT no longer translates `LOCK` or `xchg` with memory, and the interpreter executes them with a host compare-and-swap (`tech/memory.md`). Since step 4 of ADR-0023 (same day) the JIT translates them with LSE atomics (section "LSE atomics"). The barriers (`mfence`, `lfence`, `sfence`) are translated since 2026-10-13: nothing outside concurrent mode, `DMB ISH` inside it (✅ without the barrier the SB+mfence litmus test gives thousands of forbidden results, mutation 5 out of 5).
- **`xchg`** register-register and with memory (the write check on memory before anything else).
- **`cmpxchg`:** the flags are those of `cmp rax, dst` and stay lazy. If equal, dst = src; otherwise rax = dst, and a dst in memory keeps its bytes.
- **`xadd`:** flags of an addition; then src = dst and dst = sum, in the interpreter's order (it matters when src and dst are the same register).
- 32/64-bit **one-operand `mul`/`imul`**: `umulh`/`smulh` at 64 bits, 64-bit product at 32 bits; CF = OF = the high part is not the extension of the low one.

Lockstep found a mistake on the first test run: the high part of the product was in a temporary that the flag computation uses (`T3`), and `rdx` got a wrong value. Now registers are written before the flags.

Increment 17: **scalar SSE floating point**, with the host's floating-point instructions (`ldr`/`str` of `s`/`d` from the low lanes of the xmm registers in the `Cpu`, `fadd`/`fsub`/`fmul`/`fdiv`/`fsqrt`, `fcmp`, `fcsel`, `scvtf`, `fcvtzs`/`fcvtns`, `fcvt`; all new in the encoder and verified by `encoder-check`). The rules and the cases left to the interpreter are in `tech/floating-point.md`. Verification: special values in the random generator and the test `float_operations_on_special_values`. Nine mutations caught; two only after that test (`max` with `ge`: it shows only with +0 and −0). A tenth, the NaN check in `cvtss2sd`/`cvtsd2ss`, turned out to be equivalent on the values tried.

Everything else closes the block: the interpreter executes the instruction (`Cpu::step`), then execution resumes with the next block. For example: 8/16-bit `adc`/`sbb`, 32/64-bit shifts in memory, `rcl`/`rcr`, 8/16-bit `mul`/`div`, `bt*` with a register on memory, x87, strings, syscalls.

How the new forms are translated:
- **8 and 16 bits:** the guest registers are in the `Cpu`, so writing AL, AH or AX is a `strb`/`strh` at the right offset (AH: the second byte of rax), and the rest of the register stays as it is. For the flags of `add`/`sub`/`cmp`/`inc`/`dec`/`neg` the operands go to the top of a 32-bit register (`lsl #24` or `#16`), so the host's carry and overflow are the x86 ones for the narrow size.
- **Shifts:** CF is the last bit shifted out, obtained with a separate shift; OF follows `armor_cpu::alu::shift`; with a masked count of 0 nothing changes, not even the flags (with `cl`, a `cbz` skips everything).
- **32-bit `imul`:** 64-bit product of the sign-extended factors; CF = OF = the product does not fit in 32 bits. At 64 bits: `smulh` compared with the sign of the low result.
- **SSE:** 128-bit copies done with two 64-bit registers (`ldp`/`stp`), without NEON. For the aligned forms an address that is not a multiple of 16 goes to the slow path, and the interpreter raises #GP.

**Block.** At most 64 instructions; it ends at a jump, at the first untranslated instruction or at the limit. It is an `extern "C" fn(cpu, page_table, parity_table, context) -> state` function without calls or stack: it uses only caller-saved registers (`x0`–`x13`, list in "Chaining"). It returns with `Cpu::rip` set and a status: "continue", "interpret the instruction at rip", or "continue, and this exit can be linked" (since increment 5).

**Provisional choices of this increment**, compared with ADR-0021:
- **guest registers in memory:** each instruction loads the operands from `Cpu::regs` and writes the result back. The state is exact after each instruction, so the slow path does not have to write anything back. The static mapping onto host registers (ADR-0021 §4) is a later increment, measurable separately;
- **flags always computed:** each instruction that writes flags updates the status flags in `rflags`, kept in a register (`x12`) for the whole block, and across chained blocks (read from and written to the `Cpu` only on entry from the engine and on exit). ZF, SF, CF and OF come from the host's NZCV (CF inverted for subtractions), PF from a 256-byte table, AF from `(a ^ b ^ r) & 0x10`. Computing only the live flags is step 7;
- **`jcc`** reads the bits of `x3` (`tbz`/`tbnz`, one or two extra instructions for `be`, `l`, `le` and their opposites).

**Block cache** (`engine.rs`), one per guest thread:
- a `HashMap` by address, with a multiplicative hasher instead of SipHash;
- in front of it, a direct-mapped cache with 16,384 entries (`recent`), because the engine looks up the block at every interpreted instruction;
- an "interpret" entry remembers the addresses whose first instruction is not translated;
- a 64 MiB code buffer (reserved space, memory used as needed): when it is full everything is flushed and retranslated.

**Signals and GIL.** As for the interpreter: `Jit::run` executes about 10,000 guest instructions, then returns to `run_thread`, which delivers pending signals and releases the GIL. Since chaining (increment 5) the count is kept in the generated code, at the entry of each block: see "Chaining".

**Fork.** The buffer is a shared mapping (first a `memfd`, now anonymous), and `fork` is a host fork (ADR-0019): parent and child would write into the same buffer. The engine notices it with a fork counter, incremented in the child by a `pthread_atfork` handler (armor does `fork` with `libc::fork`), and in a child it starts again with a new buffer. Until 2026-10-13 it compared the pid on every `run`, but `getpid` is a real syscall and cost one extra host syscall for every guest syscall (`tech/performance.md`). ✅ Without the detection, or with the counter stuck (mutations), `c/processes` in its 3 variants and `busybox/sh-pipeline` with `--jit` end with SIGSEGV.

## Chaining (increment 5, 2026-10-12)
Before, each block returned to the engine, which looked up the next one in a table. Now a block jumps directly to the next one when the jump has a known address: `jmp`, both branches of `jcc`, `call`, and the end of a block that reached the length limit.
- **Linkable exit:** a `b` to an out-of-line stub. The stub sets `rip`, puts the address of that `b` in `Context::site` (with `adr`, added to the encoder and verified by `encoder-check`) and returns with `EXIT_CHAIN`.
- **Patch:** the engine looks up (or translates) the destination block and rewrites the `b` so that it jumps to its **chained entry**, 8 bytes after the start. There the prologue has already run, which loads `rflags` into `x12` and the budget into `x13`: between chained blocks those two registers stay live. The rewrite (`CodeBuffer::patch`) happens only if the buffer has not been flushed in the meantime (`epoch`).
- **Budget:** each block starts with `subs x13, x13, #instructions` and `b.mi`, and returns to the engine when the budget runs out. So a loop chained onto itself still lets signals and other threads through every ~10,000 instructions (ADR-0021 §6).
- **Invalidation:** blocks are never removed one by one. When the code generation changes, or the buffer is full, everything is flushed, rewritten jumps included.
- **Not chained:** exits to the interpreter return to the engine. Indirect jumps, since increment 8, go through a table (section "Indirect jumps").
- **Lockstep:** it uses single-instruction blocks, without chaining: every instruction must go through the comparison.

Block registers in this increment: `x0` = `Cpu`, `x1` = page table, `x2` = parity table, `x3` = `Context`; `x12` = `rflags`, `x13` = budget. The current layout is in "How it is made today".

**Verification:**
- `chained_loops_match_the_interpreter`: a 1,000-iteration loop with a `call`, against the interpreter;
- `chained_loops_stay_in_translated_code`: the same loop with a large budget must call blocks from the engine fewer than 1,010 times (the `ret`, indirect, remains). Without rewriting the jumps there are 3,002 calls, and the test fails;
- `chained_code_returns_when_the_budget_runs_out`: a loop chained onto itself, with budget 50, must return to the engine at least 100 times. Without the budget check it returns only once, and the test fails;
- mutation tests: chained entry without skipping the prologue, taken branch of `jcc` with the destination of the not-taken branch. Both caught;
- `difftest` 217/217 with both engines and in lockstep; 20 runs out of 20 identical for the four C tests with threads, signals and processes.

## Indirect jumps (increment 8, 2026-10-13)
`ret`, `jmp` and `call` via register or memory have their destination only at run time. Before, they always returned to the engine; now the generated code first looks in a **direct-mapped table** of the engine (`Jit::targets`, 4,096 pairs guest address → chained entry of the block, address in `Context::targets`):
```
eor  x10, x4, x4, lsr #12      // x4 = destination
and  x10, x10, #0xfff          // slot (translator::target_slot)
ldr  x11, [x3, #targets]
add  x11, x11, x10, lsl #4
ldp  x14, x15, [x11]           // (address, entry)
cmp  x14, x4 ; b.ne miss
br   x15                       // chained entry: the block checks the budget
miss: ... return to the engine
```
- The engine writes the pair every time it finds or translates a block, and flushes the table together with the buffer (`flush`). So the table never points to code from an earlier epoch.
- Lockstep: no lookup, as for chaining.

**Verification:**
- `chained_loops_stay_in_translated_code`: calls from the engine drop below 20, against about 1,000 before (one `ret` per iteration);
- `indirect_jumps_keep_flags_and_budget`: the flags of a `cmp` cross a `ret` in a 1,000-iteration loop, compared with the interpreter; with budget 50 the engine regains control at least 100 times;
- `writes_to_code_go_through_the_interpreter` checks, after the rewritten code, that every table entry points to the current translation (`Jit::check_targets`);
- mutation tests, all caught, but two only after adding the tests above: different hash in the generated code, jump to the start of the block instead of the chained entry, table not flushed with the buffer;
- `difftest` 217/217 with both engines and in lockstep.

## Lazy flags (increment 9, 2026-10-13)
The profile of the python loop (`tech/performance.md`) gave 32% of the time to flag computation. Before this increment every instruction that writes flags computed all six of them.

Now `add`/`sub`/`cmp`/`and`/`or`/`xor`/`test`/`neg` save only the operands (or the result, for logical operations) in `x19`/`x20` (`FA`, `FB`). The translator statically remembers the kind of the pending operation (`Translator::pending`), and the flags are actually computed (`materialize`) only:
- before an instruction that reads them or writes only part of them (`jcc`, `cmov`, `setcc`, `adc`/`sbb`, shifts and rotations, `imul`, `inc`/`dec`);
- before every exit (`exit`, before the linkable `b` of `exit_direct`, before the lookup of `exit_to`);
- in the slow path of each instruction, out of line, with the lazy state of that point (saved when the label is created).

`inc`/`dec` stay pending with the mask without CF, after making `FLAGS` current. A lazy operation followed by another one before any read costs nothing more than two `mov`s.

`x19`/`x20` are callee-saved: the prologue saves them on the stack (`stp … [sp, #-16]!`) and the epilogue restores them; `CHAINED_ENTRY` becomes 12 bytes.

**How much it can be worth.** Estimate before writing the code, on the same samples as the profile: only 24% of the time spent in instructions that write flags concerns instructions whose flags are overwritten in the same block before being read. Each block ends with a jump and materialises the last one. Measurement: python loop 3.17 → 3.00 s.

**Verification:** six mutation tests, all caught by the existing tests: lazy state not saved in the slow path, no materialisation before `exit_to`, before `exit_direct`, before `exit`, before the instructions that read the flags, `inc`/`dec` with CF in the mask. `difftest` 217/217 with both engines and in lockstep.

## Guest registers in host registers (increment 13, 2026-10-13)
Up to increment 12 each instruction loaded the guest registers from `Cpu::regs` and wrote them back. So every dependency between two instructions went through an `str` and an `ldr` from the same address, with a few cycles of latency. The profile attributed that wait to the instruction that used the value (often the permission check), not to the `ldr`/`str` (`tech/performance.md`).

Now 12 of the 16 registers live in host registers (`GUEST` in `translator.rs`), chosen by time-weighted use in the python loop:

| Guest | Host | | Guest | Host |
|---|---|---|---|---|
| rax, rcx, rdx, rbx | x22–x25 | | r12, r13 | x3, x9 |
| rsp, rbp, rsi, rdi | x26–x29 | | r14, r15 | x16, x17 |

r8–r11 (less than 4% of the uses) stay in the `Cpu`. To free host registers:
- the `Context` is passed in `x2` (blocks now have three arguments: `cpu`, page table, `Context`);
- the base of the flag table is in `Context::flag_table`;
- the exit status goes into `Context::status`, and the epilogue puts it back into `x0`;
- `T5` is the same register as `EXIT_RIP` (`x8`), set only on exit.

**Narrow reads and writes:**
- from a host register: `mov` (64 bits), `mov w` (32 bits), `ubfx`/`sbfx` (8 and 16 bits, AH–DH from bit 8);
- into a host register: `mov`, `mov w` (32 bits: clears the high part as on x86), `bfi` (8 and 16 bits: the rest stays). The encoder now has the inserting `bfm`, verified by `encoder-check`.

**Boundaries.** The prologue saves `x19`–`x30` on the stack and loads the mapped registers from the `Cpu` with six `ldp`s; the epilogue writes them back with six `stp`s. Between chained blocks, and through the indirect jump table, they stay in registers. Every exit to the engine (slow paths, interpreter, lockstep) goes through the epilogue, so `Cpu::regs` is exact when someone else reads it. `CHAINED_ENTRY` becomes 60 bytes.

**Verification:**
- six mutations caught: registers loaded from the wrong offsets, a pair not written back, AH at bit 0, 32-bit write without clearing, flag routine that clobbers rax, signed narrow read done unsigned. A seventh, the order of the pairs, was equivalent: a construction mistake of mine;
- `difftest` 217/217 with both engines and in lockstep. The full lockstep run goes from 83 s to 1 min 57 s, because every instruction goes through prologue and epilogue; the slowest case (python traceback) takes 19 s, below the 120 s per case.

## Lazy flags across blocks (increment 12, 2026-10-13)
After increment 9 a new profile still gave about 30% to the flags. The reason: every block ends with a jump, and the final `jcc` read them. Two changes:
1. **`jcc` from the operands.** If the pending operation is known at translation time, `jcc` redoes it on `FA`/`FB` (1–3 instructions, for the host's NZCV) and jumps with the equivalent AArch64 condition (`host_condition`).
   - Subtractions: CF = inverted carry, so `jb` → `b.lo`, `jbe` → `b.ls` and so on.
   - Additions and logical operations: CF = carry; after an addition `jbe`/`ja` have no equivalent.
   - Never for parity, nor for CF after `inc`/`dec` (which keep an older CF).
   - Without an equivalent, the flags are computed as before.
2. **Lazy flags across blocks.** On exit the block does not compute the flags: it writes into `x21` (`KIND`) a code for the pending operation (kind, size, whether CF is excluded; 0 = `FLAGS` current) and leaves the operands in `x19`/`x20`. The next block starts in the "dynamic" state. If it needs the flags before having rewritten them it calls (`blr`) a shared routine (`flags_routine`), which jumps to the right code with a jump table indexed by `KIND`, computes them and clears `KIND`. The epilogue towards the engine does the same, so `Cpu::rflags` stays exact for the interpreter, signals and lockstep.
   - The routine sits at the start of the code buffer and is republished after every flush; its address is in `Context::flags_routine`.
   - The prologue also saves `x21` and `x30` (the call uses the link register) and sets `KIND` to 0. `CHAINED_ENTRY` becomes 20 bytes.

**First version wrong in performance.** The routine looked up the code with up to 28 comparisons in a row: measurement unchanged (median 2.90 → 3.15 s, within the noise). With the jump table: 2.88 → 2.58 s and 3.01 → 2.76 s in two series of 7 alternating runs.

**Verification:**
- new test `conditions_after_every_lazy_flag_operation`: 9 operations × 4 sizes × 16 conditions × 3 forms, with boundary values. The forms are: `jcc` right after; `jcc` in the next block; `setcc` and `adc` in the next block;
- nine mutations caught: wrong conditions for subtractions and logical operations, pending code lost on exit, epilogue without computation, routine without the `inc`/`dec` variants, block start considered "current", CF of `inc`/`dec` used, jump table index scaled wrongly. A tenth mutation, swapping the size codes, is equivalent: it is an internal label used consistently;
- `difftest` 217/217 with both engines and in lockstep; 20 out of 20 for the C tests with threads, signals and processes.

## Flag table (increment 10, 2026-10-13)
When the flags had to be computed, they used to cost about 17 instructions: a `cset` for CF, ZF, SF and OF, each merged with an `orr`, plus the parity table. Now:
```
mrs  x10, nzcv
lsr  x10, x10, #20            // NZCV in bits 11..8
and  x14, x6, #0xff           // low byte of the result
orr  x10, x10, x14
orr  x10, x10, #0x1000        // only for subtractions: CF = !C
ldrh w10, [x2, x10, lsl #1]   // CF, PF, ZF, SF, OF in the x86 positions
```
Then AF (`(a ^ b ^ r) & 0x10`, 4 instructions, not for logical operations) and the merge into `rflags`. `FLAG_TABLE` has 8,192 16-bit entries (16 KB, built at compile time) and takes the place of the parity table as the blocks' third argument. Shifts and `imul` use it too (`result_flags`): after a test `ands` C and V are zero, so the table adds only ZF, SF and PF.

**Verification:** four mutations caught (subtraction bit, position of NZCV in the index, `inc`/`dec` mask not applied, parity on 7 bits). `difftest` 217/217 with both engines and in lockstep.

## Lockstep (increment 4, 2026-10-12)
`armor --lockstep <program>` (or `cargo xtask run --lockstep`, `cargo xtask difftest --lockstep`) is `--jit` with single-instruction blocks, each one checked against the interpreter:
1. registers, `rip`, `rflags`, (since increment 20) MXCSR and (since 2026-10-14, with MMX translated) the x87 state are saved;
2. the interpreter executes the instruction through a memory that records old and new bytes of every write;
3. writes and state are undone;
4. the translation of the instruction runs;
5. registers, `rip`, `rflags`, MXCSR, the x87 and MMX state and the bytes the interpreter had written are compared. If the translation took the slow path, the state must be identical to the saved one, except for MXCSR flags that the interpreter sets anyway (increment 20); then the interpreter executes the instruction for real, and that is how a fault reaches the guest.

At the first difference armor stops (exit 125) with a report: address, bytes and decoding of the instruction, registers before/interpreter/translation, differing status bits. A real example, with the CF of subtractions deliberately not inverted:
```
armor lockstep: the translation of the instruction at 0x403c86 disagrees with the interpreter: different state
  bytes [48, 83, ec, 18]
  decoded Ok(Instruction { op: Alu { op: Sub, dst: Reg(Gpr(4)), src: Imm(24) }, size: B64, len: 4, lock: false })
     rsp: before 0x00007ff729392b58  interpreter 0x00007ff729392b40  translation 0x00007ff729392b40
  rip:    interpreter 0x403c8a  translation 0x403c8a
  rflags: before 0x246  interpreter 0x202  translation 0x203 (status bits differing: 0x1)
```
With a `push` that writes the wrong register the report says "memory at 0x7ff6c0952b90: interpreter wrote [98, 2b, …], translation left [00, …]".

**Limits:**
- a translated write to an address the interpreter did not write shows up only if it changes bytes the interpreter wrote;
- state the translator does not touch (segment bases) is not compared; xmm registers are, since increment 7, MXCSR since increment 20, the x87 state (registers, TOP, empty registers) since 2026-10-14;
- it does not check the passage between instructions inside a long block: the blocks here have a single instruction. That part is verified by `difftest --jit` and the differential tests.
- **memory that another writer changes between the two runs** (since 2026-10-15): the kernel writes the owner's thread id into a priority-inheritance futex word at any moment, even with the guest's threads running one at a time. Before, `c32-pie/pi_mutex` and `c32-dyn/pi_mutex` failed in lockstep about once in two runs: the two runs of a `mov` read different words ("different state"), or the undo of the interpreter's write erased the kernel's, and glibc stopped on `assertion failed: robust || (oldval & FUTEX_OWNER_DIED) == 0`. Now the recorder also keeps what the interpreter read; if a byte the interpreter wrote no longer holds its value before the undo, the interpreter's run stands, and if a byte it read changed before the translation runs, the instruction runs once more, interpreted; neither is compared. A change while the translation itself runs is still reported. ✅ `pi_mutex` in its six variants 36 times out of 36 in lockstep (before the change: failures in 4 batches out of 5, also without this day's other changes); full `difftest --lockstep` 461/461; a mutation (sign extension from memory translated as zero extension) is still caught by `mov/extend`, `c/strings`, `c/printf_int`, `busybox/sort` and `coreutils/sort`.

**Verification:** `difftest --lockstep` 217/217 (1 min 23 s); the differential tests of `armor-jit` also run every random program in lockstep.

**Panic in a thread:** since this increment every armor panic, in any guest thread, terminates armor with 125 (`conventions.md`). Before, a panic in a secondary thread stopped only that thread.

## Guest memory access
Sequence emitted for each access since increment 18 (`Translator::check`), with the address in `x7` (`x10` temporary, `x1` the table):
```
tst   x7, #0xffff800000000000      // beyond 2^47: outside the table
b.ne  slow
lsr   x10, x7, #12
ldrb  w10, [x1, x10]               // flat table
tbz   w10, #5, edge                // multi-byte read: this page and the next readable
tbz   w10, #6, edge                // multi-byte write: this and the next writable, not executable
tbz   w10, #3, slow                // single-byte access: only the page's bit (#4 for writes)
back:
ldr/str ..., [x7]                  // identity mapping

edge:                              // out of line, after the block
tbz   w10, #3, slow                // the page itself (#4 for writes)
and   x10, x7, #0xfff
cmp   x10, #(4096 - size)          // does the access cross two pages?
b.hi  slow
b     back
```
For each access only one of the three `tbz`s is emitted. **Increment 18 (2026-10-13):** the table has two more bits, `PAGE_READABLE_SPAN_BIT` (5) and `PAGE_WRITABLE_DATA_SPAN_BIT` (6) in `armor-mem`. They hold when the corresponding bit (3 or 4) holds both for the page and for the next one. `PageTable::set` recomputes them for the changed page and for the one before. With the "next page too" bit an access of up to 16 bytes that starts in the page is allowed anyway, whether or not it crosses the boundary, so the crossing test is needed only when the bit is missing: at the last page of a mapping or before a page with different permissions. The fast path goes from 7 instructions to 5 for multi-byte accesses. Measurement in `tech/performance.md`.

Verification of increment 18: new test `span_bits_follow_both_pages` (bits that follow both pages, also when unmapping or changing the permissions of the page after or the page before); five mutations caught: fast `tbz` on the bit of the page alone, out-of-line piece without the crossing test, out-of-line piece without the page check, previous page not recomputed, "next page too" bit with one or the other page instead of both. `difftest` 235/235 with both engines and in lockstep.

Since increment 11 (2026-10-13): the page table has two bits derived from the permissions (`PAGE_READABLE_BIT`, `PAGE_WRITABLE_DATA_BIT` in `armor-mem`), and every permission check is a single `tbz`. For single-byte accesses the page-crossing test (`and`, `cmp`, `b.hi`) is not emitted: it cannot fire. From 9 instructions to 7 for reads and writes, 4 for bytes. Verification: tests of the bits for all 8 permission combinations, four mutations caught ("data" bit that ignores X, "readable" only with R, write checked with the read bit, crossing test removed beyond 1 byte).

The **slow path** is out of line, one per instruction: it sets `rip` to the instruction and exits with "interpret". Since registers and flags are already exact, the interpreter re-executes the instruction from scratch. It performs the access if it is allowed (crossed pages, write to code) or produces the fault with the exact rip, which becomes a synchronous signal as today (ADR-0017).

`push`, `call` and `pop` check the stack slot before changing `rsp`.

## Dead flags (increment 19, 2026-10-13)
**Problem**, seen in the code generated for the hot loop of `bench threads` (`xor`, `add`, `imul`, `mov`, `shr`, `xor`, `cmp`, `jne`, disassembled with the `dump_translation` tool, `tech/performance.md`): about 83 AArch64 instructions for 8 x86 instructions. Almost half of them computed flags that nobody reads:
- before `imul` the pending flags of `add` were materialised (14 instructions), even though `imul` rewrites them all;
- `imul` and `shr` computed all the flags right away (13 and 17 instructions, each with an `mrs nzcv` and a table access), then rewritten by `xor` and `cmp`.

In the profile, the two hottest spots (17% and 15% of the samples) followed the reads of the flag table. After: about 35 instructions per iteration, no `mrs`.

**Analysis** (`live_flags`): a backward pass over the block computes, after each instruction, the status flags whose value can be observed. At the end of the block all are live (the exits, the next block, the engine). Before an instruction:
- all live if the instruction reads flags (`jcc`, `setcc`, `cmov`, `adc`/`sbb` and, out of caution, every instruction that touches the flags without an exact description) or if it can leave the block midway (`stays_in_block` false: memory accesses, `div`, floating point, jumps), because the interpreter wants the exact state;
- otherwise the ones live after, minus those the instruction always writes (`flags_written`).

`flags_written` is a safe underestimate: it says 0 for the instructions it cannot describe (rotations, narrow shifts or shifts with the count from `cl`, which can be 0, `bsf`/`bt*`), and their flags then stay live. In debug builds every translated instruction checks that it really wrote the flags that `flags_written` attributes to it (`flags_done`).

**Use:**
- an instruction whose written flags are all dead and that reads none does not compute them (`skip_flags`: no `defer`, no computation in `imul`, `mul`, shifts), and leaves the lazy state as it was. It is correct because every flag it would have written is rewritten before any read, exit or slow path, so the older state that stays in its place is never seen;
- an instruction that writes all the status flags from the operands alone, without reading any, does not first materialise the pending ones; afterwards, `FLAGS` is current.

In lockstep blocks have a single instruction, so all flags are live at the end and nothing is skipped: lockstep keeps comparing all the flags of every instruction.

**Verification:**
- `dead_flags_are_not_computed`: no `mrs nzcv` in the loop of `bench threads`, and the loop gives the same registers and flags as the interpreter; without the optimisation it fails;
- the JIT's random tests compare the final flags and the state at the slow paths;
- eight mutations caught: slow paths that do not require the flags, `jcc`/`setcc`/`cmov` that do not read them, `adc` that does not read CF, `inc`/`dec` that declare they write CF, nothing live at the end of the block, materialisation skipped even before instructions that write only some flags, shift with the count from `cl` declared as a writer, `FLAGS` not marked current after a complete writer;
- `difftest` with both engines and in lockstep (numbers in `COMPATIBILITY_MATRIX.md` and `testing.md`).

**Measurements** (`tech/performance.md`): `integer` 398 → 318 ms (qemu 333: armor now faster), `threads` 428 → 253 ms (qemu 291), `python-int` 1,867 → 1,739 ms, `python-float` 1,493 → 1,408 ms; the others unchanged.

**Remaining**, in the same loop: copies between registers (`mov x4, x22` before every operation, because the operands go through the temporaries `A`/`B`), and `cmp` followed by `jcc` that redoes the subtraction from the saved operands.

## MXCSR flags (increment 20, ADR-0024, 2026-10-13)
Translated code computes scalar SSE floating point with the host's instructions, which accumulate their flags in FPSR as x86 accumulates them in MXCSR. Correspondence: IOC, DZC, OFC, UFC, IXC are IE, ZE, OE, UE, PE; DE has no counterpart (IDC is set only with FPCR.FZ, which is off).

**How they reach MXCSR:** the engine clears FPSR before each run of translated code and, on return, adds the FPSR flags to `Cpu::mxcsr` (`execute`). Every return to the engine goes through there, so the interpreter, `stmxcsr`, `fxsave`, signal frames and new threads always see the complete MXCSR. The Rust code between two runs dirties nothing: FPSR is cleared again on entry.

**Where the host differs from x86** (✅ measured on the Cortex-A76 and on the Xeon X5675):
- **DE:** a denormal operand sends the instruction to the interpreter before the operation. The check reads the operand's bits with an integer `ldr` from the `Cpu` for xmm registers (cheaper than `fmov`), with `fmov` from the already loaded register for guest memory (rereading it could give another value if another thread writes it); if the exponent is 0 it goes out of line to look at the mantissa. Conversions to integer have no check: x86 gives no DE there;
- **UE:** x86 detects tininess after rounding, the host before. On a result that rounds to ± the smallest normal the host sets UFC (with IXC) and x86 only PE. It happens only after `mul`, `div` and `cvtsd2ss` (a small sum is exact, `sqrt` does not go below). After these, a result with magnitude ≤ the smallest normal (zeros included) goes out of line: there `mrs fpsr`; if UFC is set it is cleared, and it becomes UE in `Cpu::mxcsr` only if the result is not ± the smallest normal. So translated code never leaves UFC set in FPSR, and a UFC found there was set by the operation just performed;
- **signalling comparisons:** `comis`, `min`/`max` and the ordered predicates of `cmp` (`lt`, `le`, `nlt`, `nle`) use FCMPE, which gives IOC for quiet NaNs too, like x86 IE; `ucomis` and the other predicates use FCMP.

**MXCSR control** (increment 6 of M4): translated code assumes round-to-nearest, no FZ or DAZ and masked exceptions. With another control setting SSE floating point is not translated (`uses_mxcsr_control`) and the interpreter executes it; when the control enters or leaves the default the engine flushes the block cache (`Jit::sync_mxcsr`). Translated code never changes the control: only `ldmxcsr`/`fxrstor` (interpreted), signal delivery and `rt_sigreturn` change it, so it is enough to check after each interpreted instruction and on every `run`. Mutation caught: without the check `sse/mxcsr_control` and `c/sse_exceptions` fail with `--jit`.

**What does not need correcting:** a NaN result goes to the interpreter with IOC already set by the host exactly when x86 gives IE (SNaN or invalid operation); the interpreter sets it again, and the flags are cumulative. An out-of-range conversion gives only IOC on the host (✅ measured with 32- and 64-bit `fcvtzs`/`fcvtns`, 2026-10-13: no IXC, contrary to what was written earlier in ADR-0024) and only IE on x86.

**Discarded:** saving FPSR before each `mul`/`div` and restoring it before the slow path. It works, but `mrs fpsr` waits for the operations in flight: `bench float` went from 223 to 374 ms. With the invariant on UFC the read happens only out of line.

**Verification:**
- `sse/mxcsr_flags` (in `difftest`) and `sse/mxcsr_hardware` (only with `--oracle`, `testing.md`): 106 cases identical to the Xeon X5675 with the interpreter, with the JIT and in lockstep;
- `float_operations_on_special_values` also compares MXCSR, with values that round to the smallest normal and out-of-range conversions;
- lockstep compares MXCSR;
- five mutations caught both by the guest tests against the hardware and by the unit test: UE also for the smallest normal, UE lost, comparison `<` instead of `≤`, denormal check removed, FCMP instead of FCMPE.

**Measurements** (`tech/performance.md`): `bench float` 223 → 267 ms (qemu 718), `python-float` unchanged.

## Memory ordering in concurrent mode (ADR-0023, steps 2 and 4)
When the process is in concurrent mode (`MemView::is_concurrent`, ADR-0023), the engine translates with `Options::concurrent`. Guest memory accesses then go through `guest_load` and `guest_store`:
- **loads:** `LDAPR` (load-acquire RCpc, `lrcpc`), plus a separate sign extension for signed loads (`LDAPRS` would require FEAT_LRCPC2, which is absent);
- **stores:** `STLR` (store-release). That `LDAPR` plus `STLR` give TSO (a load can pass an earlier store, as on x86, and nothing else) is ⚠️ a reasoning on ARM's memory model (RCpc in the ARM ARM), not a verification: what is verified ✅ is the result of the litmus tests (0 forbidden results, while without ordering they appear);
- **misaligned addresses:** before the access, 1 to 3 `tbnz`s on the low bits of `ADDR`, which do not touch the flags and use no registers. If the address is misaligned, an out-of-line piece does the ordinary access with a barrier: `LDR` then `DMB ISHLD`, or `DMB ISH` then `STR`. Without FEAT_LSE2 misaligned `LDAPR`/`STLR` give SIGBUS (✅ `environment.md`);
- **SSE and floating point in memory:** there are no acquire/release forms, so `DMB ISH` before the instruction and `DMB ISHLD` after it;
- **`LOCK` and `xchg` with memory:** since step 4 (2026-10-13) LSE atomics, below; before, they all went to the interpreter. x86 barriers become `DMB ISH`.

Outside concurrent mode the generated code is identical to before.

Verification:
- `random_code_matches_the_interpreter_in_concurrent_mode`: 1,500 random programs, with often misaligned accesses, against the interpreter;
- `concurrent_mode_emits_ordered_accesses`: `LDAPR`, `STLR` and the barriers are there, and without concurrent mode they are not;
- four mutations caught:
  - alignment test removed, and only bit 0 checked: SIGBUS;
  - missing sign extension;
  - `STLR` replaced by `STR`.

Cost measured with the GIL still active: `c/threads` about 44 ms as in step 1, `bench threads` from 1.00 to about 1.03 s. Since step 3 (GIL removed): `c/threads` 37 ms, `bench threads` 0.55 s (`tech/performance.md`).

### LSE atomics (step 4, 2026-10-13)
In concurrent mode (`Options::concurrent`) the x86 atomic instructions (`needs_atomic`: `LOCK` and `xchg` with memory) that have an equivalent LSE atomic are translated with it (`Translator::atomic`); the others stay with the interpreter (`supported_concurrent`). Outside concurrent mode the generated code does not change.

| x86 on memory | AArch64 |
|---|---|
| `lock add` | `LDADDAL` |
| `lock sub`, `lock dec` | `LDADDAL` with the negated operand |
| `lock inc` | `LDADDAL` with 1 |
| `lock and` | `LDCLRAL` with the negated operand |
| `lock or` | `LDSETAL` |
| `lock xor`, `lock not` | `LDEORAL` (all ones for `not`) |
| `xchg` | `SWPAL` |
| `lock xadd` | `LDADDAL`, the old value in the register |
| `lock cmpxchg` | `CASAL` with `rax`; `rax` rewritten only if different |

- **Order of the steps:** address, **write** permission check (even a failing `cmpxchg` writes, on x86; a write to code goes to the interpreter, which invalidates), test of the low bits with `tbnz` towards the slow path, then the atomic, which leaves the old value in `A`. Result and lazy flags come as in the ordinary translation, from `A` and the operand.
- **Misaligned** (split lock): LSE atomics without FEAT_LSE2 give SIGBUS, so the instruction goes to the interpreter before writing anything. There it stays a read plus a write, not atomic, as before (ADR-0023).
- **Ordering:** the `AL` forms have acquire and release, and they are the same ones Linux uses for arm64's "fully ordered" atomics ⚠️ (remembered, `arch/arm64/include/asm/atomic_lse.h`). That they act as a full barrier like an x86 `LOCK` between `STLR` and `LDAPR` is ✅ verified by the SB+lock litmus test (below), not proven.
- **Interpreter and JIT together:** the interpreter uses a host `SeqCst` compare-and-swap on the same word, so an instruction executed by the interpreter and a translated one, in two threads, stay atomic with respect to each other.
- **Encoder:** `Assembler::atomic` and `Assembler::casal`, 72 new samples in `encoder-check` (6 instructions × 4 sizes × 3 register triples), 0 differences; a wrong opcode for `LDCLRAL` gives 12 differences.

Verification:
- `concurrent_mode_emits_lse_atomics`: `lock add`, `lock xadd`, `xchg` and `lock cmpxchg` become `LDADDAL`, `SWPAL`, `CASAL` only in concurrent mode; `lock adc` stays with the interpreter;
- the random generator now produces `lock` add/or/adc/sbb/and/sub/xor (register and immediate), inc/dec/not/neg at 8, 16, 32 and 64 bits, with often aligned operands: `random_code_matches_the_interpreter_in_concurrent_mode` compares them with the interpreter;
- eight mutations caught: subtraction and `dec` without negation, `and` with `LDSETAL`, CF touched by `inc`/`dec`, old value of `xchg` lost, `xadd` register not written, `rax` rewritten by a successful `cmpxchg`, alignment test removed (SIGBUS);
- `c/litmus` has a third test, **SB+lock**, with `lock addl $0, (%rsp)` as the barrier: 0 forbidden results under qemu and with both engines. With `LDADD` without acquire/release (mutation) forbidden results appear in 5 runs out of 5 (from 20 to 305);
- with the atomics replaced by ordinary read-modify-writes (mutation) `c/shared_atomics` hangs and `c/threads` fails;
- `c/threads`, `c/threads_signals`, `c/shared_atomics`, `c/litmus`, `c/processes`, `c-dyn/threads`: 50 runs out of 50 identical to qemu for each engine;
- `difftest` 255/255 with both engines and in lockstep.

Measurements (`tech/performance.md`): `bench threads` 546 → 428 ms (qemu 290 ms); `integer`, `syscall`, `python-int` and `start-dynamic` unchanged.

## Self-modifying code
Increment 3: writes to executable pages always go to the slow path. The interpreter writes and advances the "code generation" (ADR-0020); the engine compares it before each block and, if it has changed, throws away all the blocks.

Still to do: the per-page invalidation of ADR-0021 §7: a "page with translated code" bit in the table, and the invalidation of only the blocks of that page.

## Verification of increment 3
- **JIT-versus-interpreter differential tests** (`crates/armor-jit/src/tests.rs`): same code and same initial state, comparison of registers, `rip`, `rflags`, memory and error.
  - 3,000 random programs of 1 to 24 instructions: register, memory and immediate ALU, unary operations, `push`/`pop`, `lea` with an index, extensions, `jcc`, `call`/`ret`, indirect jumps, plus 8- and 16-bit forms that go to the interpreter. Initial values chosen for the edge cases of carry, overflow and sign; accesses also straddling a page, beyond the mapping and on a `PROT_NONE` page;
  - each of the 16 conditions in each of the 64 flag combinations;
  - a fault in the middle of a block;
  - code that rewrites itself after being translated.
  - Not generated by the random programs, covered only by `difftest`: absolute addresses without a base, FS/GS segments (every program's TLS), `ret imm16`.
- **Mutation tests**, all caught by the tests:
  - CF without inversion for subtractions;
  - 32-bit `lea` without clearing the high half;
  - `push` without a check;
  - `jle`/`jg` swapped;
  - wrong page-crossing check;
  - write to executable pages without the slow path;
  - read of `PROT_NONE` pages allowed;
  - `inc`/`dec` that touch CF.
- **`difftest` with `--jit`:** 217/217. Without the generation check in the engine `control/self_modifying`, `faults/exec_unmapped` and `faults/exec_after_mprotect` fail.

## 32-bit code (ADR-0029, since 2026-10-14)
The JIT translates i386 guests too. The engine decodes in the `Cpu`'s mode (`Jit::mode`, taken from the `Cpu` on every `run`; a change flushes the translations) and tells the translator (`Options::compat`). The rest comes from the instruction itself:
- **stack and jumps:** `insn.size` is the stack width (32 bits only in compatibility mode), so `push`, `pop`, `call`, `ret`, indirect `call`/`jmp` load and write 4 bytes with 32-bit arithmetic on `esp` (`W` subtractions), and direct targets are masked to 32 bits (`target`);
- **addresses:** `Mem::addr32` is translated in both modes: the offset is truncated to 32 bits (the semantics of the 67 prefix in 64-bit mode, which used to go to the interpreter), and in compatibility mode the sum with the `fs`/`gs` base too (`zero_extend_32`);
- **left to the interpreter:** `int 0x80`, `loop`/`jecxz`, string instructions, `mov` to and from segment registers, x87, the MMX forms not listed below.
- **Verification:** `random_code_matches_the_interpreter_in_compatibility_mode` (3,000 random programs read as 32-bit code, JIT, lockstep and interpreter identical: it found an interpreter panic on `cvtpi2ps xmm, m64`, fixed); all the i386 cases of `difftest` with `--jit` and `--lockstep`. ✅ `c32/malloc`: 256 ms interpreted, 18 ms with the JIT (2026-10-14).

## MMX and SSSE3 (for the i386 Steam client, 2026-10-14)
**Why.** Under armor the Steam client update failed: downloads of the large packages timed out (the client has timeouts of about 60 seconds on requests and of a few minutes on packages) because Steam's cryptography ran in the interpreter. `ARMOR_JIT_STATS=interpreted` (`tech/performance.md`) showed what: first MMX (`movd`/`movq` to and from mm, `pmuludq`, shifts, logical operations: big-number arithmetic), then, once MMX was translated, `pshufb` on xmm (200 million instructions in 100 s), then `palignr`.

**MMX.** An mm register is the mantissa of an x87 register (`Cpu::x87`); `armor_cpu::mmx_layout` gives the offsets in the `Cpu` (mantissa, exponent and sign of each register, TOP, empty-register bits, the two scratch registers), computed with `offset_of!` inside `armor-cpu`. The translator does what the interpreter does (`sse.rs`):
- **moves** (`movd`/`movq` with general-purpose registers, memory, mm; `movntq`; `movq2dq`; `movdq2q`) and `pinsrw mm`: directly;
- **lane operations** whose result in the low 64 bits, on zero-extended operands, is the MMX one (the lanes translated for xmm, logical operations, `pmuludq`, `punpckl*`, `pshufw`, shifts by immediate): the mm operands (or 8 bytes of memory) go into the two 128-bit scratch registers (`Reg::Mm(8)`, `Reg::Mm(9)`, which `xmm()` sees as xmm 16 and 17), the existing SSE translation runs, and the low 64 bits go back to the destination;
- **effects on the x87 state:** writing mm`n` sets the sign and exponent of its register to ones; every mm operand sets TOP to 0 and all registers valid; `emms` empties them. The scratch registers go back to zero after each instruction;
- **left to the interpreter:** conversions (`cvtpi2ps` & co.), packs, high unpacks, the SSE2 operations not translated for xmm, `pshufb`/`palignr` on mm, `pextrw`/`pmovmskb` from mm.

**SSSE3.** `pshufb`: `TBL` with the index `src & 0x8f` (an index with bit 7 becomes ≥ 128, and TBL gives 0 as on x86). `palignr`: `EXT` of src and dst (count < 16), of dst and zeros (16–31), zero (≥ 32). New in the encoder: `TBL` and single-byte `MOVI`, verified by `encoder-check` (12,554 instructions, 0 differences).

**Verification:** the random program generator has an MMX case (all the forms above, plus packs, high unpacks and `emms`) and a `pshufb`/`palignr` one; the comparisons also check the x87 state (`Cpu::fpu_state`), and so does lockstep. Mutations caught: wrong exponent in the mm registers, `pshufb` mask without bit 7, `EXT` operands swapped. `mmx/ops`, `i386/mmx/bignum`, `sse/ssse3` identical with `--jit` and `--lockstep`.

**Measured effect** (Steam update, same network, 2026-10-14): before, the 503 MB downloaded at about 0.5 MB/s and the large packages failed; after, downloaded in about 85 s even with the statistics enabled.

## Every ARMv8.0 (ADR-0032, M9, 2026-10-14)
In concurrent mode the JIT used two extensions later than ARMv8.0: FEAT_LRCPC (`LDAPR`, ARMv8.3) for TSO-ordered loads and FEAT_LSE (`LDADDAL` & co., `SWPAL`, `CASAL`, ARMv8.1) for atomic instructions. The Pi 5's Cortex-A76 has both; the Cortex-A53 and A72 (Raspberry Pi 3 and 4, many cheap SoCs) have neither.
- **Detection:** `HostFeatures::detect()` reads `AT_HWCAP` (`HWCAP_ATOMICS`, `HWCAP_LRCPC`) when the engine is created; the translator receives them in `Options::features`.
- **Without FEAT_LRCPC:** `LDAR` instead of `LDAPR`. It is stronger (RCsc: ordered also after earlier `STLR`s), so TSO ordering stays guaranteed; it costs more.
- **Without FEAT_LSE:** atomics become exclusive loops `LDAXR` / operation / `STLXR` / `CBNZ`, with acquire and release like the LSE instructions; `cmpxchg` compares after `LDAXR` and, if the values differ, releases the monitor (`CLREX`) and puts a `DMB ISH`, because x86's `cmpxchg` with LOCK is a full barrier even when it fails. They use the temporaries `T1` and `T4`.
- **New in the encoder:** `LDAR`, `LDAXR`, `STLXR`, `CLREX`, verified by `encoder-check` (12,579 instructions, 0 differences).
- **`lock cmpxchg16b`** (in `armor-mem`, inline assembly): `CASPAL` with FEAT_LSE, otherwise an `LDAXP`/`STLXP` loop that on a failed comparison writes back the value read (x86 writes in both cases, and only a successful `STLXP` makes the 16-byte read atomic); chosen with `AT_HWCAP` and the same `ARMOR_HOST_FEATURES`. Unit test `compare_exchange_16_both_ways` on both versions.
- **Verification:** `ARMOR_HOST_FEATURES=baseline` pretends the CPU lacks both extensions, so the ARMv8.0 paths run on the Pi 5 too. `random_code_matches_the_interpreter_on_armv8_0` (1,500 random programs in concurrent mode, JIT and lockstep against the interpreter); mutations caught on the atomic addition and on the `cmpxchg` comparison; with the variable, `threads`, `threads_signals`, `shared_atomics`, `litmus`, `processes`, `shared_memory` (x86-64 and i386) identical to qemu with `--jit` and `--lockstep`, and `shared_atomics` and `litmus` identical in 20 runs out of 20. ⚠️ A real CPU without the extensions (a Pi 4) is missing: until there is one, the variable is the proof.

## Side exits and operands in place (M10, 2026-10-15)
Found with a profile of 7-Zip (`tech/performance.md`, "7-Zip"):
- **Side exits.** `ends_block` no longer stops at `jcc`. Inside a block, `jcc` emits only the conditional branch (`branch_if`, with the host condition when the flags are pending) to a label whose code is out of line, after the body: the lazy-flags state of the jump (`side_exits`), then `exit_direct` to the target, chainable like any exit. The fall-through continues in the block with its registers and pending flags, instead of starting a block whose flags are `Lazy::Dynamic` (a `jb` after a `je` called the generic flag routine). Liveness (`live_flags`) needs no change: `jcc` already counts as reading every flag. A `jcc` that is the block's last instruction keeps both exits. The budget is subtracted at the block's start for every instruction, so a side exit counts instructions that did not run: the engine only gets control back a little earlier. Lockstep blocks are one instruction, so side exits are checked by the random tests (`jcc` over an `inc`) and by `difftest` with the JIT.
- **Operands in place** (`operand_in`, `destination_in`, `whole_register`): a guest register mapped whole, read at 32 or 64 bits, is used directly by ALU operations, `test`, `mov` and as the destination of `movzx`/`movsx`, without a copy into `A`/`B`. A 32-bit operand may have other bits above in its host register, so the lazy flags save 32-bit operands with `mov w` (zero-extending), and a `mov` into a register kept in the `Cpu` still goes through `read_operand`, which zero-extends (the random tests found both).
- **r10 and r11** in `x18` and `x30` (table above).

## Dead and deferred flags everywhere (M11, 2026-10-20)
**Problem**, seen in the hot block of Python's dictionary lookup (`PyDict_Contains`, `0x530910` in Debian's `python3.13`, with the `dump_translation` tool): every shift, rotate and multiplication computed its flags right away (15 to 25 instructions each, with an `mrs nzcv` and a table access) whenever the next instruction could take a slow path, and that is any memory access: liveness (`live_flags`) makes every flag live before an instruction that may hand the state to the interpreter. Shifts and rotates by `cl`, rotates by a constant, `bt*`, `bsf`/`bsr` and `comisd` were also treated as readers of every flag, so the instructions before them computed theirs too.

**Changes:**
- **Liveness per flag** (`live_flags`): before an instruction that stays in the block, the live flags are those live after it minus those it always writes (`flags_written`, which now also describes rotates by a constant: CF and OF; narrow shifts by a constant; `bt*`: CF; `bsf`/`bsr`: ZF; `comis*`: all), plus those it reads (`flags_read`: the condition's flags for `setcc` and `cmov`, CF for `adc`/`sbb`, all for `jcc`, which may leave the block). A shift or rotate by `cl` writes nothing for sure (a count of 0 leaves the flags), so the flags before it stay live while its own are.
- **Dead flags for every instruction:** an instruction whose flags may all be dead (`flags_may_write`: for a count from `cl`, those of a non-zero count) computes none, and does not materialize the pending ones first: shifts and rotates by `cl` or by a constant, narrow shifts, `bt*`, `bsf`/`bsr` and `comis*` too.
- **Deferred shifts and multiplications** (`FlagKind::Shift`, `FlagKind::Mul`): `shl`, `shr` and `sar` of a 32- or 64-bit register, two- and three-operand `imul` and one-operand `mul`/`imul` no longer compute their flags: their operands go to `FA`/`FB`, like `add` and `sub`, and whoever observes the flags redoes the operation (`shift_flags`, `mul_flags`). The flag routine does not know these kinds, so an exit computes them before it leaves (`publish_lazy`): on a slow path or a side exit, out of line. A shift by `cl` whose count is 0 must leave the flags as they were: out of line, the flags pending before it go into `FLAGS` (`zero_counts`), and its deferred flags, with a count of 0, then leave `FLAGS` as it is.
- **inc, dec, adc and sbb need only CF** from the pending flags: when CF is live it alone goes into `FLAGS` (`materialize_carry`: the operation redone and one `cset`), not every flag; when CF is dead after `inc`/`dec`, they defer all six flags like `add`, CF included (a dead value).

**Measured** (`cargo xtask bench --runs 5`, Pi 5 with 4K pages, before and after on the same day, `tech/performance.md`): `python-int` 1,346 → 1,255 ms, `python-float` 1,196 → 1,104 ms, `float` 265 → 254 ms; the rest within the noise. In the dictionary block above, the main path (without the out-of-line code) went from 546 to 439 instructions.

**Verification:**
- the new test `conditions_after_deferred_and_partial_flags`: every shift and rotate (8 to 64 bits, by `cl`, 1 and 5), `imul`, `mul`, `bt`, `bsf`, `inc`, `dec`, followed by every condition in the same block, in the next one, after an earlier `cmp` whose flags must survive, at a fault right after, and by `setcc`/`cmov` whose flags alone stay live; counts and operands at the edges (0, 31, 32, 63, 64);
- the random JIT tests, in all four modes, and lockstep;
- mutations, each caught: OF of `shr` from the result, the count not set back to 0 after the out-of-line path, the carry of `materialize_carry` inverted, `inc`/`dec` deferring CF while it is live, rotates declared as writers of every flag, the unsigned 32-bit overflow of `mul` tested as signed, `setle` declared as reading ZF alone, `seto` as reading nothing. Two were equivalent: the CF bit of `sar` by a constant taken with `lsr` (the same bit), and a 32-bit shift's value saved with all 64 bits (`shift_flags` reads only the low 32);
- `difftest` with both engines, lockstep and the ARMv8.0 paths.

## Operations in place (M11, 2026-10-20)
The same block, after the flags: copies of operands through the temporaries `A`/`B`/`R`.
- `add`, `sub`, `cmp`, `and`, `or`, `xor` on a guest register mapped whole, at 32 or 64 bits (`alu_in_place`): the instruction writes the register itself, an immediate that fits goes in the instruction (`sub rsp, 0x58`: from five instructions to three, two of them saving the deferred operands), and `cmp` emits only the deferred operands (`jcc` redoes the comparison).
- `test` writes its result straight into `FA`; `jcc` after a logical operation tests `FA` (`redo_nzcv`).
- Addresses (`address_into`): a mapped base and index are added in one instruction, base plus displacement too; `lea` into a mapped register computes there.
- `push`, `pop` and `ret` take `rsp`'s register directly; `push` stores a mapped register without a copy, `pop` loads straight into a mapped destination (but `pop rsp` loads after the increment).
- Shifts by a constant of a mapped register shift in place; a 64-bit `mov` into r8 or r9 (kept in the `Cpu`) stores the mapped source directly.

Measured with the flags above (`tech/performance.md`): 7-Zip 16.4 → 14.9 s, `integer` 225 → 195 ms, `python-float` 1,104 → 945 ms; the block's main path from 439 to 352 instructions.

Verification: the random tests now also generate addresses without a base and with a base kept in the `Cpu`; mutations caught: the sense of an immediate's sign, a negative displacement added, an index without a base left unscaled (caught only after the generator's new forms), the deferred operand taken after the register changed (logical and arithmetic), a 64-bit `mov` into a register in the `Cpu` widened to 32 bits. Surviving: a 32-bit stack pointer that wraps at 4 GiB in compatibility mode (`push`, `pop`; the same arithmetic as before, which the random tests do not reach), and the immediate of `test` at 64 bits instead of 32 (equivalent: the immediate has no higher bits).

## xmm registers in vector registers (M11, 2026-10-20)
Until 2026-10-20 every SSE instruction read its operands from `Cpu::xmm` and wrote its result back there: `addsd xmm0, xmm1` was 13 instructions, two loads and a store among them, plus the checks for denormals and NaN, and integer SSE loaded both registers with `ldr q` and stored the destination.

Now xmm0–xmm15 live in `v16`–`v31` for the whole chain of blocks, like the mapped general registers: the prologue loads them with eight `ldp q`, the epilogue stores them with eight `stp q` (v16–v31 are caller-saved for the code that calls a block, and blocks call nothing that uses them).
- **Lane operations, logic, unpacks, shifts:** straight into the destination's register (`vector_op`); a memory source is one `ldr q` into a temporary.
- **Shuffles** (`pshufd`, `pshuflw`/`pshufhw`, `shufps`/`shufpd`) with `INS` (element), lane by lane into a temporary and then one move, so that a source that is also the destination is read whole first; `pextrw` with `UMOV`, `pinsrw` and `movhps`/`movlps` loads with `INS` from a general register.
- **Moves:** register to register one `mov v.16b`; to and from memory one `ldr q`/`str q`; `movd`/`movq` to xmm with `FMOV` from a general register, which zeroes the rest as x86 does; `movss`/`movsd` loads with a scalar `ldr`, which zeroes it too.
- **Scalar floating point:** the operands are the registers' low lanes; the result goes into the destination's low lane with `INS`, which keeps the other lanes, as x86 does. The denormal check reads an operand's bits with `FMOV` from the register.
- **MMX:** the two scratch registers became `v3` and `v4`, so MMX lane operations no longer touch memory; `movq2dq`/`movdq2q` use `FMOV`.

The encoder gained `ldp`/`stp q`, `INS` (element and general), `UMOV`, `FMOV` from a general register and the vector `MOV`, all checked by `encoder-check` (12,582 samples, 0 differences).

Verification: the random JIT tests in every mode and lockstep (which compares xmm registers after every instruction); mutations caught: the scalar result written over the whole register, `pshufd`'s lanes reversed, the halves of `pshufw` exchanged, `movhlps` and `movlhps` exchanged, `movq xmm, xmm` keeping the upper half, a `cmpsd` mask over 64 bits for `cmpss`, the prologue's pairs exchanged, the epilogue storing one pair less.

## Packed SSE floating point (M11, 2026-10-20)
With the xmm registers in vector registers, packed floating point is translated too (`sse_packed`): `addps`/`addpd`, `subps`, `mulps`, `divps`, `minps`, `maxps`, `sqrtps` (and their `pd` forms), `cmpps`/`cmppd` with the eight predicates, and the conversions `cvtdq2ps`, `cvtps2dq`, `cvttps2dq`, `cvtdq2pd`, `cvtps2pd`, `cvtpd2ps` (`cvtpd2dq`/`cvttpd2dq`, `rcpps` and `rsqrtps` stay with the interpreter). The scalar path's rules apply to every lane (`tech/floating-point.md`):
- a denormal operand in any lane goes to the interpreter before the operation (x86 raises DE): integer vector operations on the lanes' bits (`slow_if_denormal_lanes`: the bits without the sign, minus 1, below twice the smallest normal number minus 1) and one `UMAXV`;
- a NaN result in any lane goes to the interpreter, which applies x86's NaN rules (`FCMEQ` of the result with itself, `UMINV`); an operand of a conversion is checked with integer compares instead, because a floating-point compare raises IOC for a signaling NaN before the interpreter runs (lockstep found it);
- UFC after a multiplication, a division or `cvtpd2ps`: cleared out of line, then the interpreter, which decides UE (tininess after rounding). It is checked before the NaN, so that a NaN's slow path does not find another lane's UFC in FPSR (lockstep found that too);
- `minps`/`maxps` are a signaling compare (`FCMGT`) and a select (`BSL`): src when unordered or equal, IE for any NaN, as x86; the predicates: `FCMEQ` (quiet) for eq, unord, neq, ord, `FCMGT`/`FCMGE` (signaling) for lt, le, nlt, nle, and `NOT` for the negated ones;
- `cvtps2dq`: a lane the host saturates to 0x7fffffff goes to the interpreter (x86 gives 0x80000000; for large negative values both give 0x80000000).
- They are translated only with MXCSR's control bits at their default, like scalar floating point (`uses_mxcsr_control`).

The encoder gained the vector `FADD`, `FSUB`, `FMUL`, `FDIV`, `FSQRT`, `FCMEQ`, `FCMGE`, `FCMGT`, `SCVTF`, `FCVTZS`, `FCVTNS`, `FCVTL`, `FCVTN`, `SSHLL`, `XTN`, `NOT`, `UMINV`, `UMAXV`, `BSL`, `CMHI` and `DUP` (`encoder-check`: 12,630 samples, 0 differences).

Verification: the new test `packed_float_operations_on_special_values` (4,000 rounds: every operation, predicate and conversion, with registers and memory, on lanes drawn from NaNs, infinities, denormals, zeros, products that underflow, values out of int32's range), with lockstep, which requires a slow path to leave the state as it was but for MXCSR flags the interpreter raises too. Mutations caught: `minps`'s operands exchanged, the negated predicates, lt as le, no check for 0x7fffffff, `cvtdq2pd` without the sign extension, no denormal check. Two survive and are equivalent in the results: a denormal limit one too high and a NaN check that also catches the largest finite values, which only send more cases to the interpreter.

## Reads checked by the host (ADR-0046, 2026-10-20)
With host protection (`tech/memory.md`), a read needs no table check: `check` emits only the test of the address against the region's end (`ands` and `b.ne` to the slow path: above it, the interpreter decides) and the next guest load records its offset and the instruction's slow path in the block's fault table (`mark_access`, `Translation::faults`). The engine keeps the table of every published block, sorted by address (`Jit::faults`), and a SIGSEGV or SIGBUS handler (`on_fault`, installed once) looks the faulting PC up there and resumes at the slow path: the interpreter runs the instruction again from the exact state, since translated code writes guest registers only after its last access that can fault, and raises the guest's signal. Any other fault is armor's own: the default action, as before. Concurrent mode's ordered loads out of line record their fault too.

Verification: `difftest` (470 cases, `faults/*` included) and lockstep with host protection on; the new test `random_code_matches_the_interpreter_with_host_protection`, which runs in a process of its own (the region must be free) with the random programs, plain and in concurrent mode; mutations caught: a load not recorded in the fault table (armor crashes on the first fault), inaccessible guest pages left readable on the host.

## Writes checked by the host, the stack with writeback (ADR-0046, 2026-10-20)
With host protection the host checks writes too. A guest page is writable on the host only when the guest may write it and it is not executable (`host_prot_of`): a write to code faults and reaches the interpreter, which makes the page writable for the copy (`with_access`, one thread at a time), writes and drops the translations, as before. A write must not write part of itself where x86 writes nothing: AArch64 may write the first part of a store that crosses into a page it may not write (CONSTRAINED UNPREDICTABLE), so one test covers both the region's end and the alignment (`ands` with the bits outside the region and the size's low bits: an aligned write cannot cross a page), and a misaligned one goes out of line (`write_edges`): within one page, back to the host; across two, both pages from the table. Every access an instruction makes after its check is recorded in the fault table, a read-modify-write's load and store both.
- **Writes before flags:** with the host checking, a store faults at the store itself; an instruction that wrote memory after computing its flags (`sbb [mem], r`, `inc [mem]`, `xadd`, narrow shifts on memory) changed `FLAGS` or `FA`/`FB` before its slow path. They now store first (lockstep found it). `cmpxchg` on memory writes in both cases, its old value when the comparison fails, as x86 does, so that it faults in both.
- **The stack with writeback:** `push` is `ands` of rsp with the bits outside the region and 7, then `str x, [rsp, #-8]!`; `pop` and `ret` test rsp and use `ldr x, [rsp], #8` (with `ret imm16` the immediate added). A misaligned rsp goes to the interpreter. A 64 KiB guard reserved above the region makes an access that starts below its end and runs past it fault, so testing rsp (or the start of an access) is enough. A faulting access with writeback leaves the base register as it was (✅ the stack cases of the host-protection test).
- ✅ Measured on the Cortex-A76: a store that crosses into a page it may not write writes nothing (the mutations without the alignment test survive the stack cases, which include a `push` across into the guard); the test stays, as the architecture allows the partial write.

Verification: the host-protection test with stack cases (into the guard and the read-only page, a misaligned rsp, `pop` and `ret` from the guard, a `push` across a page); `difftest` and lockstep; the encoder's `ldr`/`str` with writeback in `encoder-check`. Mutations caught: an ALU write to memory after its flags (lockstep, at once).

## Pending operands held, operations in place, r8 and r9 (M11, 2026-10-20)
- **Held operands:** the pending flags of `add`, `sub`, `cmp`, the logical operations and `inc`/`dec` on a guest register mapped whole no longer copy their operands to `FA`/`FB`: `Pending` records where they are (`Held`: the guest register, a constant, `FA`/`FB`), and for an operation that wrote its destination in place, that the register holds the result (the first operand is then the result minus, or plus, the second). `redo` sets the host NZCV for a condition straight from there; `spill` copies them to `FA`/`FB` (the form the flag routine and `Context::kind` describe) only when needed: before an instruction that writes a held register and does not replace the pending flags with its own, at an exit, and in every slow path or side exit, out of line, where the registers still hold them (translated code writes guest registers only after its last slow path). A `jcc` that ends the block restores the flags state between its two exits, since the first one's copy is on its own path only (the new form of `conditions_after_deferred_and_partial_flags` found it).
- **In place:** `neg`, `not`, `inc`, `dec` and `cmov` on a mapped register write it directly; a `jcc`, `setcc` or `cmov` right after another one reuses the host NZCV of the same pending operation (`nzcv_valid`), and a narrow comparison's redo no longer shifts the result back.
- **r8 and r9** in `x8` and `x21`: the exit's `rip` moved to `x4` (an exit needs no operand any more) and `KIND` into the `Context` (`Context::kind`; one more instruction where an exit publishes a pending operation). Every epilogue leaves `kind` at 0, so the prologue does not clear it: `CHAINED_ENTRY` stays 96.

Verification: the random tests in every mode, lockstep and `difftest`; mutations caught: a held operand of `neg` exchanged, `cmov`'s sense, the narrow redo's shift, CF kept wrong by `inc`, NZCV reused after a `setcc` to memory, NZCV reused by a `cmov` from memory (caught after a new form `jcc` + `cmov` from memory), r8 and r9 not stored by the epilogue, the restore of the flags state between a last `jcc`'s exits; the prologue's clear of `kind` turned out unnecessary (removed).

## Carries at block entry, operands from memory, 32-bit offsets, alignment in the host check (M11, 2026-10-20)
Found by comparing, block by block, the code armor and box64 generate for 7-Zip's hottest loops (`tech/performance.md`, "7-Zip under the per-block comparison"):
- **CF from the kind left pending** (`carry_from_kind`): an `inc`, `dec`, `adc` or `sbb` at the start of a block, where the flags pending from the block before are known only through `Context::kind`, needs only their CF; it used to call the flag routine, which computes all six (about 45 instructions with the call). The exit that chains to a block now records `KIND` in `Context::exit_kind` (in the chain stub, which runs until the exit is patched, so when the target is translated), and the translation takes it as `Options::entry_kind`: the code compares `KIND` with it and computes that operation's CF inline (redo, `cset`, `bfi`: about 8 instructions), calling the routine only for another kind. The flags stay `Lazy::Dynamic`: `KIND` and `FA`/`FB` still describe them, and CF in `FLAGS` agrees.
- **Memory operands into `FA`/`FB`:** the memory operand of `cmp`, and of `add`/`sub`/`cmp` into a mapped register, is loaded straight into the register that keeps it for the deferred flags, without a copy. It is the instruction's only access, and a load that faults leaves its register unchanged, so the slow path still finds the flags pending before it intact.
- **32-bit offsets in W registers** (`address_into_w`): `lea` into a 32-bit register and every address of 32-bit code (the address-size prefix, compatibility mode, ADR-0029) is computed modulo 2^32 with W operations, which clear the top half, instead of 64-bit operations and a zero-extension after (`add x, …` then `mov w, w`): one instruction less for each such address, so for every access of i386 programs and of the WoW64 backend.
- **Alignment in the host check, concurrent mode:** LDAPR and STLR need an aligned address on CPUs without FEAT_LSE2, so each ordered access tested two low bits with `tbnz` after the check of the region. With host protection the check's `ands` now tests both (the region's bits and the size's low bits, as writes already did), and the unaligned case goes out of line (`read_edges`, `write_edges`) to the plain access and barrier (`unaligned_at`); an unaligned case that no ordered access takes (a vector access) goes back to right after the check (`misaligned_stubs`). Two instructions less for every access of a multithreaded program.

Verification: the new test `carry_from_the_kind_left_pending` (one block entered from four with different kinds pending: an 8-bit subtraction, a 32-bit addition, a logical operation, an increment that keeps CF; `adc` sums the carries over hundreds of iterations, with the expected kind depending on the count); the random tests in every mode, lockstep and `difftest`. Mutations caught: the inline carry inverted, the routine skipped for the other kinds, the kind not compared; the memory operand loaded into `FB` but held as `FA`, and loaded into `FB` for a `cmp` that holds `FA`; 32-bit offsets computed at 64 bits.

## Region tests once per base, accesses at an offset, test and shifts in place (M11, 2026-10-20)
- **Checked addresses** (`Translator::checked`): with host protection, an access whose address is a mapped base register plus a constant records, when its region test passes, the base, the constant and the alignment the test covered. A later access in the block with the same base, at most `REGION_GUARD` (64 KiB, ADR-0046) from it and aligned as it needs (writes, and ordered accesses in concurrent mode, to their size), needs no test (`covered`): below the tested address it is in the region or faults (nothing is mapped below the region, and an address that wrapped is not canonical); above it, it falls in the guard at worst. The entries follow a base that moves by a constant (`base_moved`: `push`, `pop`, `add`/`sub` of an immediate, `lea` from itself, at 64 bits) and are dropped when an instruction writes the base otherwise (`registers_written`). `push`, `pop` and `ret` use them too: of a run of pushes only the first tests rsp.
- **Straight to `[base, #offset]`** (`direct`): a covered plain load or store (not concurrent mode) whose constant the instruction encodes (a multiple of the size, below 4096 times it) is one `ldr`/`str` from the base register, without computing the address.
- **`test r, r`** of a mapped register at 32 or 64 bits holds the register as the pending logical result (`Held::Reg`), without a copy into `FA`.
- **Shifts by cl in place:** `shl`, `shr` and `sar` of a mapped register by cl shift the register itself (`and` of cl into the count, the shift; with dead flags nothing else at 64 bits, and a test of the count at 32 bits, where a count of 0 writes nothing, as the interpreter does); a constant shift in place no longer copies the register first.

Verification: the new test `region_tests_cover_nearby_accesses` counts the tests on the main path (nearby reads and writes, a written base, `add`, `sub` and `lea` of the base, pushes, a load from their slots and pops, misaligned writes, the edge of the guard's distance after a push); the host-protection test gains accesses after the base moves to armor's own memory above the region, which must fault as on x86 and leave that memory unwritten; the random tests in every mode. Mutations caught: the base not dropped when written, the sign of the move, `push` moving rsp up, `test` holding `FA` instead of the register, the 32-bit count of 0 shifting (with live and with dead flags).

## syscall translated, known addresses (M11, 2026-10-20)
- **`syscall`** (64-bit mode) ends a block as the interpreter runs it: `rcx` = the next rip, `r11` = `rflags` (made exact first), then an exit with the status `EXIT_SYSCALL`, which the engine returns as `Step::Syscall`. Before, every syscall left the block for the interpreter, which decoded and ran the instruction. Lockstep compares the two and checks that both stopped for it. In compatibility mode (`int 0x80` and `syscall` of i386 code) the interpreter, as before.
- **Known addresses:** an address known at translation time (RIP-relative, or absolute without base and index: globals, constants) has its region test decided then: below the region's end, and aligned when the access needs it, no test.

Verification: the random tests end every program with a `syscall`, now translated, compared with the interpreter (registers, rcx and r11 included) and in lockstep; `region_tests_cover_nearby_accesses` gains RIP-relative reads and writes, aligned and not (the mutation without the alignment condition caught).

## Loops start from the state of their back edge (M11, 2026-10-21)
- **The copy of a loop** (`Translator::loops`, `exit_direct`): an exit back to the block's own first instruction, with the flags known (`Lazy::Static` or `Current`), no longer leaves the block. It jumps to a copy of the block's body translated from that state: the flags pending as they are (no copy of the operands into `FA`/`FB`, no `KIND` stored, no flag routine at the next iteration's first reader) and the xmm values known not to be denormal (`normal`), with its own budget check at its head and its own way out when the budget runs out (the pending flags published then). Its own back edges jump to its head when their state implies the one it was translated from (the same pending flags, at least the same known values: always so, as its body is the block's); any other exit leaves as before. One copy per block; the checked addresses (`checked`) start empty in it. Not in lockstep (one instruction per block).
- **Spills skipped** when no flag pending is live after the instruction that writes a held register (`live`): nothing observes them after it, and the instruction itself (a `cmov` on the same register, say) reads them before it writes. A spill no longer copies `FB` onto itself. `cmp [mem], reg` holds the register (`Held::Reg`) instead of copying it into `FB`.

✅ Best of 5: `float` 167 → 154 ms, `simd` 84 → 75 ms (box64 135 and 92), 7-Zip 10.51 → 10.41 s; the Python loops, `integer` and `memory` within the noise.

Verification: the new test `loops_start_from_the_state_of_their_back_edge`: loops whose back edge leaves `cmp`'s flags pending and whose body reads them first (`setb`, `adc` after a `dec` that keeps CF, `cmovb`), with the back edge an unconditional `jmp`, a conditional jump inside the block and the block's last instruction; the random tests (whose programs end with loops on `jnz`) and lockstep. Mutations caught: the copy started with the flags current instead of pending; spills never made (the random tests, at once); `cmp [mem], reg` holding the wrong register. Equivalent: the copy's back edges compared without the known values (the body being the same, its state always implies its entry's).

## Required alignment in the region's test (M11, 2026-10-21)
- `movdqa`, `movaps`, `movapd` and the 16-byte memory operands of legacy SSE require an address aligned to 16 (#GP otherwise, from the interpreter). With host protection the alignment and the region are now one test (`check_aligned`: `ands` with the region's bits and 15, to the slow path), as both cases go to the interpreter; an aligned access cannot cross a page, so a write needs nothing out of line.
- **The alignment a checked address proves** (`Translator::checked`) is now only a required one: a write's test, or an ordered read's, also tests the low bits, but a misaligned address goes on out of line (`write_edges`, `read_edges`), so it proves no alignment. Recording it as proven, as since the checked addresses were introduced, let a later write near a misaligned one skip its test; with a required alignment it skipped a #GP (the host-protection random test found it). A write near another one is therefore tested again; reads, which need no alignment, are covered as before.

✅ `simd` 128 → 84 ms (best of 5; box64 92); 7-Zip 10.3 → 10.45 s, the Python loops about 1% slower, from the writes tested again.

Verification: the random tests with host protection (an aligned store after a misaligned one at the same base, now a #GP like the interpreter's); `region_tests_cover_nearby_accesses` updated (a write after a write is tested again; `movaps` after `movdqa` at the same address is not).

## Operands known not to be denormal (M11, 2026-10-21)
Every scalar SSE operation tests its operands for denormals (x86 raises DE, the host nothing) with a move from the vector register to a general one, an `ands` and a branch; with the test of a multiplication's or division's result for underflow, three such moves per operation. On `bench`'s `float` workload, a build made for the purpose without either kind of test took 136 ms instead of 186 (box64 135): the moves between the register files, not the instructions, are what costs.
- **What is known** (`Translator::normal`, `update_normal`): per xmm register, whether its low lane is known not to be denormal as a double and as a single. Known after an integer converted (`cvtsi2sd`/`ss`), a square root, `min`/`max` (one of their tested operands), a comparison's mask, `cvtss2sd`, a register zeroed with `xorps`/`xorpd`, and a `mul`, `div` or `cvtsd2ss` whose result passed `underflow`: its out-of-line part now sends a denormal result (not 0, not ±the smallest normal number) to the interpreter, so that what comes back never is one. Copied by `movaps`/`movapd` and `movss`/`movsd` between registers (`movss` keeps only the single's). Any other write forgets the register; `pcmpestrm`/`pcmpistrm` and the state instructions forget all. The result of `add` and `sub` is not known: an exact difference can be denormal.
- **Use:** `slow_if_denormal` emits no test for a known operand, and an operand that passes its test is known from then on (the same register used twice is tested once). Within a block: at its start nothing is known.

✅ `float` 186 → 167 ms (best of 5); `simd` and the Python loops unchanged (packed operations and other code).

Verification: the new test `denormal_tests_skipped_only_where_known`: 3,000 random sequences of scalar operations, conversions, copies, zeroing and comparisons at both widths on denormals, values whose squares or differences are denormal, zeros, ±the smallest normal numbers and infinities, and targeted cases where an exact subtraction, addition, multiplication, division or `cvtsd2ss` makes a denormal that the next operation uses (with no denormal before, as DE is sticky); MXCSR compared with the interpreter. Mutations caught: the zero test of `underflow_fixup` inverted (it had been written so, sending zeros to the interpreter and letting denormal products through, and the random tests had not seen it); `add` and `sub` results taken as known (caught only by the targeted cases); every xmm operand taken as known; `movss` passing on the double's property.

## Translations cached on disk (ADR-0048, since 2026-10-20)
A translated block depends only on its guest bytes, its address and the options: since the shared prologue and epilogue it does not depend on where it is published. `cache.rs` keeps the translations of a program in a file and the engine reuses them in later runs:
- **Lookup** (`Jit::translate`, before decoding): the records for the block's address and options key (`cache_key`: the `Options` but the entry kind, which is a hint, and MXCSR's default); one is used only if the guest bytes now at the address are exactly its recorded bytes, and if its checksum holds (checked the first time it is needed). A hit is published like a fresh translation (its exits unchained), with its fault table. Not in lockstep.
- **Record:** a new translation is added with its guest bytes, words, fault table and instruction count; at most four per address and key (code that changes in place, a guest JIT's, does not accumulate).
- **File:** one per program, named after armor's build (its executable's size and modification time), the program (canonical path, size, modification time) and its mode, in `ARMOR_JIT_CACHE_DIR`, else `$XDG_CACHE_HOME/armor/jit`, else `~/.cache/armor/jit`, mode 0600. The first lookup reads it and indexes the records by their headers; the new records are appended (one `write` with `O_APPEND`, so that processes of the same program do not overwrite each other) when the process exits or replaces itself (`BEFORE_EXEC`, before `execve`). A record cut short at the end is cut off before appending; a file that is not a cache file or is larger than 256 MiB starts over; past 1 GiB in the directory, the files used least recently go (`trim`; reading a file renews its date).
- **Processes:** a child of `fork` inherits the parent's unsaved records; they are the parent's to save, so the cache drops them at its first use in the child (it keeps the pid it belongs to). A process stops recording past 64 MiB of unsaved records (a guest that keeps generating code).
- **Cost:** a warm start reads the whole file (10.5 MB for Python's startup: about 32 ms of system time with the page faults), whatever part of it the run needs; a program whose file grows towards the 256 MiB limit pays proportionally.
- **Off** with `ARMOR_JIT_CACHE=off`, in lockstep, with the interpreter, and with `--sandbox`, whose guest is not trusted with host code (`tech/security.md`). The Windows backend (ADR-0047) has no cache: it has no file system of its own.

✅ Measured (Pi 5, 4K pages, 2026-10-20, best of 5): Python's startup 185 ms without the cache, 218 ms the first time (the records encoded and 10.5 MB written; 7 ms of them the write), 114 ms after (16,776 blocks reused, a handful translated); `python-int` 690 → 623 ms, `python-float` 617 → 537 ms with the cache warm.

Verification: `translations_come_back_from_the_cache` (in a process of its own: the cache is process-wide) translates three random programs, saves, reads the file again as a new process would and runs them from it, compared with the interpreter; then damages the first word of every record (the checksums left as they were) and checks that none is used. The same test forks after recording and checks that the child, saving, writes nothing of its parent's. Mutations caught: a lookup that does not compare the guest bytes; no checksum check; a forked child taken for its parent. The file format (`records_round_trip_and_damage_is_found`), the per-address limit and `trim` have unit tests; the chain runs `difftest --jit` twice, the second with the cache warm (476/476 both).

## Inside Wine: the WoW64 backend (ADR-0047, since 2026-10-20)
`armor-wow64.dll` (`wow64/`, its own workspace; `cargo xtask wow64`) is armor's engine as the CPU backend that Hangover's `wow64.dll` loads for 32-bit x86 Windows code, in FEX's and box64's place: `HODLL=armor-wow64.dll`, the DLL in the prefix's `system32`.
- ✅ **The DLL:** built for `aarch64-pc-windows-msvc` with LLVM's `lld-link`, without MSVC or a Windows SDK: the import libraries of `ntdll` and `wow64.dll` come from `ntdll.def` and `wow64.def` (`llvm-dlltool`), `/NODEFAULTLIB /NOENTRY`, no C runtime. A small runtime of its own (`wow64/src/rt.rs`): the allocator on the process heap (`RtlAllocateHeap`), the panic handler (the process ends), the memory functions the compiler calls. Wine's loader takes it as a builtin only with `Wine builtin DLL` at offset 64 of the DOS stub, which `cargo xtask wow64` writes (`target/wow64/armor-wow64.dll`). Placed only in the engine's own directory, Wine does not find it: `armor run` copies it into `system32` of the application's prefix.
- **Shared code, `no_std`:** `armor-guest` (new: the `GuestMemory` trait, `Prot`, `Access`, `MemFault`, the page constants), `armor-x86` and `armor-cpu` build without `std` (with `alloc`); `rdtsc` asks the memory for the time stamp counter (`GuestMemory::time_stamp_counter`: `Instant` on Linux, `cntvct_el0` in the DLL). `armor-jit` without its default feature `std` is only the translator and the encoder; the Linux engine, buffer, lockstep and statistics need `std`.
- **The interface** (`wow64/src/lib.rs`): `BTCpuProcessInit` allocates a page below 2 GiB with two `int 0x2e` stubs (`BTCpuGetBopCode`: the system call; `__wine_get_unix_opcode`: Wine's unix call); the guest state is a `Cpu` per thread (in a TLS slot), converted to and from `WOW64_CONTEXT` (`BTCpuGetContext`/`SetContext`, through `NtQueryInformationThread` class 29 for another thread), with `fs` based at the 32-bit TEB. `BTCpuSimulate` runs the guest until it reaches a stub: the system call goes to `Wow64SystemServiceEx` with the number in `eax` and the arguments above the return address; the unix call pops its handle, code and arguments and calls `__wine_unix_call_dispatcher`. Memory notifications (`BTCpuFlushInstructionCache2`, `NotifyMemoryFree`, `NotifyMemoryProtect`, `NotifyUnmapViewOfSection`) drop the translations.
- **Guest exceptions:** an x86 fault the interpreter reports (an access violation, an illegal instruction, a division by zero, a breakpoint) is raised with `NtRaiseException` as Wine's x86 exception, with the guest state saved; Wine's WoW64 dispatch then delivers it to the guest's handlers (`BTCpuResetToConsistentState` restores the guest's `rip` and copies in the context captured at `BTCpuSimulate`'s entry).
- **The JIT** (`wow64/src/jit.rs`): the same translator in compatibility mode, with a 64 MiB code buffer from `NtAllocateVirtualMemory`, a block cache, chained exits and the indirect-jump table, the interpreter for the rest. Guest accesses are checked by the host's own protections (ADR-0046's reads and writes, `Options::page_table = false`: Wine keeps the guest's protections on its pages, and addresses wrap at 4 GiB): a fault in translated code reaches `BTCpuResetToConsistentState`, which finds the access in the fault table and resumes at its slow path with `NtContinue`, where the interpreter runs the instruction again and raises the guest's exception. In compatibility mode the prologue does not load r8–r15 (`x18` is the TEB on Windows). Concurrent mode from the second thread on.
- ✅ **Measured (2026-10-20, Hangover 11.16, 4K pages):** `hostname.exe` and `cmd.exe /c echo` print what they print under FEX; a test program (32-bit, built with `clang --target=i686-pc-windows-msvc` and `lld-link`, outside the repository) whose `IsBadReadPtr(NULL)`, `IsBadReadPtr(&data)` and `IsBadWritePtr(code)` make Wine's `kernel32` catch access violations with SEH prints the same three answers and exit status as under FEX; 7-Zip 26.04 x86 (`7z a -t7z -mx=9 -mmt=1` of `tests/guest`) gives the archive FEX gives, byte for byte, in 23.3 s with the interpreter alone, 1.3 s with the JIT, 0.4 s under FEX; on a 12 MB input, 16.5 s against FEX's 8.7 and box64's 8.4, and 13.2 s after the JIT's later changes of the same day (`tech/engines.md`, "armor's own WoW64 backend"). `cargo xtask app-check --x86-engine armor`: 7-Zip x86 level 3, Notepad++ and KeePass 2 (32-bit installers) level 1.

## Postponed
Traces, SMC through host protection, per-page invalidation. Done after this list: scalar SSE floating point (increment 17), parallelism with TSO (ADR-0023).
