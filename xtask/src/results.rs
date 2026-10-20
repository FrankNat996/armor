//! Recorded measurements and the matrix generated from them (ADR-0045).
//!
//! `docs/results/<machine>.tsv` holds one machine's results: header lines
//! (`#key<TAB>value`) describing the machine, then one line per result:
//! kind, name, engine, reference, result, date, commit, command. A new
//! result with the same kind, name, engine and reference replaces the old
//! one. `docs/results/by-hand.tsv` has the same lines for what cannot run
//! unattended, each with its machine in the first header.
//!
//! `cargo xtask matrix [--check]` renders them into `COMPATIBILITY_MATRIX.md`
//! and `README.md` between the `matrix` markers.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::progress::replace_between;

/// The machine a measurement runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub cpu: String,
    pub pages: u64,
    pub system: String,
    /// native, kvm or container.
    pub place: String,
    pub kernel: String,
}

impl Machine {
    /// This machine, as its own files describe it.
    pub fn here() -> Machine {
        let lscpu = output("lscpu", &[]);
        let cpu = lscpu
            .lines()
            .find_map(|l| l.strip_prefix("Model name:"))
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|| "unknown CPU".to_string());
        let pages = output("getconf", &["PAGESIZE"]).trim().parse().unwrap_or(0);
        let system = fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "unknown system".to_string());
        let place = if Path::new("/run/.containerenv").exists() || Path::new("/.dockerenv").exists()
        {
            "container"
        } else if fs::read_to_string("/sys/class/dmi/id/sys_vendor")
            .is_ok_and(|v| v.trim() == "QEMU")
        {
            "kvm"
        } else {
            "native"
        };
        Machine {
            cpu,
            pages,
            system,
            place: place.to_string(),
            kernel: output("uname", &["-r"]).trim().to_string(),
        }
    }

    /// The file name: CPU, page size, system (without its code name or
    /// variant in brackets) and place.
    pub fn id(&self) -> String {
        let raw = format!(
            "{}-{}k-{}-{}",
            self.cpu,
            self.pages / 1024,
            self.system.split(" (").next().unwrap_or_default(),
            self.place
        );
        let mut id = String::new();
        for c in raw.to_lowercase().chars() {
            if c.is_ascii_alphanumeric() {
                id.push(c);
            } else if !id.ends_with('-') {
                id.push('-');
            }
        }
        id.trim_matches('-').to_string()
    }

    fn header(&self) -> String {
        format!(
            "#cpu\t{}\n#pages\t{}\n#system\t{}\n#place\t{}\n#kernel\t{}\n",
            self.cpu, self.pages, self.system, self.place, self.kernel
        )
    }
}

fn output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// One result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub kind: String,
    pub name: String,
    pub engine: String,
    pub reference: String,
    pub result: String,
    pub date: String,
    pub commit: String,
    pub command: String,
}

impl Record {
    /// A result measured now, on this tree.
    pub fn now(
        kind: &str,
        name: &str,
        engine: &str,
        reference: &str,
        result: &str,
        command: &str,
    ) -> Record {
        let commit = {
            let head = output("git", &["rev-parse", "--short", "HEAD"])
                .trim()
                .to_string();
            // Results recorded by an earlier `--report` run are not
            // changes to what is measured.
            let dirty = !output(
                "git",
                &[
                    "status",
                    "--porcelain",
                    "--untracked-files=no",
                    "--",
                    ".",
                    ":!docs/results",
                ],
            )
            .trim()
            .is_empty();
            if dirty {
                format!("{head}+changes")
            } else {
                head
            }
        };
        Record {
            kind: kind.to_string(),
            name: name.to_string(),
            engine: engine.to_string(),
            reference: reference.to_string(),
            result: result.to_string(),
            date: output("date", &["-u", "+%F"]).trim().to_string(),
            commit,
            command: command.to_string(),
        }
    }

    fn key(&self) -> (String, String, String, String) {
        (
            self.kind.clone(),
            self.name.clone(),
            self.engine.clone(),
            self.reference.clone(),
        )
    }

