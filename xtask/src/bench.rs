//! `cargo xtask bench`: reproducible benchmarks (roadmap M6,
//! docs/tech/performance.md). Each workload runs under `armor --jit`,
//! qemu-x86_64 and, where the host can, natively on AArch64 (the same C
//! source built by the host gcc, the host's python of the same version),
//! alternating the runners in every round. armor's output must match
//! qemu's: a benchmark that computes something else measures nothing.
//!
//! The armor binary is the release build. Times are wall-clock, from
//! spawning the process to its exit; a watchdog kills runs over the time
//! limit without wrapping the measured process in another one.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::{GUEST_CC, GUEST_ROOT, Paths, QEMU, QEMU_CPU, build_one, exec, rootfs};

const USAGE: &str =
    "usage: cargo xtask bench [--runs N] [--interpreter] [--box64] [--fex] [name...]";

/// Rounds per workload unless `--runs` says otherwise.
const DEFAULT_RUNS: usize = 5;

/// Longest a single run may take.
const TIME_LIMIT: Duration = Duration::from_secs(300);

/// The host's gcc, for the native builds.
const HOST_CC: &str = "gcc";

/// The host's python: the same version as the guest root's (3.13), so the
/// native column runs the same interpreter code.
const HOST_PYTHON: &str = "/usr/bin/python3.13";
const GUEST_PYTHON: &str = "usr/bin/python3.13";

const PYTHON_INTEGER_LOOP: &str = "s=0\nfor i in range(1000000):\n    s+=i*i%7\nprint(s)";
const PYTHON_FLOAT_LOOP: &str = "s=0.0\nfor i in range(1000000):\n    s+=i*0.5/3.0\nprint(s)";

/// The C benchmarks in `tests/bench/`, with the compiler flags of both
/// builds. Native builds also get `-ffp-contract=off`: gcc fuses
/// multiplications and additions on AArch64 by default, which x86-64
/// without FMA cannot do, and the results would differ.
const C_BENCHMARKS: &[(&str, &str, &[&str])] = &[
    (
        "integer",
        "integer code: sieve, hash mixing, division, sorting",
        &["-O2"],
    ),
    (
        "float",
        "scalar SSE double precision: Mandelbrot, sqrt, division",
        &["-O2", "-fno-tree-vectorize"],
    ),
    (
        "simd",
        "SSE2 integer: glibc string functions, vectorized byte loop",
        &["-O3"],
    ),
    (
        "syscall",
        "syscall round trips: getppid, write to /dev/null",
        &["-O2"],
    ),
    (
        "memory",
        "malloc/free, memset/memcpy, mmap/munmap",
        &["-O2"],
    ),
    (
        "threads",
        "4 threads: independent work, then a mutex and an atomic",
        &["-O2", "-pthread"],
    ),
];

/// What runs a workload.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Runner {
    Jit,
    Interpreter,
    Qemu,
    Native,
    /// Other engines on the same workloads (ADR-0032), from `ARMOR_BOX64`
    /// and `ARMOR_FEX` as in `difftest --engine`.
    Box64,
    Fex,
}

impl Runner {
    fn label(self) -> &'static str {
        match self {
            Runner::Jit => "armor --jit",
            Runner::Interpreter => "armor (interpreter)",
            Runner::Qemu => "qemu-x86_64",
            Runner::Native => "native",
            Runner::Box64 => "box64",
            Runner::Fex => "FEX",
        }
    }

    /// Another project's engine: a failure or a different output is shown
    /// in its cell instead of stopping the benchmark.
    fn is_other(self) -> bool {
        matches!(self, Runner::Box64 | Runner::Fex)
    }
}

struct Workload {
    name: String,
    what: String,
    /// The x86-64 program, its arguments and the guest root.
    guest: PathBuf,
    root: PathBuf,
    args: Vec<String>,
    /// The AArch64 program and its arguments, if there is one.
    native: Option<(PathBuf, Vec<String>)>,
}

/// One timed run.
struct Sample {
    time: Duration,
    /// Time armor spent translating (`ARMOR_JIT_STATS`), JIT runs only.
    translating: Option<Duration>,
}

