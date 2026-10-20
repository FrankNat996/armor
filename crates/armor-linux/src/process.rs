//! Processes (ADR-0019): fork and vfork as a host fork of armor, execve
//! that runs armor again for x86_64 programs, wait4.

use std::ffi::{CString, OsStr};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;

use armor_cpu::{Cpu, RAX, RSP};
use armor_mem::{AddressSpace, GuestMemory};

use crate::syscall::{Process, Thread};

// clone flags, identical on both architectures.
const CLONE_VM: u64 = 0x100;
const CLONE_VFORK: u64 = 0x4000;
const CLONE_SETTLS: u64 = 0x8_0000;
const CLONE_PARENT_SETTID: u64 = 0x10_0000;
const CLONE_CHILD_CLEARTID: u64 = 0x20_0000;
const CLONE_CHILD_SETTID: u64 = 0x100_0000;
/// The low byte of the flags: the signal the parent gets when the child
/// ends.
const CSIGNAL: u64 = 0xff;
const SIGCHLD: u64 = 17;

/// fork(2): exit signal SIGCHLD, no other flag.
pub const FORK_FLAGS: u64 = SIGCHLD;
/// vfork(2), as glibc's posix_spawn also asks with clone.
pub const VFORK_FLAGS: u64 = CLONE_VM | CLONE_VFORK | SIGCHLD;

/// Longest argument or environment string (MAX_ARG_STRLEN) and the most
/// strings armor reads for execve.
const MAX_ARG_STRLEN: usize = 128 * 1024;
const MAX_ARGS: usize = 64 * 1024;

/// A syscall result, or `Err` naming what armor does not support.
type Outcome = Result<Result<i64, i64>, String>;

fn errno() -> i64 {
    -i64::from(
        std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO),
    )
}

