# x86-64 instruction encoding

**Reference source:** Intel SDM Vol. 2, ch. 2 ("Instruction Format") and ch. 3.
**Verification status:** the "Implemented in M1" section is ✅. The sections marked ⚠️ are written from memory (2026-10-12); before implementing one of them:
1. verify it against the SDM;
2. verify it empirically with `x86_64-linux-gnu-objdump` (`testing.md`);
3. then change the section's marker to ✅, stating how it was verified.

## Implemented (`armor-x86`, M1–M2)
Every form below is covered by unit tests (bytes taken from `objdump`) ✅, by differential tests against qemu ✅ and by `cargo xtask decoder-check`: on about 114,000 instructions of static glibc, 0 lengths different from objdump ✅.

Modules: `lib.rs` (types), `decoder.rs` (prefixes, ModRM/SIB, 1-byte and `0F` maps), `sse.rs` (SSE opcodes of the `0F` map), `maps.rs` (three-byte maps `0F38` and `0F3A`, since 2026-10-13), `x87.rs` (x87 opcodes `D8`–`DF`), `length.rs` (length-only decoder, below).

### 32-bit mode (ADR-0029, 2026-10-13)
`decode_in(bytes, Mode::Compat32)` and `length_in(bytes, Mode::Compat32)` decode i386 code (compatibility mode); `decode`/`length` remain the 64-bit mode. Differences:
- **no REX:** 40–47 are `inc r32`, 48–4F `dec r32`;
- **32-bit addresses by default** (`Instruction::addr32`, `Mem::addr32`); mod 00 rm 101 is an absolute address (no RIP); with 67 addresses are 16-bit (different ModRM, without SIB): the length-only decoder measures them, `decode` reports them as unsupported;
- **32-bit stack:** `push`, `pop`, `call`, `ret`, `enter`, `leave`, `pushf`, `popf`, jumps and `loop` have `size` 32 (the stack width); string instructions, `loop`/`jecxz` and `xlat` use esi, edi, ecx, ebx;
- **32-bit-only opcodes**, whose length `length` measures and for which `decode` says "unsupported" until a program asks for them: `push`/`pop` of segment registers, BCD (`daa`, `das`, `aaa`, `aas`, `aam`, `aad`), `pusha`/`popa`, `bound`, `arpl`, `les`/`lds` (C4/C5 are VEX only with ModRM.mod = 11, like 62 for EVEX), `into`, `salc`, far `call`/`jmp` with a pointer (6 bytes), 82 as an alias of 80;
- `int 0x80` is the syscall (`Op::Int80`); `syscall` (0F 05) is #UD; `mov sreg, r/m16` (`Op::LoadSegment`) loads gs/fs with the selector of a TLS (`set_thread_area`), ds/es/ss only with the flat data selector; `mov r/m, sreg` gives cs 0x23 and ds/es/ss 0x2b;
- `moffs` has 4 address bytes (2 with 67); with 66 relative jumps are 16-bit.

✅ `decoder-check` on i386 glibc (`libc.so.6`, `libm.so.6`, `ld-linux.so.2`), a static i386 hello and the 39 i386 ELF files of the Steam bootstrap: **4,622,370 instructions, all with objdump's length** in the length-only decoder; 4,606,100 decoded for execution (the rest was not supported then: VEX, x87 BCD, …; x87 BCD is since 2026-10-20). Two new `decoder-check` criteria, for bytes that objdump disassembles and the CPU rejects with #UD (data in code sections): `lock` on an instruction that does not allow it, `mov cs,` and `mov ?,`. Also fixed in the meantime for 64 bits: `0F 20–23` (control and debug registers) and `0F 24`/`26` (test registers) have only the ModRM, the mod field is ignored; `fwait` merged with an x87 instruction that armor does not execute.