pub fn run(args: &[String]) -> Result<(), String> {
    let (mut runs, mut interpreter, mut filter) = (DEFAULT_RUNS, false, Vec::new());
    let mut report = false;
    let mut others = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--runs" => {
                runs = rest
                    .next()
                    .and_then(|n| n.parse().ok())
                    .filter(|&n| n > 0)
                    .ok_or(USAGE)?;
            }
            "--interpreter" => interpreter = true,
            "--report" => report = true,
            "--box64" => others.push(Runner::Box64),
            "--fex" => others.push(Runner::Fex),
            name => filter.push(name.to_string()),
        }
    }
    let paths = Paths::new();
    let armor = build_release_armor(&paths)?;
    let workloads: Vec<Workload> = workloads(&paths)?
        .into_iter()
        .filter(|w| filter.is_empty() || filter.contains(&w.name))
        .collect();
    if workloads.is_empty() {
        return Err(format!("no benchmark matches {filter:?}\n{USAGE}"));
    }

    let mut runners = vec![Runner::Jit, Runner::Qemu, Runner::Native];
    if interpreter {
        runners.insert(1, Runner::Interpreter);
    }
    runners.extend(others);
    println!("{}", environment(&paths));
    let mut table = Vec::new();
    for workload in &workloads {
        eprint!("{} ", workload.name);
        let mut samples: BTreeMap<Runner, Vec<Sample>> = BTreeMap::new();
        let mut expected: Option<Vec<u8>> = None;
        let mut broken: BTreeMap<Runner, String> = BTreeMap::new();
        for _ in 0..runs {
            for &runner in &runners {
                if broken.contains_key(&runner) {
                    continue;
                }
                let Some(mut cmd) = command(runner, &armor, workload) else {
                    continue;
                };
                let (output, time) = timed(&mut cmd)?;
                eprint!(".");
                if runner.is_other() {
                    let mut first = expected.clone();
                    let why = if !output.status.success() {
                        Some(format!("fails ({})", output.status))
                    } else if check_output(workload, runner, &output.stdout, &mut first).is_err() {
                        Some("different output".to_string())
                    } else {
                        None
                    };
                    if let Some(why) = why {
                        broken.insert(runner, why);
                        continue;
                    }
                }
                if !output.status.success() {
                    return Err(format!(
                        "{} under {} failed ({}):\n{}",
                        workload.name,
                        runner.label(),
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
                check_output(workload, runner, &output.stdout, &mut expected)?;
                let translating = (runner == Runner::Jit)
                    .then(|| translation_time(&output.stderr))
                    .flatten();
                samples
                    .entry(runner)
                    .or_default()
                    .push(Sample { time, translating });
            }
        }
        eprintln!();
        table.push((workload, samples, broken));
    }
    print_table(&runners, &table, runs);
    if report {
        let command = format!(
            "cargo xtask bench {}",
            args.join(" ").replace("--report", "")
        )
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
        let mut records = Vec::new();
        for (workload, samples, broken) in &table {
            let native = samples
                .get(&Runner::Native)
                .map(|s| median(&mut s.iter().map(|x| x.time).collect::<Vec<_>>()));
            for &runner in runners.iter().filter(|r| **r != Runner::Native) {
                let result = match (broken.get(&runner), samples.get(&runner), native) {
                    (Some(why), _, _) => why.clone(),
                    (None, Some(s), Some(n)) => {
                        let t = median(&mut s.iter().map(|x| x.time).collect::<Vec<_>>());
                        format!(
                            "{:.3} s, {:.2}× the native time (median of {runs})",
                            t.as_secs_f64(),
                            t.as_secs_f64() / n.as_secs_f64()
                        )
                    }
                    _ => continue,
                };
                records.push(crate::results::Record::now(
                    "performance",
                    &workload.name,
                    runner.label(),
                    "the native AArch64 build, same machine",
                    &result,
                    &command,
                ));
            }
        }
        let file = crate::results::record(&paths.root, &crate::results::Machine::here(), &records)?;
        println!("recorded in {}", file.display());
    }
    Ok(())
}

/// Builds armor's release binary, the one users run.
fn build_release_armor(paths: &Paths) -> Result<PathBuf, String> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--quiet", "--release", "--package", "armor"])
        .current_dir(&paths.root)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err("building armor failed".to_string());
    }
    Ok(paths.root.join("target/release/armor"))
}

