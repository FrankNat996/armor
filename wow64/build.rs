//! Import libraries for ntdll and wow64 from their `.def` files, made by
//! LLVM's `llvm-dlltool` (no Windows SDK).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for name in ["ntdll", "wow64"] {
        let def = format!("{name}.def");
        println!("cargo:rerun-if-changed={def}");
        let status = Command::new("llvm-dlltool")
            .args(["-m", "arm64", "-d", &def, "-l"])
            .arg(out.join(format!("{name}.lib")))
            .status()
            .expect("llvm-dlltool");
        assert!(status.success(), "llvm-dlltool failed for {def}");
    }
    println!("cargo:rustc-link-search=native={}", out.display());
}
