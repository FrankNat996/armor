//! i386 syscalls (`int 0x80`) of a 32-bit guest (ADR-0029). Numbers from
//! /usr/i686-linux-gnu/include/asm/unistd_32.h; the number is in eax, the
//! arguments in ebx, ecx, edx, esi, edi, ebp, the result in eax.
//!
//! Where the i386 layout of a call matches the x86_64 one (fixed-size
//! fields, or only integers), the x86_64 implementation serves both: the
//! arguments are zero-extended 32-bit values. The calls whose structures
//! have `long`s or pointers (`iovec`, `rlimit`, the 32-bit `timespec`,
//! `user_desc`) are converted here. Calls not listed stop armor with an
//! explicit message, as for x86_64 (docs/tech/syscalls.md).

use armor_cpu::{
    Cpu, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP, TLS_ENTRIES, TLS_FIRST_ENTRY, TlsEntry,
};
use armor_mem::AddressSpace;

use super::{
    NewTls, Outcome, Process, SysResult, Thread, brk, chdir, clock_gettime, clock_nanosleep,
    clone_or_fork, copy_in, copy_out, err, faccessat, fcntl, futex, getcwd, getdents64, getrandom,
    host, host_errno, ioctl, kill, madvise, mmap, mprotect, mremap, munmap, openat, own_pid, pipe2,
    pread64, prlimit64, pwrite64, read, readlinkat, rt_sigpending, rt_sigprocmask, statx, tgkill,
    uname, unlinkat, write, writev,
};
use crate::events;
use crate::files::{self, AT_FDCWD};
use crate::net::Word;
use crate::signal::{NSIG, SIGACTION_SIZE};

pub mod nr {
    pub const EXIT: u64 = 1;
    pub const FORK: u64 = 2;
    pub const WAITPID: u64 = 7;
    pub const EXECVE: u64 = 11;
    pub const FTRUNCATE: u64 = 93;
    pub const WAIT4: u64 = 114;
    pub const CLONE: u64 = 120;
    pub const VFORK: u64 = 190;
    pub const FTRUNCATE64: u64 = 194;
    pub const CLONE3: u64 = 435;
    pub const FUTEX: u64 = 240;
    pub const SETPGID: u64 = 57;
    pub const UMASK: u64 = 60;
    pub const GETPGRP: u64 = 65;
    pub const SETSID: u64 = 66;
    pub const GETPRIORITY: u64 = 96;
    pub const SETPRIORITY: u64 = 97;
    pub const FSYNC: u64 = 118;
    pub const GETPGID: u64 = 132;
    pub const FCHDIR: u64 = 133;
    pub const GETSID: u64 = 147;
    pub const FDATASYNC: u64 = 148;
    pub const PRCTL: u64 = 172;
    pub const SCHED_GETAFFINITY: u64 = 242;
    pub const STATFS64: u64 = 268;
    pub const FSTATFS64: u64 = 269;
    pub const FADVISE64_64: u64 = 272;
    pub const SYSINFO: u64 = 116;
    pub const SOCKETCALL: u64 = 102;
    pub const SOCKET: u64 = 359;
    pub const SOCKETPAIR: u64 = 360;
    pub const BIND: u64 = 361;
    pub const CONNECT: u64 = 362;
    pub const LISTEN: u64 = 363;
    pub const ACCEPT4: u64 = 364;
    pub const GETSOCKOPT: u64 = 365;
    pub const SETSOCKOPT: u64 = 366;
    pub const GETSOCKNAME: u64 = 367;
    pub const GETPEERNAME: u64 = 368;
    pub const SENDTO: u64 = 369;
    pub const SENDMSG: u64 = 370;
    pub const RECVFROM: u64 = 371;
    pub const RECVMSG: u64 = 372;
    pub const SHUTDOWN: u64 = 373;
    pub const LLISTXATTR: u64 = 233;
    pub const TIMER_CREATE: u64 = 259;
    pub const TIMER_SETTIME: u64 = 260;
    pub const TIMER_GETTIME: u64 = 261;
    pub const TIMER_GETOVERRUN: u64 = 262;
    pub const TIMER_DELETE: u64 = 263;
    pub const TIMER_GETTIME64: u64 = 408;
    pub const TIMER_SETTIME64: u64 = 409;
    pub const MSYNC: u64 = 144;
    pub const NANOSLEEP: u64 = 162;
    pub const CLOCK_NANOSLEEP: u64 = 267;
    pub const ALARM: u64 = 27;
    pub const PAUSE: u64 = 29;
    pub const SETITIMER: u64 = 104;
    pub const GETITIMER: u64 = 105;
    pub const SIGRETURN: u64 = 119;
    pub const RT_SIGRETURN: u64 = 173;
    pub const RT_SIGACTION: u64 = 174;
    pub const RT_SIGPENDING: u64 = 176;
    pub const RT_SIGSUSPEND: u64 = 179;
    pub const PREAD64: u64 = 180;
    pub const PWRITE64: u64 = 181;
    pub const SIGALTSTACK: u64 = 186;
    pub const TKILL: u64 = 238;
    pub const READ: u64 = 3;
    pub const WRITE: u64 = 4;
    pub const OPEN: u64 = 5;
    pub const CLOSE: u64 = 6;
    pub const UNLINK: u64 = 10;
    pub const CHDIR: u64 = 12;
    pub const LSEEK: u64 = 19;
    pub const GETPID: u64 = 20;
    pub const ACCESS: u64 = 33;
    pub const KILL: u64 = 37;
    pub const MKDIR: u64 = 39;
    pub const RMDIR: u64 = 40;
    pub const DUP: u64 = 41;
    pub const PIPE: u64 = 42;
    pub const BRK: u64 = 45;
    pub const IOCTL: u64 = 54;
    pub const FCNTL: u64 = 55;
    pub const DUP2: u64 = 63;
    pub const GETPPID: u64 = 64;
    pub const GETTIMEOFDAY: u64 = 78;
    pub const READLINK: u64 = 85;
    pub const MUNMAP: u64 = 91;
    pub const UNAME: u64 = 122;
    pub const MPROTECT: u64 = 125;
    pub const LLSEEK: u64 = 140;
    pub const WRITEV: u64 = 146;
    pub const SCHED_YIELD: u64 = 158;
    pub const MREMAP: u64 = 163;
    pub const RT_SIGPROCMASK: u64 = 175;
    pub const SIGPROCMASK: u64 = 126;
    pub const MEMFD_CREATE: u64 = 356;
    pub const CLOCK_GETRES: u64 = 266;
    pub const CLOCK_GETRES_TIME64: u64 = 406;
    pub const SCHED_SETPARAM: u64 = 154;
    pub const SCHED_GETPARAM: u64 = 155;
    pub const SCHED_SETSCHEDULER: u64 = 156;
    pub const SCHED_GETSCHEDULER: u64 = 157;
    pub const SCHED_GET_PRIORITY_MAX: u64 = 159;
    pub const SCHED_GET_PRIORITY_MIN: u64 = 160;
    pub const SCHED_RR_GET_INTERVAL: u64 = 161;
    pub const SCHED_RR_GET_INTERVAL_TIME64: u64 = 423;
    pub const GETCWD: u64 = 183;
    pub const UGETRLIMIT: u64 = 191;
    pub const MMAP2: u64 = 192;
    pub const GETUID32: u64 = 199;
    pub const GETGID32: u64 = 200;
    pub const GETEUID32: u64 = 201;
    pub const GETEGID32: u64 = 202;
    pub const MADVISE: u64 = 219;
    pub const GETDENTS64: u64 = 220;
    pub const FCNTL64: u64 = 221;
    pub const GETTID: u64 = 224;
    pub const SET_THREAD_AREA: u64 = 243;
    pub const GET_THREAD_AREA: u64 = 244;
    pub const EXIT_GROUP: u64 = 252;
    pub const SET_TID_ADDRESS: u64 = 258;
    pub const CLOCK_GETTIME: u64 = 265;
    pub const TGKILL: u64 = 270;
    pub const OPENAT: u64 = 295;
    pub const UNLINKAT: u64 = 301;
    pub const READLINKAT: u64 = 305;
    pub const FACCESSAT: u64 = 307;
    pub const SET_ROBUST_LIST: u64 = 311;
    pub const DUP3: u64 = 330;
    pub const PIPE2: u64 = 331;
    pub const PRLIMIT64: u64 = 340;
    pub const GETRANDOM: u64 = 355;
    pub const STATX: u64 = 383;
    pub const RSEQ: u64 = 386;
    pub const CLOCK_GETTIME64: u64 = 403;
    pub const CLOCK_NANOSLEEP_TIME64: u64 = 407;
    pub const FUTEX_TIME64: u64 = 422;
    pub const FACCESSAT2: u64 = 439;
    pub const NEWSELECT: u64 = 142;
    pub const POLL: u64 = 168;
    pub const EPOLL_CREATE: u64 = 254;
    pub const EPOLL_CTL: u64 = 255;
    pub const EPOLL_WAIT: u64 = 256;
    pub const EPOLL_PWAIT: u64 = 319;
    pub const EVENTFD: u64 = 323;
    pub const EVENTFD2: u64 = 328;
    pub const EPOLL_CREATE1: u64 = 329;
    pub const PSELECT6: u64 = 308;
    pub const PPOLL: u64 = 309;
    pub const PSELECT6_TIME64: u64 = 413;
    pub const PPOLL_TIME64: u64 = 414;
    pub const EPOLL_PWAIT2: u64 = 441;
    pub const LINK: u64 = 9;
    pub const MKNOD: u64 = 14;
    pub const CHMOD: u64 = 15;
    pub const RENAME: u64 = 38;
    pub const SYMLINK: u64 = 83;
    pub const FCHMOD: u64 = 94;
    pub const IPC: u64 = 117;
    pub const FLOCK: u64 = 143;
    pub const STAT64: u64 = 195;
    pub const LSTAT64: u64 = 196;
    pub const FSTAT64: u64 = 197;
    pub const LCHOWN32: u64 = 198;
    pub const FCHOWN32: u64 = 207;
    pub const CHOWN32: u64 = 212;
    pub const MKDIRAT: u64 = 296;
    pub const MKNODAT: u64 = 297;
    pub const FCHOWNAT: u64 = 298;
    pub const FSTATAT64: u64 = 300;
    pub const RENAMEAT: u64 = 302;
    pub const LINKAT: u64 = 303;
    pub const SYMLINKAT: u64 = 304;
    pub const FCHMODAT: u64 = 306;
    pub const UTIMENSAT: u64 = 320;
    pub const RENAMEAT2: u64 = 353;
    pub const SEMGET: u64 = 393;
    pub const SEMCTL: u64 = 394;
    pub const UTIMENSAT_TIME64: u64 = 412;
    pub const SEMTIMEDOP_TIME64: u64 = 420;
    pub const FCHMODAT2: u64 = 452;
}