/// 7-Zip 26.04 compressing with LZMA2 (`-txz -mx=9 -mmt=1`, the archive
/// on stdout, so every engine's output is compared byte for byte): the
/// Linux x86-64 `7zz` against the native ARM64 one, both from the
/// downloads of `app-check`, in the games root (it needs libstdc++). The
/// input is made here from a fixed seed: 10 MB of pseudo-random text
/// from a small vocabulary and 2 MB of random bytes. Left out when the
/// downloads or the games root are missing.
fn seven_zip_workload(paths: &Paths, out: &Path) -> Result<Option<Workload>, String> {
    let downloads = paths.root.join("target/apps/downloads/7zip-26.04");
    let games = rootfs::dir_of(paths, rootfs::Arch::Games);
    let archives = [
        downloads.join("7z2604-linux-x64.tar.xz"),
        downloads.join("7z2604-linux-arm64.tar.xz"),
    ];
    if !archives.iter().all(|a| a.is_file()) || !rootfs::available_of(paths, rootfs::Arch::Games) {
        return Ok(None);
    }
    let mut programs = Vec::new();
    for (archive, dir) in archives.iter().zip(["7zip-x64", "7zip-arm64"]) {
        let dir = out.join(dir);
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if !dir.join("7zz").is_file() {
            exec(
                Command::new("tar")
                    .arg("-xf")
                    .arg(archive)
                    .arg("-C")
                    .arg(&dir),
            )?;
        }
        programs.push(dir.join("7zz"));
    }
    let input = out.join("7zip-input.bin");
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    const WORDS: [&str; 24] = [
        "armor",
        "translates",
        "x86",
        "programs",
        "to",
        "AArch64",
        "the",
        "guest",
        "memory",
        "page",
        "register",
        "flags",
        "signal",
        "thread",
        "kernel",
        "file",
        "window",
        "library",
        "and",
        "with",
        "for",
        "each",
        "result",
        "compressed",
    ];
    let mut data = Vec::with_capacity(12 << 20);
    while data.len() < 10 << 20 {
        let word = WORDS[(next() % WORDS.len() as u64) as usize];
        data.extend_from_slice(word.as_bytes());
        data.push(if next() % 12 == 0 { b'\n' } else { b' ' });
    }
    while data.len() < 12 << 20 {
        data.extend_from_slice(&next().to_le_bytes());
    }
    fs::write(&input, &data).map_err(|e| format!("{}: {e}", input.display()))?;
    let args: Vec<String> = ["a", "-txz", "-mx=9", "-mmt=1", "-so", "-an"]
        .iter()
        .map(|s| s.to_string())
        .chain([input.display().to_string()])
        .collect();
    Ok(Some(Workload {
        name: "7zip".to_string(),
        what: "7-Zip 26.04 (Linux 7zz) compressing 12 MB with LZMA2, one thread".to_string(),
        guest: programs[0].clone(),
        root: games,
        args: args.clone(),
        native: Some((programs[1].clone(), args)),
    }))
}

