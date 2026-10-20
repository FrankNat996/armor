//! Development tasks for armor. Run with `cargo xtask <command>`.
//!
//! Guest test programs live in `tests/guest/asm/**/*.S` and are built into
//! `target/guest/asm/`. A program is named by its path relative to
//! `tests/guest/asm` without the extension, e.g. `basic/hello`. The i386
//! ones (ADR-0029) are `tests/guest/asm32/**/*.S`, named `i386/<path>`.
//! See docs/testing.md.

mod app_check;
mod bench;
mod busybox;
mod coverage;
mod deb;
mod decoder_check;
mod difftest;
mod distro;
mod encoder_check;
mod fuzz;
mod gl;
mod glgen;
mod oracle;
mod package;
mod progress;
mod results;
mod rootfs;
mod sandbox_check;
mod wow64;

use std::env;
use std::fs;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Output};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  build-guest                    build every guest test program
  run [--armor|--jit|--lockstep] <name> [args...]
                                 build one guest program and run it under
                                 qemu-x86_64 (or armor: --armor interpreted,
                                 --jit with the JIT, --lockstep checking the
                                 JIT against the interpreter)
  difftest [--oracle | --save DIR | --expected DIR]
           [--interpreter|--jit|--lockstep] [name...]
                                 run guest programs, busybox applets and
                                 coreutils under qemu-x86_64 and armor and
                                 compare stdout and exit status (default: all
                                 programs, armor with each engine); --oracle:
                                 a real x86-64 machine instead of qemu, static
                                 programs only (ARMOR_ORACLE, ARMOR_ORACLE_KEY);
                                 --save: also keep qemu's results in DIR;
                                 --expected: compare with results kept in DIR
                                 instead of running qemu (hosts with 16K or
                                 64K pages)
  bench [--runs N] [--interpreter] [name...]
                                 time benchmarks under armor --jit, qemu-x86_64
                                 and natively; print the environment and a
                                 table (docs/tech/performance.md)
  fuzz [--iterations N] [--seed S]
                                 run the fuzz tests of the decoder, the ELF
                                 parser and syscall arguments, long, with a
                                 new seed (docs/testing.md)
  fetch-busybox                  download the pinned Debian busybox-static (amd64)
  fetch-rootfs [--i386|--games]  download Debian amd64 coreutils, grep, sed, gawk,
                                 findutils, gzip, xz, bzip2, tar, diffutils, make,
                                 dash, perl, python and their libraries into the
                                 guest root target/guest/rootfs (--i386: i386
                                 coreutils into target/guest/rootfs-i386;
                                 --games: plus Mesa, X11, SDL2, sound and
                                 libstdc++ into target/guest/rootfs-games)
  decoder-check <elf>...         compare the decoder with objdump on every
                                 instruction of the given executables
  encoder-check                  compare the JIT's AArch64 encoder with the
                                 host's GNU assembler
  sandbox-check                  prove each control of armor --sandbox from
                                 inside, with an x86-64 probe under armor and
                                 a native AArch64 one (docs/tech/security.md)
  progress [--check]             update the completion percentages in README.md
                                 (--check: fail if they are stale instead)
  coverage [--check]             update the decoder and syscall coverage badges
                                 in README.md (needs the x86_64 toolchain)
  package                        build target/package/armor_<version>_arm64.deb
  gl-gen [--check]               generate armor's OpenGL table and the guest's
                                 libGL stubs from third_party/khronos/gl.xml
                                 (--check: fail if they are stale instead)
  wow64                          build armor's WoW64 CPU backend for 32-bit
                                 Windows programs into target/wow64/ (needs
                                 the aarch64-pc-windows-msvc target, lld and
                                 llvm; ADR-0047)
  gl-check                       draw with OpenGL under armor, with the guest's
                                 Mesa and with the host's GPU (--host-gl), and
                                 compare the pixels (needs an X display and
                                 fetch-rootfs --games)
  distro-check                   the package's archive installed and used in
                                 podman containers of Fedora and Arch Linux
                                 ARM: x86-64 and i386 programs, an x86-64
                                 .deb, the Windows engine (ADR-0043)
  app-check [--report] [--x86-engine armor|fex] [id...]
                                 install and run real applications under
                                 Xvfb: a window, or 7-Zip's archive against
                                 its ARM64 build (ADR-0045); --x86-engine
                                 armor: Windows 32-bit code under armor's
                                 WoW64 backend instead of FEX (ADR-0047)
  matrix [--check]               render the matrix in COMPATIBILITY_MATRIX.md
                                 and README.md from docs/results/ (ADR-0045)";

