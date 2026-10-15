//! x86_64 syscall dispatch. Numbers verified against
//! /usr/x86_64-linux-gnu/include/asm/unistd_64.h; constants and struct
//! layouts compared on both architectures in docs/tech/syscalls.md.
//!
//! Guest buffers always go through the checked memory path
//! (`GuestMemory`): armor copies them in or out of host buffers and never
//! hands a guest pointer to the host kernel.

use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use armor_cpu::{Cpu, R8, R9, R10, RAX, RDI, RDX, RSI};
use armor_mem::{
    AddressSpace, GUEST_PAGE_SIZE, GuestMemory, MapError, Prot, ProtectError, SharedSource,
};
use armor_x86::Mode;

use crate::events;
use crate::files::{self, AT_FDCWD};
use crate::net::{self, Word};
use crate::root::GuestRoot;
use crate::signal::{NSIG, SIGACTION_SIZE, Signals};

pub mod i386;

pub mod nr {
    pub const READ: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const OPEN: u64 = 2;
    pub const CLOSE: u64 = 3;
    pub const LSEEK: u64 = 8;
    pub const DUP: u64 = 32;
    pub const DUP2: u64 = 33;
    pub const SENDFILE: u64 = 40;
    pub const UNAME: u64 = 63;
    pub const FCNTL: u64 = 72;
    pub const GETCWD: u64 = 79;
    pub const GETUID: u64 = 102;
    pub const GETGID: u64 = 104;
    pub const SETUID: u64 = 105;
    pub const SETGID: u64 = 106;
    pub const GETEUID: u64 = 107;
    pub const GETEGID: u64 = 108;
    pub const GETGROUPS: u64 = 115;
    pub const PRCTL: u64 = 157;
    pub const TIME: u64 = 201;
    pub const GETDENTS64: u64 = 217;
    pub const OPENAT: u64 = 257;
    pub const FSTAT: u64 = 5;
    pub const PREAD64: u64 = 17;
    pub const PWRITE64: u64 = 18;
    pub const WRITEV: u64 = 20;
    pub const ACCESS: u64 = 21;
    pub const FACCESSAT: u64 = 269;
    pub const FACCESSAT2: u64 = 439;
    pub const MMAP: u64 = 9;
    pub const MPROTECT: u64 = 10;
    pub const PKEY_MPROTECT: u64 = 329;
    pub const PKEY_ALLOC: u64 = 330;
    pub const PKEY_FREE: u64 = 331;
    pub const MUNMAP: u64 = 11;
    pub const BRK: u64 = 12;
    pub const IOCTL: u64 = 16;
    pub const EXIT: u64 = 60;
    pub const ARCH_PRCTL: u64 = 158;
    pub const SET_TID_ADDRESS: u64 = 218;
    pub const EXIT_GROUP: u64 = 231;
    pub const NEWFSTATAT: u64 = 262;
    pub const READLINKAT: u64 = 267;
    pub const SET_ROBUST_LIST: u64 = 273;
    pub const PRLIMIT64: u64 = 302;
    pub const GETRANDOM: u64 = 318;
    pub const DUP3: u64 = 292;
    pub const RSEQ: u64 = 334;
    pub const UMASK: u64 = 95;
    pub const UNLINK: u64 = 87;
    pub const UNLINKAT: u64 = 263;
    pub const LANDLOCK_CREATE_RULESET: u64 = 444;
    pub const LANDLOCK_ADD_RULE: u64 = 445;
    pub const LANDLOCK_RESTRICT_SELF: u64 = 446;
    pub const RT_SIGACTION: u64 = 13;
    pub const RT_SIGPROCMASK: u64 = 14;
    pub const RT_SIGRETURN: u64 = 15;
    pub const GETPID: u64 = 39;
    pub const KILL: u64 = 62;
    pub const RT_SIGPENDING: u64 = 127;
    pub const SIGALTSTACK: u64 = 131;
    pub const GETTID: u64 = 186;
    pub const TKILL: u64 = 200;
    pub const TGKILL: u64 = 234;
    pub const PIPE: u64 = 22;
    pub const PAUSE: u64 = 34;
    pub const SETITIMER: u64 = 38;
    pub const PIPE2: u64 = 293;
    pub const MADVISE: u64 = 28;
    pub const MINCORE: u64 = 27;
    pub const RT_SIGTIMEDWAIT: u64 = 128;
    pub const RT_SIGQUEUEINFO: u64 = 129;
    pub const RT_TGSIGQUEUEINFO: u64 = 297;
    pub const READV: u64 = 19;
    pub const SCHED_YIELD: u64 = 24;
    pub const NANOSLEEP: u64 = 35;
    pub const GETITIMER: u64 = 36;
    pub const GETRLIMIT: u64 = 97;
    pub const SETRLIMIT: u64 = 160;
    pub const WAITID: u64 = 247;
    pub const PREADV: u64 = 295;
    pub const PWRITEV: u64 = 296;
    pub const PREADV2: u64 = 327;
    pub const PWRITEV2: u64 = 328;
    pub const TIMERFD_CREATE: u64 = 283;
    pub const TIMERFD_SETTIME: u64 = 286;
    pub const TIMERFD_GETTIME: u64 = 287;
    pub const CAPGET: u64 = 125;
    pub const CAPSET: u64 = 126;
    pub const FSYNC: u64 = 74;
    pub const FDATASYNC: u64 = 75;
    pub const GETRUSAGE: u64 = 98;
    pub const TIMES: u64 = 100;
    pub const MLOCK: u64 = 149;
    pub const MUNLOCK: u64 = 150;
    pub const MLOCKALL: u64 = 151;
    pub const MUNLOCKALL: u64 = 152;
    pub const SYNC: u64 = 162;
    pub const IOPRIO_SET: u64 = 251;
    pub const IOPRIO_GET: u64 = 252;
    pub const SYNC_FILE_RANGE: u64 = 277;
    pub const SYNCFS: u64 = 306;
    pub const GETCPU: u64 = 309;
    pub const MEMBARRIER: u64 = 324;
    pub const MREMAP: u64 = 25;
    pub const CLONE: u64 = 56;
    pub const CLONE3: u64 = 435;
    pub const FORK: u64 = 57;
    pub const VFORK: u64 = 58;
    pub const EXECVE: u64 = 59;
    pub const WAIT4: u64 = 61;
    pub const GETPPID: u64 = 110;
    pub const SETPGID: u64 = 109;
    pub const GETPGRP: u64 = 111;
    pub const SETSID: u64 = 112;
    pub const GETPGID: u64 = 121;
    pub const GETSID: u64 = 124;
    pub const GETPRIORITY: u64 = 140;
    pub const SETPRIORITY: u64 = 141;
    pub const TIMER_CREATE: u64 = 222;
    pub const TIMER_SETTIME: u64 = 223;
    pub const TIMER_GETTIME: u64 = 224;
    pub const TIMER_GETOVERRUN: u64 = 225;
    pub const TIMER_DELETE: u64 = 226;
    pub const RT_SIGSUSPEND: u64 = 130;
    pub const POLL: u64 = 7;
    pub const FLOCK: u64 = 73;
    pub const MEMFD_CREATE: u64 = 319;
    pub const CLOCK_GETRES: u64 = 229;
    pub const SHMGET: u64 = 29;
    pub const SHMAT: u64 = 30;
    pub const SHMCTL: u64 = 31;
    pub const SHMDT: u64 = 67;
    pub const FALLOCATE: u64 = 285;
    pub const GETRESUID: u64 = 118;
    pub const GETRESGID: u64 = 120;
    pub const SCHED_SETAFFINITY: u64 = 203;
    pub const SCHED_SETPARAM: u64 = 142;
    pub const SCHED_GETPARAM: u64 = 143;
    pub const SCHED_SETSCHEDULER: u64 = 144;
    pub const SCHED_GETSCHEDULER: u64 = 145;
    pub const SCHED_GET_PRIORITY_MAX: u64 = 146;
    pub const SCHED_GET_PRIORITY_MIN: u64 = 147;
    pub const SCHED_RR_GET_INTERVAL: u64 = 148;
    pub const SEMGET: u64 = 64;
    pub const SEMOP: u64 = 65;
    pub const SEMCTL: u64 = 66;
    pub const SEMTIMEDOP: u64 = 220;
    pub const PSELECT6: u64 = 270;
    pub const PPOLL: u64 = 271;
    pub const GETTIMEOFDAY: u64 = 96;
    pub const CHDIR: u64 = 80;
    pub const FCHDIR: u64 = 81;
    pub const READLINK: u64 = 89;
    pub const SYSINFO: u64 = 99;
    pub const STATFS: u64 = 137;
    pub const FSTATFS: u64 = 138;
    pub const SOCKET: u64 = 41;
    pub const CONNECT: u64 = 42;
    pub const ACCEPT: u64 = 43;
    pub const SENDTO: u64 = 44;
    pub const RECVFROM: u64 = 45;
    pub const SENDMSG: u64 = 46;
    pub const RECVMSG: u64 = 47;
    pub const SHUTDOWN: u64 = 48;
    pub const BIND: u64 = 49;
    pub const LISTEN: u64 = 50;
    pub const GETSOCKNAME: u64 = 51;
    pub const GETPEERNAME: u64 = 52;
    pub const SOCKETPAIR: u64 = 53;
    pub const SETSOCKOPT: u64 = 54;
    pub const GETSOCKOPT: u64 = 55;
    pub const ACCEPT4: u64 = 288;
    pub const LLISTXATTR: u64 = 195;
    pub const FUTEX: u64 = 202;
    pub const SCHED_GETAFFINITY: u64 = 204;
    pub const FADVISE64: u64 = 221;
    pub const CLOCK_GETTIME: u64 = 228;
    pub const CLOCK_NANOSLEEP: u64 = 230;
    pub const STATX: u64 = 332;
    pub const MSYNC: u64 = 26;
    pub const TRUNCATE: u64 = 76;
    pub const FTRUNCATE: u64 = 77;
    pub const STAT: u64 = 4;
    pub const LSTAT: u64 = 6;
    pub const SELECT: u64 = 23;
    pub const ALARM: u64 = 37;
    pub const GETDENTS: u64 = 78;
    pub const RENAME: u64 = 82;
    pub const MKDIR: u64 = 83;
    pub const RMDIR: u64 = 84;
    pub const CREAT: u64 = 85;
    pub const LINK: u64 = 86;
    pub const SYMLINK: u64 = 88;
    pub const CHMOD: u64 = 90;
    pub const CHOWN: u64 = 92;
    pub const LCHOWN: u64 = 94;
    pub const UTIME: u64 = 132;
    pub const UTIMES: u64 = 235;
    pub const FUTIMESAT: u64 = 261;
    pub const MKNOD: u64 = 133;
    pub const EPOLL_CREATE: u64 = 213;
    pub const EPOLL_WAIT: u64 = 232;
    pub const EPOLL_CTL: u64 = 233;
    pub const EPOLL_PWAIT: u64 = 281;
    pub const EPOLL_PWAIT2: u64 = 441;
    pub const EPOLL_CREATE1: u64 = 291;
    pub const EVENTFD: u64 = 284;
    pub const EVENTFD2: u64 = 290;
    pub const INOTIFY_INIT: u64 = 253;
    pub const INOTIFY_INIT1: u64 = 294;
    pub const INOTIFY_ADD_WATCH: u64 = 254;
    pub const INOTIFY_RM_WATCH: u64 = 255;
    pub const MKDIRAT: u64 = 258;
    pub const MKNODAT: u64 = 259;
    pub const FCHOWNAT: u64 = 260;
    pub const RENAMEAT: u64 = 264;
    pub const RENAMEAT2: u64 = 316;
    pub const LINKAT: u64 = 265;
    pub const SYMLINKAT: u64 = 266;
    pub const FCHMODAT: u64 = 268;
    pub const FCHMODAT2: u64 = 452;
    pub const UTIMENSAT: u64 = 280;
    pub const FCHMOD: u64 = 91;
    pub const FCHOWN: u64 = 93;
    pub const MODIFY_LDT: u64 = 154;
    pub const SIGNALFD: u64 = 282;
    pub const SIGNALFD4: u64 = 289;
    pub const USELIB: u64 = 134;
    pub const SYSCTL: u64 = 156;
    pub const USTAT: u64 = 136;
    pub const SYSFS: u64 = 139;
    pub const SET_THREAD_AREA: u64 = 205;
    pub const GET_THREAD_AREA: u64 = 211;
    pub const CREATE_MODULE: u64 = 174;
    pub const GET_KERNEL_SYMS: u64 = 177;
    pub const QUERY_MODULE: u64 = 178;
    pub const GETPMSG: u64 = 181;
    pub const PUTPMSG: u64 = 182;
    pub const AFS_SYSCALL: u64 = 183;
    pub const TUXCALL: u64 = 184;
    pub const SECURITY: u64 = 185;
    pub const VSERVER: u64 = 236;
    pub const EPOLL_CTL_OLD: u64 = 214;
    pub const EPOLL_WAIT_OLD: u64 = 215;
    pub const IOPL: u64 = 172;
    pub const IOPERM: u64 = 173;
    pub const URETPROBE: u64 = 335;
}

// fcntl commands, identical on both architectures.
const F_DUPFD: u64 = 0;
const F_GETFD: u64 = 1;
const F_SETFD: u64 = 2;
const F_GETFL: u64 = 3;
const F_SETFL: u64 = 4;
/// Record locks with a `struct flock` (32 bytes on x86_64 and AArch64,
/// the same layout: ✅ measured with `sizeof`/`offsetof`, 2026-10-14), and
/// the open file description locks with the same structure.
const F_ADD_SEALS: u64 = 1033;
const F_GET_SEALS: u64 = 1034;
pub(crate) const F_GETLK: u64 = 5;
pub(crate) const F_SETLK: u64 = 6;
pub(crate) const F_SETLKW: u64 = 7;
pub(crate) const F_OFD_GETLK: u64 = 36;
pub(crate) const F_OFD_SETLK: u64 = 37;
pub(crate) const F_OFD_SETLKW: u64 = 38;
const F_DUPFD_CLOEXEC: u64 = 1030;

// Open flags that differ between x86_64 (asm-generic) and AArch64
// (asm/fcntl.h), verified in both kernel headers; all others match.
const X86_O_DIRECT: u64 = 0o40000;
const X86_O_LARGEFILE: u64 = 0o100000;
const X86_O_DIRECTORY: u64 = 0o200000;
const X86_O_NOFOLLOW: u64 = 0o400000;
const ARM_O_DIRECTORY: u64 = 0o40000;
const ARM_O_NOFOLLOW: u64 = 0o100000;
const ARM_O_DIRECT: u64 = 0o200000;
const ARM_O_LARGEFILE: u64 = 0o400000;
const OPEN_FLAG_PAIRS: [(u64, u64); 4] = [
    (X86_O_DIRECT, ARM_O_DIRECT),
    (X86_O_LARGEFILE, ARM_O_LARGEFILE),
    (X86_O_DIRECTORY, ARM_O_DIRECTORY),
    (X86_O_NOFOLLOW, ARM_O_NOFOLLOW),
];
const OPEN_FLAG_MOVED: u64 = 0o740000;

/// Guest (x86_64) open flags to host (AArch64) open flags.
pub fn open_flags_to_host(flags: u64) -> u64 {
    OPEN_FLAG_PAIRS
        .iter()
        .fold(flags & !OPEN_FLAG_MOVED, |acc, &(x86, arm)| {
            if flags & x86 != 0 { acc | arm } else { acc }
        })
}

/// Host (AArch64) open flags to guest (x86_64) open flags, e.g. for
/// fcntl(F_GETFL).
pub fn open_flags_to_guest(flags: u64) -> u64 {
    OPEN_FLAG_PAIRS
        .iter()
        .fold(flags & !OPEN_FLAG_MOVED, |acc, &(x86, arm)| {
            if flags & arm != 0 { acc | x86 } else { acc }
        })
}

// prctl options, identical on both architectures.
const PR_SET_NAME: u64 = 15;
const PR_GET_NAME: u64 = 16;
const PR_CAPBSET_READ: u64 = 23;
const PR_GET_DUMPABLE: u64 = 3;
const PR_SET_DUMPABLE: u64 = 4;
const PR_SET_NO_NEW_PRIVS: u64 = 38;
/// `PR_SET_VMA` ("SVMA"): names anonymous memory, for diagnostics.
const PR_SET_VMA: u64 = 0x5356_4d41;
const PR_GET_NO_NEW_PRIVS: u64 = 39;
const TASK_COMM_LEN: usize = 16;

/// Syscalls of the x32 ABI carry this bit; armor does not implement x32.
const X32_SYSCALL_BIT: u64 = 0x4000_0000;

/// Largest single read/write the kernel performs (MAX_RW_COUNT).
const MAX_RW_COUNT: u64 = 0x7fff_f000;

/// Guest bytes are copied to the host in chunks of this size.
const CHUNK: usize = 64 * 1024;
/// The largest host buffer for one read: a regular file gives a read
/// everything it asks for, and programs rely on it (an AppImage's runtime
/// failed on a short pread of 85585 bytes); beyond this, a short read.
const READ_BUFFER: usize = 64 * 1024 * 1024;

const PATH_MAX: usize = 4096;

// arch_prctl codes (asm/prctl.h).
const ARCH_SET_GS: u64 = 0x1001;
const ARCH_SET_FS: u64 = 0x1002;
const ARCH_GET_FS: u64 = 0x1003;
const ARCH_GET_GS: u64 = 0x1004;

// ioctl requests with the same number and layout on x86_64 and AArch64.
const TCGETS: u64 = 0x5401;
const TIOCGWINSZ: u64 = 0x5413;
/// `int` arguments, the same numbers on x86_64, i386 and AArch64 (✅
/// measured, 2026-10-14).
const FIONREAD: u64 = 0x541b;
const FIONBIO: u64 = 0x5421;
const KERNEL_TERMIOS_SIZE: usize = 36;
const WINSIZE_SIZE: usize = 8;

// mmap flags, identical on both architectures.
const MAP_SHARED: u64 = 0x01;
/// PROT_WRITE, the same on both architectures.
const PROT_WRITE_BIT: u64 = 2;
const PROT_EXEC_BIT: u64 = 4;
const MAP_PRIVATE: u64 = 0x02;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;
const MAP_GROWSDOWN: u64 = 0x100;
/// Ignored by the kernel (both architectures); ld.so still passes it.
const MAP_DENYWRITE: u64 = 0x800;
/// Ignored by the kernel (both architectures).
const MAP_EXECUTABLE: u64 = 0x1000;
const MAP_NORESERVE: u64 = 0x4000;
const MAP_POPULATE: u64 = 0x8000;
const MAP_STACK: u64 = 0x2_0000;
const MAP_FIXED_NOREPLACE: u64 = 0x10_0000;

// Struct sizes, identical on x86_64 and AArch64 (docs/tech/syscalls.md).
const STATFS_SIZE: usize = 120;
const SYSINFO_SIZE: usize = 112;
const STATX_SIZE: usize = 256;
const TIMESPEC_SIZE: usize = 16;

// futex operations (low bits of the op argument), identical on both.
const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
const FUTEX_CMD_MASK: u64 = 0x7f;

/// The TLS a new thread or process gets with CLONE_SETTLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewTls {
    /// x86_64: the fs base.
    Base(u64),
    /// i386: a TLS entry of the GDT, from a `user_desc` (ADR-0029).
    Entry(usize, Option<armor_cpu::TlsEntry>),
}

impl NewTls {
    pub fn apply(self, cpu: &mut Cpu) {
        match self {
            NewTls::Base(base) => cpu.fs_base = base,
            NewTls::Entry(index, entry) => cpu.set_tls_entry(index, entry),
        }
    }
}

/// A thread created by clone, for the caller to start (crates/armor).
#[derive(Debug)]
pub struct NewThread {
    /// The child's registers: the parent's, with rax 0, the new stack and
    /// the new TLS.
    pub cpu: Cpu,
    pub thread: Thread,
    /// CLONE_PARENT_SETTID: where the parent stores the child's tid.
    pub parent_tid: Option<u64>,
    /// CLONE_CHILD_SETTID: where the child stores its own tid.
    pub child_tid: Option<u64>,
}

// clone flags, identical on both architectures.
const CLONE_VM: u64 = 0x100;
const CLONE_FS: u64 = 0x200;
const CLONE_FILES: u64 = 0x400;
const CLONE_SIGHAND: u64 = 0x800;
const CLONE_THREAD: u64 = 0x1_0000;
const CLONE_SYSVSEM: u64 = 0x4_0000;
const CLONE_SETTLS: u64 = 0x8_0000;
const CLONE_PARENT_SETTID: u64 = 0x10_0000;
const CLONE_CHILD_CLEARTID: u64 = 0x20_0000;
const CLONE_CHILD_SETTID: u64 = 0x100_0000;
/// What pthread_create passes (measured with qemu -strace, glibc 2.41).
const CLONE_THREAD_FLAGS: u64 = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;
const CLONE_THREAD_OPTIONAL: u64 =
    CLONE_SYSVSEM | CLONE_SETTLS | CLONE_PARENT_SETTID | CLONE_CHILD_CLEARTID | CLONE_CHILD_SETTID;

// madvise advice, identical on both.
const MADV_DONTNEED: u64 = 4;

