//! `armor --sandbox` (ADR-0027, docs/tech/security.md): isolates armor's
//! process, and so the guest, with kernel mechanisms that hold even if a
//! bug in armor gives the guest control of the process.
//!
//! [`enter`] runs before the guest is loaded, while armor has one thread:
//! 1. new user, mount, PID and (without `network`) network namespaces;
//!    the user is mapped onto itself, so ids look the same inside;
//! 2. a fork: the parent stays outside as the supervisor, which waits and
//!    ends the way the guest ended (exit status or signal) and forwards
//!    termination signals inward;
//! 3. the child is PID 1 of the new namespace (an init, which the kernel
//!    protects from signals without a handler, so the guest must not be
//!    it). It builds the new root and forks again; its child, PID 2,
//!    returns from `enter` and runs armor. The init reaps processes and
//!    reports how PID 2 ended to the supervisor through a pipe.
//!
//! The new root is a tmpfs with: the paths given read-write and read-only
//! at the same absolute paths, `/dev/{null,zero,full,random,urandom,tty}`,
//! a new `/proc` (of the new PID namespace: no other processes) and an
//! empty `/tmp`. Nothing else of the host's file system is reachable.

use std::ffi::CString;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};

use crate::terminate_by_signal;

/// What the guest may reach.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Bound read-write at the same path (the current directory, `--allow`).
    pub read_write: Vec<PathBuf>,
    /// Bound read-only at the same path (the guest root, armor and its
    /// libraries, `--allow-ro`).
    pub read_only: Vec<PathBuf>,
    /// Keep the host's network (`--allow-net`).
    pub network: bool,
}

/// Device nodes the guest gets, if the host has them.
const DEVICES: [&str; 6] = [
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
];

/// Signals the supervisor and the init pass on to the guest: those another
/// process (a shell, `timeout`) sends to stop it. Terminal signals (SIGINT,
/// SIGQUIT) reach the guest directly, through its process group.
const FORWARDED: [i32; 5] = [
    libc::SIGTERM,
    libc::SIGHUP,
    libc::SIGUSR1,
    libc::SIGUSR2,
    libc::SIGCONT,
];

/// Where forwarded signals go: the init (in the supervisor) or armor (in
/// the init).
static FORWARD_TO: AtomicI32 = AtomicI32::new(0);

extern "C" fn forward(signal: libc::c_int) {
    let pid = FORWARD_TO.load(Ordering::Relaxed);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(pid, signal) };
    }
}

/// How the guest ended, as the init reports it.
enum Ended {
    Exit(i32),
    Signal(i32),
}

/// Isolates the process; returns in the process that goes on to run armor.
/// The other two never return.
pub fn enter(config: &Config) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let mut read_write = vec![cwd.clone()];
    read_write.extend(config.read_write.iter().cloned());
    for path in read_write.iter().chain(&config.read_only) {
        if std::fs::symlink_metadata(path).is_err() {
            return Err(format!("{}: no such file or directory", path.display()));
        }
    }
    // SAFETY: getuid and getgid cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let mut flags = libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWPID;
    if !config.network {
        flags |= libc::CLONE_NEWNET;
    }
    // SAFETY: unshare has no memory-safety preconditions; the process has
    // one thread, as CLONE_NEWUSER requires.
    if unsafe { libc::unshare(flags) } != 0 {
        return Err(format!(
            "cannot create the namespaces ({}): unprivileged user namespaces may be disabled \
             on this system",
            std::io::Error::last_os_error()
        ));
    }
    write_file("/proc/self/setgroups", "deny")?;
    write_file("/proc/self/uid_map", &format!("{uid} {uid} 1"))?;
    write_file("/proc/self/gid_map", &format!("{gid} {gid} 1"))?;

    let (status_read, status_write) = pipe()?;
    // SAFETY: the process has one thread; the child only runs code that is
    // safe after fork (no locks held).
    match unsafe { libc::fork() } {
        -1 => Err(format!("fork: {}", std::io::Error::last_os_error())),
        0 => {
            drop(status_read);
            init(&read_write, &config.read_only, &cwd, status_write)
        }
        init_pid => {
            drop(status_write);
            supervise(init_pid, status_read)
        }
    }
}

