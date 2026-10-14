//! `cargo xtask difftest`: runs guest programs under qemu-x86_64 (the oracle)
//! and under armor, and compares stdout bytes and how the process ended
//! (exit code or signal). stderr is shown on failure but not compared, since
//! qemu and armor print different diagnostics. armor runs each program
//! twice, interpreted and with the JIT (ADR-0021 §9), unless `--interpreter`,
//! `--jit` or `--lockstep` picks one. With `--oracle` the expected output
//! comes from a real x86-64 machine instead (`oracle.rs`, ADR-0025), for
//! the static programs only; a few cases where qemu is known to be wrong
//! run only then (`HARDWARE_ONLY`). See docs/testing.md.

use std::fs::File;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output, Stdio};

use crate::oracle::Oracle;
use crate::{
    Engine, GUEST_ROOT, Paths, Runner, all_guest_names, build_armor, build_one, busybox,
    describe_status, exec, guest_root, rootfs,
};

/// A program run to compare: a built test program, or a busybox applet.
struct Run {
    name: String,
    bin: PathBuf,
    args: Vec<String>,
    /// Working directory and stdin file (busybox and coreutils cases).
    data: Option<PathBuf>,
    /// Guest root, the same for qemu and armor.
    root: PathBuf,
}

/// Cases whose name ends with this run with stdout connected to a pipe
/// whose read end is already closed, as in `program | head -0`: they check
/// how the program reacts to EPIPE or SIGPIPE.
const CLOSED_STDOUT_SUFFIX: &str = "-closed-stdout";

/// Cases whose output depends on the machine, not on the CPU semantics:
/// skipped with `--oracle` (docs/testing.md, hardware oracle).
const ENVIRONMENT_DEPENDENT: [(&str, &str); 10] = [
    ("basic/cpuid", "a different CPU"),
    (
        "c/drm_version",
        "the GPU of the machine (the oracle has none)",
    ),
    (
        "c-pie/drm_version",
        "the GPU of the machine (the oracle has none)",
    ),
    (
        "sse/reciprocal",
        "approximations that differ between processors",
    ),
    ("busybox/ls", "different files"),
    ("busybox/pwd", "a different directory"),
    ("busybox/id", "a different user"),
    (
        "c/legacy_syscalls",
        "iopl depends on privileges and kernel configuration",
    ),
    (
        "c-pie/legacy_syscalls",
        "iopl depends on privileges and kernel configuration",
    ),
    ("busybox/ls-long", "files owned by a different user"),
];

/// Cases where qemu is known to differ from the hardware, so that only
/// `--oracle` compares them (docs/testing.md, known discrepancies). A name
/// with a `*/` prefix is a C source in every variant of the program
/// (`c/`, `c-pie/`, `c-dyn/`, or the i386 `c32/`, `c32-pie/`, `c32-dyn/`).
const HARDWARE_ONLY: [(&str, &str); 16] = [
    (
        "sse/mxcsr_hardware",
        "qemu never raises DE and propagates the wrong NaN",
    ),
    (
        "sse/sse42_hardware",
        "qemu reads 32-bit lengths in pcmpestr with REX.W",
    ),
    ("*/sse_exceptions", "qemu ignores unmasked SSE exceptions"),
    (
        "i386/basic/tls",
        "qemu-i386 has the GDT of a 32-bit kernel (TLS entries from 6, not 12)",
    ),
    (
        "*/mremap@i386",
        "qemu-i386 aborts on an assertion in mremap",
    ),
    ("*/shared_memory@i386", "qemu-i386 crashes with SIGSEGV"),
    (
        "*/data_selectors",
        "qemu-x86_64 shows 0x2b in DS and ES; Linux on x86-64 has 0",
    ),
    (
        "*/pkeys",
        "qemu answers ENOSYS to the pkey syscalls; Linux x86 without PKU gives EINVAL",
    ),
    (
        "*/bad_ioctl",
        "qemu answers ENOTTY to an unknown ioctl on a closed descriptor, Linux EBADF",
    ),
    (
        "*/legacy32",
        "qemu-i386 clears the blocked signals above 32 on the old sigprocmask's SIG_SETMASK",
    ),
    (
        "*/scm_rights_truncated",
        "qemu passes descriptors that do not fit without MSG_CTRUNC and leaves the rest open",
    ),
    (
        "*/signal_context",
        "qemu-i386 has no trapno, the FPU frame of a 32-bit kernel and XSAVE",
    ),
    (
        "*/uname_machine@i386",
        "qemu-i386 says i686, a 64-bit kernel x86_64 to an i386 process",
    ),
    (
        "coreutils32/uname",
        "qemu-i386 says i686, a 64-bit kernel x86_64 (checked by c32/uname_machine)",
    ),
    (
        "mmx/x87_state",
        "qemu leaves the exponent bits of a written MMX register at 0",
    ),
    (
        "*/x87_transcendental",
        "qemu computes the x87 transcendentals differently from the hardware (ADR-0030)",
    ),
];