const GUEST_CC: &str = "x86_64-linux-gnu-gcc";
const QEMU: &str = "qemu-x86_64";
/// The i386 cross compiler and oracle (ADR-0029).
const GUEST_CC_I386: &str = "i686-linux-gnu-gcc";
const QEMU_I386: &str = "qemu-i386";
/// The CPU model qemu-i386 emulates: qemu32 with the SSE extensions armor
/// reports (qemu-i386 has no qemu64 with long mode).
const QEMU_I386_CPU: &str = "qemu32,+sse3,+ssse3,+sse4.1,+sse4.2,+popcnt";

/// The CPU model qemu emulates: qemu64 without the features armor does not
/// implement, so that CPUID matches armor's (crates/armor-cpu/src/cpuid.rs).
const QEMU_CPU: &str = "qemu64,-svm,+ssse3,+sse4.1,+sse4.2,+popcnt";

/// Assembly tests use no libc and a fixed load address (ELF type ET_EXEC).
const GUEST_ASM_FLAGS: &[&str] = &["-nostdlib", "-static", "-no-pie"];

/// Every C test (`tests/guest/c/<stem>.c`) is built three ways, named
/// `<prefix><stem>` and written to `target/guest/<prefix><stem>`:
/// - `c/`: static glibc, ET_EXEC on Debian's cross toolchain;
/// - `c-pie/`: static-pie (ET_DYN without an interpreter);
/// - `c-dyn/`: the toolchain default, a dynamically linked PIE that needs
///   the x86_64 ld.so and libc.so.6 from the guest root.
const C_VARIANTS: &[(&str, &[&str])] = &[
    ("c/", &["-static", "-O2", "-Wall"]),
    ("c-pie/", &["-static-pie", "-O2", "-Wall"]),
    ("c-dyn/", &["-O2", "-Wall"]),
];

/// The i386 C tests (ADR-0029), built with `GUEST_CC_I386` from
/// `tests/guest/c32/<stem>.c`, or from `tests/guest/c/<stem>.c` for the
/// tests of `I386_SHARED_C_TESTS`, in the variants `c32/` (static),
/// `c32-pie/` (static-pie) and `c32-dyn/` (dynamic, with the i386 ld.so
/// and libraries of `I386_GUEST_ROOT`).
const I386_C_VARIANTS: &[(&str, &[&str])] = &[
    ("c32/", &["-static", "-O2", "-Wall"]),
    ("c32-pie/", &["-static-pie", "-O2", "-Wall"]),
    ("c32-dyn/", &["-O2", "-Wall"]),
];

/// The guest root of the dynamic i386 tests: Debian's
/// `libc6-i386-cross` installs `ld-linux.so.2` and the i386 glibc there.
const I386_GUEST_ROOT: &str = "/usr/i686-linux-gnu";

/// The guest root a built test program runs with.
fn guest_root(name: &str) -> &'static str {
    if name.starts_with("c32-dyn/") {
        I386_GUEST_ROOT
    } else {
        GUEST_ROOT
    }
}

/// The tests of `tests/guest/c` that also run as i386 programs: those that
/// compile for i386 (the others use `__int128`, 64-bit assembly or
/// `REG_RIP`).
const I386_SHARED_C_TESTS: &[&str] = &[
    "close_range",
    "events",
    "file_locks",
    "fork_threads",
    "hello",
    "malloc",
    "memfd",
    "mremap",
    "mremap_shared",
    "pi_mutex",
    "ppoll_mask",
    "printf_int",
    "processes",
    "sched",
    "scm_rights_truncated",
    "shared_memory",
    "signals_async",
    "sockets",
    "strings",
    "sysv_semaphores",
    "threads",
    "threads_signals",
    "writev",
    "x87_bcd",
    "x87_more",
    "x87_transcendental",
];