/// The i386 `mmap2` offset unit: 4096 bytes, whatever the page size.
const MMAP2_UNIT: u64 = 4096;
/// `RLIM_INFINITY` of the 32-bit `struct rlimit` (`COMPAT_RLIM_INFINITY`).
const RLIM32_INFINITY: u64 = 0xffff_ffff;
/// `struct user_desc`: entry_number, base_addr, limit, and the flags word.
const USER_DESC_SIZE: usize = 16;
/// The `user_desc` flags of an empty entry: read_exec_only and
/// seg_not_present (Linux's `LDT_empty`).
const USER_DESC_EMPTY_FLAGS: u32 = 1 << 3 | 1 << 5;
const USER_DESC_SEG_32BIT: u32 = 1;
const USER_DESC_CONTENTS: u32 = 3 << 1;
/// The flags bits `user_desc` has on i386 (no `lm`).
const USER_DESC_FLAGS: u32 = 0x7f;

/// Whether the i386 `dispatch` handles syscall `number` (possibly only
/// some uses of it). Kept in sync with `dispatch` by the
/// `implemented_list_matches_dispatch` test.
pub fn is_implemented(number: u64) -> bool {
    use nr::*;
    matches!(
        number,
        EXIT | FORK
            | WAITPID
            | EXECVE
            | FTRUNCATE
            | WAIT4
            | CLONE
            | VFORK
            | FTRUNCATE64
            | CLONE3
            | FUTEX
            | SETPGID
            | UMASK
            | GETPGRP
            | SETSID
            | GETPRIORITY
            | SETPRIORITY
            | FSYNC
            | GETPGID
            | FCHDIR
            | GETSID
            | FDATASYNC
            | PRCTL
            | SCHED_GETAFFINITY
            | STATFS64
            | FSTATFS64
            | FADVISE64_64
            | SYSINFO
            | SOCKETCALL
            | SOCKET
            | SOCKETPAIR
            | BIND
            | CONNECT
            | LISTEN
            | ACCEPT4
            | GETSOCKOPT
            | SETSOCKOPT
            | GETSOCKNAME
            | GETPEERNAME
            | SENDTO
            | SENDMSG
            | RECVFROM
            | RECVMSG
            | SHUTDOWN
            | LLISTXATTR
            | TIMER_CREATE
            | TIMER_SETTIME
            | TIMER_GETTIME
            | TIMER_GETOVERRUN
            | TIMER_DELETE
            | TIMER_GETTIME64
            | TIMER_SETTIME64
            | MSYNC
            | NANOSLEEP
            | CLOCK_NANOSLEEP
            | ALARM
            | PAUSE
            | SETITIMER
            | GETITIMER
            | SIGRETURN
            | RT_SIGRETURN
            | RT_SIGACTION
            | RT_SIGPENDING
            | RT_SIGSUSPEND
            | PREAD64
            | PWRITE64
            | SIGALTSTACK
            | TKILL
            | READ
            | WRITE
            | OPEN
            | CLOSE
            | UNLINK
            | CHDIR
            | LSEEK
            | GETPID
            | ACCESS
            | KILL
            | MKDIR
            | RMDIR
            | DUP
            | PIPE
            | BRK
            | IOCTL
            | FCNTL
            | DUP2
            | GETPPID
            | GETTIMEOFDAY
            | READLINK
            | MUNMAP
            | UNAME
            | MPROTECT
            | LLSEEK
            | WRITEV
            | SCHED_YIELD
            | MREMAP
            | RT_SIGPROCMASK
            | GETCWD
            | UGETRLIMIT
            | MMAP2
            | GETUID32
            | GETGID32
            | GETEUID32
            | GETEGID32
            | MADVISE
            | GETDENTS64
            | FCNTL64
            | GETTID
            | SET_THREAD_AREA
            | GET_THREAD_AREA
            | EXIT_GROUP
            | SET_TID_ADDRESS
            | CLOCK_GETTIME
            | TGKILL
            | OPENAT
            | UNLINKAT
            | READLINKAT
            | FACCESSAT
            | SET_ROBUST_LIST
            | DUP3
            | PIPE2
            | PRLIMIT64
            | GETRANDOM
            | STATX
            | RSEQ
            | CLOCK_GETTIME64
            | CLOCK_NANOSLEEP_TIME64
            | FUTEX_TIME64
            | FACCESSAT2
            | NEWSELECT
            | POLL
            | EPOLL_CREATE
            | EPOLL_CTL
            | EPOLL_WAIT
            | EPOLL_PWAIT
            | EVENTFD
            | EVENTFD2
            | EPOLL_CREATE1
            | PSELECT6
            | PPOLL
            | PSELECT6_TIME64
            | PPOLL_TIME64
            | EPOLL_PWAIT2
            | LINK
            | MKNOD
            | CHMOD
            | RENAME
            | SYMLINK
            | FCHMOD
            | IPC
            | FLOCK
            | STAT64
            | LSTAT64
            | FSTAT64
            | LCHOWN32
            | FCHOWN32
            | CHOWN32
            | MKDIRAT
            | MKNODAT
            | FCHOWNAT
            | FSTATAT64
            | RENAMEAT
            | LINKAT
            | SYMLINKAT
            | FCHMODAT
            | UTIMENSAT
            | RENAMEAT2
            | SEMGET
            | SEMCTL
            | UTIMENSAT_TIME64
            | SEMTIMEDOP_TIME64
            | FCHMODAT2
            | SIGPROCMASK
            | MEMFD_CREATE
            | CLOCK_GETRES
            | CLOCK_GETRES_TIME64
            | SCHED_SETPARAM
            | SCHED_GETPARAM
            | SCHED_SETSCHEDULER
            | SCHED_GETSCHEDULER
            | SCHED_GET_PRIORITY_MAX
            | SCHED_GET_PRIORITY_MIN
            | SCHED_RR_GET_INTERVAL
            | SCHED_RR_GET_INTERVAL_TIME64
    )
}

