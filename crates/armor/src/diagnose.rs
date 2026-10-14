//! `--diagnose` (ADR-0028): what happened during the run, reported when
//! armor's process ends, with a concrete suggestion for each problem.
//!
//! The report holds no guest data: no arguments, environment, file
//! contents or memory, only the program path, syscall numbers, signal
//! names, armor's own messages and counters (docs/tech/security.md).

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use armor_linux::{
    SIGABRT, SIGALRM, SIGBUS, SIGFPE, SIGHUP, SIGILL, SIGINT, SIGKILL, SIGPIPE, SIGQUIT, SIGSEGV,
    SIGTERM, SIGTRAP,
};

/// Where the report goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Stderr,
    /// Appended to this file: a process and the armor processes it
    /// starts with execve each add their own report.
    File(PathBuf),
}

/// How armor's process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Status(i32),
    Signal(i32),
    /// armor stopped: the reason is in `stopped`.
    ArmorError,
    /// armor handed the process over to a native program (execve).
    Native,
}

#[derive(Default)]
struct Events {
    target: Option<Target>,
    program: String,
    profile: Option<String>,
    path: String,
    jit: bool,
    enosys: BTreeMap<u64, usize>,
    faults: BTreeMap<i32, usize>,
    stopped: Option<String>,
    jit_stats: Option<String>,
}

static EVENTS: OnceLock<Mutex<Events>> = OnceLock::new();

fn events() -> std::sync::MutexGuard<'static, Events> {
    EVENTS
        .get_or_init(|| Mutex::new(Events::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Turns the report on for this process.
pub fn enable(target: Target, program: &str) {
    let mut e = events();
    e.target = Some(target);
    e.program = program.to_string();
}

pub fn enabled() -> bool {
    events().target.is_some()
}

/// The profile in use (ADR-0028).
pub fn profile(name: &str) {
    events().profile = Some(name.to_string());
}

/// The execution path chosen, and whether the JIT runs.
pub fn path(description: &str, jit: bool) {
    let mut e = events();
    e.path = description.to_string();
    e.jit = jit;
}

pub fn enosys(number: u64) {
    let mut e = events();
    if e.target.is_some() {
        *e.enosys.entry(number).or_insert(0) += 1;
    }
}

pub fn fault(signal: i32) {
    let mut e = events();
    if e.target.is_some() {
        *e.faults.entry(signal).or_insert(0) += 1;
    }
}

pub fn stopped(what: &str) {
    events().stopped = Some(what.to_string());
}

pub fn jit_stats(summary: String) {
    events().jit_stats = Some(summary);
}

/// Writes the report, if `--diagnose` is on. Called once, as the process
/// ends.
pub fn report(ending: Ending) {
    let e = events();
    let Some(target) = &e.target else {
        return;
    };
    let text = render(&e, ending, std::process::id());
    match target {
        Target::Stderr => eprint!("{text}"),
        Target::File(path) => {
            let written = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| f.write_all(text.as_bytes()));
            if let Err(err) = written {
                eprintln!(
                    "armor: cannot write the diagnose report to {}: {err}",
                    path.display()
                );
            }
        }
    }
}

fn render(e: &Events, ending: Ending, pid: u32) -> String {
    let mut out = format!("armor diagnose report (process {pid})\n");
    let mut line = |key: &str, value: &str| out.push_str(&format!("  {key:<10} {value}\n"));
    line("program:", &e.program);
    if let Some(profile) = &e.profile {
        line("profile:", profile);
    }
    line("path:", &e.path);
    line(
        "ending:",
        &match ending {
            Ending::Status(s) => format!("exit status {s}"),
            Ending::Signal(s) => format!("killed by {}", signal_name(s)),
            Ending::ArmorError => "stopped by armor (exit status 125)".to_string(),
            Ending::Native => "handed over to the native program".to_string(),
        },
    );
    if let Some(what) = &e.stopped {
        line("stopped:", what);
    }
    if !e.enosys.is_empty() {
        let list: Vec<String> = e
            .enosys
            .iter()
            .map(|(n, count)| format!("{n} ({count}x)"))
            .collect();
        line(
            "ENOSYS:",
            &format!("syscalls {} (declined on purpose)", list.join(", ")),
        );
    }
    if !e.faults.is_empty() {
        let list: Vec<String> = e
            .faults
            .iter()
            .map(|(s, count)| format!("{} ({count}x)", signal_name(*s)))
            .collect();
        line("faults:", &list.join(", "));
    }
    if let Some(stats) = e.jit_stats.as_ref().filter(|_| e.jit) {
        line("jit:", stats);
    }
    let suggestions = suggestions(e, ending);
    if suggestions.is_empty() {
        out.push_str("  nothing to suggest\n");
    } else {
        out.push_str("  suggestions:\n");
        for s in suggestions {
            out.push_str(&format!("  - {s}\n"));
        }
    }
    out
}

/// One concrete action for each problem the run showed.
fn suggestions(e: &Events, ending: Ending) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(suggestion) = e.stopped.as_deref().and_then(stop_suggestion) {
        out.push(suggestion.to_string());
    }
    if e.faults.contains_key(&SIGILL) {
        out.push(
            "SIGILL: an invalid instruction, or one of an extension armor's CPU does not \
             report (AVX, AVX-512, …; ADR-0026): `armor inspect` lists them under \"missing\""
                .to_string(),
        );
    }
    let crashed = [SIGSEGV, SIGBUS, SIGFPE]
        .iter()
        .any(|s| e.faults.contains_key(s));
    if crashed && e.jit {
        out.push(
            "a crash under the JIT: run again with `--interpreter`; if it then works, it is \
             a bug in armor's JIT, and `--lockstep` finds the first instruction it translates \
             wrongly"
                .to_string(),
        );
    }
    let failed = !matches!(ending, Ending::Status(0) | Ending::Native);
    if failed && !e.enosys.is_empty() {
        out.push(
            "the program failed, and armor answered some syscalls with ENOSYS (on purpose, \
             as Linux or qemu-user may: docs/tech/syscalls.md): a program that does not fall \
             back from them may fail because of it"
                .to_string(),
        );
    }
    if ending == Ending::Status(127) {
        out.push(
            "exit status 127 often means the dynamic loader could not find a library: check \
             that the guest root (ARMOR_ROOT, --root) has it"
                .to_string(),
        );
    }
    out
}