/// What armor's front end does around a fork of a guest with several
/// threads, before the locks of `fork` and after it: the GIL, which must be
/// taken first (its holder runs a block of instructions and waits for the
/// page tables). Set once with `set_fork_hooks`.
pub trait ForkHooks: Sync {
    fn prepare(&'static self);
    fn parent(&'static self);
    fn child(&'static self);
}

static FORK_HOOKS: std::sync::OnceLock<&'static dyn ForkHooks> = std::sync::OnceLock::new();

pub fn set_fork_hooks(hooks: &'static dyn ForkHooks) {
    let _ = FORK_HOOKS.set(hooks);
}

/// fork, vfork and clone without CLONE_THREAD: a fork of the whole armor
/// process (ADR-0019). `stack`, `parent_tid`, `child_tid` and `tls` are
/// clone's arguments (0 and None for fork and vfork).
#[allow(clippy::too_many_arguments)]
pub fn fork(
    cpu: &mut Cpu,
    mem: &AddressSpace,
    process: &Process,
    thread: &mut Thread,
    flags: u64,
    stack: u64,
    parent_tid: u64,
    child_tid: u64,
    tls: Option<crate::syscall::NewTls>,
) -> Outcome {
    let vfork = flags & (CLONE_VM | CLONE_VFORK) == CLONE_VM | CLONE_VFORK;
    let mut allowed =
        CSIGNAL | CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID | CLONE_PARENT_SETTID | CLONE_SETTLS;
    if vfork {
        allowed |= CLONE_VM | CLONE_VFORK;
    }
    if flags & !allowed != 0 || flags & CSIGNAL != SIGCHLD {
        return Err(format!(
            "clone with flags {flags:#x} (a process other than fork or vfork)"
        ));
    }
    // With several guest threads, the others keep running during the
    // fork: as Linux does, the child gets only this one. So that the copy
    // of armor's state the child gets is consistent, this thread takes
    // every lock the others may hold first (brk, the signal actions, the
    // page tables, which also stops them at the end of their current block
    // of instructions); parent and child release them after. The GIL, when
    // there is one, has its own pthread_atfork handlers (armor's gil.rs).
    let several = process
        .live_threads
        .load(std::sync::atomic::Ordering::SeqCst)
        != 1;
    let hooks = FORK_HOOKS.get().filter(|_| several);
    if let Some(hooks) = hooks {
        hooks.prepare();
    }
    let brk = several.then(|| process.hold_brk());
    let actions = several.then(|| thread.signals.hold_actions());
    let tables = several.then(|| mem.exclusive());
    // vfork: the parent waits until the child closes this pipe, which
    // happens at execve (O_CLOEXEC) or when it exits.
    let mut pipe = [-1i32; 2];
    // SAFETY: pipe is writable for two descriptors.
    if vfork && unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Ok(Err(errno()));
    }
    // SAFETY: this thread holds the locks the others could hold (above),
    // so the child, which has only this thread, gets a consistent copy of
    // armor's state; glibc's own locks (malloc) are handled by its fork.
    let pid = unsafe { libc::fork() };
    drop((tables, actions, brk));
    if let Some(hooks) = hooks {
        if pid == 0 {
            hooks.child();
        } else {
            hooks.parent();
        }
    }
    if pid < 0 {
        let e = errno();
        close_pair(pipe);
        return Ok(Err(e));
    }
    if pid == 0 {
        // The other guest threads do not exist in the child.
        process
            .live_threads
            .store(1, std::sync::atomic::Ordering::SeqCst);
        if vfork {
            // SAFETY: closing the read end of the pipe made above.
            unsafe { libc::close(pipe[0]) };
        }
        if stack != 0 {
            cpu.regs[RSP] = stack;
        }
        if let Some(tls) = tls {
            tls.apply(cpu);
        }
        let tid = crate::current_tid();
        if flags & CLONE_CHILD_SETTID != 0 {
            // Like Linux, a failed write is ignored.
            let _ = mem.write(child_tid, &(tid as u32).to_le_bytes());
        }
        if flags & CLONE_CHILD_CLEARTID != 0 {
            thread.clear_child_tid = child_tid;
        }
        cpu.regs[RAX] = 0;
        return Ok(Ok(0));
    }
    if vfork {
        // SAFETY: closing the write end, then reading the read end, both
        // made above.
        unsafe { libc::close(pipe[1]) };
        let mut byte = 0u8;
        loop {
            // SAFETY: byte is writable for 1 byte.
            let n = unsafe { libc::read(pipe[0], (&raw mut byte).cast(), 1) };
            if n >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break;
            }
        }
        // SAFETY: closing the read end.
        unsafe { libc::close(pipe[0]) };
    }
    if flags & CLONE_PARENT_SETTID != 0 {
        let _ = mem.write(parent_tid, &(pid as u32).to_le_bytes());
    }
    Ok(Ok(i64::from(pid)))
}

fn close_pair(pipe: [i32; 2]) {
    for fd in pipe {
        if fd >= 0 {
            // SAFETY: closing descriptors made by pipe2.
            unsafe { libc::close(fd) };
        }
    }
}

/// Directories of the host's own programs and configuration.
const HOST_SYSTEM_DIRS: [&[u8]; 6] = [
    b"/usr/", b"/bin/", b"/sbin/", b"/lib/", b"/lib64/", b"/etc/",
];

/// Whether the script at guest path `guest_path` (`host_path` once
/// resolved in the guest root) is one of the host system's: in a system
/// directory and not overridden by the root. Such a script is the host's
/// program (Debian's `/usr/bin/chromium` launcher, `xdg-open`), written for
/// the host's tools, so it runs with the host's interpreter (ADR-0028):
/// under the root's x86-64 `sh`, `uname -m` said `x86_64` and the
/// launcher looked for SSE3 in the host's `/proc/cpuinfo`, then refused
/// to start. Scripts anywhere else (a game's launcher next to its files)
/// and those the root provides keep the root's interpreter.
pub fn is_host_system_script(guest_path: &[u8], host_path: &[u8]) -> bool {
    guest_path == host_path
        && HOST_SYSTEM_DIRS
            .iter()
            .any(|dir| guest_path.starts_with(dir))
}

/// execve(2). x86_64 programs (directly or as a script's interpreter) run
/// under a new armor; anything else, and the host system's scripts
/// (`is_host_system_script`), goes to the host kernel. Returns only
/// on failure. `word` is the size of the guest's pointers in argv and
/// envp: 8, or 4 for an i386 guest.
pub fn execve(
    mem: &AddressSpace,
    process: &Process,
    path: u64,
    argv: u64,
    envp: u64,
    word: usize,
) -> Result<i64, i64> {
    let guest_path = read_string(mem, path)?;
    execve_path(mem, process, guest_path, argv, envp, word)
}

/// execve(2) of the program at `guest_path`, already read from the guest
/// (execveat builds it from a descriptor).
pub fn execve_path(
    mem: &AddressSpace,
    process: &Process,
    guest_path: Vec<u8>,
    argv: u64,
    envp: u64,
    word: usize,
) -> Result<i64, i64> {
    let args = read_strings(mem, argv, word)?;
    let env = read_strings(mem, envp, word)?;
    let guest_path = if crate::syscall::is_self_exe(&guest_path) {
        process.exe_path.clone()
    } else {
        guest_path
    };
    let host_path = process.root.resolve(&guest_path);
    let header = read_header(&host_path)?;
    if is_guest_elf(&header) {
        let argv0 = args.first().cloned().unwrap_or_else(|| guest_path.clone());
        let rest = args.get(1..).unwrap_or(&[]).to_vec();
        return exec_armor(process, &argv0, &host_path, &rest, &env);
    }
    if let Some((interpreter, optional)) = script_interpreter(&header)
        && !is_host_system_script(&guest_path, &host_path)
    {
        let interpreter_host = process.root.resolve(&interpreter);
        if read_header(&interpreter_host).is_ok_and(|h| is_guest_elf(&h)) {
            // The kernel's arguments for a script: interpreter, its
            // optional argument, the script path, then argv[1..].
            let mut rest: Vec<Vec<u8>> = optional.into_iter().collect();
            rest.push(guest_path);
            rest.extend(args.get(1..).unwrap_or(&[]).iter().cloned());
            return exec_armor(process, &interpreter, &interpreter_host, &rest, &env);
        }
    }
    exec_host(&host_path, &args, &env)
}

fn exec_armor(
    process: &Process,
    argv0: &[u8],
    program: &[u8],
    rest: &[Vec<u8>],
    env: &[Vec<u8>],
) -> Result<i64, i64> {
    let root = process
        .root
        .dir()
        .map(|d| d.as_os_str().as_bytes().to_vec())
        .unwrap_or_default();
    let armor = process.armor_exe.as_os_str().as_bytes().to_vec();
    let mut args = vec![armor.clone()];
    // Always the engine by name: the default may differ (ADR-0028).
    args.push(if process.lockstep {
        b"--lockstep".to_vec()
    } else if process.jit {
        b"--jit".to_vec()
    } else {
        b"--interpreter".to_vec()
    });
    args.extend(process.diagnose.iter().cloned());
    if process.root.host_gl().is_some() {
        args.push(b"--host-gl".to_vec());
    }
    args.extend([
        b"--root".to_vec(),
        root,
        b"--argv0".to_vec(),
        argv0.to_vec(),
        program.to_vec(),
    ]);
    args.extend(rest.iter().cloned());
    // The guest's seccomp filters go to the new armor (ADR-0044).
    let mut env = env.to_vec();
    env.extend(crate::seccomp::exec_variable());
    exec_host(&armor, &args, &env)
}

/// Replaces armor with the host program `path` (a native AArch64 program,
/// or a script whose interpreter is native: ADR-0028). Returns only on
/// failure, with the negated errno.
pub fn exec_native(path: &[u8], args: &[Vec<u8>], env: &[Vec<u8>]) -> i64 {
    match exec_host(path, args, env) {
        Ok(_) => unreachable!("execve returns only on failure"),
        Err(errno) => errno,
    }
}

/// Run before armor replaces itself (`execve`): the JIT's cache is
/// written then (ADR-0048), as the process does not exit.
pub static BEFORE_EXEC: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

fn exec_host(path: &[u8], args: &[Vec<u8>], env: &[Vec<u8>]) -> Result<i64, i64> {
    if let Some(hook) = BEFORE_EXEC.get() {
        hook();
    }
    // A native program dumps cores as it would without armor; a new armor
    // limits its own again.
    crate::restore_core_dumps();
    let c = |bytes: &Vec<u8>| CString::new(bytes.clone()).map_err(|_| -i64::from(libc::EINVAL));
    let path = CString::new(path.to_vec()).map_err(|_| -i64::from(libc::EINVAL))?;
    let args = args.iter().map(c).collect::<Result<Vec<_>, _>>()?;
    let env = env.iter().map(c).collect::<Result<Vec<_>, _>>()?;
    let mut arg_ptrs: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    arg_ptrs.push(std::ptr::null());
    let mut env_ptrs: Vec<*const libc::c_char> = env.iter().map(|e| e.as_ptr()).collect();
    env_ptrs.push(std::ptr::null());
    // SAFETY: path and every string are NUL-terminated and alive; both
    // pointer arrays end with null.
    unsafe { libc::execve(path.as_ptr(), arg_ptrs.as_ptr(), env_ptrs.as_ptr()) };
    Err(errno())
}

/// The first bytes of a file, or the errno execve would report.
fn read_header(path: &[u8]) -> Result<Vec<u8>, i64> {
    let mut file = std::fs::File::open(OsStr::from_bytes(path))
        .map_err(|e| -i64::from(e.raw_os_error().unwrap_or(libc::ENOENT)))?;
    let mut header = vec![0u8; 256];
    let n = file
        .read(&mut header)
        .map_err(|e| -i64::from(e.raw_os_error().unwrap_or(libc::EIO)))?;
    header.truncate(n);
    Ok(header)
}

pub fn is_x86_64_elf(header: &[u8]) -> bool {
    header.len() >= 20
        && header.starts_with(b"\x7fELF")
        && header[4] == 2
        && header[18..20] == [62, 0]
}

/// An i386 ELF (ELFCLASS32, EM_386), which armor runs in compatibility
/// mode (ADR-0029).
pub fn is_i386_elf(header: &[u8]) -> bool {
    header.len() >= 20
        && header.starts_with(b"\x7fELF")
        && header[4] == 1
        && header[18..20] == [3, 0]
}

/// A program armor runs: x86-64 or i386.
pub fn is_guest_elf(header: &[u8]) -> bool {
    is_x86_64_elf(header) || is_i386_elf(header)
}

/// The interpreter of a `#!` script and its optional single argument, as
/// the kernel splits the first line.
pub fn script_interpreter(header: &[u8]) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let line = header.strip_prefix(b"#!")?;
    let line = line.split(|&b| b == b'\n').next()?;
    let line = line.trim_ascii();
    let mut parts = line.splitn(2, |b| b.is_ascii_whitespace());
    let interpreter = parts.next().filter(|i| !i.is_empty())?.to_vec();
    let optional = parts
        .next()
        .map(|a| a.trim_ascii().to_vec())
        .filter(|a| !a.is_empty());
    Some((interpreter, optional))
}