/// Per-process state the syscalls need.
pub struct Process {
    /// Absolute path of the guest executable, returned for /proc/self/exe.
    pub exe_path: Vec<u8>,
    /// Where absolute guest paths are looked up first.
    pub root: GuestRoot,
    brk: Mutex<Brk>,
    /// armor's own executable, which execve runs for x86_64 programs.
    pub armor_exe: PathBuf,
    /// Guest threads alive (the last exit ends the process; fork needs 1).
    pub live_threads: AtomicUsize,
    /// Run with the JIT (`--jit`), or in lockstep (`--lockstep`); execve
    /// passes them on.
    pub jit: bool,
    pub lockstep: bool,
    /// armor's `--diagnose` option, which execve passes on too
    /// (ADR-0028).
    pub diagnose: Option<Vec<u8>>,
}

/// The program break.
struct Brk {
    start: u64,
    current: u64,
    /// End of the guest pages mapped for the heap.
    mapped: u64,
}

impl Process {
    /// Holds the brk lock (fork of a guest with several threads,
    /// `process.rs`).
    pub(crate) fn hold_brk(&self) -> impl Drop + '_ {
        self.brk.lock().expect("brk lock poisoned")
    }

    /// `brk_start` is the page-aligned end of the loaded segments.
    pub fn new(exe_path: Vec<u8>, brk_start: u64, root: GuestRoot, armor_exe: PathBuf) -> Process {
        Process {
            exe_path,
            root,
            armor_exe,
            live_threads: AtomicUsize::new(1),
            jit: false,
            lockstep: false,
            diagnose: None,
            brk: Mutex::new(Brk {
                start: brk_start,
                current: brk_start,
                mapped: brk_start,
            }),
        }
    }
}

/// Per-thread state the syscalls need (the process-wide state is in
/// `Process`, shared by every thread).
#[derive(Debug, Default)]
pub struct Thread {
    /// Signal mask, alternate stack, and the shared actions.
    pub signals: Signals,
    /// Address registered with set_tid_address or CLONE_CHILD_CLEARTID:
    /// zeroed and woken when the thread exits.
    pub clear_child_tid: u64,
}

#[derive(Debug)]
pub enum Outcome {
    /// The result is in rax: resume the guest.
    Continue,
    /// The guest process ends as if killed by this signal (for example a
    /// signal frame rt_sigreturn cannot read); the string says why.
    Fatal(i32, String),
    /// The guest process ends with this status (exit_group).
    Exit(i32),
    /// The calling thread ends with this status (exit); the process ends
    /// with it if this was the last thread.
    ExitThread(i32),
    /// clone created a thread: the caller starts it and stores its tid in
    /// the parent's rax.
    NewThread(Box<NewThread>),
    /// armor does not implement this syscall, or this use of it, yet.
    Unsupported(String),
}

/// A negative errno, as the raw syscall ABI returns it.
pub(crate) type SysResult = Result<i64, i64>;

pub(crate) fn err(errno: i32) -> i64 {
    -i64::from(errno)
}

pub(crate) fn host_errno() -> i64 {
    err(std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO))
}

/// Whether `dispatch` handles syscall `number` (possibly only some of its
/// uses, see docs/tech/syscalls.md). Kept in sync with `dispatch` by the
/// `implemented_list_matches_dispatch` test.
pub fn is_implemented(number: u64) -> bool {
    matches!(
        number,
        nr::READ
            | nr::RT_SIGTIMEDWAIT
            | nr::RT_SIGQUEUEINFO
            | nr::RT_TGSIGQUEUEINFO
            | nr::READV
            | nr::SCHED_YIELD
            | nr::NANOSLEEP
            | nr::GETITIMER
            | nr::GETRLIMIT
            | nr::SETRLIMIT
            | nr::WAITID
            | nr::PREADV
            | nr::PWRITEV
            | nr::PREADV2
            | nr::PWRITEV2
            | nr::TIMERFD_CREATE
            | nr::TIMERFD_SETTIME
            | nr::TIMERFD_GETTIME
            | nr::CAPGET
            | nr::CAPSET
            | nr::MINCORE
            | nr::FSYNC
            | nr::FDATASYNC
            | nr::SYNCFS
            | nr::SYNC
            | nr::SYNC_FILE_RANGE
            | nr::GETRUSAGE
            | nr::TIMES
            | nr::GETCPU
            | nr::MLOCK
            | nr::MUNLOCK
            | nr::MLOCKALL
            | nr::MUNLOCKALL
            | nr::MEMBARRIER
            | nr::IOPRIO_SET
            | nr::IOPRIO_GET
            | nr::PKEY_MPROTECT
            | nr::PKEY_ALLOC
            | nr::PKEY_FREE
            | nr::PREAD64
            | nr::PWRITE64
            | nr::WRITEV
            | nr::ACCESS
            | nr::FACCESSAT
            | nr::FACCESSAT2
            | nr::DUP
            | nr::DUP2
            | nr::DUP3
            | nr::FCNTL
            | nr::OPEN
            | nr::CLOSE
            | nr::LSEEK
            | nr::SENDFILE
            | nr::UNAME
            | nr::GETCWD
            | nr::GETUID
            | nr::GETGID
            | nr::SETUID
            | nr::SETGID
            | nr::GETEUID
            | nr::GETEGID
            | nr::GETGROUPS
            | nr::PRCTL
            | nr::TIME
            | nr::GETDENTS64
            | nr::OPENAT
            | nr::WRITE
            | nr::FSTAT
            | nr::NEWFSTATAT
            | nr::MMAP
            | nr::MUNMAP
            | nr::MPROTECT
            | nr::BRK
            | nr::IOCTL
            | nr::ARCH_PRCTL
            | nr::SET_TID_ADDRESS
            | nr::SET_ROBUST_LIST
            | nr::RSEQ
            | nr::UMASK
            | nr::UNLINK
            | nr::UNLINKAT
            | nr::LANDLOCK_CREATE_RULESET
            | nr::LANDLOCK_ADD_RULE
            | nr::LANDLOCK_RESTRICT_SELF
            | nr::READLINKAT
            | nr::PRLIMIT64
            | nr::GETRANDOM
            | nr::EXIT
            | nr::EXIT_GROUP
            | nr::RT_SIGACTION
            | nr::RT_SIGPROCMASK
            | nr::RT_SIGRETURN
            | nr::RT_SIGPENDING
            | nr::SIGALTSTACK
            | nr::GETPID
            | nr::GETTID
            | nr::KILL
            | nr::TKILL
            | nr::TGKILL
            | nr::PIPE
            | nr::PIPE2
            | nr::PAUSE
            | nr::SETITIMER
            | nr::MADVISE
            | nr::MREMAP
            | nr::CLONE
            | nr::CLONE3
            | nr::FORK
            | nr::VFORK
            | nr::EXECVE
            | nr::WAIT4
            | nr::GETPPID
            | nr::SETPGID
            | nr::GETPGRP
            | nr::SETSID
            | nr::GETPGID
            | nr::GETSID
            | nr::GETPRIORITY
            | nr::SETPRIORITY
            | nr::TIMER_CREATE
            | nr::TIMER_SETTIME
            | nr::TIMER_GETTIME
            | nr::TIMER_GETOVERRUN
            | nr::TIMER_DELETE
            | nr::RT_SIGSUSPEND
            | nr::POLL
            | nr::FLOCK
            | nr::MEMFD_CREATE
            | nr::CLOCK_GETRES
            | nr::SHMGET
            | nr::SHMAT
            | nr::SHMCTL
            | nr::SHMDT
            | nr::FALLOCATE
            | nr::GETRESUID
            | nr::GETRESGID
            | nr::SCHED_SETAFFINITY
            | nr::SCHED_SETPARAM
            | nr::SCHED_GETPARAM
            | nr::SCHED_SETSCHEDULER
            | nr::SCHED_GETSCHEDULER
            | nr::SCHED_GET_PRIORITY_MAX
            | nr::SCHED_GET_PRIORITY_MIN
            | nr::SCHED_RR_GET_INTERVAL
            | nr::SEMGET
            | nr::SEMOP
            | nr::SEMCTL
            | nr::SEMTIMEDOP
            | nr::PSELECT6
            | nr::PPOLL
            | nr::GETTIMEOFDAY
            | nr::CHDIR
            | nr::FCHDIR
            | nr::READLINK
            | nr::SYSINFO
            | nr::STATFS
            | nr::FSTATFS
            | nr::SOCKET
            | nr::CONNECT
            | nr::ACCEPT
            | nr::SENDTO
            | nr::RECVFROM
            | nr::SENDMSG
            | nr::RECVMSG
            | nr::SHUTDOWN
            | nr::BIND
            | nr::LISTEN
            | nr::GETSOCKNAME
            | nr::GETPEERNAME
            | nr::SOCKETPAIR
            | nr::SETSOCKOPT
            | nr::GETSOCKOPT
            | nr::ACCEPT4
            | nr::LLISTXATTR
            | nr::FUTEX
            | nr::SCHED_GETAFFINITY
            | nr::FADVISE64
            | nr::CLOCK_GETTIME
            | nr::CLOCK_NANOSLEEP
            | nr::STATX
            | nr::MSYNC
            | nr::TRUNCATE
            | nr::FTRUNCATE
            | nr::STAT
            | nr::LSTAT
            | nr::GETDENTS
            | nr::MKDIR
            | nr::MKDIRAT
            | nr::MKNOD
            | nr::MKNODAT
            | nr::RMDIR
            | nr::CREAT
            | nr::RENAME
            | nr::RENAMEAT
            | nr::RENAMEAT2
            | nr::LINK
            | nr::LINKAT
            | nr::SYMLINK
            | nr::SYMLINKAT
            | nr::CHMOD
            | nr::FCHMODAT
            | nr::FCHMODAT2
            | nr::FCHMOD
            | nr::CHOWN
            | nr::LCHOWN
            | nr::FCHOWNAT
            | nr::FCHOWN
            | nr::UTIME
            | nr::UTIMES
            | nr::FUTIMESAT
            | nr::UTIMENSAT
            | nr::SELECT
            | nr::ALARM
            | nr::EPOLL_CREATE
            | nr::EPOLL_CREATE1
            | nr::EPOLL_CTL
            | nr::EPOLL_WAIT
            | nr::EPOLL_PWAIT
            | nr::EPOLL_PWAIT2
            | nr::EVENTFD
            | nr::EVENTFD2
            | nr::INOTIFY_INIT
            | nr::INOTIFY_INIT1
            | nr::INOTIFY_ADD_WATCH
            | nr::INOTIFY_RM_WATCH
            | nr::MODIFY_LDT
            | nr::USELIB
            | nr::SYSCTL
            | nr::USTAT
            | nr::SYSFS
            | nr::SET_THREAD_AREA
            | nr::GET_THREAD_AREA
            | nr::CREATE_MODULE
            | nr::GET_KERNEL_SYMS
            | nr::QUERY_MODULE
            | nr::GETPMSG
            | nr::PUTPMSG
            | nr::AFS_SYSCALL
            | nr::TUXCALL
            | nr::SECURITY
            | nr::VSERVER
            | nr::EPOLL_CTL_OLD
            | nr::EPOLL_WAIT_OLD
            | nr::IOPL
            | nr::IOPERM
            | nr::URETPROBE
    )
}

