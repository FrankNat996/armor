//! `cargo xtask encoder-check`: compares armor's AArch64 encoder with the
//! GNU assembler (ADR-0021). Every sample of `armor_jit::check` is written
//! as assembly, assembled by the host's `as`, and the resulting words must
//! equal the encoder's.

use std::fs;
use std::process::Command;

use armor_jit::check::{Sample, samples};

use crate::{Paths, exec};

pub fn run() -> Result<(), String> {
    let paths = Paths::new();
    let work = paths.root.join("target/encoder-check");
    fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    let samples = samples();
    let reference = assemble(&work, &samples)?;
    let expected: usize = samples.iter().map(|s| s.words.len()).sum();
    if reference.len() != expected {
        // An assembler alias expanded to a different number of words:
        // find which sample by assembling them one at a time.
        for sample in &samples {
            let words = assemble(&work, std::slice::from_ref(sample))?;
            if words.len() != sample.words.len() {
                return Err(format!(
                    "`{}`: the assembler emits {} words, the encoder {}",
                    sample.text.replace('\n', "; "),
                    words.len(),
                    sample.words.len()
                ));
            }
        }
        return Err("word counts differ, but no single sample explains it".to_string());
    }
    let mut at = 0;
    let mut mismatches = 0;
    for sample in &samples {
        let theirs = &reference[at..at + sample.words.len()];
        at += sample.words.len();
        if theirs != sample.words.as_slice() {
            mismatches += 1;
            if mismatches <= 20 {
                println!(
                    "MISMATCH `{}`: assembler {}, encoder {}",
                    sample.text.replace('\n', "; "),
                    hex(theirs),
                    hex(&sample.words)
                );
            }
        }
    }
    println!(
        "{} samples, {expected} instructions, {mismatches} mismatches",
        samples.len()
    );
    if mismatches == 0 {
        Ok(())
    } else {
        Err(format!("{mismatches} encoder mismatches"))
    }
}

fn hex(words: &[u32]) -> String {
    words
        .iter()
        .map(|w| format!("{w:08x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Assembles the samples' text with `as` and returns the words of .text.
fn assemble(work: &std::path::Path, samples: &[Sample]) -> Result<Vec<u32>, String> {
    let source = work.join("samples.s");
    let object = work.join("samples.o");
    let binary = work.join("samples.bin");
    // The Raspberry Pi 5's architecture, with LDAPR (ADR-0023).
    let text: String = std::iter::once(".arch armv8.2-a+rcpc\n".to_string())
        .chain(samples.iter().map(|s| format!("{}\n", s.text)))
        .collect();
    fs::write(&source, text).map_err(|e| format!("cannot write {}: {e}", source.display()))?;
    let output = exec(Command::new("as").arg("-o").arg(&object).arg(&source))?;
    if !output.status.success() {
        return Err(format!(
            "the assembler rejected a sample:\n{}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .take(10)
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    let output = exec(
        Command::new("objcopy")
            .args(["-O", "binary", "-j", ".text"])
            .arg(&object)
            .arg(&binary),
    )?;
    if !output.status.success() {
        return Err(format!(
            "objcopy failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let bytes = fs::read(&binary).map_err(|e| format!("cannot read {}: {e}", binary.display()))?;
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect())
}
