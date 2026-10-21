# AVX and AVX2 (x86-64-v3)

**Roadmap:** M11, item "AVX and AVX2 (x86-64-v3)". **Code:** `crates/armor-cpu/src/cpuid.rs` (`Features`), `crates/armor-cpu/src/xsave.rs`, `crates/armor-cpu/src/bmi.rs`, `crates/armor-cpu/src/avx.rs`, `crates/armor-cpu/src/avx_ops.rs`, `crates/armor-linux/src/signal/xstate.rs`, the decoder in `crates/armor-x86` (`vex.rs`). **Tests:** `tests/guest/asm/xsave/`, `tests/guest/asm/bmi/`, `tests/guest/asm/avx/`, `tests/guest/asm/fma/`, `tests/guest/asm/f16c/`, `faults/xgetbv_undeclared`, `faults/andn_undeclared`, `faults/movbe_undeclared`, `faults/vex_undeclared`, `c/signal_xstate`, `c/bmi_compiled`, unit tests in `armor-cpu`, `armor-x86` and `armor-linux`.

Every statement is marked ✅ (verified, with how) or ⚠️ (to verify, with how).

## Status
- ✅ By default armor's CPUID is unchanged: it declares x86-64-v2 and nothing of v3 (`basic/cpuid` in `difftest`, identical to qemu's `qemu64` model, `tech/cpuid.md`).
- ✅ Implemented, declared only on request (`ARMOR_CPU_FEATURES`, below):
  - XSAVE: `xgetbv`, `xsave`, `xrstor`, CPUID leaf 0xD with XCR0 = x87 | SSE, signal frames in the XSAVE format (`tech/signals.md`, "The FPU state with XSAVE"), and the upper halves of the ymm registers in the CPU state (`Cpu::ymm_hi`), which no instruction changes yet;
  - the VEX prefix in the decoder, with the instructions of BMI1 and BMI2; LZCNT and the BMI1 `tzcnt`; MOVBE (section "BMI1, BMI2, LZCNT, MOVBE");
  - AVX and AVX2, in part: the VEX.128 and VEX.256 forms of the SSE instructions armor implements (the 256-bit integer ones are AVX2's), with every VEX compare predicate, `vzeroupper`, `vzeroall`, `vldmxcsr`, `vstmxcsr`, XCR0 with the AVX component (sections "AVX: the VEX.128 forms" and "The VEX.256 forms");
  - the instructions AVX and AVX2 add: broadcasts, `vinsert`/`vextract` `f128`/`i128`, `vperm2f128`/`vperm2i128`, `vpermq`, `vpermpd`, `vpermd`, `vpermps`, `vpermilps`/`pd`, `vblendvps`/`pd`, `vpblendvb`, `vpblendd`, the variable shifts, the masked moves, `vtestps`/`pd` (section "The instructions AVX and AVX2 add").
  - FMA3 and F16C (section "FMA and F16C").
  - the AVX2 gathers (section "The instructions AVX and AVX2 add").
- ✅ So every instruction of x86-64-v3 runs in the interpreter when declared: with `ARMOR_CPU_FEATURES=+xsave,+avx,+avx2,+fma,+f16c,+bmi1,+bmi2,+abm,+movbe`, the guest root's `ld.so --help` under armor lists `x86-64-v3 (supported, searched)` (without: only v2). The full `difftest` with that set: 502 of 502 cases identical to qemu with the same features, interpreter and JIT (2026-10-22; the gathers' test came after it).
- Not yet: declaring v3 by default (pending: the JIT translation of the hot VEX forms, measured on real programs, and the startup cost of `xsave` in the interpreter), and the JIT translation of all of the above (the interpreter runs them). The order of the work is in "Recommended order" at the end.
- The full x86-64-v3 set will be declared by default only when all of it works (psABI: v3 is AVX, AVX2, BMI1, BMI2, F16C, FMA, LZCNT, MOVBE and OSXSAVE together, section 2).

## Declaring features on request: `ARMOR_CPU_FEATURES`
- ✅ `ARMOR_CPU_FEATURES=+xsave armor program` declares XSAVE in CPUID, on top of the default model; the accepted names are `xsave`, `avx` (only with `xsave`), `avx2`, `fma` and `f16c` (only with `avx`), `bmi1`, `bmi2`, `abm` (LZCNT) and `movbe` (`crates/armor/src/main.rs`; `Features::parse`). The names are qemu's (`qemu-x86_64 -cpu help`), separated by commas, with or without `+`, so the same list extends qemu's model: `-cpu qemu64,-svm,+ssse3,+sse4.1,+sse4.2,+popcnt,+xsave`.
- ✅ Only features armor implements are accepted; any other name stops armor with an error that lists the accepted ones (unit test `features_parse_like_qemu_additions`).
- ✅ An instruction of such a feature, when the feature is not declared, raises #UD (SIGILL), as on a CPU without it: the decoder decodes it, and the interpreter refuses it before executing it (`declared` in `crates/armor-cpu/src/lib.rs`). `faults/xgetbv_undeclared`, `faults/andn_undeclared`, `faults/movbe_undeclared` and `faults/vex_undeclared` are identical to qemu without the features (✅ `difftest`, 2026-10-21). The exception is F3 0F BC/BD, which runs as `bsf`/`bsr` without BMI1/LZCNT (below).
- ✅ The JIT's cache of translations (ADR-0048) keys a translation with the declared features too, so a run with other features never reuses it; with none declared the key is the same as before (`Jit::cache_key`).
- ✅ `cargo xtask difftest` and `cargo xtask run` pass the variable from their own environment to both sides: qemu's `-cpu` model (and `QEMU_CPU` for the children it execs) and armor's environment (`cpu_features`, `xtask/src/main.rs`). Cases that test such instructions declare what they need in `CASE_FEATURES` (`xtask/src/difftest.rs`): the cases under `xsave/` and `*/signal_xstate` run with `+xsave` always, those under `avx/` with `+xsave,+avx,+avx2`, those under `avx1/` with `+xsave,+avx`, those under `fma/` with `+xsave,+avx,+fma`, those under `f16c/` with `+xsave,+avx,+f16c`, those under `bmi/` and `*/bmi_compiled` with `+bmi1,+bmi2,+abm,+movbe`, so they execute rather than raise #UD under both.
- ⚠️ For i386 programs the features are added to `qemu32`, whose highest basic leaf is 4: under qemu-i386 leaf 0xD gives leaf 4's values (✅ measured with a CPUID program under `qemu-i386 -cpu qemu32,...,+xsave`), so CPUID differs there from armor's, which uses the same model in both modes. No i386 case compares CPUID.

## XSAVE
### CPUID and XCR0
✅ Measured with `basic/cpuid` and a small CPUID program under `qemu-x86_64 10.0.13 -cpu qemu64,-svm,+ssse3,+sse4.1,+sse4.2,+popcnt,+xsave`, and reproduced by armor (`cpuid.rs`, unit test `xsave_declares_x87_and_sse_in_leaf_0xd`; `xsave/xsave` dumps the same leaves under both):
- leaf 1 ecx = 0x8c982201: XSAVE (bit 26) and OSXSAVE (bit 27) added;
- leaf 0xD subleaf 0 = (3, 0x240, 0x240, 0): x87 and SSE, 576 bytes (512 legacy + 64 header); subleaf 1 = (0, 0x240, 0, 0): no XSAVEOPT, XSAVEC, XGETBV with ecx = 1 or XSAVES; subleaves 2–63 zero;
- `xgetbv` with ecx = 0 gives XCR0 = 3.
- ✅ A leaf past the highest basic leaf (0xd) or extended leaf (0x8000_000a) gives leaf 0xD, with the subleaf in ecx, as on Intel CPUs: leaves 0xe, 0x20, 0x3fff_ffff, 0x4000_0011, 0x5000_0000, 0x8000_000b, 0x8000_0020, 0xc000_0000 and 0xffff_ffff under qemu with `+xsave`. Before XSAVE, leaf 0xD was zero, so those leaves were zero too. armor keeps its empty hypervisor leaves 0x4000_0000–0x4000_0010 (`tech/cpuid.md`).
- ✅ With XCR0 = 3, glibc 2.41's dynamic loader takes `_dl_runtime_resolve_xsave`: under qemu with `+xsave`, `perl -e 'print "hi\n"'` executes `xgetbv`, `xsave 0x40(%rsp)` and `xrstor 0x40(%rsp)`, with the mask edx:eax = 0x800ee (no x87 bit) and the header zeroed first (`-d in_asm` log).