/// Handles the syscall the guest just executed (number in rax, arguments
/// in rdi, rsi, rdx, r10, r8, r9) and stores the result in rax. An i386
/// guest's `int 0x80` goes to `i386::dispatch`.
pub fn dispatch(
    cpu: &mut Cpu,
    mem: &AddressSpace,
    process: &Process,
    thread: &mut Thread,
) -> Outcome {
    if cpu.mode == Mode::Compat32 {
        return i386::dispatch(cpu, mem, process, thread);
    }
    let number = cpu.regs[RAX];
    let a = [
        cpu.regs[RDI],
        cpu.regs[RSI],
        cpu.regs[RDX],
        cpu.regs[R10],
        cpu.regs[R8],
        cpu.regs[R9],
    ];
    let result: SysResult = match number {
        crate::host::HOST_CALL => crate::host::host_call(mem, a[0], a[1]),
        nr::READ => read(mem, a[0], a[1], a[2]),
        nr::PREAD64 => pread64(mem, a[0], a[1], a[2], a[3]),
        nr::PWRITE64 => pwrite64(mem, a[0], a[1], a[2], a[3]),
        nr::WRITE => Ok(write(mem, a[0], a[1], a[2])),
        nr::WRITEV => writev(mem, a[0], a[1], a[2], 8),
        nr::OPEN => openat(mem, process, libc::AT_FDCWD as u64, a[0], a[1], a[2]),
        nr::OPENAT => openat(mem, process, a[0], a[1], a[2], a[3]),
        nr::ACCESS => faccessat(mem, &process.root, libc::AT_FDCWD as u64, a[0], a[1], 0),
        nr::FACCESSAT => faccessat(mem, &process.root, a[0], a[1], a[2], 0),
        nr::FACCESSAT2 => faccessat(mem, &process.root, a[0], a[1], a[2], a[3]),
        nr::CLOSE => host(libc::SYS_close, &[a[0]]),
        nr::DUP => host(libc::SYS_dup, &[a[0]]),
        nr::DUP2 => dup2(a[0], a[1]),
        // O_CLOEXEC, the only dup3 flag, has the same value on both.
        nr::DUP3 => host(libc::SYS_dup3, &[a[0], a[1], a[2]]),
        nr::FCNTL
            if matches!(
                a[1],
                F_GETLK | F_SETLK | F_SETLKW | F_OFD_GETLK | F_OFD_SETLK | F_OFD_SETLKW
            ) =>
        {
            match copy_in::<32>(mem, a[2]) {
                Ok(raw) => record_lock(a[0], a[1], raw).and_then(|(r, out)| {
                    if matches!(a[1], F_GETLK | F_OFD_GETLK) {
                        copy_out(mem, a[2], &out)?;
                    }
                    Ok(r)
                }),
                Err(e) => Err(e),
            }
        }
        nr::FCNTL => match fcntl(a[0], a[1], a[2]) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("fcntl command {}", a[1])),
        },
        nr::LSEEK => host(libc::SYS_lseek, &[a[0], a[1], a[2]]),
        nr::GETDENTS64 => getdents64(mem, a[0], a[1], a[2]),
        nr::SENDFILE => sendfile(mem, a[0], a[1], a[2], a[3]),
        nr::UNAME => uname(mem, a[0]),
        nr::GETCWD => getcwd(mem, a[0], a[1]),
        nr::TIME => time(mem, a[0]),
        nr::GETUID => host(libc::SYS_getuid, &[]),
        nr::GETGID => host(libc::SYS_getgid, &[]),
        nr::GETEUID => host(libc::SYS_geteuid, &[]),
        nr::GETEGID => host(libc::SYS_getegid, &[]),
        nr::SETUID => host(libc::SYS_setuid, &[a[0]]),
        nr::SETGID => host(libc::SYS_setgid, &[a[0]]),
        nr::GETGROUPS => getgroups(mem, a[0], a[1]),
        nr::PRCTL => match prctl(mem, a[0], a[1], [a[2], a[3], a[4]]) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("prctl option {}", a[0])),
        },
        nr::FSTAT => fstat(mem, a[0], a[1]),
        nr::NEWFSTATAT => newfstatat(mem, &process.root, a[0], a[1], a[2], a[3]),
        nr::MMAP => match mmap(mem, a[0], a[1], a[2], a[3], a[4], a[5]) {
            Ok(result) => {
                trace_map(&result, a[1], a[2], a[3], a[4], a[5]);
                result
            }
            Err(what) => return Outcome::Unsupported(format!("mmap: {what}")),
        },
        nr::MUNMAP => munmap(mem, a[0], a[1]),
        nr::MPROTECT => match mprotect(mem, a[0], a[1], a[2]) {
            Ok(result) => result,
            Err(what) => return Outcome::Unsupported(what.to_string()),
        },
        // Memory protection keys: armor's CPU has no PKU, and Linux x86 then
        // fails pkey_alloc with EINVAL (measured on the Xeon X5675; qemu
        // answers ENOSYS); key -1 makes pkey_mprotect an mprotect.
        nr::PKEY_ALLOC | nr::PKEY_FREE => Err(err(libc::EINVAL)),
        nr::PKEY_MPROTECT if a[3] as i32 == -1 => match mprotect(mem, a[0], a[1], a[2]) {
            Ok(result) => result,
            Err(what) => return Outcome::Unsupported(what.to_string()),
        },
        nr::PKEY_MPROTECT => Err(err(libc::EINVAL)),
        nr::MSYNC => msync(mem, a[0], a[1], a[2]),
        nr::FTRUNCATE => host(libc::SYS_ftruncate, &[a[0], a[1]]),
        nr::TRUNCATE => files::truncate(mem, &process.root, a[0], a[1]),
        nr::BRK => Ok(brk(mem, process, a[0])),
        nr::IOCTL => match ioctl(mem, a[0], a[1], a[2]) {
            Some(result) => result,
            None => {
                return Outcome::Unsupported(format!("ioctl request {:#x}", a[1]));
            }
        },
        nr::ARCH_PRCTL => arch_prctl(cpu, mem, a[0], a[1]),
        nr::SET_TID_ADDRESS => {
            thread.clear_child_tid = a[0];
            // SAFETY: gettid has no preconditions.
            Ok(i64::from(unsafe { libc::gettid() }))
        }
        // Like qemu-x86_64: glibc works without robust futex lists and rseq.
        nr::SET_ROBUST_LIST | nr::RSEQ => Err(err(libc::ENOSYS)),
        // Landlock (xz's sandbox): not offered, as by qemu-x86_64; xz
        // then runs without it.
        nr::LANDLOCK_CREATE_RULESET | nr::LANDLOCK_ADD_RULE | nr::LANDLOCK_RESTRICT_SELF => {
            Err(err(libc::ENOSYS))
        }
        // The file mode creation mask of the process: armor's own, which
        // the guest's file creations go through.
        nr::UMASK => host(libc::SYS_umask, &[a[0] & 0o777]),
        nr::UNLINK => unlinkat(mem, &process.root, libc::AT_FDCWD as u64, a[0], 0),
        nr::UNLINKAT => unlinkat(mem, &process.root, a[0], a[1], a[2]),
        nr::READLINKAT => readlinkat(mem, process, a[0], a[1], a[2], a[3]),
        // Absent on AArch64: readlinkat(AT_FDCWD, ...).
        nr::READLINK => readlinkat(mem, process, libc::AT_FDCWD as u64, a[0], a[1], a[2]),
        nr::PRLIMIT64 => prlimit64(mem, a[0], a[1], a[2], a[3]),
        nr::GETRANDOM => getrandom(mem, a[0], a[1], a[2]),
        nr::RT_SIGACTION => rt_sigaction(mem, thread, a[0], a[1], a[2], a[3]),
        nr::RT_SIGPROCMASK => rt_sigprocmask(mem, thread, a[0], a[1], a[2], a[3]),
        nr::RT_SIGPENDING => rt_sigpending(mem, thread, a[0], a[1]),
        nr::SIGALTSTACK => sigaltstack(mem, thread, cpu.regs[armor_cpu::RSP], a[0], a[1]),
        // rt_sigreturn restores every register, rax included.
        nr::RT_SIGRETURN => {
            return match thread.signals.sigreturn(cpu, mem) {
                Ok(()) => Outcome::Continue,
                Err(why) => Outcome::Fatal(libc::SIGSEGV, why),
            };
        }
        nr::GETPID => host(libc::SYS_getpid, &[]),
        nr::GETTID => host(libc::SYS_gettid, &[]),
        nr::KILL => kill(a[0], a[1]),
        nr::TKILL => tgkill(own_pid(), a[0], a[1]),
        nr::TGKILL => tgkill(a[0], a[1], a[2]),
        nr::PIPE => pipe2(mem, a[0], 0),
        nr::PIPE2 => pipe2(mem, a[0], a[1]),
        // AArch64 has no pause(2): libc's pause waits the same way.
        // SAFETY: pause has no preconditions.
        nr::PAUSE => Err(if unsafe { libc::pause() } < 0 {
            host_errno()
        } else {
            err(libc::EINTR)
        }),
        nr::SETITIMER => setitimer(mem, a[0], a[1], a[2]),
        nr::SYSINFO => sysinfo(mem, a[0]),
        nr::STATFS => statfs(mem, &process.root, a[0], a[1]),
        nr::FSTATFS => fstatfs(mem, a[0], a[1]),
        // Domains, types and SOCK_NONBLOCK/SOCK_CLOEXEC match on both.
        nr::SOCKET => net::socket(a[0], a[1], a[2]),
        nr::CONNECT => net::connect(mem, a[0], a[1], a[2]),
        nr::BIND => net::bind(mem, a[0], a[1], a[2]),
        nr::LISTEN => net::listen(a[0], a[1]),
        nr::ACCEPT => net::accept4(mem, a[0], a[1], a[2], 0),
        nr::ACCEPT4 => net::accept4(mem, a[0], a[1], a[2], a[3]),
        nr::GETSOCKNAME => net::name(mem, false, a[0], a[1], a[2]),
        nr::GETPEERNAME => net::name(mem, true, a[0], a[1], a[2]),
        nr::SOCKETPAIR => net::socketpair(mem, a[0], a[1], a[2], a[3]),
        nr::SENDTO => net::sendto(mem, a[0], a[1], a[2], a[3], a[4], a[5]),
        nr::RECVFROM => net::recvfrom(mem, a[0], a[1], a[2], a[3], a[4], a[5]),
        nr::SENDMSG => net::sendmsg(mem, a[0], a[1], a[2], Word::W64),
        nr::RECVMSG => net::recvmsg(mem, a[0], a[1], a[2], Word::W64),
        nr::SHUTDOWN => net::shutdown(a[0], a[1]),
        nr::SETSOCKOPT => net::setsockopt(mem, a[0], a[1], a[2], a[3], a[4], Word::W64),
        nr::GETSOCKOPT => net::getsockopt(mem, a[0], a[1], a[2], a[3], a[4], Word::W64),
        nr::LLISTXATTR => llistxattr(mem, &process.root, a[0], a[1], a[2]),
        nr::FUTEX => match futex(mem, a[0], a[1], a[2], a[3], a[4], a[5], false) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("futex operation {}", a[1])),
        },
        nr::SCHED_GETAFFINITY => sched_getaffinity(mem, a[0], a[1], a[2]),
        // POSIX_FADV_* match on both; AArch64 calls it fadvise64 too.
        nr::FADVISE64 => host(libc::SYS_fadvise64, &[a[0], a[1], a[2], a[3]]),
        nr::CLOCK_GETTIME => clock_gettime(mem, a[0], a[1]),
        nr::CLOCK_NANOSLEEP => clock_nanosleep(mem, a[0], a[1], a[2], a[3]),
        nr::STATX => statx(mem, &process.root, a[0], a[1], a[2], a[3], a[4]),
        // Absent on AArch64 (docs/tech/syscalls.md): newfstatat with
        // AT_FDCWD, and the x86-64 struct stat.
        nr::STAT => newfstatat(mem, &process.root, AT_FDCWD, a[0], a[1], 0),
        nr::LSTAT => newfstatat(
            mem,
            &process.root,
            AT_FDCWD,
            a[0],
            a[1],
            libc::AT_SYMLINK_NOFOLLOW as u64,
        ),
        nr::GETDENTS => files::getdents(mem, a[0], a[1], a[2]),
        nr::MKDIR => files::mkdirat(mem, &process.root, AT_FDCWD, a[0], a[1]),
        nr::MKDIRAT => files::mkdirat(mem, &process.root, a[0], a[1], a[2]),
        nr::MKNOD => files::mknodat(mem, &process.root, AT_FDCWD, a[0], a[1], a[2]),
        nr::MKNODAT => files::mknodat(mem, &process.root, a[0], a[1], a[2], a[3]),
        nr::RMDIR => unlinkat(
            mem,
            &process.root,
            AT_FDCWD,
            a[0],
            libc::AT_REMOVEDIR as u64,
        ),
        // O_CREAT | O_WRONLY | O_TRUNC in x86-64 values, translated by openat.
        nr::CREAT => openat(mem, process, AT_FDCWD, a[0], 0o1101, a[1]),
        nr::RENAME => files::renameat2(mem, &process.root, AT_FDCWD, a[0], AT_FDCWD, a[1], 0),
        nr::RENAMEAT => files::renameat2(mem, &process.root, a[0], a[1], a[2], a[3], 0),
        nr::RENAMEAT2 => files::renameat2(mem, &process.root, a[0], a[1], a[2], a[3], a[4]),
        nr::LINK => files::linkat(mem, &process.root, AT_FDCWD, a[0], AT_FDCWD, a[1], 0),
        nr::LINKAT => files::linkat(mem, &process.root, a[0], a[1], a[2], a[3], a[4]),
        nr::SYMLINK => files::symlinkat(mem, &process.root, a[0], AT_FDCWD, a[1]),
        nr::SYMLINKAT => files::symlinkat(mem, &process.root, a[0], a[1], a[2]),
        nr::CHMOD => files::fchmodat(mem, &process.root, AT_FDCWD, a[0], a[1], 0),
        nr::FCHMODAT => files::fchmodat(mem, &process.root, a[0], a[1], a[2], 0),
        nr::FCHMODAT2 => files::fchmodat(mem, &process.root, a[0], a[1], a[2], a[3]),
        nr::FCHMOD => host(libc::SYS_fchmod, &[a[0], a[1]]),
        nr::CHOWN => files::fchownat(mem, &process.root, AT_FDCWD, a[0], a[1], a[2], 0),
        nr::LCHOWN => files::fchownat(
            mem,
            &process.root,
            AT_FDCWD,
            a[0],
            a[1],
            a[2],
            libc::AT_SYMLINK_NOFOLLOW as u64,
        ),
        nr::FCHOWNAT => files::fchownat(mem, &process.root, a[0], a[1], a[2], a[3], a[4]),
        nr::FCHOWN => host(libc::SYS_fchown, &[a[0], a[1], a[2]]),
        nr::UTIME => files::utime(mem, &process.root, a[0], a[1]),
        nr::UTIMES => files::futimesat(mem, &process.root, AT_FDCWD, a[0], a[1]),
        nr::FUTIMESAT => files::futimesat(mem, &process.root, a[0], a[1], a[2]),
        nr::UTIMENSAT => files::utimensat(mem, &process.root, a[0], a[1], a[2], a[3]),
        nr::SELECT => events::select(
            mem,
            Word::W64,
            a[0],
            a[1],
            a[2],
            a[3],
            a[4],
            events::Timeout::Timeval(Word::W64),
        ),
        nr::PSELECT6 => match events::pselect6(mem, Word::W64, Word::W64, a) {
            Some(result) => result,
            None => return Outcome::Unsupported("pselect6 with a signal mask".to_string()),
        },
        nr::PPOLL => ppoll(mem, thread, Word::W64, a),
        nr::ALARM => events::alarm(a[0]),
        nr::EPOLL_CREATE => events::epoll_create(a[0]),
        nr::EPOLL_CREATE1 => events::epoll_create1(a[0]),
        nr::EPOLL_CTL => events::epoll_ctl(mem, a[0], a[1], a[2], a[3]),
        nr::EPOLL_WAIT | nr::EPOLL_PWAIT | nr::EPOLL_PWAIT2 => {
            let sigmask = if number == nr::EPOLL_WAIT { 0 } else { a[4] };
            let timespec = number == nr::EPOLL_PWAIT2;
            match events::epoll_wait(mem, a[0], a[1], a[2], a[3], timespec, sigmask) {
                Some(result) => result,
                None => return Outcome::Unsupported("epoll_pwait with a signal mask".to_string()),
            }
        }
        nr::EVENTFD => events::eventfd2(a[0], 0),
        nr::EVENTFD2 => events::eventfd2(a[0], a[1]),
        nr::INOTIFY_INIT => events::inotify_init1(0),
        nr::INOTIFY_INIT1 => events::inotify_init1(a[0]),
        nr::INOTIFY_ADD_WATCH => events::inotify_add_watch(mem, &process.root, a[0], a[1], a[2]),
        nr::INOTIFY_RM_WATCH => host(libc::SYS_inotify_rm_watch, &[a[0], a[1]]),
        // modify_ldt: reading the LDT, which is empty (armor loads no
        // segments). Writing one is not supported.
        nr::MODIFY_LDT if a[0] == 0 => Ok(0),
        nr::MODIFY_LDT => {
            return Outcome::Unsupported(format!("modify_ldt function {}", a[0]));
        }
        // Obsolete or x86-only calls, answered as Linux and qemu-x86_64
        // answer them (docs/tech/syscalls.md): never implemented, removed
        // (uselib, _sysctl, sysfs, ustat), or meaningless without segment
        // registers and port I/O, which armor never grants.
        nr::USELIB
        | nr::SYSCTL
        | nr::USTAT
        | nr::SYSFS
        | nr::SET_THREAD_AREA
        | nr::GET_THREAD_AREA
        | nr::CREATE_MODULE
        | nr::GET_KERNEL_SYMS
        | nr::QUERY_MODULE
        | nr::GETPMSG
        | nr::PUTPMSG
        | nr::AFS_SYSCALL
        | nr::TUXCALL
        | nr::SECURITY
        | nr::VSERVER
        | nr::EPOLL_CTL_OLD
        | nr::EPOLL_WAIT_OLD
        | nr::IOPL
        | nr::IOPERM
        | nr::URETPROBE => Err(err(libc::ENOSYS)),
        nr::EXIT => return Outcome::ExitThread(a[0] as i32),
        nr::EXIT_GROUP => return Outcome::Exit(a[0] as i32),
        // x86_64 argument order: flags, stack, parent_tid, child_tid, tls.
        nr::CLONE => {
            let tls = (a[0] & CLONE_SETTLS != 0).then_some(NewTls::Base(a[4]));
            return clone_or_fork(cpu, mem, process, thread, [a[0], a[1], a[2], a[3]], tls);
        }
        nr::FORK | nr::VFORK => {
            let flags = if number == nr::FORK {
                crate::process::FORK_FLAGS
            } else {
                crate::process::VFORK_FLAGS
            };
            match crate::process::fork(cpu, mem, process, thread, flags, 0, 0, 0, None) {
                Ok(result) => result,
                Err(what) => return Outcome::Unsupported(what),
            }
        }
        nr::EXECVE => crate::process::execve(mem, process, a[0], a[1], a[2], 8),
        nr::WAIT4 => crate::process::wait4(mem, a[0], a[1], a[2], a[3], false),
        nr::GETPPID => host(libc::SYS_getppid, &[]),
        // Process groups, sessions and priorities: integers only, the same
        // on both architectures (getpriority returns 20 - nice, raw).
        nr::SETPGID => host(libc::SYS_setpgid, &[a[0], a[1]]),
        // AArch64 has no getpgrp: getpgid(0).
        nr::GETPGRP => host(libc::SYS_getpgid, &[0]),
        nr::SETSID => host(libc::SYS_setsid, &[]),
        nr::GETPGID => host(libc::SYS_getpgid, &[a[0]]),
        nr::GETSID => host(libc::SYS_getsid, &[a[0]]),
        nr::GETPRIORITY => host(libc::SYS_getpriority, &[a[0], a[1]]),
        nr::SETPRIORITY => host(libc::SYS_setpriority, &[a[0], a[1], a[2]]),
        nr::TIMER_CREATE => timer_create(mem, a[0], a[1], a[2]),
        nr::TIMER_SETTIME => timer_settime(mem, a[0], a[1], a[2], a[3]),
        nr::TIMER_GETTIME => timer_gettime(mem, a[0], a[1]),
        nr::TIMER_GETOVERRUN => host(libc::SYS_timer_getoverrun, &[a[0]]),
        nr::TIMER_DELETE => host(libc::SYS_timer_delete, &[a[0]]),
        nr::POLL => events::poll(mem, a[0], a[1], a[2]),
        nr::FLOCK => host(libc::SYS_flock, &[a[0], a[1]]),
        nr::MEMFD_CREATE => memfd_create(mem, a[0], a[1]),
        nr::CLOCK_GETRES => clock_getres(mem, a[0], a[1]),
        nr::SHMGET => crate::ipc::shmget(a[0], a[1], a[2]),
        nr::SHMAT => crate::ipc::shmat(mem, a[0], a[1], a[2]),
        nr::SHMDT => crate::ipc::shmdt(mem, a[0]),
        nr::SHMCTL => match crate::ipc::shmctl(mem, a[0], a[1], a[2]) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("shmctl command {}", a[1])),
        },
        // fallocate: integers, modes the same on both.
        nr::FALLOCATE => host(libc::SYS_fallocate, &[a[0], a[1], a[2], a[3]]),
        nr::GETRESUID | nr::GETRESGID => {
            getres_ids(mem, number == nr::GETRESUID, [a[0], a[1], a[2]])
        }
        nr::SCHED_SETAFFINITY => sched_setaffinity(mem, a[0], a[1], a[2]),
        nr::SCHED_SETPARAM | nr::SCHED_GETPARAM | nr::SCHED_SETSCHEDULER => {
            sched_param(mem, number, a[0], a[1], a[2])
        }
        nr::SCHED_GETSCHEDULER => host(libc::SYS_sched_getscheduler, &[a[0]]),
        nr::SCHED_GET_PRIORITY_MAX => host(libc::SYS_sched_get_priority_max, &[a[0]]),
        nr::SCHED_GET_PRIORITY_MIN => host(libc::SYS_sched_get_priority_min, &[a[0]]),
        nr::SCHED_RR_GET_INTERVAL => sched_rr_get_interval(a[0]).and_then(|ts| {
            copy_out(mem, a[1], &ts)?;
            Ok(0)
        }),
        nr::SEMGET => crate::ipc::semget(a[0], a[1], a[2]),
        nr::SEMOP => crate::ipc::semtimedop(mem, a[0], a[1], a[2], 0, Word::W64),
        nr::SEMTIMEDOP => crate::ipc::semtimedop(mem, a[0], a[1], a[2], a[3], Word::W64),
        nr::SEMCTL => match crate::ipc::semctl(mem, a[0], a[1], a[2], a[3]) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("semctl command {}", a[2])),
        },
        nr::GETTIMEOFDAY => gettimeofday(mem, a[0], a[1]),
        nr::CHDIR => chdir(mem, &process.root, a[0]),
        nr::FCHDIR => host(libc::SYS_fchdir, &[a[0]]),
        nr::RT_SIGSUSPEND => {
            if a[1] != 8 {
                Err(err(libc::EINVAL))
            } else {
                match copy_in::<8>(mem, a[0]) {
                    Ok(set) => Err(thread.signals.suspend(u64::from_le_bytes(set))),
                    Err(e) => Err(e),
                }
            }
        }
        // glibc tries clone3 first and falls back to clone on ENOSYS.
        nr::CLONE3 => Err(err(libc::ENOSYS)),
        nr::MADVISE => madvise(mem, a[0], a[1], a[2]),
        nr::MINCORE => mincore(mem, a[0], a[1], a[2]),
        nr::RT_SIGTIMEDWAIT => rt_sigtimedwait(mem, thread, a[0], a[1], a[2], a[3]),
        nr::RT_SIGQUEUEINFO => queue_signal(mem, None, a[0], a[1], a[2]),
        nr::RT_TGSIGQUEUEINFO => queue_signal(mem, Some(a[0]), a[1], a[2], a[3]),
        nr::SCHED_YIELD => host(libc::SYS_sched_yield, &[]),
        // Linux's nanosleep sleeps on CLOCK_MONOTONIC.
        nr::NANOSLEEP => clock_nanosleep(mem, libc::CLOCK_MONOTONIC as u64, 0, a[0], a[1]),
        nr::GETRLIMIT => prlimit64(mem, 0, a[0], 0, a[1]),
        nr::SETRLIMIT => prlimit64(mem, 0, a[0], a[1], 0),
        nr::GETITIMER => getitimer(mem, a[0], a[1]),
        nr::WAITID => waitid(mem, a[0], a[1], a[2], a[3], a[4]),
        nr::READV => vector_io(mem, a[0], a[1], a[2], None, 0, false),
        nr::PREADV => vector_io(mem, a[0], a[1], a[2], Some(a[3]), 0, false),
        nr::PREADV2 => vector_io(mem, a[0], a[1], a[2], offset2(a[3]), a[5], false),
        nr::PWRITEV => vector_io(mem, a[0], a[1], a[2], Some(a[3]), 0, true),
        nr::PWRITEV2 => vector_io(mem, a[0], a[1], a[2], offset2(a[3]), a[5], true),
        nr::TIMERFD_CREATE => host(libc::SYS_timerfd_create, &[a[0], a[1]]),
        nr::TIMERFD_SETTIME => timerfd_settime(mem, a[0], a[1], a[2], a[3]),
        nr::TIMERFD_GETTIME => timerfd_gettime(mem, a[0], a[1]),
        nr::CAPGET => capabilities(mem, false, a[0], a[1]),
        nr::CAPSET => capabilities(mem, true, a[0], a[1]),
        nr::FSYNC => host(libc::SYS_fsync, &[a[0]]),
        nr::FDATASYNC => host(libc::SYS_fdatasync, &[a[0]]),
        nr::SYNCFS => host(libc::SYS_syncfs, &[a[0]]),
        nr::SYNC => host(libc::SYS_sync, &[]),
        nr::SYNC_FILE_RANGE => host(libc::SYS_sync_file_range, &[a[0], a[1], a[2], a[3]]),
        nr::GETRUSAGE => getrusage(mem, a[0], a[1]),
        nr::TIMES => times(mem, a[0]),
        nr::GETCPU => getcpu(mem, a[0], a[1]),
        // The ranges are guest addresses, which are host addresses (armor's
        // identity mapping): the kernel only walks its page tables.
        nr::MLOCK => host(libc::SYS_mlock, &[a[0], a[1]]),
        nr::MUNLOCK => host(libc::SYS_munlock, &[a[0], a[1]]),
        nr::MLOCKALL => host(libc::SYS_mlockall, &[a[0]]),
        nr::MUNLOCKALL => host(libc::SYS_munlockall, &[]),
        nr::MEMBARRIER => host(libc::SYS_membarrier, &[a[0], a[1], a[2]]),
        nr::IOPRIO_SET => host(libc::SYS_ioprio_set, &[a[0], a[1], a[2]]),
        nr::IOPRIO_GET => host(libc::SYS_ioprio_get, &[a[0], a[1]]),
        nr::MREMAP => match mremap(mem, a[0], a[1], a[2], a[3]) {
            Ok(result) => result,
            Err(what) => return Outcome::Unsupported(format!("mremap: {what}")),
        },
        n if n & X32_SYSCALL_BIT != 0 => Err(err(libc::ENOSYS)),
        n => return Outcome::Unsupported(format!("syscall {n}")),
    };
    cpu.regs[RAX] = result.unwrap_or_else(|e| e) as u64;
    Outcome::Continue
}

/// The result the last syscall left in rax: eax sign-extended for an
/// i386 guest.
pub fn result(cpu: &Cpu) -> i64 {
    match cpu.mode {
        Mode::Long => cpu.regs[RAX] as i64,
        Mode::Compat32 => i64::from(cpu.regs[RAX] as u32 as i32),
    }
}

/// Whether Linux restarts syscall `number` of a guest in `mode`,
/// interrupted by a signal whose handler has SA_RESTART. Linux decides per
/// call (ERESTARTSYS); this list covers the implemented syscalls that can
/// block ⚠️ (docs/tech/signals.md).
pub fn restartable(mode: Mode, number: u64) -> bool {
    if mode == Mode::Compat32 {
        return i386::restartable(number);
    }
    matches!(
        number,
        nr::READ
            | nr::WRITE
            | nr::PREAD64
            | nr::PWRITE64
            | nr::OPEN
            | nr::OPENAT
            | nr::IOCTL
            | nr::SENDFILE
            | nr::ACCEPT
            | nr::ACCEPT4
            | nr::RECVFROM
            | nr::RECVMSG
            | nr::SENDTO
            | nr::SENDMSG
            | nr::FLOCK
            | nr::FCNTL
    )
}

/// A host syscall whose arguments are plain values (no guest pointers).
pub(crate) fn host(number: libc::c_long, args: &[u64]) -> SysResult {
    let arg = |i: usize| args.get(i).copied().unwrap_or(0);
    // SAFETY: callers only pass syscalls whose arguments are integers, so
    // the host kernel never dereferences a guest address.
    let result = unsafe { libc::syscall(number, arg(0), arg(1), arg(2), arg(3)) };
    if result < 0 {
        Err(host_errno())
    } else {
        Ok(result)
    }
}

/// dup2(2), absent on AArch64: dup3 without flags, except that
/// dup2(fd, fd) returns fd (after checking it is open) where dup3 fails.
fn dup2(old: u64, new: u64) -> SysResult {
    if old == new {
        return host(libc::SYS_fcntl, &[old, F_GETFD]).map(|_| old as i64);
    }
    host(libc::SYS_dup3, &[old, new, 0])
}

/// fcntl(2) for descriptor flags, status flags (translated between the
/// two architectures' O_* values) and duplication; `None` for other
/// commands (locks, leases, ...).
fn fcntl(fd: u64, cmd: u64, arg: u64) -> Option<SysResult> {
    Some(match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC | F_GETFD | F_SETFD => host(libc::SYS_fcntl, &[fd, cmd, arg]),
        F_GETFL => {
            host(libc::SYS_fcntl, &[fd, cmd]).map(|flags| open_flags_to_guest(flags as u64) as i64)
        }
        F_SETFL => host(libc::SYS_fcntl, &[fd, cmd, open_flags_to_host(arg)]),
        // Seals of a memfd: integers, the same values on both.
        F_ADD_SEALS | F_GET_SEALS => host(libc::SYS_fcntl, &[fd, cmd, arg]),
        _ => return None,
    })
}

/// memfd_create(2): an anonymous file in memory. The name is read from
/// the guest; the flags (`MFD_CLOEXEC`, `MFD_ALLOW_SEALING`, `MFD_HUGETLB`,
/// `MFD_NOEXEC_SEAL`, `MFD_EXEC`) have the same values on x86_64, i386 and
/// AArch64 (✅ measured, 2026-10-14). Mapping it is `MAP_SHARED` of a file
/// (ADR-0022).
pub(crate) fn memfd_create(mem: &AddressSpace, name: u64, flags: u64) -> SysResult {
    let name = read_path(mem, name)?;
    // SAFETY: name is NUL-terminated; flags is an integer.
    let fd = unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), flags as u32) };
    if fd < 0 { Err(host_errno()) } else { Ok(fd) }
}