/// Handles the i386 syscall the guest just made with `int 0x80` and
/// stores the result in eax.
pub fn dispatch(
    cpu: &mut Cpu,
    mem: &AddressSpace,
    process: &Process,
    thread: &mut Thread,
) -> Outcome {
    let reg = |n: usize| cpu.regs[n] & 0xffff_ffff;
    let number = reg(RAX);
    let a = [reg(RBX), reg(RCX), reg(RDX), reg(RSI), reg(RDI), reg(RBP)];
    // A signed 32-bit argument (an offset, a descriptor that may be
    // AT_FDCWD), sign-extended for the host.
    let signed = |v: u64| v as u32 as i32 as i64 as u64;
    let root = &process.root;
    let result: SysResult = match number {
        nr::EXIT => return Outcome::ExitThread(a[0] as i32),
        // i386 argument order (CLONE_BACKWARDS): flags, stack, parent_tid,
        // tls, child_tid; the TLS is a `user_desc`.
        nr::CLONE => {
            let tls = if a[0] & CLONE_SETTLS != 0 {
                match clone_tls(mem, a[3]) {
                    Ok(tls) => Some(tls),
                    Err(e) => {
                        cpu.regs[RAX] = e as u64 & 0xffff_ffff;
                        return Outcome::Continue;
                    }
                }
            } else {
                None
            };
            return clone_or_fork(cpu, mem, process, thread, [a[0], a[1], a[2], a[4]], tls);
        }
        nr::FORK | nr::VFORK => {
            let flags = if number == nr::FORK {
                crate::process::FORK_FLAGS
            } else {
                crate::process::VFORK_FLAGS
            };
            return clone_or_fork(cpu, mem, process, thread, [flags, 0, 0, 0], None);
        }
        // glibc falls back to clone, as on x86_64.
        nr::CLONE3 => Err(err(libc::ENOSYS)),
        nr::EXECVE => crate::process::execve(mem, process, a[0], a[1], a[2], 4),
        nr::WAIT4 => crate::process::wait4(mem, signed(a[0]), a[1], a[2], a[3], true),
        nr::WAITPID => crate::process::wait4(mem, signed(a[0]), a[1], a[2], 0, true),
        nr::FTRUNCATE => host(libc::SYS_ftruncate, &[a[0], signed(a[1])]),
        nr::FTRUNCATE64 => host(libc::SYS_ftruncate, &[a[0], a[1] | a[2] << 32]),
        nr::EXIT_GROUP => return Outcome::Exit(a[0] as i32),
        nr::READ => read(mem, a[0], a[1], a[2]),
        // The 64-bit offset in two registers, low half first.
        nr::PREAD64 => pread64(mem, a[0], a[1], a[2], a[3] | a[4] << 32),
        nr::PWRITE64 => pwrite64(mem, a[0], a[1], a[2], a[3] | a[4] << 32),
        nr::WRITE => Ok(write(mem, a[0], a[1], a[2])),
        nr::WRITEV => writev(mem, a[0], a[1], a[2], 4),
        nr::OPEN => openat(mem, process, AT_FDCWD, a[0], a[1], a[2]),
        nr::OPENAT => openat(mem, process, signed(a[0]), a[1], a[2], a[3]),
        nr::CLOSE => host(libc::SYS_close, &[a[0]]),
        nr::DUP => host(libc::SYS_dup, &[a[0]]),
        nr::DUP2 => super::dup2(a[0], a[1]),
        nr::DUP3 => host(libc::SYS_dup3, &[a[0], a[1], a[2]]),
        nr::PIPE => pipe2(mem, a[0], 0),
        nr::PIPE2 => pipe2(mem, a[0], a[1]),
        // Record locks: `struct flock64` (24 bytes) with the 64-bit
        // commands and the OFD ones, the old `struct flock` (16 bytes, 32-bit
        // offsets) with F_GETLK/F_SETLK/F_SETLKW.
        nr::FCNTL | nr::FCNTL64 if lock_layout(a[1]).is_some() => {
            record_lock32(mem, a[0], a[1], a[2])
        }
        nr::FCNTL | nr::FCNTL64 => match fcntl(a[0], a[1], a[2]) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("i386 fcntl command {}", a[1])),
        },
        nr::LSEEK => lseek(a[0], signed(a[1]), a[2]),
        nr::LLSEEK => llseek(mem, a[0], a[1], a[2], a[3], a[4]),
        nr::IOCTL => match ioctl(mem, a[0], a[1], a[2]) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("i386 ioctl request {:#x}", a[1])),
        },
        nr::GETDENTS64 => getdents64(mem, a[0], a[1], a[2]),
        nr::GETCWD => getcwd(mem, a[0], a[1]),
        nr::CHDIR => chdir(mem, root, a[0]),
        nr::ACCESS => faccessat(mem, root, AT_FDCWD, a[0], a[1], 0),
        nr::FACCESSAT => faccessat(mem, root, signed(a[0]), a[1], a[2], 0),
        nr::FACCESSAT2 => faccessat(mem, root, signed(a[0]), a[1], a[2], a[3]),
        nr::UNLINK => unlinkat(mem, root, AT_FDCWD, a[0], 0),
        nr::UNLINKAT => unlinkat(mem, root, signed(a[0]), a[1], a[2]),
        nr::MKDIR => files::mkdirat(mem, root, AT_FDCWD, a[0], a[1]),
        nr::RMDIR => unlinkat(mem, root, AT_FDCWD, a[0], libc::AT_REMOVEDIR as u64),
        nr::READLINK => readlinkat(mem, process, AT_FDCWD, a[0], a[1], a[2]),
        nr::READLINKAT => readlinkat(mem, process, signed(a[0]), a[1], a[2], a[3]),
        // `struct statx` has fixed-size fields: the same on i386.
        nr::STATX => statx(mem, root, signed(a[0]), a[1], a[2], a[3], a[4]),
        nr::BRK => Ok(brk(mem, process, a[0])),
        nr::MMAP2 => match a[5].checked_mul(MMAP2_UNIT) {
            Some(offset) => match mmap(mem, a[0], a[1], a[2], a[3], signed(a[4]), offset) {
                Ok(result) => result,
                Err(what) => return Outcome::Unsupported(format!("mmap2: {what}")),
            },
            None => Err(err(libc::EINVAL)),
        },
        nr::MUNMAP => munmap(mem, a[0], a[1]),
        nr::MPROTECT => match mprotect(mem, a[0], a[1], a[2]) {
            Ok(result) => result,
            Err(what) => return Outcome::Unsupported(what.to_string()),
        },
        nr::MREMAP => match mremap(mem, a[0], a[1], a[2], a[3]) {
            Ok(result) => result,
            Err(what) => return Outcome::Unsupported(format!("mremap: {what}")),
        },
        nr::MADVISE => madvise(mem, a[0], a[1], a[2]),
        nr::UNAME => uname(mem, a[0]),
        nr::GETPID => host(libc::SYS_getpid, &[]),
        nr::GETPPID => host(libc::SYS_getppid, &[]),
        nr::GETTID => host(libc::SYS_gettid, &[]),
        nr::GETUID32 => host(libc::SYS_getuid, &[]),
        nr::GETGID32 => host(libc::SYS_getgid, &[]),
        nr::GETEUID32 => host(libc::SYS_geteuid, &[]),
        nr::GETEGID32 => host(libc::SYS_getegid, &[]),
        nr::KILL => kill(signed(a[0]), a[1]),
        nr::TGKILL => tgkill(a[0], a[1], a[2]),
        nr::TKILL => tgkill(own_pid(), a[0], a[1]),
        nr::ALARM => events::alarm(a[0]),
        nr::SETITIMER => itimer(mem, a[0], a[1], a[2]),
        nr::GETITIMER => itimer(mem, a[0], 0, a[1]),
        // SAFETY: pause has no preconditions.
        nr::PAUSE => Err(if unsafe { libc::pause() } < 0 {
            host_errno()
        } else {
            err(libc::EINTR)
        }),
        nr::RT_SIGACTION => rt_sigaction(mem, thread, a[0], a[1], a[2], a[3]),
        nr::RT_SIGPENDING => rt_sigpending(mem, thread, a[0], a[1]),
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
        nr::SIGALTSTACK => sigaltstack(mem, thread, reg(RSP), a[0], a[1]),
        // Both restore every register, eax included.
        nr::SIGRETURN | nr::RT_SIGRETURN => {
            return match thread
                .signals
                .sigreturn32(cpu, mem, number == nr::RT_SIGRETURN)
            {
                Ok(()) => Outcome::Continue,
                Err(why) => Outcome::Fatal(libc::SIGSEGV, why),
            };
        }
        nr::SCHED_YIELD => host(libc::SYS_sched_yield, &[]),
        nr::SET_TID_ADDRESS => {
            thread.clear_child_tid = a[0];
            host(libc::SYS_gettid, &[])
        }
        // As for x86_64: glibc works without robust futex lists and rseq.
        nr::SET_ROBUST_LIST | nr::RSEQ => Err(err(libc::ENOSYS)),
        nr::SET_THREAD_AREA => set_thread_area(cpu, mem, a[0]),
        nr::GET_THREAD_AREA => get_thread_area(cpu, mem, a[0]),
        // sigset_t is 8 bytes on both.
        nr::RT_SIGPROCMASK => rt_sigprocmask(mem, thread, a[0], a[1], a[2], a[3]),
        nr::SIGPROCMASK => sigprocmask(mem, thread, a[0], a[1], a[2]),
        nr::MEMFD_CREATE => super::memfd_create(mem, a[0], a[1]),
        nr::CLOCK_GETRES_TIME64 => super::clock_getres(mem, a[0], a[1]),
        nr::CLOCK_GETRES => clock_getres32(mem, a[0], a[1]),
        nr::SCHED_SETPARAM => super::sched_param(mem, super::nr::SCHED_SETPARAM, a[0], a[1], 0),
        nr::SCHED_GETPARAM => super::sched_param(mem, super::nr::SCHED_GETPARAM, a[0], a[1], 0),
        nr::SCHED_SETSCHEDULER => {
            super::sched_param(mem, super::nr::SCHED_SETSCHEDULER, a[0], a[1], a[2])
        }
        nr::SCHED_GETSCHEDULER => host(libc::SYS_sched_getscheduler, &[a[0]]),
        nr::SCHED_GET_PRIORITY_MAX => host(libc::SYS_sched_get_priority_max, &[a[0]]),
        nr::SCHED_GET_PRIORITY_MIN => host(libc::SYS_sched_get_priority_min, &[a[0]]),
        nr::SCHED_RR_GET_INTERVAL | nr::SCHED_RR_GET_INTERVAL_TIME64 => {
            super::sched_rr_get_interval(a[0]).and_then(|ts| {
                if number == nr::SCHED_RR_GET_INTERVAL {
                    let wide = [
                        i64::from_le_bytes(ts[..8].try_into().unwrap()),
                        i64::from_le_bytes(ts[8..].try_into().unwrap()),
                    ];
                    copy_out(mem, a[1], &narrow_pair(wide)?)?;
                } else {
                    copy_out(mem, a[1], &ts)?;
                }
                Ok(0)
            })
        }
        nr::UGETRLIMIT => ugetrlimit(mem, a[0], a[1]),
        // `struct rlimit64` is two u64 on both.
        nr::PRLIMIT64 => prlimit64(mem, a[0], a[1], a[2], a[3]),
        nr::GETRANDOM => getrandom(mem, a[0], a[1], a[2]),
        // The time64 calls take the 64-bit `struct timespec` of x86_64.
        nr::CLOCK_GETTIME64 => clock_gettime(mem, a[0], a[1]),
        nr::CLOCK_NANOSLEEP_TIME64 => clock_nanosleep(mem, a[0], a[1], a[2], a[3]),
        nr::FUTEX | nr::FUTEX_TIME64 => {
            let timespec32 = number == nr::FUTEX;
            match futex(mem, a[0], a[1], a[2], a[3], a[4], a[5], timespec32) {
                Some(result) => result,
                None => return Outcome::Unsupported(format!("futex operation {}", a[1])),
            }
        }
        nr::CLOCK_GETTIME => clock_gettime32(mem, a[0], a[1]),
        nr::CLOCK_NANOSLEEP => clock_nanosleep32(mem, a[0], a[1], a[2], a[3]),
        nr::NANOSLEEP => clock_nanosleep32(mem, libc::CLOCK_REALTIME as u64, 0, a[0], a[1]),
        nr::MSYNC => super::msync(mem, a[0], a[1], a[2]),
        // Process groups, sessions, priorities, the file mode mask: only
        // integers, as for x86_64.
        nr::SETPGID => host(libc::SYS_setpgid, &[a[0], a[1]]),
        nr::GETPGRP => host(libc::SYS_getpgid, &[0]),
        nr::SETSID => host(libc::SYS_setsid, &[]),
        nr::GETPGID => host(libc::SYS_getpgid, &[a[0]]),
        nr::GETSID => host(libc::SYS_getsid, &[a[0]]),
        nr::GETPRIORITY => host(libc::SYS_getpriority, &[a[0], a[1]]),
        nr::SETPRIORITY => host(libc::SYS_setpriority, &[a[0], a[1], signed(a[2])]),
        nr::UMASK => host(libc::SYS_umask, &[a[0] & 0o777]),
        nr::FCHDIR => host(libc::SYS_fchdir, &[a[0]]),
        nr::FSYNC => host(libc::SYS_fsync, &[a[0]]),
        nr::FDATASYNC => host(libc::SYS_fdatasync, &[a[0]]),
        nr::PRCTL => match super::prctl(mem, a[0], a[1], [a[2], a[3], a[4]]) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("prctl option {}", a[0])),
        },
        nr::SCHED_GETAFFINITY => sched_getaffinity(mem, a[0], a[1], a[2]),
        nr::SYSINFO => sysinfo(mem, a[0]),
        // Domains, types and SOCK_NONBLOCK/SOCK_CLOEXEC match on both, as
        // `struct sockaddr`.
        nr::SOCKETCALL => match socketcall(mem, a[0], a[1]) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("i386 socketcall {}", a[0])),
        },
        nr::SOCKET..=nr::SHUTDOWN => socket_call(mem, number, a),
        nr::LLISTXATTR => super::llistxattr(mem, root, a[0], a[1], a[2]),
        nr::TIMER_CREATE => timer_create(mem, a[0], a[1], a[2]),
        // The time64 calls take the x86_64 `struct itimerspec`.
        nr::TIMER_SETTIME64 => super::timer_settime(mem, a[0], a[1], a[2], a[3]),
        nr::TIMER_GETTIME64 => super::timer_gettime(mem, a[0], a[1]),
        nr::TIMER_SETTIME => timer_settime32(mem, a[0], a[1], Some(a[2]), a[3]),
        nr::TIMER_GETTIME => timer_settime32(mem, a[0], 0, None, a[1]),
        nr::TIMER_GETOVERRUN => host(libc::SYS_timer_getoverrun, &[a[0]]),
        nr::TIMER_DELETE => host(libc::SYS_timer_delete, &[a[0]]),
        nr::STATFS64 => statfs64(mem, Some((root, a[0])), 0, a[1], a[2]),
        nr::FSTATFS64 => statfs64(mem, None, a[0], a[1], a[2]),
        // The offset and the length in two registers each, low half first;
        // POSIX_FADV_* match on both.
        nr::FADVISE64_64 => host(
            libc::SYS_fadvise64,
            &[a[0], a[1] | a[2] << 32, a[3] | a[4] << 32, a[5]],
        ),
        nr::GETTIMEOFDAY => gettimeofday32(mem, a[0], a[1]),
        // The waiting calls: fd_set is an array of 32-bit longs, and the
        // timeval and timespec have 32-bit fields except in the time64
        // calls; pollfd and epoll_event (12 bytes, unpadded since a u64 is
        // 4-aligned on i386) are the x86_64 layouts.
        nr::NEWSELECT => events::select(
            mem,
            Word::W32,
            a[0],
            a[1],
            a[2],
            a[3],
            a[4],
            events::Timeout::Timeval(Word::W32),
        ),
        nr::PSELECT6 | nr::PSELECT6_TIME64 => {
            let time = if number == nr::PSELECT6 {
                Word::W32
            } else {
                Word::W64
            };
            match events::pselect6(mem, Word::W32, time, a) {
                Some(result) => result,
                None => return Outcome::Unsupported("pselect6 with a signal mask".to_string()),
            }
        }
        nr::POLL => events::poll(mem, a[0], a[1], a[2]),
        nr::PPOLL | nr::PPOLL_TIME64 => {
            let time = if number == nr::PPOLL {
                Word::W32
            } else {
                Word::W64
            };
            super::ppoll(mem, thread, time, a)
        }
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
        // Files: the x86_64 implementations with the i386 numbers; uids and
        // gids are 32-bit (the `*32` calls), as on x86_64.
        nr::LINK => files::linkat(mem, root, AT_FDCWD, a[0], AT_FDCWD, a[1], 0),
        nr::LINKAT => files::linkat(mem, root, signed(a[0]), a[1], signed(a[2]), a[3], a[4]),
        nr::MKNOD => files::mknodat(mem, root, AT_FDCWD, a[0], a[1], a[2]),
        nr::MKNODAT => files::mknodat(mem, root, signed(a[0]), a[1], a[2], a[3]),
        nr::MKDIRAT => files::mkdirat(mem, root, signed(a[0]), a[1], a[2]),
        nr::CHMOD => files::fchmodat(mem, root, AT_FDCWD, a[0], a[1], 0),
        nr::FCHMODAT => files::fchmodat(mem, root, signed(a[0]), a[1], a[2], 0),
        nr::FCHMODAT2 => files::fchmodat(mem, root, signed(a[0]), a[1], a[2], a[3]),
        nr::FCHMOD => host(libc::SYS_fchmod, &[a[0], a[1]]),
        nr::RENAME => files::renameat2(mem, root, AT_FDCWD, a[0], AT_FDCWD, a[1], 0),
        nr::RENAMEAT => files::renameat2(mem, root, signed(a[0]), a[1], signed(a[2]), a[3], 0),
        nr::RENAMEAT2 => files::renameat2(mem, root, signed(a[0]), a[1], signed(a[2]), a[3], a[4]),
        nr::SYMLINK => files::symlinkat(mem, root, a[0], AT_FDCWD, a[1]),
        nr::SYMLINKAT => files::symlinkat(mem, root, a[0], signed(a[1]), a[2]),
        nr::CHOWN32 => files::fchownat(mem, root, AT_FDCWD, a[0], a[1], a[2], 0),
        nr::LCHOWN32 => files::fchownat(
            mem,
            root,
            AT_FDCWD,
            a[0],
            a[1],
            a[2],
            libc::AT_SYMLINK_NOFOLLOW as u64,
        ),
        nr::FCHOWNAT => files::fchownat(mem, root, signed(a[0]), a[1], a[2], a[3], a[4]),
        nr::FCHOWN32 => host(libc::SYS_fchown, &[a[0], a[1], a[2]]),
        nr::UTIMENSAT => files::utimensat32(mem, root, signed(a[0]), a[1], a[2], a[3]),
        nr::UTIMENSAT_TIME64 => files::utimensat(mem, root, signed(a[0]), a[1], a[2], a[3]),
        nr::STAT64 => stat64(mem, super::host_fstatat(mem, root, AT_FDCWD, a[0], 0), a[1]),
        nr::LSTAT64 => stat64(
            mem,
            super::host_fstatat(mem, root, AT_FDCWD, a[0], libc::AT_SYMLINK_NOFOLLOW as u64),
            a[1],
        ),
        nr::FSTAT64 => stat64(mem, super::host_fstat(a[0]), a[1]),
        nr::FSTATAT64 => stat64(
            mem,
            super::host_fstatat(mem, root, signed(a[0]), a[1], a[3]),
            a[2],
        ),
        nr::FLOCK => host(libc::SYS_flock, &[a[0], a[1]]),
        // System V semaphores: `ipc` (glibc 2.41 i386 uses it) and the
        // direct calls of Linux 5.1.
        nr::IPC => match crate::ipc::ipc(mem, a) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("i386 ipc call {:#x}", a[0])),
        },
        nr::SEMGET => crate::ipc::semget(a[0], a[1], a[2]),
        nr::SEMCTL => match crate::ipc::semctl(mem, a[0], a[1], a[2], a[3]) {
            Some(result) => result,
            None => return Outcome::Unsupported(format!("semctl command {}", a[2])),
        },
        nr::SEMTIMEDOP_TIME64 => crate::ipc::semtimedop(mem, a[0], a[1], a[2], a[3], Word::W64),
        n => return Outcome::Unsupported(format!("i386 syscall {n}")),
    };
    // eax holds the low 32 bits; the upper half stays zero in
    // compatibility mode.
    cpu.regs[RAX] = result.unwrap_or_else(|e| e) as u64 & 0xffff_ffff;
    Outcome::Continue
}