### Length-only decoder (`length.rs`, M4, 2026-10-13)
`armor_x86::length(bytes)` gives the length and **extension** of any x86-64 instruction, including those armor does not execute: prefixes (REX counts only right before the opcode; with 66 and REX.W the immediate stays 32-bit), 1-byte maps, `0F`, `0F38`, `0F3A`, VEX (C4/C5: maps 1–3; `vzeroupper` without ModRM; immediate in map 3 and in 70–73, C2, C4–C6 of map 1), EVEX (62: maps 1–3, 5, 6, always ModRM, compressed disp8 that stays one byte), XOP (8F with map field ≥ 8: 1-byte immediate in map 8, 4-byte in map A), 3DNow! (`0F 0F`, operation in the final byte), VIA PadLock (`0F A6`/`A7`). The extension comes from the opcode and the mandatory prefix: base x86-64, x87, MMX, SSE/SSE2, SSE3, SSSE3, SSE4.1, SSE4.2, POPCNT, AES-NI, PCLMULQDQ, SHA, XSAVE (since 2026-10-21: `0F 01 D0`/`D1`, `0F AE /4–/6` with memory and no mandatory prefix, `0F C7 /3–/5` with memory), MOVBE (since 2026-10-21: `0F 38 F0/F1` without F2/F3), 3DNow!, VEX (AVX, AVX2, FMA, BMI… not distinguished), EVEX (AVX-512), XOP, VIA PadLock.
- ✅ `decoder-check` on all 513 ELF files of the guest root (executables and libraries), busybox and the test programs: **11,261,314 instructions, all with objdump's length**; where `decode` succeeds, the two lengths always match. On the first run only the 18 PadLock instructions of `engines-3/padlock.so` were missing.
- ✅ fuzzing (`fuzz_decoder`, 3 million cases): `length` never panics and matches every successful `decode`; it found the immediate with 66 + REX.W.
- Used by `decoder-check` and by `armor inspect` (`extensions` and `missing` lines, linear scan of the `SHF_EXECINSTR` sections as objdump does). `decode` remains the decoder armor executes with; when it says "unsupported", the interpreter asks `length` for the extension, and if CPUID does not declare it the instruction raises #UD, that is SIGILL (ADR-0026, since 2026-10-13; before, exit 125). ✅ `c/undeclared_extensions` identical to qemu and to the Xeon X5675 (VEX, EVEX, BMI1, SHA, XOP, 3DNow!, PadLock).

### M2
- **Prefixes:**
  - `67`: address computed as 32-bit (`Mem::addr32`). With string instructions and `loop`/`jrcxz` → unsupported;
  - `F0` (LOCK): accepted only on read-modify-write with a memory destination (ALU except `cmp`, `inc`/`dec`/`not`/`neg`, `xchg`, `cmpxchg`, `xadd`, `bts`/`btr`/`btc`); elsewhere #UD (`Invalid`);
  - `F2`/`F3`: `rep`/`repne` for string instructions, mandatory prefix for SSE, ignored elsewhere (`rep ret`, `bnd call`, `pause`).