/// A record lock command (`F_GETLK` & co.) with the 32-byte `struct
/// flock` of x86_64 and AArch64: the host's fcntl on a local copy. Returns
/// the result and the structure after the call (F_GETLK fills it in).
pub(crate) fn record_lock(fd: u64, cmd: u64, raw: [u8; 32]) -> Result<(i64, [u8; 32]), i64> {
    let mut lock = raw;
    // SAFETY: `lock` is a 32-byte struct flock (same layout on the host),
    // readable and writable for the call.
    let r = unsafe { libc::fcntl(fd as i32, cmd as i32, lock.as_mut_ptr()) };
    if r < 0 {
        Err(host_errno())
    } else {
        Ok((i64::from(r), lock))
    }
}

/// ioctls whose argument is a structure of fixed-width fields, with no
/// pointer, the same on x86_64 and AArch64 (✅ the uapi headers of both,
/// 2026-10-14): request, size, and whether the kernel writes it back. The
/// structure goes through a host copy.
const SIOCETHTOOL: u64 = 0x8946;

const PLAIN_IOCTLS: &[(u64, usize, bool)] = &[
    // UDMABUF_CREATE: a dma-buf from a memfd (memfd, flags, offset, size).
    (0x4018_7542, 24, false),
    // DMA_BUF_IOCTL_SYNC (u64 flags), EXPORT_SYNC_FILE and
    // IMPORT_SYNC_FILE (u32 flags, s32 fd): Mesa sharing images with the
    // X server.
    (0x4008_6200, 8, false),
    (0xc008_6202, 8, true),
    (0x4008_6203, 8, false),
    // SIOCGIFNAME: an interface's name from its index, in a 40-byte
    // `struct ifreq` (name, then the index at 16), identical on both
    // architectures (Chromium's network code).
    (0x8910, 40, true),
    // SIOCGIWNAME: whether an interface is wireless (its protocol name in
    // a 32-byte `struct iwreq`; this request uses no pointer).
    (0x8b01, 32, true),
    // F2FS_IOC_GET_FEATURES: a u32 (Chromium asks whether its cache is on
    // f2fs; other file systems answer ENOTTY).
    (0x8004_f50c, 4, true),
    // TIOCGPGRP and TIOCSPGRP: a terminal's foreground process group, an
    // int (bash asks it at start).
    (0x540f, 4, true),
    (0x5410, 4, false),
];

fn plain_ioctl(
    mem: &AddressSpace,
    fd: u64,
    request: u64,
    arg: u64,
    size: usize,
    reads: bool,
) -> SysResult {
    let mut data = vec![0u8; size];
    mem.read(arg, &mut data).map_err(|_| err(libc::EFAULT))?;
    // SAFETY: `data` holds the structure the request takes, `size` bytes.
    let r = unsafe { libc::ioctl(fd as i32, request as libc::c_ulong, data.as_mut_ptr()) };
    if r < 0 {
        return Err(host_errno());
    }
    if reads {
        copy_out(mem, arg, &data)?;
    }
    Ok(i64::from(r))
}

/// DRM_IOCTL_VERSION ('d', 0x00, read-write, 64 bytes).
const DRM_IOCTL_VERSION: u64 = 0xc040_6400;

/// DRM_IOCTL_VERSION on a GPU device (the guest's Mesa asks it which
/// driver to load): `struct drm_version` has three ints, then three
/// (length, pointer) pairs for the driver's name, date and description,
/// 64 bytes with the same layout on x86_64 and AArch64. The strings go
/// through host buffers of the guest's lengths (at most 4096 bytes each);
/// the lengths the kernel writes back are the full ones, as in Linux.
fn drm_version(mem: &AddressSpace, fd: u64, arg: u64) -> SysResult {
    const MAX_STRING: usize = 4096;
    let mut raw = copy_in::<64>(mem, arg)?;
    let field = |raw: &[u8; 64], at: usize| u64::from_le_bytes(raw[at..at + 8].try_into().unwrap());
    let mut buffers: Vec<Vec<u8>> = [16, 32, 48]
        .iter()
        .map(|&at| vec![0u8; (field(&raw, at) as usize).min(MAX_STRING)])
        .collect();
    let guest_ptrs: Vec<u64> = [24, 40, 56].iter().map(|&at| field(&raw, at)).collect();
    let mut host = raw;
    for (i, buffer) in buffers.iter_mut().enumerate() {
        let at = 16 + 16 * i;
        host[at..at + 8].copy_from_slice(&(buffer.len() as u64).to_le_bytes());
        host[at + 8..at + 16].copy_from_slice(&(buffer.as_mut_ptr() as u64).to_le_bytes());
    }
    // SAFETY: `host` is a struct drm_version whose pointers are host
    // buffers of the lengths it gives.
    let r = unsafe {
        libc::ioctl(
            fd as i32,
            DRM_IOCTL_VERSION as libc::c_ulong,
            host.as_mut_ptr(),
        )
    };
    if r < 0 {
        return Err(host_errno());
    }
    raw[..16].copy_from_slice(&host[..16]);
    for (i, buffer) in buffers.iter().enumerate() {
        let at = 16 + 16 * i;
        let full = field(&host, at) as usize;
        if guest_ptrs[i] != 0 {
            copy_out(mem, guest_ptrs[i], &buffer[..full.min(buffer.len())])?;
        }
        raw[at..at + 8].copy_from_slice(&(full as u64).to_le_bytes());
    }
    copy_out(mem, arg, &raw)?;
    Ok(i64::from(r))
}

/// read(2) into a host buffer, then copied to the guest. At most
/// `READ_BUFFER` bytes per call.
fn read(mem: &AddressSpace, fd: u64, buf: u64, count: u64) -> SysResult {
    let mut bytes = vec![0u8; (count as usize).min(READ_BUFFER)];
    // SAFETY: bytes is writable for its length.
    let n = unsafe { libc::read(fd as i32, bytes.as_mut_ptr().cast(), bytes.len()) };
    if n < 0 {
        return Err(host_errno());
    }
    copy_out(mem, buf, &bytes[..n as usize])?;
    Ok(n as i64)
}

/// pread64(2), like `read` at an offset.
fn pread64(mem: &AddressSpace, fd: u64, buf: u64, count: u64, offset: u64) -> SysResult {
    let mut bytes = vec![0u8; (count as usize).min(READ_BUFFER)];
    // SAFETY: bytes is writable for its length.
    let n = unsafe {
        libc::pread(
            fd as i32,
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            offset as libc::off_t,
        )
    };
    if n < 0 {
        return Err(host_errno());
    }
    copy_out(mem, buf, &bytes[..n as usize])?;
    Ok(n as i64)
}

/// openat(2). `/proc/self/exe` opens the guest's program, not armor (the
/// AppImage runtime reads itself through it).
pub(crate) fn openat(
    mem: &AddressSpace,
    process: &Process,
    dirfd: u64,
    path: u64,
    flags: u64,
    mode: u64,
) -> SysResult {
    let guest = read_path(mem, path)?;
    let path = if is_self_exe(guest.as_bytes()) {
        std::ffi::CString::new(process.exe_path.clone()).map_err(|_| err(libc::ENOENT))?
    } else {
        process.root.resolve_c(guest)
    };
    // SAFETY: path is NUL-terminated; flags are translated to the host's
    // values and mode is a plain integer.
    let fd = unsafe {
        libc::openat(
            dirfd as i32,
            path.as_ptr(),
            open_flags_to_host(flags) as i32,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        Err(host_errno())
    } else {
        Ok(i64::from(fd))
    }
}

/// getdents64(2): `struct linux_dirent64` has the same layout on both
/// architectures, so the records are copied as they are.
fn getdents64(mem: &AddressSpace, fd: u64, buf: u64, count: u64) -> SysResult {
    let mut bytes = vec![0u8; (count as usize).min(CHUNK)];
    // SAFETY: bytes is writable for its length.
    let n = unsafe {
        libc::syscall(
            libc::SYS_getdents64,
            fd as i32,
            bytes.as_mut_ptr(),
            bytes.len(),
        )
    };
    if n < 0 {
        return Err(host_errno());
    }
    copy_out(mem, buf, &bytes[..n as usize])?;
    Ok(n)
}

fn sendfile(mem: &AddressSpace, out_fd: u64, in_fd: u64, offset: u64, count: u64) -> SysResult {
    let mut position = if offset != 0 {
        Some(i64::from_le_bytes(copy_in::<8>(mem, offset)?))
    } else {
        None
    };
    let position_ptr = position
        .as_mut()
        .map_or(std::ptr::null_mut(), |p| p as *mut i64);
    // SAFETY: position_ptr is null or points to a local i64.
    let n = unsafe { libc::sendfile(out_fd as i32, in_fd as i32, position_ptr, count as usize) };
    if n < 0 {
        return Err(host_errno());
    }
    if let Some(position) = position {
        copy_out(mem, offset, &position.to_le_bytes())?;
    }
    Ok(n as i64)
}

/// uname(2) of the host with `machine` set to "x86_64", as on an x86_64
/// kernel (and as qemu-x86_64 reports).
fn uname(mem: &AddressSpace, buf: u64) -> SysResult {
    // SAFETY: an all-zero `utsname` is a valid value of this plain C struct.
    let mut names: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: names is a valid, writable utsname.
    if unsafe { libc::uname(&mut names) } < 0 {
        return Err(host_errno());
    }
    // c_char is u8 on AArch64, the only host armor targets (ADR-0002).
    let field = |chars: &[libc::c_char]| chars.to_vec();
    let mut machine = [0u8; 65];
    machine[..6].copy_from_slice(b"x86_64");
    // Six 65-byte fields, the same layout on both architectures.
    let mut out = Vec::with_capacity(390);
    out.extend(field(&names.sysname));
    out.extend(field(&names.nodename));
    out.extend(field(&names.release));
    out.extend(field(&names.version));
    out.extend(machine);
    out.extend(field(&names.domainname));
    copy_out(mem, buf, &out)?;
    Ok(0)
}

fn getcwd(mem: &AddressSpace, buf: u64, size: u64) -> SysResult {
    let mut bytes = vec![0u8; (size as usize).min(PATH_MAX)];
    // SAFETY: bytes is writable for its length.
    let n = unsafe { libc::syscall(libc::SYS_getcwd, bytes.as_mut_ptr(), bytes.len()) };
    if n < 0 {
        return Err(host_errno());
    }
    // The raw syscall returns the length including the terminating NUL.
    copy_out(mem, buf, &bytes[..n as usize])?;
    Ok(n)
}

fn time(mem: &AddressSpace, tloc: u64) -> SysResult {
    // SAFETY: a null argument asks time() only for its return value.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    if tloc != 0 {
        copy_out(mem, tloc, &now.to_le_bytes())?;
    }
    Ok(now)
}

/// capget(2) and capset(2) (`set`): an 8-byte header (version, pid) and
/// one 12-byte record (version 1) or two (versions 2 and 3), the same on
/// both architectures. The kernel writes its preferred version back into
/// the header when it does not know the guest's.
fn capabilities(mem: &AddressSpace, set: bool, header: u64, data: u64) -> SysResult {
    const VERSION_1: u32 = 0x1998_0330;
    let mut head = copy_in::<8>(mem, header)?;
    let version = u32::from_le_bytes(head[..4].try_into().expect("4 bytes"));
    let records = if version == VERSION_1 { 1 } else { 2 };
    let mut body = [0u8; 24];
    if set {
        mem.read(data, &mut body[..12 * records])
            .map_err(|_| err(libc::EFAULT))?;
    }
    let (number, data_ptr) = match (set, data) {
        (true, _) => (libc::SYS_capset, body.as_mut_ptr()),
        (false, 0) => (libc::SYS_capget, std::ptr::null_mut()),
        (false, _) => (libc::SYS_capget, body.as_mut_ptr()),
    };
    // SAFETY: head is the 8-byte header; data_ptr is null or 24 bytes, room
    // for two records.
    let r = unsafe { libc::syscall(number, head.as_mut_ptr(), data_ptr) };
    let failed = (r < 0).then(host_errno);
    if !set {
        copy_out(mem, header, &head)?;
        if failed.is_none() && data != 0 {
            copy_out(mem, data, &body[..12 * records])?;
        }
    }
    match failed {
        Some(e) => Err(e),
        None => Ok(0),
    }
}

/// preadv2/pwritev2's offset: -1 means the file's current position.
fn offset2(offset: u64) -> Option<u64> {
    (offset as i64 != -1).then_some(offset)
}

/// readv, preadv, preadv2 (`write` false) and pwritev, pwritev2 (`write`
/// true) on x86_64: the guest's iovecs (16 bytes each) checked as Linux
/// does, the data moved through one host buffer, at `offset` if given.
/// A read stops at the first guest byte it cannot write, returning what it
/// copied before it (EFAULT if nothing).
fn vector_io(
    mem: &AddressSpace,
    fd: u64,
    iov: u64,
    iovcnt: u64,
    offset: Option<u64>,
    flags: u64,
    write: bool,
) -> SysResult {
    if iovcnt > IOV_MAX {
        return Err(err(libc::EINVAL));
    }
    let mut vector = vec![0u8; iovcnt as usize * 16];
    mem.read(iov, &mut vector).map_err(|_| err(libc::EFAULT))?;
    let mut segments = Vec::with_capacity(iovcnt as usize);
    let mut total: u64 = 0;
    for entry in vector.as_chunks::<16>().0 {
        let base = u64::from_le_bytes(entry[..8].try_into().expect("8 bytes"));
        let len = u64::from_le_bytes(entry[8..].try_into().expect("8 bytes"));
        if (len as i64) < 0 {
            return Err(err(libc::EINVAL));
        }
        if base.checked_add(len).is_none_or(|end| end > USER_END) {
            return Err(err(libc::EFAULT));
        }
        let len = len
            .min(MAX_RW_COUNT - total)
            .min(READ_BUFFER as u64 - total);
        total += len;
        segments.push((base, len));
    }
    let mut buffer = vec![0u8; total as usize];
    if write {
        let mut at = 0usize;
        for &(base, len) in &segments {
            let end = at + len as usize;
            if mem.read(base, &mut buffer[at..end]).is_err() {
                if at == 0 {
                    return Err(err(libc::EFAULT));
                }
                buffer.truncate(at);
                break;
            }
            at = end;
        }
    }
    let host_iov = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    };
    let fd = fd as i32;
    let pos = offset.map_or(-1i64, |o| o as i64);
    // SAFETY: one iovec over `buffer`, which lives for the call; the
    // offset and flags are integers.
    let n = unsafe {
        match (write, offset, flags) {
            (false, None, 0) => libc::readv(fd, &host_iov, 1) as i64,
            (true, None, 0) => libc::writev(fd, &host_iov, 1) as i64,
            (false, _, _) => libc::syscall(libc::SYS_preadv2, fd, &host_iov, 1, pos, 0, flags),
            (true, _, _) => libc::syscall(libc::SYS_pwritev2, fd, &host_iov, 1, pos, 0, flags),
        }
    };
    if n < 0 {
        return Err(host_errno());
    }
    if !write {
        let mut at = 0usize;
        let n = n as usize;
        for &(base, len) in &segments {
            if at >= n {
                break;
            }
            let end = (at + len as usize).min(n);
            if mem.write(base, &buffer[at..end]).is_err() {
                return if at == 0 {
                    Err(err(libc::EFAULT))
                } else {
                    Ok(at as i64)
                };
            }
            at = end;
        }
    }
    Ok(n)
}

/// getitimer(2): `struct itimerval` (two timevals, 32 bytes) is the same
/// on both architectures.
fn getitimer(mem: &AddressSpace, which: u64, current: u64) -> SysResult {
    let mut value = [0u8; 32];
    // SAFETY: value is a 32-byte host `itimerval`.
    let r = unsafe { libc::syscall(libc::SYS_getitimer, which as i32, value.as_mut_ptr()) };
    if r < 0 {
        return Err(host_errno());
    }
    copy_out(mem, current, &value)?;
    Ok(0)
}

/// waitid(2): the child's `siginfo_t` (128 bytes; the SIGCHLD fields sit at
/// the same offsets on both) and, if asked, its `struct rusage` (144).
fn waitid(
    mem: &AddressSpace,
    idtype: u64,
    id: u64,
    info: u64,
    options: u64,
    rusage: u64,
) -> SysResult {
    let mut siginfo = [0u8; 128];
    let mut usage = [0u8; 144];
    // SAFETY: siginfo and usage are host buffers of the sizes waitid writes.
    let r = unsafe {
        libc::syscall(
            libc::SYS_waitid,
            idtype as i32,
            id as i32,
            siginfo.as_mut_ptr(),
            options as i32,
            usage.as_mut_ptr(),
        )
    };
    if r < 0 {
        return Err(host_errno());
    }
    if info != 0 {
        copy_out(mem, info, &siginfo)?;
    }
    if rusage != 0 {
        copy_out(mem, rusage, &usage)?;
    }
    Ok(0)
}

/// timerfd_settime(2): `struct itimerspec` (interval, value: two timespecs,
/// 32 bytes) is the same on both architectures.
fn timerfd_settime(mem: &AddressSpace, fd: u64, flags: u64, new: u64, old: u64) -> SysResult {
    let value = copy_in::<32>(mem, new)?;
    let mut previous = [0u8; 32];
    // SAFETY: value and previous are 32-byte itimerspecs.
    let r = unsafe {
        libc::syscall(
            libc::SYS_timerfd_settime,
            fd as i32,
            flags as i32,
            value.as_ptr(),
            previous.as_mut_ptr(),
        )
    };
    if r < 0 {
        return Err(host_errno());
    }
    if old != 0 {
        copy_out(mem, old, &previous)?;
    }
    Ok(0)
}

/// timerfd_gettime(2).
fn timerfd_gettime(mem: &AddressSpace, fd: u64, current: u64) -> SysResult {
    let mut value = [0u8; 32];
    // SAFETY: value is a 32-byte itimerspec.
    let r = unsafe { libc::syscall(libc::SYS_timerfd_gettime, fd as i32, value.as_mut_ptr()) };
    if r < 0 {
        return Err(host_errno());
    }
    copy_out(mem, current, &value)?;
    Ok(0)
}

/// getrusage(2): `struct rusage` is two timevals and fourteen longs, 144
/// bytes on both architectures.
fn getrusage(mem: &AddressSpace, who: u64, usage: u64) -> SysResult {
    let mut buf = [0u8; 144];
    // SAFETY: buf is 144 bytes, the size of the host's struct rusage.
    let r = unsafe { libc::syscall(libc::SYS_getrusage, who as i32, buf.as_mut_ptr()) };
    if r < 0 {
        return Err(host_errno());
    }
    copy_out(mem, usage, &buf)?;
    Ok(0)
}

/// times(2): `struct tms` is four `clock_t` (longs) on both; the result is
/// the clock ticks since boot.
fn times(mem: &AddressSpace, buf: u64) -> SysResult {
    let mut tms = [0u8; 32];
    // SAFETY: tms is 32 bytes, the size of the host's struct tms.
    let ticks = unsafe { libc::syscall(libc::SYS_times, tms.as_mut_ptr()) };
    if buf != 0 {
        copy_out(mem, buf, &tms)?;
    }
    Ok(ticks)
}

/// getcpu(2): the CPU and NUMA node, each written if its pointer is set.
fn getcpu(mem: &AddressSpace, cpu: u64, node: u64) -> SysResult {
    let (mut c, mut n) = (0u32, 0u32);
    // SAFETY: both pointers are to local u32s; the cache argument is unused.
    let r = unsafe { libc::syscall(libc::SYS_getcpu, &mut c, &mut n, 0usize) };
    if r < 0 {
        return Err(host_errno());
    }
    if cpu != 0 {
        copy_out(mem, cpu, &c.to_le_bytes())?;
    }
    if node != 0 {
        copy_out(mem, node, &n.to_le_bytes())?;
    }
    Ok(0)
}

/// mincore(2), one byte per guest page: ENOMEM where the guest has nothing
/// mapped, else whether the host page holding it is resident.
fn mincore(mem: &AddressSpace, addr: u64, len: u64, vec: u64) -> SysResult {
    if !addr.is_multiple_of(GUEST_PAGE_SIZE) {
        return Err(err(libc::EINVAL));
    }
    let pages = len.div_ceil(GUEST_PAGE_SIZE);
    if pages > 1 << 24 {
        return Err(err(libc::ENOMEM));
    }
    let host_page = mem.host_page_size();
    let mut out = vec![0u8; pages as usize];
    for (i, byte) in out.iter_mut().enumerate() {
        let page = addr + i as u64 * GUEST_PAGE_SIZE;
        if mem.prot(page).is_none() {
            return Err(err(libc::ENOMEM));
        }
        let mut resident = 0u8;
        // SAFETY: the host page holding a mapped guest page is mapped;
        // resident receives one byte.
        let r = unsafe {
            libc::mincore(
                (page & !(host_page - 1)) as *mut libc::c_void,
                host_page as usize,
                &mut resident,
            )
        };
        if r < 0 {
            return Err(host_errno());
        }
        *byte = resident & 1;
    }
    copy_out(mem, vec, &out)?;
    Ok(0)
}

fn getgroups(mem: &AddressSpace, size: u64, list: u64) -> SysResult {
    let mut groups = vec![0 as libc::gid_t; (size as usize).min(65536)];
    // SAFETY: groups is writable for its length (gid_t is u32 on both).
    let n = unsafe { libc::getgroups(groups.len() as i32, groups.as_mut_ptr()) };
    if n < 0 {
        return Err(host_errno());
    }
    if size != 0 {
        let bytes: Vec<u8> = groups[..n as usize]
            .iter()
            .flat_map(|g| g.to_le_bytes())
            .collect();
        copy_out(mem, list, &bytes)?;
    }
    Ok(i64::from(n))
}