### The instructions
- ✅ Decoded (`decoder.rs`): `xgetbv` (0F 01 D0), `xsetbv` (0F 01 D1), `xsave` (0F AE /4) and `xrstor` (0F AE /5) with memory and no 66, F2 or F3 prefix, with or without REX.W (`xsave64`, `xrstor64`). Bytes and mnemonics checked with `x86_64-linux-gnu-objdump -D -b binary` (unit test in `armor-x86`). Not decoded: `xsaveopt` (0F AE /6), `xsavec` and `xsaves` (0F C7 /4, /5), `xrstors` (0F C7 /3); the length decoder classifies all of them as `Extension::Xsave` (`length.rs`), so without XSAVE declared they raise #UD, and with it they stop armor ("not supported yet") ⚠️: CPUID does not declare them, so a program that checks finds them absent.
- ✅ `xgetbv`: XCR0 in edx:eax for ecx = 0; #GP for any other ecx (`xsave/xgetbv_xinuse`). `xsetbv`: #GP, privileged (`xsave/xsetbv`).
- ✅ `xsave` (standard format, `xsave.rs`), with RFBM = XCR0 & edx:eax: the x87 component writes bytes 0..24 and 32..160 of the legacy region, SSE writes MXCSR, MXCSR_MASK and the xmm registers (24..32, 160..416), and MXCSR is also written for the AVX component; XSTATE_BV (header bytes 0..8) gets the RFBM bits set and keeps the others; no other byte is written. `xsave/xsave` compares the whole 576-byte area, prefilled with a pattern, under the masks 0, x87, SSE, AVX alone (not in XCR0: as 0), all, the loader's 0x800ee, and with REX.W: identical to qemu (✅ `difftest`, interpreter and JIT, 2026-10-21; the reserved bytes after each x87 register and FOP are cleared before the dump, as in `sse/state`, because armor writes zeros there like the Xeon X5675 and qemu leaves them).
- ✅ armor does not track which components are in their initial state (XINUSE): every saved component is marked in use in XSTATE_BV, as qemu does. ⚠️ The Intel SDM allows it (vol. 1, 13.6: a processor may mark a component in use when it is in its initial state); hardware usually clears the bit of a component in its initial state. How to verify: `xsave/xsave` on an x86-64 machine with XSAVE (the hardware oracle, a Xeon X5675, has none: `testing.md`).
- ✅ `xrstor`: for each RFBM component, from the area if its XSTATE_BV bit is set, otherwise its initial state (x87 as after `fninit` with the registers zeroed, xmm zeroed); components outside RFBM unchanged; MXCSR from the area whenever SSE or AVX is requested, even with their XSTATE_BV bits clear. `xsave/xsave` checks a full restore, XSTATE_BV 0 (both initial, MXCSR 0x1f81 from the area), XSTATE_BV SSE only, RFBM x87 only, RFBM 0 and `xrstor64`: identical to qemu (✅ `difftest`, 2026-10-21). ⚠️ The MXCSR rule against hardware: as above.
- ✅ #GP, with nothing changed: an area not aligned to 64 bytes (`xsave/misaligned`), an XSTATE_BV bit outside XCR0 (`xsave/bad_header`), XCOMP_BV not zero (`xsave/bad_xcomp_bv`: the compacted format needs XSAVEC), a reserved header byte among bytes 16..24 (`xsave/bad_reserved`): identical to qemu. A reserved MXCSR bit: #GP in armor as the SDM says, accepted by qemu as in `ldmxcsr` (unit test `xrstor_with_a_reserved_mxcsr_bit_is_a_general_protection_fault`).
- ✅ The JIT does not translate these instructions: the interpreter executes them (`translator::supported` returns false for them). With XSAVE declared, every lazily bound call of a dynamic program therefore leaves translated code twice (`xsave`, `xrstor`). ⚠️ The cost: measure `python3 -c pass` and `perl -e 1` with and without `ARMOR_CPU_FEATURES=+xsave`.

### The upper ymm halves
- ✅ `Cpu::ymm_hi: [u128; 16]` holds bits 255:128 of ymm0–ymm15 next to `xmm` (the low halves), as box64 and FEX keep them (section 3). Legacy SSE instructions, `fxsave` and `fxrstor` do not touch them (unit test `fxrstor_leaves_the_upper_ymm_halves`); a signal handler starts with them zeroed (`Cpu::reset_fpu`); `xsave`/`xrstor` handle them as the AVX component, which XCR0 does not enable until AVX is declared.
- ✅ A VEX.128 instruction zeroes them while a legacy SSE one keeps them (Intel SDM vol. 1, 14.1.2): `avx/vex128`, identical to qemu.

## BMI1, BMI2, LZCNT, MOVBE
### CPUID
✅ Measured with a CPUID program under `qemu-x86_64 10.0.13 -cpu qemu64,...,+xsave,+FEATURE` and reproduced by armor (`cpuid.rs`, unit test `bmi_lzcnt_and_movbe_bits_as_qemu`; `basic/cpuid` with all of them in `ARMOR_CPU_FEATURES`):
- `bmi1`: leaf 7 subleaf 0 ebx bit 3 (0x8); `bmi2`: ebx bit 8 (0x100); leaf 7 eax (the highest subleaf) stays 0, subleaf 1 is zero;
- `abm`: leaf 0x8000_0001 ecx bit 5 (0x21 with LAHF-LM);
- `movbe`: leaf 1 ecx bit 22 (0x80d82201).

### The VEX prefix
- ✅ `Decoder::vex_prefix` (`decoder.rs`) reads C5 (R̄, vvvv̄, L, pp; map 0F, W 0) and C4 (R̄ X̄ B̄, map, W, vvvv̄, L, pp); VEX.R, X, B and W act as REX's, vvvv names the extra register, pp the mandatory prefix (none, 66, F3, F2). #UD for a map other than 1–3 and for a 66, F2, F3, LOCK or REX prefix before it. In compatibility mode C4/C5 are VEX only when the next byte has mod = 11 (else `les`/`lds`, as before), and VEX.B and the top bit of vvvv are ignored. Bytes from the GNU assembler and objdump (unit test `bmi_and_movbe`).
- ✅ `vex.rs` decodes map 2 F2, F3, F5–F7 and map 3 F0: the BMI instructions (`Op::Bmi`, `BmiOp`); VEX.L = 1, a reserved ModRM.reg in group 17 (F3), or VEX.vvvv other than 1111 in `rorx`: #UD. Operand size 32 bits, 64 with VEX.W in 64-bit mode (32 in compatibility mode). Every other VEX instruction is still `Unsupported("AVX (VEX)")`, which raises #UD while AVX is not declared.