/// The supervisor, outside the PID namespace: forwards signals to the
/// init, waits for it, and ends the way the guest ended.
fn supervise(init_pid: i32, status: OwnedFd) -> ! {
    FORWARD_TO.store(init_pid, Ordering::Relaxed);
    install_forwarding();
    // The guest gets terminal signals through its process group; the
    // supervisor must outlive it.
    ignore(libc::SIGINT);
    ignore(libc::SIGQUIT);
    let mut report = [0u8; 8];
    let mut file = std::fs::File::from(status);
    let complete = file.read_exact(&mut report).is_ok();
    let init_status = wait_for(init_pid);
    let ended = if complete {
        let kind = i32::from_le_bytes(report[..4].try_into().unwrap());
        let value = i32::from_le_bytes(report[4..].try_into().unwrap());
        if kind == 0 {
            Ended::Exit(value)
        } else {
            Ended::Signal(value)
        }
    } else {
        // The init died before reporting: say how it ended.
        decode(init_status)
    };
    match ended {
        Ended::Exit(code) => std::process::exit(code),
        Ended::Signal(signal) => terminate_by_signal(signal),
    }
}

/// PID 1 of the new namespace: builds the new root, starts armor as PID 2,
/// reaps every process, and reports how armor ended.
fn init(
    read_write: &[PathBuf],
    read_only: &[PathBuf],
    cwd: &Path,
    status: OwnedFd,
) -> Result<(), String> {
    // SAFETY: prctl with PR_SET_PDEATHSIG has no memory-safety
    // preconditions. If the supervisor dies, the init dies, and with it
    // every process of the namespace.
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
    if let Err(e) = build_root(read_write, read_only, cwd) {
        eprintln!("armor: --sandbox: {e}");
        report(status, 0, 125);
    }
    // SAFETY: as in `enter`.
    let armor = unsafe { libc::fork() };
    if armor == 0 {
        drop(status);
        return seccomp();
    }
    if armor < 0 {
        eprintln!(
            "armor: --sandbox: fork: {}",
            std::io::Error::last_os_error()
        );
        report(status, 0, 125);
    }
    FORWARD_TO.store(armor, Ordering::Relaxed);
    install_forwarding();
    loop {
        let mut raw = 0;
        // SAFETY: waitpid writes the status through a valid pointer.
        let pid = unsafe { libc::waitpid(-1, &mut raw, 0) };
        if pid == armor {
            match decode(raw) {
                Ended::Exit(code) => report(status, 0, code),
                Ended::Signal(signal) => report(status, 1, signal),
            }
        }
        if pid < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            report(status, 0, 125);
        }
    }
}