/// The thread name options and PR_CAPBSET_READ for now; `None` for others.
fn prctl(mem: &AddressSpace, option: u64, arg: u64, rest: [u64; 3]) -> Option<SysResult> {
    let mut name = [0u8; TASK_COMM_LEN];
    Some(match option {
        PR_SET_NAME => (|| {
            // The name is at most 16 bytes; it may end at a page boundary.
            for (i, byte) in name.iter_mut().enumerate().take(TASK_COMM_LEN - 1) {
                let mut b = [0u8];
                mem.read(arg.wrapping_add(i as u64), &mut b)
                    .map_err(|_| err(libc::EFAULT))?;
                if b[0] == 0 {
                    break;
                }
                *byte = b[0];
            }
            // SAFETY: name is NUL-terminated and 16 bytes long.
            if unsafe { libc::prctl(libc::PR_SET_NAME, name.as_ptr()) } < 0 {
                return Err(host_errno());
            }
            Ok(0)
        })(),
        PR_GET_NAME => (|| {
            // SAFETY: name is writable for the 16 bytes the kernel writes.
            if unsafe { libc::prctl(libc::PR_GET_NAME, name.as_mut_ptr()) } < 0 {
                return Err(host_errno());
            }
            copy_out(mem, arg, &name)?;
            Ok(0)
        })(),
        // Capability numbers are architecture-independent.
        PR_CAPBSET_READ => host(libc::SYS_prctl, &[option, arg]),
        // no_new_privs of armor's process is the guest's: it only limits
        // what execve grants, and armor's execve is the host's.
        PR_SET_NO_NEW_PRIVS | PR_GET_NO_NEW_PRIVS => {
            host(libc::SYS_prctl, &[option, arg, rest[0], rest[1], rest[2]])
        }
        // Whether the process can be dumped or traced: armor's process is
        // the guest's (the crash handler of the Steam client asks).
        PR_GET_DUMPABLE | PR_SET_DUMPABLE => host(libc::SYS_prctl, &[option, arg]),
        // As a kernel built without CONFIG_ANON_VMA_NAME: the names are only
        // for /proc/<pid>/maps, which armor does not show.
        PR_SET_VMA => Err(err(libc::EINVAL)),
        _ => return None,
    })
}

/// Names the armor process after the guest program, as exec(2) would, so
/// that `ps`, /proc/self/comm and PR_GET_NAME show it.
pub fn set_process_name(program: &[u8]) {
    let base = program.rsplit(|&b| b == b'/').next().unwrap_or(program);
    let mut name = [0u8; TASK_COMM_LEN];
    let len = base.len().min(TASK_COMM_LEN - 1);
    name[..len].copy_from_slice(&base[..len]);
    // SAFETY: name is NUL-terminated and 16 bytes long.
    unsafe { libc::prctl(libc::PR_SET_NAME, name.as_ptr()) };
}

/// pwrite64(2): like `write`, at an explicit offset (the file position
/// does not move).
fn pwrite64(mem: &AddressSpace, fd: u64, buf: u64, count: u64, offset: u64) -> SysResult {
    let count = count.min(MAX_RW_COUNT);
    let mut chunk = vec![0u8; (count as usize).min(CHUNK)];
    let mut written: u64 = 0;
    while written < count {
        let len = ((count - written) as usize).min(CHUNK);
        if mem
            .read(buf.wrapping_add(written), &mut chunk[..len])
            .is_err()
        {
            return if written == 0 {
                Err(err(libc::EFAULT))
            } else {
                Ok(written as i64)
            };
        }
        // SAFETY: chunk is valid for reads of `len` bytes.
        let n = unsafe {
            libc::pwrite(
                fd as i32,
                chunk.as_ptr().cast(),
                len,
                offset.wrapping_add(written) as libc::off_t,
            )
        };
        if n < 0 {
            return if written == 0 {
                Err(host_errno())
            } else {
                Ok(written as i64)
            };
        }
        written += n as u64;
        if (n as usize) < len {
            break;
        }
    }
    Ok(written as i64)
}

/// write(2) through the checked memory path, in chunks.
fn write(mem: &AddressSpace, fd: u64, buf: u64, count: u64) -> i64 {
    let fd = fd as i32;
    let count = count.min(MAX_RW_COUNT);
    let mut chunk = vec![0u8; (count as usize).min(CHUNK)];
    let mut written: u64 = 0;
    while written < count {
        let len = ((count - written) as usize).min(CHUNK);
        if mem
            .read(buf.wrapping_add(written), &mut chunk[..len])
            .is_err()
        {
            return if written == 0 {
                err(libc::EFAULT)
            } else {
                written as i64
            };
        }
        // SAFETY: chunk is valid for reads of `len` bytes.
        let n = unsafe { libc::write(fd, chunk.as_ptr().cast(), len) };
        if n < 0 {
            return if written == 0 {
                host_errno()
            } else {
                written as i64
            };
        }
        written += n as u64;
        if (n as usize) < len {
            break;
        }
    }
    written as i64
}

/// The most iovecs in one call (`UIO_MAXIOV`).
const IOV_MAX: u64 = 1024;
/// x86_64's `TASK_SIZE_MAX`: a buffer must end below it (`access_ok`).
const USER_END: u64 = (1 << 47) - 4096;

/// writev(2), as Linux does it (checked against the host kernel with a
/// native C program, 2026-10-13):
/// - the descriptor first (EBADF), then the vector: more than `IOV_MAX`
///   entries or a negative length is EINVAL, an unreadable vector or a
///   buffer beyond the user address space EFAULT; the total is capped at
///   `MAX_RW_COUNT`;
/// - the segments are gathered in order into a host buffer and written
///   with one host `write` per `CHUNK` bytes, so that a short `writev`
///   reaches a pipe as one write, as from the guest's own kernel;
/// - a buffer that faults ends the gathering at its first bad byte: what
///   came before is written, as Linux does for a regular file, or EFAULT
///   if nothing did. For a pipe Linux writes nothing and returns EFAULT
///   (docs/tech/syscalls.md).
///
/// `word` is the size of the pointer and of the length in the guest's
/// `struct iovec`: 8, or 4 for an i386 guest.
fn writev(mem: &AddressSpace, fd: u64, iov: u64, iovcnt: u64, word: usize) -> SysResult {
    let fd = fd as i32;
    // SAFETY: no iovecs: the kernel only checks the descriptor (and that
    // it is open for writing) and writes nothing.
    if unsafe { libc::writev(fd, std::ptr::null(), 0) } < 0 {
        return Err(host_errno());
    }
    if iovcnt > IOV_MAX {
        return Err(err(libc::EINVAL));
    }
    let mut vector = vec![0u8; iovcnt as usize * 2 * word];
    mem.read(iov, &mut vector).map_err(|_| err(libc::EFAULT))?;
    let field = |bytes: &[u8]| {
        let mut value = [0u8; 8];
        value[..word].copy_from_slice(bytes);
        u64::from_le_bytes(value)
    };
    let entries: Vec<(u64, u64)> = vector
        .chunks_exact(2 * word)
        .map(|entry| {
            let (base, len) = entry.split_at(word);
            (field(base), field(len))
        })
        .collect();
    // The length is signed (ssize_t), of the guest's width.
    let negative = |len: u64| len >> (8 * word - 1) & 1 != 0;
    if entries.iter().any(|&(_, len)| negative(len)) {
        return Err(err(libc::EINVAL));
    }
    let mut segments = Vec::with_capacity(entries.len());
    let mut total: u64 = 0;
    for (base, len) in entries {
        if base.checked_add(len).is_none_or(|end| end > USER_END) {
            return Err(err(libc::EFAULT));
        }
        let len = len.min(MAX_RW_COUNT - total);
        total += len;
        segments.push((base, len));
    }
    let mut pending: Vec<u8> = Vec::with_capacity((total as usize).min(CHUNK));
    let mut written: u64 = 0;
    let mut faulted = false;
    'gather: for (base, len) in segments {
        let mut done = 0;
        while done < len {
            let start = pending.len();
            let take = ((len - done) as usize).min(CHUNK - start);
            let addr = base + done;
            pending.resize(start + take, 0);
            if let Err(fault) = mem.read(addr, &mut pending[start..]) {
                // The bytes before the first bad one are readable.
                let good = (fault.addr - addr) as usize;
                pending.truncate(start + good);
                mem.read(addr, &mut pending[start..])
                    .map_err(|_| err(libc::EFAULT))?;
                faulted = true;
                break 'gather;
            }
            done += take as u64;
            if pending.len() == CHUNK {
                let n = host_write(fd, &pending, written)?;
                written += n as u64;
                if n < pending.len() {
                    return Ok(written as i64);
                }
                pending.clear();
            }
        }
    }
    if !pending.is_empty() {
        written += host_write(fd, &pending, written)? as u64;
    }
    if written == 0 && faulted {
        return Err(err(libc::EFAULT));
    }
    Ok(written as i64)
}

/// One host write of `bytes`, for a call that has already written
/// `before` bytes: an error then ends the call with what was written.
fn host_write(fd: i32, bytes: &[u8], before: u64) -> Result<usize, i64> {
    // SAFETY: bytes is valid for reads of its length.
    let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    match n {
        n if n >= 0 => Ok(n as usize),
        _ if before > 0 => Ok(0),
        _ => Err(host_errno()),
    }
}

pub(crate) fn copy_out(mem: &AddressSpace, addr: u64, bytes: &[u8]) -> Result<(), i64> {
    mem.write(addr, bytes).map_err(|_| err(libc::EFAULT))
}

pub(crate) fn copy_in<const N: usize>(mem: &AddressSpace, addr: u64) -> Result<[u8; N], i64> {
    let mut bytes = [0u8; N];
    mem.read(addr, &mut bytes).map_err(|_| err(libc::EFAULT))?;
    Ok(bytes)
}

/// Reads a NUL-terminated string from guest memory.
pub(crate) fn read_path(mem: &AddressSpace, addr: u64) -> Result<CString, i64> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8];
        mem.read(addr.wrapping_add(bytes.len() as u64), &mut byte)
            .map_err(|_| err(libc::EFAULT))?;
        if byte[0] == 0 {
            break;
        }
        if bytes.len() >= PATH_MAX {
            return Err(err(libc::ENAMETOOLONG));
        }
        bytes.push(byte[0]);
    }
    Ok(CString::new(bytes).expect("no interior NUL by construction"))
}

/// Converts the AArch64 `struct stat` to the x86_64 layout (144 bytes).
fn x86_stat(st: &libc::stat) -> [u8; 144] {
    let mut out = [0u8; 144];
    let mut put = |offset: usize, bytes: &[u8]| {
        out[offset..offset + bytes.len()].copy_from_slice(bytes);
    };
    put(0, &st.st_dev.to_le_bytes());
    put(8, &st.st_ino.to_le_bytes());
    put(16, &u64::from(st.st_nlink).to_le_bytes());
    put(24, &st.st_mode.to_le_bytes());
    put(28, &st.st_uid.to_le_bytes());
    put(32, &st.st_gid.to_le_bytes());
    put(40, &st.st_rdev.to_le_bytes());
    put(48, &st.st_size.to_le_bytes());
    put(56, &i64::from(st.st_blksize).to_le_bytes());
    put(64, &st.st_blocks.to_le_bytes());
    put(72, &st.st_atime.to_le_bytes());
    put(80, &st.st_atime_nsec.to_le_bytes());
    put(88, &st.st_mtime.to_le_bytes());
    put(96, &st.st_mtime_nsec.to_le_bytes());
    put(104, &st.st_ctime.to_le_bytes());
    put(112, &st.st_ctime_nsec.to_le_bytes());
    out
}

/// The host's `struct stat` of descriptor `fd`.
pub(crate) fn host_fstat(fd: u64) -> Result<libc::stat, i64> {
    // SAFETY: an all-zero `stat` is a valid value of this plain C struct.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is a valid, writable `stat`.
    if unsafe { libc::fstat(fd as i32, &mut st) } < 0 {
        return Err(host_errno());
    }
    Ok(st)
}

/// The host's `struct stat` of a guest path (in the guest root).
pub(crate) fn host_fstatat(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    flags: u64,
) -> Result<libc::stat, i64> {
    let path = root.resolve_c(read_path(mem, path)?);
    // SAFETY: an all-zero `stat` is a valid value of this plain C struct.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is NUL-terminated and `st` is writable. AT_* flags have
    // the same values on both architectures.
    if unsafe { libc::fstatat(dirfd as i32, path.as_ptr(), &mut st, flags as i32) } < 0 {
        return Err(host_errno());
    }
    Ok(st)
}

fn fstat(mem: &AddressSpace, fd: u64, buf: u64) -> SysResult {
    copy_out(mem, buf, &x86_stat(&host_fstat(fd)?))?;
    Ok(0)
}

fn newfstatat(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    buf: u64,
    flags: u64,
) -> SysResult {
    let st = host_fstatat(mem, root, dirfd, path, flags)?;
    copy_out(mem, buf, &x86_stat(&st))?;
    Ok(0)
}

/// access(2), faccessat(2) and faccessat2(2): AArch64 only has the last
/// two; AT_* flags and the mode bits have the same values on both.
fn faccessat(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    mode: u64,
    flags: u64,
) -> SysResult {
    let path = root.resolve_c(read_path(mem, path)?);
    // SAFETY: path is NUL-terminated; the other arguments are integers.
    let result = unsafe {
        libc::syscall(
            libc::SYS_faccessat2,
            dirfd as i32,
            path.as_ptr(),
            mode as i32,
            flags as i32,
        )
    };
    if result < 0 { Err(host_errno()) } else { Ok(0) }
}

/// unlinkat(2) (and unlink, with AT_FDCWD): the path is resolved in the
/// guest root like every other; AT_REMOVEDIR has the same value on both
/// architectures.
fn unlinkat(mem: &AddressSpace, root: &GuestRoot, dirfd: u64, path: u64, flags: u64) -> SysResult {
    let path = root.resolve_c(read_path(mem, path)?);
    // SAFETY: path is NUL-terminated; the other arguments are integers.
    let result = unsafe {
        libc::syscall(
            libc::SYS_unlinkat,
            dirfd as i32,
            path.as_ptr(),
            flags as i32,
        )
    };
    if result < 0 { Err(host_errno()) } else { Ok(0) }
}

fn prot(bits: u64) -> Prot {
    let mut prot = Prot::NONE;
    for (flag, p) in [(1, Prot::READ), (2, Prot::WRITE), (4, Prot::EXEC)] {
        if bits & flag != 0 {
            prot = prot | p;
        }
    }
    prot
}

/// Anonymous and file-backed private mappings. A file mapping is a copy:
/// armor maps anonymous memory and reads the file into it, because guest
/// offsets and addresses are only 4K-aligned and the host maps files in 16K
/// pages (docs/tech/memory.md). `Err` describes what is not supported.
/// Set in armor's environment: report every file mapping of the guest on
/// stderr (address range, file, offset, protection), to find which library
/// an address belongs to (armor maps private file mappings by copy, so the
/// host's /proc/<pid>/maps does not show the files).
pub const TRACE_MAPS_VARIABLE: &str = "ARMOR_TRACE_MAPS";

fn trace_map(result: &SysResult, len: u64, prot: u64, flags: u64, fd: u64, offset: u64) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os(TRACE_MAPS_VARIABLE).is_some()) {
        return;
    }
    let Ok(addr) = result else {
        return;
    };
    if flags & MAP_ANONYMOUS != 0 {
        return;
    }
    let file = std::fs::read_link(format!("/proc/self/fd/{}", fd as i32))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| format!("fd {}", fd as i32));
    eprintln!(
        "armor: map {addr:#x}-{:#x} {file} offset {offset:#x} prot {prot:#x}",
        *addr as u64 + len
    );
}

fn mmap(
    mem: &AddressSpace,
    addr: u64,
    len: u64,
    prot_bits: u64,
    flags: u64,
    fd: u64,
    offset: u64,
) -> Result<SysResult, &'static str> {
    let known = MAP_SHARED
        | MAP_PRIVATE
        | MAP_FIXED
        | MAP_ANONYMOUS
        | MAP_GROWSDOWN
        | MAP_DENYWRITE
        | MAP_EXECUTABLE
        | MAP_NORESERVE
        | MAP_POPULATE
        | MAP_STACK
        | MAP_FIXED_NOREPLACE;
    if flags & !known != 0 {
        return Err("unknown flags");
    }
    let file = flags & MAP_ANONYMOUS == 0;
    if flags & MAP_GROWSDOWN != 0 {
        return Err("MAP_GROWSDOWN");
    }
    // MAP_SHARED_VALIDATE (both bits) is MAP_SHARED with the flags checked,
    // which armor always does.
    let shared = flags & MAP_SHARED != 0;
    if len == 0 || flags & (MAP_PRIVATE | MAP_SHARED) == 0 {
        return Ok(Err(err(libc::EINVAL)));
    }
    let mut writable_file = false;
    if file {
        if !offset.is_multiple_of(GUEST_PAGE_SIZE) {
            return Ok(Err(err(libc::EINVAL)));
        }
        // Like the kernel: the descriptor must be open for reading.
        match host(libc::SYS_fcntl, &[fd, F_GETFL]) {
            Err(e) => return Ok(Err(e)),
            Ok(mode) if mode & libc::O_ACCMODE as i64 == libc::O_WRONLY as i64 => {
                return Ok(Err(err(libc::EACCES)));
            }
            Ok(mode) => writable_file = mode & libc::O_ACCMODE as i64 == libc::O_RDWR as i64,
        }
    }
    if shared {
        return map_shared(mem, addr, len, prot_bits, flags, fd, offset, writable_file);
    }
    let addr = match map_memory(mem, addr, len, prot(prot_bits), flags)? {
        Ok(addr) => addr,
        Err(e) => return Ok(Err(e)),
    };
    if file && let Err(e) = read_file_into(mem, fd, offset, addr, len) {
        let _ = mem.unmap(addr, len);
        return Ok(Err(e));
    }
    Ok(Ok(addr as i64))
}

/// mmap with MAP_SHARED (ADR-0022): host pages mapped shared, anonymous
/// or from the file; `writable_file` is whether `fd` is open for writing.
#[allow(clippy::too_many_arguments)]
fn map_shared(
    mem: &AddressSpace,
    addr: u64,
    len: u64,
    prot_bits: u64,
    flags: u64,
    fd: u64,
    offset: u64,
    writable_file: bool,
) -> Result<SysResult, &'static str> {
    let file = flags & MAP_ANONYMOUS == 0;
    if prot_bits & PROT_EXEC_BIT != 0 {
        return Err("executable shared mappings (ADR-0022)");
    }
    // As on Linux: writing to a shared file mapping needs a file open for
    // writing.
    if file && prot_bits & PROT_WRITE_BIT != 0 && !writable_file {
        return Ok(Err(err(libc::EACCES)));
    }
    let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
    if fixed && !addr.is_multiple_of(GUEST_PAGE_SIZE) {
        return Ok(Err(err(libc::EINVAL)));
    }
    if flags & MAP_FIXED_NOREPLACE != 0 && mem.any_mapped(addr, len) {
        return Ok(Err(err(libc::EEXIST)));
    }
    let source = if file {
        SharedSource::File {
            fd: fd as i32,
            offset,
        }
    } else {
        SharedSource::Anonymous
    };
    let writable = !file || writable_file;
    match mem.map_shared(
        fixed.then_some(addr),
        len,
        prot(prot_bits),
        source,
        writable,
    ) {
        Ok(addr) => Ok(Ok(addr as i64)),
        Err(MapError::SharedPlacement(why)) => Err(why),
        Err(MapError::AddressInUse { .. }) => Err("MAP_FIXED over memory used by armor"),
        Err(MapError::Host(e)) => Ok(Err(err(e.raw_os_error().unwrap_or(libc::ENOMEM)))),
        Err(_) => Ok(Err(err(libc::ENOMEM))),
    }
}

/// The memory part of `mmap`: returns the guest address of the new pages.
fn map_memory(
    mem: &AddressSpace,
    addr: u64,
    len: u64,
    prot: Prot,
    flags: u64,
) -> Result<Result<u64, i64>, &'static str> {
    if flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0 {
        if !addr.is_multiple_of(GUEST_PAGE_SIZE) {
            return Ok(Err(err(libc::EINVAL)));
        }
        if flags & MAP_FIXED_NOREPLACE != 0 && mem.any_mapped(addr, len) {
            return Ok(Err(err(libc::EEXIST)));
        }
        return match mem.map_fixed(addr, len, prot) {
            Ok(()) => Ok(Ok(addr)),
            // The range holds armor's own memory: armor cannot hand it over.
            Err(MapError::AddressInUse { .. }) => Err("MAP_FIXED over memory used by armor"),
            Err(MapError::SharedPlacement(why)) => Err(why),
            Err(_) => Ok(Err(err(libc::ENOMEM))),
        };
    }
    // Without MAP_FIXED the address is only a hint, which armor ignores.
    Ok(mem.map_anywhere(len, prot).map_err(|_| err(libc::ENOMEM)))
}

