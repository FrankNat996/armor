//! seccomp for the guest (ADR-0044): `seccomp(2)` and
//! `prctl(PR_SET_SECCOMP)`, emulated by armor. A filter given to the host
//! would judge armor's own AArch64 syscalls by x86-64 numbers, so armor
//! keeps each thread's filters and runs them, as the kernel does, before
//! every guest syscall, on a `struct seccomp_data` with the guest's
//! numbers, `AUDIT_ARCH_X86_64` (or `AUDIT_ARCH_I386`), the address after
//! the syscall instruction and the six arguments.
//!
//! As in Linux: filters are classic BPF programs checked when they are
//! installed (the instructions seccomp allows, jumps inside the program,
//! a return at the end, scratch memory written before it is read); the
//! action of highest precedence among a thread's filters wins; a new
//! thread inherits its creator's filters and SECCOMP_FILTER_FLAG_TSYNC
//! gives the caller's to every thread; filters survive execve (armor
//! passes them to the armor that runs the new program); installing one
//! needs no_new_privs or CAP_SYS_ADMIN. SECCOMP_RET_TRAP raises SIGSYS
//! with `si_code` SYS_SECCOMP, the call's address, number and
//! architecture, and the syscall number back in rax; a SIGSYS the guest
//! blocks or ignores kills it, as `force_sig_seccomp` does.
//!
//! Not done: user-space notification (SECCOMP_FILTER_FLAG_NEW_LISTENER,
//! for which armor stops with a message, and SECCOMP_RET_USER_NOTIF, which
//! answers ENOSYS as a filter without a listener does), and filters on a
//! native program that armor starts (its syscalls are AArch64's).

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use armor_mem::{AddressSpace, GuestMemory};

/// `AUDIT_ARCH_X86_64` and `AUDIT_ARCH_I386`.
pub const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
pub const AUDIT_ARCH_I386: u32 = 0x4000_0003;

const SIGSYS: i32 = 31;
const SYS_SECCOMP: i32 = 1;

// Actions (SECCOMP_RET_*).
const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_KILL_THREAD: u32 = 0;
const RET_TRAP: u32 = 0x0003_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const RET_USER_NOTIF: u32 = 0x7fc0_0000;
const RET_TRACE: u32 = 0x7ff0_0000;
const RET_LOG: u32 = 0x7ffc_0000;
const RET_ALLOW: u32 = 0x7fff_0000;
const RET_ACTION_FULL: u32 = 0xffff_0000;
const RET_DATA: u32 = 0x0000_ffff;

// seccomp(2) operations and filter flags.
const SET_MODE_STRICT: u64 = 0;
const SET_MODE_FILTER: u64 = 1;
const GET_ACTION_AVAIL: u64 = 2;
const GET_NOTIF_SIZES: u64 = 3;
const FLAG_TSYNC: u64 = 1;
const FLAG_LOG: u64 = 2;
const FLAG_SPEC_ALLOW: u64 = 4;
const FLAG_NEW_LISTENER: u64 = 8;
const FLAG_TSYNC_ESRCH: u64 = 16;
const FLAG_WAIT_KILLABLE_RECV: u64 = 32;

/// The largest program, and the most instructions on one thread's path.
const BPF_MAXINSNS: usize = 4096;
const MAX_INSNS_PER_PATH: usize = 32768;
/// `sizeof(struct seccomp_data)`.
const DATA_SIZE: u32 = 64;

/// A classic BPF instruction (`struct sock_filter`, 8 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Insn {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

/// One installed filter.
#[derive(Debug)]
struct Filter {
    insns: Vec<Insn>,
}

/// A thread's seccomp state: strict mode, or its filters (oldest first),
/// shared with the threads and processes that inherited them.
#[derive(Clone, Debug, Default)]
pub struct Seccomp {
    strict: bool,
    filters: Arc<Vec<Arc<Filter>>>,
    /// The TSYNC generation this thread has adopted (`Process`).
    generation: u64,
}

/// The process-wide part: the filters a TSYNC gave every thread, and a
/// counter each thread compares with its own before a syscall.
#[derive(Debug, Default)]
pub struct Shared {
    generation: AtomicU64,
    synced: Mutex<Option<Seccomp>>,
}