    fn line(&self) -> String {
        let clean = |s: &str| s.replace(['\t', '\n'], " ");
        [
            &self.kind,
            &self.name,
            &self.engine,
            &self.reference,
            &self.result,
            &self.date,
            &self.commit,
            &self.command,
        ]
        .map(|s| clean(s))
        .join("\t")
    }

    fn parse(line: &str) -> Option<Record> {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 8 {
            return None;
        }
        Some(Record {
            kind: f[0].to_string(),
            name: f[1].to_string(),
            engine: f[2].to_string(),
            reference: f[3].to_string(),
            result: f[4].to_string(),
            date: f[5].to_string(),
            commit: f[6].to_string(),
            command: f[7].to_string(),
        })
    }
}

/// A results file: its machine facts and its records.
#[derive(Debug, Default)]
pub struct Sheet {
    pub facts: BTreeMap<String, String>,
    pub records: Vec<Record>,
}

impl Sheet {
    pub fn read(path: &Path) -> Result<Sheet, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut sheet = Sheet::default();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.starts_with("# ") {
                continue;
            }
            if let Some(fact) = line.strip_prefix('#') {
                if let Some((k, v)) = fact.split_once('\t') {
                    sheet.facts.insert(k.to_string(), v.to_string());
                }
                continue;
            }
            sheet.records.push(
                Record::parse(line)
                    .ok_or_else(|| format!("{}:{}: not 8 fields", path.display(), n + 1))?,
            );
        }
        Ok(sheet)
    }
}

/// `docs/results/`.
pub fn dir(root: &Path) -> PathBuf {
    root.join("docs/results")
}