/// The guest root of test programs and busybox: qemu gets it with `-L`,
/// armor with `ARMOR_ROOT` (crates/armor-linux/src/root.rs). Debian's
/// `libc6-amd64-cross` installs the x86_64 ld.so and libraries there.
/// coreutils, tools and interpreters run with the root of `fetch-rootfs`
/// instead.
const GUEST_ROOT: &str = "/usr/x86_64-linux-gnu";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("build-guest") => build_all(),
        Some("run") => match &args[1..] {
            [flag, name, rest @ ..] if flag == "--armor" => {
                run(Runner::Under(Engine::Interpreter), name, rest)
            }
            [flag, name, rest @ ..] if flag == "--jit" => {
                run(Runner::Under(Engine::Jit), name, rest)
            }
            [flag, name, rest @ ..] if flag == "--lockstep" => {
                run(Runner::Under(Engine::Lockstep), name, rest)
            }
            [name, rest @ ..] => run(Runner::Qemu, name, rest),
            [] => Err(format!("missing program name\n\n{USAGE}")),
        },
        Some("difftest") => difftest::run(&args[1..]),
        Some("bench") => bench::run(&args[1..]),
        Some("sandbox-check") => sandbox_check::run(),
        Some("fuzz") => fuzz::run(&args[1..]),
        Some("fetch-busybox") => busybox::fetch(),
        Some("fetch-rootfs") => match args.get(1).map(String::as_str) {
            None => rootfs::fetch(rootfs::Arch::Amd64),
            Some("--i386") => rootfs::fetch(rootfs::Arch::I386),
            Some("--games") => rootfs::fetch(rootfs::Arch::Games),
            Some(other) => Err(format!("fetch-rootfs: unknown option {other}")),
        },
        Some("decoder-check") => decoder_check::run(&args[1..]),
        Some("encoder-check") => encoder_check::run(),
        Some("progress") => match &args[1..] {
            [] => progress::run(&Paths::new().root, false),
            [flag] if flag == "--check" => progress::run(&Paths::new().root, true),
            _ => Err(format!("usage: cargo xtask progress [--check]\n\n{USAGE}")),
        },
        Some("package") => package::run(&Paths::new().root),
        Some("gl-gen") => match &args[1..] {
            [] => glgen::run(&Paths::new().root, false),
            [flag] if flag == "--check" => glgen::run(&Paths::new().root, true),
            _ => Err(format!("usage: cargo xtask gl-gen [--check]\n\n{USAGE}")),
        },
        Some("gl-check") => gl::check(&Paths::new()),
        Some("wow64") => wow64::run(&Paths::new().root),
        Some("distro-check") => {
            let mut rest = args[1..].to_vec();
            let report = results::take_flag(&mut rest);
            distro::check(&Paths::new(), report)
        }
        Some("app-check") => app_check::run(&args[1..]),
        Some("matrix") => match &args[1..] {
            [] => results::run(&Paths::new().root, false),
            [flag] if flag == "--check" => results::run(&Paths::new().root, true),
            _ => Err(format!("usage: cargo xtask matrix [--check]\n\n{USAGE}")),
        },
        Some("coverage") => match &args[1..] {
            [] => coverage::run(false),
            [flag] if flag == "--check" => coverage::run(true),
            _ => Err(format!("usage: cargo xtask coverage [--check]\n\n{USAGE}")),
        },
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
        None => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("xtask: {msg}");
            ExitCode::FAILURE
        }
    }
}

struct Paths {
    root: PathBuf,
    asm_src: PathBuf,
    asm_include: PathBuf,
    asm_out: PathBuf,
    c_src: PathBuf,
    asm32_src: PathBuf,
    c32_src: PathBuf,
    guest_out: PathBuf,
    armor: PathBuf,
}