/// What a syscall gets.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Errno(i64),
    /// SIGSYS with this `si_errno`.
    Trap(i32),
    KillThread,
    KillProcess,
    /// Strict mode: SIGKILL.
    Kill,
}

impl Seccomp {
    pub fn active(&self) -> bool {
        self.strict || !self.filters.is_empty()
    }

    /// The mode PR_GET_SECCOMP reports: 0, 1 (strict) or 2 (filter).
    pub fn mode(&self) -> u64 {
        if self.strict {
            1
        } else if self.filters.is_empty() {
            0
        } else {
            2
        }
    }

    /// Adopts the filters of the latest TSYNC, if newer than this thread's.
    pub fn sync(&mut self, shared: &Shared) {
        if shared.generation.load(Ordering::Acquire) > self.generation
            && let Some(synced) = shared.synced.lock().expect("seccomp lock").clone()
        {
            *self = synced;
        }
    }

    /// The verdict on syscall `number` (in `arch`) at `ip` with `args`.
    pub fn judge(&self, number: u64, arch: u32, ip: u64, args: [u64; 6]) -> Verdict {
        if self.strict {
            let allowed: &[u64] = if arch == AUDIT_ARCH_I386 {
                &[3, 4, 1, 119] // read, write, exit, sigreturn
            } else {
                &[0, 1, 60, 15] // read, write, exit, rt_sigreturn
            };
            return if allowed.contains(&number) {
                Verdict::Allow
            } else {
                Verdict::Kill
            };
        }
        let mut data = [0u8; DATA_SIZE as usize];
        data[..4].copy_from_slice(&(number as u32).to_le_bytes());
        data[4..8].copy_from_slice(&arch.to_le_bytes());
        data[8..16].copy_from_slice(&ip.to_le_bytes());
        for (i, arg) in args.iter().enumerate() {
            data[16 + i * 8..24 + i * 8].copy_from_slice(&arg.to_le_bytes());
        }
        let mut result = RET_ALLOW;
        for filter in self.filters.iter().rev() {
            let value = run(&filter.insns, &data);
            if ((value & RET_ACTION_FULL) as i32) < ((result & RET_ACTION_FULL) as i32) {
                result = value;
            }
        }
        let data = result & RET_DATA;
        match result & RET_ACTION_FULL {
            RET_ALLOW | RET_LOG => Verdict::Allow,
            RET_ERRNO => Verdict::Errno(-i64::from(data.min(4095))),
            RET_TRAP => Verdict::Trap(data as i32),
            // No tracer (armor does not do ptrace), no listener: the
            // syscall is skipped with ENOSYS, as in Linux.
            RET_TRACE | RET_USER_NOTIF => Verdict::Errno(-i64::from(libc::ENOSYS)),
            RET_KILL_THREAD => Verdict::KillThread,
            _ => Verdict::KillProcess,
        }
    }
}

/// Runs a checked program on `data`.
fn run(insns: &[Insn], data: &[u8; DATA_SIZE as usize]) -> u32 {
    let (mut a, mut x) = (0u32, 0u32);
    let mut memory = [0u32; 16];
    let mut pc = 0usize;
    while let Some(&Insn { code, jt, jf, k }) = insns.get(pc) {
        pc += 1;
        let src = if code & 0x08 != 0 { x } else { k };
        match code {
            0x20 => {
                a = u32::from_le_bytes(
                    data[k as usize..k as usize + 4]
                        .try_into()
                        .expect("checked"),
                )
            }
            0x80 => a = DATA_SIZE,
            0x81 => x = DATA_SIZE,
            0x00 => a = k,
            0x01 => x = k,
            0x60 => a = memory[k as usize],
            0x61 => x = memory[k as usize],
            0x02 => memory[k as usize] = a,
            0x03 => memory[k as usize] = x,
            0x07 => x = a,
            0x87 => a = x,
            0x84 => a = a.wrapping_neg(),
            0x04 | 0x0c => a = a.wrapping_add(src),
            0x14 | 0x1c => a = a.wrapping_sub(src),
            0x24 | 0x2c => a = a.wrapping_mul(src),
            0x34 | 0x3c => {
                // Division by a zero X ends the program with 0.
                if src == 0 {
                    return 0;
                }
                a /= src;
            }
            0x54 | 0x5c => a &= src,
            0x44 | 0x4c => a |= src,
            0xa4 | 0xac => a ^= src,
            0x64 | 0x6c => a = a.checked_shl(src).unwrap_or(0),
            0x74 | 0x7c => a = a.checked_shr(src).unwrap_or(0),
            0x05 => pc += k as usize,
            0x15 | 0x1d | 0x25 | 0x2d | 0x35 | 0x3d | 0x45 | 0x4d => {
                let taken = match code & 0xf0 {
                    0x10 => a == src,
                    0x20 => a > src,
                    0x30 => a >= src,
                    _ => a & src != 0,
                };
                pc += usize::from(if taken { jt } else { jf });
            }
            0x06 => return k,
            0x16 => return a,
            _ => return 0,
        }
    }
    0
}