fn read_string(mem: &AddressSpace, addr: u64) -> Result<Vec<u8>, i64> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8];
        mem.read(addr.wrapping_add(bytes.len() as u64), &mut byte)
            .map_err(|_| -i64::from(libc::EFAULT))?;
        if byte[0] == 0 {
            return Ok(bytes);
        }
        if bytes.len() >= MAX_ARG_STRLEN {
            return Err(-i64::from(libc::E2BIG));
        }
        bytes.push(byte[0]);
    }
}

/// A NULL-terminated array of string pointers (argv, envp) of `word`
/// bytes each; a null array is empty, as Linux accepts.
fn read_strings(mem: &AddressSpace, addr: u64, word: usize) -> Result<Vec<Vec<u8>>, i64> {
    let mut strings = Vec::new();
    if addr == 0 {
        return Ok(strings);
    }
    loop {
        let mut pointer = [0u8; 8];
        let at = addr.wrapping_add((word * strings.len()) as u64);
        mem.read(at, &mut pointer[..word])
            .map_err(|_| -i64::from(libc::EFAULT))?;
        let pointer = u64::from_le_bytes(pointer);
        if pointer == 0 {
            return Ok(strings);
        }
        if strings.len() >= MAX_ARGS {
            return Err(-i64::from(libc::E2BIG));
        }
        strings.push(read_string(mem, pointer)?);
    }
}