impl Paths {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives inside the workspace root")
            .to_path_buf();
        Paths {
            asm_src: root.join("tests/guest/asm"),
            asm_include: root.join("tests/guest/include"),
            asm_out: root.join("target/guest/asm"),
            c_src: root.join("tests/guest/c"),
            asm32_src: root.join("tests/guest/asm32"),
            c32_src: root.join("tests/guest/c32"),
            guest_out: root.join("target/guest"),
            armor: root.join("target/difftest/armor"),
            root,
        }
    }
}

fn build_all() -> Result<(), String> {
    let paths = Paths::new();
    for name in all_guest_names(&paths)? {
        let out = build_one(&paths, &name)?;
        println!("built {name} -> {}", out.display());
    }
    let libgl = gl::build_libgl(&paths)?;
    println!("built armor's libGL -> {}", libgl.display());
    Ok(())
}

/// Names of every guest program: assembly programs first, then each C
/// program in every variant of `C_VARIANTS`, each group sorted.
fn all_guest_names(paths: &Paths) -> Result<Vec<String>, String> {
    let list = |dir: &Path, ext: &str| {
        find_sources(dir, ext).map_err(|e| format!("cannot list {}: {e}", dir.display()))
    };
    let mut names = Vec::new();
    for src in list(&paths.asm_src, "S")? {
        names.push(
            guest_name(&paths.asm_src, &src)
                .ok_or_else(|| format!("unexpected source path {}", src.display()))?,
        );
    }
    if paths.c_src.is_dir() {
        let sources = list(&paths.c_src, "c")?;
        for (prefix, _) in C_VARIANTS {
            for src in &sources {
                let name = guest_name(&paths.c_src, src)
                    .ok_or_else(|| format!("unexpected source path {}", src.display()))?;
                names.push(format!("{prefix}{name}"));
            }
        }
    }
    for src in list(&paths.asm32_src, "S")? {
        let name = guest_name(&paths.asm32_src, &src)
            .ok_or_else(|| format!("unexpected source path {}", src.display()))?;
        names.push(format!("{I386_PREFIX}{name}"));
    }
    let mut stems: Vec<String> = I386_SHARED_C_TESTS.iter().map(|s| s.to_string()).collect();
    for src in list(&paths.c32_src, "c")? {
        stems.push(
            guest_name(&paths.c32_src, &src)
                .ok_or_else(|| format!("unexpected source path {}", src.display()))?,
        );
    }
    stems.sort();
    for (prefix, _) in I386_C_VARIANTS {
        names.extend(stems.iter().map(|stem| format!("{prefix}{stem}")));
    }
    if names.is_empty() {
        return Err("no guest programs found".to_string());
    }
    Ok(names)
}

/// The name prefix of the i386 assembly tests.
const I386_PREFIX: &str = "i386/";

/// Whether `bin` is an i386 ELF (ELFCLASS32, EM_386), for qemu-i386.
fn is_i386(bin: &Path) -> bool {
    let mut header = [0u8; 20];
    fs::File::open(bin)
        .and_then(|mut f| io::Read::read_exact(&mut f, &mut header))
        .is_ok_and(|()| {
            header.starts_with(b"\x7fELF") && header[4] == 1 && header[18..20] == [3, 0]
        })
}

#[derive(Clone, Copy)]
enum Runner {
    Qemu,
    /// The engine under test.
    Under(Engine),
}

/// What runs the guest code under test: armor in one of its modes, or
/// another emulator measured on the same cases (ADR-0032).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Engine {
    Interpreter,
    Jit,
    /// The JIT, each translated instruction checked against the
    /// interpreter (ADR-0021 §9.2).
    Lockstep,
    /// box64, from `ARMOR_BOX64` (default: `box64` in PATH).
    Box64,
    /// FEX (its loader, `FEX`), from `ARMOR_FEX` (default: in PATH).
    Fex,
}

