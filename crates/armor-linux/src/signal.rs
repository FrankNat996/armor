//! Signals for the guest (milestone M3, ADR-0017).
//!
//! - **Asynchronous signals** (sent by another process, by the terminal,
//!   by a timer, or by the guest to itself with `kill`/`tgkill`) reach
//!   armor through the host kernel. For every signal the guest handles,
//!   armor installs `on_host_signal`, which only records the signal and
//!   its `siginfo` and leaves the signal blocked in the host. The main loop
//!   then delivers it to the guest between two instructions, or after the
//!   syscall it interrupted. Further instances wait, blocked, in the host
//!   kernel, which therefore keeps pending and queued signals exactly as
//!   it would for a native process.
//! - **Synchronous signals** (SIGSEGV, SIGFPE, SIGILL, SIGTRAP) come from
//!   faults of the interpreter (`fault_signal`), never from the host.
//! - **The guest's signal mask** is kept here and mirrored into the host
//!   mask, minus the signals armor needs itself.
//! - **Delivery** builds the x86_64 `rt_sigframe` on the guest stack:
//!   return address (`sa_restorer`), `ucontext` with the registers, the
//!   `siginfo`, and the FPU state as a 512-byte FXSAVE image, as Linux does
//!   on a CPU without XSAVE. `rt_sigreturn` reads it back.
//!
//! Layouts: the kernel's `struct rt_sigframe`, `struct ucontext` and
//! `struct sigcontext` for x86_64 (docs/tech/signals.md).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use armor_cpu::{
    Cpu, CpuError, Exception, R8, R9, R10, R11, R12, R13, R14, R15, RAX, RBP, RBX, RCX, RDI, RDX,
    RSI, RSP,
};
use armor_mem::{AddressSpace, GuestMemory};
use armor_x86::{DecodeError, Mode};

mod i386;
pub use i386::siginfo32;

pub const NSIG: u64 = 64;
pub const SIGACTION_SIZE: usize = 32;
pub const SIGINFO_SIZE: usize = 128;

const SIG_DFL: u64 = 0;
const SIG_IGN: u64 = 1;

// sa_flags, identical on x86_64 and AArch64 (docs/tech/syscalls.md).
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;
/// x86_64 only: sa_restorer is valid. Linux requires it on x86_64.
const SA_RESTORER: u64 = 0x0400_0000;

// sigaltstack flags.
const SS_ONSTACK: u32 = 1;
const SS_DISABLE: u32 = 2;
const MINSIGSTKSZ: u64 = 2048;

// si_code values.
const SI_USER: i32 = 0;
const SI_KERNEL: i32 = 0x80;
const SI_TKILL: i32 = -6;
const SEGV_MAPERR: i32 = 1;
const SEGV_ACCERR: i32 = 2;
const TRAP_BRKPT: i32 = 1;
const ILL_ILLOPN: i32 = 2;
const FPE_INTDIV: i32 = 1;
const FPE_FLTDIV: i32 = 3;
const FPE_FLTOVF: i32 = 4;
const FPE_FLTUND: i32 = 5;
const FPE_FLTRES: i32 = 6;
const FPE_FLTINV: i32 = 7;

// Frame layout (x86_64): pretcode, then struct ucontext, then siginfo.
const UC_FLAGS: u64 = 8;
const UC_LINK: u64 = 16;
const UC_STACK: u64 = 24;
const UC_MCONTEXT: u64 = 48;
const UC_SIGMASK: u64 = 304;
const FRAME_INFO: u64 = 312;
const FRAME_SIZE: u64 = FRAME_INFO + SIGINFO_SIZE as u64;
const SIGCONTEXT_SIZE: usize = 256;
const FXSAVE_SIZE: u64 = 512;
const RED_ZONE: u64 = 128;
/// UC_SIGCONTEXT_SS | UC_STRICT_RESTORE_SS, as Linux sets on x86_64.
const UC_FLAGS_VALUE: u64 = 0x6;
const USER_CS: u16 = 0x33;
const USER_SS: u16 = 0x2b;

/// RFLAGS bits a signal frame may change: AC, OF, DF, TF, SF, ZF, AF, PF,
/// CF and RF (the kernel's FIX_EFLAGS).
const FIX_EFLAGS: u64 =
    0x4_0000 | 0x800 | 0x400 | 0x100 | 0x80 | 0x40 | 0x10 | 0x4 | 0x1 | 0x1_0000;