/// Why `name` runs with `--oracle` only, if it does. `*/stem` matches the
/// C program `stem` in every variant; with `@i386` only the i386 ones.
fn hardware_only(name: &str) -> Option<&'static str> {
    HARDWARE_ONLY.iter().find_map(|&(pattern, why)| {
        let matches = match pattern.strip_prefix("*/") {
            None => pattern == name,
            Some(rest) => {
                let (stem, i386_only) = match rest.strip_suffix("@i386") {
                    Some(stem) => (stem, true),
                    None => (rest, false),
                };
                name.split_once('/').is_some_and(|(variant, program)| {
                    program == stem
                        && variant.starts_with('c')
                        && (!i386_only || variant.starts_with("c32"))
                })
            }
        };
        matches.then_some(why)
    })
}

pub fn run(args: &[String]) -> Result<(), String> {
    let (oracle, args) = match args {
        [flag, rest @ ..] if flag == "--oracle" => (Some(Oracle::from_env()?), rest),
        _ => (None, args),
    };
    let (engines, filter): (&[Engine], _) = match args {
        [flag, rest @ ..] if flag == "--interpreter" => (&[Engine::Interpreter], rest),
        [flag, rest @ ..] if flag == "--jit" => (&[Engine::Jit], rest),
        [flag, rest @ ..] if flag == "--lockstep" => (&[Engine::Lockstep], rest),
        // Another emulator on the same cases (ADR-0032).
        [flag, engine, rest @ ..] if flag == "--engine" => match engine.as_str() {
            "box64" => (&[Engine::Box64], rest),
            "fex" => (&[Engine::Fex], rest),
            other => return Err(format!("unknown engine {other} (box64 or fex)")),
        },
        _ => (&[Engine::Interpreter, Engine::Jit], args),
    };
    let paths = Paths::new();
    if engines.iter().any(|e| e.is_armor()) {
        build_armor(&paths)?;
    }
    let mut runs = collect(&paths, filter)?;
    let reference = if oracle.is_some() { "hardware" } else { "qemu" };
    if let Some(oracle) = &oracle {
        // Only static programs run there: the dynamic ones need the guest
        // root, and a closed stdout cannot cross ssh.
        runs.retain(|run| {
            let runnable = run.root == Path::new(GUEST_ROOT)
                && !run.name.starts_with("c-dyn/")
                && !run.name.starts_with("c32-dyn/")
                && !run.name.ends_with(CLOSED_STDOUT_SUFFIX);
            if !runnable {
                println!("SKIP {} (not runnable on the oracle)", run.name);
            }
            runnable
        });
        runs.retain(|run| {
            match ENVIRONMENT_DEPENDENT
                .iter()
                .find(|(name, _)| *name == run.name)
            {
                Some((_, why)) => {
                    println!("SKIP {} ({why})", run.name);
                    false
                }
                None => true,
            }
        });
        let names: Vec<String> = runs.iter().map(|run| run.name.clone()).collect();
        oracle.prepare(&busybox::data_dir(&paths), &names)?;
    } else {
        runs.retain(|run| match hardware_only(&run.name) {
            Some(why) => {
                println!("SKIP {} (hardware oracle only: {why})", run.name);
                false
            }
            None => true,
        });
    }

    let mut failures = 0;
    let mut uploaded = std::collections::HashMap::new();
    for run in &runs {
        let expected = match &oracle {
            Some(oracle) => {
                let key = run.bin.clone();
                let remote = match uploaded.get(&key) {
                    Some(remote) => remote,
                    None => {
                        let remote = oracle.upload(&run.bin, &run.name)?;
                        uploaded.entry(key).or_insert(remote)
                    }
                };
                let stdin = run.data.as_ref().map(|dir| dir.join(busybox::STDIN_FILE));
                oracle.run(remote, &run.args, run.data.is_some(), stdin.as_deref())?
            }
            None => execute(Runner::Qemu, &paths, run)?,
        };
        let mut failed = Vec::new();
        for &engine in engines {
            let actual = execute(Runner::Under(engine), &paths, run)?;
            if let Err(why) = compare(&expected, &actual, reference, engine.program()) {
                failed.push((engine, why, actual.stderr));
            }
        }
        if failed.is_empty() {
            println!("PASS {}", run.name);
            continue;
        }
        failures += 1;
        for (engine, why, stderr) in failed {
            println!("FAIL {} ({}): {why}", run.name, engine.name());
            if !stderr.is_empty() {
                println!(
                    "     {} stderr: {}",
                    engine.program(),
                    String::from_utf8_lossy(&stderr).trim_end()
                );
            }
        }
    }
    println!("{} passed, {failures} failed", runs.len() - failures);
    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} differential test(s) failed"))
    }
}