/// What to do when armor stops with `what` (its message), printed even
/// without `--diagnose` (ADR-0028).
pub fn stop_suggestion(what: &str) -> Option<&'static str> {
    if what.contains("not supported yet") {
        Some(
            "armor does not implement this yet: `armor inspect` on the program shows which \
             instruction-set extensions its code uses, and docs/tech/syscalls.md which \
             syscalls armor handles",
        )
    } else if what.starts_with("cannot load") && what.contains("No such file") {
        Some(
            "a file the program needs (its dynamic loader or a library) is missing: check \
             the guest root (ARMOR_ROOT, --root)",
        )
    } else {
        None
    }
}

fn signal_name(signal: i32) -> String {
    let name = match signal {
        SIGHUP => "SIGHUP",
        SIGINT => "SIGINT",
        SIGQUIT => "SIGQUIT",
        SIGILL => "SIGILL",
        SIGTRAP => "SIGTRAP",
        SIGABRT => "SIGABRT",
        SIGBUS => "SIGBUS",
        SIGFPE => "SIGFPE",
        SIGKILL => "SIGKILL",
        SIGSEGV => "SIGSEGV",
        SIGPIPE => "SIGPIPE",
        SIGALRM => "SIGALRM",
        SIGTERM => "SIGTERM",
        _ => return format!("signal {signal}"),
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(stopped: Option<&str>, faults: &[i32], enosys: &[u64]) -> Events {
        let mut e = Events {
            target: Some(Target::Stderr),
            program: "./prog".to_string(),
            path: "x86-64 under armor, JIT".to_string(),
            jit: true,
            stopped: stopped.map(str::to_string),
            ..Events::default()
        };
        for &s in faults {
            *e.faults.entry(s).or_insert(0) += 1;
        }
        for &n in enosys {
            *e.enosys.entry(n).or_insert(0) += 1;
        }
        e
    }

    #[test]
    fn a_clean_run_has_nothing_to_suggest() {
        let text = render(&events(None, &[], &[]), Ending::Status(0), 42);
        assert!(
            text.starts_with("armor diagnose report (process 42)\n"),
            "{text}"
        );
        assert!(text.contains("  ending:    exit status 0\n"), "{text}");
        assert!(text.contains("nothing to suggest"), "{text}");
    }

    #[test]
    fn each_problem_gets_a_suggestion() {
        let e = events(
            Some("ioctl request 0x5401 is not supported yet"),
            &[SIGSEGV, SIGSEGV],
            &[334],
        );
        let text = render(&e, Ending::Signal(SIGSEGV), 7);
        assert!(text.contains("killed by SIGSEGV"), "{text}");
        assert!(text.contains("faults:    SIGSEGV (2x)"), "{text}");
        assert!(text.contains("ENOSYS:    syscalls 334 (1x)"), "{text}");
        assert!(text.contains("--interpreter"), "{text}");
        assert!(text.contains("armor does not implement this yet"), "{text}");
        assert!(text.contains("the program failed, and armor"), "{text}");
        // ENOSYS alone, in a successful run, is not a problem.
        let text = render(&events(None, &[], &[334]), Ending::Status(0), 7);
        assert!(text.contains("nothing to suggest"), "{text}");
        let text = render(&events(None, &[SIGILL], &[]), Ending::Status(127), 7);
        assert!(text.contains("missing"), "{text}");
        assert!(text.contains("guest root"), "{text}");
    }
}