impl Engine {
    fn flag(self) -> Option<&'static str> {
        match self {
            // The JIT is armor's default (ADR-0028): name the engine always.
            Engine::Interpreter => Some("--interpreter"),
            Engine::Jit => Some("--jit"),
            Engine::Lockstep => Some("--lockstep"),
            Engine::Box64 | Engine::Fex => None,
        }
    }

    /// The program under test, for messages.
    fn program(self) -> &'static str {
        match self {
            Engine::Box64 => "box64",
            Engine::Fex => "FEX",
            _ => "armor",
        }
    }

    fn is_armor(self) -> bool {
        !matches!(self, Engine::Box64 | Engine::Fex)
    }

    fn name(self) -> &'static str {
        match self {
            Engine::Interpreter => "interpreter",
            Engine::Jit => "jit",
            Engine::Lockstep => "lockstep",
            Engine::Box64 => "box64",
            Engine::Fex => "fex",
        }
    }
}

/// Every guest run is limited by coreutils `timeout`: a guest that hangs
/// (a lost wakeup between threads, say) must fail the test, not stop the
/// whole run. `timeout` exits with this status when the limit expires.
const RUN_TIME_LIMIT: &str = "120";
pub const TIMED_OUT_STATUS: i32 = 124;

impl Runner {
    /// A command running `bin` with guest root `root`, under `timeout`.
    fn command(self, paths: &Paths, bin: &Path, root: &Path) -> Command {
        let mut cmd = Command::new("timeout");
        cmd.args(["--kill-after=5", RUN_TIME_LIMIT]);
        match self {
            Runner::Qemu if is_i386(bin) => {
                cmd.arg(QEMU_I386)
                    .args(["-cpu", QEMU_I386_CPU, "-L"])
                    .arg(root);
                cmd.env("QEMU_CPU", QEMU_I386_CPU)
                    .env("QEMU_LD_PREFIX", root);
            }
            Runner::Qemu => {
                cmd.arg(QEMU).args(["-cpu", QEMU_CPU, "-L"]).arg(root);
                // The same CPU and root for the x86_64 children the guest
                // execs: binfmt_misc starts them under a new qemu, which
                // reads these variables (ADR-0019).
                cmd.env("QEMU_CPU", QEMU_CPU).env("QEMU_LD_PREFIX", root);
            }
            Runner::Under(Engine::Box64) => {
                cmd.arg(env::var_os("ARMOR_BOX64").unwrap_or_else(|| "box64".into()));
                // The x86 libraries of the root, for programs linked
                // dynamically (box64 wraps libc and a few others natively).
                let libs = [
                    "usr/lib/x86_64-linux-gnu",
                    "lib/x86_64-linux-gnu",
                    "usr/lib/i386-linux-gnu",
                    "lib/i386-linux-gnu",
                    "lib",
                ]
                .map(|dir| root.join(dir).display().to_string())
                .join(":");
                cmd.env("BOX64_LD_LIBRARY_PATH", &libs)
                    .env("BOX64_NOBANNER", "1")
                    .env("BOX64_LOG", "0");
            }
            Runner::Under(Engine::Fex) => {
                cmd.arg(env::var_os("ARMOR_FEX").unwrap_or_else(|| "FEX".into()));
                cmd.env("FEX_ROOTFS", root);
            }
            Runner::Under(engine) => {
                cmd.arg(&paths.armor).env("ARMOR_ROOT", root);
                cmd.args(engine.flag());
            }
        };
        cmd.arg(bin);
        cmd
    }
}

fn run(runner: Runner, name: &str, guest_args: &[String]) -> Result<(), String> {
    let paths = Paths::new();
    if let Runner::Under(engine) = runner
        && engine.is_armor()
    {
        build_armor(&paths)?;
    }
    let bin = build_one(&paths, name)?;
    let output = exec(
        runner
            .command(&paths, &bin, Path::new(guest_root(name)))
            .args(guest_args),
    )?;
    print_outcome(&output);
    Ok(())
}

/// Builds the armor binary (`difftest` profile: optimized, with debug
/// assertions and overflow checks) with the cargo running xtask.
fn build_armor(paths: &Paths) -> Result<(), String> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args([
            "build",
            "--quiet",
            "--profile",
            "difftest",
            "--package",
            "armor",
        ])
        .current_dir(&paths.root)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("building armor failed".to_string())
    }
}

