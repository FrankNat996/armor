//! armor: run Linux x86_64 programs on Linux AArch64.
//!
//! Usage:
//! - `armor [--jit|--interpreter|--lockstep] [--root DIR] [--argv0 NAME]
//!   <program> [args...]`: run a program, choosing the path from the file
//!   (ADR-0028): an x86-64 program runs translated to AArch64 code where
//!   the JIT can (armor_jit, ADR-0021; the default, or `--jit`), else
//!   interpreted (`--interpreter` for the interpreter alone); a native
//!   AArch64 program runs directly; a `#!` script runs its interpreter,
//!   under armor if that is x86-64; anything else is refused with a
//!   suggestion. `--lockstep` is the JIT checking every translated
//!   instruction against the interpreter, for debugging
//!   (armor_jit::lockstep). Dynamically linked programs get their x86_64 loader and
//!   libraries from the guest root (`--root`, else `ARMOR_ROOT`, see
//!   armor_linux::root). `--argv0` sets the guest's `argv[0]` (default:
//!   the program path); armor's `execve` uses both. The guest's exit status
//!   becomes armor's; a guest fault ends armor with the same signal the
//!   kernel would send. armor's own errors exit with 125
//!   (docs/conventions.md).
//! - `armor inspect <file>`: describe a file and say whether armor can
//!   load it (`inspect.rs`). A guest program named `inspect` runs with
//!   `armor ./inspect`.
//! - `armor install [--name NAME] <file.exe|file.msi|file.deb|file.AppImage|
//!   file.tar.*|steam> [-- arguments]`, `armor run <application>
//!   [arguments]`, `armor list`, `armor remove <application>`,
//!   `armor engine fetch`: x86 Windows and Linux programs and Steam
//!   installed in a place of their own, with menu entries (ADR-0033,
//!   `apps.rs`, `steam.rs`); `armor update [--check]`: armor updates itself
//!   from its releases (`update.rs`). Guest programs with these names run
//!   as `armor ./run` and so on.

#![forbid(unsafe_code)]

mod apps;
mod debs;
mod diagnose;
mod engines;
mod gil;
mod inspect;
mod limits;
mod profile;
mod shortcuts;
mod steam;
mod update;

use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::mpsc;

use armor_cpu::{Cpu, RAX, Step};
use armor_jit::Jit;
use armor_linux::root::{GuestRoot, ROOT_VARIABLE};
use armor_linux::signal::{self, Delivered};
use armor_linux::{
    EAGAIN, EINTR, current_tid, loader, process, reset_signal_dispositions, syscall,
    terminate_by_signal,
};
use armor_mem::{AddressSpace, GuestMemory};
use armor_x86::Mode;

/// Exit status for armor's own errors (bad usage, unloadable program,
/// unsupported instruction or syscall), as opposed to the guest's status.
const EXIT_ARMOR_ERROR: u8 = 125;

/// Set in armor's environment: print the JIT's translation counters at
/// exit (`armor_jit::stats`).
const JIT_STATS_VARIABLE: &str = "ARMOR_JIT_STATS";

/// Set in armor's environment: run one guest thread at a time even in
/// concurrent mode (the GIL of ADR-0018), to isolate a problem.
const GIL_VARIABLE: &str = "ARMOR_GIL";

