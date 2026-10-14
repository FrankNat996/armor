//! `armor inspect` through the real binary: argument handling, exit codes
//! and output. The verdict logic itself is unit-tested in src/inspect.rs.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn armor(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_armor"))
        .args(args)
        .output()
        .expect("armor runs")
}

/// A file with `contents` in a directory private to this test.
fn temp_file(name: &str, contents: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("armor-inspect-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn inspect_prints_a_report_and_exits_zero() {
    let path = temp_file("script.sh", b"#!/bin/sh\necho hi\n");
    let out = armor(&["inspect", path.to_str().unwrap()]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("format:      script\n"), "{text}");
    assert!(text.contains("interpreter: /bin/sh\n"), "{text}");
    assert!(
        text.contains("verdict:     recognized, not loadable\n"),
        "{text}"
    );
}

#[test]
fn inspect_of_an_unreadable_file_is_an_armor_error() {
    let out = armor(&["inspect", "/nonexistent/armor-test-file"]);
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot read"));
}

#[test]
fn inspect_needs_exactly_one_file() {
    assert_eq!(armor(&["inspect"]).status.code(), Some(125));
    assert_eq!(armor(&["inspect", "a", "b"]).status.code(), Some(125));
}