fn workloads(paths: &Paths) -> Result<Vec<Workload>, String> {
    let out = paths.root.join("target/bench");
    fs::create_dir_all(&out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let hello = paths.c_src.join("hello.c");
    let mut list = vec![
        Workload {
            name: "start-static".to_string(),
            what: "startup: static glibc hello world".to_string(),
            guest: build_one(paths, "c/hello")?,
            root: PathBuf::from(GUEST_ROOT),
            args: Vec::new(),
            native: Some((
                build_native(&hello, &out.join("hello-static"), &["-O2", "-static"])?,
                Vec::new(),
            )),
        },
        Workload {
            name: "start-dynamic".to_string(),
            what: "startup: dynamic glibc hello world (ld.so, libc.so.6)".to_string(),
            guest: build_one(paths, "c-dyn/hello")?,
            root: PathBuf::from(GUEST_ROOT),
            args: Vec::new(),
            native: Some((
                build_native(&hello, &out.join("hello-dynamic"), &["-O2"])?,
                Vec::new(),
            )),
        },
    ];
    for &(name, what, flags) in C_BENCHMARKS {
        let src = paths.root.join("tests/bench").join(format!("{name}.c"));
        let guest = out.join(format!("{name}.x86_64"));
        let built = exec(
            Command::new(GUEST_CC)
                .args(flags)
                .arg("-static")
                .arg("-o")
                .arg(&guest)
                .arg(&src)
                .arg("-lm"),
        )?;
        if !built.status.success() {
            return Err(format!(
                "{GUEST_CC} failed on {}:\n{}",
                src.display(),
                String::from_utf8_lossy(&built.stderr)
            ));
        }
        let native = build_native(&src, &out.join(format!("{name}.aarch64")), flags)?;
        list.push(Workload {
            name: name.to_string(),
            what: what.to_string(),
            guest,
            root: PathBuf::from(GUEST_ROOT),
            args: Vec::new(),
            native: Some((native, Vec::new())),
        });
    }
    if let Some(seven_zip) = seven_zip_workload(paths, &out)? {
        list.push(seven_zip);
    }
    if !rootfs::available(paths) {
        return Err(
            "the guest root is missing: run `cargo xtask fetch-rootfs` (docs/testing.md)"
                .to_string(),
        );
    }
    let root = rootfs::dir(paths);
    let host_python = Path::new(HOST_PYTHON).is_file();
    for (name, what, code) in [
        (
            "python-start",
            "python startup (`-c pass`): code run a few times, translation cost dominates",
            "pass",
        ),
        (
            "python-int",
            "python loop, integers (1M iterations)",
            PYTHON_INTEGER_LOOP,
        ),
        (
            "python-float",
            "python loop, floats (1M iterations)",
            PYTHON_FLOAT_LOOP,
        ),
    ] {
        let args = vec!["-c".to_string(), code.to_string()];
        list.push(Workload {
            name: name.to_string(),
            what: what.to_string(),
            guest: root.join(GUEST_PYTHON),
            root: root.clone(),
            native: host_python.then(|| (PathBuf::from(HOST_PYTHON), args.clone())),
            args,
        });
    }
    Ok(list)
}

/// Builds `src` for the host, with `-ffp-contract=off` (see
/// `C_BENCHMARKS`).
fn build_native(src: &Path, out: &Path, flags: &[&str]) -> Result<PathBuf, String> {
    let built = exec(
        Command::new(HOST_CC)
            .args(flags)
            .arg("-ffp-contract=off")
            .arg("-o")
            .arg(out)
            .arg(src)
            .arg("-lm"),
    )?;
    if !built.status.success() {
        return Err(format!(
            "{HOST_CC} failed on {}:\n{}",
            src.display(),
            String::from_utf8_lossy(&built.stderr)
        ));
    }
    Ok(out.to_path_buf())
}

/// The command for one run, or `None` if `runner` has no version of the
/// workload.
fn command(runner: Runner, armor: &Path, workload: &Workload) -> Option<Command> {
    let mut cmd = match runner {
        Runner::Jit | Runner::Interpreter => {
            let mut cmd = Command::new(armor);
            cmd.env("ARMOR_ROOT", &workload.root);
            if runner == Runner::Jit {
                cmd.arg("--jit").env("ARMOR_JIT_STATS", "1");
            } else {
                cmd.arg("--interpreter");
            }
            cmd.arg(&workload.guest).args(&workload.args);
            cmd
        }
        Runner::Qemu => {
            let mut cmd = Command::new(QEMU);
            cmd.args(["-cpu", QEMU_CPU, "-L"])
                .arg(&workload.root)
                .env("QEMU_CPU", QEMU_CPU)
                .env("QEMU_LD_PREFIX", &workload.root)
                .arg(&workload.guest)
                .args(&workload.args);
            cmd
        }
        Runner::Box64 => {
            let mut cmd =
                Command::new(env::var_os("ARMOR_BOX64").unwrap_or_else(|| "box64".into()));
            let libs = ["usr/lib/x86_64-linux-gnu", "lib/x86_64-linux-gnu", "lib"]
                .map(|dir| workload.root.join(dir).display().to_string())
                .join(":");
            cmd.env("BOX64_LD_LIBRARY_PATH", libs)
                .env("BOX64_NOBANNER", "1")
                .env("BOX64_LOG", "0")
                .arg(&workload.guest)
                .args(&workload.args);
            cmd
        }
        Runner::Fex => {
            let mut cmd = Command::new(env::var_os("ARMOR_FEX").unwrap_or_else(|| "FEX".into()));
            cmd.env("FEX_ROOTFS", &workload.root)
                .arg(&workload.guest)
                .args(&workload.args);
            cmd
        }
        Runner::Native => {
            let (program, args) = workload.native.as_ref()?;
            let mut cmd = Command::new(program);
            cmd.args(args);
            cmd
        }
    };
    cmd.stdin(Stdio::null());
    Some(cmd)
}

/// Runs `cmd` and measures it. A watchdog thread kills it after
/// `TIME_LIMIT`.
fn timed(cmd: &mut Command) -> Result<(Output, Duration), String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let start = Instant::now();
    let child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run `{program}`: {e}"))?;
    let pid = child.id().to_string();
    let (done, finished) = mpsc::channel::<()>();
    let watchdog = thread::spawn(move || {
        if finished.recv_timeout(TIME_LIMIT).is_err() {
            let _ = Command::new("kill").args(["-KILL", &pid]).status();
            return true;
        }
        false
    });
    let output = child
        .wait_with_output()
        .map_err(|e| format!("waiting for `{program}`: {e}"))?;
    let time = start.elapsed();
    let _ = done.send(());
    if watchdog.join().unwrap_or(false) {
        return Err(format!("`{program}` did not finish within {TIME_LIMIT:?}"));
    }
    Ok((output, time))
}