/// Copies `len` bytes of file `fd` from `offset` to guest address `addr`.
/// Bytes past the end of the file stay zero.
fn read_file_into(
    mem: &AddressSpace,
    fd: u64,
    offset: u64,
    addr: u64,
    len: u64,
) -> Result<(), i64> {
    let mut chunk = vec![0u8; CHUNK];
    let mut done = 0;
    while done < len {
        let want = ((len - done) as usize).min(CHUNK);
        // SAFETY: chunk is writable for `want` <= CHUNK bytes.
        let n = unsafe {
            libc::pread(
                fd as i32,
                chunk.as_mut_ptr().cast(),
                want,
                offset.wrapping_add(done) as libc::off_t,
            )
        };
        if n < 0 {
            return Err(host_errno());
        }
        if n == 0 {
            break;
        }
        mem.write_privileged(addr + done, &chunk[..n as usize])
            .map_err(|_| err(libc::EFAULT))?;
        done += n as u64;
    }
    Ok(())
}

const MREMAP_MAYMOVE: u64 = 1;

/// mremap(2): shrinking unmaps the tail; growing a private mapping maps
/// the pages after it if they are free, otherwise (with MREMAP_MAYMOVE)
/// maps a new range, copies the old one and unmaps it; growing a shared
/// one is the host's mremap of the same memory.
/// Like Linux, the old range must be mapped with one protection.
/// MREMAP_FIXED and MREMAP_DONTUNMAP are not supported.
fn mremap(
    mem: &AddressSpace,
    old: u64,
    old_len: u64,
    new_len: u64,
    flags: u64,
) -> Result<SysResult, &'static str> {
    if flags & !MREMAP_MAYMOVE != 0 {
        return Err("MREMAP_FIXED or MREMAP_DONTUNMAP");
    }
    let round = |n: u64| n.checked_next_multiple_of(GUEST_PAGE_SIZE);
    let (Some(old_len), Some(new_len)) = (round(old_len), round(new_len)) else {
        return Ok(Err(err(libc::EINVAL)));
    };
    if !old.is_multiple_of(GUEST_PAGE_SIZE) || new_len == 0 || old_len == 0 {
        return Ok(Err(err(libc::EINVAL)));
    }
    let Some(old_end) = old.checked_add(old_len) else {
        return Ok(Err(err(libc::EINVAL)));
    };
    let Some(prot) = mem.prot(old) else {
        return Ok(Err(err(libc::EFAULT)));
    };
    let shared = mem.any_shared(old, old_len);
    let mut page = old;
    while page < old_end {
        if mem.prot(page) != Some(prot) {
            return Ok(Err(err(libc::EFAULT)));
        }
        page += GUEST_PAGE_SIZE;
    }
    if new_len <= old_len {
        if new_len < old_len {
            let _ = mem.unmap(old + new_len, old_len - new_len);
        }
        return Ok(Ok(old as i64));
    }
    // A shared mapping grows by the host's own mremap, which keeps it the
    // same memory (ADR-0022, `AddressSpace::remap_shared`).
    if shared {
        return match mem.remap_shared(old, old_len, new_len, flags & MREMAP_MAYMOVE != 0) {
            Ok(new) => Ok(Ok(new as i64)),
            Err(MapError::SharedPlacement(_)) => {
                Err("growing a shared mapping not on host-page boundaries (ADR-0022)")
            }
            Err(MapError::Host(e)) => Ok(Err(err(e.raw_os_error().unwrap_or(libc::ENOMEM)))),
            Err(_) => Ok(Err(err(libc::ENOMEM))),
        };
    }
    // Grow in place when every page after the old range is free.
    let grow = new_len - old_len;
    let free = old_end.checked_add(grow).is_some() && !mem.any_mapped(old_end, grow);
    if free {
        match mem.map_fixed(old_end, grow, prot) {
            Ok(()) => return Ok(Ok(old as i64)),
            // armor's own memory is there: move instead.
            Err(MapError::AddressInUse { .. }) => {}
            Err(_) => return Ok(Err(err(libc::ENOMEM))),
        }
    }
    if flags & MREMAP_MAYMOVE == 0 {
        return Ok(Err(err(libc::ENOMEM)));
    }
    let Ok(new) = mem.map_anywhere(new_len, prot) else {
        return Ok(Err(err(libc::ENOMEM)));
    };
    let mut chunk = vec![0u8; CHUNK];
    let mut done = 0;
    while done < old_len {
        let n = ((old_len - done) as usize).min(CHUNK);
        if mem.read_privileged(old + done, &mut chunk[..n]).is_err()
            || mem.write_privileged(new + done, &chunk[..n]).is_err()
        {
            let _ = mem.unmap(new, new_len);
            return Ok(Err(err(libc::EFAULT)));
        }
        done += n as u64;
    }
    let _ = mem.unmap(old, old_len);
    Ok(Ok(new as i64))
}

fn munmap(mem: &AddressSpace, addr: u64, len: u64) -> SysResult {
    if len == 0 {
        return Err(err(libc::EINVAL));
    }
    mem.unmap(addr, len).map_err(|_| err(libc::EINVAL))?;
    Ok(0)
}

fn mprotect(
    mem: &AddressSpace,
    addr: u64,
    len: u64,
    prot_bits: u64,
) -> Result<SysResult, &'static str> {
    Ok(match mem.protect(addr, len, prot(prot_bits)) {
        Ok(()) => Ok(0),
        Err(ProtectError::Unaligned) => Err(err(libc::EINVAL)),
        Err(ProtectError::NotMapped) => Err(err(libc::ENOMEM)),
        Err(ProtectError::ReadOnlyShared) => Err(err(libc::EACCES)),
        // Linux allows it; armor does not, yet (ADR-0022).
        Err(ProtectError::ExecutableShared) => return Err("executable shared memory (ADR-0022)"),
    })
}

/// msync(2): MS_ASYNC (1), MS_INVALIDATE (2) and MS_SYNC (4) have the same
/// values on both; MS_ASYNC with MS_SYNC, or other bits, are EINVAL.
fn msync(mem: &AddressSpace, addr: u64, len: u64, flags: u64) -> SysResult {
    const MS_ASYNC: u64 = 1;
    const MS_SYNC: u64 = 4;
    if !addr.is_multiple_of(GUEST_PAGE_SIZE)
        || flags & !7 != 0
        || flags & (MS_ASYNC | MS_SYNC) == MS_ASYNC | MS_SYNC
    {
        return Err(err(libc::EINVAL));
    }
    mem.sync(addr, len, flags as i32).map_err(err)?;
    Ok(0)
}

/// brk(2): grows or shrinks the heap after the loaded segments. Like the
/// kernel, returns the current break when the request cannot be honoured.
fn brk(mem: &AddressSpace, process: &Process, requested: u64) -> i64 {
    let mut brk = process.brk.lock().expect("brk lock poisoned");
    if requested < brk.start {
        return brk.current as i64;
    }
    let Some(needed) = requested.checked_next_multiple_of(GUEST_PAGE_SIZE) else {
        return brk.current as i64;
    };
    if needed > brk.mapped {
        let grow = needed - brk.mapped;
        // Like Linux, the heap does not grow over another mapping.
        if mem.any_mapped(brk.mapped, grow)
            || mem
                .map_fixed(brk.mapped, grow, Prot::READ | Prot::WRITE)
                .is_err()
        {
            return brk.current as i64;
        }
    } else if needed < brk.mapped {
        // Shrinking cannot fail on an aligned address.
        let _ = mem.unmap(needed, brk.mapped - needed);
    }
    brk.mapped = needed;
    brk.current = requested;
    requested as i64
}

/// Only TCGETS and TIOCGWINSZ for now; `None` for other requests.
fn ioctl(mem: &AddressSpace, fd: u64, request: u64, arg: u64) -> Option<SysResult> {
    if request == DRM_IOCTL_VERSION {
        return Some(drm_version(mem, fd, arg));
    }
    // SIOCETHTOOL: a `struct ifreq` pointing to a command-specific
    // structure. Answered as a driver without ethtool support: Chromium
    // only asks for the link type, and copes with the refusal.
    if request == SIOCETHTOOL {
        return Some(Err(err(libc::EOPNOTSUPP)));
    }
    // Wireless extensions (0x8b00-0x8bff) other than SIOCGIWNAME carry
    // pointers in `struct iwreq`: answered as a device without them.
    if (0x8b00..=0x8bff).contains(&request) && request != 0x8b01 {
        return Some(Err(err(libc::EOPNOTSUPP)));
    }
    if let Some(&(_, size, reads)) = PLAIN_IOCTLS.iter().find(|(r, ..)| *r == request) {
        return Some(plain_ioctl(mem, fd, request, arg, size, reads));
    }
    let size = match request {
        TCGETS => KERNEL_TERMIOS_SIZE,
        TIOCGWINSZ => WINSIZE_SIZE,
        FIONREAD => 4,
        // An `int` read from the guest: non-blocking or not.
        FIONBIO => {
            let mut value = match copy_in::<4>(mem, arg) {
                Ok(bytes) => i32::from_le_bytes(bytes),
                Err(e) => return Some(Err(e)),
            };
            // SAFETY: FIONBIO reads one int, which `value` is.
            let result = unsafe { libc::ioctl(fd as i32, FIONBIO as libc::c_ulong, &mut value) };
            return Some(if result < 0 {
                Err(host_errno())
            } else {
                Ok(i64::from(result))
            });
        }
        // An unknown request on a descriptor that is not open: EBADF, as
        // Linux answers before looking at the request.
        // SAFETY: F_GETFD takes no argument.
        _ if unsafe { libc::fcntl(fd as i32, libc::F_GETFD) } < 0 => {
            return Some(Err(err(libc::EBADF)));
        }
        _ => return None,
    };
    let mut buf = [0u8; KERNEL_TERMIOS_SIZE];
    // SAFETY: these requests write at most `size` bytes, and buf is at
    // least that large. Request numbers and layouts match on both
    // architectures.
    let result = unsafe { libc::ioctl(fd as i32, request as libc::c_ulong, buf.as_mut_ptr()) };
    Some(if result < 0 {
        Err(host_errno())
    } else {
        copy_out(mem, arg, &buf[..size]).map(|()| i64::from(result))
    })
}

fn arch_prctl(cpu: &mut Cpu, mem: &AddressSpace, code: u64, addr: u64) -> SysResult {
    match code {
        ARCH_SET_FS => cpu.fs_base = addr,
        ARCH_SET_GS => cpu.gs_base = addr,
        ARCH_GET_FS => copy_out(mem, addr, &cpu.fs_base.to_le_bytes())?,
        ARCH_GET_GS => copy_out(mem, addr, &cpu.gs_base.to_le_bytes())?,
        _ => return Err(err(libc::EINVAL)),
    }
    Ok(0)
}

fn readlinkat(
    mem: &AddressSpace,
    process: &Process,
    dirfd: u64,
    path: u64,
    buf: u64,
    size: u64,
) -> SysResult {
    if size as i64 <= 0 {
        return Err(err(libc::EINVAL));
    }
    let path = read_path(mem, path)?;
    let target = if is_self_exe(path.as_bytes()) {
        // The guest's own executable, not armor.
        process.exe_path.clone()
    } else {
        let path = process.root.resolve_c(path);
        let mut target = vec![0u8; PATH_MAX];
        // SAFETY: path is NUL-terminated and target is writable for its length.
        let n = unsafe {
            libc::readlinkat(
                dirfd as i32,
                path.as_ptr(),
                target.as_mut_ptr().cast(),
                target.len(),
            )
        };
        if n < 0 {
            return Err(host_errno());
        }
        target.truncate(n as usize);
        target
    };
    let n = target.len().min(size as usize);
    copy_out(mem, buf, &target[..n])?;
    Ok(n as i64)
}

/// `/proc/self/exe` or `/proc/<our pid>/exe`.
pub(crate) fn is_self_exe(path: &[u8]) -> bool {
    let Some(rest) = path.strip_prefix(b"/proc/") else {
        return false;
    };
    let Some(who) = rest.strip_suffix(b"/exe") else {
        return false;
    };
    who == b"self" || who == std::process::id().to_string().as_bytes()
}

fn prlimit64(mem: &AddressSpace, pid: u64, resource: u64, new: u64, old: u64) -> SysResult {
    // struct rlimit64 is two u64 on both architectures.
    let new_limit = if new != 0 {
        Some(copy_in::<16>(mem, new)?)
    } else {
        None
    };
    let mut old_limit = [0u8; 16];
    let new_ptr = new_limit.as_ref().map_or(std::ptr::null(), |l| l.as_ptr());
    let old_ptr = if old != 0 {
        old_limit.as_mut_ptr()
    } else {
        std::ptr::null_mut()
    };
    // SAFETY: the pointers are null or point to 16-byte host buffers;
    // RLIMIT_* numbers are the same on both architectures.
    let result =
        unsafe { libc::syscall(libc::SYS_prlimit64, pid as i32, resource, new_ptr, old_ptr) };
    if result < 0 {
        return Err(host_errno());
    }
    if old != 0 {
        copy_out(mem, old, &old_limit)?;
    }
    Ok(0)
}

fn getrandom(mem: &AddressSpace, buf: u64, len: u64, flags: u64) -> SysResult {
    let mut bytes = vec![0u8; (len as usize).min(CHUNK)];
    // SAFETY: bytes is writable for its length; GRND_* flags match.
    let n = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), flags as u32) };
    if n < 0 {
        return Err(host_errno());
    }
    copy_out(mem, buf, &bytes[..n as usize])?;
    Ok(n as i64)
}

/// rt_sigaction(2): records the guest's action and returns the previous
/// one; `Signals::set_action` sets the matching host disposition.
fn rt_sigaction(
    mem: &AddressSpace,
    thread: &mut Thread,
    signal: u64,
    act: u64,
    oldact: u64,
    sigsetsize: u64,
) -> SysResult {
    if sigsetsize != 8 || !(1..=NSIG).contains(&signal) {
        return Err(err(libc::EINVAL));
    }
    let new = if act != 0 {
        if signal == libc::SIGKILL as u64 || signal == libc::SIGSTOP as u64 {
            return Err(err(libc::EINVAL));
        }
        Some(copy_in::<SIGACTION_SIZE>(mem, act)?)
    } else {
        None
    };
    if oldact != 0 {
        copy_out(mem, oldact, &thread.signals.action(signal))?;
    }
    if let Some(new) = new {
        thread.signals.set_action(signal, new);
    }
    Ok(0)
}

// rt_sigprocmask `how`, identical on both architectures.
pub(crate) const SIG_BLOCK: u64 = 0;
pub(crate) const SIG_UNBLOCK: u64 = 1;
pub(crate) const SIG_SETMASK: u64 = 2;

/// ppoll(2), for both ABIs (`time`: the width of the timeout's fields). With
/// a signal mask (always 8 bytes, as Linux's `sigsetsize`), the guest's mask
/// is switched for the wait (`Signals::begin_masked_wait`).
pub(crate) fn ppoll(mem: &AddressSpace, thread: &mut Thread, time: Word, a: [u64; 6]) -> SysResult {
    if a[3] == 0 {
        return events::ppoll(mem, time, a[0], a[1], a[2], None);
    }
    if a[4] != 8 {
        return Err(err(libc::EINVAL));
    }
    let mask = u64::from_le_bytes(copy_in::<8>(mem, a[3])?);
    match thread.signals.begin_masked_wait(mask) {
        Some(host) => events::ppoll(mem, time, a[0], a[1], a[2], Some(&host)),
        None => Err(err(libc::EINTR)),
    }
}

/// rt_sigqueueinfo(2) (`tgid` none: `pid`, `signal`, `info`) and
/// rt_tgsigqueueinfo(2) (`tgid`, then the thread): the siginfo of a queued
/// signal has the same layout on both architectures, and the pids and tids
/// are the host's.
fn queue_signal(
    mem: &AddressSpace,
    tgid: Option<u64>,
    id: u64,
    signal: u64,
    info: u64,
) -> SysResult {
    let info = copy_in::<128>(mem, info)?;
    // SAFETY: info is a 128-byte siginfo; the rest are integers.
    let r = unsafe {
        match tgid {
            None => libc::syscall(
                libc::SYS_rt_sigqueueinfo,
                id as i32,
                signal as i32,
                info.as_ptr(),
            ),
            Some(tgid) => libc::syscall(
                libc::SYS_rt_tgsigqueueinfo,
                tgid as i32,
                id as i32,
                signal as i32,
                info.as_ptr(),
            ),
        }
    };
    if r < 0 { Err(host_errno()) } else { Ok(0) }
}

/// rt_sigtimedwait(2) on x86_64: a 64-bit set, an optional 16-byte
/// timespec, the siginfo written if asked.
fn rt_sigtimedwait(
    mem: &AddressSpace,
    thread: &mut Thread,
    set: u64,
    info: u64,
    timeout: u64,
    sigsetsize: u64,
) -> SysResult {
    if sigsetsize != 8 {
        return Err(err(libc::EINVAL));
    }
    let wanted = u64::from_le_bytes(copy_in::<8>(mem, set)?);
    let limit = if timeout != 0 {
        let raw = copy_in::<16>(mem, timeout)?;
        let sec = i64::from_le_bytes(raw[..8].try_into().expect("8 bytes"));
        let nsec = i64::from_le_bytes(raw[8..].try_into().expect("8 bytes"));
        if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
            return Err(err(libc::EINVAL));
        }
        Some(libc::timespec {
            tv_sec: sec,
            tv_nsec: nsec,
        })
    } else {
        None
    };
    let (signal, siginfo) = thread.signals.timed_wait(wanted, limit.as_ref())?;
    if info != 0 {
        copy_out(mem, info, &siginfo)?;
    }
    Ok(signal as i64)
}

fn rt_sigprocmask(
    mem: &AddressSpace,
    thread: &mut Thread,
    how: u64,
    set: u64,
    oldset: u64,
    sigsetsize: u64,
) -> SysResult {
    if sigsetsize != 8 {
        return Err(err(libc::EINVAL));
    }
    let old = thread.signals.mask();
    if set != 0 {
        let new = u64::from_le_bytes(copy_in::<8>(mem, set)?);
        let mask = match how {
            SIG_BLOCK => old | new,
            SIG_UNBLOCK => old & !new,
            SIG_SETMASK => new,
            _ => return Err(err(libc::EINVAL)),
        };
        thread.signals.set_mask(mask);
    }
    if oldset != 0 {
        copy_out(mem, oldset, &old.to_le_bytes())?;
    }
    Ok(0)
}

fn rt_sigpending(mem: &AddressSpace, thread: &Thread, set: u64, sigsetsize: u64) -> SysResult {
    if sigsetsize > 8 {
        return Err(err(libc::EINVAL));
    }
    let pending = thread.signals.pending().to_le_bytes();
    copy_out(mem, set, &pending[..sigsetsize as usize])?;
    Ok(0)
}

/// sigaltstack(2); `stack_t` is sp (8), flags (4, then 4 of padding),
/// size (8) on x86_64.
fn sigaltstack(mem: &AddressSpace, thread: &mut Thread, rsp: u64, new: u64, old: u64) -> SysResult {
    let new = if new != 0 {
        let raw = copy_in::<24>(mem, new)?;
        let word = |at: usize| u64::from_le_bytes(raw[at..at + 8].try_into().expect("8 bytes"));
        Some((word(0), word(8) as u32, word(16)))
    } else {
        None
    };
    let (sp, flags, size) = thread.signals.alt_stack(new, rsp).map_err(err)?;
    if old != 0 {
        let mut raw = [0u8; 24];
        raw[..8].copy_from_slice(&sp.to_le_bytes());
        raw[8..12].copy_from_slice(&flags.to_le_bytes());
        raw[16..24].copy_from_slice(&size.to_le_bytes());
        copy_out(mem, old, &raw)?;
    }
    Ok(0)
}

/// pipe(2) and pipe2(2): flags (O_CLOEXEC, O_NONBLOCK, O_DIRECT) are
/// translated like open's.
fn pipe2(mem: &AddressSpace, fds: u64, flags: u64) -> SysResult {
    let mut pair = [0i32; 2];
    // SAFETY: pair is writable for the two descriptors pipe2 returns.
    if unsafe { libc::pipe2(pair.as_mut_ptr(), open_flags_to_host(flags) as i32) } < 0 {
        return Err(host_errno());
    }
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&pair[0].to_le_bytes());
    bytes[4..].copy_from_slice(&pair[1].to_le_bytes());
    if let Err(e) = copy_out(mem, fds, &bytes) {
        // SAFETY: closing the two descriptors just created.
        unsafe {
            libc::close(pair[0]);
            libc::close(pair[1]);
        }
        return Err(e);
    }
    Ok(0)
}

/// setitimer(2): `struct itimerval` (two timevals, 32 bytes) and the
/// timer numbers are the same on both architectures.
fn setitimer(mem: &AddressSpace, which: u64, new: u64, old: u64) -> SysResult {
    let value = if new != 0 {
        Some(copy_in::<32>(mem, new)?)
    } else {
        None
    };
    let mut previous = [0u8; 32];
    let value_ptr = value.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
    // SAFETY: both pointers are null or 32-byte host buffers.
    let result = unsafe {
        libc::syscall(
            libc::SYS_setitimer,
            which as i32,
            value_ptr,
            previous.as_mut_ptr(),
        )
    };
    if result < 0 {
        return Err(host_errno());
    }
    if old != 0 {
        copy_out(mem, old, &previous)?;
    }
    Ok(0)
}

