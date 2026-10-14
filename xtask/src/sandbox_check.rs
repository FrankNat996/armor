//! `cargo xtask sandbox-check`: proves each control of `armor --sandbox`
//! (ADR-0027, docs/tech/security.md) from inside, twice: with a probe
//! built for x86-64 and run as a guest under armor, and with the same
//! probe built for AArch64 and run natively in the same sandbox, where it
//! stands for armor already taken over by the guest. The second is the
//! one that counts for the boundary.
//!
//! The same checks also run without `--sandbox`, where they must succeed:
//! proof that the probes would see a leak.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::{GUEST_CC, Paths, build_armor, exec};

const PROBE_SOURCE: &str = "tests/sandbox/probe.c";
/// The host's C compiler, for the native probe.
const HOST_CC: &str = "cc";

/// What a check must print inside the sandbox, and outside it.
struct Case {
    name: &'static str,
    args: Vec<String>,
    /// armor options besides `--sandbox`.
    options: Vec<String>,
    inside: Expect,
    /// None: not checked outside (the result depends on the host).
    outside: Option<Expect>,
    /// Only for the x86-64 probe: a control armor enforces itself, which
    /// native code is not subject to.
    x86_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Expect {
    Prints(String),
    /// A count of processes no larger than this.
    AtMost(usize),
    /// More processes than this.
    MoreThan(usize),
    /// Refused: an errno, or armor stopping (exit 125) because it does
    /// not implement the syscall; never "ok".
    Refused,
    /// Killed by this signal.
    Killed(i32),
}

pub fn run() -> Result<(), String> {
    let paths = Paths::new();
    build_armor(&paths)?;
    let dir = paths.root.join("target/sandbox-check");
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["allowed", "read-only", "probes"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(|e| format!("mkdir {sub}: {e}"))?;
    }
    let source = paths.root.join(PROBE_SOURCE);
    let x86 = dir.join("probes/probe-x86_64");
    let native = dir.join("probes/probe-aarch64");
    compile(GUEST_CC, &source, &x86)?;
    compile(HOST_CC, &source, &native)?;
    let secret = dir.join("secret.txt");
    let readable = dir.join("read-only/data.txt");
    for file in [&secret, &readable] {
        std::fs::write(file, "data\n").map_err(|e| format!("{}: {e}", file.display()))?;
    }
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot listen on lo: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });
    let mut bystander = Command::new("sleep")
        .arg("300")
        .spawn()
        .map_err(|e| format!("cannot start a bystander process: {e}"))?;
    let result = check_all(
        &paths,
        &dir,
        &[&x86, &native],
        &secret,
        &readable,
        port,
        &bystander,
    );
    stop(&mut bystander);
    result
}

