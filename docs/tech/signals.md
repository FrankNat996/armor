# Signals

State as of 2026-10-12 (with additions of 2026-10-14, marked): **delivery to the guest's handlers is implemented** (ADR-0017, `crates/armor-linux/src/signal.rs`), also with multiple threads (ADR-0018).

## How signals arrive
- **Asynchronous** (from another process, from the terminal, from a timer, or from the guest itself with `kill`/`tgkill`/`tkill`): they go through the host kernel.
  - For each signal with a guest handler, armor installs `on_host_signal` in the host. The handler is async-signal-safe: it copies the host's `siginfo` into a static table of atomics, marks the signal as pending and **leaves it blocked** on return, by adding it to `uc_sigmask` of the host context.
  - Later instances of the same signal stay pending in the host kernel, which queues them as for a native process. This also holds for real-time signals.
  - The guest's `SIG_DFL` and `SIG_IGN` become the same disposition in the host, so the kernel carries out the default actions (termination, ignoring).
- **Synchronous** (faults): armor generates them from the interpreter's errors (`fault_signal`). A fault with the signal blocked or ignored terminates the process, as Linux does (it forces the default action).

  | Interpreter error | Signal | `si_code` | `si_addr` | trapno |
  |---|---|---|---|---|
  | access to unmapped memory | SIGSEGV | `SEGV_MAPERR` (1) | address | 14 (cr2 = address) |
  | access forbidden by the permissions | SIGSEGV | `SEGV_ACCERR` (2) | address | 14 |
  | #GP (`hlt`, `cli`/`sti`, port I/O, `int n`, misaligned SSE or `cmpxchg16b`, instruction > 15 bytes) | SIGSEGV | `SI_KERNEL` (0x80) | 0 | 13 |
  | #UD (invalid instruction, or one from an extension that CPUID does not declare: AVX, AVX-512, SHA…, ADR-0026) | SIGILL | `ILL_ILLOPN` (2) | rip | 6 |
  | #DE | SIGFPE | `FPE_INTDIV` (1) | rip | 0 |
  | #XM (unmasked SSE floating-point exception, since 2026-10-13) | SIGFPE | from the first MXCSR flag that is set and unmasked, in Linux's order: IE → `FPE_FLTINV` (7), ZE → `FPE_FLTDIV` (3), OE → `FPE_FLTOVF` (4), UE or DE → `FPE_FLTUND` (5), PE → `FPE_FLTRES` (6) | rip | 19 |
  | `int3` (CC, and CD 03) | SIGTRAP | `SI_KERNEL` | 0 | 3; the handler resumes after the instruction (1 or 2 bytes) |
  | `int1` (F1) | SIGTRAP | `TRAP_BRKPT` (1) ⚠️ | 0 | 1; the handler resumes after it |

  ✅ #XM: signo, code, `si_addr` and the frame's rip verified on the Xeon X5675 by `c/sse_exceptions` (qemu does not raise #XM). ✅ For the others, signo, code and address verified against qemu by the `c/signals` test; the signal of the privileged instructions, of `int n` and of `int1` by the `c/privileged` test (only the signal number and the resume point: the `si_code` of `int1` ⚠️ is the one recalled from the Linux sources). trapno, err and cr2 in the sigcontext ⚠️: written as Linux does according to its sources (recalled), not compared.
- The signals that armor reserves in the host (SIGSEGV, SIGBUS, SIGILL, SIGFPE, SIGTRAP: the Rust runtime uses them; SIGKILL, SIGSTOP; 32 and 33, internal to glibc) **sent by the guest to itself** do not go through the host: armor records them directly, with `si_code` `SI_USER` or `SI_TKILL`, the pid and the uid. From another process, instead, they do not reach the guest ⚠️.

## Threads
- **Actions** belong to the process (shared by all threads); the **mask and alternate stack** belong to each thread. A new thread starts with its parent's mask and without an alternate stack, as with `clone(CLONE_VM)`.
- The signals recorded by the host handler live in **thread-local** variables (initialised to a constant and without destructors, so they are accessible from a signal handler). The kernel picks the thread: for `tgkill`/`pthread_kill` the named thread, for a process signal a thread that does not block it. That thread delivers it.
- ✅ test `c/threads_signals`: `pthread_kill` handled by the target thread; a process `kill` handled by the only thread that does not block it; a blocked signal pending only on its thread, delivered when that thread unblocks it.

