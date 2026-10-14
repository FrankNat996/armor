//! `cargo xtask fuzz`: the fuzz tests of the components that read guest
//! input (roadmap M3, docs/testing.md), run long. They are ordinary tests
//! (`fuzz_*` in armor-x86, armor-elf and armor-linux) that `cargo test`
//! runs briefly with a fixed seed; here they run in release with many
//! iterations and a new seed, printed so that a failure can be replayed
//! with `--seed`.

use std::env;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Paths;

const USAGE: &str = "usage: cargo xtask fuzz [--iterations N] [--seed S]";

/// Iterations of each fuzz test unless `--iterations` says otherwise.
const DEFAULT_ITERATIONS: u64 = 1_000_000;

/// The crates with fuzz tests.
const CRATES: &[&str] = &["armor-x86", "armor-elf", "armor-linux"];

pub fn run(args: &[String]) -> Result<(), String> {
    let (mut iterations, mut seed) = (DEFAULT_ITERATIONS, None);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || rest.next().and_then(|v| v.parse::<u64>().ok()).ok_or(USAGE);
        match arg.as_str() {
            "--iterations" => iterations = value()?,
            "--seed" => seed = Some(value()?),
            _ => return Err(USAGE.to_string()),
        }
    }
    let seed = seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64)
    });
    println!("fuzz: {iterations} iterations per test, seed {seed} (replay: --seed {seed})");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["test", "--release"]);
    for name in CRATES {
        cmd.args(["--package", name]);
    }
    let status = cmd
        .args(["fuzz_", "--", "--nocapture"])
        .env("ARMOR_FUZZ_ITERATIONS", iterations.to_string())
        .env("ARMOR_FUZZ_SEED", seed.to_string())
        .current_dir(&Paths::new().root)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("a fuzz test failed (seed {seed})"))
    }
}