/// Drops every capability armor holds in its user namespace (as its
/// creator's descendant it has them all there, until an execve): the
/// bounding set, the ambient set, and the effective, permitted and
/// inheritable sets. Nothing armor does in the sandbox needs one.
fn drop_capabilities() -> Result<(), String> {
    let last = std::fs::read_to_string("/proc/sys/kernel/cap_last_cap")
        .ok()
        .and_then(|s| s.trim().parse::<libc::c_ulong>().ok())
        .unwrap_or(63);
    for cap in 0..=last {
        // SAFETY: prctl with PR_CAPBSET_DROP takes a plain number.
        if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) } != 0 {
            return Err(format!(
                "dropping capability {cap}: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    // SAFETY: prctl with these arguments takes plain numbers.
    unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        )
    };
    // struct __user_cap_header_struct and two __user_cap_data_struct
    // (version 3: 64 capability bits).
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let data = [Data {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    // SAFETY: capset reads a valid header and two data structs.
    if unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) } != 0 {
        return Err(format!("capset: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Syscalls armor never makes, which only serve to leave the sandbox or to
/// reach more of the kernel: EPERM (docs/tech/security.md).
const DENIED: [libc::c_long; 36] = [
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_bpf,
    libc::SYS_perf_event_open,
    libc::SYS_keyctl,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_reboot,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_acct,
    libc::SYS_quotactl,
    libc::SYS_open_by_handle_at,
    libc::SYS_name_to_handle_at,
    libc::SYS_fsopen,
    libc::SYS_fsconfig,
    libc::SYS_fsmount,
    libc::SYS_fspick,
    libc::SYS_move_mount,
    libc::SYS_open_tree,
    libc::SYS_mount_setattr,
    libc::SYS_userfaultfd,
    libc::SYS_io_uring_setup,
    libc::SYS_syslog,
];

/// clone flags that make namespaces: EPERM. CLONE_NEWTIME is left out: in
/// clone its bit belongs to the exit signal.
const NAMESPACE_FLAGS: u32 = (libc::CLONE_NEWNS
    | libc::CLONE_NEWCGROUP
    | libc::CLONE_NEWUTS
    | libc::CLONE_NEWIPC
    | libc::CLONE_NEWUSER
    | libc::CLONE_NEWPID
    | libc::CLONE_NEWNET) as u32;

/// The process and all it starts get a seccomp filter: AArch64 syscalls
/// only (anything else kills the process), `DENIED` and clone with
/// `NAMESPACE_FLAGS` refused with EPERM, clone3 with ENOSYS (a filter
/// cannot read its flags; glibc then falls back to clone). Applied last,
/// after the root is built.
fn seccomp() -> Result<(), String> {
    drop_capabilities()?;
    const LD_W_ABS: u16 = 0x20;
    const JEQ_K: u16 = 0x15;
    const JSET_K: u16 = 0x45;
    const RET_K: u16 = 0x06;
    const RET_KILL_PROCESS: u32 = 0x8000_0000;
    const RET_ERRNO: u32 = 0x0005_0000;
    const RET_ALLOW: u32 = 0x7fff_0000;
    const AUDIT_ARCH_AARCH64: u32 = 0xc000_00b7;
    // struct seccomp_data: nr at 0, arch at 4, args from 16 (little-endian).
    const NR: u32 = 0;
    const ARCH: u32 = 4;
    const ARG0_LOW: u32 = 16;
    let op = |code, jt, jf, k| libc::sock_filter { code, jt, jf, k };
    let mut program = vec![
        op(LD_W_ABS, 0, 0, ARCH),
        op(JEQ_K, 1, 0, AUDIT_ARCH_AARCH64),
        op(RET_K, 0, 0, RET_KILL_PROCESS),
        op(LD_W_ABS, 0, 0, NR),
    ];
    for nr in DENIED {
        program.push(op(JEQ_K, 0, 1, nr as u32));
        program.push(op(RET_K, 0, 0, RET_ERRNO | libc::EPERM as u32));
    }
    program.extend([
        op(JEQ_K, 0, 1, libc::SYS_clone3 as u32),
        op(RET_K, 0, 0, RET_ERRNO | libc::ENOSYS as u32),
        op(JEQ_K, 0, 3, libc::SYS_clone as u32),
        op(LD_W_ABS, 0, 0, ARG0_LOW),
        op(JSET_K, 0, 1, NAMESPACE_FLAGS),
        op(RET_K, 0, 0, RET_ERRNO | libc::EPERM as u32),
        op(RET_K, 0, 0, RET_ALLOW),
    ]);
    let fprog = libc::sock_fprog {
        len: program.len() as u16,
        filter: program.as_mut_ptr(),
    };
    // SAFETY: prctl with these options reads only `fprog` and the filter
    // it points to, both alive for the call. No new privileges is required
    // for an unprivileged filter, and makes setuid binaries harmless.
    let installed = unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0
            && libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &fprog as *const libc::sock_fprog,
            ) == 0
    };
    if installed {
        Ok(())
    } else {
        Err(format!("seccomp: {}", std::io::Error::last_os_error()))
    }
}

/// Sends how the guest ended to the supervisor and exits; the processes
/// left in the namespace die with the init.
fn report(status: OwnedFd, kind: i32, value: i32) -> ! {
    let mut message = [0u8; 8];
    message[..4].copy_from_slice(&kind.to_le_bytes());
    message[4..].copy_from_slice(&value.to_le_bytes());
    let _ = std::fs::File::from(status).write_all(&message);
    std::process::exit(0)
}

fn decode(raw: i32) -> Ended {
    if libc::WIFSIGNALED(raw) {
        Ended::Signal(libc::WTERMSIG(raw))
    } else {
        Ended::Exit(libc::WEXITSTATUS(raw))
    }
}

fn wait_for(pid: i32) -> i32 {
    let mut raw = 0;
    // SAFETY: waitpid writes the status through a valid pointer.
    while unsafe { libc::waitpid(pid, &mut raw, 0) } < 0 {
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return 125 << 8;
        }
    }
    raw
}

fn install_forwarding() {
    for signal in FORWARDED {
        // SAFETY: `forward` is an async-signal-safe handler.
        unsafe { libc::signal(signal, forward as *const () as libc::sighandler_t) };
    }
}

fn ignore(signal: i32) {
    // SAFETY: ignoring a signal has no preconditions.
    unsafe { libc::signal(signal, libc::SIG_IGN) };
}

/// The new root (see the module comment), entered with `pivot_root`.
fn build_root(read_write: &[PathBuf], read_only: &[PathBuf], cwd: &Path) -> Result<(), String> {
    // Nothing done here reaches the host's mount namespace.
    mount(None, "/", None, libc::MS_REC | libc::MS_PRIVATE)?;
    // A tmpfs base, entered first so that the host's root (and its /tmp,
    // under the tmpfs) stays reachable at /oldroot while the new root is
    // built in /newroot.
    mount(
        Some("tmpfs"),
        "/tmp",
        Some("tmpfs"),
        libc::MS_NOSUID | libc::MS_NODEV,
    )?;
    chdir("/tmp")?;
    for dir in ["newroot", "oldroot"] {
        std::fs::create_dir(dir).map_err(|e| format!("mkdir {dir}: {e}"))?;
    }
    pivot_root(".", "oldroot")?;
    chdir("/")?;
    // An empty /tmp, then the bound paths, parents first so that nested
    // ones stack correctly.
    std::fs::create_dir_all("/newroot/tmp").map_err(|e| format!("mkdir /tmp: {e}"))?;
    mount(
        Some("tmpfs"),
        "/newroot/tmp",
        Some("tmpfs"),
        libc::MS_NOSUID | libc::MS_NODEV,
    )?;
    let mut binds: Vec<(PathBuf, bool)> = read_only.iter().map(|p| (p.clone(), false)).collect();
    binds.extend(read_write.iter().map(|p| (p.clone(), true)));
    binds.sort_by_key(|(p, _)| p.components().count());
    for (path, writable) in &binds {
        bind(path, *writable)?;
    }
    for device in DEVICES {
        if Path::new("/oldroot").join(&device[1..]).exists() {
            bind(Path::new(device), true)?;
        }
    }
    std::fs::create_dir_all("/newroot/proc").map_err(|e| format!("mkdir /proc: {e}"))?;
    mount(
        Some("proc"),
        "/newroot/proc",
        Some("proc"),
        libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
    )?;
    // Into the new root; the old one goes away.
    mount(
        Some("/newroot"),
        "/newroot",
        None,
        libc::MS_BIND | libc::MS_REC,
    )?;
    chdir("/newroot")?;
    pivot_root(".", ".")?;
    umount_detach(".")?;
    chdir(cwd.to_str().ok_or("the current directory is not UTF-8")?)?;
    Ok(())
}

/// Binds the host's `path` (under /oldroot) at the same path in the new
/// root, read-only unless `writable`. A symbolic link is recreated as a
/// link, and its target bound too.
fn bind(path: &Path, writable: bool) -> Result<(), String> {
    let relative = path
        .strip_prefix("/")
        .map_err(|_| format!("{}: not absolute", path.display()))?;
    let source = Path::new("/oldroot").join(relative);
    let target = Path::new("/newroot").join(relative);
    let meta =
        std::fs::symlink_metadata(&source).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    if meta.file_type().is_symlink() {
        let link = std::fs::read_link(&source).map_err(|e| format!("{}: {e}", path.display()))?;
        if std::fs::symlink_metadata(&target).is_err() {
            std::os::unix::fs::symlink(&link, &target)
                .map_err(|e| format!("symlink {}: {e}", target.display()))?;
        }
        let resolved = path.parent().unwrap_or(Path::new("/")).join(&link);
        let resolved = normalize(&resolved);
        return bind(&resolved, writable);
    }
    if meta.is_dir() {
        std::fs::create_dir_all(&target).map_err(|e| format!("mkdir {}: {e}", target.display()))?;
    } else if std::fs::symlink_metadata(&target).is_err() {
        std::fs::File::create(&target).map_err(|e| format!("create {}: {e}", target.display()))?;
    }
    let target_str = target.to_str().ok_or("path is not UTF-8")?;
    mount(
        source.to_str(),
        target_str,
        None,
        libc::MS_BIND | libc::MS_REC,
    )?;
    if !writable {
        // A remount must keep the flags the kernel locked on the source
        // mount (nosuid, nodev, noexec), or it fails in a user namespace.
        let locked = locked_flags(target_str)?;
        mount(
            None,
            target_str,
            None,
            libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY | locked,
        )?;
    }
    Ok(())
}

/// `path` with `.` and `..` resolved lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::Normal(name) => out.push(name),
            _ => {}
        }
    }
    out
}

