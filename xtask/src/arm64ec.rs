//! `cargo xtask arm64ec`: armor's ARM64EC emulator (ADR-0047, stage 2),
//! the DLL that Wine's ntdll loads as `HODLL64` to run x86-64 Windows
//! code.
//!
//! `arm64ec/` is its own workspace for `arm64ec-pc-windows-msvc`, linked by
//! LLVM's `lld-link` against an import library from `llvm-dlltool` (no
//! Windows SDK, no MSVC). The DLL is built into `target/arm64ec-build/`
//! and written to `target/arm64ec/armor-arm64ec.dll` with the mark Wine's
//! loader requires of a builtin DLL, as the WoW64 backend is.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "arm64ec-pc-windows-msvc";

/// Where the marked DLL is written.
pub fn dll_path(root: &Path) -> PathBuf {
    root.join("target/arm64ec/armor-arm64ec.dll")
}

pub fn run(root: &Path) -> Result<(), String> {
    let dll = build(root)?;
    println!("built armor's ARM64EC emulator -> {}", dll.display());
    Ok(())
}

/// Builds the DLL (release) and writes the marked copy.
pub fn build(root: &Path) -> Result<PathBuf, String> {
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--target-dir",
            "../target/arm64ec-build",
        ])
        .current_dir(root.join("arm64ec"))
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "building the ARM64EC emulator failed: {status} (it needs `rustup target add \
             {TARGET}`, lld-link and llvm-dlltool: lld and llvm)"
        ));
    }
    let built = root.join(format!(
        "target/arm64ec-build/{TARGET}/release/armor_arm64ec.dll"
    ));
    let mut bytes = fs::read(&built).map_err(|e| format!("{}: {e}", built.display()))?;
    crate::wow64::mark_builtin(&mut bytes)?;
    let out = dll_path(root);
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(&out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(out)
}
