# 0018 — Threads: 1:1 with the host, only one thread at a time runs guest code

- **Status:** Superseded by 0023 for the execution of guest code (2026-10-13): with several threads or shared memory the code runs in parallel, with TSO ordering. The rest (1:1, `futex`, table locks) remains. `fork` arrived with 0019. Accepted on 2026-10-12.
- **Date:** 2026-10-12

## Context
Real programs create threads (`pthread_create` → `clone` with `CLONE_VM|CLONE_THREAD|…`, measured on 2026-10-12 with `qemu -strace` on the `c/threads` test). What is needed:
- one `Cpu` per thread;
- the shared address space;
- real `futex`, TLS (`CLONE_SETTLS`), `CLONE_CHILD_CLEARTID` for `pthread_join`;
- per-thread `exit`;
- per-thread signal mask.

The underlying problem: x86 guarantees atomicity for instructions with the `LOCK` prefix (and for `xchg` with memory) and TSO ordering of normal loads and stores. ARM has a weaker model. With truly parallel host threads, every guest access would have to be translated into atomic host operations with acquire/release, and every `LOCK` read-modify-write into a CAS loop. The roadmap puts this work (memory model) in M6, together with the JIT.

## Decision
- **1:1:** every guest thread is a host thread (`std::thread`), with its own `Cpu` and its own tid, which the guest sees with `gettid`.
- **A global lock (GIL) for the execution of guest code:** only one thread at a time interprets instructions.
  - `LOCK` instructions stay atomic by construction, because an instruction completes within one step.
  - The ordering seen by the guest is sequentially consistent, stronger than TSO, therefore correct.
- **The GIL is held only inside `Cpu::run` and signal delivery**, never during a syscall. A syscall that blocks (`futex` wait, `read`, `pause`) does not stop the other threads.
- **The lock is fair** (a ticket lock): a thread that finishes its block of instructions gives the turn to the others. Without fairness, a guest spinlock would livelock: the spinning thread would hold the lock and the owner of the spinlock would never run.
- **The page table has its own `RwLock`.** A thread in the GIL reads it while another, in a syscall, runs `mmap`. Lock order: GIL → table; syscalls take only the table.
- **`futex` passes the guest address to the host.** It is the only exception to the rule "no guest pointer to the host kernel": the address is armor's memory mapped identically, and armor first verifies that it is mapped and writable.
- `clone`: only the POSIX thread flag set. Without `CLONE_VM` (fork) → not supported for now (later ADR-0019: host `fork`, with a single-threaded guest). `clone3` → `ENOSYS`, because glibc falls back to `clone`.

## Alternatives considered
- **Truly parallel threads** with host atomics for `LOCK`, acquire/release loads and stores, CAS for the unaligned cases: uses more cores, but it is a lot of work with many possible bugs, and for an interpreter it gives no proportionate advantage. Postponed to M6, where the JIT will have to face the memory model anyway (`tech/memory.md`).
- **"Green" threads on a single host thread:** no lock, but every blocking syscall would block all the guest's threads.

## Consequences
- \+ Correctness simple to reason about: no races between threads inside the interpreter.
- \+ Blocking syscalls stay parallel.
- − Guest code does not use more than one core: a program with N compute threads runs at the speed of one.
- − The GIL and the table lock cost something at every block and every memory access: to be measured.
- To be redone with the JIT (M6): the TSO memory model on ARM.