## Mask
- The guest's mask lives in `Signals` and is **mirrored in the host mask** (`pthread_sigmask`), plus the signals that are recorded and not yet delivered, minus the reserved ones. This way a blocked signal stays pending in the kernel as for a native process, and `rt_sigpending` sees it.
- At startup the guest **inherits** armor's mask and ignored signals, as after `exec`. Not the handlers.

## `rt_sigsuspend`
- armor calls the host's `rt_sigsuspend` with the new mask without applying it first: the mask change and the wait are a single step, so a signal that arrives in between is not lost.
- The handler's frame saves the mask **from before** (`saved_mask`, like Linux's `saved_sigmask`), so `rt_sigreturn` restores it. If no handler uses it, armor puts it back after the deliveries.
- ✅ `c/signals_async`: SIGALRM blocked, `sigsuspend` with an empty mask, the handler runs and after it SIGALRM is blocked again; the mutation test (frame with the wrong mask) fails.

## `ppoll` with a signal mask (since 2026-10-14)
- Like `rt_sigsuspend`: `Signals::begin_masked_wait` saves the guest's mask (`saved_mask`) and makes the given one current (minus SIGKILL and SIGSTOP); if a signal is already deliverable under it, the wait returns EINTR at once, as on Linux; otherwise the host's `ppoll` receives the matching host mask (plus the signals armor recorded and has not delivered), so switching masks and waiting is one step. A handler's frame saves the old mask; without a handler armor puts it back after the deliveries (`restore_saved_mask`).
- x86-64 and i386 (`ppoll`, `ppoll_time64`); the mask is 8 bytes. ✅ test `ppoll_mask`, run in `c/`, `c32/` and `c-dyn/`: identical to qemu with the interpreter and the JIT and to the Xeon (2026-10-14); ⚠️ the other three variants await the full chain.
- Still unsupported with a mask: `pselect6`, `epoll_pwait`, `epoll_pwait2` (`tech/syscalls.md`).

## `rt_sigtimedwait` and queued signals (since 2026-10-14)
- `Signals::timed_wait`: first the host **blocks the set**, so that a signal arriving meanwhile stays queued in the kernel instead of reaching armor's handler; then a signal of the set that armor has already recorded is taken (with the `siginfo_t` armor kept for it); otherwise the host's `rt_sigtimedwait` waits for one, until the timeout (none: forever). EAGAIN at the timeout, EINTR if interrupted. Afterwards the guest's mask is mirrored to the host again. The `siginfo_t` of a signal sent to a process has the same layout on the two architectures (`tech/syscalls.md`, "Structs confirmed identical").
- `rt_sigqueueinfo` and `rt_tgsigqueueinfo`: the guest's 128-byte `siginfo_t` is passed to the host's calls as it is, with the host's pids and tids (the guest's).
- ✅ test `sigtimedwait`, identical to qemu and to the Xeon (2026-10-14; ⚠️ all its variants in the next full chain).

## When they are delivered
- Between two instructions: the main loop (`main.rs`) checks for pending signals after every syscall and every 10,000 instructions (`SIGNAL_CHECK_INTERVAL`). ✅ test `c/signals_async` (a timer interrupts a compute loop).
- **Before a syscall:** if a signal is pending when the guest reaches `syscall`, armor delivers it without executing the syscall. After the handler the syscall starts again, as when the kernel delivers a signal that arrived before the syscall. Without this measure a blocking syscall (`pause`, `read`) could wait for a signal already taken.
  - ⚠️ A narrow window remains: a signal that arrives between this check and the entry into the host syscall. The host handler records it and the host syscall is not interrupted. Closing it completely requires a syscall that is atomic with respect to signals (for example `ppoll`/`sigsuspend` with the mask), to be evaluated. The guest's own `rt_sigsuspend` and, since 2026-10-14, `ppoll` with a mask already work this way (below); the other blocking syscalls keep the window.