/// Builds `tests/guest/asm/<name>.S`, or `tests/guest/c/<stem>.c` for a C
/// variant `<prefix><stem>`, and returns the path of the executable.
fn build_one(paths: &Paths, name: &str) -> Result<PathBuf, String> {
    let c_variant = C_VARIANTS
        .iter()
        .find_map(|(prefix, flags)| Some((name.strip_prefix(prefix)?, *flags)));
    let i386_c_variant = I386_C_VARIANTS
        .iter()
        .find_map(|(prefix, flags)| Some((name.strip_prefix(prefix)?, *flags)));
    let (src, out, flags, cc) = match (c_variant, i386_c_variant, name.strip_prefix(I386_PREFIX)) {
        (Some((stem, flags)), _, _) => (
            paths.c_src.join(format!("{stem}.c")),
            paths.guest_out.join(name),
            flags,
            GUEST_CC,
        ),
        (_, Some((stem, flags)), _) => {
            let own = paths.c32_src.join(format!("{stem}.c"));
            let src = if own.is_file() {
                own
            } else {
                paths.c_src.join(format!("{stem}.c"))
            };
            (src, paths.guest_out.join(name), flags, GUEST_CC_I386)
        }
        (_, _, Some(rest)) => (
            paths.asm32_src.join(format!("{rest}.S")),
            paths.guest_out.join(name),
            GUEST_ASM_FLAGS,
            GUEST_CC_I386,
        ),
        _ => (
            paths.asm_src.join(format!("{name}.S")),
            paths.asm_out.join(name),
            GUEST_ASM_FLAGS,
            GUEST_CC,
        ),
    };
    if !src.is_file() {
        return Err(format!(
            "no guest program `{name}` ({} not found)",
            src.display()
        ));
    }
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let output = exec(
        Command::new(cc)
            .args(flags)
            .arg("-I")
            .arg(&paths.asm_include)
            .arg("-o")
            .arg(&out)
            .arg(&src),
    )?;
    if !output.status.success() {
        return Err(format!(
            "{cc} failed on {}:\n{}",
            src.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(out)
}

/// Runs a command to completion, turning a missing tool into a setup hint.
fn exec(cmd: &mut Command) -> Result<Output, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.output().map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => {
            format!("`{program}` not found: install it as described in docs/environment.md")
        }
        _ => format!("cannot run `{program}`: {e}"),
    })
}

fn print_outcome(output: &Output) {
    println!("== stdout ==");
    print!("{}", String::from_utf8_lossy(&output.stdout));
    println!("== stderr ==");
    print!("{}", String::from_utf8_lossy(&output.stderr));
    println!("== {} ==", describe_status(output.status));
}

fn describe_status(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit code {code}"),
        (None, Some(sig)) => format!("killed by signal {sig}"),
        (None, None) => "unknown termination".to_string(),
    }
}

/// Returns every file with extension `ext` under `dir`, recursively, sorted.
fn find_sources(dir: &Path, ext: &str) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == ext) {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// `<asm_src>/basic/hello.S` -> `basic/hello`.
fn guest_name(asm_src: &Path, src: &Path) -> Option<String> {
    let relative = src.strip_prefix(asm_src).ok()?.with_extension("");
    relative.to_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_name_is_relative_path_without_extension() {
        let root = Path::new("/w/tests/guest/asm");
        assert_eq!(
            guest_name(root, Path::new("/w/tests/guest/asm/basic/hello.S")).as_deref(),
            Some("basic/hello")
        );
    }

    #[test]
    fn guest_name_rejects_paths_outside_root() {
        let root = Path::new("/w/tests/guest/asm");
        assert_eq!(guest_name(root, Path::new("/elsewhere/hello.S")), None);
    }

    #[test]
    fn exit_status_is_described_by_code_or_signal() {
        assert_eq!(
            describe_status(ExitStatus::from_raw(42 << 8)),
            "exit code 42"
        );
        assert_eq!(
            describe_status(ExitStatus::from_raw(11)),
            "killed by signal 11"
        );
    }
}