- **Integer (1-byte map):** `movsxd` (63), three-operand `imul` (69, 6B), `xchg` (86, 87, 90+r), `pop r/m` (8F), `cbw`/`cwd` and variants (98, 99), string instructions (A4–A7, AA–AF), shifts and rotations (C0, C1, D0–D3), `ret imm16` (C2), `enter` (C8; with prefix 66 unsupported), `leave` (C9), `int3` (CC, and CD 03), `int n` (CD: #GP, except 3; `int 0x80`, the 32-bit syscalls, unsupported), `int1` (F1, #DB), `popf` (9D), `sahf`/`lahf` (9E, 9F; valid in 64-bit with LAHF-LM), `mov` between accumulator and absolute address (A0–A3, 8 address bytes, 4 with 67), `xlat` (D7), `mov r/m, cs`/`ss` (8C: the fixed selectors 0x33 and 0x2b; since 2026-10-14 also `es`, `ds`, `fs`, `gs`, which read 0, the null selector Linux leaves in them ✅ Xeon X5675, test `c/data_selectors` only with `--oracle`: qemu-x86_64 shows 0x2b in `ds` and `es`, `testing.md`), privileged instructions that give #GP in user mode (`hlt` F4, `cli`/`sti` FA/FB, port I/O E4–E7, EC–EF, 6C–6F), `loop*`/`jrcxz` (E0–E3), `hlt` (F4), `cmc`/`clc`/`stc`/`cld`/`std`, group 3 (F6/F7: `not`, `neg`, `mul`, `imul`, `div`, `idiv`), groups 4/5 (FE/FF: `inc`, `dec`, indirect `call`/`jmp`, `push r/m`).
- **Integer (`0F` map):** `ud2` (#UD), hint NOPs and prefetch (0D, 18–1F, including `endbr64`), `cmovcc` (40–4F), `setcc` (90–9F), `cpuid` (A2), `bt`/`bts`/`btr`/`btc` (A3, AB, B3, BB, BA), `lfence`/`mfence`/`sfence` (AE /5–7, register; `Op::Fence`, a full host barrier since 2026-10-13, ADR-0023), `imul r, r/m` (AF), `cmpxchg` (B0, B1), `movzx`/`movsx` (B6, B7, BE, BF), `bsf`/`bsr` (BC, BD, also with F3), `xadd` (C0, C1), `movnti` (C3), `cmpxchg8b`/`cmpxchg16b` (C7 /1, memory only, also with LOCK; `cmpxchg16b` requires the operand aligned to 16, otherwise #GP), `bswap` (C8–CF).
- **SSE2** (`sse.rs`): `movups`/`movaps`/`movupd`/`movapd`, `movdqa`/`movdqu`, `movntps`/`movntpd`/`movntdq`, `movd`/`movq` (6E, 7E, F3 7E, D6), `movlps`/`movhps`/`movlpd`/`movhpd`, `movhlps`/`movlhps`, `unpck*ps`/`pd`, `andps`/`andnps`/`orps`/`xorps` (and pd), `punpck*`, `pcmpeq*`, `pcmpgt*`, `pshufd`, shifts by immediate (71–73), `padd*`, `psub*`, `pminub`/`pmaxub`/`pminsw`/`pmaxsw`, `pand`/`pandn`/`por`/`pxor`, `pmuludq`, `pmovmskb`. The integer opcodes without prefix 66 are MMX → unsupported.
- **SSE3** (`sse.rs`, M4, 2026-10-13): `addsubps/pd` (66/F2 D0), `haddps/pd` and `hsubps/pd` (66/F2 7C, 7D), `movddup` (F2 12), `movsldup` (F3 12), `movshdup` (F3 16), `lddqu` (F2 F0, memory only).
- **SSSE3** (`maps.rs`, M4, 2026-10-13): `66 0F 38` 00–0B (`pshufb`, `phaddw/d/sw`, `pmaddubsw`, `phsubw/d/sw`, `psignb/w/d`, `pmulhrsw`), 1C–1E (`pabsb/w/d`), `66 0F 3A 0F` (`palignr`). The forms without 66 are MMX → unsupported. Semantics in `armor-cpu/src/simd.rs` (pure functions, with unit tests); ✅ `sse/ssse3` identical to qemu and to the Xeon X5675.
- **SSE4.1** (`maps.rs`, M4, 2026-10-13): `66 0F 38` 10, 14, 15 (`pblendvb`, `blendvps/pd`, implicit mask in xmm0), 17 (`ptest`), 20–25 and 30–35 (`pmovsx`/`pmovzx`, memory source of only the width read), 28 (`pmuldq`), 29 (`pcmpeqq`), 2A (`movntdqa`, memory only), 2B (`packusdw`), 38–3F (`pmin`/`pmax` sb, sd, uw, ud), 40 (`pmulld`), 41 (`phminposuw`); `66 0F 3A` 08–0B (`roundps/pd/ss/sd`), 0C–0E (`blendps/pd`, `pblendw`), 14–17 (`pextrb/w/d/q`, `extractps`, to register or memory), 20–22 (`pinsrb`, `insertps`, `pinsrd/q`), 40–42 (`dpps`, `dppd`, `mpsadbw`). ✅ `sse/sse41` identical to qemu and to the Xeon X5675.
- **SSE4.2 and POPCNT** (M4, 2026-10-13): `66 0F 38 37` (`pcmpgtq`), `F2 0F 38 F0/F1` (`crc32` r32/r64 with source r/m8, r/m16, r/m32, r/m64; without F2 they are `movbe`, unsupported), `66 0F 3A 60–63` (`pcmpestrm`, `pcmpestri`, `pcmpistrm`, `pcmpistri`; with REX.W the lengths are 64-bit `rax`/`rdx`; memory without an alignment constraint), `F3 0F B8` (`popcnt`; without F3 it is `jmpe`, which does not exist in 64-bit: #UD). ✅ `sse/sse42` identical to qemu and to the Xeon X5675; `sse/sse42_hardware` (64-bit lengths, where qemu reads only `eax`/`edx`) identical to the Xeon.
- **XSAVE** (since 2026-10-21, `tech/avx.md`): `xgetbv` (`0F 01 D0`, `Op::Xgetbv`), `xsetbv` (`0F 01 D1`, `Op::Xsetbv`, #GP), `xsave` and `xrstor` (`0F AE /4`, `/5`, memory, no 66/F2/F3; `StateOp::Xsave`, `Xrstor`; REX.W gives `xsave64`/`xrstor64`, the same operation). ✅ unit test with bytes from `objdump -D -b binary`; `xsave/*` in `difftest`. They raise #UD unless CPUID declares XSAVE (`ARMOR_CPU_FEATURES`); `xsaveopt`, `xsavec`, `xsaves`, `xrstors` and the rest of `0F 01` stay unsupported.
- **VEX, BMI1, BMI2, LZCNT, MOVBE** (since 2026-10-21, `vex.rs`, `tech/avx.md`): the C4/C5 prefixes (`Decoder::vex_prefix`: REX bits, VEX.W, vvvv, L, pp; #UD after 66, F2, F3, LOCK or REX, and for maps other than 1–3); map 2 F2 (`andn`), F3 /1–/3 (`blsr`, `blsmsk`, `blsi`), F5 (`bzhi`, F3 `pext`, F2 `pdep`), F6 F2 (`mulx`), F7 (`bextr`, 66 `shlx`, F3 `sarx`, F2 `shrx`), map 3 F0 F2 (`rorx`), as `Op::Bmi`; VEX.L = 1 is #UD. `movbe` (0F 38 F0/F1 without F2/F3, memory only; `Op::Movbe`). F3 0F BC/BD keep `Op::BitScan` with `counts_zeros`, which counts zeros only with BMI1/LZCNT declared. ✅ unit test `bmi_and_movbe` with bytes from the GNU assembler and objdump; `bmi/bmi` and `c/bmi_compiled` in `difftest`. They raise #UD unless CPUID declares them.
- **VEX forms of SSE** (AVX and, for the 256-bit integer forms, AVX2; since 2026-10-22, `vex.rs`, `tech/avx.md`): maps 1–3 decoded by the legacy tables with VEX.pp as the mandatory prefix, as `Op::Vex` (VEX.vvvv the first source, the destination of the shifts by an immediate, or unused and 1111); `vzeroupper`/`vzeroall` (VEX 0F 77, `Op::Vzero`); `vldmxcsr`/`vstmxcsr` (VEX 0F AE /2, /3, `Op::VexMxcsr`); compare predicates 0–31; VEX.L = 1 is #UD for the operations without a 256-bit form. MMX forms and the legacy blendv opcodes: #UD. ✅ unit test `vex_sse_forms`; `avx/vex128` in `difftest`.
- **Unsupported, with an explicit reason:** VEX (C4/C5) other than BMI and the VEX forms of SSE (the AVX-only instructions, AVX2, FMA, F16C), EVEX (62), the rest of the `0F38`/`0F3A` maps (AES, PCLMULQDQ, SHA: not declared) and system instructions (all of D8–DF are supported: transcendentals since 2026-10-14, packed BCD since 2026-10-20, `tech/floating-point.md`), `rdrand`/`rdseed`/`rdpid` (rest of the `0F C7` group, not declared by CPUID), far returns and `iret`, 16-bit forms of `enter`/`popf` (16-bit `push`/`pop` since 2026-10-14, below); in 64-bit mode also `int 0x80` and loading segment registers (8E), which are supported in compatibility mode (ADR-0029). Reading `es`, `ds`, `fs`, `gs` (8C) in 64-bit mode gives 0 since 2026-10-14 (above).
- **MMX** (since 2026-10-14, `sse.rs` and `maps.rs`): without a prefix, the integer opcodes of SSE2 and SSSE3 are the MMX forms on `mm0`–`mm7` (`Reg::Mm`; REX does not extend them). The decoder reads them like the 66 form and changes the operands (`to_mmx`: 2-bit indexes for `pinsrw`/`pextrw`, no whole-register byte shifts); separately `movq` (6F, 7F), `movntq` (E7), `pshufw` (70), `emms` (77), `movq2dq`/`movdq2q` (F3/F2 D6) and the conversions `cvtpi2ps`, `cvt(t)ps2pi`, `cvtpi2pd`, `cvt(t)pd2pi` (`MmxConversion`). Without a prefix 0F D6, E6, F0 and FF (`ud0`) are #UD.
- **Reading `ds`/`es`** ⚠️: qemu returns 0x2b. I recall that 64-bit Linux zeroes them at process start, but this cannot be verified on this host (a real x86-64 would be needed). armor leaves them unsupported until a program asks for them and the value is verified.
- **`popf`:** changes the status flags, DF, NT, AC and ID; IF and IOPL stay as they are (✅ test `stack/enter_popf`, same as qemu). TF (single-step) → unsupported: it would require a SIGTRAP after the following instruction. AC with alignment checking enabled would give #AC on misaligned accesses under Linux ⚠️: qemu does not emulate it and neither does armor.
- **16-bit `push` and `pop`** (since 2026-10-14; obfuscated code in a Spotify build uses them): with the operand-size prefix `66` and without REX.W, `push`/`pop` move 2 bytes and change `rsp` by 2; `pop` of a 16-bit register keeps its upper bits. Forms: `66 50+r`/`66 58+r` (registers; REX.B selects r8–r15 as usual), `66 FF /6` and `66 8F /0` (memory, e.g. `66 41 FF 37`), `66 68 iw`, `66 6A ib` (sign-extended to 16 bits). With REX.W they stay 64-bit ⚠️ (SDM rule, from the code; not in the test). The JIT translates only 32- and 64-bit stack operations and leaves these to the interpreter. ✅ test `stack/push_pop16`, identical under qemu, the interpreter, the JIT, lockstep and the Xeon X5675 (2026-10-14).

### M1
- **Prefixes:** `66`; `64`/`65` (FS/GS); `26 2E 36 3E` ignored; REX with the "must precede the opcode" rule (test `rex_before_a_legacy_prefix_is_ignored`).
- **ModRM/SIB:** all cases, including RIP-relative, r12/r13 as base and absent index with `100`.
- **Opcodes:**

  | Forms | Opcode |
  |---|---|
  | ALU (`add or adc sbb and sub xor cmp`) | `00`–`3D` (rows), `80`/`81`/`83 /digit` |
  | `test` | `84`, `85`, `A8`, `A9`, `F6 /0`, `F7 /0` |
  | `mov` | `88`–`8B`, `B0`–`B7`, `B8`–`BF` (imm64 with REX.W), `C6 /0`, `C7 /0` |
  | `lea` | `8D` (with `mod=11` → #UD) |
  | stack | `50`–`5F`, `68`, `6A`, `9C` (`pushfq`) |
  | jumps and calls | `70`–`7F`, `0F 80`–`8F`, `EB`, `E9`, `E8`, `C3` |
  | `syscall` | `0F 05` |

  `66` with jmp/call/jcc/pushf (short and long forms) → unsupported. `66` with push/pop: unsupported until 2026-10-14, then decoded (M2 section, "16-bit `push` and `pop`").
- **Invalid in 64-bit mode** (`DecodeError::Invalid`, SIGILL):
  - `06 07 0E 16 17 1E 1F 27 2F 37 3F 60 61 82 9A EA`: ✅ SIGILL under qemu too;
  - `CE D4 D5`: ✅ "Invalid" according to the SDM (felixcloutier.com), even though qemu executes them;
  - `D6`: ⚠️ not documented by Intel.

## General format ⚠️
```
[legacy prefixes 0–4] [REX] [opcode 1–3 bytes] [ModRM] [SIB] [displacement 0/1/2/4] [immediate 0/1/2/4/8]
```
- Maximum length: **15 bytes**. Beyond this limit the CPU generates #GP.
- With VEX the REX prefix is not allowed: its bits are contained in the VEX.

## Legacy prefixes ⚠️
| Group | Byte | Meaning |
|---|---|---|
| 1 | `F0` | LOCK |
| 1 | `F2` / `F3` | REPNE / REP(E). In SSE instructions they also act as a mandatory prefix. |
| 2 | `2E 36 3E 26` | CS/SS/DS/ES override: ignored in 64-bit mode. `2E`/`3E` are also branch hints. |
| 2 | `64` / `65` | **FS** / **GS** override: relevant (TLS). |
| 3 | `66` | Operand size → 16 bits. In SSE instructions it also acts as a mandatory prefix. |
| 4 | `67` | Address size → 32 bits |

Edge cases to verify: prefix order, repeated prefixes, several prefixes from the same group.

## REX (`0x40`–`0x4F`) ⚠️
Format `0100WRXB`:
- **W:** 64-bit operand size.
- **R:** extends `ModRM.reg`.
- **X:** extends `SIB.index`.
- **B:** extends `ModRM.rm`, `SIB.base` or the register encoded in the opcode.

Rules:
- REX must **immediately** precede the opcode. If another prefix appears between REX and the opcode, the REX is ignored.
- If there is a REX (even `0x40`), 8-bit registers 4–7 become `SPL/BPL/SIL/DIL` instead of `AH/CH/DH/BH`.

## Opcode maps ⚠️
1-byte map, `0F xx`, `0F 38 xx`, `0F 3A xx`.
In SSE instructions the mandatory prefix (`66`/`F2`/`F3`, or none) selects different instructions for the same opcode.

## ModRM ⚠️
Fields: `mod` (bits 7–6), `reg` (bits 5–3), `rm` (bits 2–0).
- `mod=11`: `rm` is a register.
- `rm=100` with `mod≠11`: a **SIB** byte follows. This also holds with REX.B=1 (r12 always requires the SIB).
- `mod=00` and `rm=101`: **RIP-relative** addressing with disp32 (in 64-bit mode). This also holds with REX.B=1 (r13 with `mod=00` requires an explicit disp).
- `mod=01` / `mod=10`: 8-bit (sign-extended) / 32-bit displacement.
- In some opcodes `reg` does not indicate a register but an opcode extension (`/digit`, for example the `80`–`83` group).

## SIB ⚠️
Fields: `scale` (bits 7–6, ×1/2/4/8), `index` (bits 5–3), `base` (bits 2–0).
- `index=100` without REX.X: **no index**. With REX.X=1 the index is r12, which is valid.
- `base=101` with `mod=00`: no base, a disp32 follows. This also holds with REX.B=1.

## Operand and address size ⚠️
- Default in 64-bit mode: **32-bit** operands, 64-bit addresses.
- REX.W → 64 bits. `66` → 16 bits. If both are present, REX.W takes precedence.
- Some instructions have 64-bit operands by default: `push`/`pop`, near `call`/`jmp`, `ret` and others (full list to be taken from the SDM).
- **Writing a 32-bit register zeroes the upper 32 bits** of the 64-bit register. 8- and 16-bit writes instead preserve the rest of the register.
- Immediates: at most 32 bits, sign-extended to 64. The exception is `mov r64, imm64` (`REX.W B8+r`).

## VEX ⚠️
- `C5` (2 bytes) and `C4` (3 bytes). In 64-bit mode they are always VEX, because `LES`/`LDS` do not exist.
- The `R`, `X`, `B` and `vvvv` fields are stored **inverted**.
- `L` indicates the vector length (128/256).
- `pp` encodes the implicit prefix (none/`66`/`F3`/`F2`).
- `mmmmm` indicates the map (`0F`/`0F38`/`0F3A`).
- EVEX (`62`, AVX-512): out of scope (`vision.md`).

## Opcodes invalid in 64-bit mode ⚠️
Various 1-byte legacy opcodes generate #UD, for example push/pop of segment registers, BCD instructions like `DAA`/`AAA`, `PUSHA`/`POPA`, `INTO`. The full list must be taken from the SDM tables (Vol. 2, appendix A).
In armor: `DecodeError::Invalid`, which becomes SIGILL for the guest (above, "Implemented": list of invalid opcodes).