fn locked_flags(path: &str) -> Result<libc::c_ulong, String> {
    let c = cstring(path)?;
    // SAFETY: statvfs is plain data, valid when zeroed.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a valid pointer to the struct.
    let result = unsafe { libc::statvfs(c.as_ptr(), &mut stat) };
    if result != 0 {
        return Err(format!(
            "statvfs {path}: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut flags = 0;
    for (st, ms) in [
        (libc::ST_NOSUID, libc::MS_NOSUID),
        (libc::ST_NODEV, libc::MS_NODEV),
        (libc::ST_NOEXEC, libc::MS_NOEXEC),
        (libc::ST_NOATIME, libc::MS_NOATIME),
        (libc::ST_NODIRATIME, libc::MS_NODIRATIME),
        (libc::ST_RELATIME, libc::MS_RELATIME),
    ] {
        if stat.f_flag & st != 0 {
            flags |= ms;
        }
    }
    Ok(flags)
}

fn cstring(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| format!("{s:?} contains a NUL byte"))
}

fn mount(
    source: Option<&str>,
    target: &str,
    fstype: Option<&str>,
    flags: libc::c_ulong,
) -> Result<(), String> {
    let source_c = source.map(cstring).transpose()?;
    let target_c = cstring(target)?;
    let fstype_c = fstype.map(cstring).transpose()?;
    let ptr = |c: &Option<CString>| c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    // SAFETY: every pointer is null or a NUL-terminated string alive for
    // the call; no data argument.
    let result = unsafe {
        libc::mount(
            ptr(&source_c),
            target_c.as_ptr(),
            ptr(&fstype_c),
            flags,
            std::ptr::null(),
        )
    };
    if result != 0 {
        return Err(format!(
            "mount {} on {target}: {}",
            source.unwrap_or("-"),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn pivot_root(new_root: &str, put_old: &str) -> Result<(), String> {
    let (new_c, old_c) = (cstring(new_root)?, cstring(put_old)?);
    // SAFETY: two NUL-terminated paths alive for the call.
    if unsafe { libc::syscall(libc::SYS_pivot_root, new_c.as_ptr(), old_c.as_ptr()) } != 0 {
        return Err(format!("pivot_root: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

fn umount_detach(path: &str) -> Result<(), String> {
    let c = cstring(path)?;
    // SAFETY: a NUL-terminated path alive for the call.
    if unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) } != 0 {
        return Err(format!(
            "umount {path}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn chdir(path: &str) -> Result<(), String> {
    std::env::set_current_dir(path).map_err(|e| format!("chdir {path}: {e}"))
}

fn write_file(path: &str, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))
}

fn pipe() -> Result<(OwnedFd, OwnedFd), String> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 writes two descriptors into a valid array.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(format!("pipe: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: both descriptors were just created and are owned here.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// A resource limit the kernel enforces (`setrlimit`, ADR-0027).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// Open file descriptors (RLIMIT_NOFILE): EMFILE beyond.
    Files,
    /// Processes and threads of the user, counted per user namespace
    /// (RLIMIT_NPROC): EAGAIN from fork and clone beyond.
    Processes,
    /// Size of a file written (RLIMIT_FSIZE): SIGXFSZ beyond.
    FileSize,
    /// CPU time in seconds (RLIMIT_CPU): SIGXCPU at the limit, SIGKILL
    /// one second later (the hard limit; with both equal the kernel sends
    /// SIGKILL at once).
    Cpu,
}

/// Sets `limit` to `value` (soft and hard: the guest cannot raise it).
pub fn set_limit(limit: Limit, value: u64) -> Result<(), String> {
    let resource = match limit {
        Limit::Files => libc::RLIMIT_NOFILE,
        Limit::Processes => libc::RLIMIT_NPROC,
        Limit::FileSize => libc::RLIMIT_FSIZE,
        Limit::Cpu => libc::RLIMIT_CPU,
    };
    let hard = if limit == Limit::Cpu {
        value.saturating_add(1)
    } else {
        value
    };
    let rlimit = libc::rlimit {
        rlim_cur: value,
        rlim_max: hard,
    };
    // SAFETY: setrlimit reads a valid struct.
    if unsafe { libc::setrlimit(resource, &rlimit) } != 0 {
        return Err(format!(
            "{limit:?} limit: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// The read-only paths armor itself needs inside the sandbox: its binary
/// (execve starts it again for x86-64 children) and the host's shared
/// libraries it and its loader live in.
pub fn armor_paths(armor_exe: &Path) -> Vec<PathBuf> {
    let mut paths = vec![armor_exe.to_path_buf()];
    for dir in ["/lib", "/usr/lib", "/lib64"] {
        if Path::new(dir).exists() {
            paths.push(PathBuf::from(dir));
        }
    }
    paths
}