/// What the sandbox lets the guest reach (ADR-0027): `--allow` read-write
/// (the current directory is added by `sandbox::enter`), and read-only the
/// guest root, the program, armor and its libraries, and `--allow-ro`. A
/// `--diagnose` file is created first and bound read-write, so that the
/// report can be written from inside.
fn sandbox_config(
    program: &OsString,
    root: &GuestRoot,
    diagnose_arg: &Option<OsString>,
    allow: &[OsString],
    allow_ro: &[OsString],
) -> Result<armor_linux::sandbox::Config, String> {
    let absolute = |p: &OsStr| fs::canonicalize(p).map_err(|e| format!("{}: {e}", p.display()));
    let mut config = armor_linux::sandbox::Config::default();
    for path in allow {
        config.read_write.push(absolute(path)?);
    }
    if let Some(file) = diagnose_arg
        .as_ref()
        .and_then(|a| a.as_bytes().strip_prefix(b"--diagnose="))
    {
        let file = OsStr::from_bytes(file);
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        config.read_write.push(absolute(file)?);
    }
    let armor = env::current_exe().map_err(|e| format!("armor's own path: {e}"))?;
    config
        .read_only
        .extend(armor_linux::sandbox::armor_paths(&armor));
    config.read_only.push(absolute(program)?);
    if let Some(dir) = root.dir() {
        config.read_only.push(absolute(dir.as_os_str())?);
    }
    for path in allow_ro {
        config.read_only.push(absolute(path)?);
    }
    Ok(config)
}

/// How `armor <program>` runs it (ADR-0028).
enum ExecPath {
    /// Under armor: an x86-64 or i386 program, or the interpreter of a
    /// script (such a program) with the script's arguments.
    Armor {
        program: OsString,
        file: Vec<u8>,
        argv0: OsString,
        args: Vec<OsString>,
    },
    /// Directly on the host: a native AArch64 program, or a script whose
    /// interpreter is not x86-64.
    Native { path: OsString, argv: Vec<OsString> },
}

/// The execution path for `program`, or armor's exit code after saying
/// why it cannot run it and what to do.
fn choose_path(
    program: &OsString,
    argv0: OsString,
    args: &[OsString],
    root: &GuestRoot,
) -> Result<ExecPath, ExitCode> {
    let file = match fs::read(program) {
        Ok(file) => file,
        Err(e) => {
            return Err(armor_error(format_args!(
                "cannot read {}: {e}",
                program.display()
            )));
        }
    };
    if process::is_guest_elf(&file) {
        return Ok(ExecPath::Armor {
            program: program.clone(),
            file,
            argv0,
            args: args.to_vec(),
        });
    }
    let native = |what: &str| {
        eprintln!(
            "armor: {} {what}: running it directly on the host",
            program.display()
        );
        let argv = std::iter::once(argv0.clone()).chain(args.iter().cloned());
        Ok(ExecPath::Native {
            path: program.clone(),
            argv: argv.collect(),
        })
    };
    if let Some((interpreter, optional)) = process::script_interpreter(&file) {
        if process::is_host_system_script(program.as_bytes(), &root.resolve(program.as_bytes())) {
            return native("is a script of the host system");
        }
        let host = root.resolve(&interpreter);
        let interpreter_file = fs::read(OsStr::from_bytes(&host)).unwrap_or_default();
        if !process::is_guest_elf(&interpreter_file) {
            return native("is a script whose interpreter is not an x86-64 program");
        }
        // The kernel's arguments for a script: interpreter, its optional
        // argument, the script path, then the script's arguments.
        let rest = optional
            .into_iter()
            .map(OsString::from_vec)
            .chain([program.clone()])
            .chain(args.iter().cloned());
        return Ok(ExecPath::Armor {
            program: OsString::from_vec(host),
            file: interpreter_file,
            argv0: OsString::from_vec(interpreter),
            args: rest.collect(),
        });
    }
    let report = inspect::inspect(&file);
    if report.facts.iter().any(|(_, v)| v.starts_with("AArch64"))
        && report.reason.starts_with("native AArch64")
    {
        return native("is a native AArch64 program");
    }
    eprintln!("armor: cannot run {}: {}", program.display(), report.reason);
    eprintln!("armor: suggestion: {}", inspect::suggestion(&file, &report));
    Err(ExitCode::from(EXIT_ARMOR_ERROR))
}