/// wait4(2): `int` status and `struct rusage` (144 bytes) are the same on
/// both architectures. `compat`: an i386 guest's 72-byte `struct rusage`
/// (two 32-bit `timeval`s and fourteen 32-bit `long`s).
pub fn wait4(
    mem: &AddressSpace,
    pid: u64,
    status: u64,
    options: u64,
    rusage: u64,
    compat: bool,
) -> Result<i64, i64> {
    let mut code = 0i32;
    let mut usage = [0u8; 144];
    // SAFETY: code and usage are host buffers of the sizes wait4 writes.
    let result = unsafe {
        libc::syscall(
            libc::SYS_wait4,
            pid as i32,
            &raw mut code,
            options as i32,
            usage.as_mut_ptr(),
        )
    };
    if result < 0 {
        return Err(errno());
    }
    if result > 0 {
        if status != 0 {
            mem.write(status, &code.to_le_bytes())
                .map_err(|_| -i64::from(libc::EFAULT))?;
        }
        if rusage != 0 {
            let usage = if compat {
                // Every 8-byte field (the two timevals are two each) as
                // 4 bytes.
                usage
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .flat_map(|field| (i64::from_le_bytes(*field) as i32).to_le_bytes())
                    .collect()
            } else {
                usage.to_vec()
            };
            mem.write(rusage, &usage)
                .map_err(|_| -i64::from(libc::EFAULT))?;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_split_like_the_kernel_does() {
        assert_eq!(
            script_interpreter(b"#!/bin/sh\necho"),
            Some((b"/bin/sh".to_vec(), None))
        );
        assert_eq!(
            script_interpreter(b"#! /usr/bin/env  python3 -u \nx"),
            Some((b"/usr/bin/env".to_vec(), Some(b"python3 -u".to_vec())))
        );
        assert_eq!(script_interpreter(b"#!\n"), None);
        assert_eq!(script_interpreter(b"\x7fELF"), None);
    }

    #[test]
    fn host_system_scripts_are_those_the_root_does_not_override() {
        assert!(is_host_system_script(
            b"/usr/bin/chromium",
            b"/usr/bin/chromium"
        ));
        assert!(is_host_system_script(b"/etc/init.d/x", b"/etc/init.d/x"));
        // Provided by the root: its own script.
        assert!(!is_host_system_script(
            b"/usr/bin/xdg-open",
            b"/apps/x/root/usr/bin/xdg-open"
        ));
        // Outside the system directories: a game's launcher, a test.
        assert!(!is_host_system_script(
            b"/home/u/game/start.sh",
            b"/home/u/game/start.sh"
        ));
        assert!(!is_host_system_script(
            b"/opt/game/run.sh",
            b"/opt/game/run.sh"
        ));
        assert!(!is_host_system_script(b"./run.sh", b"./run.sh"));
        assert!(!is_host_system_script(b"/usrx/run.sh", b"/usrx/run.sh"));
    }

    #[test]
    fn x86_64_elf_headers_are_recognized() {
        let mut header = vec![0u8; 64];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = 2;
        header[18] = 62;
        assert!(is_x86_64_elf(&header));
        header[18] = 183; // AArch64
        assert!(!is_x86_64_elf(&header));
    }
}