/// timer_create(2): `struct sigevent` (64 bytes) is the same on both
/// architectures, and a SIGEV_THREAD_ID target is a host tid, which is the
/// guest's.
fn timer_create(mem: &AddressSpace, clock: u64, event: u64, id: u64) -> SysResult {
    let event = if event != 0 {
        Some(copy_in::<64>(mem, event)?)
    } else {
        None
    };
    let event_ptr = event.as_ref().map_or(std::ptr::null(), |e| e.as_ptr());
    let mut timer = 0i32;
    // SAFETY: event_ptr is null or a 64-byte host buffer; timer is writable.
    let result = unsafe {
        libc::syscall(
            libc::SYS_timer_create,
            clock as i32,
            event_ptr,
            &raw mut timer,
        )
    };
    if result < 0 {
        return Err(host_errno());
    }
    copy_out(mem, id, &timer.to_le_bytes())?;
    Ok(0)
}

/// timer_settime(2): `struct itimerspec` (32 bytes) is the same on both.
fn timer_settime(mem: &AddressSpace, timer: u64, flags: u64, new: u64, old: u64) -> SysResult {
    let value = copy_in::<32>(mem, new)?;
    let mut previous = [0u8; 32];
    // SAFETY: both buffers are 32-byte host buffers.
    let result = unsafe {
        libc::syscall(
            libc::SYS_timer_settime,
            timer as i32,
            flags as i32,
            value.as_ptr(),
            previous.as_mut_ptr(),
        )
    };
    if result < 0 {
        return Err(host_errno());
    }
    if old != 0 {
        copy_out(mem, old, &previous)?;
    }
    Ok(0)
}

fn timer_gettime(mem: &AddressSpace, timer: u64, value: u64) -> SysResult {
    let mut current = [0u8; 32];
    // SAFETY: current is a 32-byte host buffer.
    let result =
        unsafe { libc::syscall(libc::SYS_timer_gettime, timer as i32, current.as_mut_ptr()) };
    if result < 0 {
        return Err(host_errno());
    }
    copy_out(mem, value, &current)?;
    Ok(0)
}

/// gettimeofday(2): `struct timeval` (16 bytes) and the obsolete
/// `struct timezone` (8 bytes) are the same on both.
fn gettimeofday(mem: &AddressSpace, tv: u64, tz: u64) -> SysResult {
    let mut time = [0u8; 16];
    let mut zone = [0u8; 8];
    // SAFETY: both are host buffers of the sizes the kernel writes.
    if unsafe { libc::syscall(libc::SYS_gettimeofday, time.as_mut_ptr(), zone.as_mut_ptr()) } < 0 {
        return Err(host_errno());
    }
    if tv != 0 {
        copy_out(mem, tv, &time)?;
    }
    if tz != 0 {
        copy_out(mem, tz, &zone)?;
    }
    Ok(0)
}

/// chdir(2), with the path resolved in the guest root like the other path
/// syscalls (ADR-0013).
fn chdir(mem: &AddressSpace, root: &GuestRoot, path: u64) -> SysResult {
    let path = root.resolve_c(read_path(mem, path)?);
    // SAFETY: path is NUL-terminated.
    if unsafe { libc::chdir(path.as_ptr()) } < 0 {
        Err(host_errno())
    } else {
        Ok(0)
    }
}

fn own_pid() -> u64 {
    // SAFETY: getpid has no preconditions.
    u64::from(unsafe { libc::getpid() } as u32)
}

/// kill(2). Signals to armor's own process go through the host kernel,
/// whose handler records them for the guest (signal.rs), except those armor
/// keeps on the host (SIGSEGV, ...), which it records itself.
fn kill(pid: u64, signal: u64) -> SysResult {
    if pid == own_pid() && crate::signal::send_to_self(signal, false) {
        return Ok(0);
    }
    host(libc::SYS_kill, &[pid, signal])
}

/// tgkill(2) (and tkill, with armor's own process as the group).
fn tgkill(tgid: u64, tid: u64, signal: u64) -> SysResult {
    // SAFETY: gettid has no preconditions.
    let own_tid = u64::from(unsafe { libc::gettid() } as u32);
    if tgid == own_pid() && tid == own_tid && crate::signal::send_to_self(signal, true) {
        return Ok(0);
    }
    host(libc::SYS_tgkill, &[tgid, tid, signal])
}

fn sysinfo(mem: &AddressSpace, buf: u64) -> SysResult {
    let mut info = [0u8; SYSINFO_SIZE];
    // SAFETY: info is as large as `struct sysinfo` (checked in the docs on
    // both architectures), and the kernel writes only that much.
    if unsafe { libc::syscall(libc::SYS_sysinfo, info.as_mut_ptr()) } < 0 {
        return Err(host_errno());
    }
    copy_out(mem, buf, &info)?;
    Ok(0)
}

fn statfs(mem: &AddressSpace, root: &GuestRoot, path: u64, buf: u64) -> SysResult {
    let path = root.resolve_c(read_path(mem, path)?);
    let mut out = [0u8; STATFS_SIZE];
    // SAFETY: path is NUL-terminated; out is as large as `struct statfs`.
    if unsafe { libc::syscall(libc::SYS_statfs, path.as_ptr(), out.as_mut_ptr()) } < 0 {
        return Err(host_errno());
    }
    copy_out(mem, buf, &out)?;
    Ok(0)
}

fn fstatfs(mem: &AddressSpace, fd: u64, buf: u64) -> SysResult {
    let mut out = [0u8; STATFS_SIZE];
    // SAFETY: out is as large as `struct statfs`.
    if unsafe { libc::syscall(libc::SYS_fstatfs, fd as i32, out.as_mut_ptr()) } < 0 {
        return Err(host_errno());
    }
    copy_out(mem, buf, &out)?;
    Ok(0)
}

fn llistxattr(mem: &AddressSpace, root: &GuestRoot, path: u64, list: u64, size: u64) -> SysResult {
    let path = root.resolve_c(read_path(mem, path)?);
    let mut names = vec![0u8; (size as usize).min(CHUNK)];
    // SAFETY: path is NUL-terminated; names is writable for its length (a
    // zero size only asks for the needed size).
    let n = unsafe { libc::llistxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    if n < 0 {
        return Err(host_errno());
    }
    if size != 0 {
        copy_out(mem, list, &names[..n as usize])?;
    }
    Ok(n as i64)
}

// futex operations (`op & FUTEX_CMD_MASK`), identical on both.
const FUTEX_REQUEUE: u64 = 3;
const FUTEX_CMP_REQUEUE: u64 = 4;
const FUTEX_WAKE_OP: u64 = 5;
const FUTEX_WAIT_BITSET: u64 = 9;
const FUTEX_WAKE_BITSET: u64 = 10;
/// Priority-inheritance futexes: the word holds the owner's TID, and a
/// guest thread's TID is its host thread's (ADR-0018), so the host's
/// operations work on the guest's words as they are.
const FUTEX_LOCK_PI: u64 = 6;
const FUTEX_UNLOCK_PI: u64 = 7;
const FUTEX_TRYLOCK_PI: u64 = 8;
const FUTEX_WAIT_REQUEUE_PI: u64 = 11;
const FUTEX_CMP_REQUEUE_PI: u64 = 12;
const FUTEX_LOCK_PI2: u64 = 13;

/// futex(2), passed to the host kernel with the guest's addresses: the
/// one place where armor hands a guest pointer to the host (ADR-0018).
/// The address is armor's own identity-mapped memory, and armor checks
/// first that the words are mapped and writable. Timeouts are copied into
/// a host buffer. Priority-inheritance operations too (the word holds a
/// TID, and guest TIDs are host TIDs); the robust-list ones are not
/// needed (`set_robust_list` is ENOSYS, as under qemu). `timespec32`: the timeout is the 32-bit `struct timespec` of
/// an i386 guest's old futex (240), widened for the host.
#[allow(clippy::too_many_arguments)]
fn futex(
    mem: &AddressSpace,
    addr: u64,
    op: u64,
    value: u64,
    timeout_or_value2: u64,
    addr2: u64,
    value3: u64,
    timespec32: bool,
) -> Option<SysResult> {
    let cmd = op & FUTEX_CMD_MASK;
    let uses_timeout = matches!(
        cmd,
        FUTEX_WAIT | FUTEX_WAIT_BITSET | FUTEX_LOCK_PI | FUTEX_LOCK_PI2 | FUTEX_WAIT_REQUEUE_PI
    );
    let uses_addr2 = matches!(
        cmd,
        FUTEX_REQUEUE
            | FUTEX_CMP_REQUEUE
            | FUTEX_WAKE_OP
            | FUTEX_WAIT_REQUEUE_PI
            | FUTEX_CMP_REQUEUE_PI
    );
    if !matches!(
        cmd,
        FUTEX_WAIT
            | FUTEX_WAKE
            | FUTEX_WAIT_BITSET
            | FUTEX_WAKE_BITSET
            | FUTEX_LOCK_PI
            | FUTEX_LOCK_PI2
            | FUTEX_UNLOCK_PI
            | FUTEX_TRYLOCK_PI
    ) && !uses_addr2
    {
        return None;
    }
    Some((|| {
        futex_word(mem, addr)?;
        if uses_addr2 {
            futex_word(mem, addr2)?;
        }
        let timeout = if uses_timeout && timeout_or_value2 != 0 && timespec32 {
            let narrow = copy_in::<8>(mem, timeout_or_value2)?;
            let mut wide = [0u8; TIMESPEC_SIZE];
            for i in 0..2 {
                let field = i32::from_le_bytes(narrow[4 * i..4 * i + 4].try_into().expect("4"));
                wide[8 * i..8 * i + 8].copy_from_slice(&i64::from(field).to_le_bytes());
            }
            Some(wide)
        } else if uses_timeout && timeout_or_value2 != 0 {
            Some(copy_in::<TIMESPEC_SIZE>(mem, timeout_or_value2)?)
        } else {
            None
        };
        let fourth = match &timeout {
            Some(t) => t.as_ptr() as usize,
            None if uses_timeout => 0,
            None => timeout_or_value2 as usize,
        };
        // SAFETY: `addr` (and `addr2` when used) are 4-byte aligned words of
        // guest memory, mapped and writable (checked above), so the host
        // kernel may read and compare them; the timeout is a host buffer.
        let result = unsafe {
            libc::syscall(
                libc::SYS_futex,
                addr as usize,
                op as i32,
                value as u32,
                fourth,
                addr2 as usize,
                value3 as u32,
            )
        };
        if result < 0 {
            Err(host_errno())
        } else {
            Ok(result)
        }
    })())
}

/// A futex word: 4-byte aligned, mapped and writable, or EINVAL/EFAULT.
/// Only the page permissions are checked: writing the word to test it
/// would be a non-atomic read-modify-write racing with the other guest
/// threads (it lost mutex updates, found by `c/threads`).
fn futex_word(mem: &AddressSpace, addr: u64) -> Result<(), i64> {
    if !addr.is_multiple_of(4) {
        return Err(err(libc::EINVAL));
    }
    match mem.prot(addr) {
        Some(prot) if prot.contains(Prot::WRITE) => Ok(()),
        _ => Err(err(libc::EFAULT)),
    }
}

/// Wakes the waiters of the futex at `addr` (shared, not private: what
/// Linux does for CLONE_CHILD_CLEARTID, and what pthread_join waits on).
pub fn futex_wake_all(addr: u64) {
    // SAFETY: the caller checked that `addr` is a mapped, writable word.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            addr as usize,
            FUTEX_WAKE as i32,
            i32::MAX,
            0usize,
            0usize,
            0u32,
        )
    };
}

/// clone(2) for threads. Arguments in x86_64 order: flags, stack,
/// parent_tid, child_tid, tls.
/// clone(2) with `[flags, stack, parent_tid, child_tid]` and the TLS of
/// CLONE_SETTLS: a thread (`clone`) or a process (`process::fork`).
fn clone_or_fork(
    cpu: &mut Cpu,
    mem: &AddressSpace,
    process: &Process,
    thread: &mut Thread,
    [flags, stack, parent_tid, child_tid]: [u64; 4],
    tls: Option<NewTls>,
) -> Outcome {
    if flags & CLONE_THREAD == 0 {
        let forked = crate::process::fork(
            cpu, mem, process, thread, flags, stack, parent_tid, child_tid, tls,
        );
        return match forked {
            Ok(result) => {
                cpu.regs[RAX] = result.unwrap_or_else(|e| e) as u64 & mask_of(cpu);
                Outcome::Continue
            }
            Err(what) => Outcome::Unsupported(what),
        };
    }
    // A second guest thread: from now on guest code may run in parallel
    // (ADR-0023). Before the thread exists, so that no code translated for
    // one thread runs in two.
    mem.enter_concurrent_mode();
    clone(cpu, thread, flags, stack, parent_tid, child_tid, tls)
}

/// The bits of rax a syscall result fills: eax for an i386 guest.
fn mask_of(cpu: &Cpu) -> u64 {
    match cpu.mode {
        Mode::Long => u64::MAX,
        Mode::Compat32 => 0xffff_ffff,
    }
}

fn clone(
    cpu: &Cpu,
    thread: &Thread,
    flags: u64,
    stack: u64,
    parent_tid: u64,
    child_tid: u64,
    tls: Option<NewTls>,
) -> Outcome {
    if flags & CLONE_THREAD_FLAGS != CLONE_THREAD_FLAGS
        || flags & !(CLONE_THREAD_FLAGS | CLONE_THREAD_OPTIONAL) != 0
    {
        return Outcome::Unsupported(format!(
            "clone with flags {flags:#x} (only threads; processes need fork)"
        ));
    }
    let mut child = cpu.clone();
    child.regs[RAX] = 0;
    if stack != 0 {
        child.regs[armor_cpu::RSP] = stack;
    }
    if let Some(tls) = tls {
        tls.apply(&mut child);
    }
    let mut child_thread = Thread {
        signals: thread.signals.for_new_thread(),
        clear_child_tid: 0,
    };
    if flags & CLONE_CHILD_CLEARTID != 0 {
        child_thread.clear_child_tid = child_tid;
    }
    Outcome::NewThread(Box::new(NewThread {
        cpu: child,
        thread: child_thread,
        parent_tid: (flags & CLONE_PARENT_SETTID != 0).then_some(parent_tid),
        child_tid: (flags & CLONE_CHILD_SETTID != 0).then_some(child_tid),
    }))
}

/// madvise(2): MADV_DONTNEED zeroes the range, as Linux does for private
/// anonymous memory (glibc frees thread stacks this way); other advice is
/// only a hint.
fn madvise(mem: &AddressSpace, addr: u64, len: u64, advice: u64) -> SysResult {
    if !addr.is_multiple_of(GUEST_PAGE_SIZE) {
        return Err(err(libc::EINVAL));
    }
    if advice == MADV_DONTNEED {
        addr.checked_add(len).ok_or(err(libc::EINVAL))?;
        mem.discard(addr, len).map_err(|_| err(libc::ENOMEM))?;
    }
    Ok(0)
}

fn sched_getaffinity(mem: &AddressSpace, pid: u64, len: u64, mask: u64) -> SysResult {
    let mut bytes = vec![0u8; (len as usize).min(CHUNK)];
    // SAFETY: bytes is writable for its length.
    let n = unsafe {
        libc::syscall(
            libc::SYS_sched_getaffinity,
            pid as i32,
            bytes.len(),
            bytes.as_mut_ptr(),
        )
    };
    if n < 0 {
        return Err(host_errno());
    }
    copy_out(mem, mask, &bytes[..n as usize])?;
    Ok(n)
}

fn clock_gettime(mem: &AddressSpace, clock: u64, ts: u64) -> SysResult {
    let mut out = [0u8; TIMESPEC_SIZE];
    // SAFETY: out is as large as `struct timespec`; clock ids match on both.
    if unsafe { libc::syscall(libc::SYS_clock_gettime, clock as i32, out.as_mut_ptr()) } < 0 {
        return Err(host_errno());
    }
    copy_out(mem, ts, &out)?;
    Ok(0)
}

/// getresuid(2) and getresgid(2): three 32-bit ids, each to its pointer.
fn getres_ids(mem: &AddressSpace, users: bool, ptrs: [u64; 3]) -> SysResult {
    let mut ids = [0u32; 3];
    let number = if users {
        libc::SYS_getresuid
    } else {
        libc::SYS_getresgid
    };
    let p = ids.as_mut_ptr();
    // SAFETY: three writable u32s of `ids` (the offsets stay inside it).
    if unsafe { libc::syscall(number, p, p.add(1), p.add(2)) } < 0 {
        return Err(host_errno());
    }
    for (ptr, id) in ptrs.into_iter().zip(ids) {
        copy_out(mem, ptr, &id.to_le_bytes())?;
    }
    Ok(0)
}

/// sched_setparam, sched_getparam and sched_setscheduler: `struct
/// sched_param` is one int, the same on x86_64, i386 and AArch64, and the
/// policies have the same numbers. `number` says which (x86_64 numbers;
/// i386 passes them mapped). For sched_setscheduler, `a1` is the policy
/// and `a2` the structure; for the others `a1` is the structure.
pub(crate) fn sched_param(
    mem: &AddressSpace,
    number: u64,
    pid: u64,
    a1: u64,
    a2: u64,
) -> SysResult {
    let mut param = [0u8; 4];
    match number {
        nr::SCHED_GETPARAM => {
            // SAFETY: param is one writable int.
            if unsafe { libc::syscall(libc::SYS_sched_getparam, pid as i32, param.as_mut_ptr()) }
                < 0
            {
                return Err(host_errno());
            }
            copy_out(mem, a1, &param)?;
            Ok(0)
        }
        nr::SCHED_SETPARAM => {
            param = copy_in::<4>(mem, a1)?;
            // SAFETY: param is one int.
            check_result(unsafe {
                libc::syscall(libc::SYS_sched_setparam, pid as i32, param.as_ptr())
            })
        }
        _ => {
            param = copy_in::<4>(mem, a2)?;
            // SAFETY: param is one int; the policy an integer.
            check_result(unsafe {
                libc::syscall(
                    libc::SYS_sched_setscheduler,
                    pid as i32,
                    a1 as i32,
                    param.as_ptr(),
                )
            })
        }
    }
}