/// Whether Linux restarts i386 syscall `number`, interrupted by a signal
/// whose handler has SA_RESTART: the i386 numbers of the x86_64 list
/// (`syscall::restartable`).
pub fn restartable(number: u64) -> bool {
    matches!(
        number,
        nr::READ
            | nr::WRITE
            | nr::PREAD64
            | nr::PWRITE64
            | nr::OPEN
            | nr::OPENAT
            | nr::IOCTL
            | nr::ACCEPT4
            | nr::RECVFROM
            | nr::RECVMSG
            | nr::SENDTO
            | nr::SENDMSG
            | nr::FLOCK
            | nr::FCNTL
            | nr::FCNTL64
            // ⚠️ Linux decides by the call inside (accept, recv, …).
            | nr::SOCKETCALL
    )
}

/// The i386 record-lock commands: the host command and whether the guest's
/// structure is `struct flock64` (24 bytes: type, whence, then start@4,
/// len@12, pid@20) or the old `struct flock` (16 bytes: start@4, len@8,
/// pid@12, 32-bit offsets). ✅ Measured with `sizeof`/`offsetof` under
/// i686 glibc, 2026-10-14.
fn lock_layout(cmd: u64) -> Option<(u64, bool)> {
    use super::{F_GETLK, F_OFD_GETLK, F_OFD_SETLK, F_OFD_SETLKW, F_SETLK, F_SETLKW};
    Some(match cmd {
        F_GETLK | F_SETLK | F_SETLKW => (cmd, false),
        12 => (F_GETLK, true),
        13 => (F_SETLK, true),
        14 => (F_SETLKW, true),
        F_OFD_GETLK | F_OFD_SETLK | F_OFD_SETLKW => (cmd, true),
        _ => return None,
    })
}

