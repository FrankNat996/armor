//! `cargo xtask app-check [--report] [--x86-engine armor|fex] [id...]`: real applications installed
//! and run as a user would, unattended (ADR-0045).
//!
//! Each application's installer is pinned by SHA-256 (downloaded into
//! `target/apps/downloads/` when missing, where a URL is known). It is
//! installed with `armor install` into a throwaway data directory
//! (`target/app-check/<id>/`, with the Windows engine linked from the
//! user's), then checked under its own Xvfb display:
//! - **a window** with the application's title appears (level 1, "starts");
//! - **7-Zip**: the archive it makes of `tests/guest/` is byte-for-byte the
//!   one its native ARM64 build makes of the same files with the same
//!   options (level 3).
//!
//! Windows applications run their 32-bit code under FEX, or, with
//! `--x86-engine armor`, under armor's WoW64 backend (ADR-0047; the check
//! fails if its line at start is not in the log, since armor falls back to
//! FEX without it).
//!
//! The data directory is removed afterwards. `--report` records each result
//! in this machine's `docs/results/` file.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Paths, exec};

/// How an application is checked.
enum Check {
    /// A window whose title contains this text, within `wait` seconds of
    /// `armor run`.
    Window { title: &'static str, wait: u64 },
    /// 7-Zip for Windows: `7z.exe` at this path in the prefix, compared with
    /// the ARM64 build from the installer `arm64` in the same directory.
    SevenZipWindows {
        exe: &'static str,
        arm64: &'static str,
    },
    /// 7-Zip for Linux: `7zz` under armor against the native one from the
    /// archive `native` in the same directory.
    SevenZipLinux { native: &'static str },
}

struct App {
    id: &'static str,
    name: &'static str,
    engine: &'static str,
    /// The directory under `target/apps/downloads/` (or a path under
    /// `target/` for a file without a public URL).
    dir: &'static str,
    file: &'static str,
    url: Option<&'static str>,
    sha256: &'static str,
    /// Arguments for the installer, after `--`.
    install: &'static [&'static str],
    check: Check,
}

const APPS: &[App] = &[
    App {
        id: "7zip-linux",
        name: "7-Zip 26.04 for Linux x86-64 (7zz)",
        engine: "armor (JIT)",
        dir: "7zip-26.04",
        file: "7z2604-linux-x64.tar.xz",
        url: Some("https://github.com/ip7z/7zip/releases/download/26.04/7z2604-linux-x64.tar.xz"),
        sha256: "fc0327ba27e89bd086cf426dff17d77de582953cdbbc10a6576540a06853ffcd",
        install: &[],
        check: Check::SevenZipLinux {
            native: "7z2604-linux-arm64.tar.xz",
        },
    },
    App {
        id: "7zip-x64",
        name: "7-Zip 26.04 for Windows x64 (7z2604-x64.exe)",
        engine: "Hangover 11.16 (FEX ARM64EC)",
        dir: "7zip-26.04",
        file: "7z2604-x64.exe",
        url: Some("https://github.com/ip7z/7zip/releases/download/26.04/7z2604-x64.exe"),
        sha256: "d54bf805f9f3704d1e8db2fa3498ae7ef2df0312b40b558e7c71c734430a665d",
        install: &["/S"],
        check: Check::SevenZipWindows {
            exe: "C:\\Program Files\\7-Zip\\7z.exe",
            arm64: "7z2604-arm64.exe",
        },
    },
    App {
        id: "7zip-x86",
        name: "7-Zip 26.04 for Windows x86 (7z2604.exe)",
        engine: "Hangover 11.16 (FEX WoW64)",
        dir: "7zip-26.04",
        file: "7z2604.exe",
        url: Some("https://github.com/ip7z/7zip/releases/download/26.04/7z2604.exe"),
        sha256: "39bf65045153fc26c42fa9fe47afd8cd1b11671c7f11d031e738c9a13960c009",
        install: &["/S"],
        check: Check::SevenZipWindows {
            exe: "C:\\Program Files (x86)\\7-Zip\\7z.exe",
            arm64: "7z2604-arm64.exe",
        },
    },
    App {
        id: "notepad-plus-plus",
        name: "Notepad++ 8.9.8.1 for Windows x64",
        engine: "Hangover 11.16 (FEX ARM64EC)",
        dir: "notepad++-8.9.8.1",
        file: "npp.8.9.8.1.Installer.x64.exe",
        url: Some(
            "https://github.com/notepad-plus-plus/notepad-plus-plus/releases/download/v8.9.8.1/npp.8.9.8.1.Installer.x64.exe",
        ),
        sha256: "26f3bcced788fadbac4e7e577b430ea2ce6060cf3bcad993ef4612ecd7d743da",
        install: &["/S"],
        check: Check::Window {
            title: "Notepad++",
            wait: 180,
        },
    },
    App {
        id: "keepass2",
        name: "KeePass 2.61.1 for Windows (.NET)",
        engine: "Hangover 11.16 with Wine Mono",
        dir: "keepass-2.61.1",
        file: "KeePass-2.61.1-Setup.exe",
        url: Some(
            "https://sourceforge.net/projects/keepass/files/KeePass%202.x/2.61.1/KeePass-2.61.1-Setup.exe/download",
        ),
        sha256: "9923a8b228fe3d8fbe46979991f45d7e0d63f649766596da191eb879b293d38d",
        install: &["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"],
        check: Check::Window {
            title: "KeePass",
            wait: 240,
        },
    },
    App {
        id: "funkin",
        name: "Friday Night Funkin' 0.8.6 for Windows x64 (itch.io zip)",
        engine: "Hangover 11.16 (FEX ARM64EC)",
        dir: "../../itch",
        file: "funkin-windows.zip",
        url: None,
        sha256: "3b6e4d82951c7d122bccec0a0d226fc4559ccb5437192272d0dc0f6a237dfe1a",
        install: &[],
        check: Check::Window {
            title: "Friday Night Funkin",
            wait: 240,
        },
    },
    App {
        id: "xnviewmp",
        name: "XnView MP 1.12.2 (.deb, x86-64)",
        engine: "armor (JIT), Debian's x86-64 libraries",
        dir: "xnviewmp",
        file: "XnViewMP-linux-x64.deb",
        url: Some("https://download.xnview.com/XnViewMP-linux-x64.deb"),
        sha256: "7a20d10c386a3699bb60a7f4d18c4a8bc359564904f6df21d7a08783feb2efbe",
        install: &[],
        check: Check::Window {
            title: "XnView",
            wait: 240,
        },
    },
    App {
        id: "keepassxc",
        name: "KeePassXC 2.7.12 (AppImage, x86-64)",
        engine: "armor (JIT), Debian's x86-64 libraries",
        dir: "keepassxc-2.7.12",
        file: "KeePassXC-2.7.12-x86_64.AppImage",
        url: Some(
            "https://github.com/keepassxreboot/keepassxc/releases/download/2.7.12/KeePassXC-2.7.12-x86_64.AppImage",
        ),
        sha256: "564fe8b751b9ef7aa057e4d3d0b2878db24eaa0f6b1c855c82e699ab0913ae49",
        install: &[],
        check: Check::Window {
            title: "KeePassXC",
            wait: 240,
        },
    },
    // Sublime Text (build 4200, .tar.xz) is not here: under Xvfb it starts
    // but makes no window, where on a desktop it opens (checked by hand,
    // docs/results/by-hand.tsv); the cause is not known yet.
    App {
        id: "spotify",
        name: "Spotify 1.3.4.258 (.deb, x86-64, Chromium)",
        engine: "armor (JIT), Debian's x86-64 libraries",
        dir: "spotify",
        file: "spotify-client_1.3.4.258.gc60b10b0946a_amd64.deb",
        url: Some(
            "http://repository.spotify.com/pool/non-free/s/spotify-client/spotify-client_1.3.4.258.gc60b10b0946a_amd64.deb",
        ),
        sha256: "20f327441d11b202c82e65b69ec990ef631eb66605da15f41076c049acb718e4",
        install: &[],
        check: Check::Window {
            title: "Spotify",
            wait: 300,
        },
    },
];

/// The Xvfb display the checks use.
const DISPLAY: &str = ":91";

fn sha256(path: &Path) -> Result<String, String> {
    let out = exec(Command::new("sha256sum").arg(path))?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string())
}

