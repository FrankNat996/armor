//! armor's x86-64 `libGL.so.1` (ADR-0041) and `cargo xtask gl-check`.
//!
//! The library is built from `guest/libgl/` (the generated `gl_stubs.c`
//! and the GLX functions of `glx.c`) into `target/guest/libgl/libGL.so.1`;
//! `armor --host-gl` gives it to the guest in place of its own libGL.
//!
//! `gl-check` builds the programs of `tests/guest/gl/` and runs each one
//! three times in the games root (`fetch-rootfs --games`): under
//! `qemu-x86_64` and armor with the guest's software Mesa, and under
//! `armor --host-gl` with the host's OpenGL. The programs draw only what
//! every conforming renderer draws the same way and print a checksum of
//! the pixels, so the three outputs must be equal. It needs an X display
//! (`$DISPLAY`), so it is not part of `difftest`.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::{GUEST_CC, Paths, QEMU, QEMU_CPU, build_armor, exec, rootfs};

/// Where the library is built.
pub fn libgl_path(paths: &Paths) -> PathBuf {
    paths.guest_out.join("libgl/libGL.so.1")
}

/// Builds armor's `libGL.so.1` when a source in `guest/libgl/` is newer
/// than it (the generated stubs take about 20 s to compile).
pub fn build_libgl(paths: &Paths) -> Result<PathBuf, String> {
    let src = paths.root.join("guest/libgl");
    let out = libgl_path(paths);
    let mut sources = Vec::new();
    let mut newest = std::time::SystemTime::UNIX_EPOCH;
    for entry in fs::read_dir(&src).map_err(|e| format!("{}: {e}", src.display()))? {
        let path = entry.map_err(|e| format!("{}: {e}", src.display()))?.path();
        let modified = fs::metadata(&path)
            .and_then(|m| m.modified())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        newest = newest.max(modified);
        if path.extension().is_some_and(|e| e == "c") {
            sources.push(path);
        }
    }
    sources.sort();
    let built = fs::metadata(&out).and_then(|m| m.modified()).ok();
    if built.is_some_and(|built| built >= newest) {
        return Ok(out);
    }
    let dir = out.parent().expect("the library is inside a directory");
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let output = exec(
        Command::new(GUEST_CC)
            .args([
                "-shared",
                "-fPIC",
                "-O1",
                "-Wall",
                "-fvisibility=hidden",
                "-Wl,-soname,libGL.so.1",
                "-I",
            ])
            .arg(&src)
            .arg("-o")
            .arg(&out)
            .args(&sources),
    )?;
    if !output.status.success() {
        return Err(format!(
            "{GUEST_CC} failed on guest/libgl:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(out)
}

/// Builds the programs of `tests/guest/gl/` (dynamic, run in the games
/// root) and returns their paths.
fn build_tests(paths: &Paths) -> Result<Vec<PathBuf>, String> {
    let src = paths.root.join("tests/guest/gl");
    let mut sources: Vec<PathBuf> = fs::read_dir(&src)
        .map_err(|e| format!("{}: {e}", src.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "c"))
        .collect();
    sources.sort();
    let mut programs = Vec::new();
    for source in sources {
        let stem = source.file_stem().expect("a source file has a name");
        let out = paths.guest_out.join("gl").join(stem);
        fs::create_dir_all(out.parent().expect("inside target/guest/gl"))
            .map_err(|e| format!("cannot create target/guest/gl: {e}"))?;
        let output = exec(
            Command::new(GUEST_CC)
                .args(["-O2", "-Wall", "-o"])
                .arg(&out)
                .arg(&source),
        )?;
        if !output.status.success() {
            return Err(format!(
                "{GUEST_CC} failed on {}:\n{}",
                source.display(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        programs.push(out);
    }
    Ok(programs)
}

/// One run: its stdout, and the renderer it reported on stderr.
fn run_one(cmd: &mut Command, what: &str) -> Result<(String, String), String> {
    let output = exec(cmd)?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(format!(
            "{what} failed ({}):\n{stdout}{stderr}",
            output.status
        ));
    }
    let renderer = stderr
        .lines()
        .find_map(|l| l.strip_prefix("renderer: "))
        .unwrap_or("?")
        .to_string();
    Ok((stdout, renderer))
}

pub fn check(paths: &Paths) -> Result<(), String> {
    if env::var_os("DISPLAY").is_none() {
        return Err("gl-check needs an X display: set DISPLAY".to_string());
    }
    let root = rootfs::dir_of(paths, rootfs::Arch::Games);
    if !rootfs::available_of(paths, rootfs::Arch::Games) {
        return Err(
            "the games root is missing: run `cargo xtask fetch-rootfs --games`".to_string(),
        );
    }
    build_armor(paths)?;
    let libgl = build_libgl(paths)?;
    let mut failed = 0;
    for program in build_tests(paths)? {
        let name = program
            .file_name()
            .expect("a program has a name")
            .to_string_lossy()
            .into_owned();
        let qemu = run_one(
            Command::new(QEMU)
                .args(["-cpu", QEMU_CPU, "-L"])
                .arg(&root)
                .arg(&program),
            &format!("{name} under {QEMU}"),
        )?;
        let armor = |host_gl: bool| {
            let mut cmd = Command::new(&paths.armor);
            cmd.arg("--jit").arg("--root").arg(&root);
            if host_gl {
                cmd.arg("--host-gl").env("ARMOR_HOST_GL_STUBS", &libgl);
            }
            cmd.arg(&program);
            cmd
        };
        let guest = run_one(&mut armor(false), &format!("{name} under armor"))?;
        let host = run_one(&mut armor(true), &format!("{name} under armor --host-gl"))?;
        println!(
            "{name}: renderers: qemu {}, armor {}, armor --host-gl {}",
            qemu.1, guest.1, host.1
        );
        if qemu.0 == guest.0 && guest.0 == host.0 {
            println!("{name}: same pixels\n{}", qemu.0.trim_end());
        } else {
            failed += 1;
            println!(
                "{name}: DIFFERENT\n--- qemu\n{}--- armor\n{}--- armor --host-gl\n{}",
                qemu.0, guest.0, host.0
            );
        }
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(format!("gl-check: {failed} program(s) differ"))
    }
}