/// Writes `records` into this machine's results file, replacing those with
/// the same key.
pub fn record(root: &Path, machine: &Machine, records: &[Record]) -> Result<PathBuf, String> {
    let dir = dir(root);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.tsv", machine.id()));
    let mut kept = if path.is_file() {
        Sheet::read(&path)?.records
    } else {
        Vec::new()
    };
    for new in records {
        kept.retain(|old| old.key() != new.key());
        kept.push(new.clone());
    }
    kept.sort_by_key(Record::key);
    let mut text = String::from(
        "# armor's measurements on one machine (ADR-0045): written by the cargo xtask commands run with --report.\n",
    );
    text.push_str(&machine.header());
    for r in &kept {
        text.push_str(&r.line());
        text.push('\n');
    }
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Removes `--report` from `args`; whether it was there.
pub fn take_flag(args: &mut Vec<String>) -> bool {
    let before = args.len();
    args.retain(|a| a != "--report");
    args.len() != before
}

/// A markdown cell: pipes escaped, runs of blanks made one (`lscpu`
/// pads CPU names).
fn cell(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// A machine's `bench` results as a table of workloads × engines.
fn performance(sheet: &Sheet) -> String {
    let rows: Vec<&Record> = sheet
        .records
        .iter()
        .filter(|r| r.kind == "performance")
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    let mut engines: Vec<&str> = Vec::new();
    let mut workloads: Vec<&str> = Vec::new();
    let mut runs: Vec<String> = Vec::new();
    for r in &rows {
        if !engines.contains(&r.engine.as_str()) {
            engines.push(&r.engine);
        }
        if !workloads.contains(&r.name.as_str()) {
            workloads.push(&r.name);
        }
        let run = format!("`{}`, {}, {}", cell(&r.command), r.date, r.commit);
        if !runs.contains(&run) {
            runs.push(run);
        }
    }
    let mut table = format!(
        "\nPerformance against {} ({}): time, and the multiple of the native time{}.\n\n| Workload |",
        cell(&rows[0].reference),
        runs.join("; "),
        // "(median of 5)", from the results.
        rows[0]
            .result
            .split_once(" (")
            .map(|(_, rest)| format!(", {}", rest.trim_end_matches(')')))
            .unwrap_or_default()
    );
    for engine in &engines {
        table.push_str(&format!(" {} |", cell(engine)));
    }
    table.push_str("\n|---|");
    table.push_str(&"---|".repeat(engines.len()));
    table.push('\n');
    for workload in workloads {
        table.push_str(&format!("| {} |", cell(workload)));
        for engine in &engines {
            let result = rows
                .iter()
                .find(|r| r.name == workload && r.engine == *engine)
                .map(|r| {
                    let result = r.result.split(" (median").next().unwrap_or_default();
                    cell(&result.replace("× the native time", "×"))
                })
                .unwrap_or_else(|| "—".to_string());
            table.push_str(&format!(" {result} |"));
        }
        table.push('\n');
    }
    table
}

/// The applications × machines table: each application's result and
/// engine on every machine that recorded it.
fn applications(sheets: &[(String, Sheet)], label: impl Fn(&Sheet) -> String) -> String {
    let machines: Vec<&Sheet> = sheets
        .iter()
        .map(|(_, s)| s)
        .filter(|s| s.records.iter().any(|r| r.kind == "application"))
        .collect();
    let mut names: Vec<&str> = Vec::new();
    for sheet in &machines {
        for r in sheet.records.iter().filter(|r| r.kind == "application") {
            if !names.contains(&r.name.as_str()) {
                names.push(&r.name);
            }
        }
    }
    if names.is_empty() {
        return String::new();
    }
    let mut table = String::from("\n### Applications × machines\n| Application |");
    for sheet in &machines {
        table.push_str(&format!(" {} |", cell(&label(sheet))));
    }
    table.push_str("\n|---|");
    table.push_str(&"---|".repeat(machines.len()));
    table.push('\n');
    for name in names {
        table.push_str(&format!("| {} |", cell(name)));
        for sheet in &machines {
            let found: Vec<String> = sheet
                .records
                .iter()
                .filter(|r| r.kind == "application" && r.name == name)
                .map(|r| format!("{} — {} ({})", cell(&r.result), cell(&r.engine), r.date))
                .collect();
            let text = if found.is_empty() {
                "not measured".to_string()
            } else {
                found.join("<br>")
            };
            table.push_str(&format!(" {text} |"));
        }
        table.push('\n');
    }
    table
}

/// The sections generated from every results file.
fn render(root: &Path) -> Result<(String, String, String), String> {
    let mut sheets: Vec<(String, Sheet)> = Vec::new();
    let mut by_hand = None;
    let mut entries: Vec<PathBuf> = fs::read_dir(dir(root))
        .map_err(|e| format!("docs/results: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "tsv"))
        .collect();
    entries.sort();
    for path in entries {
        let sheet = Sheet::read(&path)?;
        let stem = path
            .file_stem()
            .expect("a file name")
            .to_string_lossy()
            .into_owned();
        if stem == "by-hand" {
            by_hand = Some(sheet);
        } else {
            sheets.push((stem, sheet));
        }
    }
    let rank = |s: &Sheet| match s.facts.get("place").map(String::as_str) {
        Some("native") => 0,
        Some("kvm") => 1,
        _ => 2,
    };
    sheets.sort_by(|(a_id, a), (b_id, b)| (rank(a), a_id).cmp(&(rank(b), b_id)));
    let describe = |s: &Sheet| {
        let fact = |k: &str| s.facts.get(k).cloned().unwrap_or_default();
        let pages = fact("pages").parse::<u64>().unwrap_or(0) / 1024;
        let system = fact("system");
        let system = system.split(" (").next().unwrap_or_default().to_string();
        format!(
            "{}, {}K pages, {} ({})",
            fact("cpu"),
            pages,
            system,
            match fact("place").as_str() {
                "kvm" => "KVM virtual machine".to_string(),
                place => place.to_string(),
            }
        )
    };
    let mut matrix = String::from(
        "<!-- Generated by `cargo xtask matrix` from docs/results/ (ADR-0045). Do not edit. -->\n",
    );
    matrix.push_str(&applications(&sheets, describe));
    let mut summary = String::from(
        "<!-- Generated by `cargo xtask matrix` from docs/results/ (ADR-0045). Do not edit. -->\n\
         | Machine | armor's test cases, against | Real applications | Measured |\n\
         |---|---|---|---|\n",
    );
    for (_, sheet) in &sheets {
        let title = describe(sheet);
        matrix.push_str(&format!(
            "\n### {}\nKernel `{}`.\n\n| Kind | What | Engine | Reference | Result | Date | Commit | Command |\n|---|---|---|---|---|---|---|---|\n",
            cell(&title),
            sheet.facts.get("kernel").cloned().unwrap_or_default()
        ));
        for r in sheet.records.iter().filter(|r| r.kind != "performance") {
            matrix.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | `{}` |\n",
                r.kind,
                cell(&r.name),
                cell(&r.engine),
                cell(&r.reference),
                cell(&r.result),
                r.date,
                r.commit,
                cell(&r.command)
            ));
        }
        matrix.push_str(&performance(sheet));
        let suites: Vec<String> = sheet
            .records
            .iter()
            .filter(|r| r.kind == "suite")
            .map(suite_label)
            .collect();
        let apps = sheet
            .records
            .iter()
            .filter(|r| r.kind == "application")
            .count();
        let last = sheet
            .records
            .iter()
            .map(|r| r.date.as_str())
            .max()
            .unwrap_or("");
        summary.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            cell(&title),
            cell(&suites.join("<br>")),
            if apps == 0 {
                "—".to_string()
            } else {
                format!("{apps} checked unattended")
            },
            last
        ));
    }
    if let Some(sheet) = by_hand {
        matrix.push_str(
            "\n### Checked by hand\nWhat cannot run unattended (a store account, a sign-in): each entry says how it was checked and when.\n\n\
             | Kind | What | Engine | Machine | Result | Date | Commit | How |\n|---|---|---|---|---|---|---|---|\n",
        );
        for r in &sheet.records {
            matrix.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                r.kind,
                cell(&r.name),
                cell(&r.engine),
                cell(&r.reference),
                cell(&r.result),
                r.date,
                r.commit,
                cell(&r.command)
            ));
        }
    }
    let bench = sheets
        .iter()
        .map(|(_, sheet)| speed(sheet, &describe(sheet)))
        .collect::<String>();
    Ok((matrix, summary, bench))
}

