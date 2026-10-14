//! busybox as a real-world guest program (milestone M2).
//!
//! `cargo xtask fetch-busybox` downloads the official Debian package
//! `busybox-static` for amd64 (pinned version and SHA-256), extracts it in
//! an empty directory with `ar` and `tar`, and copies only the binary to
//! `target/guest/busybox/busybox`. Nothing from the package is executed
//! outside the emulators. The binary is not committed (docs/testing.md).
//!
//! The applets `difftest` runs are listed in `tests/guest/busybox.txt`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Paths;
use crate::deb::{self, Package, fresh_dir};

/// Debian trixie, `busybox-static` 1:1.37.0-6+b9, amd64. Values from the
/// Packages index of deb.debian.org (2026-10-12).
const PACKAGE: Package = Package {
    pool_path: "pool/main/b/busybox/busybox-static_1.37.0-6+b9_amd64.deb",
    sha256: "598a3fd92bdafc34cd81196b2952ad36e910e47a4ecf162f8ce5e8e262598e53",
};
/// Path of the binary inside the package's data archive.
const BINARY_IN_PACKAGE: &str = "./usr/bin/busybox";

/// One applet run: `busybox <args...>`.
pub struct Case {
    pub name: String,
    pub args: Vec<String>,
}

pub fn binary(paths: &Paths) -> PathBuf {
    paths.root.join("target/guest/busybox/busybox")
}

/// Working directory and stdin of every busybox case.
pub fn data_dir(paths: &Paths) -> PathBuf {
    paths.root.join("tests/guest/data")
}

pub const STDIN_FILE: &str = "fruits.txt";

pub fn fetch() -> Result<(), String> {
    let paths = Paths::new();
    let work = paths.root.join("target/guest/busybox-download");
    let scratch = work.join("deb");
    fresh_dir(&work)?;
    fresh_dir(&scratch)?;
    let deb = deb::download(&PACKAGE, &work)?;
    deb::extract(&deb, &scratch, &work, &[BINARY_IN_PACKAGE])?;

    let target = binary(&paths);
    fs::create_dir_all(target.parent().expect("binary path has a parent"))
        .map_err(|e| format!("cannot create the busybox directory: {e}"))?;
    fs::copy(work.join(BINARY_IN_PACKAGE), &target)
        .map_err(|e| format!("cannot copy busybox: {e}"))?;
    fs::remove_dir_all(&work).map_err(|e| format!("cannot clean {}: {e}", work.display()))?;
    println!("busybox ready: {}", target.display());
    Ok(())
}

/// Reads `tests/guest/busybox.txt`: one case per line, `name: args...`,
/// arguments separated by spaces; `#` starts a comment line.
pub fn cases(paths: &Paths) -> Result<Vec<Case>, String> {
    let list = paths.root.join("tests/guest/busybox.txt");
    let text =
        fs::read_to_string(&list).map_err(|e| format!("cannot read {}: {e}", list.display()))?;
    parse_cases(&text, "busybox/")
}

/// Parses a case list: one case per line, `name: args...`, arguments
/// separated by spaces, or enclosed in double or single quotes to contain
/// spaces (no escapes; each kind of quote can contain the other); `#`
/// starts a comment line. Case names get `prefix`.
pub fn parse_cases(text: &str, prefix: &str) -> Result<Vec<Case>, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, args) = line
                .split_once(':')
                .ok_or_else(|| format!("expected `name: args`, got `{line}`"))?;
            Ok(Case {
                name: format!("{prefix}{}", name.trim()),
                args: split_arguments(args)
                    .ok_or_else(|| format!("unterminated quote in `{line}`"))?,
            })
        })
        .collect()
}

/// Splits on spaces, keeping `"..."` and `'...'` together (quotes removed).
fn split_arguments(text: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut chars = text.trim().chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut arg = String::new();
        if c == '"' || c == '\'' {
            chars.next();
            loop {
                match chars.next()? {
                    end if end == c => break,
                    other => arg.push(other),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                arg.push(c);
                chars.next();
            }
        }
        args.push(arg);
    }
    Some(args)
}

/// Whether the busybox binary has been fetched.
pub fn available(paths: &Paths) -> bool {
    Path::new(&binary(paths)).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases_are_parsed() {
        let cases = parse_cases(
            "# comment\n\necho: echo hello world\ntrue: true\n",
            "busybox/",
        )
        .unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].name, "busybox/echo");
        assert_eq!(cases[0].args, ["echo", "hello", "world"]);
        assert_eq!(cases[1].args, ["true"]);
        assert!(parse_cases("no colon here", "busybox/").is_err());
        let quoted = parse_cases("sh: sh -c \"echo a  b\" x", "busybox/").unwrap();
        assert_eq!(quoted[0].args, ["sh", "-c", "echo a  b", "x"]);
        assert!(parse_cases("bad: sh -c \"open", "busybox/").is_err());
        let single = parse_cases("awk: awk '{ print \"a b\" }'", "x/").unwrap();
        assert_eq!(single[0].args, ["awk", "{ print \"a b\" }"]);
    }
}