- **Interrupted syscalls:** the host handlers are installed without `SA_RESTART`, so a blocking host syscall returns `EINTR`. If the guest's handler has `SA_RESTART` and the syscall is among those that Linux restarts, armor puts rip back on the `syscall` and rax back to the original number before building the frame. Otherwise the guest sees `-EINTR`. ✅ `c/signals_async`: `read` on an empty pipe, with and without `SA_RESTART`.
  - ⚠️ The list of restartable syscalls (`syscall::restartable`) approximates the choice that Linux makes case by case: `read`, `write`, `pread64`, `pwrite64`, `open`, `openat`, `ioctl`, `sendfile`.

## The frame (x86_64, Linux's `struct rt_sigframe`)
From the guest's stack (or from the alternate stack with `SA_ONSTACK`), downwards:
1. 128 bytes of red zone (not with the alternate stack);
2. the FPU state, 512 bytes aligned to 64: an FXSAVE image (`Cpu::fx_image`), as Linux does on a CPU without XSAVE (armor's CPU model has no XSAVE, ADR-0010);
3. the frame, aligned so that rsp + 8 is a multiple of 16, as after a `call`:

| Offset | Field |
|---|---|
| 0 | `pretcode` = `sa_restorer` (mandatory on x86_64: without `SA_RESTORER` → SIGSEGV) |
| 8 | `uc_flags` = 6 (`UC_SIGCONTEXT_SS` \| `UC_STRICT_RESTORE_SS`) ⚠️ |
| 16 | `uc_link` = 0 |
| 24 | `uc_stack` (sp, flags, size of the alternate stack) |
| 48 | `uc_mcontext` = `struct sigcontext`, 256 bytes: r8–r15, rdi, rsi, rbp, rbx, rdx, rax, rcx, rsp, rip, eflags, cs (0x33), gs, fs, ss (0x2b), err, trapno, oldmask, cr2, pointer to the FPU state |
| 304 | `uc_sigmask` (8 bytes) |
| 312 | `siginfo` (128 bytes; for signals from the host it is the host's, with the same layout) |

On entry to the handler: rdi = number, rsi = &siginfo, rdx = &ucontext, rax = 0, rsp = frame, rip = handler; TF, DF and RF cleared. **Initial FPU state** (since 2026-10-13, `Cpu::reset_fpu`): xmm zeroed, MXCSR 0x1f80, x87 as after `fninit` with the registers zeroed; the state of the interrupted code stays in the frame and comes back with `rt_sigreturn` ✅ (Xeon X5675, `c/sse_exceptions`: the handler sees xmm5 = 0, MXCSR 1f80, control word 037f, tag ffff; after the return, the previous values. qemu instead leaves the interrupted state to the handler). Mask: the previous one plus `sa_mask` plus the signal itself (not with `SA_NODEFER`). `SA_RESETHAND` sets the action back to `SIG_DFL`.

✅ Layout verified by the `c/signals` test: a handler modifies rip and rax in `uc_mcontext.gregs` (glibc offsets `REG_RIP`, `REG_RAX`), and `siglongjmp` leaves fault handlers while restoring the mask.

**trapno, err and cr2 stay those of the last fault** (since 2026-10-14): Linux keeps them in the thread and writes them into every later frame, also for signals that are not faults. ✅ Xeon X5675: a SIGUSR1 after a `ud2` has trapno 6, in a 64-bit process and in a 32-bit one; armor gave 0 (a real difference that no test saw; now `Signals::last_fault`, copied into new threads). `c32/signal_context` checks it against the Xeon.

## The i386 frame (ADR-0029, since 2026-10-14)
A 32-bit process on an x86-64 kernel receives the frames of `ia32_setup_rt_frame` (with `SA_SIGINFO`) and `ia32_setup_frame` (without), in `crates/armor-linux/src/signal/i386.rs`. From the stack (or from the alternate stack), downwards, without a red zone:
1. the 512-byte FXSAVE image aligned to 64, with the software-reserved bytes (464–511) of `_fpx_sw_bytes`: magic `0x46505853`, `extended_size` 628 (512 + 4 + 112), `xfeatures` 0, `xstate_size` 512 ✅ Xeon;
2. right below it, the 112-byte **FSAVE header** that the kernel converts from the FXSAVE state (`convert_from_fxsr`): `cw` and `sw` with the upper half at `0xffff`, the full tag word computed from the registers, `cssel` = 0x23 (the process's `cs`), `datasel` = `0xffff002b`, the 8 10-byte registers, `status` = sw and `magic` = 0 (FXSR format) ✅ Xeon (`c32/signal_context`). FIP and FDP are 0: armor does not keep them. The `fpstate` pointer of the `sigcontext` points here;
3. the frame, aligned so that `(esp + 4) & 15` is 0 on entry, as after a `call`.

**rt frame** (268 bytes): `pretcode` (`sa_restorer`, mandatory as on x86_64: armor has no i386 vDSO), the signal number, the pointers to `info` and to `uc`, the compat `siginfo` (128 bytes: the union starts at 12 instead of 16, with 32-bit `long` and pointers, according to the signal's class as in Linux's `siginfo_layout`), the `ucontext` (`uc_flags` 0 ✅ Xeon, qemu-i386 says 1; `uc_link`; the 12-byte alternate stack; `sigcontext_32`; the 8-byte mask) and 8 bytes of return code (`movl $173, %eax; int $0x80`).
**Old frame** (732 bytes): `pretcode`, the number, `sigcontext_32`, 624 unused bytes, the upper half of the mask (the lower half is in `oldmask`), return code (`popl %eax; movl $119, %eax; int $0x80`). glibc uses it for handlers without `SA_SIGINFO`, returning with `sigreturn`.
**`sigcontext_32`** (88 bytes): gs, fs, es, ds (32-bit selectors: gs the TLS one, 0x63; es and ds 0x2b), edi, esi, ebp, esp, ebx, edx, ecx, eax, trapno, err, eip, cs (0x23), eflags, esp at the signal, ss (0x2b), `fpstate`, `oldmask`, cr2 ✅ Xeon.
On entry: eax = number, edx = &info and ecx = &uc (for `-mregparm=3` handlers; 0 and 0 in the old frame), esp = frame, eip = handler; TF, DF, RF cleared; initial FPU as in 64-bit.
**`rt_sigreturn`/`sigreturn`** (173 and 119): the frame is 4 or 8 bytes below esp; they restore the registers, the modifiable eflags, gs and fs (a selector that does not name a segment terminates with SIGSEGV), the mask and the FXSAVE image ⚠️ (Linux also takes the x87 environment and the registers of the FSAVE header, which a handler might have changed; armor does not).
✅ `c32/signals32` (`qemu-i386` and Xeon): `kill` and `raise` with `SA_SIGINFO`, a handler without `SA_SIGINFO` (old frame), `sa_mask`, `SA_RESETHAND`, a signal blocked and delivered on unblocking, SIGSEGV with `si_addr` and `eip` moved, SIGFPE and `siglongjmp`, SIGILL, alternate stack, x87/SSE state restored after a handler that changes it. `c32/signals_async`: timer (i386 `setitimer`), `pause`, interrupted and restarted syscalls, `rt_sigsuspend`.

## `rt_sigreturn`
It reads the frame at rsp (the handler's `ret` has already removed `pretcode`), restores all registers, only the modifiable RFLAGS bits (AC, OF, DF, TF, SF, ZF, AF, PF, CF, RF), the FPU state from the pointer (if not null) and the mask. An unreadable frame or an FPU state that armor does not support terminates the guest with SIGSEGV and a message.

## Not yet
- Stop and continue signals (SIGTSTP & co.) with a handler: they behave like the others; without a handler the host kernel handles them, which stops armor.
- `signalfd`: not yet required by a measured program (`rt_sigtimedwait` since 2026-10-14, above).
- The XSAVE extended state in the frame: the CPU model has no XSAVE.