/// Every test program, busybox case and coreutils case, or those named in
/// `filter` (`busybox/<name>` selects one busybox case, `coreutils/<name>`
/// one coreutils case).
fn collect(paths: &Paths, filter: &[String]) -> Result<Vec<Run>, String> {
    let selected = |name: &str| filter.is_empty() || filter.iter().any(|f| f == name);
    let mut runs = Vec::new();
    for name in all_guest_names(paths)? {
        if selected(&name) {
            runs.push(Run {
                bin: build_one(paths, &name)?,
                root: PathBuf::from(guest_root(&name)),
                name,
                args: Vec::new(),
                data: None,
            });
        }
    }
    let cases: Vec<_> = busybox::cases(paths)?
        .into_iter()
        .filter(|case| selected(&case.name))
        .collect();
    if !cases.is_empty() && !busybox::available(paths) {
        return Err(
            "busybox is missing: run `cargo xtask fetch-busybox` (docs/testing.md)".to_string(),
        );
    }
    for case in cases {
        runs.push(Run {
            name: case.name,
            bin: busybox::binary(paths),
            args: case.args,
            data: Some(busybox::data_dir(paths)),
            root: PathBuf::from(GUEST_ROOT),
        });
    }
    let cases: Vec<_> = rootfs::cases(paths)?
        .into_iter()
        .filter(|case| selected(&case.name))
        .collect();
    if !cases.is_empty() && !rootfs::available(paths) {
        return Err(
            "the coreutils guest root is missing: run `cargo xtask fetch-rootfs` \
             (docs/testing.md)"
                .to_string(),
        );
    }
    let i386: Vec<_> = rootfs::cases_i386(paths)?
        .into_iter()
        .filter(|case| selected(&case.name))
        .collect();
    if !i386.is_empty() && !rootfs::available_of(paths, rootfs::Arch::I386) {
        return Err(
            "the i386 guest root is missing: run `cargo xtask fetch-rootfs --i386` \
             (docs/testing.md)"
                .to_string(),
        );
    }
    for (cases, arch) in [(cases, rootfs::Arch::Amd64), (i386, rootfs::Arch::I386)] {
        let root = rootfs::dir_of(paths, arch);
        for case in cases {
            let (program, args) = case
                .args
                .split_first()
                .ok_or_else(|| format!("{}: no program", case.name))?;
            runs.push(Run {
                name: case.name,
                bin: root.join("usr/bin").join(program),
                args: args.to_vec(),
                data: Some(busybox::data_dir(paths)),
                root: root.clone(),
            });
        }
    }
    if runs.is_empty() {
        return Err(format!("no test matches {filter:?}"));
    }
    Ok(runs)
}