/// armor (either engine) must print what qemu prints. The native build
/// should too: if it does not, its column is not comparable, which is an
/// error as well (fix the benchmark, do not compare different work).
fn check_output(
    workload: &Workload,
    runner: Runner,
    stdout: &[u8],
    expected: &mut Option<Vec<u8>>,
) -> Result<(), String> {
    match expected {
        None => {
            *expected = Some(stdout.to_vec());
            Ok(())
        }
        Some(first) if first.as_slice() == stdout => Ok(()),
        Some(first) => Err(format!(
            "{}: output under {} differs from the first run\n  first: {}\n  this:  {}",
            workload.name,
            runner.label(),
            String::from_utf8_lossy(first).trim_end(),
            String::from_utf8_lossy(stdout).trim_end()
        )),
    }
}

/// The translation time from armor's `ARMOR_JIT_STATS` line
/// (`armor_jit::stats::summary`).
fn translation_time(stderr: &[u8]) -> Option<Duration> {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().find(|l| l.contains("jit stats:"))?;
    let micros = line
        .strip_suffix(" us translating")?
        .rsplit(' ')
        .next()?
        .parse()
        .ok()?;
    Some(Duration::from_micros(micros))
}

/// Date, host, kernel, page size, tool versions and armor's commit.
fn environment(paths: &Paths) -> String {
    let run = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .current_dir(&paths.root)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "?".to_string())
    };
    let cpu = run("lscpu", &[])
        .lines()
        .find_map(|l| l.strip_prefix("Model name:").map(|m| m.trim().to_string()))
        .unwrap_or_else(|| "?".to_string());
    let model = fs::read_to_string("/proc/device-tree/model")
        .map(|m| m.trim_end_matches('\0').to_string())
        .unwrap_or_else(|_| "?".to_string());
    let dirty = if run("git", &["status", "--porcelain"]).is_empty() {
        ""
    } else {
        " (uncommitted changes)"
    };
    let qemu = run(QEMU, &["--version"]);
    format!(
        "date: {}\nhost: {model}, {cpu}, {} CPUs, page size {}\nkernel: {}\n\
         armor: {}{dirty}, release build\nqemu: {}\nguest gcc: {}\nhost gcc: {}\n",
        run("date", &["-I"]),
        run("nproc", &[]),
        run("getconf", &["PAGESIZE"]),
        run("uname", &["-r"]),
        run("git", &["rev-parse", "--short", "HEAD"]),
        qemu.lines().next().unwrap_or("?"),
        run(GUEST_CC, &["-dumpfullversion"]),
        run(HOST_CC, &["-dumpfullversion"]),
    )
}