/// The time of a `bench` result ("16.542 s, …"), in seconds or, below
/// one, milliseconds.
fn seconds(r: &Record) -> String {
    let time: Option<f64> = r
        .result
        .split(" s,")
        .next()
        .and_then(|t| t.trim().parse().ok());
    match time {
        Some(t) if t >= 1.0 => format!("{t:.1} s"),
        Some(t) => format!("{:.0} ms", t * 1000.0),
        None => "?".to_string(),
    }
}

/// A suite result for the README: the result, the mode and the reference
/// in a few words ("470/470 lockstep, qemu", "412/470 under fex, qemu").
fn suite_label(r: &Record) -> String {
    if r.name != "difftest" {
        return format!(
            "{} {}",
            r.name,
            r.result.split(" (").next().unwrap_or_default()
        );
    }
    // The same cases under another engine (`difftest --engine`).
    let mode = if !r.engine.starts_with("armor") {
        format!("under {}", r.engine)
    } else if r.engine.contains("lockstep") {
        "lockstep".to_string()
    } else if r.engine.contains("ARMv8.0") {
        "ARMv8.0 paths".to_string()
    } else {
        "interpreter and JIT".to_string()
    };
    let reference = if r.reference.starts_with("x86-64 hardware") {
        "a real x86 CPU"
    } else if r.reference.contains("AArch64") {
        "native builds"
    } else if r.reference.contains("saved") {
        "qemu's saved results"
    } else {
        "qemu"
    };
    format!("{} {mode}, {reference}", r.result)
}