/// Cleared on handler entry: TF, DF, RF.
const ENTRY_CLEARED_FLAGS: u64 = 0x100 | 0x400 | 0x1_0000;

/// Signals whose host disposition armor keeps: the faults of armor's own
/// code, signals that cannot be caught, and glibc's two internal
/// real-time signals (SIGCANCEL, SIGSETXID).
fn reserved_on_host(signal: u64) -> bool {
    matches!(
        signal as i32,
        libc::SIGSEGV
            | libc::SIGBUS
            | libc::SIGILL
            | libc::SIGFPE
            | libc::SIGTRAP
            | libc::SIGKILL
            | libc::SIGSTOP
    ) || signal == 32
        || signal == 33
}

fn bit(signal: u64) -> u64 {
    1 << (signal - 1)
}

/// Signals that cannot be blocked.
const UNBLOCKABLE: u64 = (1 << (libc::SIGKILL - 1)) | (1 << (libc::SIGSTOP - 1));

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);
#[allow(clippy::declare_interior_mutable_const)]
const NO_INFO: [AtomicU64; SIGINFO_SIZE / 8] = [ZERO; SIGINFO_SIZE / 8];

// Recorded by `on_host_signal` on the thread the kernel chose, consumed by
// `take_pending` on the same thread. Constant-initialized and without
// destructors, so reaching them from a signal handler is safe.
thread_local! {
    static PENDING: AtomicU64 = const { AtomicU64::new(0) };
    static INFO: [[AtomicU64; SIGINFO_SIZE / 8]; NSIG as usize] =
        const { [NO_INFO; NSIG as usize] };
}

fn pending_bits() -> u64 {
    PENDING.with(|p| p.load(Ordering::Acquire))
}

/// The host handler: async-signal-safe, it only stores the siginfo, marks
/// the signal pending, and keeps it blocked when it returns.
extern "C" fn on_host_signal(
    signal: libc::c_int,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    let index = (signal - 1) as usize;
    let words = info.cast::<u64>();
    INFO.with(|table| {
        for (i, slot) in table[index].iter().enumerate() {
            // SAFETY: the kernel passes a valid 128-byte siginfo.
            slot.store(unsafe { words.add(i).read() }, Ordering::Relaxed);
        }
    });
    PENDING.with(|p| p.fetch_or(1 << index, Ordering::Release));
    // SAFETY: the kernel passes a valid ucontext; sigaddset only writes
    // the signal mask restored when this handler returns.
    unsafe {
        libc::sigaddset(
            &mut (*context.cast::<libc::ucontext_t>()).uc_sigmask,
            signal,
        );
    }
}

/// `struct sigaction` (x86_64 layout: handler, flags, restorer, mask) of
/// every signal, index = number - 1: shared by all the guest's threads.
type Actions = [[u8; SIGACTION_SIZE]; NSIG as usize];

/// A guest thread's signal state: the process-wide actions, and its own
/// mask and alternate stack.
#[derive(Debug)]
pub struct Signals {
    actions: Arc<Mutex<Actions>>,
    /// Blocked signals.
    mask: u64,
    alt_stack: AltStack,
    /// The mask rt_sigsuspend replaced: the next signal frame saves it, so
    /// that rt_sigreturn restores it (Linux's saved_sigmask).
    saved_mask: Option<u64>,
    /// The last fault's trapno, error code and address: Linux keeps them
    /// in the thread and writes them into every later frame too, also of
    /// signals that are not faults (✅ Xeon X5675: a SIGUSR1 after a ud2
    /// has trapno 6, in a 64-bit and in a 32-bit process).
    last_fault: FaultContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AltStack {
    sp: u64,
    size: u64,
    disabled: bool,
}

impl Default for Signals {
    fn default() -> Signals {
        Signals {
            actions: Arc::new(Mutex::new([[0; SIGACTION_SIZE]; NSIG as usize])),
            mask: 0,
            saved_mask: None,
            last_fault: FaultContext {
                trapno: 0,
                err: 0,
                cr2: 0,
            },
            alt_stack: AltStack {
                sp: 0,
                size: 0,
                disabled: true,
            },
        }
    }
}

/// A signal ready for delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub signal: u64,
    pub info: [u8; SIGINFO_SIZE],
    /// Fault details for the sigcontext (trapno, err, cr2).
    pub fault: Option<FaultContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultContext {
    pub trapno: u64,
    pub err: u64,
    pub cr2: u64,
}

/// What delivery decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivered {
    /// A guest handler will run.
    Handler,
    /// Ignored, explicitly or by default.
    Ignored,
    /// The process must end with this signal (default action, or a frame
    /// that cannot be written).
    Terminate(i32),
}