/// Checks a program as Linux does (`bpf_check_classic` and
/// `seccomp_check_filter`); EINVAL if it is refused.
fn check(insns: &[Insn]) -> Result<(), i64> {
    let invalid = Err(-i64::from(libc::EINVAL));
    if insns.is_empty() || insns.len() > BPF_MAXINSNS {
        return invalid;
    }
    let len = insns.len();
    for (pc, insn) in insns.iter().enumerate() {
        let target = |offset: usize| pc + 1 + offset < len;
        let ok = match insn.code {
            0x20 => insn.k < DATA_SIZE && insn.k % 4 == 0,
            0x80 | 0x81 | 0x00 | 0x01 | 0x07 | 0x87 | 0x84 => true,
            0x60 | 0x61 | 0x02 | 0x03 => insn.k < 16,
            0x04 | 0x0c | 0x14 | 0x1c | 0x24 | 0x2c | 0x3c | 0x54 | 0x5c | 0x44 | 0x4c | 0xa4
            | 0xac | 0x6c | 0x7c => true,
            0x34 => insn.k != 0,
            0x64 | 0x74 => insn.k < 32,
            0x05 => target(insn.k as usize),
            0x15 | 0x1d | 0x25 | 0x2d | 0x35 | 0x3d | 0x45 | 0x4d => {
                target(usize::from(insn.jt)) && target(usize::from(insn.jf))
            }
            0x06 | 0x16 => true,
            _ => false,
        };
        if !ok {
            return invalid;
        }
    }
    if !matches!(insns[len - 1].code, 0x06 | 0x16) {
        return invalid;
    }
    // Scratch memory must be written before it is read, on every path
    // (`check_load_and_stores`).
    let mut masks = vec![0xffffu16; len];
    let mut valid = 0u16;
    for (pc, insn) in insns.iter().enumerate() {
        valid &= masks[pc];
        match insn.code {
            0x02 | 0x03 => valid |= 1 << insn.k,
            0x60 | 0x61 if valid & (1 << insn.k) == 0 => return invalid,
            0x05 => {
                masks[pc + 1 + insn.k as usize] &= valid;
                valid = !0;
            }
            0x15 | 0x1d | 0x25 | 0x2d | 0x35 | 0x3d | 0x45 | 0x4d => {
                masks[pc + 1 + usize::from(insn.jt)] &= valid;
                masks[pc + 1 + usize::from(insn.jf)] &= valid;
                valid = !0;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether this process may install a filter: no_new_privs, or
/// CAP_SYS_ADMIN in its effective set.
fn may_install() -> bool {
    // SAFETY: PR_GET_NO_NEW_PRIVS takes no pointer.
    if unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } == 1 {
        return true;
    }
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("CapEff:"))
                .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
        })
        .is_some_and(|caps| caps & (1 << 21) != 0)
}

/// Reads the `struct sock_fprog` (length, pad, pointer) at `at` and its
/// instructions.
fn read_program(mem: &AddressSpace, at: u64) -> Result<Vec<Insn>, i64> {
    let fault = -i64::from(libc::EFAULT);
    let mut header = [0u8; 16];
    mem.read(at, &mut header).map_err(|_| fault)?;
    let len = usize::from(u16::from_le_bytes([header[0], header[1]]));
    let filter = u64::from_le_bytes(header[8..].try_into().expect("8 bytes"));
    if len == 0 || len > BPF_MAXINSNS {
        return Err(-i64::from(libc::EINVAL));
    }
    let mut raw = vec![0u8; len * 8];
    mem.read(filter, &mut raw).map_err(|_| fault)?;
    Ok(raw
        .chunks(8)
        .map(|c| Insn {
            code: u16::from_le_bytes([c[0], c[1]]),
            jt: c[2],
            jf: c[3],
            k: u32::from_le_bytes(c[4..8].try_into().expect("4 bytes")),
        })
        .collect())
}

