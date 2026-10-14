# Glossary

| Term | Meaning |
|---|---|
| **Guest** | The x86_64 program run by armor and everything it "sees": registers, memory, ABI. |
| **Host** | The real machine: Linux AArch64 on the Raspberry Pi 5. |
| **User-mode emulation** | Only the user space of the process is emulated. Syscalls go to the host kernel after translation. |
| **Oracle** | Reference implementation armor is compared against (`qemu-x86_64`, `objdump`). |
| **Differential test** | The same input run on the oracle and on armor, with the results compared. |
| **Golden file** | Oracle output saved to a file and used as a reference by the tests. |
| **Interpreter** | Runs the guest one instruction at a time (fetch, decode, execute). |
| **JIT / dynarec** | Translates blocks of x86_64 code into native AArch64 code at runtime. "Dynarec" is the term used by box64. |
| **Basic block** | Sequence of instructions with a single entry and a single exit (it ends with a jump). It is the JIT's unit of translation. |
| **Chaining** | In the JIT, a block that jumps directly to the next block instead of returning to the engine (`tech/jit.md`). |
| **Slow path** | In the JIT, the route taken when the translated code cannot continue (permission check failed, division raising #DE…): the instruction goes to the interpreter with the exact state from before. |
| **Lazy flags** | x86 flags not computed immediately: the operands of the last operation are kept and the flags are computed only if someone reads them. Used by the JIT (`tech/jit.md`); the interpreter computes them immediately. |
| **Dead flags** | Flags that an instruction writes but that nobody reads before another instruction rewrites them. The JIT finds them with a backward liveness analysis on every block and does not compute them (`tech/jit.md`, increment 19). |
| **Lockstep** | JIT verification mode (`--lockstep`): every translated instruction also runs in the interpreter from the same state, and the results must match. |
| **Mutation test** | An error deliberately introduced into the code to check that the tests find it (`testing.md`). |
| **Block cache** | Map from guest address to translated code. |
| **IR** | Intermediate representation between decoder and backend. armor's JIT does not use one: it translates the decoded instructions directly (ADR-0021 §3). |
| **Undefined flags** | Flags that the architecture leaves with an unspecified value after certain instructions. |
| **REX / VEX / ModRM / SIB** | Components of the x86-64 encoding (`tech/x86-64-encoding.md`). |
| **LSE** | ARM's Large System Extensions: atomic instructions (`CAS`, `LDADD`, `SWP`, …). Feature `atomics`. |
| **LDAPR / RCpc** | "Weak" load-acquire from ARMv8.3. Feature `lrcpc`, present on the host. |
| **W^X** | A page is never writable and executable at the same time. |
| **SMC** | Self-modifying code: code that modifies itself; it forces the block cache to be invalidated. |
| **Identity mapping** | Guest address = host address. |
| **auxv** | Auxiliary vector that the kernel passes on the stack when a process starts (`AT_PAGESZ`, `AT_ENTRY`, …). |
| **vDSO** | Library that the kernel maps into the process for fast syscalls. The guest has no x86_64 vDSO. |
| **ET_EXEC / ET_DYN / PIE** | Fixed-address executable / relocatable object / position-independent executable. |
| **PT_INTERP** | ELF segment that names the dynamic loader (`ld.so`). |
| **TLS** | Thread-Local Storage. On x86_64 it is addressed through the base of the FS segment. |
| **Guest rootfs** | File tree with x86_64 libraries (including `ld.so`) used by dynamic binaries. |
| **Wrapping / thunk** | Replacing an x86_64 library with calls to the host's native library (box64's approach; FEX calls them thunks). |
| **ADR** | Architecture Decision Record (`decisions/`). |
| **GIL** | Global interpreter lock: a lock that lets only one thread at a time run guest code. With ADR-0023 it is needed only in lockstep and with `ARMOR_GIL=1`; without concurrent mode there is only one thread anyway. |
| **TSO** | Total Store Order, the x86 memory model: stores leave in order, loads do not overtake each other, only a load can pass an earlier store. AArch64 allows more; armor reproduces it with `LDAPR`/`STLR` and barriers (ADR-0023). |
| **Concurrent mode** | The state of a guest process from the moment other code can write its memory in parallel (a second thread or a writable shared mapping): accesses ordered as TSO, atomic `LOCK` (LSE in the JIT), no GIL (ADR-0023). |
| **Litmus test** | A program that races two threads on a few memory locations and counts the results that a memory model forbids (`c/litmus`, `testing.md`). |