/// The README's speed table for one machine: each engine's time as a
/// multiple of the native build's, the fastest translator in bold.
fn speed(sheet: &Sheet, machine: &str) -> String {
    let rows: Vec<&Record> = sheet
        .records
        .iter()
        .filter(|r| r.kind == "performance")
        .collect();
    let Some(first) = rows.first() else {
        return String::new();
    };
    // armor first, then the others as recorded.
    let mut engines: Vec<&str> = vec![];
    for r in rows.iter().filter(|r| r.engine.starts_with("armor")) {
        if !engines.contains(&r.engine.as_str()) {
            engines.push(&r.engine);
        }
    }
    for r in &rows {
        if !engines.contains(&r.engine.as_str()) {
            engines.push(&r.engine);
        }
    }
    let mut workloads: Vec<&str> = Vec::new();
    for r in &rows {
        if !workloads.contains(&r.name.as_str()) {
            workloads.push(&r.name);
        }
    }
    let multiple = |r: &Record| -> Option<f64> {
        let (_, rest) = r.result.split_once(", ")?;
        rest.split('×').next()?.trim().parse().ok()
    };
    let mut table = format!(
        "\n{} — `{}`, {}, commit {}. Time as a multiple of the native ARM64 build's (lower is better) and in seconds, median of 5:\n\n| Workload |",
        cell(machine),
        cell(&first.command),
        first.date,
        first.commit
    );
    for engine in &engines {
        let name = engine.trim_end_matches(" --jit").replace(" --jit,", ",");
        table.push_str(&format!(" {} |", cell(&name)));
    }
    table.push_str("\n|---|");
    table.push_str(&"---:|".repeat(engines.len()));
    table.push('\n');
    for workload in workloads {
        let found: Vec<Option<&Record>> = engines
            .iter()
            .map(|e| {
                rows.iter()
                    .copied()
                    .find(|r| r.name == workload && r.engine == *e)
            })
            .collect();
        let best = found
            .iter()
            .flatten()
            .filter_map(|r| multiple(r))
            .fold(f64::INFINITY, f64::min);
        table.push_str(&format!("| {} |", cell(workload)));
        for r in found {
            let text = match r {
                None => "—".to_string(),
                Some(r) => match multiple(r) {
                    Some(m) if m == best => format!("**{m:.2}×** ({})", seconds(r)),
                    Some(m) => format!("{m:.2}× ({})", seconds(r)),
                    None => cell(r.result.split(" (").next().unwrap_or_default()),
                },
            };
            table.push_str(&format!(" {text} |"));
        }
        table.push('\n');
    }
    table
}

/// `cargo xtask matrix [--check]`.
pub fn run(root: &Path, check: bool) -> Result<(), String> {
    let (matrix, summary, bench) = render(root)?;
    let bench = format!(
        "<!-- Generated by `cargo xtask matrix` from docs/results/ (ADR-0045). Do not edit. -->\n{bench}"
    );
    for (file, marker, block) in [
        ("docs/COMPATIBILITY_MATRIX.md", "matrix", &matrix),
        ("README.md", "matrix", &summary),
        ("README.md", "bench", &bench),
    ] {
        let path = root.join(file);
        let text = fs::read_to_string(&path).map_err(|e| format!("{file}: {e}"))?;
        let (start, end) = (
            format!("<!-- {marker}:start -->"),
            format!("<!-- {marker}:end -->"),
        );
        let wrapped = format!("{start}\n{block}{end}");
        let updated = replace_between(&text, &wrapped, &start, &end)
            .map_err(|_| format!("{file} needs the markers {start} and {end}"))?;
        if updated == text {
            println!("{file} {marker} is up to date");
        } else if check {
            return Err(format!(
                "{file} {marker} is stale: run `cargo xtask matrix`"
            ));
        } else {
            fs::write(&path, updated).map_err(|e| format!("{file}: {e}"))?;
            println!("{file} {marker} updated");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let r = Record {
            kind: "suite".into(),
            name: "difftest".into(),
            engine: "armor (interpreter, JIT)".into(),
            reference: "qemu-x86_64".into(),
            result: "470/470".into(),
            date: "2026-10-15".into(),
            commit: "d40b290".into(),
            command: "cargo xtask difftest".into(),
        };
        assert_eq!(Record::parse(&r.line()), Some(r));
    }

    #[test]
    fn machine_ids_are_file_names() {
        let m = Machine {
            cpu: "Cortex-A76".into(),
            pages: 16384,
            system: "Debian GNU/Linux 13 (trixie)".into(),
            place: "kvm".into(),
            kernel: "6.12".into(),
        };
        assert_eq!(m.id(), "cortex-a76-16k-debian-gnu-linux-13-kvm");
    }
}