impl Seccomp {
    /// SECCOMP_SET_MODE_STRICT, or PR_SET_SECCOMP with mode 1.
    pub fn set_strict(&mut self) -> Result<i64, i64> {
        if !self.filters.is_empty() {
            return Err(-i64::from(libc::EINVAL));
        }
        self.strict = true;
        Ok(0)
    }

    /// SECCOMP_SET_MODE_FILTER, or PR_SET_SECCOMP with mode 2: `Err(None)`
    /// for a flag armor does not support.
    pub fn add_filter(
        &mut self,
        mem: &AddressSpace,
        shared: &Shared,
        flags: u64,
        program: u64,
    ) -> Result<Result<i64, i64>, String> {
        let known = FLAG_TSYNC
            | FLAG_LOG
            | FLAG_SPEC_ALLOW
            | FLAG_NEW_LISTENER
            | FLAG_TSYNC_ESRCH
            | FLAG_WAIT_KILLABLE_RECV;
        if flags & !known != 0 || self.strict {
            return Ok(Err(-i64::from(libc::EINVAL)));
        }
        if flags & FLAG_NEW_LISTENER != 0 {
            return Err(
                "seccomp user-space notification (SECCOMP_FILTER_FLAG_NEW_LISTENER)".to_string(),
            );
        }
        if !may_install() {
            return Ok(Err(-i64::from(libc::EACCES)));
        }
        let insns = match read_program(mem, program) {
            Ok(insns) => insns,
            Err(e) => return Ok(Err(e)),
        };
        if let Err(e) = check(&insns) {
            return Ok(Err(e));
        }
        let path: usize = self
            .filters
            .iter()
            .map(|f| f.insns.len() + 4)
            .sum::<usize>()
            + insns.len();
        if path > MAX_INSNS_PER_PATH {
            return Ok(Err(-i64::from(libc::ENOMEM)));
        }
        let mut filters = (*self.filters).clone();
        filters.push(Arc::new(Filter { insns }));
        self.filters = Arc::new(filters);
        if flags & FLAG_TSYNC != 0 {
            let mut synced = shared.synced.lock().expect("seccomp lock");
            self.generation = shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
            *synced = Some(self.clone());
        }
        Ok(Ok(0))
    }
}

/// seccomp(2): operation, flags, argument.
pub fn syscall(
    mem: &AddressSpace,
    shared: &Shared,
    state: &mut Seccomp,
    op: u64,
    flags: u64,
    arg: u64,
) -> Result<Result<i64, i64>, String> {
    Ok(match op {
        SET_MODE_STRICT if flags != 0 || arg != 0 => Err(-i64::from(libc::EINVAL)),
        SET_MODE_STRICT => state.set_strict(),
        SET_MODE_FILTER => return state.add_filter(mem, shared, flags, arg),
        GET_ACTION_AVAIL => {
            if flags != 0 {
                return Ok(Err(-i64::from(libc::EINVAL)));
            }
            let mut action = [0u8; 4];
            if mem.read(arg, &mut action).is_err() {
                return Ok(Err(-i64::from(libc::EFAULT)));
            }
            match u32::from_le_bytes(action) {
                RET_KILL_PROCESS | RET_KILL_THREAD | RET_TRAP | RET_ERRNO | RET_TRACE | RET_LOG
                | RET_ALLOW => Ok(0),
                _ => Err(-i64::from(libc::EOPNOTSUPP)),
            }
        }
        GET_NOTIF_SIZES => {
            if flags != 0 {
                return Ok(Err(-i64::from(libc::EINVAL)));
            }
            // seccomp_notif, seccomp_notif_resp, seccomp_data.
            let sizes = [80u16, 24, 64];
            let bytes: Vec<u8> = sizes.iter().flat_map(|s| s.to_le_bytes()).collect();
            match mem.write(arg, &bytes) {
                Ok(()) => Ok(0),
                Err(_) => Err(-i64::from(libc::EFAULT)),
            }
        }
        _ => Err(-i64::from(libc::EINVAL)),
    })
}