fn main() -> ExitCode {
    // A bug in armor (a panic, in any guest thread) ends the whole guest
    // as an armor error, after the usual panic message: a panicking guest
    // thread must not leave the others running.
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report(info);
        std::process::exit(i32::from(EXIT_ARMOR_ERROR));
    }));
    // ARMOR_JIT_STATS=interpreted also counts the instructions left to the
    // interpreter, by kind (docs/tech/performance.md).
    if env::var_os(JIT_STATS_VARIABLE).is_some_and(|v| v == "interpreted") {
        armor_jit::stats::count_interpreted();
    }
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "inspect") {
        return match &args[1..] {
            [path] => inspect_file(path),
            _ => armor_error(format_args!("usage: armor inspect <file>")),
        };
    }
    if let Some((command, tail)) = args.split_first()
        && let Some(code) = apps::main(command, tail)
    {
        return code;
    }
    let mut rest = &args[..];
    // Options given on the command line; a profile fills the others.
    let (mut root_arg, mut argv0, mut engine) = (None, None, None);
    let (mut diagnose_arg, mut profile_name) = (None, None);
    let (mut sandbox, mut allow_net) = (None, None);
    let mut host_gl = false;
    let (mut allow, mut allow_ro): (Vec<OsString>, Vec<OsString>) = (Vec::new(), Vec::new());
    let mut limit_args: Vec<(String, String)> = Vec::new();
    loop {
        match rest {
            [flag, tail @ ..] if flag.as_bytes().starts_with(b"--diagnose") => {
                diagnose_arg = Some(flag.clone());
                rest = tail;
            }
            [flag, tail @ ..] if flag == "--jit" => {
                engine = Some(profile::Engine::Jit);
                rest = tail;
            }
            [flag, tail @ ..] if flag == "--interpreter" => {
                engine = Some(profile::Engine::Interpreter);
                rest = tail;
            }
            [flag, tail @ ..] if flag == "--lockstep" => {
                engine = Some(profile::Engine::Lockstep);
                rest = tail;
            }
            [flag, tail @ ..] if flag == "--host-gl" => {
                host_gl = true;
                rest = tail;
            }
            [flag, tail @ ..] if flag == "--sandbox" => {
                sandbox = Some(true);
                rest = tail;
            }
            [flag, tail @ ..] if flag == "--allow-net" => {
                allow_net = Some(true);
                rest = tail;
            }
            [flag, value, tail @ ..] if flag == "--allow" => {
                allow.push(value.clone());
                rest = tail;
            }
            [flag, value, tail @ ..] if flag == "--allow-ro" => {
                allow_ro.push(value.clone());
                rest = tail;
            }
            [flag, value, tail @ ..]
                if flag
                    .to_str()
                    .and_then(|f| f.strip_prefix("--"))
                    .is_some_and(|f| limits::OPTIONS.contains(&f)) =>
            {
                let name = flag.to_string_lossy()[2..].to_string();
                limit_args.push((name, value.to_string_lossy().into_owned()));
                rest = tail;
            }
            [flag, value, tail @ ..] if flag == "--profile" => {
                profile_name = Some(value.to_string_lossy().into_owned());
                rest = tail;
            }
            [flag, value, tail @ ..] if flag == "--root" => {
                root_arg = Some(value.clone());
                rest = tail;
            }
            [flag, value, tail @ ..] if flag == "--argv0" => {
                argv0 = Some(value.clone());
                rest = tail;
            }
            _ => break,
        }
    }
    let Some((program, guest_args)) = rest.split_first() else {
        eprintln!(
            "usage: armor [--jit|--interpreter|--lockstep] [--root DIR] [--argv0 NAME] \
             [--profile NAME] [--diagnose[=FILE]] [--host-gl] [--sandbox [--allow PATH] [--allow-ro PATH] \
             [--allow-net]] [--limit-files N] [--limit-processes N] [--limit-file-size SIZE] \
             [--limit-cpu SECONDS] [--limit-memory SIZE] <program> [args...]"
        );
        eprintln!("       armor inspect <file>");
        return ExitCode::from(EXIT_ARMOR_ERROR);
    };
    let profiles = profile::dir(
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    );
    let profile = match profile::find(
        profiles.as_deref(),
        profile_name.as_deref(),
        std::path::Path::new(program),
    ) {
        Ok(profile) => profile,
        Err(e) => return armor_error(format_args!("{e}")),
    };
    if let Some(p) = &profile {
        engine = engine.or(p.engine);
        root_arg = root_arg.or_else(|| p.root.clone().map(OsString::from));
        argv0 = argv0.or_else(|| p.argv0.clone().map(OsString::from));
        sandbox = sandbox.or(p.sandbox);
        allow_net = allow_net.or(p.allow_net);
        host_gl = host_gl || p.host_gl == Some(true);
        allow.extend(p.allow.iter().map(OsString::from));
        for (option, value) in &p.limits {
            if !limit_args.iter().any(|(o, _)| o == option) {
                limit_args.push((option.clone(), value.clone()));
            }
        }
        allow_ro.extend(p.allow_ro.iter().map(OsString::from));
        diagnose_arg = diagnose_arg.or_else(|| {
            p.diagnose.as_ref().map(|d| match d.as_str() {
                "stderr" => OsString::from("--diagnose"),
                file => OsString::from(format!("--diagnose={file}")),
            })
        });
    }
    // The JIT by default (ADR-0028).
    let (jit, lockstep) = match engine.unwrap_or(profile::Engine::Jit) {
        profile::Engine::Jit => (true, false),
        profile::Engine::Interpreter => (false, false),
        profile::Engine::Lockstep => (true, true),
    };
    let root = match root_arg {
        Some(dir) if dir.is_empty() => GuestRoot::none(),
        Some(dir) => GuestRoot::new(dir),
        None => GuestRoot::from_env(env::var_os(ROOT_VARIABLE)),
    };
    // `--host-gl` (ADR-0041): the guest's libGL.so.1 is armor's stubs. The
    // sandbox has neither the X server's socket nor the GPU's devices.
    if host_gl && sandbox == Some(true) {
        return armor_error(format_args!(
            "--host-gl and --sandbox do not go together: the sandbox has no X server \
             and no GPU to draw on"
        ));
    }
    let root = if host_gl {
        match host_gl_stubs() {
            Some(stubs) => root.with_host_gl(stubs),
            None => {
                return armor_error(format_args!(
                    "--host-gl: armor's x86-64 OpenGL stubs (libGL.so.1) are not installed \
                     (ARMOR_HOST_GL_STUBS, or lib/armor/x86_64-linux-gnu next to armor)"
                ));
            }
        }
    } else {
        root
    };
    let mut resource_limits = Vec::new();
    for (option, value) in &limit_args {
        match limits::parse(option, value) {
            Ok(n) => resource_limits.push((option.clone(), n)),
            Err(e) => return armor_error(format_args!("--{e}")),
        }
    }
    if sandbox == Some(true) {
        let config = match sandbox_config(program, &root, &diagnose_arg, &allow, &allow_ro) {
            Ok(mut config) => {
                config.network = allow_net == Some(true);
                config
            }
            Err(e) => return armor_error(format_args!("--sandbox: {e}")),
        };
        if let Err(e) = armor_linux::sandbox::enter(&config) {
            return armor_error(format_args!("--sandbox: {e}"));
        }
    } else if allow_net.is_some() || !allow.is_empty() || !allow_ro.is_empty() {
        return armor_error(format_args!(
            "--allow, --allow-ro and --allow-net only make sense with --sandbox"
        ));
    }
    if let Err(e) = limits::apply_kernel(&resource_limits) {
        return armor_error(format_args!("{e}"));
    }
    // The seccomp filters of the program that exec'd this one (ADR-0044):
    // armor's, not the guest's environment.
    let seccomp_fd = env::var(armor_linux::seccomp::EXEC_VARIABLE).ok();
    let env: Vec<Vec<u8>> = env::vars_os()
        .filter(|(key, _)| key != armor_linux::seccomp::EXEC_VARIABLE)
        .map(|(key, value)| [key.as_bytes(), b"=", value.as_bytes()].concat())
        .collect();
    let env = match &profile {
        Some(p) => profile::apply_env(p, env),
        None => env,
    };
    let env_bytes: Vec<&[u8]> = env.iter().map(Vec::as_slice).collect();
    if let Some(arg) = &diagnose_arg {
        let target = match arg.as_bytes().strip_prefix(b"--diagnose") {
            Some(b"") => diagnose::Target::Stderr,
            Some([b'=', file @ ..]) if !file.is_empty() => {
                diagnose::Target::File(OsStr::from_bytes(file).into())
            }
            _ => {
                return armor_error(format_args!(
                    "unknown option {} (--diagnose or --diagnose=FILE)",
                    arg.display()
                ));
            }
        };
        diagnose::enable(target, &program.to_string_lossy());
        if let Some(p) = &profile {
            diagnose::profile(&p.name);
        }
    }
    let argv0 = argv0.unwrap_or_else(|| program.clone());
    let path = match choose_path(program, argv0, guest_args, &root) {
        Ok(path) => path,
        Err(code) => {
            diagnose::path("refused", false);
            diagnose::report(diagnose::Ending::ArmorError);
            return code;
        }
    };
    let (program, file, argv0, guest_args) = match path {
        ExecPath::Armor {
            program,
            file,
            argv0,
            args,
        } => (program, file, argv0, args),
        ExecPath::Native { path, argv } => {
            diagnose::path("native program, run directly on the host", false);
            diagnose::report(diagnose::Ending::Native);
            let argv: Vec<Vec<u8>> = argv.into_iter().map(OsString::into_vec).collect();
            let errno = process::exec_native(path.as_bytes(), &argv, &env);
            return armor_error(format_args!(
                "cannot run {} natively: {}",
                path.display(),
                std::io::Error::from_raw_os_error(-errno as i32)
            ));
        }
    };
    armor_linux::limit_core_dumps();
    let program = &program;
    let arg_bytes: Vec<&[u8]> = std::iter::once(&argv0)
        .chain(&guest_args)
        .map(|a| a.as_bytes())
        .collect();
    let mem = AddressSpace::new();
    limits::apply_memory(&resource_limits, &mem);
    let image = match loader::load(
        &mem,
        &file,
        &arg_bytes,
        &env_bytes,
        program.as_bytes(),
        &root,
    ) {
        Ok(image) => image,
        Err(e) => return armor_error(format_args!("cannot load {}: {e}", program.display())),
    };

    // /proc/self/exe must name the guest program, not armor.
    let exe_path = fs::canonicalize(program)
        .map(|p| p.into_os_string().into_vec())
        .unwrap_or_else(|_| program.as_bytes().to_vec());
    let armor_exe = match env::current_exe() {
        Ok(path) => path,
        Err(e) => return armor_error(format_args!("cannot find armor's own path: {e}")),
    };
    let mut process = syscall::Process::new(exe_path, image.brk_start, root, armor_exe);
    process.jit = jit;
    process.lockstep = lockstep;
    process.diagnose = diagnose_arg.map(OsString::into_vec);
    let i386 = image.mode == Mode::Compat32;
    let engine = if lockstep {
        "lockstep"
    } else if jit {
        "JIT"
    } else {
        "interpreter"
    };
    let architecture = if i386 { "i386" } else { "x86-64" };
    diagnose::path(&format!("{architecture} under armor, {engine}"), jit);
    syscall::set_process_name(program.as_bytes());
    reset_signal_dispositions();

    // Like exec, the guest starts with armor's signal mask and ignored
    // signals.
    let mut thread = syscall::Thread::default();
    thread.signals.inherit_from_host();
    if let Some(filters) = seccomp_fd
        .as_deref()
        .and_then(armor_linux::seccomp::inherited)
    {
        thread.seccomp = filters;
    }

    // Shared by every guest thread for the rest of armor's life.
    // The GIL stays for lockstep, whose run-twice-and-compare scheme needs
    // one runner at a time, and with ARMOR_GIL set (ADR-0023).
    let always_serialize = lockstep || env::var_os(GIL_VARIABLE).is_some();
    let guest: &'static Guest = Box::leak(Box::new(Guest {
        mem,
        process,
        gil: gil::Gil::new(),
        always_serialize,
    }));
    let mut cpu = match image.mode {
        Mode::Long => Cpu::new(image.entry, image.stack_pointer),
        Mode::Compat32 => Cpu::new_compat(image.entry, image.stack_pointer),
    };
    if always_serialize {
        armor_linux::process::set_fork_hooks(&guest.gil);
    }
    run_thread(guest, &mut cpu, &mut thread, true);
    ExitCode::SUCCESS
}