fn record_lock32(mem: &AddressSpace, fd: u64, cmd: u64, arg: u64) -> SysResult {
    let (host_cmd, wide) = lock_layout(cmd).expect("checked by dispatch");
    let mut host = [0u8; 32];
    if wide {
        let raw = copy_in::<24>(mem, arg)?;
        host[..4].copy_from_slice(&raw[..4]);
        host[8..24].copy_from_slice(&raw[4..20]);
        host[24..28].copy_from_slice(&raw[20..24]);
    } else {
        let raw = copy_in::<16>(mem, arg)?;
        host[..4].copy_from_slice(&raw[..4]);
        for (to, from) in [(8, 4), (16, 8)] {
            let value = i64::from(i32::from_le_bytes(raw[from..from + 4].try_into().unwrap()));
            host[to..to + 8].copy_from_slice(&value.to_le_bytes());
        }
        host[24..28].copy_from_slice(&raw[12..16]);
    }
    let (result, out) = super::record_lock(fd, host_cmd, host)?;
    if matches!(host_cmd, super::F_GETLK | super::F_OFD_GETLK) {
        if wide {
            let mut raw = [0u8; 24];
            raw[..4].copy_from_slice(&out[..4]);
            raw[4..20].copy_from_slice(&out[8..24]);
            raw[20..24].copy_from_slice(&out[24..28]);
            copy_out(mem, arg, &raw)?;
        } else {
            let start = i64::from_le_bytes(out[8..16].try_into().unwrap());
            let len = i64::from_le_bytes(out[16..24].try_into().unwrap());
            // A lock beyond 32 bits does not fit the old structure.
            let (Ok(start), Ok(len)) = (i32::try_from(start), i32::try_from(len)) else {
                return Err(err(libc::EOVERFLOW));
            };
            let mut raw = [0u8; 16];
            raw[..4].copy_from_slice(&out[..4]);
            raw[4..8].copy_from_slice(&start.to_le_bytes());
            raw[8..12].copy_from_slice(&len.to_le_bytes());
            raw[12..16].copy_from_slice(&out[24..28]);
            copy_out(mem, arg, &raw)?;
        }
    }
    Ok(result)
}

/// The old sigprocmask(2) of i386: a 32-bit `old_sigset_t`, the first 32
/// signals. As in Linux, SIG_SETMASK keeps the blocked signals above 32,
/// and the new set is read before `how` is checked.
fn sigprocmask(mem: &AddressSpace, thread: &mut Thread, how: u64, set: u64, old: u64) -> SysResult {
    let current = thread.signals.mask();
    if set != 0 {
        let new = u64::from(u32::from_le_bytes(copy_in::<4>(mem, set)?));
        let mask = match how {
            super::SIG_BLOCK => current | new,
            super::SIG_UNBLOCK => current & !new,
            super::SIG_SETMASK => current & !0xffff_ffff | new,
            _ => return Err(err(libc::EINVAL)),
        };
        thread.signals.set_mask(mask);
    }
    if old != 0 {
        copy_out(mem, old, &(current as u32).to_le_bytes())?;
    }
    Ok(0)
}

/// The i386 `struct stat64` (96 bytes: ✅ measured with `sizeof` and
/// `offsetof` of the kernel's header under i686 gcc, 2026-10-14). As
/// Linux's `cp_stat64` does, the inode is also written truncated to 32
/// bits, and the times to 32 bits.
fn stat64(mem: &AddressSpace, st: Result<libc::stat, i64>, buf: u64) -> SysResult {
    let st = st?;
    let mut out = [0u8; 96];
    let mut put = |offset: usize, bytes: &[u8]| {
        out[offset..offset + bytes.len()].copy_from_slice(bytes);
    };
    put(0, &st.st_dev.to_le_bytes());
    put(12, &(st.st_ino as u32).to_le_bytes());
    put(16, &st.st_mode.to_le_bytes());
    put(20, &st.st_nlink.to_le_bytes());
    put(24, &st.st_uid.to_le_bytes());
    put(28, &st.st_gid.to_le_bytes());
    put(32, &st.st_rdev.to_le_bytes());
    put(44, &st.st_size.to_le_bytes());
    put(52, &(st.st_blksize as u32).to_le_bytes());
    put(56, &st.st_blocks.to_le_bytes());
    for (offset, sec, nsec) in [
        (64, st.st_atime, st.st_atime_nsec),
        (72, st.st_mtime, st.st_mtime_nsec),
        (80, st.st_ctime, st.st_ctime_nsec),
    ] {
        put(offset, &(sec as u32).to_le_bytes());
        put(offset + 4, &(nsec as u32).to_le_bytes());
    }
    put(88, &st.st_ino.to_le_bytes());
    copy_out(mem, buf, &out)?;
    Ok(0)
}

/// clone's flag for a new TLS.
const CLONE_SETTLS: u64 = 0x8_0000;

/// `struct sigaction` of i386: handler, flags, restorer (4 bytes each) and
/// the 8-byte mask.
const SIGACTION32_SIZE: usize = 20;

/// rt_sigaction(2) with the i386 `struct sigaction`, kept in the x86_64
/// layout that `Signals` uses.
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
        let raw = copy_in::<SIGACTION32_SIZE>(mem, act)?;
        let mut wide = [0u8; SIGACTION_SIZE];
        for i in 0..3 {
            wide[8 * i..8 * i + 4].copy_from_slice(&raw[4 * i..4 * i + 4]);
        }
        wide[24..32].copy_from_slice(&raw[12..20]);
        Some(wide)
    } else {
        None
    };
    if oldact != 0 {
        let wide = thread.signals.action(signal);
        let mut raw = [0u8; SIGACTION32_SIZE];
        for i in 0..3 {
            raw[4 * i..4 * i + 4].copy_from_slice(&wide[8 * i..8 * i + 4]);
        }
        raw[12..20].copy_from_slice(&wide[24..32]);
        copy_out(mem, oldact, &raw)?;
    }
    if let Some(new) = new {
        thread.signals.set_action(signal, new);
    }
    Ok(0)
}