fn execute(runner: Runner, paths: &Paths, run: &Run) -> Result<Output, String> {
    let mut cmd = runner.command(paths, &run.bin, &run.root);
    cmd.args(&run.args);
    if run.name.ends_with(CLOSED_STDOUT_SUFFIX) {
        let (reader, writer) = std::io::pipe().map_err(|e| format!("cannot create a pipe: {e}"))?;
        drop(reader);
        cmd.stdout(writer);
    }
    match &run.data {
        Some(dir) => {
            let stdin = File::open(dir.join(busybox::STDIN_FILE))
                .map_err(|e| format!("cannot open the stdin file in {}: {e}", dir.display()))?;
            cmd.current_dir(dir).stdin(stdin);
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }
    exec(&mut cmd)
}

/// How a process ended, comparable between qemu and armor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Termination {
    Exit(i32),
    Signal(i32),
}

fn termination(status: ExitStatus) -> Option<Termination> {
    status
        .code()
        .map(Termination::Exit)
        .or_else(|| status.signal().map(Termination::Signal))
}

/// `Ok` if the engine under test (`tested`: armor, box64, FEX) behaved
/// like the reference (qemu or the hardware), otherwise a description of
/// the difference.
fn compare(
    expected: &Output,
    actual: &Output,
    reference: &str,
    tested: &str,
) -> Result<(), String> {
    for (who, output) in [(reference, expected), (tested, actual)] {
        if output.status.code() == Some(crate::TIMED_OUT_STATUS) {
            return Err(format!("{who} did not finish within the time limit"));
        }
    }
    if termination(expected.status) != termination(actual.status) {
        return Err(format!(
            "{reference}: {}, {tested}: {}",
            describe_status(expected.status),
            describe_status(actual.status)
        ));
    }
    let (e, a) = (&expected.stdout, &actual.stdout);
    if e == a {
        return Ok(());
    }
    let at = e.iter().zip(a).take_while(|(x, y)| x == y).count();
    Err(format!(
        "stdout differs at byte {at} ({reference} {} bytes, {tested} {} bytes)\n     {reference}: {}\n     {tested}: {}",
        e.len(),
        a.len(),
        hex_window(e, at),
        hex_window(a, at)
    ))
}

/// Hex bytes around `at`, 8-byte aligned, as the dumps are 8-byte values.
fn hex_window(bytes: &[u8], at: usize) -> String {
    let start = at - at % 8;
    let end = (start + 16).min(bytes.len());
    if start >= bytes.len() {
        return "(end of output)".to_string();
    }
    let hex: Vec<String> = bytes[start..end]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("[{start}..{end}] {}", hex.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(raw_status: i32, stdout: &[u8]) -> Output {
        Output {
            status: ExitStatus::from_raw(raw_status),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn hardware_only_patterns_cover_the_variants() {
        assert!(hardware_only("sse/mxcsr_hardware").is_some());
        assert!(hardware_only("c-dyn/sse_exceptions").is_some());
        assert!(hardware_only("c32-pie/x87_transcendental").is_some());
        assert!(hardware_only("c32-dyn/mremap").is_some());
        assert!(hardware_only("c/mremap").is_none());
        assert!(hardware_only("c32/hello").is_none());
        assert!(hardware_only("busybox/sse_exceptions").is_none());
    }

    #[test]
    fn identical_runs_match() {
        assert_eq!(
            compare(&output(0, b"hi"), &output(0, b"hi"), "qemu", "armor"),
            Ok(())
        );
    }

    #[test]
    fn exit_status_and_signal_are_compared() {
        assert!(compare(&output(42 << 8, b""), &output(0, b""), "qemu", "armor").is_err());
        assert!(compare(&output(11, b""), &output(11, b""), "qemu", "armor").is_ok());
        assert!(compare(&output(11, b""), &output(4, b""), "qemu", "armor").is_err());
    }

    #[test]
    fn stdout_difference_reports_the_first_differing_byte() {
        let err = compare(
            &output(0, b"0123456789abcdefXYZ"),
            &output(0, b"0123456789abcdefXYz"),
            "qemu",
            "armor",
        )
        .unwrap_err();
        assert!(err.contains("at byte 18"), "{err}");
        assert!(err.contains("[16..19] 58 59 5a"), "{err}");
        assert!(err.contains("[16..19] 58 59 7a"), "{err}");
    }

    #[test]
    fn missing_output_is_reported() {
        let err = compare(&output(0, b"abc"), &output(0, b""), "qemu", "armor").unwrap_err();
        assert!(err.contains("(end of output)"), "{err}");
    }
}