/// What the guest's threads share (ADR-0018, ADR-0023).
struct Guest {
    mem: AddressSpace,
    process: syscall::Process,
    /// With `always_serialize` (lockstep, `ARMOR_GIL`), only the thread
    /// holding it runs guest code.
    gil: gil::Gil,
    always_serialize: bool,
}

/// Runs one guest thread until it exits. Errors that end the whole guest
/// (an unsupported instruction or syscall, a fatal signal) end armor from
/// here.
fn run_thread(guest: &'static Guest, cpu: &mut Cpu, thread: &mut syscall::Thread, main: bool) {
    let (mem, process) = (&guest.mem, &guest.process);
    // Each thread translates for itself (armor_jit::engine).
    let mut jit = process.jit.then(|| {
        let jit = if process.lockstep {
            Jit::lockstep()
        } else {
            Jit::new()
        };
        jit.unwrap_or_else(|e| {
            fatal_armor_error(format_args!("cannot allocate the JIT's code buffer: {e}"))
        })
    });
    let mut turns = Turns::new(current_tid());
    loop {
        // The syscall that returned EINTR, if any: a signal handler with
        // SA_RESTART may restart it.
        let mut interrupted = None;
        let step = {
            // In concurrent mode guest threads run in parallel, with the
            // accesses ordered like x86 (ADR-0023). Before it there is only
            // one guest thread (a second one turns concurrent mode on before
            // it starts), so there is nothing to serialize: taking the GIL
            // cost a futex wake per syscall.
            let _turn = guest.always_serialize.then(|| guest.gil.lock());
            let budget = if guest.always_serialize {
                turns.next()
            } else {
                SIGNAL_CHECK_INTERVAL
            };
            // One page-table lock for the whole block of instructions.
            let view = mem.view();
            match &mut jit {
                Some(jit) => jit.run(cpu, &view, budget),
                None => cpu.run(&view, budget),
            }
        };
        match step {
            Ok(Step::Syscall) if thread.signals.has_deliverable() => {
                // A signal arrived before the syscall: deliver it first and
                // run the syscall after the handler, as Linux would. Without
                // this, a blocking syscall could wait for a signal already
                // taken.
                cpu.rip = cpu.rip.wrapping_sub(2);
            }
            Ok(Step::Syscall) => {
                let number = cpu.regs[RAX];
                match syscall::dispatch(cpu, mem, process, thread) {
                    syscall::Outcome::Continue => {
                        let result = syscall::result(cpu);
                        if result == -i64::from(EINTR) {
                            interrupted = Some(number);
                        } else if result == -i64::from(armor_linux::ENOSYS) {
                            diagnose::enosys(number);
                        }
                    }
                    syscall::Outcome::Exit(status) => exit_process(status),
                    syscall::Outcome::ExitThread(status) => {
                        exit_thread(guest, thread, status, main);
                        return;
                    }
                    syscall::Outcome::NewThread(new) => start_thread(guest, cpu, *new),
                    syscall::Outcome::Unsupported(what) => {
                        fatal_armor_error(format_args!(
                            "{what} is not supported yet (rip {:#x})",
                            cpu.rip
                        ));
                    }
                    syscall::Outcome::Fatal(signal, why) => {
                        eprintln!("armor: {why}");
                        terminate(signal);
                    }
                }
            }
            Ok(Step::Continue) => {}
            Err(error) => match signal::fault_signal(&error, cpu, mem) {
                Some(fault) => {
                    diagnose::fault(fault.signal as i32);
                    match thread.signals.deliver(cpu, mem, &fault, true) {
                        Delivered::Handler | Delivered::Ignored => {}
                        Delivered::Terminate(signal) => {
                            eprintln!("armor: guest {error}");
                            terminate(signal);
                        }
                    }
                }
                None => fatal_armor_error(format_args!("{error}")),
            },
        }
        while let Some(pending) = thread.signals.take_pending() {
            if let Some(number) = interrupted.take()
                && thread.signals.restarts(pending.signal)
                && syscall::restartable(cpu.mode, number)
            {
                // Back to the syscall instruction, as Linux does.
                cpu.rip = cpu.rip.wrapping_sub(2);
                cpu.regs[RAX] = number;
            }
            match thread.signals.deliver(cpu, mem, &pending, false) {
                Delivered::Handler | Delivered::Ignored => {}
                Delivered::Terminate(signal) => terminate(signal),
            }
        }
        thread.signals.restore_saved_mask();
    }
}