/// sigaltstack(2) with the i386 `stack_t`: pointer, flags, size (4 bytes
/// each).
fn sigaltstack(mem: &AddressSpace, thread: &mut Thread, esp: u64, new: u64, old: u64) -> SysResult {
    let new = if new != 0 {
        let raw = copy_in::<12>(mem, new)?;
        let word = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().expect("4 bytes"));
        Some((u64::from(word(0)), word(4), u64::from(word(8))))
    } else {
        None
    };
    let (sp, flags, size) = thread.signals.alt_stack(new, esp).map_err(err)?;
    if old != 0 {
        let mut raw = [0u8; 12];
        raw[..4].copy_from_slice(&(sp as u32).to_le_bytes());
        raw[4..8].copy_from_slice(&flags.to_le_bytes());
        raw[8..12].copy_from_slice(&(size as u32).to_le_bytes());
        copy_out(mem, old, &raw)?;
    }
    Ok(0)
}

/// sched_getaffinity(2) of an i386 guest: the length is a multiple of 4
/// (32-bit `long`s), the host's of 8; the result is at most `len`.
fn sched_getaffinity(mem: &AddressSpace, pid: u64, len: u64, mask: u64) -> SysResult {
    if !len.is_multiple_of(4) || len == 0 {
        return Err(err(libc::EINVAL));
    }
    let mut bytes = vec![0u8; (len.next_multiple_of(8) as usize).min(64 * 1024)];
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
        return Err(super::host_errno());
    }
    let n = (n as u64).min(len) as usize;
    copy_out(mem, mask, &bytes[..n])?;
    Ok(n as i64)
}

/// sysinfo(2) with the i386 `struct sysinfo` (64 bytes, 32-bit `long`s).
/// As Linux's `compat_sysinfo`, memory sizes that do not fit 32 bits are
/// scaled down, `mem_unit` growing up to the page size (4096).
fn sysinfo(mem: &AddressSpace, buf: u64) -> SysResult {
    let mut wide = [0u8; 112];
    // SAFETY: wide is as large as the host's struct sysinfo.
    if unsafe { libc::syscall(libc::SYS_sysinfo, wide.as_mut_ptr()) } < 0 {
        return Err(super::host_errno());
    }
    let long = |at: usize| u64::from_le_bytes(wide[at..at + 8].try_into().expect("8 bytes"));
    // totalram, freeram, sharedram, bufferram, totalswap, freeswap,
    // totalhigh, freehigh.
    let mut sizes = [32, 40, 48, 56, 64, 72, 88, 96].map(long);
    let mut unit = u32::from_le_bytes(wide[104..108].try_into().expect("4 bytes"));
    if sizes[0] >> 32 != 0 || sizes[4] >> 32 != 0 {
        let mut shift = 0;
        while unit < 4096 {
            unit <<= 1;
            shift += 1;
        }
        sizes = sizes.map(|v| v >> shift);
    }
    let mut out = [0u8; 64];
    let mut put =
        |at: usize, value: u64| out[at..at + 4].copy_from_slice(&(value as u32).to_le_bytes());
    put(0, long(0));
    for i in 0..3 {
        put(4 + 4 * i, long(8 + 8 * i));
    }
    for (i, &size) in sizes[..6].iter().enumerate() {
        put(16 + 4 * i, size);
    }
    put(44, sizes[6]);
    put(48, sizes[7]);
    put(52, u64::from(unit));
    // procs (16 bits), then padding.
    out[40..42].copy_from_slice(&wide[80..82]);
    copy_out(mem, buf, &out)?;
    Ok(0)
}

/// timer_create(2) with the i386 `struct sigevent`: a 4-byte `sigev_value`
/// before the signal, the notification and the thread id (64 bytes in
/// all, as the host's, whose value is 8 bytes).
fn timer_create(mem: &AddressSpace, clock: u64, event: u64, id: u64) -> SysResult {
    let event = if event != 0 {
        let narrow = copy_in::<64>(mem, event)?;
        let mut wide = [0u8; 64];
        wide[0..4].copy_from_slice(&narrow[0..4]);
        wide[8..12].copy_from_slice(&narrow[4..8]);
        wide[12..16].copy_from_slice(&narrow[8..12]);
        wide[16..20].copy_from_slice(&narrow[12..16]);
        Some(wide)
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
        return Err(super::host_errno());
    }
    copy_out(mem, id, &timer.to_le_bytes())?;
    Ok(0)
}

/// timer_settime(2) (`new` given) or timer_gettime(2) with the 32-bit
/// `struct itimerspec`: interval and value, each two 32-bit `long`s.
fn timer_settime32(
    mem: &AddressSpace,
    timer: u64,
    flags: u64,
    new: Option<u64>,
    old: u64,
) -> SysResult {
    let mut previous = [0i64; 4];
    // SAFETY: the buffers are host `struct itimerspec`s (32 bytes).
    let result = unsafe {
        match new {
            Some(new) => {
                let narrow = copy_in::<16>(mem, new)?;
                let value: [i64; 4] = std::array::from_fn(|i| {
                    i64::from(i32::from_le_bytes(
                        narrow[4 * i..4 * i + 4].try_into().expect("4"),
                    ))
                });
                libc::syscall(
                    libc::SYS_timer_settime,
                    timer as i32,
                    flags as i32,
                    value.as_ptr(),
                    previous.as_mut_ptr(),
                )
            }
            None => libc::syscall(libc::SYS_timer_gettime, timer as i32, previous.as_mut_ptr()),
        }
    };
    if result < 0 {
        return Err(super::host_errno());
    }
    if old != 0 {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&narrow_pair([previous[0], previous[1]])?);
        out[8..].copy_from_slice(&narrow_pair([previous[2], previous[3]])?);
        copy_out(mem, old, &out)?;
    }
    Ok(0)
}

/// The socket calls with the direct i386 numbers (359–373): the
/// arguments in registers, the structures in the i386 layout.
fn socket_call(mem: &AddressSpace, number: u64, a: [u64; 6]) -> SysResult {
    use crate::net;
    match number {
        nr::SOCKET => net::socket(a[0], a[1], a[2]),
        nr::SOCKETPAIR => net::socketpair(mem, a[0], a[1], a[2], a[3]),
        nr::BIND => net::bind(mem, a[0], a[1], a[2]),
        nr::CONNECT => net::connect(mem, a[0], a[1], a[2]),
        nr::LISTEN => net::listen(a[0], a[1]),
        nr::ACCEPT4 => net::accept4(mem, a[0], a[1], a[2], a[3]),
        nr::GETSOCKOPT => net::getsockopt(mem, a[0], a[1], a[2], a[3], a[4], net::Word::W32),
        nr::SETSOCKOPT => net::setsockopt(mem, a[0], a[1], a[2], a[3], a[4], net::Word::W32),
        nr::GETSOCKNAME => net::name(mem, false, a[0], a[1], a[2]),
        nr::GETPEERNAME => net::name(mem, true, a[0], a[1], a[2]),
        nr::SENDTO => net::sendto(mem, a[0], a[1], a[2], a[3], a[4], a[5]),
        nr::SENDMSG => net::sendmsg(mem, a[0], a[1], a[2], net::Word::W32),
        nr::RECVFROM => net::recvfrom(mem, a[0], a[1], a[2], a[3], a[4], a[5]),
        nr::RECVMSG => net::recvmsg(mem, a[0], a[1], a[2], net::Word::W32),
        _ => net::shutdown(a[0], a[1]),
    }
}

/// socketcall(2): the i386 multiplexer of the socket calls (linux/net.h:
/// 1 socket … 18 accept4), whose arguments are 32-bit words at `args`.
/// None for recvmmsg and sendmmsg (19, 20), not handled yet.
fn socketcall(mem: &AddressSpace, call: u64, args: u64) -> Option<SysResult> {
    // The number of arguments, and the direct syscall of each call.
    let (count, number) = match call {
        1 => (3, nr::SOCKET),
        2 => (3, nr::BIND),
        3 => (3, nr::CONNECT),
        4 => (2, nr::LISTEN),
        5 | 18 => (if call == 5 { 3 } else { 4 }, nr::ACCEPT4),
        6 => (3, nr::GETSOCKNAME),
        7 => (3, nr::GETPEERNAME),
        8 => (4, nr::SOCKETPAIR),
        // send and recv: sendto and recvfrom without an address.
        9 | 11 => (if call == 9 { 4 } else { 6 }, nr::SENDTO),
        10 | 12 => (if call == 10 { 4 } else { 6 }, nr::RECVFROM),
        13 => (2, nr::SHUTDOWN),
        14 => (5, nr::SETSOCKOPT),
        15 => (5, nr::GETSOCKOPT),
        16 => (3, nr::SENDMSG),
        17 => (3, nr::RECVMSG),
        _ => return None,
    };
    let mut words = [0u64; 6];
    for (i, word) in words.iter_mut().take(count).enumerate() {
        match copy_in::<4>(mem, args.wrapping_add(4 * i as u64)) {
            Ok(bytes) => *word = u64::from(u32::from_le_bytes(bytes)),
            Err(e) => return Some(Err(e)),
        }
    }
    Some(socket_call(mem, number, words))
}

