//! The libraries the link needs, without a Windows SDK: ntdll's import
//! library from `ntdll.def`, made by LLVM's `llvm-dlltool` for ARM64EC, and
//! an empty `softintrin.lib`, which the ARM64EC target always asks for
//! (MSVC's emulation of x86 intrinsics, which nothing here uses).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=ntdll.def");
    let status = Command::new("llvm-dlltool")
        .args(["-m", "arm64ec", "-d", "ntdll.def", "-l"])
        .arg(out.join("ntdll.lib"))
        .status()
        .expect("llvm-dlltool");
    assert!(status.success(), "llvm-dlltool failed for ntdll.def");
    // An archive with no members.
    std::fs::write(out.join("softintrin.lib"), b"!<arch>\n").expect("softintrin.lib");
    println!("cargo:rustc-link-search=native={}", out.display());
}