fn field(action: &[u8; SIGACTION_SIZE], i: usize) -> u64 {
    u64::from_le_bytes(action[i * 8..i * 8 + 8].try_into().expect("8 bytes"))
}

impl Signals {
    /// The state of a new thread (clone): the same actions, the parent's
    /// mask, no alternate stack.
    pub fn for_new_thread(&self) -> Signals {
        Signals {
            actions: Arc::clone(&self.actions),
            mask: self.mask,
            saved_mask: None,
            // Copied with the rest of the thread by clone.
            last_fault: self.last_fault,
            alt_stack: Signals::default().alt_stack,
        }
    }

    /// Holds the process-wide actions (fork of a guest with several
    /// threads, `process.rs`).
    pub(crate) fn hold_actions(&self) -> impl Drop + '_ {
        self.actions()
    }

    fn actions(&self) -> MutexGuard<'_, Actions> {
        // Poisoned only if a thread panicked while holding it.
        self.actions.lock().expect("signal actions lock poisoned")
    }

    pub fn action(&self, signal: u64) -> [u8; SIGACTION_SIZE] {
        self.actions()[(signal - 1) as usize]
    }

    fn handler(&self, signal: u64) -> u64 {
        field(&self.action(signal), 0)
    }

    fn flags(&self, signal: u64) -> u64 {
        field(&self.action(signal), 1)
    }

    /// The state a process starts with after exec: the signal mask and
    /// the ignored signals are inherited, handlers are not.
    pub fn inherit_from_host(&mut self) {
        // SAFETY: an all-zero sigset is valid; pthread_sigmask fills it.
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: a null new set only queries the current mask.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut set) };
        for signal in 1..=NSIG {
            // SAFETY: set is a valid sigset_t.
            if unsafe { libc::sigismember(&set, signal as i32) } == 1 {
                self.mask |= bit(signal);
            }
            if reserved_on_host(signal) {
                continue;
            }
            // SAFETY: an all-zero sigaction is a valid value of this C struct.
            let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: a null new action only queries the current one.
            unsafe { libc::sigaction(signal as i32, std::ptr::null(), &mut current) };
            if current.sa_sigaction == libc::SIG_IGN {
                self.actions()[(signal - 1) as usize][..8].copy_from_slice(&SIG_IGN.to_le_bytes());
            }
        }
        self.mask &= !UNBLOCKABLE;
    }

    pub fn mask(&self) -> u64 {
        self.mask
    }

    /// rt_sigaction: records the action and sets the host disposition:
    /// armor's recorder for a guest handler, otherwise the same default or
    /// ignore.
    pub fn set_action(&mut self, signal: u64, action: [u8; SIGACTION_SIZE]) {
        let mut actions = self.actions();
        actions[(signal - 1) as usize] = action;
        update_host(&actions, signal);
    }
}

/// Sets the host disposition of `signal` from the guest's action.
fn update_host(actions: &Actions, signal: u64) {
    if reserved_on_host(signal) {
        return;
    }
    let handler = field(&actions[(signal - 1) as usize], 0);
    // SAFETY: an all-zero sigaction is a valid value of this C struct.
    let mut host: libc::sigaction = unsafe { std::mem::zeroed() };
    host.sa_sigaction = match handler {
        SIG_DFL => libc::SIG_DFL,
        SIG_IGN => libc::SIG_IGN,
        _ => on_host_signal as *const () as usize,
    };
    // No SA_RESTART: a blocking syscall must return EINTR to armor,
    // which decides about restarting (`restart_syscall`).
    host.sa_flags = libc::SA_SIGINFO;
    // SAFETY: sa_mask is a valid sigset; the handler (if any) is
    // async-signal-safe.
    unsafe {
        libc::sigfillset(&mut host.sa_mask);
        libc::sigaction(signal as i32, &host, std::ptr::null_mut());
    }
}

impl Signals {
    /// rt_sigprocmask's new mask, mirrored into the host.
    pub fn set_mask(&mut self, mask: u64) {
        self.mask = mask & !UNBLOCKABLE;
        self.mirror_mask();
    }