/// The i386 `struct statfs64` (84 bytes, 4-byte aligned: ✅ measured with
/// `sizeof` and `offsetof` under i686 glibc).
const STATFS64_SIZE: usize = 84;

/// statfs64(2) (`path`: the root and the path) or fstatfs64(2) (`fd`),
/// with the size of the i386 struct, which the kernel checks (EINVAL).
fn statfs64(
    mem: &AddressSpace,
    path: Option<(&crate::root::GuestRoot, u64)>,
    fd: u64,
    size: u64,
    buf: u64,
) -> SysResult {
    if size != STATFS64_SIZE as u64 {
        return Err(err(libc::EINVAL));
    }
    // The host's `struct statfs`: 64-bit fields, the fsid in two ints.
    let mut wide = [0u64; 15];
    // SAFETY: wide is as large as the host's struct statfs (120 bytes); the
    // path is NUL-terminated.
    let result = unsafe {
        match path {
            Some((root, path)) => {
                let path = root.resolve_c(super::read_path(mem, path)?);
                libc::syscall(libc::SYS_statfs, path.as_ptr(), wide.as_mut_ptr())
            }
            None => libc::syscall(libc::SYS_fstatfs, fd as i32, wide.as_mut_ptr()),
        }
    };
    if result < 0 {
        return Err(super::host_errno());
    }
    let mut out = [0u8; STATFS64_SIZE];
    // f_type, f_bsize: 32 bits; f_blocks .. f_ffree: 64; f_fsid: 8 bytes;
    // f_namelen, f_frsize, f_flags, f_spare[4]: 32.
    out[0..4].copy_from_slice(&(wide[0] as u32).to_le_bytes());
    out[4..8].copy_from_slice(&(wide[1] as u32).to_le_bytes());
    for i in 0..5 {
        out[8 + 8 * i..16 + 8 * i].copy_from_slice(&wide[2 + i].to_le_bytes());
    }
    out[48..56].copy_from_slice(&wide[7].to_le_bytes());
    for (i, field) in wide[8..11].iter().enumerate() {
        out[56 + 4 * i..60 + 4 * i].copy_from_slice(&(*field as u32).to_le_bytes());
    }
    copy_out(mem, buf, &out)?;
    Ok(0)
}

/// lseek(2) with a 32-bit offset: a result beyond 2 GiB is EOVERFLOW, as
/// for a 32-bit `off_t` (the descriptor keeps its new position, as in
/// Linux).
fn lseek(fd: u64, offset: u64, whence: u64) -> SysResult {
    let position = host(libc::SYS_lseek, &[fd, offset, whence])?;
    if position > i64::from(i32::MAX) {
        return Err(err(libc::EOVERFLOW));
    }
    Ok(position)
}

/// _llseek(2): the 64-bit offset in two halves, the result stored at
/// `result` (a 64-bit `loff_t`).
fn llseek(mem: &AddressSpace, fd: u64, high: u64, low: u64, result: u64, whence: u64) -> SysResult {
    let position = host(libc::SYS_lseek, &[fd, high << 32 | low, whence])?;
    copy_out(mem, result, &position.to_le_bytes())?;
    Ok(0)
}

/// ugetrlimit(2): the 32-bit `struct rlimit`, values above 4 GiB - 1
/// shown as infinite (Linux's `COMPAT_RLIM_INFINITY`).
fn ugetrlimit(mem: &AddressSpace, resource: u64, limit: u64) -> SysResult {
    let mut wide = [0u64; 2];
    // SAFETY: wide is a writable `struct rlimit64`; no new limit.
    let result = unsafe {
        libc::syscall(
            libc::SYS_prlimit64,
            0,
            resource as i32,
            std::ptr::null::<u64>(),
            wide.as_mut_ptr(),
        )
    };
    if result < 0 {
        return Err(super::host_errno());
    }
    let mut out = [0u8; 8];
    for (i, value) in wide.into_iter().enumerate() {
        let value = value.min(RLIM32_INFINITY) as u32;
        out[4 * i..4 * i + 4].copy_from_slice(&value.to_le_bytes());
    }
    copy_out(mem, limit, &out)?;
    Ok(0)
}

/// clock_gettime(2) with the 32-bit `struct timespec` (seconds and
/// nanoseconds as 32-bit `long`s).
fn clock_gettime32(mem: &AddressSpace, clock: u64, ts: u64) -> SysResult {
    let mut wide = [0i64; 2];
    // SAFETY: wide is as large as the host's `struct timespec`.
    if unsafe { libc::syscall(libc::SYS_clock_gettime, clock as i32, wide.as_mut_ptr()) } < 0 {
        return Err(super::host_errno());
    }
    copy_out(mem, ts, &narrow_pair(wide)?)?;
    Ok(0)
}

/// clock_getres(2) with a 32-bit `struct timespec`.
fn clock_getres32(mem: &AddressSpace, clock: u64, ts: u64) -> SysResult {
    let mut wide = [0i64; 2];
    // SAFETY: wide is as large as the host's `struct timespec`.
    if unsafe { libc::syscall(libc::SYS_clock_getres, clock as i32, wide.as_mut_ptr()) } < 0 {
        return Err(super::host_errno());
    }
    if ts != 0 {
        copy_out(mem, ts, &narrow_pair(wide)?)?;
    }
    Ok(0)
}

/// clock_nanosleep(2) (and nanosleep(2), on CLOCK_REALTIME without
/// flags) with 32-bit `struct timespec`s: the time left goes back when a
/// signal interrupts the sleep.
fn clock_nanosleep32(
    mem: &AddressSpace,
    clock: u64,
    flags: u64,
    request: u64,
    remain: u64,
) -> SysResult {
    let narrow = copy_in::<8>(mem, request)?;
    let field = |i: usize| {
        i64::from(i32::from_le_bytes(
            narrow[4 * i..4 * i + 4].try_into().expect("4 bytes"),
        ))
    };
    let wanted = [field(0), field(1)];
    let mut left = [0i64; 2];
    // SAFETY: both buffers are host `struct timespec`s.
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
        if errno == err(libc::EINTR) && remain != 0 && flags & 1 == 0 {
            copy_out(mem, remain, &narrow_pair(left)?)?;
        }
        return Err(errno);
    }
    Ok(0)
}

/// gettimeofday(2) with the 32-bit `struct timeval`; the time zone is two
/// ints on both.
fn gettimeofday32(mem: &AddressSpace, tv: u64, tz: u64) -> SysResult {
    let mut time = [0i64; 2];
    let mut zone = [0u8; 8];
    // SAFETY: both are host buffers of the sizes the kernel writes.
    if unsafe { libc::syscall(libc::SYS_gettimeofday, time.as_mut_ptr(), zone.as_mut_ptr()) } < 0 {
        return Err(super::host_errno());
    }
    if tv != 0 {
        copy_out(mem, tv, &narrow_pair(time)?)?;
    }
    if tz != 0 {
        copy_out(mem, tz, &zone)?;
    }
    Ok(0)
}

/// setitimer(2) (`new` set) or getitimer(2) with the i386 `struct
/// itimerval`: interval and value, each a 32-bit `timeval`.
fn itimer(mem: &AddressSpace, which: u64, new: u64, old: u64) -> SysResult {
    let value = if new != 0 {
        let raw = copy_in::<16>(mem, new)?;
        let mut wide = [0i64; 4];
        for (i, slot) in wide.iter_mut().enumerate() {
            *slot = i64::from(i32::from_le_bytes(
                raw[4 * i..4 * i + 4].try_into().expect("4 bytes"),
            ));
        }
        Some(wide)
    } else {
        None
    };
    let mut previous = [0i64; 4];
    // SAFETY: the pointers are null or 32-byte host `itimerval`s.
    let result = unsafe {
        match &value {
            Some(v) => libc::syscall(
                libc::SYS_setitimer,
                which as i32,
                v.as_ptr(),
                previous.as_mut_ptr(),
            ),
            None => libc::syscall(libc::SYS_getitimer, which as i32, previous.as_mut_ptr()),
        }
    };
    if result < 0 {
        return Err(host_errno());
    }
    if old != 0 {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&narrow_pair([previous[0], previous[1]])?);
        out[8..].copy_from_slice(&narrow_pair([previous[2], previous[3]])?);
        copy_out(mem, old, &out)?;
    }
    Ok(0)
}

/// Two 64-bit values as two 32-bit `long`s; EOVERFLOW past 2038.
fn narrow_pair(pair: [i64; 2]) -> Result<[u8; 8], i64> {
    let mut out = [0u8; 8];
    for (i, value) in pair.into_iter().enumerate() {
        let value = i32::try_from(value).map_err(|_| err(libc::EOVERFLOW))?;
        out[4 * i..4 * i + 4].copy_from_slice(&value.to_le_bytes());
    }
    Ok(out)
}