fn median(values: &mut [Duration]) -> Duration {
    values.sort();
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2
    }
}

/// "123 ms (120–130)": median, then min–max; one decimal below 100 ms.
fn describe(samples: &[Sample]) -> String {
    let mut times: Vec<Duration> = samples.iter().map(|s| s.time).collect();
    let mid = median(&mut times);
    let (low, high) = (times[0], times[times.len() - 1]);
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    // One decimal for short runs, where startup times live.
    let digits = if ms(high) < 100.0 { 1 } else { 0 };
    format!(
        "{:.digits$} ms ({:.digits$}–{:.digits$})",
        ms(mid),
        ms(low),
        ms(high)
    )
}

type Row<'w> = (
    &'w Workload,
    BTreeMap<Runner, Vec<Sample>>,
    BTreeMap<Runner, String>,
);

fn print_table(runners: &[Runner], table: &[Row], runs: usize) {
    println!("Median of {runs} runs, min–max in brackets. armor/qemu < 1: armor is faster.\n");
    let mut header = "| Benchmark |".to_string();
    for runner in runners {
        header.push_str(&format!(" {} |", runner.label()));
    }
    header.push_str(" armor/qemu | translating (jit) |");
    println!("{header}");
    println!("|{}", "---|".repeat(runners.len() + 3));
    for (workload, samples, broken) in table {
        let mut line = format!("| {} |", workload.name);
        for runner in runners {
            match (broken.get(runner), samples.get(runner)) {
                (Some(why), _) => line.push_str(&format!(" {why} |")),
                (None, Some(s)) => line.push_str(&format!(" {} |", describe(s))),
                (None, None) => line.push_str(" — |"),
            }
        }
        let mid = |r: Runner| {
            samples
                .get(&r)
                .map(|s| median(&mut s.iter().map(|x| x.time).collect::<Vec<_>>()))
        };
        match (mid(Runner::Jit), mid(Runner::Qemu)) {
            (Some(a), Some(q)) => {
                line.push_str(&format!(" {:.2} |", a.as_secs_f64() / q.as_secs_f64()))
            }
            _ => line.push_str(" — |"),
        }
        let translating: Option<Vec<Duration>> = samples
            .get(&Runner::Jit)
            .map(|s| s.iter().filter_map(|x| x.translating).collect());
        match (translating, mid(Runner::Jit)) {
            (Some(mut t), Some(total)) if !t.is_empty() => {
                let t = median(&mut t);
                line.push_str(&format!(
                    " {:.1} ms ({:.0}%) |",
                    t.as_secs_f64() * 1000.0,
                    100.0 * t.as_secs_f64() / total.as_secs_f64()
                ));
            }
            _ => line.push_str(" — |"),
        }
        println!("{line}");
    }
    println!();
    for (workload, _, _) in table {
        println!("- `{}`: {}", workload.name, workload.what);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_odd_and_even_counts() {
        let ms = Duration::from_millis;
        assert_eq!(median(&mut [ms(3), ms(1), ms(2)]), ms(2));
        assert_eq!(
            median(&mut [ms(4), ms(1), ms(3), ms(2)]),
            Duration::from_micros(2500)
        );
    }

    #[test]
    fn translation_time_is_read_from_the_stats_line() {
        let stderr = b"noise\narmor: jit stats: 5 blocks, 9 guest instructions, \
                       200 host instructions, 1234 us translating\n";
        assert_eq!(translation_time(stderr), Some(Duration::from_micros(1234)));
        assert_eq!(translation_time(b"armor: something else\n"), None);
    }
}