/// The siginfo of a SECCOMP_RET_TRAP: SIGSYS, `si_errno`, SYS_SECCOMP,
/// then `_sigsys`: the call's address, number and architecture.
pub fn trap_info(errno: i32, ip: u64, number: u64, arch: u32) -> [u8; 128] {
    let mut info = [0u8; 128];
    info[..4].copy_from_slice(&SIGSYS.to_le_bytes());
    info[4..8].copy_from_slice(&errno.to_le_bytes());
    info[8..12].copy_from_slice(&SYS_SECCOMP.to_le_bytes());
    info[16..24].copy_from_slice(&ip.to_le_bytes());
    info[24..28].copy_from_slice(&(number as i32).to_le_bytes());
    info[28..32].copy_from_slice(&arch.to_le_bytes());
    info
}

/// The descriptor of the filters written for the next execve
/// (`prepare_exec`), -1 for none.
static EXEC_FD: AtomicI32 = AtomicI32::new(-1);

/// The environment variable that carries the filters to the armor that
/// runs the program an execve starts.
pub const EXEC_VARIABLE: &str = "ARMOR_SECCOMP_FD";

impl Seccomp {
    fn serialize(&self) -> Vec<u8> {
        let mut out = b"armor-seccomp-1\n".to_vec();
        out.push(u8::from(self.strict));
        out.extend((self.filters.len() as u32).to_le_bytes());
        for filter in self.filters.iter() {
            out.extend((filter.insns.len() as u32).to_le_bytes());
            for insn in &filter.insns {
                out.extend(insn.code.to_le_bytes());
                out.extend([insn.jt, insn.jf]);
                out.extend(insn.k.to_le_bytes());
            }
        }
        out
    }

    fn deserialize(bytes: &[u8]) -> Option<Seccomp> {
        let rest = bytes.strip_prefix(b"armor-seccomp-1\n")?;
        let strict = *rest.first()? != 0;
        let count = u32::from_le_bytes(rest.get(1..5)?.try_into().ok()?);
        let mut at = 5;
        let mut filters = Vec::new();
        for _ in 0..count {
            let len = u32::from_le_bytes(rest.get(at..at + 4)?.try_into().ok()?) as usize;
            at += 4;
            let raw = rest.get(at..at + len * 8)?;
            at += len * 8;
            let insns: Vec<Insn> = raw
                .chunks(8)
                .map(|c| Insn {
                    code: u16::from_le_bytes([c[0], c[1]]),
                    jt: c[2],
                    jf: c[3],
                    k: u32::from_le_bytes(c[4..8].try_into().expect("4 bytes")),
                })
                .collect();
            check(&insns).ok()?;
            filters.push(Arc::new(Filter { insns }));
        }
        Some(Seccomp {
            strict,
            filters: Arc::new(filters),
            generation: 0,
        })
    }
}

/// Before an execve: writes the thread's filters into a memfd that the
/// next armor reads (`exec_variable`).
pub fn prepare_exec(state: &Seccomp) {
    if !state.active() {
        return;
    }
    let bytes = state.serialize();
    // SAFETY: a NUL-terminated name; no flags, so the descriptor survives
    // the execve.
    let fd = unsafe { libc::memfd_create(c"armor-seccomp".as_ptr(), 0) };
    if fd < 0 {
        return;
    }
    // SAFETY: bytes is readable for its length.
    let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    if written != bytes.len() as isize {
        // SAFETY: closing the memfd just made.
        unsafe { libc::close(fd) };
        return;
    }
    let old = EXEC_FD.swap(fd, Ordering::AcqRel);
    if old >= 0 {
        // SAFETY: closing the previous attempt's memfd.
        unsafe { libc::close(old) };
    }
}

/// The `ARMOR_SECCOMP_FD=<fd>` entry for the armor an execve starts, if
/// `prepare_exec` wrote filters.
pub fn exec_variable() -> Option<Vec<u8>> {
    let fd = EXEC_FD.load(Ordering::Acquire);
    (fd >= 0).then(|| format!("{EXEC_VARIABLE}={fd}").into_bytes())
}