/// clone: starts the new guest thread on a new host thread and returns its
/// tid to the parent. The child runs guest code only after the parent has
/// stored the tid (CLONE_PARENT_SETTID): otherwise a child exiting at once
/// would clear its tid before the parent writes it, and pthread_join would
/// wait forever.
fn start_thread(guest: &'static Guest, parent: &mut Cpu, new: syscall::NewThread) {
    let (tid_sender, tid_receiver) = mpsc::channel();
    let (go_sender, go_receiver) = mpsc::channel::<()>();
    let syscall::NewThread {
        mut cpu,
        mut thread,
        parent_tid,
        child_tid,
    } = new;
    guest.process.live_threads.fetch_add(1, Ordering::SeqCst);
    let spawned = std::thread::Builder::new()
        .name("armor-guest".to_string())
        .spawn(move || {
            let tid = current_tid();
            if tid_sender.send(tid).is_err() || go_receiver.recv().is_err() {
                return;
            }
            if let Some(addr) = child_tid {
                // Like Linux, a failed write is ignored.
                let _ = guest.mem.write(addr, &(tid as u32).to_le_bytes());
            }
            // Mirror the inherited guest mask into this host thread.
            let mask = thread.signals.mask();
            thread.signals.set_mask(mask);
            run_thread(guest, &mut cpu, &mut thread, false);
        });
    let tid = match spawned.map(|_| tid_receiver.recv()) {
        Ok(Ok(tid)) => tid,
        _ => {
            guest.process.live_threads.fetch_sub(1, Ordering::SeqCst);
            // eax only for an i386 guest, whose upper halves stay zero.
            let mask = match parent.mode {
                Mode::Long => u64::MAX,
                Mode::Compat32 => 0xffff_ffff,
            };
            parent.regs[RAX] = (-i64::from(EAGAIN)) as u64 & mask;
            return;
        }
    };
    if let Some(addr) = parent_tid {
        let _ = guest.mem.write(addr, &(tid as u32).to_le_bytes());
    }
    parent.regs[RAX] = tid;
    // The child cannot have exited: it waits for this.
    let _ = go_sender.send(());
}