/// set_thread_area(2): sets one of the thread's three TLS entries of the
/// GDT from a `struct user_desc`, as Linux's `do_set_thread_area`. Entry
/// -1 asks for a free one, whose number is written back. A selector in fs
/// or gs naming the entry takes the new base at once, as Linux reloads it.
fn set_thread_area(cpu: &mut Cpu, mem: &AddressSpace, desc: u64) -> SysResult {
    let (number, entry) = user_desc(mem, desc)?;
    let index = if number == u32::MAX {
        let free = cpu
            .segments
            .tls
            .iter()
            .position(Option::is_none)
            .ok_or(err(libc::ESRCH))?;
        let number = TLS_FIRST_ENTRY + free as u32;
        copy_out(mem, desc, &number.to_le_bytes())?;
        free
    } else {
        tls_index(number)?
    };
    cpu.set_tls_entry(index, entry);
    Ok(0)
}

/// The TLS entry index (0–2) of GDT entry `number`, or EINVAL.
fn tls_index(number: u32) -> Result<usize, i64> {
    match number.checked_sub(TLS_FIRST_ENTRY) {
        Some(index) if (index as usize) < TLS_ENTRIES => Ok(index as usize),
        _ => Err(err(libc::EINVAL)),
    }
}

/// Reads a `struct user_desc`: its entry number and the entry it
/// describes, None for an all-zero descriptor or Linux's "empty" one,
/// which clear the entry. EINVAL unless it is a 32-bit data or code
/// segment (Linux's `tls_desc_okay`).
fn user_desc(mem: &AddressSpace, desc: u64) -> Result<(u32, Option<TlsEntry>), i64> {
    let raw = copy_in::<USER_DESC_SIZE>(mem, desc)?;
    let field = |i: usize| u32::from_le_bytes(raw[4 * i..4 * i + 4].try_into().expect("4 bytes"));
    let (number, base, limit, flags) = (field(0), field(1), field(2), field(3) & USER_DESC_FLAGS);
    if base == 0 && limit == 0 && (flags == 0 || flags == USER_DESC_EMPTY_FLAGS) {
        return Ok((number, None));
    }
    if flags & USER_DESC_SEG_32BIT == 0 || (flags & USER_DESC_CONTENTS) >> 1 > 1 {
        return Err(err(libc::EINVAL));
    }
    Ok((number, Some(TlsEntry { base, limit, flags })))
}

/// The TLS of a new thread or process (clone with CLONE_SETTLS): a
/// `user_desc` naming the entry to set, in the child only (Linux's
/// `do_set_thread_area` without allocation: entry -1 is EINVAL).
fn clone_tls(mem: &AddressSpace, desc: u64) -> Result<NewTls, i64> {
    let (number, entry) = user_desc(mem, desc)?;
    Ok(NewTls::Entry(tls_index(number)?, entry))
}

/// get_thread_area(2): the `struct user_desc` of a TLS entry; an unset
/// one reads as Linux's empty descriptor.
fn get_thread_area(cpu: &Cpu, mem: &AddressSpace, desc: u64) -> SysResult {
    let number = u32::from_le_bytes(copy_in::<4>(mem, desc)?);
    let index = tls_index(number)?;
    let entry = cpu.segments.tls[index].unwrap_or(TlsEntry {
        base: 0,
        limit: 0,
        flags: USER_DESC_EMPTY_FLAGS,
    });
    let mut out = [0u8; USER_DESC_SIZE];
    for (i, value) in [number, entry.base, entry.limit, entry.flags]
        .into_iter()
        .enumerate()
    {
        out[4 * i..4 * i + 4].copy_from_slice(&value.to_le_bytes());
    }
    copy_out(mem, desc, &out)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use armor_mem::{GUEST_PAGE_SIZE, GuestMemory, Prot};

    fn low_memory() -> (AddressSpace, u64) {
        let mem = AddressSpace::new();
        mem.set_ceiling(1 << 32);
        let page = mem
            .map_anywhere(GUEST_PAGE_SIZE, Prot::READ | Prot::WRITE)
            .unwrap();
        (mem, page)
    }

    fn user_desc(number: u32, base: u32, limit: u32, flags: u32) -> [u8; USER_DESC_SIZE] {
        let mut raw = [0u8; USER_DESC_SIZE];
        for (i, v) in [number, base, limit, flags].into_iter().enumerate() {
            raw[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
        }
        raw
    }

    /// What i386 glibc does at startup: entry -1, a 32-bit, page-granular,
    /// usable segment; then `mov gs` with the returned entry's selector.
    #[test]
    fn set_thread_area_picks_an_entry_and_gs_follows() {
        let (mem, desc) = low_memory();
        let mut cpu = Cpu::new_compat(0, 0);
        // seg_32bit, limit_in_pages, useable.
        let flags = 1 | 1 << 4 | 1 << 6;
        mem.write(desc, &user_desc(u32::MAX, 0x1234_5000, 0xfffff, flags))
            .unwrap();
        assert_eq!(set_thread_area(&mut cpu, &mem, desc), Ok(0));
        assert_eq!(copy_in::<4>(&mem, desc), Ok(12u32.to_le_bytes()));
        assert_eq!(cpu.segment_base(12 << 3 | 3), Some(0x1234_5000));
        // The next free one is 13.
        mem.write(desc, &user_desc(u32::MAX, 0x2000, 0xfffff, flags))
            .unwrap();
        assert_eq!(set_thread_area(&mut cpu, &mem, desc), Ok(0));
        assert_eq!(copy_in::<4>(&mem, desc), Ok(13u32.to_le_bytes()));
        // gs already holding entry 12 takes its new base.
        cpu.segments.gs = 12 << 3 | 3;
        mem.write(desc, &user_desc(12, 0x5000, 0xfffff, flags))
            .unwrap();
        assert_eq!(set_thread_area(&mut cpu, &mem, desc), Ok(0));
        assert_eq!(cpu.gs_base, 0x5000);
        // Reading it back.
        mem.write(desc, &12u32.to_le_bytes()).unwrap();
        assert_eq!(get_thread_area(&cpu, &mem, desc), Ok(0));
        assert_eq!(
            copy_in::<USER_DESC_SIZE>(&mem, desc),
            Ok(user_desc(12, 0x5000, 0xfffff, flags))
        );
        // Cleared: the empty descriptor, and no base for its selector.
        mem.write(desc, &user_desc(12, 0, 0, USER_DESC_EMPTY_FLAGS))
            .unwrap();
        assert_eq!(set_thread_area(&mut cpu, &mem, desc), Ok(0));
        assert_eq!(cpu.segment_base(12 << 3 | 3), None);
        assert_eq!(cpu.gs_base, 0);
        // Out of range, or a 16-bit segment.
        mem.write(desc, &user_desc(15, 0x5000, 0xfffff, flags))
            .unwrap();
        assert_eq!(
            set_thread_area(&mut cpu, &mem, desc),
            Err(err(libc::EINVAL))
        );
        mem.write(desc, &user_desc(14, 0x5000, 0xfffff, 0)).unwrap();
        assert_eq!(
            set_thread_area(&mut cpu, &mem, desc),
            Err(err(libc::EINVAL))
        );
    }

    #[test]
    fn implemented_list_matches_dispatch() {
        let (mem, _) = low_memory();
        let process = Process::new(
            Vec::new(),
            0x1000_0000,
            crate::root::GuestRoot::none(),
            std::path::PathBuf::new(),
        );
        let mut thread = Thread::default();
        // brk to 4 GiB would map most of the address space; pause, and
        // ppoll with no descriptors and no timeout, wait for a signal; fork,
        // vfork and clone would duplicate the test process (whose child
        // then hung on a lock of the test harness, and a `wait4` here or in
        // the x86_64 test waited for it forever).
        let skip = [
            nr::BRK,
            nr::PAUSE,
            nr::PPOLL,
            nr::PPOLL_TIME64,
            nr::FORK,
            nr::VFORK,
            nr::CLONE,
        ];
        for number in (0..512).filter(|n| !skip.contains(n)) {
            let mut cpu = Cpu::new_compat(0, 0);
            cpu.regs[RAX] = number;
            cpu.regs[RBX] = 0xffff_ffff;
            let outcome = dispatch(&mut cpu, &mem, &process, &mut thread);
            let unknown = matches!(&outcome, Outcome::Unsupported(what) if *what == format!("i386 syscall {number}"));
            assert_eq!(!unknown, is_implemented(number), "syscall {number}");
        }
    }

    #[test]
    fn results_are_32_bit_in_eax() {
        let (mem, _) = low_memory();
        let process = Process::new(
            Vec::new(),
            0,
            crate::root::GuestRoot::none(),
            std::path::PathBuf::new(),
        );
        let mut thread = Thread::default();
        let mut cpu = Cpu::new_compat(0, 0);
        // close(-1): EBADF, as a 32-bit negative number.
        cpu.regs[RAX] = nr::CLOSE;
        cpu.regs[RBX] = 0xffff_ffff;
        assert!(matches!(
            dispatch(&mut cpu, &mem, &process, &mut thread),
            Outcome::Continue
        ));
        assert_eq!(cpu.regs[RAX], u64::from(-libc::EBADF as u32));
        // The upper halves of the registers are ignored.
        cpu.regs[RAX] = 1 << 32 | nr::GETPID;
        assert!(matches!(
            dispatch(&mut cpu, &mem, &process, &mut thread),
            Outcome::Continue
        ));
        assert_eq!(cpu.regs[RAX], u64::from(std::process::id()));
    }
}