/// The installer, downloaded if missing and checked against its SHA-256.
fn installer(paths: &Paths, app: &App) -> Result<PathBuf, String> {
    let dir = paths.root.join("target/apps/downloads").join(app.dir);
    let file = dir.join(app.file);
    if !file.is_file() {
        let url = app.url.ok_or_else(|| {
            format!(
                "{} is not here and has no public URL: put it there",
                file.display()
            )
        })?;
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let status = Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&file)
            .arg(url)
            .status()
            .map_err(|e| format!("curl: {e}"))?;
        if !status.success() {
            return Err(format!("downloading {url} failed"));
        }
    }
    let got = sha256(&file)?;
    if got != app.sha256 {
        return Err(format!(
            "{}: SHA-256 {got}, pinned {} (the publisher changed the file?)",
            file.display(),
            app.sha256
        ));
    }
    Ok(file)
}

/// An Xvfb server for the checks, stopped when dropped.
struct Display(Child);

impl Display {
    fn start() -> Result<Display, String> {
        let child = Command::new("Xvfb")
            .args([DISPLAY, "-screen", "0", "1280x800x24", "-nolisten", "tcp"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Xvfb: {e} (package xvfb)"))?;
        std::thread::sleep(Duration::from_secs(2));
        Ok(Display(child))
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The titles of the windows on the display.
fn window_titles() -> String {
    Command::new("xwininfo")
        .args(["-root", "-tree", "-display", DISPLAY])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// The environment of a check: the throwaway data and config directories.
struct Place {
    data: PathBuf,
    config: PathBuf,
    log: PathBuf,
    x86_engine: &'static str,
}

impl Place {
    fn new(paths: &Paths, app: &App, x86_engine: &'static str) -> Result<Place, String> {
        let base = paths.root.join("target/app-check").join(app.id);
        if base.exists() {
            fs::remove_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
        }
        let data = base.join("data");
        let config = base.join("config");
        fs::create_dir_all(data.join("armor")).map_err(|e| format!("{}: {e}", data.display()))?;
        fs::create_dir_all(&config).map_err(|e| format!("{}: {e}", config.display()))?;
        // The Windows engine, shared with the user's.
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let user_data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let engines = user_data.join("armor/engines");
        if engines.is_dir() {
            std::os::unix::fs::symlink(&engines, data.join("armor/engines"))
                .map_err(|e| format!("linking the engines: {e}"))?;
        }
        Ok(Place {
            data,
            config,
            log: base.join("log.txt"),
            x86_engine,
        })
    }

    fn armor(&self, paths: &Paths) -> Command {
        let mut cmd = Command::new(paths.root.join("target/release/armor"));
        // A temporary directory of its own: programs leave sockets there
        // that a later run would find (Sublime Text's single instance).
        let tmp = self
            .data
            .parent()
            .expect("inside target/app-check")
            .join("tmp");
        let _ = fs::create_dir_all(&tmp);
        cmd.env("XDG_DATA_HOME", &self.data)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("TMPDIR", tmp)
            .env("DISPLAY", DISPLAY)
            .env("ARMOR_WINDOWS_X86_ENGINE", self.x86_engine)
            .env_remove("WAYLAND_DISPLAY");
        cmd
    }

    fn remove(&self) {
        if let Some(base) = self.data.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

/// Runs `cmd` with its output in `log`, within `limit`.
fn run_logged(mut cmd: Command, log: &Path, limit: Duration) -> Result<bool, String> {
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| format!("{}: {e}", log.display()))?;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(file.try_clone().map_err(|e| e.to_string())?)
        .stderr(file)
        .spawn()
        .map_err(|e| format!("starting {cmd:?}: {e}"))?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok(status.success());
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Ends a program started under `setsid` and everything it started.
fn stop_group(child: &mut Child) {
    let pid = child.id().to_string();
    let _ = Command::new("kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .status();
    std::thread::sleep(Duration::from_secs(2));
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .status();
    let _ = child.wait();
}

/// The installed application's name (`armor list --porcelain`'s app lines).
fn installed_name(paths: &Paths, place: &Place) -> Option<String> {
    let out = place
        .armor(paths)
        .args(["list", "--porcelain"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
        l.strip_prefix("app\t")
            .map(|r| r.split('\t').next().unwrap_or_default().to_string())
    })
}

/// Installs `file` with armor: true if it worked.
fn install(paths: &Paths, place: &Place, file: &Path, args: &[&str]) -> Result<bool, String> {
    let mut cmd = place.armor(paths);
    cmd.arg("install").arg(file);
    if !args.is_empty() {
        cmd.arg("--").args(args);
    }
    run_logged(cmd, &place.log, Duration::from_secs(1800))
}

/// The window check: `armor run <name>` until a window titled `title`.
fn check_window(
    paths: &Paths,
    place: &Place,
    name: &str,
    title: &str,
    wait: u64,
) -> Result<String, String> {
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&place.log)
        .map_err(|e| e.to_string())?;
    let run = place.armor(paths);
    let program = run.get_program().to_owned();
    let mut cmd = Command::new("setsid");
    cmd.arg(program).args(["run", name]);
    for (k, v) in run.get_envs() {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|e| format!("armor run: {e}"))?;
    let start = Instant::now();
    let mut found = None;
    while start.elapsed() < Duration::from_secs(wait) {
        if window_titles().contains(title) {
            found = Some(start.elapsed().as_secs());
            break;
        }
        // The program armor started may exit early: some launchers start
        // the application in the background (Sublime Text), so the search
        // goes on until the time limit.
        let _ = child.try_wait();
        std::thread::sleep(Duration::from_secs(1));
    }
    stop_group(&mut child);
    wineserver_kill(place, name);
    stop_place(place);
    Ok(match found {
        Some(seconds) => {
            format!("level 1: its window ({title}) appeared {seconds} s after armor run")
        }
        None => format!("failed: no window titled {title} within {wait} s"),
    })
}

/// Ends every process of this check: those whose environment names its
/// data directory (programs that left the process group).
fn stop_place(place: &Place) {
    let marker = format!("XDG_DATA_HOME={}", place.data.display());
    let Ok(entries) = fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|p| p.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() as i32 {
            continue;
        }
        let environ = fs::read(entry.path().join("environ")).unwrap_or_default();
        if environ.split(|&b| b == 0).any(|v| v == marker.as_bytes()) {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }
}

/// Stops the Wine server of an installed Windows application, if any.
fn wineserver_kill(place: &Place, name: &str) {
    let prefix = place.data.join("armor/apps").join(name).join("prefix");
    let server = place.data.join("armor/engines/wine/usr/bin/wineserver");
    if prefix.is_dir() && server.is_file() {
        let _ = Command::new(server)
            .arg("-k")
            .env("WINEPREFIX", prefix)
            .status();
    }
}

/// `7z a -t7z -mx=9 -mmt=1 <archive> <tests/guest>` run by `program` (a
/// command prefix), then the archive's SHA-256.
fn seven_zip(
    mut cmd: Command,
    archive: &Path,
    input: &str,
    log: &Path,
) -> Result<Option<String>, String> {
    let _ = fs::remove_file(archive);
    cmd.args(["a", "-t7z", "-mx=9", "-mmt=1"]);
    cmd.arg(archive_arg(archive, input)).arg(input);
    if !run_logged(cmd, log, Duration::from_secs(900))? || !archive.is_file() {
        return Ok(None);
    }
    sha256(archive).map(Some)
}

/// The archive argument: a Windows path (`Z:\...`) for Windows 7-Zip.
fn archive_arg(archive: &Path, input: &str) -> String {
    if input.starts_with("Z:") {
        format!("Z:{}", archive.display()).replace('/', "\\")
    } else {
        archive.display().to_string()
    }
}

fn check_seven_zip_windows(
    paths: &Paths,
    place: &Place,
    app: &App,
    exe: &str,
    arm64: &str,
) -> Result<String, String> {
    let name =
        installed_name(paths, place).ok_or("installed, but armor list shows no application")?;
    let input = format!("Z:{}", paths.root.join("tests/guest").display()).replace('/', "\\");
    let work = place
        .data
        .parent()
        .expect("inside target/app-check")
        .to_path_buf();
    let mut x86 = place.armor(paths);
    x86.args(["run", &name, exe]);
    let ours = seven_zip(x86, &work.join("x86.7z"), &input, &place.log)?;
    wineserver_kill(place, &name);
    // The ARM64 build, installed next to it as its own application.
    let arm_file = paths
        .root
        .join("target/apps/downloads")
        .join(app.dir)
        .join(arm64);
    let mut cmd = place.armor(paths);
    cmd.args(["install", "--name", "7zip-arm64"])
        .arg(&arm_file)
        .args(["--", "/S"]);
    if !run_logged(cmd, &place.log, Duration::from_secs(900))? {
        return Ok("failed: the ARM64 build did not install".to_string());
    }
    let mut native = place.armor(paths);
    native.args(["run", "7zip-arm64", "C:\\Program Files\\7-Zip\\7z.exe"]);
    let theirs = seven_zip(native, &work.join("arm64.7z"), &input, &place.log)?;
    wineserver_kill(place, "7zip-arm64");
    Ok(match (ours, theirs) {
        (Some(a), Some(b)) if a == b => format!(
            "level 3: archive of tests/guest identical to the ARM64 build's (SHA-256 {}…)",
            &a[..16]
        ),
        (Some(a), Some(b)) => format!(
            "failed: archives differ ({}… against {}…)",
            &a[..16],
            &b[..16]
        ),
        (None, _) => "failed: no archive".to_string(),
        (_, None) => "failed: the ARM64 build made no archive".to_string(),
    })
}

fn check_seven_zip_linux(
    paths: &Paths,
    place: &Place,
    app: &App,
    file: &Path,
    native: &str,
) -> Result<String, String> {
    let work = place
        .data
        .parent()
        .expect("inside target/app-check")
        .to_path_buf();
    let x86_dir = work.join("x64");
    let arm_dir = work.join("arm64");
    for (dir, archive) in [
        (&x86_dir, file.to_path_buf()),
        (
            &arm_dir,
            paths
                .root
                .join("target/apps/downloads")
                .join(app.dir)
                .join(native),
        ),
    ] {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        exec(
            Command::new("tar")
                .arg("-xf")
                .arg(&archive)
                .arg("-C")
                .arg(dir),
        )?;
    }
    let input = paths.root.join("tests/guest").display().to_string();
    let mut ours = Command::new(paths.root.join("target/release/armor"));
    // 7zz needs libstdc++, which the games root has (fetch-rootfs --games).
    ours.arg(x86_dir.join("7zz"))
        .env("ARMOR_ROOT", paths.root.join("target/guest/rootfs-games"));
    let a = seven_zip(ours, &work.join("x64.7z"), &input, &place.log)?;
    let b = seven_zip(
        Command::new(arm_dir.join("7zz")),
        &work.join("arm64.7z"),
        &input,
        &place.log,
    )?;
    Ok(match (a, b) {
        (Some(a), Some(b)) if a == b => format!(
            "level 3: archive of tests/guest identical to the native ARM64 7zz's (SHA-256 {}…)",
            &a[..16]
        ),
        (Some(_), Some(_)) => "failed: archives differ".to_string(),
        _ => "failed: no archive".to_string(),
    })
}

/// Whether the application runs under Wine.
fn is_windows(app: &App) -> bool {
    !matches!(app.check, Check::SevenZipLinux { .. }) && !app.engine.starts_with("armor")
}

/// The engine of the record: with `--x86-engine armor`, a Windows
/// application's 32-bit code runs under armor's WoW64 backend.
fn engine_label(app: &App, x86_engine: &str) -> String {
    if x86_engine != "armor" || !is_windows(app) {
        app.engine.to_string()
    } else if app.engine.contains("FEX WoW64") {
        app.engine.replace("FEX WoW64", "armor WoW64")
    } else {
        format!("{}, 32-bit code under armor WoW64", app.engine)
    }
}

/// Checks one application; its result line.
fn check(paths: &Paths, app: &App, x86_engine: &'static str) -> Result<String, String> {
    let file = installer(paths, app)?;
    let place = Place::new(paths, app, x86_engine)?;
    let result = (|| {
        if let Check::SevenZipLinux { native } = app.check {
            return check_seven_zip_linux(paths, &place, app, &file, native);
        }
        if !install(paths, &place, &file, app.install)? {
            return Ok(format!(
                "failed: armor install (see {})",
                place.log.display()
            ));
        }
        let Some(name) = installed_name(paths, &place) else {
            return Ok("failed: installed, but armor list shows no application".to_string());
        };
        match app.check {
            Check::Window { title, wait } => check_window(paths, &place, &name, title, wait),
            Check::SevenZipWindows { exe, arm64 } => {
                check_seven_zip_windows(paths, &place, app, exe, arm64)
            }
            Check::SevenZipLinux { .. } => unreachable!("handled above"),
        }
    })();
    // armor falls back to FEX when its backend is missing: the processes
    // must have run under it (its line at start, in the log).
    let result = result.map(|r| {
        if x86_engine != "armor" || !is_windows(app) || r.starts_with("failed") {
            return r;
        }
        let log = fs::read_to_string(&place.log).unwrap_or_default();
        match log.matches("armor-wow64 ").count() {
            0 => "failed: armor's WoW64 backend did not start".to_string(),
            n => {
                println!("{n} process(es) under armor's WoW64 backend");
                r
            }
        }
    });
    if result.as_ref().is_ok_and(|r| !r.starts_with("failed")) {
        place.remove();
    }
    result
}

/// `cargo xtask app-check [--report] [--x86-engine armor|fex] [id...]`.
pub fn run(args: &[String]) -> Result<(), String> {
    let mut args = args.to_vec();
    let report = crate::results::take_flag(&mut args);
    let x86_engine = match args.iter().position(|a| a == "--x86-engine") {
        Some(i) => {
            let engine = match args.get(i + 1).map(String::as_str) {
                Some("armor") => "armor",
                Some("fex") => "fex",
                _ => return Err("--x86-engine takes armor or fex".to_string()),
            };
            args.drain(i..i + 2);
            engine
        }
        None => "fex",
    };
    let paths = Paths::new();
    let selected: Vec<&App> = APPS
        .iter()
        .filter(|a| args.is_empty() || args.iter().any(|x| x == a.id))
        .collect();
    if selected.is_empty() {
        let ids: Vec<&str> = APPS.iter().map(|a| a.id).collect();
        return Err(format!(
            "no such application; the ids are: {}",
            ids.join(", ")
        ));
    }
    let status = Command::new("cargo")
        .args(["build", "--release", "-p", "armor"])
        .current_dir(&paths.root)
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err("building armor failed".to_string());
    }
    // Next to target/release/armor, where `armor run` finds it.
    if x86_engine == "armor" {
        crate::wow64::build(&paths.root)?;
    }
    let _display = Display::start()?;
    let mut records = Vec::new();
    let mut failed = 0;
    for app in selected {
        println!("== {} ({})", app.name, app.id);
        let result = check(&paths, app, x86_engine).unwrap_or_else(|e| format!("failed: {e}"));
        println!("{result}");
        if result.starts_with("failed") {
            failed += 1;
        }
        let reference = match app.check {
            Check::Window { .. } => "a window on an X display (Xvfb)",
            Check::SevenZipWindows { .. } => "7-Zip 26.04 for Windows ARM64, run the same way",
            Check::SevenZipLinux { .. } => "7-Zip 26.04 for Linux ARM64, run natively",
        };
        records.push(crate::results::Record::now(
            "application",
            app.name,
            &engine_label(app, x86_engine),
            reference,
            &result,
            &if x86_engine == "armor" {
                format!("cargo xtask app-check --x86-engine armor {}", app.id)
            } else {
                format!("cargo xtask app-check {}", app.id)
            },
        ));
    }
    if report {
        let file = crate::results::record(&paths.root, &crate::results::Machine::here(), &records)?;
        println!("recorded in {}", file.display());
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(format!("app-check: {failed} application(s) failed"))
    }
}