/// exit(2) of one thread: clears and wakes its CLONE_CHILD_CLEARTID word
/// (as Linux does, for pthread_join), and ends the process if it was the
/// last thread. The main host thread must not return while other threads
/// run, since returning from `main` would end armor.
fn exit_thread(guest: &'static Guest, thread: &syscall::Thread, status: i32, main: bool) {
    let addr = thread.clear_child_tid;
    if addr != 0 && guest.mem.write(addr, &0u32.to_le_bytes()).is_ok() {
        syscall::futex_wake_all(addr);
    }
    if guest.process.live_threads.fetch_sub(1, Ordering::SeqCst) == 1 {
        exit_process(status);
    }
    if main {
        loop {
            std::thread::park();
        }
    }
}

/// The guest process exits with `status`. With `ARMOR_JIT_STATS` set in
/// armor's environment, the JIT's translation counters go to stderr first.
fn exit_process(status: i32) -> ! {
    if env::var_os(JIT_STATS_VARIABLE).is_some() {
        eprintln!("armor: {}", armor_jit::stats::summary());
        if armor_jit::stats::counting_interpreted() {
            eprintln!("armor: {}", armor_jit::stats::interpreted_summary());
        }
    }
    report(diagnose::Ending::Status(status));
    std::process::exit(status)
}