fn check_all(
    paths: &Paths,
    dir: &Path,
    probes: &[&Path],
    secret: &Path,
    readable: &Path,
    port: u16,
    bystander: &Child,
) -> Result<(), String> {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let read_only = dir.join("read-only");
    let allowed = dir.join("allowed");
    let ok = || Some(Expect::Prints("ok".to_string()));
    let cases = vec![
        Case {
            name: "file outside the allowed paths is not there",
            args: vec!["read".into(), s(secret)],
            options: vec![],
            inside: Expect::Prints("ENOENT".into()),
            outside: ok(),
            x86_only: false,
        },
        Case {
            name: "--allow-ro path is readable",
            args: vec!["read".into(), s(readable)],
            options: vec![],
            inside: Expect::Prints("ok".into()),
            outside: ok(),
            x86_only: false,
        },
        Case {
            name: "--allow-ro path is read-only",
            args: vec!["write".into(), s(&read_only.join("new"))],
            options: vec![],
            inside: Expect::Prints("EROFS".into()),
            outside: ok(),
            x86_only: false,
        },
        Case {
            name: "current directory is writable",
            args: vec!["write".into(), s(&allowed.join("new"))],
            options: vec![],
            inside: Expect::Prints("ok".into()),
            outside: ok(),
            x86_only: false,
        },
        Case {
            name: "no network",
            args: vec!["connect".into(), port.to_string()],
            options: vec![],
            inside: Expect::Prints("ENETUNREACH".into()),
            outside: ok(),
            x86_only: false,
        },
        Case {
            name: "network with --allow-net",
            args: vec!["connect".into(), port.to_string()],
            options: vec!["--allow-net".into()],
            inside: Expect::Prints("ok".into()),
            outside: None,
            x86_only: false,
        },
        Case {
            name: "other processes cannot be signalled",
            args: vec!["kill".into(), bystander.id().to_string()],
            options: vec![],
            inside: Expect::Prints("ESRCH".into()),
            outside: ok(),
            x86_only: false,
        },
        Case {
            name: "other processes are not visible",
            args: vec!["processes".into()],
            options: vec![],
            inside: Expect::AtMost(3),
            outside: Some(Expect::MoreThan(3)),
            x86_only: false,
        },
        Case {
            name: "no mount",
            args: vec!["mount".into()],
            options: vec![],
            inside: Expect::Refused,
            outside: None,
            x86_only: false,
        },
        Case {
            name: "no ptrace (seccomp)",
            args: vec!["ptrace".into()],
            options: vec![],
            inside: Expect::Refused,
            outside: None,
            x86_only: false,
        },
        Case {
            name: "no new user namespace with unshare (seccomp)",
            args: vec!["unshare".into()],
            options: vec![],
            inside: Expect::Refused,
            outside: None,
            x86_only: false,
        },
        Case {
            name: "no new user namespace with clone (seccomp)",
            args: vec!["clone-namespace".into()],
            options: vec![],
            inside: Expect::Refused,
            outside: None,
            x86_only: false,
        },
        Case {
            name: "--limit-files",
            args: vec!["open-files".into(), "64".into()],
            options: vec!["--limit-files".into(), "32".into()],
            inside: Expect::Prints("EMFILE".into()),
            outside: ok(),
            x86_only: false,
        },
        Case {
            name: "--limit-file-size",
            args: vec!["write-size".into(), "8192".into()],
            options: vec!["--limit-file-size".into(), "4K".into()],
            inside: Expect::Prints("EFBIG".into()),
            outside: ok(),
            x86_only: false,
        },
        Case {
            // Not checked outside: the kernel counts every process of the
            // user there, not only the probe's.
            name: "--limit-processes",
            args: vec!["fork".into(), "16".into()],
            options: vec!["--limit-processes".into(), "8".into()],
            inside: Expect::Prints("EAGAIN".into()),
            outside: None,
            x86_only: false,
        },
        Case {
            name: "--limit-cpu",
            args: vec!["spin".into(), "2".into()],
            options: vec!["--limit-cpu".into(), "1".into()],
            inside: Expect::Killed(24),
            outside: ok(),
            x86_only: false,
        },
        Case {
            // Outside, an ordinary user has none either: not a contrast.
            name: "no capabilities",
            args: vec!["capabilities".into()],
            options: vec![],
            inside: Expect::Prints("0000000000000000".into()),
            outside: None,
            x86_only: false,
        },
        Case {
            name: "--limit-memory (counted by armor)",
            args: vec!["allocate".into(), "256".into()],
            options: vec!["--limit-memory".into(), "64M".into()],
            inside: Expect::Prints("ENOMEM".into()),
            outside: ok(),
            x86_only: true,
        },
    ];
    let mut failures = 0;
    for probe in probes {
        let kind = probe.file_name().unwrap().to_string_lossy().into_owned();
        for case in &cases {
            if case.x86_only && kind.ends_with("aarch64") {
                continue;
            }
            let inside = run_probe(paths, probe, &read_only, &allowed, true, case)?;
            let mut verdicts = vec![verdict(&case.inside, &inside)];
            if let Some(outside) = &case.outside {
                let got = run_probe(paths, probe, &read_only, &allowed, false, case)?;
                verdicts
                    .push(verdict(outside, &got).map_err(|e| format!("without --sandbox: {e}")));
            }
            match verdicts.into_iter().collect::<Result<Vec<()>, String>>() {
                Ok(_) => println!("PASS {kind}: {}", case.name),
                Err(why) => {
                    failures += 1;
                    println!("FAIL {kind}: {}: {why}", case.name);
                }
            }
        }
    }
    println!("{failures} failed");
    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} sandbox check(s) failed"))
    }
}

/// What a probe run printed, or how it ended if it printed nothing.
#[derive(Debug)]
struct Outcome {
    stdout: String,
    status: Option<i32>,
    signal: Option<i32>,
}

fn run_probe(
    paths: &Paths,
    probe: &Path,
    read_only: &Path,
    allowed: &Path,
    sandbox: bool,
    case: &Case,
) -> Result<Outcome, String> {
    let mut cmd = Command::new(&paths.armor);
    cmd.current_dir(allowed)
        .env_remove("ARMOR_ROOT")
        .args(["--root", ""])
        .stdin(Stdio::null());
    if sandbox {
        cmd.arg("--sandbox").arg("--allow-ro").arg(read_only);
        cmd.args(&case.options);
    }
    cmd.arg(probe).args(&case.args);
    let output = exec(&mut cmd)?;
    Ok(Outcome {
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        status: output.status.code(),
        signal: std::os::unix::process::ExitStatusExt::signal(&output.status),
    })
}

fn verdict(expect: &Expect, got: &Outcome) -> Result<(), String> {
    let count = || got.stdout.parse::<usize>().ok();
    let good = match expect {
        Expect::Prints(text) => got.stdout == *text,
        Expect::AtMost(n) => count().is_some_and(|c| c <= *n),
        Expect::MoreThan(n) => count().is_some_and(|c| c > *n),
        Expect::Killed(signal) => got.signal == Some(*signal),
        Expect::Refused => {
            got.stdout != "ok" && (!got.stdout.is_empty() || got.status == Some(125))
        }
    };
    if good {
        Ok(())
    } else {
        Err(format!("expected {expect:?}, got {got:?}"))
    }
}

fn compile(cc: &str, source: &Path, out: &PathBuf) -> Result<(), String> {
    let output = exec(
        Command::new(cc)
            .args(["-static", "-O1", "-o"])
            .arg(out)
            .arg(source),
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{cc} failed on {}:\n{}",
            source.display(),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