    /// The host mask: the guest's, plus the signals recorded but not
    /// delivered yet (so that further instances queue in the kernel),
    /// minus those armor keeps.
    fn mirror_mask(&self) {
        let set = host_set(self.mask | pending_bits());
        // SAFETY: set is a valid sigset_t.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut()) };
    }

    /// rt_sigsuspend: waits for a signal with `mask` as the mask, and
    /// returns -EINTR. The host's rt_sigsuspend switches mask and waits in
    /// one step, so a signal arriving in between is not lost; the guest's
    /// mask becomes `mask` only for the delivery, and the frame saves the
    /// old one.
    pub fn suspend(&mut self, mask: u64) -> i64 {
        self.saved_mask = Some(self.mask);
        self.mask = mask & !UNBLOCKABLE;
        if !self.has_deliverable() {
            let host = host_set(self.mask | pending_bits());
            // SAFETY: host is a valid sigset_t; the kernel reads 8 bytes.
            unsafe { libc::syscall(libc::SYS_rt_sigsuspend, &raw const host, 8usize) };
        }
        -i64::from(libc::EINTR)
    }

    /// ppoll (and the other waits that take a mask): the guest's mask
    /// becomes `mask` for the wait, as for rt_sigsuspend (a signal frame
    /// saves the old mask; `restore_saved_mask` puts it back otherwise).
    /// Returns the host mask for the host call, which switches masks and
    /// waits in one step; `None` when a signal can already be delivered
    /// (the wait then returns EINTR at once, as on Linux).
    pub fn begin_masked_wait(&mut self, mask: u64) -> Option<libc::sigset_t> {
        self.saved_mask = Some(self.mask);
        self.mask = mask & !UNBLOCKABLE;
        if self.has_deliverable() {
            return None;
        }
        Some(host_set(self.mask | pending_bits()))
    }

    /// After deliveries: if rt_sigsuspend's mask was not consumed by a
    /// signal frame, put the old mask back (Linux does this when no
    /// handler ran).
    pub fn restore_saved_mask(&mut self) {
        if let Some(mask) = self.saved_mask.take() {
            self.set_mask(mask);
        }
    }

    /// Signals pending for the guest: blocked in the host kernel or
    /// recorded by armor.
    pub fn pending(&self) -> u64 {
        // SAFETY: an all-zero sigset is valid; sigpending fills it.
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: set is a valid sigset_t.
        unsafe { libc::sigpending(&mut set) };
        let mut pending = pending_bits();
        for signal in 1..=NSIG {
            // SAFETY: set is a valid sigset_t.
            if unsafe { libc::sigismember(&set, signal as i32) } == 1 {
                pending |= bit(signal);
            }
        }
        pending & self.mask
    }

    /// Whether `take_pending` has a signal to deliver.
    pub fn has_deliverable(&self) -> bool {
        pending_bits() & !self.mask != 0
    }

    /// The next recorded signal the guest does not block.
    pub fn take_pending(&self) -> Option<Pending> {
        let ready = pending_bits() & !self.mask;
        if ready == 0 {
            return None;
        }
        let signal = u64::from(ready.trailing_zeros()) + 1;
        PENDING.with(|p| p.fetch_and(!bit(signal), Ordering::AcqRel));
        let mut info = [0u8; SIGINFO_SIZE];
        INFO.with(|table| {
            for (i, slot) in table[(signal - 1) as usize].iter().enumerate() {
                info[i * 8..i * 8 + 8].copy_from_slice(&slot.load(Ordering::Relaxed).to_le_bytes());
            }
        });
        Some(Pending {
            signal,
            info,
            fault: None,
        })
    }