/// Ends armor as the kernel would end the guest for `signal`, after the
/// diagnose report.
fn terminate(signal: i32) -> ! {
    report(diagnose::Ending::Signal(signal));
    terminate_by_signal(signal)
}

/// The `--diagnose` report, with the JIT's counters.
fn report(ending: diagnose::Ending) {
    if diagnose::enabled() {
        diagnose::jit_stats(armor_jit::stats::summary());
        diagnose::report(ending);
    }
}

/// An armor error that ends the guest, from any thread.
fn fatal_armor_error(message: std::fmt::Arguments) -> ! {
    let message = message.to_string();
    eprintln!("armor: {message}");
    if let Some(suggestion) = diagnose::stop_suggestion(&message) {
        eprintln!("armor: suggestion: {suggestion}");
    }
    diagnose::stopped(&message);
    report(diagnose::Ending::ArmorError);
    std::process::exit(i32::from(EXIT_ARMOR_ERROR))
}

/// Instructions between two checks for pending signals: short enough for
/// a prompt delivery, long enough to cost nothing measurable.
const SIGNAL_CHECK_INTERVAL: u64 = 10_000;

/// The length of a thread's turns with the GIL (lockstep, `ARMOR_GIL`):
/// between half and all of `SIGNAL_CHECK_INTERVAL` instructions, varied
/// from turn to turn like the time slices of a real scheduler. With turns
/// of a fixed length, a thread in a loop of fixed length that starts each
/// turn at the same point (after a syscall) also ends it at the same point:
/// in `c/fork_threads` that point was inside a mutex, so every other thread
/// found the mutex taken whenever it had the GIL, and waited for seconds.
struct Turns(u64);