/// In a new armor: the filters an execve passed (`ARMOR_SECCOMP_FD`),
/// read and closed.
pub fn inherited(variable: &str) -> Option<Seccomp> {
    let fd: i32 = variable.parse().ok()?;
    let bytes = std::fs::read(format!("/proc/self/fd/{fd}")).ok();
    // SAFETY: closing the descriptor the previous armor left open for us.
    unsafe { libc::close(fd) };
    Seccomp::deserialize(&bytes?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insn(code: u16, jt: u8, jf: u8, k: u32) -> Insn {
        Insn { code, jt, jf, k }
    }

    /// Allow everything but syscall 39 (getpid), which gets EPERM; 102
    /// (getuid) traps with data 7.
    fn program() -> Vec<Insn> {
        vec![
            insn(0x20, 0, 0, 4),                 // ld arch
            insn(0x15, 1, 0, AUDIT_ARCH_X86_64), // jeq x86_64
            insn(0x06, 0, 0, RET_KILL_PROCESS),  // ret kill
            insn(0x20, 0, 0, 0),                 // ld nr
            insn(0x15, 0, 1, 39),                // jeq getpid
            insn(0x06, 0, 0, RET_ERRNO | 1),     // ret errno EPERM
            insn(0x15, 0, 1, 102),               // jeq getuid
            insn(0x06, 0, 0, RET_TRAP | 7),      // ret trap 7
            insn(0x06, 0, 0, RET_ALLOW),         // ret allow
        ]
    }

    #[test]
    fn filters_judge_syscalls() {
        check(&program()).unwrap();
        let state = Seccomp {
            strict: false,
            filters: Arc::new(vec![Arc::new(Filter { insns: program() })]),
            generation: 0,
        };
        let judge = |nr, arch| state.judge(nr, arch, 0, [0; 6]);
        assert_eq!(judge(39, AUDIT_ARCH_X86_64), Verdict::Errno(-1));
        assert_eq!(judge(102, AUDIT_ARCH_X86_64), Verdict::Trap(7));
        assert_eq!(judge(1, AUDIT_ARCH_X86_64), Verdict::Allow);
        assert_eq!(judge(1, AUDIT_ARCH_I386), Verdict::KillProcess);
    }

    #[test]
    fn the_strictest_action_wins() {
        let allow = vec![insn(0x06, 0, 0, RET_ALLOW)];
        let errno = vec![insn(0x06, 0, 0, RET_ERRNO | 5)];
        let state = Seccomp {
            strict: false,
            filters: Arc::new(vec![
                Arc::new(Filter { insns: errno }),
                Arc::new(Filter { insns: allow }),
            ]),
            generation: 0,
        };
        assert_eq!(
            state.judge(0, AUDIT_ARCH_X86_64, 0, [0; 6]),
            Verdict::Errno(-5)
        );
    }

    #[test]
    fn bad_programs_are_refused() {
        assert!(check(&[]).is_err());
        // Ends without a return.
        assert!(check(&[insn(0x00, 0, 0, 1)]).is_err());
        // A load outside seccomp_data, a byte load, a jump out.
        assert!(check(&[insn(0x20, 0, 0, 64), insn(0x06, 0, 0, 0)]).is_err());
        assert!(check(&[insn(0x30, 0, 0, 0), insn(0x06, 0, 0, 0)]).is_err());
        assert!(check(&[insn(0x05, 0, 0, 5), insn(0x06, 0, 0, 0)]).is_err());
        // Scratch memory read before it is written.
        assert!(check(&[insn(0x60, 0, 0, 0), insn(0x06, 0, 0, 0)]).is_err());
        assert!(
            check(&[
                insn(0x02, 0, 0, 0),
                insn(0x60, 0, 0, 0),
                insn(0x16, 0, 0, 0)
            ])
            .is_ok()
        );
    }

    #[test]
    fn filters_survive_serialization() {
        let state = Seccomp {
            strict: false,
            filters: Arc::new(vec![Arc::new(Filter { insns: program() })]),
            generation: 3,
        };
        let back = Seccomp::deserialize(&state.serialize()).unwrap();
        assert_eq!(back.filters.len(), 1);
        assert_eq!(back.filters[0].insns, program());
    }
}
