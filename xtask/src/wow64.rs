//! `cargo xtask wow64`: armor's WoW64 CPU backend (ADR-0047), the DLL that
//! Wine's wow64 loads as `HODLL` to run 32-bit x86 Windows code.
//!
//! `wow64/` is its own workspace for `aarch64-pc-windows-msvc`, linked by
//! LLVM's `lld-link` against import libraries from `llvm-dlltool` (no
//! Windows SDK). The DLL is built into `target/wow64-build/` and written to
//! `target/wow64/armor-wow64.dll` with the mark Wine's loader requires of a
//! builtin DLL, which `armor run` copies into the prefix's `system32`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "aarch64-pc-windows-msvc";
/// Wine loads a DLL into a builtin's place only when the DOS stub carries
/// this at offset 64 (Wine's `winebuild` writes it into every builtin).
const BUILTIN_MARK: &[u8] = b"Wine builtin DLL\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
const MARK_AT: usize = 64;

/// Where the marked DLL is written.
pub fn dll_path(root: &Path) -> PathBuf {
    root.join("target/wow64/armor-wow64.dll")
}

pub fn run(root: &Path) -> Result<(), String> {
    let dll = build(root)?;
    println!("built armor's WoW64 backend -> {}", dll.display());
    Ok(())
}

/// Builds the DLL (release) and writes the marked copy.
pub fn build(root: &Path) -> Result<PathBuf, String> {
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--target-dir",
            "../target/wow64-build",
        ])
        .current_dir(root.join("wow64"))
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "building the WoW64 backend failed: {status} (it needs `rustup target add {TARGET}`, \
             lld-link and llvm-dlltool: lld and llvm)"
        ));
    }
    let built = root.join(format!(
        "target/wow64-build/{TARGET}/release/armor_wow64.dll"
    ));
    let mut bytes = fs::read(&built).map_err(|e| format!("{}: {e}", built.display()))?;
    mark_builtin(&mut bytes)?;
    let out = dll_path(root);
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(&out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(out)
}

/// Writes Wine's builtin mark into the DOS stub, which must leave room for
/// it before the PE header.
pub(crate) fn mark_builtin(pe: &mut [u8]) -> Result<(), String> {
    let end = MARK_AT + BUILTIN_MARK.len();
    if pe.len() < 0x40 || &pe[..2] != b"MZ" {
        return Err("the DLL is not a PE file".to_string());
    }
    let pe_header = u32::from_le_bytes(pe[0x3c..0x40].try_into().expect("4 bytes")) as usize;
    if pe_header < end || pe.len() < end {
        return Err(format!(
            "the DLL's PE header at {pe_header:#x} leaves no room for the builtin mark"
        ));
    }
    pe[MARK_AT..end].copy_from_slice(BUILTIN_MARK);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_goes_into_the_dos_stub() {
        let mut pe = vec![0u8; 0x100];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c] = 0x78;
        mark_builtin(&mut pe).unwrap();
        assert_eq!(&pe[64..80], b"Wine builtin DLL");
        assert!(pe[80..96].iter().all(|&b| b == 0));
        pe[0x3c] = 0x40;
        assert!(mark_builtin(&mut pe).is_err());
    }
}