impl Turns {
    fn new(seed: u64) -> Turns {
        // Spread the seed (a tid) over the bits; xorshift needs a state
        // that is not zero.
        Turns(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next(&mut self) -> u64 {
        // xorshift64 (Marsaglia).
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        SIGNAL_CHECK_INTERVAL / 2 + x % (SIGNAL_CHECK_INTERVAL / 2 + 1)
    }
}

/// armor's x86-64 OpenGL stubs (`--host-gl`, ADR-0041): `ARMOR_HOST_GL_STUBS`,
/// else `../lib/armor/x86_64-linux-gnu/libGL.so.1` from armor's directory
/// (the package), else `../guest/libgl/libGL.so.1` (a build in `target/`).
fn host_gl_stubs() -> Option<std::path::PathBuf> {
    if let Some(path) = env::var_os("ARMOR_HOST_GL_STUBS").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(path));
    }
    let exe = env::current_exe().ok()?;
    let dir = exe.parent()?;
    [
        dir.join("../lib/armor/x86_64-linux-gnu/libGL.so.1"),
        dir.join("../guest/libgl/libGL.so.1"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// `armor inspect <file>`: exit 0 once the file was read, whatever the verdict.
fn inspect_file(path: &OsString) -> ExitCode {
    match fs::read(path) {
        Ok(file) => {
            let report = inspect::inspect(&file);
            print!("{}", inspect::render(&path.to_string_lossy(), &report));
            ExitCode::SUCCESS
        }
        Err(e) => armor_error(format_args!("cannot read {}: {e}", path.display())),
    }
}

fn armor_error(message: std::fmt::Arguments) -> ExitCode {
    let message = message.to_string();
    eprintln!("armor: {message}");
    if let Some(suggestion) = diagnose::stop_suggestion(&message) {
        eprintln!("armor: suggestion: {suggestion}");
    }
    diagnose::stopped(&message);
    report(diagnose::Ending::ArmorError);
    ExitCode::from(EXIT_ARMOR_ERROR)
}