fn check_result(r: libc::c_long) -> SysResult {
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

/// sched_rr_get_interval(2): the host's 16-byte `struct timespec`, for the
/// caller to copy (an i386 caller narrows it).
pub(crate) fn sched_rr_get_interval(pid: u64) -> Result<[u8; 16], i64> {
    let mut out = [0u8; 16];
    // SAFETY: out is as large as `struct timespec`.
    if unsafe {
        libc::syscall(
            libc::SYS_sched_rr_get_interval,
            pid as i32,
            out.as_mut_ptr(),
        )
    } < 0
    {
        return Err(host_errno());
    }
    Ok(out)
}

/// sched_setaffinity(2): the CPU mask (bytes, the same on both) copied in;
/// at most 1024 bytes, as `sched_getaffinity` reads.
fn sched_setaffinity(mem: &AddressSpace, pid: u64, len: u64, mask: u64) -> SysResult {
    let mut bytes = vec![0u8; (len as usize).min(1024)];
    mem.read(mask, &mut bytes).map_err(|_| err(libc::EFAULT))?;
    // SAFETY: bytes holds the mask, of its length.
    let r = unsafe {
        libc::syscall(
            libc::SYS_sched_setaffinity,
            pid as i32,
            bytes.len(),
            bytes.as_ptr(),
        )
    };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

/// clock_getres(2): the resolution of a clock, as a 16-byte `struct
/// timespec` (a null pointer only checks the clock id).
pub(crate) fn clock_getres(mem: &AddressSpace, clock: u64, ts: u64) -> SysResult {
    let mut out = [0u8; TIMESPEC_SIZE];
    // SAFETY: out is as large as `struct timespec`; clock ids match on both.
    if unsafe { libc::syscall(libc::SYS_clock_getres, clock as i32, out.as_mut_ptr()) } < 0 {
        return Err(host_errno());
    }
    if ts != 0 {
        copy_out(mem, ts, &out)?;
    }
    Ok(0)
}

/// clock_nanosleep(2): clock ids, TIMER_ABSTIME and `struct timespec` match
/// on both architectures. When interrupted, the time left goes back to the
/// guest.
fn clock_nanosleep(
    mem: &AddressSpace,
    clock: u64,
    flags: u64,
    request: u64,
    remain: u64,
) -> SysResult {
    let wanted = copy_in::<TIMESPEC_SIZE>(mem, request)?;
    let mut left = [0u8; TIMESPEC_SIZE];
    // SAFETY: both buffers are as large as `struct timespec`.
    let result = unsafe {
        libc::syscall(
            libc::SYS_clock_nanosleep,
            clock as i32,
            flags as i32,
            wanted.as_ptr(),
            left.as_mut_ptr(),
        )
    };
    if result < 0 {
        let errno = host_errno();
        if errno == err(libc::EINTR) && remain != 0 {
            copy_out(mem, remain, &left)?;
        }
        return Err(errno);
    }
    Ok(0)
}

/// statx(2): `struct statx` uses fixed-size fields, the same on both.
fn statx(
    mem: &AddressSpace,
    root: &GuestRoot,
    dirfd: u64,
    path: u64,
    flags: u64,
    mask: u64,
    buf: u64,
) -> SysResult {
    let path = root.resolve_c(read_path(mem, path)?);
    let mut out = [0u8; STATX_SIZE];
    // SAFETY: path is NUL-terminated; out is as large as `struct statx`.
    let result = unsafe {
        libc::syscall(
            libc::SYS_statx,
            dirfd as i32,
            path.as_ptr(),
            flags as i32,
            mask as u32,
            out.as_mut_ptr(),
        )
    };
    if result < 0 {
        return Err(host_errno());
    }
    copy_out(mem, buf, &out)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Syscalls the tests below must not make whatever the arguments:
    /// pause, and ppoll with no descriptors and no timeout, wait for a
    /// signal; fork, vfork and clone would duplicate the test process.
    const BLOCKS: [u64; 5] = [nr::PAUSE, nr::PPOLL, nr::FORK, nr::VFORK, nr::CLONE];

    /// Calls `dispatch` with the first argument -1 (an invalid fd, uid or
    /// address) and the others zero, which makes every implemented syscall
    /// fail harmlessly (EBADF, EINVAL, EFAULT) or only query state.
    #[test]
    fn implemented_list_matches_dispatch() {
        let mem = AddressSpace::new();
        let process = Process::new(
            b"/guest".to_vec(),
            0x1000_0000,
            GuestRoot::none(),
            PathBuf::from("/armor"),
        );
        let mut thread = Thread::default();
        for number in (0..1024).filter(|&n| !BLOCKS.contains(&n)) {
            let mut cpu = Cpu::new(0, 0);
            cpu.regs[RAX] = number;
            cpu.regs[RDI] = u64::MAX;
            let outcome = dispatch(&mut cpu, &mem, &process, &mut thread);
            let unknown = matches!(&outcome, Outcome::Unsupported(what) if *what == format!("syscall {number}"));
            assert_eq!(!unknown, is_implemented(number), "syscall {number}");
        }
    }

    /// mremap's errors as Linux returns them with 4K pages. A zero size
    /// (EINVAL) and an unmapped range (EFAULT) were checked on the host's
    /// kernel with a native C program (2026-10-13); qemu-x86_64 says ENOMEM
    /// and EINVAL there (docs/testing.md). Growing into a mapped page
    /// without MREMAP_MAYMOVE (ENOMEM) and a range over two mappings
    /// (EFAULT) follow mremap(2): the 16K-page host cannot show them.
    #[test]
    fn mremap_errors_follow_linux() {
        let mem = AddressSpace::new();
        let rw = Prot::READ | Prot::WRITE;
        let addr = mem.map_anywhere(4 * GUEST_PAGE_SIZE, rw).unwrap();
        let call =
            |old, old_len, new_len, flags| mremap(&mem, old, old_len, new_len, flags).unwrap();
        assert_eq!(
            call(addr, GUEST_PAGE_SIZE, 0, MREMAP_MAYMOVE),
            Err(err(libc::EINVAL))
        );
        assert_eq!(
            call(
                addr + 1,
                GUEST_PAGE_SIZE,
                2 * GUEST_PAGE_SIZE,
                MREMAP_MAYMOVE
            ),
            Err(err(libc::EINVAL))
        );
        // Growing into the next mapped page without MREMAP_MAYMOVE.
        assert_eq!(
            call(addr, GUEST_PAGE_SIZE, 2 * GUEST_PAGE_SIZE, 0),
            Err(err(libc::ENOMEM))
        );
        // Mixed protections in the old range.
        mem.protect(addr + GUEST_PAGE_SIZE, GUEST_PAGE_SIZE, Prot::READ)
            .unwrap();
        assert_eq!(
            call(
                addr,
                2 * GUEST_PAGE_SIZE,
                8 * GUEST_PAGE_SIZE,
                MREMAP_MAYMOVE
            ),
            Err(err(libc::EFAULT))
        );
        mem.unmap(addr, 4 * GUEST_PAGE_SIZE).unwrap();
        assert_eq!(
            call(addr, GUEST_PAGE_SIZE, 2 * GUEST_PAGE_SIZE, MREMAP_MAYMOVE),
            Err(err(libc::EFAULT))
        );
    }

    /// Guest input must never make armor panic: every implemented syscall
    /// with every argument -1 (fds, sizes, addresses all invalid).
    #[test]
    fn extreme_arguments_do_not_panic() {
        let mem = AddressSpace::new();
        let process = Process::new(
            b"/guest".to_vec(),
            0x1000_0000,
            GuestRoot::none(),
            PathBuf::from("/armor"),
        );
        let mut thread = Thread::default();
        for number in (0..1024).filter(|&n| is_implemented(n) && !BLOCKS.contains(&n)) {
            let mut cpu = Cpu::new(0, 0);
            cpu.regs = [u64::MAX; 16];
            cpu.regs[RAX] = number;
            dispatch(&mut cpu, &mem, &process, &mut thread);
        }
    }

    #[test]
    fn open_flags_are_translated_both_ways() {
        // O_RDONLY|O_DIRECTORY|O_CLOEXEC as glibc on x86_64 passes it.
        let guest = 0o200000 | 0o2000000;
        let host = open_flags_to_host(guest);
        assert_eq!(host, (libc::O_DIRECTORY | libc::O_CLOEXEC) as u64);
        assert_eq!(open_flags_to_guest(host), guest);
        assert_eq!(
            open_flags_to_host(0o400000 | 0o40000 | 0o100000),
            (libc::O_NOFOLLOW | libc::O_DIRECT) as u64 | ARM_O_LARGEFILE
        );
        // Flags with the same value pass through.
        let same = 0o1 | 0o100 | 0o1000 | 0o2000 | 0o4000;
        assert_eq!(open_flags_to_host(same), same);
    }

    #[test]
    fn self_exe_paths() {
        assert!(is_self_exe(b"/proc/self/exe"));
        let own = format!("/proc/{}/exe", std::process::id());
        assert!(is_self_exe(own.as_bytes()));
        assert!(!is_self_exe(b"/proc/1/exe"));
        assert!(!is_self_exe(b"/proc/self/cwd"));
    }

    #[test]
    fn stat_is_converted_to_the_x86_64_layout() {
        // SAFETY: an all-zero `stat` is a valid value of this plain C struct.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        st.st_nlink = 3;
        st.st_mode = 0o100644;
        st.st_blksize = 4096;
        st.st_size = 12345;
        let out = x86_stat(&st);
        assert_eq!(u64::from_le_bytes(out[16..24].try_into().unwrap()), 3);
        assert_eq!(
            u32::from_le_bytes(out[24..28].try_into().unwrap()),
            0o100644
        );
        assert_eq!(i64::from_le_bytes(out[48..56].try_into().unwrap()), 12345);
        assert_eq!(i64::from_le_bytes(out[56..64].try_into().unwrap()), 4096);
    }

    #[test]
    fn rt_sigaction_records_and_returns_the_previous_action() {
        let mem = AddressSpace::new();
        let mut thread = Thread::default();
        let buf = mem.map_anywhere(4096, Prot::READ | Prot::WRITE).unwrap();
        // handler 0x401000, flags SA_RESTORER, restorer 0x402000, empty mask.
        let mut action = [0u8; SIGACTION_SIZE];
        action[..8].copy_from_slice(&0x40_1000u64.to_le_bytes());
        action[8..16].copy_from_slice(&0x0400_0000u64.to_le_bytes());
        action[16..24].copy_from_slice(&0x40_2000u64.to_le_bytes());
        mem.write(buf, &action).unwrap();
        let usr2 = libc::SIGUSR2 as u64;
        assert_eq!(rt_sigaction(&mem, &mut thread, usr2, buf, 0, 8), Ok(0));
        let old = buf + 64;
        assert_eq!(rt_sigaction(&mem, &mut thread, usr2, 0, old, 8), Ok(0));
        let mut read_back = [0u8; SIGACTION_SIZE];
        mem.read(old, &mut read_back).unwrap();
        assert_eq!(read_back, action);
        // SIGKILL cannot be changed; a wrong sigset size is invalid.
        let kill = libc::SIGKILL as u64;
        let einval = Err(err(libc::EINVAL));
        assert_eq!(rt_sigaction(&mem, &mut thread, kill, buf, 0, 8), einval);
        assert_eq!(rt_sigaction(&mem, &mut thread, usr2, buf, 0, 16), einval);
        assert_eq!(rt_sigaction(&mem, &mut thread, 65, 0, old, 8), einval);
    }

    #[test]
    fn futex_checks_the_word_and_passes_the_rest_to_the_host() {
        let mem = AddressSpace::new();
        let word = mem.map_anywhere(4096, Prot::READ | Prot::WRITE).unwrap();
        mem.write(word, &5u32.to_le_bytes()).unwrap();
        let private = 128;
        let futex_at =
            |addr, op, value, timeout| futex(&mem, addr, op, value, timeout, 0, 0, false);
        assert_eq!(futex_at(word, FUTEX_WAKE | private, 1, 0), Some(Ok(0)));
        // The value changed: the wait returns at once.
        assert_eq!(
            futex_at(word, FUTEX_WAIT | private, 4, 0),
            Some(Err(err(libc::EAGAIN)))
        );
        // A matching value waits until the 1 ms timeout.
        let timeout = word + 64;
        mem.write(
            timeout,
            &[0u64.to_le_bytes(), 1_000_000u64.to_le_bytes()].concat(),
        )
        .unwrap();
        assert_eq!(
            futex_at(word, FUTEX_WAIT | private, 5, timeout),
            Some(Err(err(libc::ETIMEDOUT)))
        );
        assert_eq!(
            futex_at(word + 1, FUTEX_WAKE, 1, 0),
            Some(Err(err(libc::EINVAL)))
        );
        let read_only = mem.map_anywhere(4096, Prot::READ).unwrap();
        assert_eq!(
            futex_at(read_only, FUTEX_WAKE, 1, 0),
            Some(Err(err(libc::EFAULT)))
        );
        // FUTEX_TRYLOCK_PI on a free word: the host writes our TID there.
        let pi_word = word + 128;
        assert_eq!(futex_at(pi_word, FUTEX_TRYLOCK_PI, 0, 0), Some(Ok(0)));
        let tid = u32::from_le_bytes(copy_in::<4>(&mem, pi_word).unwrap());
        // SAFETY: gettid has no preconditions.
        assert_eq!(tid as i32, unsafe { libc::gettid() });
        // An operation Linux does not have (14): left to the caller.
        assert_eq!(futex_at(word, 14, 0, 0), None);
    }

    /// writev's results as the host kernel gives them (a native C
    /// program, 2026-10-13): the descriptor first, then EINVAL for too
    /// many entries or a negative length, EFAULT for a bad vector or a
    /// buffer beyond user space; the segments in order; a short write up
    /// to the first bad byte (what Linux does for a regular file).
    #[test]
    fn writev_follows_linux() {
        let mem = AddressSpace::new();
        // Below 4 GiB, so that the i386 layout can point at the data too.
        mem.set_ceiling(1 << 32);
        let data = mem
            .map_anywhere(4 * GUEST_PAGE_SIZE, Prot::READ | Prot::WRITE)
            .unwrap();
        let hole = data + GUEST_PAGE_SIZE;
        mem.write(data, b"ab").unwrap();
        mem.write(data + 16, b"cde").unwrap();
        mem.write(hole - 2, b"xy").unwrap();
        mem.protect(hole, GUEST_PAGE_SIZE, Prot::NONE).unwrap();
        let vector = data + 0x100;
        let set = |entries: &[(u64, u64)]| {
            for (i, &(base, len)) in entries.iter().enumerate() {
                let at = vector + 16 * i as u64;
                mem.write(at, &base.to_le_bytes()).unwrap();
                mem.write(at + 8, &len.to_le_bytes()).unwrap();
            }
        };
        let mut fds = [0i32; 2];
        // SAFETY: fds has room for the two descriptors.
        let created = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK) };
        assert_eq!(created, 0);
        let [reader, writer] = fds.map(|fd| fd as u64);
        let drain = || {
            let mut buf = [0u8; 64];
            // SAFETY: buf is writable for its length.
            let n = unsafe { libc::read(reader as i32, buf.as_mut_ptr().cast(), buf.len()) };
            buf[..n.max(0) as usize].to_vec()
        };

        set(&[(data, 2), (0, 0), (data + 16, 3)]);
        assert_eq!(writev(&mem, writer, vector, 3, 8), Ok(5));
        assert_eq!(drain(), b"abcde");
        assert_eq!(writev(&mem, writer, vector, 0, 8), Ok(0));
        assert_eq!(
            writev(&mem, writer, vector, 1025, 8),
            Err(err(libc::EINVAL))
        );
        assert_eq!(
            writev(&mem, writer, vector, u64::MAX, 8),
            Err(err(libc::EINVAL))
        );
        assert_eq!(writev(&mem, writer, hole, 1, 8), Err(err(libc::EFAULT)));
        // The descriptor comes first: not open, or not open for writing.
        assert_eq!(writev(&mem, 9999, hole, 1, 8), Err(err(libc::EBADF)));
        assert_eq!(writev(&mem, reader, vector, 3, 8), Err(err(libc::EBADF)));

        set(&[(data, 2), (data + 16, u64::MAX)]);
        assert_eq!(writev(&mem, writer, vector, 2, 8), Err(err(libc::EINVAL)));
        set(&[(data, 2), (1 << 47, 1)]);
        assert_eq!(writev(&mem, writer, vector, 2, 8), Err(err(libc::EFAULT)));
        set(&[(hole, 3), (data, 2)]);
        assert_eq!(writev(&mem, writer, vector, 2, 8), Err(err(libc::EFAULT)));
        assert_eq!(drain(), b"");

        set(&[(data, 2), (hole - 2, 4), (data + 16, 3)]);
        assert_eq!(writev(&mem, writer, vector, 3, 8), Ok(4));
        assert_eq!(drain(), b"abxy");

        // The i386 layout: 4-byte pointers and lengths, signed.
        let set32 = |entries: &[(u32, u32)]| {
            for (i, &(base, len)) in entries.iter().enumerate() {
                let at = vector + 8 * i as u64;
                mem.write(at, &base.to_le_bytes()).unwrap();
                mem.write(at + 4, &len.to_le_bytes()).unwrap();
            }
        };
        let low = u32::try_from(data).unwrap();
        set32(&[(low, 2), (low + 16, 3)]);
        assert_eq!(writev(&mem, writer, vector, 2, 4), Ok(5));
        assert_eq!(drain(), b"abcde");
        set32(&[(0, 0x8000_0000)]);
        assert_eq!(writev(&mem, writer, vector, 1, 4), Err(err(libc::EINVAL)));
        for fd in fds {
            // SAFETY: closing the pipe this test opened.
            unsafe { libc::close(fd) };
        }
    }

    #[test]
    fn prot_bits_map_to_guest_permissions() {
        assert_eq!(prot(0), Prot::NONE);
        assert_eq!(prot(7), Prot::READ | Prot::WRITE | Prot::EXEC);
    }

    /// Like Linux, brk does not grow over another mapping, and a huge
    /// request fails at once.
    #[test]
    fn brk_does_not_grow_over_a_mapping() {
        let mem = AddressSpace::new();
        let host = mem.host_page_size();
        // Guest addresses are host addresses, and tests run in parallel: a
        // host page given back could be taken by another test. So the hole
        // for the heap leaves one guest page mapped at each end, and both
        // host pages stay this address space's.
        // Two host pages, and at least four guest pages when they are
        // the same size (a 4K host): room between the two ends.
        let span = (2 * host).max(4 * GUEST_PAGE_SIZE);
        let base = mem.map_anywhere(span, Prot::READ).unwrap();
        let start = base + GUEST_PAGE_SIZE;
        let wall = base + span - GUEST_PAGE_SIZE;
        mem.unmap(start, wall - start).unwrap();
        let process = Process::new(
            b"/guest".to_vec(),
            start,
            GuestRoot::none(),
            PathBuf::from("/armor"),
        );
        assert_eq!(brk(&mem, &process, start + host), (start + host) as i64);
        assert_eq!(brk(&mem, &process, wall + 1), (start + host) as i64);
        assert_eq!(mem.prot(wall), Some(Prot::READ));
        assert_eq!(brk(&mem, &process, 1 << 46), (start + host) as i64);
        assert_eq!(brk(&mem, &process, start), start as i64);
    }

    /// xorshift64: reproducible random numbers without a dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Iterations and seed: `ARMOR_FUZZ_ITERATIONS` and `ARMOR_FUZZ_SEED`
    /// (`cargo xtask fuzz`), else a quick run with a fixed seed.
    fn fuzz_config(default_iterations: u64) -> (u64, u64) {
        let var = |name: &str| std::env::var(name).ok().and_then(|v| v.parse().ok());
        let iterations = var("ARMOR_FUZZ_ITERATIONS").unwrap_or(default_iterations);
        let seed = var("ARMOR_FUZZ_SEED").unwrap_or(0x9e37_79b9_7f4a_7c15) | 1;
        (iterations, seed)
    }

    /// Syscalls the fuzz test may call with any arguments: they only touch
    /// the guest's address space and armor's per-thread state, or only
    /// read host state. Left out: anything that writes files or
    /// descriptors, signals or creates processes, blocks (select, poll,
    /// futex, nanosleep, wait4, read), or changes process-wide host state
    /// the test harness relies on (umask, fcntl, prctl, rlimits).
    const FUZZED: &[u64] = &[
        nr::MMAP,
        nr::MUNMAP,
        nr::MPROTECT,
        nr::MREMAP,
        nr::MADVISE,
        nr::MSYNC,
        nr::BRK,
        nr::UNAME,
        nr::GETCWD,
        nr::TIME,
        nr::GETTIMEOFDAY,
        nr::CLOCK_GETTIME,
        nr::SYSINFO,
        nr::GETRANDOM,
        nr::GETGROUPS,
        nr::SCHED_GETAFFINITY,
        nr::NEWFSTATAT,
        nr::STAT,
        nr::LSTAT,
        nr::STATX,
        nr::ACCESS,
        nr::FACCESSAT,
        nr::FACCESSAT2,
        nr::READLINK,
        nr::READLINKAT,
        nr::STATFS,
        nr::FSTAT,
        nr::FSTATFS,
        nr::RT_SIGPROCMASK,
        nr::RT_SIGPENDING,
        nr::SIGALTSTACK,
        nr::ARCH_PRCTL,
        nr::SET_TID_ADDRESS,
        nr::IOCTL,
        nr::MODIFY_LDT,
        nr::TUXCALL,
    ];

    /// Arguments that probe the checks on guest pointers and sizes:
    /// addresses at the edges of a writable, a read-only and an inaccessible
    /// mapping, unmapped and non-canonical ones, and sizes and integers at
    /// their limits.
    fn argument(rng: &mut Rng, regions: &[(u64, u64)]) -> u64 {
        const PAGE: u64 = GUEST_PAGE_SIZE;
        match rng.below(10) {
            0 => 0,
            1 => rng.below(64),
            2 => u64::MAX - rng.below(8),
            3 => [0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 1 << 47, 1 << 40][rng.below(5) as usize],
            4..=7 => {
                let (start, len) = regions[rng.below(regions.len() as u64) as usize];
                match rng.below(4) {
                    0 => start + rng.below(len),
                    1 => start + len - 1 - rng.below(32),
                    2 => start + rng.below(len / PAGE) * PAGE,
                    _ => start.wrapping_sub(rng.below(32)),
                }
            }
            8 => rng.next() & ((1 << 47) - 1),
            _ => rng.next(),
        }
    }

    /// Syscall arguments come from the guest. Whatever they are, an
    /// implemented syscall must not panic or crash armor, and must leave a
    /// result or a -errno in rax (or report itself unsupported).
    #[test]
    fn fuzz_syscall_arguments() {
        let (iterations, seed) = fuzz_config(5_000);
        let mut rng = Rng(seed);
        let process = Process::new(
            b"/guest".to_vec(),
            0x1000_0000,
            GuestRoot::none(),
            PathBuf::from("/armor"),
        );
        let mut thread = Thread::default();
        let mut i = 0;
        while i < iterations {
            // A fresh address space every 64 calls, as munmap and mremap
            // change it: writable (with paths and random bytes), read-only,
            // and inaccessible regions.
            let mem = AddressSpace::new();
            let mut regions = Vec::new();
            for (pages, prot) in [
                (8, Prot::READ | Prot::WRITE),
                (4, Prot::READ),
                (2, Prot::NONE),
            ] {
                let len = pages * GUEST_PAGE_SIZE;
                let start = mem.map_anywhere(len, prot).expect("room for the regions");
                let mut fill = vec![0u8; len as usize];
                for byte in fill.iter_mut() {
                    *byte = match rng.below(8) {
                        0 => 0,
                        1 => b'/',
                        _ => b'a' + rng.below(26) as u8,
                    };
                }
                mem.write_privileged(start, &fill)
                    .expect("the region is mapped");
                regions.push((start, len));
            }
            for _ in 0..64 {
                let number = FUZZED[rng.below(FUZZED.len() as u64) as usize];
                let mut cpu = Cpu::new(0, 0);
                cpu.regs[RAX] = number;
                for reg in [RDI, RSI, RDX, R10, R8, R9] {
                    cpu.regs[reg] = argument(&mut rng, &regions);
                }
                let args = [RDI, RSI, RDX, R10, R8, R9].map(|r| cpu.regs[r]);
                let context =
                    format!("iteration {i}, seed {seed:#x}, syscall {number}, {args:#x?}");
                match dispatch(&mut cpu, &mem, &process, &mut thread) {
                    Outcome::Continue => {
                        let result = cpu.regs[RAX] as i64;
                        assert!(result >= -4095, "result {result}: {context}");
                    }
                    Outcome::Unsupported(_) => {}
                    other => panic!("unexpected outcome {other:?}: {context}"),
                }
                i += 1;
            }
        }
    }
}