### The instructions (interpreter, `bmi.rs`)
- ✅ BMI1 `andn`, `bextr`, `blsi`, `blsmsk`, `blsr`; BMI2 `bzhi`, `mulx`, `pdep`, `pext`, `rorx`, `sarx`, `shlx`, `shrx`, 32 and 64 bits. `bmi/bmi` runs each on edge values (zero, all ones, counts and indexes at and past the operand size, `bextr` lengths 0 and ≥ 64, `mulx` with the same register for both halves), with memory operands too, and compares the results and the defined flags with qemu: identical with the interpreter, the JIT and lockstep (2026-10-21). The flags compared, as the Intel SDM defines them (⚠️ recalled, not re-read: SF ZF CF OF for `andn`, `blsi`, `blsmsk`, `blsr`, `bzhi`; ZF CF OF for `bextr`; none changed by `mulx`, `pdep`, `pext`, `rorx`, `sarx`, `shlx`, `shrx`, which the test checks with every flag set before). The undefined ones get values chosen as for the other instructions (`tech/x86-64-flags.md`).
- ✅ `tzcnt` (F3 0F BC) counts the trailing zeros when BMI1 is declared, `lzcnt` (F3 0F BD) the leading zeros when LZCNT is: the operand size for a zero source, CF if the source is 0, ZF if the count is 0, the other flags unchanged (undefined). Without the feature they run as `bsf`/`bsr`, as on a CPU without it and as before (`alu/bits`, default model). In `bmi/bmi` on 16, 32 and 64 bits.
- ✅ `movbe` (0F 38 F0/F1, 66 for 16 bits; memory operand only, a register is #UD): loads and stores of 16, 32 and 64 bits in `bmi/bmi`.
- ✅ `c/bmi_compiled` (and `c-pie/`, `c-dyn/`): C functions built with `target("bmi,bmi2,lzcnt,movbe")`, as `-march=x86-64-v3` builds them (gcc 14 emits `andn`, `bextr`, `blsi`, `blsmsk`, `bzhi`, `lzcnt`, `movbe`, `mulx`, `pdep`, `pext`, `sarx`, `shlx`, `shrx`, `tzcnt` in the program, objdump), identical to qemu with the four features (2026-10-21).
- ✅ The JIT: the BMI instructions and `movbe` go to the interpreter (`translator::supported`); `tzcnt`/`lzcnt` are translated as before, as `bsf`/`bsr`, only while BMI1/LZCNT is not declared, and go to the interpreter when it is (`armor_cpu::counts_zeros`). The declared features are part of the cache key (above), so a translation made without them is never reused with them. ⚠️ Their translation (`clz`, `rbit`, `lsl`/`lsr`/`asr` by a register, `bic`, `rev`): later, measured on programs that use them.

## AVX: the VEX.128 forms
### CPUID and XCR0
✅ `qemu64,...,+xsave,+avx` (CPUID program; unit test `avx_extends_xcr0_and_leaf_0xd`; `basic/cpuid` and `xsave/xsave` identical with `ARMOR_CPU_FEATURES=+xsave,+avx`): leaf 1 ecx bit 28 (0x9c982201); leaf 0xD subleaf 0 (7, 0x340, 0x340, 0), subleaf 1 (0, 0x340, 0, 0), subleaf 2 (0x100, 0x240, 0, 0): the AVX component, 256 bytes at 576; XCR0 = 7. The XSAVE area is then 832 bytes, and so is the XSAVE image of a signal frame (836 with MAGIC2; `c/signal_xstate` identical with `+xsave,+avx`). `avx` without `xsave` is refused: qemu then raises #UD on `xgetbv`, so no program could enable the state.

### Decoding (`vex.rs`)
- ✅ The VEX forms are decoded by the legacy tables (`sse.rs`, `maps.rs`), with VEX.pp as the mandatory prefix (`Decoder::sse_prefix`), then given the role of VEX.vvvv (`Op::Vex`): the first source (NDS) for the operations that combine two sources and for the merging moves (`vmovss`/`vmovsd` between registers, `vmovlps`/`vmovhps`/… loads, `vmovhlps`, `vpinsr*`, `vinsertps`, the scalar arithmetic, conversions and rounding); none (VEX.vvvv must be 1111, else #UD) for the moves, `vpshufd`, the packed `vsqrt`/`vrcp`/`vrsqrt`/`vround`, the conversions, `vptest`, `vpmovsx`/`zx`, `vpabs`, `vphminposuw`, `vpcmp*str*`, the extracts and the mask moves; the destination (NDD) for the shifts by an immediate, whose ModRM.rm is the source. The MMX forms, `cvtpi2ps` and the like, and the legacy `pblendvb`/`blendvps`/`blendvpd` opcodes (whose VEX forms are other instructions with four operands) are #UD. Unit test `vex_sse_forms`, bytes from the GNU assembler and objdump.
- ✅ The scalar operations ignore VEX.L (LIG); for the others VEX.L = 1 is the 256-bit form (`wide`), not executed yet, or #UD for those that have none (`has_256_bit_form`: the inserts and extracts, `vmovd`/`vmovq`, the merging moves, `vphminposuw`, `vdppd`, the string compares, …). A 256-bit integer form is marked `avx2` (`integer`): AVX2 adds those, and they raise #UD while AVX2 is not declared (✅ `avx1/avx2_undeclared` with `+xsave,+avx`: `vaddps ymm` and `vxorps ymm` run, `vpaddd ymm` raises SIGILL, as under qemu).
- ✅ `vldmxcsr` and `vstmxcsr` (VEX 0F AE /2, /3; `Op::VexMxcsr`): glibc's libm runs `vstmxcsr` in its AVX paths, which it takes with AVX declared (✅ `interpreters/perl-math` and `python-math-libm` stopped armor on it in the full `difftest` with `+xsave,+avx`; identical to qemu since).
- ✅ The compare predicates: 0–7 in the legacy forms, 0–31 in the VEX forms (`compare_predicate`): 8–15 give the opposite result for unordered operands, 16–31 the opposite signaling of 0–15 (`float::compare_mask`; ⚠️ the table as recalled from the SDM's CMPPS page). `avx/vex128` compares all 32 on a NaN, packed and scalar, and MXCSR after them, with qemu.

### Execution (`avx.rs`)
- ✅ A VEX.128 operation `dst = op(src1, src)` runs as the legacy operation on the architectural registers: dst is set to src1 first, then the legacy `op dst, src`; when src is dst itself and src1 another register, src's value goes into a register borrowed for the instruction and given back. On success bits 255:128 of the destination are zeroed; on a fault the destination is put back, and nothing else changed. Stores and writes to general registers leave the ymm registers as they are; `vpcmpistrm`/`vpcmpestrm` zero the upper half of xmm0, which they write.
- ✅ Memory operands need no alignment (`Cpu::vex_unaligned` while the instruction runs), except for the moves that require it: `vmovaps`, `vmovapd`, `vmovdqa`, the non-temporal stores, `vmovntdqa` (`avx/vmovaps_misaligned`: #GP, while `vaddps` at the same address runs).
- ✅ `vzeroupper` zeroes the upper halves, `vzeroall` the whole registers.
- ✅ `avx/vex128` (asm, `difftest` with `+xsave,+avx`): about 110 instruction forms (and the 32 predicates of `vcmpps` and `vcmpsd`) across arithmetic, logic, integer lanes, shuffles, blends, inserts and extracts, moves (loads, merges, stores), conversions, rounding, `vdpps`, `vmpsadbw`, compares, `vptest`, string compares, with destinations distinct from both sources, equal to the first and equal to the second, and unaligned memory; the upper ymm halves are set with `xrstor` and read with `xsave` (AVX component) after each group: legacy SSE keeps them, VEX.128 zeroes those of the destination. Identical to qemu with the interpreter, the JIT and lockstep (2026-10-22). Float operands there are never denormal, since qemu does not raise DE (`testing.md`).
- ⚠️ MXCSR and the AVX component alone: armor's `xsave` writes MXCSR, and `xrstor` loads it, when RFBM has SSE or AVX, as the SDM says (vol. 1, 13.7 and 13.8.1, as recalled; Linux's `copy_xstate_to_uabi_buf` also ties MXCSR to SSE or YMM, as recalled); qemu 10.0.13 does so only with SSE (✅ measured: `xsave` with edx:eax = 4 leaves bytes 24..32, `xrstor` with 4 leaves MXCSR). `avx/vex128` and `xsave/xsave` keep MXCSR out of those cases. How to verify: an x86-64 machine with AVX.
- ✅ The JIT leaves `Op::Vex` and `Op::Vzero` to the interpreter. glibc does not choose its AVX variants with AVX alone (they need AVX2), so a dynamic program's VEX code is the loader's `xsave`/`xrstor` with the AVX component and libm's AVX paths. ✅ The full `difftest` with `ARMOR_CPU_FEATURES=+xsave,+avx,+bmi1,+bmi2,+abm,+movbe` (2026-10-22): 491 of 496 cases identical to qemu with the same features; the others were `c*/undeclared_extensions` (3), which checks that VEX instructions raise #UD and now meets a 256-bit one, and `interpreters/perl-math` and `python-math-libm`, which stopped on libm's `vstmxcsr`, decoded since and identical to qemu. With no feature declared: 496 of 496.

## The VEX.256 forms
- ✅ CPUID: `avx2` sets leaf 7 ebx bit 5 (0x20, ✅ `qemu64,...,+xsave,+avx,+avx2`); armor accepts it only with `avx`.
- ✅ Execution (`Cpu::vex256`, `avx.rs`): the legacy operation runs once on the low halves (`xmm`) and once on the high ones (`ymm_hi`, or the 16 bytes after a memory operand's first 16), each through `half`, which stages its inputs in the destination and a borrowed register and leaves every register as it was; the two results are written only when both succeeded. Adjusted: the immediates with a field per lane (`vshufpd` bits 3:2, `vblendps` 7:4, `vblendpd` 3:2, `vmpsadbw` 5:3 for the high lane; `vpblendw`, `vshufps`, `vpshufd`, `vpalignr` use the same immediate); the shifts by an xmm count (the same count for both halves); the conversions from xmm to ymm (`vcvtps2pd`, `vcvtdq2pd`: the high half from bits 127:64 of the source) and from ymm to xmm (`vcvtpd2ps`, `vcvt(t)pd2dq`: 64 bits from each half, the upper ymm half zeroed); the widening moves (`vpmovsx`/`zx`: the source's next lanes); the masks (`vmovmskps`, `vmovmskpd`, `vpmovmskb`: the high half's bits above the low half's) and `vptest` (ZF and CF over 256 bits); the 256-bit stores (two 16-byte stores, the low one first ⚠️: a fault on the high half leaves the low half written; how to verify: a store across the end of a mapping on hardware). The aligned 256-bit moves need 32-byte alignment (`avx/vmovaps256_misaligned`: #GP for an address aligned to 16).
- ✅ `avx/vex256` (asm, `difftest` with `+xsave,+avx,+avx2`): float arithmetic, logic and compares; destinations equal to a source; the shuffles, blends, `vdpps`, `vroundps`, `vmovddup` (register and memory), `vmovshdup`; every conversion between ymm and xmm, with memory; the masks and `vptest`; AVX2 integer lanes, `vpshufb`, `vpalignr`, `vpshufd`, `vpshufhw`, unpacks, packs, `vpblendw`, `vmpsadbw`, shifts by an immediate, by an xmm register, the byte shifts; the widening moves; aligned, unaligned and non-temporal moves; `vzeroupper` after. Identical to qemu with the interpreter, the JIT and lockstep (2026-10-22).
- ⚠️ An unmasked SIMD floating-point exception in the high half: the low half's flags are already in MXCSR when #XM is raised; hardware checks the whole register first (`floating-point.md`). How to verify: `c/sse_exceptions` with a 256-bit operation on the hardware oracle, which has no AVX.

## The instructions AVX and AVX2 add
- ✅ Decoded in `vex.rs` (`new_instruction`, `Op::Avx`, `AvxOp`): map 2 (VEX.66) 0C/0D `vpermilps`/`pd` by a vector, 0E/0F `vtestps`/`pd`, 16/36 `vpermps`/`vpermd`, 18/19/1A `vbroadcastss`/`sd`/`f128`, 2C–2F `vmaskmovps`/`pd`, 45–47 `vpsrlv`/`vpsrav`/`vpsllv`, 58/59/5A/78/79 `vpbroadcastd`/`q`, `vbroadcasti128`, `vpbroadcastb`/`w`, 8C/8E `vpmaskmovd`/`q`; map 3 00/01 `vpermq`/`vpermpd` (VEX.W1), 02 `vpblendd`, 04/05 `vpermilps`/`pd` by an immediate, 06/46 `vperm2f128`/`i128`, 18/38 `vinsertf128`/`i128`, 19/39 `vextractf128`/`i128`, 4A–4C `vblendvps`/`pd`, `vpblendvb` (the fourth register in imm8[7:4]). A VEX.W, VEX.L or operand form an instruction does not have is #UD (`vpermq` with W0 or L0, a register for `vbroadcastf128` or a masked move). AVX2 marks: `vbroadcastss`/`sd` from a register, `vpblendvb` on 256 bits, and the integer ones. Unit test `avx_new_instructions`, bytes from the GNU assembler and objdump.
- ✅ The gathers (map 2 90–93: `vpgatherdd`/`dq`/`qd`/`qq`, `vgatherdps`/`dpd`/`qps`/`qpd`; `AvxOp::Gather`): VSIB, a SIB byte whose index is a vector register (VEX.X its top bit, index 100 is xmm4, not "none"); #UD without SIB or when the destination, the mask and the index are not three different registers (unit test). Each lane whose mask lane has its top bit set is loaded from base + displacement + index × scale (dword indices sign-extended), its mask lane cleared; masked-off lanes keep the destination's value; at the end the whole mask register is zero. On a fault the lanes already loaded and their cleared mask lanes stay, rip stays on the instruction, as on hardware where a gather restarts from its mask (unit test `a_gather_fault_keeps_the_lanes_done`). `avx/gather`: the eight forms, 128 and 256 bits, dword and qword indices (negative ones too), scales 1–8, full and partial masks, identical to qemu (2026-10-22).
- ✅ Executed in `avx_ops.rs` on whole 256-bit vectors (bytes): the 128-bit forms zero the high half of the destination; memory needs no alignment; the masked moves read or write only the enabled lanes, so a disabled lane on an unmapped page does not fault (unit test `masked_off_lanes_are_not_accessed`; ⚠️ a masked store faulting on a later lane leaves the earlier ones written).
- ✅ `avx/new` (asm, `difftest` with `+xsave,+avx,+avx2`): each instruction in its 128- and 256-bit forms where it has both, with registers and memory, the per-lane immediates, the zeroing bits of `vperm2f128`, a destination equal to a source, shifts by counts at and past the lane size, and the flags of `vtestps`/`vtestpd`. Identical to qemu with the interpreter and the JIT (2026-10-22).
- ⚠️ qemu 10.0.13 differs: its `vmaskmovps` with a disabled lane past the end of a mapping faults (✅ measured: SIGSEGV under `qemu-x86_64 -cpu ...,+avx,+avx2`), where the SDM says masked-off elements cause no fault; armor follows the SDM. Not in `avx/new` for that reason.

## FMA and F16C
- ✅ CPUID: `fma` sets leaf 1 ecx bit 12 (0x9c983201 with AVX), `f16c` bit 29 (0xbc982201), as `qemu64,...,+xsave,+avx,+fma` and `+f16c` report (CPUID program; unit test `avx_extends_xcr0_and_leaf_0xd`).
- ✅ Decoding (`vex.rs`): FMA3 is map 2 (VEX.66) 96–9F, A6–AF, B6–BF (`AvxOp::Fma`): the row gives the order (132: `dst * src + src1`; 213: `src1 * dst + src`; 231: `src1 * src + dst`), the low nibble the signs (6 `vfmaddsub`, 7 `vfmsubadd`, 8/9 `vfmadd`, A/B `vfmsub`, C/D `vfnmadd`, E/F `vfnmsub`; the odd ones from 9 are scalar, which ignore VEX.L), VEX.W double precision. F16C: map 2 13 `vcvtph2ps`, map 3 1D `vcvtps2ph` (VEX.W0). Unit test `avx_new_instructions`.
- ✅ The fused multiply-add (`f80::fma`): the exact 128-bit product of the significands, the addend aligned to it with a sticky bit, one rounding by `pack`, so every rounding mode, FTZ and the flags follow the SSE rules of `float.rs`; 0 × ∞ and ∞ − ∞ are invalid (indefinite, IE); an exact zero sum is +0, −0 when rounding down. The negations of `vfnm*` and `vfmsub` apply to the product and the addend after the NaN check (`float::fma`).
- ⚠️ NaN operands: the result is the first NaN among the multiplied operands and then the addend, in that order (their roles, not their places in the instruction), made quiet, IE if any is signalling. qemu agrees (`fma/fma`, where the order changes which NaN comes out between `vfmadd132`, `213` and `231`); the SDM's rule for FMA was not re-read. How to verify: on an x86-64 machine with FMA.
- ✅ `fma/fma` (asm, `difftest` with `+xsave,+avx,+fma`): every mnemonic of the three orders and six sign kinds, packed (128 and 256 bits) and scalar, single and double, on operands whose fused result differs from a multiplication and an addition rounded apart (`(1 + 2^-27)^2 - (1 + 2^-26) = 2^-54` and its single-precision analogue), NaNs (signalling and quiet) in each operand, 0 × ∞, ∞ − ∞, a memory source, the four rounding modes and FTZ with tiny results, and MXCSR after each group. Identical to qemu with the interpreter and the JIT (2026-10-22).
- ✅ F16C (`float::half_to_single`, `float::single_to_half`, `f80::to_half`): `vcvtph2ps` is exact (a denormal half becomes a normal single; a signalling NaN is made quiet with IE); `vcvtps2ph` rounds by imm8[1:0], or by MXCSR with imm8[2], with IE, DE (unless DAZ), OE, UE and PE, tininess after rounding, and FTZ not applied (⚠️ the flags rules as recalled from the SDM). `f16c/f16c`: both directions, 128 and 256 bits, registers and memory, normal, denormal, infinite and NaN halves, every rounding control on values that round, overflow and underflow, MXCSR after each: identical to qemu (2026-10-22).

## Survey (2026-10-21): which programs need it, what FEX and box64 do
Measured on the Raspberry Pi 5 (Cortex-A76, no SVE). Raw outputs of the commands below were kept outside the repository.

### Summary
- ✅ No program on this machine was found that executes VEX instructions without first checking CPUID. Every VEX instruction that could be traced sits behind a CPUID check, and every Linux application that `difftest`, `bench` and `app-check` run under armor today starts with armor's CPUID, which declares no AVX (`docs/COMPATIBILITY_MATRIX.md`). If a program ran VEX code without a check, it would stop with SIGILL (ADR-0026).
- So the need is about what comes next, not about anything on this machine: software built for x86-64-v3, which puts VEX code everywhere and checks nothing (RHEL 10 and its rebuilds; Ubuntu's opt-in `amd64v3` variant), and games that say "AVX required".
- ✅ box64 and FEX both implement AVX, AVX2, FMA, F16C, BMI1, BMI2, MOVBE and LZCNT, and on arm64 both declare them by default. Both keep the upper halves of the ymm registers in memory in their CPU state when the host has no 256-bit vectors (section 3).

**Limits of the method.**
- ✅ `armor inspect` counts instructions with a linear sweep of the executable sections, so data inside code can be counted as VEX. On Spotify's `spotify` binary, `inspect` reports "VEX 42". objdump finds only 5 real VEX instructions there (`vpmaddwd`, `vmulsd`, `vdivsd`, `vcmpsd`, `vaddsd`); the rest are `(bad)` or empty decodings (`x86_64-linux-gnu-objdump -d` on it, grep for `c4`/`c5` opcodes). The counts below are therefore upper bounds.
- ✅ The Linux apps in `app-check` are checked at level 1 only: their window appears. Paths that do real work (decoding an image, playing audio) are not run, so a VEX instruction there that has no CPUID check would not have shown up.

### 1. Which real programs on this machine contain VEX, and how they reach it
**Method.**
- ✅ `armor inspect` (`target/release/armor`, built 2026-10-21) was run on every x86-64 ELF file under these directories:
  - the guest root `target/guest/rootfs` (457 files);
  - the games root `target/guest/rootfs-games` (547);
  - the installed roots of the Linux apps in `app-check`, under `~/.local/share/armor/apps/*/root`: Spotify (587), KeePassXC (821), Sublime Text (713), XnView MP (1015);
  - the `bench` 7-Zip, `target/bench/7zip-x64` (2).
- ✅ `inspect` sorts instructions by extension with the length decoder (`crates/armor-x86/src/length.rs`, `Extension`, `vex_or_evex`). `Extension::Vex` covers AVX, AVX2, FMA, F16C, BMI1 and BMI2 together, because BMI1 and BMI2 are VEX-encoded too.
- ✅ The gating was then traced with `x86_64-linux-gnu-objdump -d` (binutils for x86-64, installed) on a few of the files.

**Guest root (Debian 13 userland).** ✅ Only 10 of the 457 ELF files contain an extension that armor does not declare (`inspect`, field `missing`):

| File | VEX | EVEX | How it is reached |
|---|---|---|---|
| `libc.so.6` | 7345 | 2970 | ✅ glibc ifunc variants, chosen from CPUID. With armor's CPUID none of them is chosen: `ld.so --help` under armor shows only `x86-64-v2 (supported, searched)` |
| `libm.so.6` | 6889 | — | ⚠️ ifunc variants (FMA/AVX2 `exp`, `log`, …). How to verify: python `math` and `sort -g` already pass `difftest` with no AVX declared, which shows that the paths they use are gated; reading the ifunc resolvers would prove it for every function |
| `libmvec.so.1` | 5265 | 4320 | ⚠️ vector math for compilers that vectorise with AVX: only reached from code that is itself AVX. How to verify: read the symbol names (`_ZGVdN4v_*` are AVX2) |
| `ld-linux-x86-64.so.2` | 132 | 40 | ✅ string functions of the loader, plus XSAVE trampolines (see below) |
| `libcrypto.so.3` | 28134 | 96530 | ⚠️ OpenSSL's assembly, selected through `OPENSSL_ia32cap_P` (filled from CPUID). How to verify: trace the dispatch in one routine, e.g. `sha256_block_data_order` |
| `libzstd.so.1.5.7` | 604 | — | ✅ only BMI2/BMI1 (`shrx` 289, `shlx` 281, `bzhi` 21, `sarx` 6, `rorx` 5, `andn` 2), with no ymm; there are 14 `cpuid` instructions in the library. ⚠️ That the BMI2 code sits behind zstd's `ZSTD_cpuid` check: the library is stripped, so the dispatch was not traced |
| `/usr/bin/wc` | 5 | — | ✅ behind `__builtin_cpu_supports`. `4f1a: cmpb $0,0x11120` → `588b: lea __cpu_model` (0x11530), `mov 0xc(%rax)`, `and $0x400` → `58af: movb $1,0x11120` → `4f36: call afa0`, the AVX2 line counter. ⚠️ That bit 0x400 is libgcc's `FEATURE_AVX2`: read libgcc's `cpuinfo.h` |
| `/usr/bin/cksum` | 211 | 57 | ✅ same pattern. `9864`/`aaad: lea __cpu_model` (0x21300), `mov 0xc(%rax)`, `not`, `test $0x80200`, and the result chooses the routine. ⚠️ The meaning of the bits (AVX and PCLMUL): as for `wc` |
| `liblzma.so.5.8.1` | — | — | ✅ only PCLMULQDQ (44), not VEX |
| `engines-3/padlock.so` | — | — | ✅ only VIA PadLock (18) |

- ✅ `python3.13` (825,225 instructions), `perl` (421,992), the Perl and Python extension modules, and the rest of coreutils contain no VEX (`inspect` on each).
- ✅ `wc` and `cksum` are `difftest` cases (`tests/guest/coreutils.txt`) and give the same output as under qemu with armor's model, which has no AVX either. That is dynamic evidence that their VEX code is gated: an unchecked VEX instruction would have raised SIGILL.

**Linux apps of `app-check` and `bench`.**

| Program | Files with VEX | Notable files (VEX count from `inspect`) | Under armor today |
|---|---|---|---|
| 7-Zip 26.04 `7zz` | 2 | ✅ `7zz` 599 (`vmovdqa`, `vpxor`, `vpaddd`, `vperm2i128`, `vinserti128`, …: AVX2 integer), `7zzs` 3651; `7zz` contains 4 `cpuid` and 1 `xgetbv` | ✅ level 3, archive identical to the native one. So its AVX2 code is gated |
| Spotify 1.3.4.258 | 15 | ✅ `libcef.so` 296,666, `libLLVM.so.19.1` 2189, `libgallium` 4643, `libjpeg` 4379, `libgnutls` 3196, `libfreeblpriv3` 3399; `spotify` itself: 5 real ones (see "Limits") | ✅ level 1 |
| KeePassXC 2.7.12 | 25 | ✅ its own `libgcrypt.so.20` 37,695, `libcrypto.so.1.1` 15,307, `libbotan-2.so.12` 9293, `libQt5Gui` 828, `libQt5Core` 33 | ✅ level 1 |
| Sublime Text 4200 | 19 | ✅ its own `libcrypto.so.1.1` 14,895, plus the Debian libraries | ✅ works on the desktop; left out of `app-check` |
| XnView MP 1.12.2 | 42 | ✅ `XnView` 27,550, `libx265` 111,018, `libaom` 126,173, `libdav1d` 57,082, `libjxl` 66,087, `libffmpeg` 66,925, `libavcodec` 53,521, `libavutil` 25,601 | ✅ level 1 |
| Games root (Mesa, audio) | 20 | ✅ the Vulkan drivers (`libvulkan_*` about 2000–3500 each), `libLLVM`, `libgallium` | — |

- ✅ In XnView's own binary the most frequent VEX mnemonics are `vshufps` 2172, `vpextrw` 1858, `vmovdqa` 1592, `vmovss` 1574, `vaddps` 1388, `vfmadd132ps` 1321, `vmulps` 1059, `vcvtph2ps` 896 and `vbroadcastf128` 570, and the binary has 17 `cpuid` (objdump). This is the whole of x86-64-v3, FMA and F16C included, not just AVX2.
- ⚠️ Whether that code is gated: XnView reaches level 1 under armor, but opening an image is not tried. How to verify: open a JPEG, a HEIC and an EXR under armor. A SIGILL on a VEX instruction would mean code with no CPUID check.
- ⚠️ Chromium (`libcef.so`) is assumed to choose its AVX2 paths at run time (Skia, libyuv, dav1d). Spotify reaching its window under armor is only partial evidence. How to verify: find in `libcef.so` the dispatch to one AVX2 routine.
- ⚠️ Mesa's llvmpipe (`libgallium`, `libLLVM`) generates code at run time for the CPU it detects. If armor declared AVX2, llvmpipe would *generate* AVX2 code. That is code no static scan can see. How to verify: with AVX declared, run `glxgears` (the `gl` tests) and count the VEX instructions that reach the translator.

**glibc and XSAVE.**
- ✅ `ld-linux-x86-64.so.2` contains two sets of trampolines with `fxsave`/`fxrstor`, `xsave`/`xrstor` and `xsavec`/`xrstor` (at 0x13371–0x13514 and 0x17f39–0x18144), and two `xgetbv` (0x163e1, 0x19c0f) (`objdump -d`).
- ⚠️ From glibc's source: the first set is `_dl_runtime_resolve_{fxsave,xsave,xsavec}`, the second `_dl_tlsdesc_dynamic_*`, and the choice depends on `xsave_state_size` computed from CPUID leaf 0xD. How to verify: read glibc 2.41 `sysdeps/x86/cpu-features.c` and `sysdeps/x86_64/dl-machine.h`.
- ✅ `perl` and `python3.13` are linked for lazy binding (no `BIND_NOW` in `readelf -d`; `ls` has it). So every first call to a library function goes through `_dl_runtime_resolve_*`. Once XSAVE is declared, `xsave`/`xrstor` sit on the hot path of every program's startup.
- ✅ Under `qemu-x86_64 -cpu max`, `perl -v` executes `xgetbv`, `xsave 0x40(%rsp)` and `xrstor 0x40(%rsp)`, and not `xsavec`, because qemu's `max` reports XSAVEC = 0 (leaf 0xD subleaf 1, eax = 5) (`-d in_asm`). The same with XCR0 = 3 only (`+xsave`, section "XSAVE" above).
- ✅ In the same run, glibc's AVX2 string functions are chosen: `vmovdqu` 88, `vzeroupper` 25, `vpcmpeqb` 23, `vpmovmskb` 19, plus BMI1 `tzcnt` (11) and `movbe` (4). The v3 variants of glibc need BMI1 and MOVBE as well as AVX2.

### 2. What requires x86-64-v3
- ✅ **Definition** (x86-64 psABI, `low-level-sys-info.tex` at https://gitlab.com/x86-psABIs/x86-64-ABI, read 2026-10-21): v3 = v2 + AVX, AVX2, BMI1, BMI2, F16C, FMA, LZCNT, MOVBE, OSXSAVE. The document says the level is available only if the features are fully enabled, including the XCR0 check through `xgetbv`.
- ✅ **armor today is exactly v2.** The guest root's `ld.so --help` lists `x86-64-v2 (supported, searched)` under armor. Under `qemu-x86_64 -cpu max` it lists `x86-64-v3 (supported, searched)` and `x86-64-v2 (supported, searched)`.
- ✅ **What "requires" means.** A distribution built for v3 compiles its code with `-march=x86-64-v3`. The compiler then uses v3 instructions in ordinary code, with no CPUID check. Checked with `x86_64-linux-gnu-gcc` 14.2.0 (Debian) `-O2 -S` on six one-line C functions:

  | C | baseline | `-march=x86-64-v3` |
  |---|---|---|
  | `a + b` (double) | `addsd` | `vaddsd` |
  | `a * b + c` | `mulsd`, `addsd` | `vfmadd132sd` |
  | `x << n` | `sall %cl` | `shlx` |
  | `__builtin_ctz` | `rep bsf` | `tzcnt` |
  | `~a & b` | `not`, `and` | `andn` |
  | `__builtin_bswap32(*p)` | `mov`, `bswap` | `movbe` |

  - No `cpuid` is emitted. Most v3 code is therefore *scalar and 128-bit VEX forms* of instructions armor already runs in their SSE form, plus BMI, MOVBE and FMA.
  - FMA contraction changes the results: one rounding instead of two.
  - ⚠️ A binary built this way by a distribution was not inspected: Debian 13 is built for the baseline. How to verify: `armor inspect` on a binary from an RHEL 10 or AlmaLinux 10 package.
- ✅ **RHEL 10**: the 10.0 release notes, chapter "Architectures" (https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/10/html/10.0_release_notes/architectures, read 2026-10-21), give the minimum as "AMD and Intel 64-bit architectures (x86-64-v3)".
  - ⚠️ That RHEL 10 does not boot on v2 CPUs, and that hypervisors must expose AVX/AVX2: stated by SUSE's documentation (e.g. https://documentation.suse.com/suma/5.0/en/docs/client-configuration/clients-rh-cdn.html), a secondary source.
  - ⚠️ CentOS Stream 10, Rocky 10 and AlmaLinux 10 (AlmaLinux also ships a v2 build): not read.
- ✅ **Ubuntu**: the `amd64v3` *variant* in 25.10 is opt-in, about 2000 source packages of `main`, enabled with an APT setting. 26.04 LTS plans to rebuild every package as a variant as well. It is not a requirement (https://discourse.ubuntu.com/t/introducing-architecture-variants-amd64v3-now-available-in-ubuntu-25-10/71312, 2025-10-30, read 2026-10-21).
- ✅ **Games**: Steam's store data for Death Stranding (app 1190460, `store.steampowered.com/api/appdetails`) says "AVX instruction set required". The Last of Us Part I (1888930) and Ratchet & Clank: Rift Apart (1895880) say nothing about AVX. This is three games only, not a survey.
  - ⚠️ The size of the set of games that need AVX. How to verify: scan the `pc_requirements` of a list of popular Steam app ids for "AVX".
  - ✅ box64's rc file lists Windows games where AVX is set on purpose: `[ds.exe]` (`system/box64.box64rc:690`) and `[pCARS2AVX.exe]` (`:1057`).
  - ✅ 64-bit Windows programs and games run in Hangover with FEX ARM64EC. 32-bit ones run with FEX WoW64 or with armor's WoW64 backend, `armor-wow64.dll` (`docs/COMPATIBILITY_MATRIX.md`; roadmap M11). FEX declares no AVX under WoW64 (`Source/Windows/Common/CPUFeatures.cpp:90-91`), so for 32-bit Windows code armor is at par with FEX today. box64's documentation says `BOX64_AVX` is "Available in WowBox64" (`docs/USAGE.md:296`).
- ✅ **Windows on Arm**: Prism supports "legacy AVX as well as AVX2 instructions" since build 27744 of the Canary channel (PCWorld, 2024-11-06, https://www.pcworld.com/article/2513178/windows-on-arm-update-could-open-the-door-to-more-gaming.html).
  - ⚠️ That BMI, FMA and F16C are also covered, enabled by default for 64-bit apps and off for 32-bit ones (Microsoft's blog https://techcommunity.microsoft.com/blog/windowsosplatform/-/4475631): the page's text did not load, only a search summary.

### 3. What box64 and FEX support
✅ Source trees read, not built: box64 `2f130fab` (2026-08-02, v0.4.4) and FEX `14c92681` (2026-10-07). Line numbers refer to these commits.

**Instructions.**

| | box64 | FEX |
|---|---|---|
| AVX, AVX2 | ✅ dynarec `src/dynarec/arm64/dynarec_arm64_avx*.c` (dispatch `dynarec_arm64_avx.c:49-71`); interpreter `src/emu/x64runavx*.c` | ✅ `FEXCore/Source/Interface/Core/X86Tables/VEXTables.cpp`: two tables, `AVX128::BaseTable` (`:16-19`, 256-bit operations as two 128-bit halves) and `AVX256::BaseTable` (`:467-470`, SVE256) |
| FMA3 | ✅ e.g. `dynarec_arm64_avx_66_0f38.c:1777`; interpreter `x64runavx660f38.c:1979` | ✅ `VEXTables.cpp:335-366` |
| F16C | ✅ `avx_66_0f38.c:359` (vcvtph2ps), `avx_66_0f3a.c:496` (vcvtps2ph) | ✅ `VEXTables.cpp:265`, `:399` |
| BMI1, BMI2 | ✅ `dynarec_arm64_avx_0f38.c:64` (andn) … `avx_f2_0f3a.c:63` (rorx); interpreter `x64runavx0f38.c:62` etc. | ✅ `OpcodeDispatcher/VEXTables.h:7-22`, installed in both tables (`VEXTables.cpp:1382`) |
| MOVBE | ✅ `dynarec_arm64_0f.c:939,952`; `x64run0f.c:686,695` | ✅ `X86Tables/H0F38Tables.cpp:106-110` |
| LZCNT/TZCNT | ✅ `dynarec_arm64_f30f.c:697/663`; `x64runf30f.c:439,477` | ✅ `OpcodeDispatcher/SecondaryTables.h:176-177` |
| AVX-512 | ✅ no. `FillVEXFromEVEX` (`src/emu/x64run_private.h:62-96`) only re-encodes as VEX the EVEX forms that behave like VEX (`l > 1` refused, no masks or broadcast) | ✅ no (`Frontend.cpp:833-835`, "EVEX unsupported") |
| AVX-VNNI | ✅ not found | ✅ yes (`VEXTables.cpp:315-318`), declared only if the host has dot products (`CPUID.cpp:654-657`) |

**What they declare in CPUID.**
- ✅ **box64**: `BOX64_AVX` defaults to **2 on arm64** (`src/include/env.h:160-161`) and 0 elsewhere. 0 declares no AVX; 1 declares AVX, BMI1, F16C and VAES; 2 adds AVX2, BMI2, FMA, ADX, VPCLMULQDQ and RDRAND (`docs/USAGE.md:294-300`; CPUID in `src/os/my_cpuid.c:119-130`, `:208-224`).
  - MOVBE (`my_cpuid.c:123`) and LZCNT (`:403`, `:441`) are always declared.
  - The option changes only what is declared: the decoder executes VEX instructions whatever its value. ✅ `grep -rn 'BOX64ENV(avx)'` in `src/` finds it only in `my_cpuid.c`, a register dump (`emu/x64emu.c:429,439`), `emu/x64test.c:130`, the SDL2 wrapper (`wrapped/wrappedsdl2.c:50,53`) and the emulated `/proc/cpuinfo` (`wrapped/wrappedlibc.c:2196-2200`). That cpuinfo prints `bmi2` with `BOX64_AVX=1`, while CPUID declares BMI2 only with 2.
  - For Steam, box64's rc file sets `BOX64_AVX=0`, with the comment "there are probably some memory barrier missing on avx opcode" (`system/box64.box64rc:380`).
- ✅ **FEX**: on arm64, `SupportsAVX = true` always (`Source/Common/HostFeatures.cpp:544`). SVE256 only chooses the backend (`:532`; `Frontend.cpp:82-87`): without SVE256, as on the Pi 5 (`/proc/cpuinfo` has no `sve`), FEX uses AVX128.
  - The config option `HostFeatures` can be set to `enableavx` or `disableavx` (`FEXCore/Source/Interface/Config/Config.json.in:45-54`).
  - AVX, AVX2, FMA, F16C, BMI1 and BMI2 are declared together or not at all, from `SupportsAVX()` (`CPUID.cpp:475-492`, `:684-689`). MOVBE (`:485`) and LZCNT (ABM, `:1058`) are always declared.
  - With AVX disabled, FEX does not decode VEX at all, BMI included (`Frontend.cpp:754-757`).
  - **Under WoW64 (32-bit Windows) FEX disables AVX** (`Source/Windows/Common/CPUFeatures.cpp:90-91`).

**Where the upper halves of the ymm registers live.**
- ✅ **box64**: `sse_regs_t ymm[16]` next to `xmm[16]` in `x64emu_t` (`src/emu/x64emu_private.h:67-68`); `ymm[]` holds only the upper 128 bits.
  - The dynarec caches them in NEON registers when needed (`NEON_CACHE_YMMW/YMMR`, `dynarec_arm64_private.h:35-36`; `ymm_get_reg` at `dynarec_arm64_helper.c:1700-1729`; `fpu_get_reg_ymm` at `:2733`), with a "zero" bit so that an upper half known to be zero is not loaded.
  - ⚠️ Read, not run: when a signal arrives inside a block, `dynarec_arm64_arch.c:357-367` reads the saved slot (`idx = (ymm->ymm_pos>>(i*4))&0xf`) and then overwrites it with a slot computed from the register number (`EMM0 + i` or `SCRATCH0 + i - 8`). Since the slots are allocated dynamically, a cached upper half may be recovered from the wrong NEON register. How to verify: a test with a signal arriving during a loop of 256-bit operations under box64.
- ✅ **FEX**: `uint64_t avx_high[16][2]` in the CPU state, "when not being emulated by SVE256" (`FEXCore/include/FEXCore/Core/CoreState.h:112-114`).
  - In AVX128 mode the low halves sit in v0–v15 and the high ones in memory, accessed through the context cache (`OpcodeDispatcher/AVX_128.cpp:114-125`; `OpcodeDispatcher.h:1931-1938`).

**XGETBV, XSAVE, CPUID leaf 0xD.**
- ✅ **box64**:
  - `xgetbv` with ecx = 0 always returns XCR0 = 0b111, even with `BOX64_AVX=0` (`dynarec_arm64_0f.c:84-91`, `x64run0f.c:120-129`).
  - Leaf 0xD always has size 832 = 512 + 64 + 256 and YMM at offset 576 (`my_cpuid.c:258-282`).
  - `xsave`/`xrstor` are implemented in standard format (`x87emu_private.c:488-585`).
  - XSAVEC is not implemented and not declared (`dynarec_arm64_0f.c:2750`).
- ✅ **FEX**:
  - `xgetbv` returns XCR0 = 0x3, or 0x7 with AVX (`CPUID.cpp:1350-1357`; `CPUID.cpp:421-424`).
  - Leaf 0xD: 832 bytes with AVX, else 0x240; subleaf 1 declares XSAVEOPT only (`CPUID.cpp:873-918`).
  - XSAVE: `OpcodeDispatcher/Vector.cpp:2996-3050`; XRSTOR: `:3183`.
  - XSAVEC/XSAVES are invalid (`SecondaryGroupTables.cpp:185-186`).
  - ⚠️ That FEX's XRSTOR ignores the mask in edx:eax: inferred from reading, not run.

**Signal frames.**
- ✅ **box64**: reserves 512 + 64 + 256 bytes and writes them with `fpu_xsave_mask(…, 0b111)`, MAGIC1 at offset 464 (`src/libtools/signals.c:373-390`).
  - `extended_size` is 0 (`res[13] = 0`, `signals.c:390`), `xfeatures` and `xstate_size` are not written, and there is no MAGIC2 (MAGIC1 at `:389`).
- ✅ **FEX**: full `_fpx_sw_bytes` (MAGIC1, `extended_size` 840, `xfeatures` FP|SSE|YMM, MAGIC2) and `ymmh` filled (`LinuxSyscalls/SignalDelegator/GuestFramesManagement.cpp:105-123`, `:479`; layout `ArchHelpers/UContext.h:36-137`).
  - The XSAVE header's `xfeatures` (XSTATE_BV) is left at 0 (`GuestFramesManagement.cpp:120`).
- ✅ The two engines cut different corners, so a test that is exact with respect to Linux's frame would put armor ahead of both. The reference is the Linux kernel's `arch/x86/kernel/fpu/signal.c` (⚠️ not read here).

### 4. What armor needs
Sizes are rough estimates (⚠️, from the size of the comparable SSE code: `armor-x86/src/sse.rs` 700 lines plus `maps.rs` 227; `armor-cpu/src/sse.rs` 1182, `simd.rs` 523, `float.rs` 578; `wc -l` on 2026-10-21).

1. **Decoder: VEX prefixes.** ✅ Done: the prefixes and the BMI instructions (section "The VEX prefix"); the AVX forms still return `Unsupported("AVX (VEX)")`, and 62 `Unsupported("AVX-512 (EVEX)")` (`crates/armor-x86/src/decoder.rs`, `vex.rs`).
   - What is needed for AVX: a third operand (vvvv) in the SSE ops; an operand size of 256 bits.
   - ✅ The SSE ops are two-operand (`Op::Sse { op, dst, src }`, `crates/armor-x86/src/lib.rs`). VEX needs `src1` (vvvv) separate from `dst`, and a length. ⚠️ Size: about 1000–1500 lines, most of them tables of the 128/256-bit forms.
2. **CPU state.** ✅ Done: `Cpu::ymm_hi` (section "The upper ymm halves"), `vzeroupper`/`vzeroall`, the VEX.128 zeroing. ✅ The JIT keeps xmm0–15 in v16–v31 (`docs/tech/jit.md`), so the upper halves stay in memory, as in box64 and FEX, unless registers are freed.
3. **CPUID.** ✅ Done for XSAVE (section "CPUID and XCR0"). As a model for the rest, `qemu-x86_64 -cpu max` (qemu 10.0.13) gives (✅ `basic/cpuid` under it, `od -tx8`):
   - leaf 1 ecx = 0xfffa320b: FMA bit 12, MOVBE 22, XSAVE 26, OSXSAVE 27, AVX 28, F16C 29, plus AES, PCLMUL, RDRAND, x2APIC;
   - leaf 7 ebx = 0x219c47a9: BMI1 bit 3, AVX2 5, BMI2 8, plus ERMS, ADX, SHA, …;
   - leaf 0x8000_0001 ecx = 0x377: ABM/LZCNT bit 5;
   - leaf 0xD subleaf 0: eax = 0x21f, ebx = ecx = 0xa88 (x87, SSE, AVX, plus MPX and PKRU); subleaf 1: eax = 5 (XSAVEOPT, XGETBV ecx=1, no XSAVEC).
   - ✅ `qemu64,...,+xsave,+avx,+avx2,+fma,+bmi1,+bmi2,+f16c,+movbe,+abm` gives leaf 1 ecx = 0xbcd83201, leaf 7 ebx = 0x128, leaf 0x8000_0001 ecx = 0x21, leaf 0xD subleaf 0 = (7, 0x340, 0x340, 0) and subleaf 1 = (0, 0x340, 0, 0): 832 = 512 + 64 + 256 bytes, as in box64 and FEX (measured with `basic/cpuid`).
   - ✅ The unit test `reported_extensions_match_the_feature_bits` ties `reports(Extension::Vex)` to leaf 1 bit 28. It must be split by feature, because one VEX bucket cannot say "declared or not" for BMI and AVX separately.
4. **LZCNT/TZCNT semantics.** ✅ Done (section "BMI1, BMI2, LZCNT, MOVBE"): counting zeros with BMI1/LZCNT declared, `bsf`/`bsr` without. glibc's AVX2 functions use `tzcnt` (✅ the `-cpu max` trace above: 11).
5. **MOVBE** (0F 38 F0/F1 without F2): ✅ done (`maps.rs`).
6. **XGETBV, XSAVE, XRSTOR.** ✅ Done (section "XSAVE"). `xsaveopt` and `xsavec` are not needed while CPUID does not declare them: glibc then uses `xsave` (✅ traces above).
7. **Signal frames.** ✅ Done for XSAVE with XCR0 = x87 | SSE (`signal/xstate.rs`, `tech/signals.md`): the XSAVE image, `_fpx_sw_bytes`, MAGIC2, `UC_FP_XSTATE`, and `rt_sigreturn` restoring the components, compared with qemu by `c/signal_xstate`. With AVX the same code saves YMM_Hi128 at 576 (832 bytes): ⚠️ to test with the first AVX instructions.
   - ✅ The auxiliary vector has no `AT_MINSIGSTKSZ` (`crates/armor-linux/src/loader.rs`: only `AT_HWCAP`, `AT_HWCAP2`). ⚠️ Whether glibc's `sysconf(_SC_MINSIGSTKSZ)` then uses leaf 0xD: read glibc. ⚠️ Size: 150–300 lines.
8. **Interpreter semantics.** ✅ Done: the VEX.128 and VEX.256 forms, the AVX-only and AVX2-only instructions but the gathers. Left:
   - 256-bit forms (two lanes of 128);
   - AVX-only instructions: `vbroadcast*`, `vperm2f128`/`vinsertf128`/`vextractf128`, `vmaskmov`, `vtestp*`, `vpermilp*`;
   - AVX2: `vpermd`/`vpermq`/`vpermps`, `vpbroadcast*`, `vpsllv`/`vpsrlv`/`vpsrav`, gathers, `vinserti128`/`vextracti128`, `vpblendd`;
   - FMA: 3 operand orders × ps/pd/ss/sd × add/sub/addsub/subadd/neg, rounded once (fused);
   - F16C: `vcvtph2ps`/`vcvtps2ph` with rounding control;
   - BMI1: `andn`, `bextr`, `blsi`, `blsmsk`, `blsr`, `tzcnt`;
   - BMI2: `bzhi`, `mulx`, `pdep`, `pext`, `rorx`, `sarx`, `shlx`, `shrx`.
   - ⚠️ FMA must be fused (one rounding) and follow MXCSR's rounding, DAZ and FTZ, with x86's NaN rules (`docs/tech/floating-point.md`). ⚠️ Size: the largest item, about 3000–5000 lines.
9. **JIT.** ✅ Instructions the translator does not know close the block, and the interpreter executes them (`docs/tech/jit.md`). Packed SSE float is already left to the interpreter. So AVX can arrive in the interpreter first, with the JIT correct from day one, only slower.
   - Translation: a ymm as two NEON operations, the upper half loaded from and stored to `Cpu`.
   - ✅ The Pi 5 has no SVE (`/proc/cpuinfo`), so there is no 256-bit path on this host. ⚠️ Size: weeks, done by measuring which instructions the real programs leave to the interpreter, as for SSE.
10. **Tests.**
    - ✅ `qemu-x86_64` 10.0.13 executes AVX2: under `-cpu max`, glibc's AVX2 functions run in `perl -v` (167 lines with `ymm` in the `-d in_asm` log).
    - ✅ `-cpu help` lists `avx avx2 fma bmi1 bmi2 f16c movbe abm xsave xsavec xsaveopt`.
    - ✅ Done: features per case in `difftest` (`CASE_FEATURES`), asm tests in `tests/guest/asm/xsave/`.
    - What is needed: asm tests per family; a C test with `-mavx2 -mfma`, built by `build-guest` (⚠️ check that the cross-compiler accepts it); lockstep for the JIT; an AVX2 machine for `difftest --oracle`, since the current one (Xeon X5675) has no AVX (`docs/testing.md`) ⚠️. Without that machine, qemu's mistakes would go unnoticed (they already did for SSE: `sse/mxcsr_hardware`, roadmap M4).

## Recommended order
1. **XGETBV, XSAVE/XRSTOR, leaf 0xD, `ymm_hi` and signal frames with xstate.** Without these, the AVX bit cannot be declared (OSXSAVE and XCR0 are part of v3's definition, psABI ✅). They are also what glibc runs first, on every lazy-bound call. ✅ Done, tested before any AVX instruction exists: XSAVE and OSXSAVE declared with XCR0 = 0x3, compared in `difftest` against `qemu64,+xsave`.
2. **VEX decoder with BMI1, BMI2, LZCNT, MOVBE** (general registers only, no ymm). They are small, they are what zstd's fast paths use (✅ section 1), and they are part of glibc's v3 functions (✅ `tzcnt`, `movbe` in the `-cpu max` trace). ✅ Done, declared one by one with `ARMOR_CPU_FEATURES`; by default not declared, as FEX declares them only together with AVX.
3. **VEX.128 forms of the SSE ops already implemented**, then **256-bit AVX/AVX2**, then **FMA** and **F16C**, in the interpreter. Only when all of v3 is there, declare it in CPUID by default (glibc's `x86-64-v3` level and the `-march=x86-64-v3` programs need the whole set; ✅ psABI).
4. **JIT**: translate first what real programs leave to the interpreter: `vmovdqu`/`vmovdqa`, `vpcmpeqb`, `vpmovmskb`, `vzeroupper` and `vpxor` dominate glibc's functions (✅ `-cpu max` trace); `vfmadd*`, `vshufps` and `vcvtph2ps` dominate XnView (✅ objdump).

**What to measure:**
- ⚠️ a program built for v3 (from RHEL 10, AlmaLinux 10 or Ubuntu `amd64v3`) under armor today: it should stop with SIGILL at the first VEX;
- ⚠️ the startup cost of `xsave`/`xrstor` in the interpreter on python and perl, with `ARMOR_CPU_FEATURES=+xsave`.
