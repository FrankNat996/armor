//! How `armor <program>` chooses the execution path (ADR-0028), through
//! the real binary: the JIT by default for x86-64 and i386 (ADR-0029),
//! native AArch64 programs and scripts with a native interpreter run
//! directly, anything else is refused with a suggestion.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

fn armor(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_armor"))
        .args(args)
        .env("ARMOR_JIT_STATS", "1")
        .env_remove("ARMOR_ROOT")
        .output()
        .expect("armor runs")
}

/// An executable file with `contents` in a directory private to this test.
fn temp_file(name: &str, contents: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("armor-run-paths-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A minimal static x86-64 executable: `exit(7)`, one PT_LOAD at
/// 0x400000 with the code right after the headers.
fn x86_64_exit_7() -> Vec<u8> {
    const BASE: u64 = 0x40_0000;
    const CODE_OFFSET: u64 = 64 + 56;
    // mov eax, 60; mov edi, 7; syscall
    let code = [0xb8, 60, 0, 0, 0, 0xbf, 7, 0, 0, 0, 0x0f, 0x05];
    let mut f = vec![0u8; 64];
    f[..4].copy_from_slice(b"\x7fELF");
    f[4] = 2; // 64-bit
    f[5] = 1; // little-endian
    f[6] = 1; // version
    f[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    f[18..20].copy_from_slice(&62u16.to_le_bytes()); // x86-64
    f[20..24].copy_from_slice(&1u32.to_le_bytes());
    f[24..32].copy_from_slice(&(BASE + CODE_OFFSET).to_le_bytes()); // entry
    f[32..40].copy_from_slice(&64u64.to_le_bytes()); // phoff
    f[52..54].copy_from_slice(&64u16.to_le_bytes()); // ehsize
    f[54..56].copy_from_slice(&56u16.to_le_bytes()); // phentsize
    f[56..58].copy_from_slice(&1u16.to_le_bytes()); // phnum
    let size = CODE_OFFSET + code.len() as u64;
    let mut ph = vec![0u8; 56];
    ph[..4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    ph[4..8].copy_from_slice(&5u32.to_le_bytes()); // R + X
    ph[16..24].copy_from_slice(&BASE.to_le_bytes()); // vaddr
    ph[24..32].copy_from_slice(&BASE.to_le_bytes()); // paddr
    ph[32..40].copy_from_slice(&size.to_le_bytes()); // filesz
    ph[40..48].copy_from_slice(&size.to_le_bytes()); // memsz
    ph[48..56].copy_from_slice(&0x1000u64.to_le_bytes()); // align
    f.extend(ph);
    f.extend(code);
    f
}

/// The i386 version (ADR-0029): `exit(7)` through `int 0x80`, ELF32 at
/// 0x8048000.
fn i386_exit_7() -> Vec<u8> {
    const BASE: u32 = 0x804_8000;
    const CODE_OFFSET: u32 = 52 + 32;
    // mov eax, 1; mov ebx, 7; int 0x80
    let code = [0xb8, 1, 0, 0, 0, 0xbb, 7, 0, 0, 0, 0xcd, 0x80];
    let mut f = vec![0u8; 52];
    f[..4].copy_from_slice(b"\x7fELF");
    f[4] = 1; // 32-bit
    f[5] = 1; // little-endian
    f[6] = 1; // version
    f[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    f[18..20].copy_from_slice(&3u16.to_le_bytes()); // i386
    f[20..24].copy_from_slice(&1u32.to_le_bytes());
    f[24..28].copy_from_slice(&(BASE + CODE_OFFSET).to_le_bytes()); // entry
    f[28..32].copy_from_slice(&52u32.to_le_bytes()); // phoff
    f[40..42].copy_from_slice(&52u16.to_le_bytes()); // ehsize
    f[42..44].copy_from_slice(&32u16.to_le_bytes()); // phentsize
    f[44..46].copy_from_slice(&1u16.to_le_bytes()); // phnum
    let size = CODE_OFFSET + code.len() as u32;
    // type, offset, vaddr, paddr, filesz, memsz, flags (R + X), align
    for field in [1, 0, BASE, BASE, size, size, 5, 0x1000] {
        f.extend(field.to_le_bytes());
    }
    f.extend(code);
    f
}

#[test]
fn i386_programs_use_the_jit_too() {
    let path = temp_file("exit7-i386", &i386_exit_7());
    let path = path.to_str().unwrap();
    // mov eax, 1; mov ebx, 7 translated; int 0x80 interpreted.
    let out = armor(&[path]);
    assert_eq!(out.status.code(), Some(7));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("jit stats: 1 blocks"), "{stderr}");
    let out = Command::new(env!("CARGO_BIN_EXE_armor"))
        .args(["--diagnose", path])
        .env_remove("ARMOR_ROOT")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
    let report = String::from_utf8_lossy(&out.stderr);
    assert!(report.contains("i386 under armor, JIT"), "{report}");
}

#[test]
fn x86_64_programs_use_the_jit_by_default() {
    let path = temp_file("exit7", &x86_64_exit_7());
    let path = path.to_str().unwrap();
    let out = armor(&[path]);
    assert_eq!(out.status.code(), Some(7));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("jit stats: 1 blocks"), "{stderr}");
    // The interpreter alone on request: nothing translated.
    let out = armor(&["--interpreter", path]);
    assert_eq!(out.status.code(), Some(7));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("jit stats: 0 blocks"), "{stderr}");
}

#[test]
fn native_programs_and_scripts_run_directly() {
    let out = armor(&["/bin/echo", "native"]);
    assert!(out.status.success());
    assert_eq!(out.stdout, b"native\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("native AArch64 program"), "{stderr}");

    let script = temp_file("script.sh", b"#!/bin/sh\necho \"script $1\"\n");
    let out = armor(&[script.to_str().unwrap(), "arg"]);
    assert!(out.status.success());
    assert_eq!(out.stdout, b"script arg\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("interpreter is not an x86-64"), "{stderr}");
}

/// A static x86-64 executable that runs `execve(path, [path, arg], NULL)`
/// and exits with 99 if it returns.
fn x86_64_execve(path: &str, arg: &str) -> Vec<u8> {
    let mut f = x86_64_exit_7();
    let base = 0x40_0000u64;
    let code_offset = 64 + 56u64;
    // mov rdi, path; mov rsi, argv; xor edx, edx; mov eax, 59; syscall;
    // mov edi, 99; mov eax, 60; syscall
    let code_len = 10 + 10 + 2 + 5 + 2 + 5 + 5 + 2;
    let data = base + code_offset + code_len;
    let path_at = data + 24;
    let arg_at = path_at + path.len() as u64 + 1;
    let mut code = vec![0x48, 0xbf];
    code.extend(path_at.to_le_bytes());
    code.extend([0x48, 0xbe]);
    code.extend(data.to_le_bytes());
    code.extend([0x31, 0xd2, 0xb8, 59, 0, 0, 0, 0x0f, 0x05]);
    code.extend([0xbf, 99, 0, 0, 0, 0xb8, 60, 0, 0, 0, 0x0f, 0x05]);
    assert_eq!(code.len() as u64, code_len);
    code.extend(path_at.to_le_bytes());
    code.extend(arg_at.to_le_bytes());
    code.extend(0u64.to_le_bytes());
    code.extend(path.as_bytes());
    code.push(0);
    code.extend(arg.as_bytes());
    code.push(0);
    f.truncate(code_offset as usize);
    f.extend(&code);
    let size = (code_offset as usize + code.len()) as u64;
    // filesz and memsz of the single PT_LOAD.
    f[64 + 32..64 + 40].copy_from_slice(&size.to_le_bytes());
    f[64 + 40..64 + 48].copy_from_slice(&size.to_le_bytes());
    f
}

#[test]
fn host_system_scripts_use_the_host_interpreter() {
    // A guest root whose /bin/sh is an x86-64 program (exit 7): a script
    // of its own, or one outside the system directories, runs under it;
    // a script of the host system (/usr/bin/which, `#! /bin/sh`) runs
    // with the host's /bin/sh.
    let root = std::env::temp_dir().join(format!("armor-run-paths-root-{}", std::process::id()));
    fs::create_dir_all(root.join("bin")).unwrap();
    let sh = root.join("bin/sh");
    fs::write(&sh, x86_64_exit_7()).unwrap();
    fs::set_permissions(&sh, fs::Permissions::from_mode(0o755)).unwrap();
    let root = root.to_str().unwrap();

    let out = armor(&["--root", root, "/usr/bin/which", "sh"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).ends_with("/sh\n"),
        "{out:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("script of the host system"), "{stderr}");

    let script = temp_file("own.sh", b"#!/bin/sh\necho host\n");
    let out = armor(&["--root", root, script.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(7), "{out:?}");

    // The same from a guest's execve.
    let exec = temp_file("exec-which", &x86_64_execve("/usr/bin/which", "sh"));
    let out = armor(&["--root", root, exec.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).ends_with("/sh\n"),
        "{out:?}"
    );
    let exec = temp_file("exec-own", &x86_64_execve(script.to_str().unwrap(), "x"));
    let out = armor(&["--root", root, exec.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(7), "{out:?}");
}

/// The x86-64 executable of `x86_64_exit_7` with `code` in place of its
/// code.
fn x86_64_with_code(code: &[u8]) -> Vec<u8> {
    let code_offset = 64 + 56;
    let mut f = x86_64_exit_7();
    f.truncate(code_offset);
    f.extend(code);
    let size = f.len() as u64;
    // filesz and memsz of the single PT_LOAD.
    f[64 + 32..64 + 40].copy_from_slice(&size.to_le_bytes());
    f[64 + 40..64 + 48].copy_from_slice(&size.to_le_bytes());
    f
}

#[test]
fn a_guest_killed_by_a_fault_leaves_a_small_core() {
    // xor ecx, ecx; mov eax, 1; div ecx: #DE, so SIGFPE with a core dump.
    let program = temp_file(
        "divide-by-zero",
        &x86_64_with_code(&[0x31, 0xc9, 0xb8, 1, 0, 0, 0, 0xf7, 0xf1]),
    );
    let dir = std::env::temp_dir().join(format!("armor-core-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    // Through a shell, for `ulimit -c`: cores on, as where the kernel pipes
    // them to a handler, which ignores the limit.
    let out = Command::new("sh")
        .arg("-c")
        .arg("ulimit -c unlimited; exec \"$0\" \"$1\"")
        .arg(env!("CARGO_BIN_EXE_armor"))
        .arg(&program)
        .current_dir(&dir)
        .env_remove("ARMOR_ROOT")
        .output()
        .unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(out.status.signal(), Some(8), "{out:?}");
    // The guest's own SIGABRT (glibc's abort), which the kernel delivers
    // to armor itself: getpid; kill(pid, SIGABRT).
    let program = temp_file(
        "abort",
        &x86_64_with_code(&[
            0xb8, 39, 0, 0, 0, 0x0f, 0x05, 0x89, 0xc7, 0xbe, 6, 0, 0, 0, 0xb8, 62, 0, 0, 0, 0x0f,
            0x05,
        ]),
    );
    let out = Command::new("sh")
        .arg("-c")
        .arg("ulimit -c unlimited; exec \"$0\" \"$1\"")
        .arg(env!("CARGO_BIN_EXE_armor"))
        .arg(&program)
        .current_dir(&dir)
        .env_remove("ARMOR_ROOT")
        .output()
        .unwrap();
    assert_eq!(out.status.signal(), Some(6), "{out:?}");
    // Where the kernel writes cores into the working directory (the
    // Raspberry Pi), armor's is the headers alone, not its address space
    // (34 GB before).
    for entry in fs::read_dir(&dir).unwrap() {
        let size = entry.unwrap().metadata().unwrap().len();
        assert!(size < 1 << 20, "a core of {size} bytes");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn syscalls_answered_as_a_kernel_configuration() {
    // ADR-0044: each syscall, with zero arguments, exits with the errno it
    // got: mov eax, N; syscall; neg eax; mov edi, eax; mov eax, 60; syscall.
    let cases: [(u32, i32, &str); 11] = [
        (101, 1, "ptrace: EPERM (Yama ptrace_scope 3)"),
        (425, 38, "io_uring_setup: ENOSYS"),
        (426, 38, "io_uring_enter: ENOSYS"),
        (323, 38, "userfaultfd: ENOSYS"),
        (447, 38, "memfd_secret: ENOSYS"),
        (462, 38, "mseal: ENOSYS"),
        (212, 38, "lookup_dcookie: ENOSYS"),
        (216, 38, "remap_file_pages: ENOSYS"),
        (274, 38, "get_robust_list: ENOSYS"),
        (219, 4, "restart_syscall: EINTR"),
        (453, 95, "map_shadow_stack: EOPNOTSUPP"),
    ];
    for (number, errno, what) in cases {
        let mut code = vec![0xb8];
        code.extend(number.to_le_bytes());
        code.extend([
            0x0f, 0x05, 0xf7, 0xd8, 0x89, 0xc7, 0xb8, 60, 0, 0, 0, 0x0f, 0x05,
        ]);
        let program = temp_file(&format!("answer-{number}"), &x86_64_with_code(&code));
        let out = armor(&[program.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(errno), "{what}: {out:?}");
    }
}

#[test]
fn seccomp_strict_mode_allows_write_and_kills_on_the_rest() {
    // prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT); write(1, "ok\n", 3);
    // getpid(): SIGKILL, as Linux does in strict mode.
    let mut code = vec![
        0xbf, 22, 0, 0, 0, 0xbe, 1, 0, 0, 0, 0xb8, 157, 0, 0, 0, 0x0f, 0x05, // prctl
        0xbf, 1, 0, 0, 0, // mov edi, 1
    ];
    // lea rsi, [rip + text]: the text is 21 bytes after this instruction.
    code.extend([0x48, 0x8d, 0x35, 21, 0, 0, 0]);
    code.extend([0xba, 3, 0, 0, 0, 0xb8, 1, 0, 0, 0, 0x0f, 0x05]); // write
    code.extend([0xb8, 39, 0, 0, 0, 0x0f, 0x05]); // getpid
    code.extend([0x31, 0xff]); // xor edi, edi (exit 0, not reached)
    code.extend(b"ok\n");
    let program = temp_file("seccomp-strict", &x86_64_with_code(&code));
    let out = armor(&[program.to_str().unwrap()]);
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(out.stdout, b"ok\n", "{out:?}");
    assert_eq!(out.status.signal(), Some(9), "{out:?}");
}

#[test]
fn other_files_are_refused_with_a_suggestion() {
    let data = temp_file("data.txt", b"hello\n");
    let out = armor(&[data.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot run"), "{stderr}");
    assert!(stderr.contains("suggestion:"), "{stderr}");

    let mut riscv = x86_64_exit_7();
    riscv[18..20].copy_from_slice(&243u16.to_le_bytes());
    let riscv = temp_file("riscv", &riscv);
    let out = armor(&[riscv.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("a build of the program for x86"),
        "{stderr}"
    );
}

#[test]
fn diagnose_reports_the_run_without_guest_data() {
    let path = temp_file("exit7-diagnose", &x86_64_exit_7());
    let path = path.to_str().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_armor"))
        .args(["--diagnose", path, "SECRET-ARGUMENT"])
        .env("SECRET_VARIABLE", "SECRET-VALUE")
        .env_remove("ARMOR_ROOT")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
    let report = String::from_utf8_lossy(&out.stderr);
    assert!(report.contains("armor diagnose report"), "{report}");
    assert!(report.contains("x86-64 under armor, JIT"), "{report}");
    assert!(report.contains("exit status 7"), "{report}");
    assert!(!report.contains("SECRET"), "{report}");

    // To a file, appended.
    let file = temp_file("report.txt", b"");
    let target = format!("--diagnose={}", file.display());
    let out = armor(&[&target, "--interpreter", path]);
    assert_eq!(out.status.code(), Some(7));
    let report = fs::read_to_string(&file).unwrap();
    assert!(
        report.contains("x86-64 under armor, interpreter"),
        "{report}"
    );
    assert!(!report.contains("jit:"), "{report}");
}

#[test]
fn profiles_are_found_applied_and_checked() {
    let config = std::env::temp_dir().join(format!("armor-profiles-{}", std::process::id()));
    let profiles = config.join("armor").join("profiles");
    fs::create_dir_all(&profiles).unwrap();
    fs::write(
        profiles.join("exit7-profiled.conf"),
        "# by program name\nengine = interpreter\ndiagnose = stderr\n",
    )
    .unwrap();
    fs::write(profiles.join("broken.conf"), "engine = fast\n").unwrap();
    let path = temp_file("exit7-profiled", &x86_64_exit_7());
    let path = path.to_str().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_armor"))
            .args(args)
            .env("XDG_CONFIG_HOME", &config)
            .env_remove("ARMOR_ROOT")
            .output()
            .unwrap()
    };

    // Found by the program's name: the interpreter, with a report.
    let out = run(&[path]);
    assert_eq!(out.status.code(), Some(7));
    let report = String::from_utf8_lossy(&out.stderr);
    assert!(report.contains("profile:   exit7-profiled"), "{report}");
    assert!(
        report.contains("x86-64 under armor, interpreter"),
        "{report}"
    );

    // The command line wins over the profile.
    let out = run(&["--jit", path]);
    let report = String::from_utf8_lossy(&out.stderr);
    assert!(report.contains("x86-64 under armor, JIT"), "{report}");

    // A broken profile, or a missing one asked for, stops armor.
    let out = run(&["--profile", "broken", path]);
    assert_eq!(out.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("line 1: engine \"fast\""), "{stderr}");
    let out = run(&["--profile", "missing", path]);
    assert_eq!(out.status.code(), Some(125));
}