    /// rt_sigtimedwait(2): takes a signal of `set` that is pending (one
    /// armor recorded, or one the host kernel holds), else waits for one
    /// until `timeout` (none: forever). The host first blocks `set`, so a
    /// signal arriving meanwhile stays queued in the kernel instead of
    /// reaching armor's handler; the guest's mask is mirrored back after.
    /// Returns the signal and its `siginfo_t`, or a negative errno
    /// (EAGAIN at the timeout, EINTR).
    pub fn timed_wait(
        &mut self,
        set: u64,
        timeout: Option<&libc::timespec>,
    ) -> Result<(u64, [u8; SIGINFO_SIZE]), i64> {
        let set = set & !UNBLOCKABLE;
        let wanted = host_set(set);
        // SAFETY: wanted is a valid sigset_t.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &wanted, std::ptr::null_mut()) };
        let recorded = pending_bits() & set;
        if recorded != 0 {
            let signal = u64::from(recorded.trailing_zeros()) + 1;
            PENDING.with(|p| p.fetch_and(!bit(signal), Ordering::AcqRel));
            let mut info = [0u8; SIGINFO_SIZE];
            INFO.with(|table| {
                for (i, slot) in table[(signal - 1) as usize].iter().enumerate() {
                    info[i * 8..i * 8 + 8]
                        .copy_from_slice(&slot.load(Ordering::Relaxed).to_le_bytes());
                }
            });
            self.mirror_mask();
            return Ok((signal, info));
        }
        let mut info = [0u8; SIGINFO_SIZE];
        let timeout_ptr = timeout.map_or(std::ptr::null(), |t| t as *const libc::timespec);
        // SAFETY: wanted is a sigset whose first 8 bytes are the kernel's
        // mask; info is a 128-byte siginfo; timeout_ptr is null or a
        // timespec. The siginfo layout of signals sent to a process is the
        // same on both architectures.
        let r = unsafe {
            libc::syscall(
                libc::SYS_rt_sigtimedwait,
                &raw const wanted,
                info.as_mut_ptr(),
                timeout_ptr,
                8usize,
            )
        };
        let error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        self.mirror_mask();
        if r < 0 {
            return Err(-i64::from(error));
        }
        Ok((r as u64, info))
    }

    /// Whether `signal`'s action restarts syscalls interrupted by it.
    pub fn restarts(&self, signal: u64) -> bool {
        self.flags(signal) & SA_RESTART != 0
    }

    /// sigaltstack(2): returns the old stack (sp, flags, size) and applies
    /// a new one, or an errno.
    pub fn alt_stack(
        &mut self,
        new: Option<(u64, u32, u64)>,
        rsp: u64,
    ) -> Result<(u64, u32, u64), i32> {
        let on_stack = self.on_alt_stack(rsp);
        let old = &self.alt_stack;
        let old_flags = if on_stack {
            SS_ONSTACK
        } else if old.disabled {
            SS_DISABLE
        } else {
            0
        };
        let old = (old.sp, old_flags, old.size);
        if let Some((sp, flags, size)) = new {
            if on_stack {
                return Err(libc::EPERM);
            }
            if flags & SS_DISABLE != 0 {
                self.alt_stack = AltStack {
                    sp: 0,
                    size: 0,
                    disabled: true,
                };
            } else if flags & !SS_ONSTACK != 0 {
                return Err(libc::EINVAL);
            } else if size < MINSIGSTKSZ {
                return Err(libc::ENOMEM);
            } else {
                self.alt_stack = AltStack {
                    sp,
                    size,
                    disabled: false,
                };
            }
        }
        Ok(old)
    }

    fn on_alt_stack(&self, rsp: u64) -> bool {
        let s = &self.alt_stack;
        !s.disabled && rsp > s.sp && rsp - s.sp <= s.size
    }

    /// Delivers `pending` to the guest: runs its handler (building the
    /// frame), ignores it, or tells the caller to terminate.
    /// `synchronous`: a fault, which cannot be blocked or ignored (Linux
    /// forces the default action then).
    pub fn deliver(
        &mut self,
        cpu: &mut Cpu,
        mem: &AddressSpace,
        pending: &Pending,
        synchronous: bool,
    ) -> Delivered {
        let signal = pending.signal;
        let handler = self.handler(signal);
        let blocked = self.mask & bit(signal) != 0;
        if synchronous && (blocked || handler == SIG_IGN) {
            return Delivered::Terminate(signal as i32);
        }
        match handler {
            SIG_IGN => return Delivered::Ignored,
            SIG_DFL => {
                return if default_ignores(signal) {
                    Delivered::Ignored
                } else {
                    Delivered::Terminate(signal as i32)
                };
            }
            _ => {}
        }
        match self.build_frame(cpu, mem, pending) {
            Ok(()) => Delivered::Handler,
            // The frame cannot be written: Linux kills with SIGSEGV.
            Err(()) => Delivered::Terminate(libc::SIGSEGV),
        }
    }

    fn build_frame(
        &mut self,
        cpu: &mut Cpu,
        mem: &AddressSpace,
        pending: &Pending,
    ) -> Result<(), ()> {
        if let Some(fault) = pending.fault {
            self.last_fault = fault;
        }
        let fault = self.last_fault;
        if cpu.mode == Mode::Compat32 {
            return self.build_frame32(cpu, mem, pending, fault);
        }
        let signal = pending.signal;
        let action = self.action(signal);
        let (handler, flags, restorer, sa_mask) = (
            field(&action, 0),
            field(&action, 1),
            field(&action, 2),
            field(&action, 3),
        );
        if flags & SA_RESTORER == 0 {
            return Err(());
        }
        let rsp = cpu.regs[RSP];
        let on_stack = self.on_alt_stack(rsp);
        let mut sp = if flags & SA_ONSTACK != 0 && !self.alt_stack.disabled && !on_stack {
            self.alt_stack.sp.wrapping_add(self.alt_stack.size)
        } else {
            rsp.wrapping_sub(RED_ZONE)
        };
        // FPU state, 64-byte aligned, then the frame, so that the handler
        // starts with rsp + 8 aligned to 16, as after a call.
        sp = sp.wrapping_sub(FXSAVE_SIZE) & !63;
        let fpstate = sp;
        let frame = (sp.wrapping_sub(FRAME_SIZE) & !15).wrapping_sub(8);

        // The mask the handler's return restores: rt_sigsuspend's caller
        // mask if one is saved, otherwise the current one.
        let old_mask = self.saved_mask.take().unwrap_or(self.mask);
        let mut sigcontext = [0u8; SIGCONTEXT_SIZE];
        let regs = [
            cpu.regs[R8],
            cpu.regs[R9],
            cpu.regs[R10],
            cpu.regs[R11],
            cpu.regs[R12],
            cpu.regs[R13],
            cpu.regs[R14],
            cpu.regs[R15],
            cpu.regs[RDI],
            cpu.regs[RSI],
            cpu.regs[RBP],
            cpu.regs[RBX],
            cpu.regs[RDX],
            cpu.regs[RAX],
            cpu.regs[RCX],
            rsp,
            cpu.rip,
            cpu.rflags,
        ];
        for (i, value) in regs.iter().enumerate() {
            sigcontext[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        // cs, gs, fs, ss (16 bits each), err, trapno, oldmask, cr2, fpstate.
        sigcontext[144..146].copy_from_slice(&USER_CS.to_le_bytes());
        sigcontext[150..152].copy_from_slice(&USER_SS.to_le_bytes());
        for (at, value) in [
            (152, fault.err),
            (160, fault.trapno),
            (168, old_mask),
            (176, fault.cr2),
            (184, fpstate),
        ] {
            sigcontext[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }

        let mut bytes = vec![0u8; FRAME_SIZE as usize];
        let mut put = |at: u64, data: &[u8]| {
            bytes[at as usize..at as usize + data.len()].copy_from_slice(data);
        };
        put(0, &restorer.to_le_bytes());
        put(UC_FLAGS, &UC_FLAGS_VALUE.to_le_bytes());
        put(UC_LINK, &0u64.to_le_bytes());
        let stack_flags = if on_stack {
            SS_ONSTACK
        } else if self.alt_stack.disabled {
            SS_DISABLE
        } else {
            0
        };
        put(UC_STACK, &self.alt_stack.sp.to_le_bytes());
        put(UC_STACK + 8, &stack_flags.to_le_bytes());
        put(UC_STACK + 16, &self.alt_stack.size.to_le_bytes());
        put(UC_MCONTEXT, &sigcontext);
        put(UC_SIGMASK, &old_mask.to_le_bytes());
        put(FRAME_INFO, &pending.info);

        mem.write(fpstate, &cpu.fx_image()).map_err(|_| ())?;
        mem.write(frame, &bytes).map_err(|_| ())?;
        // The handler starts with the initial floating-point state; the
        // frame keeps the interrupted one for sigreturn.
        cpu.reset_fpu();

        cpu.regs[RDI] = signal;
        cpu.regs[RSI] = frame + FRAME_INFO;
        cpu.regs[RDX] = frame + UC_FLAGS;
        cpu.regs[RAX] = 0;
        cpu.regs[RSP] = frame;
        cpu.rip = handler;
        cpu.rflags &= !ENTRY_CLEARED_FLAGS;

        let mut mask = self.mask | sa_mask;
        if flags & SA_NODEFER == 0 {
            mask |= bit(signal);
        }
        if flags & SA_RESETHAND != 0 {
            let mut actions = self.actions();
            actions[(signal - 1) as usize][..8].fill(0);
            update_host(&actions, signal);
        }
        self.set_mask(mask);
        Ok(())
    }

    /// rt_sigreturn: restores the state saved by `build_frame`. The frame
    /// starts 8 bytes below rsp (the handler's `ret` popped pretcode).
    /// `Err` if the frame is unreadable or invalid (Linux: SIGSEGV).
    pub fn sigreturn(&mut self, cpu: &mut Cpu, mem: &AddressSpace) -> Result<(), String> {
        let uc = cpu.regs[RSP];
        let mut ucontext = [0u8; (UC_SIGMASK - UC_FLAGS + 8) as usize];
        mem.read(uc, &mut ucontext)
            .map_err(|e| format!("rt_sigreturn: unreadable frame ({e})"))?;
        let word = |at: u64| {
            u64::from_le_bytes(
                ucontext[at as usize..at as usize + 8]
                    .try_into()
                    .expect("8 bytes"),
            )
        };
        let sc = UC_MCONTEXT - UC_FLAGS;
        let reg = |i: u64| word(sc + i * 8);
        let fpstate = word(sc + 184);
        if fpstate != 0 {
            let mut image = [0u8; FXSAVE_SIZE as usize];
            mem.read(fpstate, &mut image)
                .map_err(|e| format!("rt_sigreturn: unreadable FPU state ({e})"))?;
            cpu.load_fx_image(&image)
                .map_err(|what| format!("rt_sigreturn: {what}"))?;
        }
        for (i, r) in [
            R8, R9, R10, R11, R12, R13, R14, R15, RDI, RSI, RBP, RBX, RDX, RAX, RCX, RSP,
        ]
        .into_iter()
        .enumerate()
        {
            cpu.regs[r] = reg(i as u64);
        }
        cpu.rip = reg(16);
        cpu.rflags = (cpu.rflags & !FIX_EFLAGS) | (reg(17) & FIX_EFLAGS);
        self.set_mask(word(UC_SIGMASK - UC_FLAGS));
        Ok(())
    }
}

/// The host signal set for guest set `blocked`, minus the signals armor
/// keeps.
pub(crate) fn host_set(blocked: u64) -> libc::sigset_t {
    // SAFETY: an all-zero sigset is valid; sigemptyset initializes it.
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: set is a valid sigset_t for these calls.
    unsafe {
        libc::sigemptyset(&mut set);
        for signal in 1..=NSIG {
            if blocked & bit(signal) != 0 && !reserved_on_host(signal) {
                libc::sigaddset(&mut set, signal as i32);
            }
        }
    }
    set
}

/// Signals whose default action is to do nothing.
fn default_ignores(signal: u64) -> bool {
    matches!(
        signal as i32,
        libc::SIGCHLD | libc::SIGURG | libc::SIGWINCH | libc::SIGCONT
    )
}

/// A `siginfo` for a signal the guest sends itself (`kill`, `tgkill`) or a
/// fault: signo, errno 0, code, then pid/uid or the fault address.
pub fn make_info(signal: u64, code: i32, first: u64) -> [u8; SIGINFO_SIZE] {
    let mut info = [0u8; SIGINFO_SIZE];
    info[..4].copy_from_slice(&(signal as i32).to_le_bytes());
    info[8..12].copy_from_slice(&code.to_le_bytes());
    info[16..24].copy_from_slice(&first.to_le_bytes());
    info
}

/// `si_pid` and `si_uid` packed as the first union field.
fn sender() -> u64 {
    // SAFETY: getpid and getuid have no preconditions.
    let (pid, uid) = unsafe { (libc::getpid(), libc::getuid()) };
    u64::from(pid as u32) | (u64::from(uid) << 32)
}

/// A signal the guest sends itself with kill (`thread` false) or tgkill.
/// For signals armor reserves on the host (SIGSEGV, ...), armor records it
/// itself instead of sending it.
pub fn send_to_self(signal: u64, thread: bool) -> bool {
    if signal == 0
        || signal > NSIG
        || !reserved_on_host(signal)
        || matches!(signal as i32, libc::SIGKILL | libc::SIGSTOP)
    {
        return false;
    }
    let code = if thread { SI_TKILL } else { SI_USER };
    let info = make_info(signal, code, sender());
    INFO.with(|table| {
        for (i, slot) in table[(signal - 1) as usize].iter().enumerate() {
            let word = u64::from_le_bytes(info[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
            slot.store(word, Ordering::Relaxed);
        }
    });
    PENDING.with(|p| p.fetch_or(bit(signal), Ordering::Release));
    true
}

/// The synchronous signal for an interpreter error, Linux's choice of
/// signal, si_code, si_addr and trap details; `None` for armor's own
/// limits (unsupported instructions). `cpu.rip` must still point at the
/// faulting instruction; for traps (int3) it is moved past it.
pub fn fault_signal(error: &CpuError, cpu: &mut Cpu, mem: &AddressSpace) -> Option<Pending> {
    let (signal, code, addr, fault) = match error {
        CpuError::Fault { fault, .. } => {
            let mapped = mem.prot(fault.addr).is_some();
            let write = fault.access == armor_mem::Access::Write;
            let exec = fault.access == armor_mem::Access::Execute;
            // Page-fault error code: present, write, user, instruction fetch.
            let err = u64::from(mapped) | (u64::from(write) << 1) | 4 | (u64::from(exec) << 4);
            let code = if mapped { SEGV_ACCERR } else { SEGV_MAPERR };
            (
                libc::SIGSEGV,
                code,
                fault.addr,
                FaultContext {
                    trapno: 14,
                    err,
                    cr2: fault.addr,
                },
            )
        }
        CpuError::Exception { rip, exception } => match exception {
            Exception::DivideError => (libc::SIGFPE, FPE_INTDIV, *rip, trap(0)),
            Exception::SimdFloatingPoint => (libc::SIGFPE, simd_code(cpu.mxcsr), *rip, trap(19)),
            Exception::GeneralProtection => (libc::SIGSEGV, SI_KERNEL, 0, trap(13)),
            Exception::Breakpoint(len) => {
                // int3 is a trap: execution resumes after it.
                cpu.rip = rip.wrapping_add(u64::from(*len));
                (libc::SIGTRAP, SI_KERNEL, 0, trap(3))
            }
            Exception::Debug => {
                cpu.rip = rip.wrapping_add(1);
                (libc::SIGTRAP, TRAP_BRKPT, 0, trap(1))
            }
        },
        CpuError::Decode {
            rip,
            error: DecodeError::Invalid,
            ..
        } => (libc::SIGILL, ILL_ILLOPN, *rip, trap(6)),
        CpuError::Decode {
            error: DecodeError::TooLong,
            ..
        } => (libc::SIGSEGV, SI_KERNEL, 0, trap(13)),
        CpuError::Decode { .. } | CpuError::Unsupported { .. } => return None,
    };
    Some(Pending {
        signal: signal as u64,
        info: make_info(signal as u64, code, addr),
        fault: Some(fault),
    })
}

/// `si_code` of a #XM, as Linux computes it from MXCSR: the first of the
/// unmasked exception flags in the order invalid, divide by zero,
/// overflow, underflow or denormal, precision (✅ Xeon X5675 for each,
/// `c/sse_exceptions`). Older flags still set count too, as in Linux.
fn simd_code(mxcsr: u32) -> i32 {
    let raised = mxcsr & !(mxcsr >> 7) & 0x3f;
    if raised & 0x01 != 0 {
        FPE_FLTINV
    } else if raised & 0x04 != 0 {
        FPE_FLTDIV
    } else if raised & 0x08 != 0 {
        FPE_FLTOVF
    } else if raised & 0x12 != 0 {
        FPE_FLTUND
    } else if raised & 0x20 != 0 {
        FPE_FLTRES
    } else {
        0
    }
}

fn trap(trapno: u64) -> FaultContext {
    FaultContext {
        trapno,
        err: 0,
        cr2: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `si_code` of a #XM, as an Intel Xeon X5675 under Linux gives
    /// them (`c/sse_exceptions`): from the unmasked flags only, in Linux's
    /// order.
    #[test]
    fn simd_codes_follow_linux() {
        // (MXCSR at the fault, si_code)
        for (mxcsr, code) in [
            (0x1d84, FPE_FLTDIV),
            (0x1f01, FPE_FLTINV),
            (0x1e82, FPE_FLTUND),
            (0x0fa2, FPE_FLTRES),
            (0x1b88, FPE_FLTOVF),
            (0x17b0, FPE_FLTUND),
            (0x0030, FPE_FLTUND),
            (0x0fa1, FPE_FLTRES),
            (0x1d85, FPE_FLTDIV),
        ] {
            assert_eq!(simd_code(mxcsr), code, "{mxcsr:#x}");
        }
    }
}
