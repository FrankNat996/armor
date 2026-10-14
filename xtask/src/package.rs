//! `cargo xtask package`: a Debian package of armor for arm64
//! (`target/package/armor_<version>_arm64.deb`), so that armor installs
//! without a Rust toolchain (milestone M7).
//!
//! The package holds `/usr/bin/armor`, the window `/usr/bin/armor-gui`
//! with its menu entry "armor" (ADR-0035), the desktop entry "Install with
//! armor" (a double click on a Windows `.exe` or `.msi`, an x86-64 `.deb` or
//! an AppImage installs it, in a terminal window that shows the progress) and
//! the licenses. Nothing runs
//! at installation except refreshing the desktop's MIME cache. The Windows
//! engine is not in the package: `armor engine fetch` downloads it.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::exec;

/// The desktop entry for double-clicking installers, `.deb` packages and
/// AppImages.
const INSTALL_ENTRY: &str = "[Desktop Entry]
Type=Application
Name=Install with armor
Comment=Install an x86 program (Windows .exe or .msi, Linux .deb or AppImage) on this ARM64 computer
Exec=armor install --pause %f
Terminal=true
NoDisplay=true
Icon=system-software-install
MimeType=application/vnd.microsoft.portable-executable;application/x-msdownload;application/x-ms-dos-executable;application/x-msi;application/vnd.debian.binary-package;application/vnd.appimage;application/x-iso9660-appimage;
Categories=System;
";

/// The menu entry of the window (`armor-gui`, ADR-0035).
const WINDOW_ENTRY: &str = "[Desktop Entry]
Type=Application
Name=armor
GenericName=x86 applications
Comment=Install, open and remove x86 Windows and Linux programs on this ARM64 computer
Exec=armor-gui
Terminal=false
Icon=system-software-install
Categories=System;
StartupWMClass=io.github.FrankNat996.armor
";

/// Refreshes the desktop's MIME cache after installation and removal.
const MAINTAINER_SCRIPT: &str = "#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
";

/// The package version: the workspace version alone when the commit being
/// built is its release tag (`v<version>`) and the tree has no changes,
/// else the workspace version with the date and commit of the build
/// (`<version>~git<date>.<commit>`, which sorts before the release; the
/// workspace version is always the next release's).
fn version(root: &Path) -> Result<String, String> {
    let manifest =
        fs::read_to_string(root.join("Cargo.toml")).map_err(|e| format!("Cargo.toml: {e}"))?;
    let base = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|v| v.strip_suffix('"'))
        .ok_or("Cargo.toml: no workspace version")?
        .to_string();
    let log = exec(
        Command::new("git")
            .args(["log", "-1", "--format=%cd.%h", "--date=format:%Y%m%d%H%M%S"])
            .current_dir(root),
    )?;
    let stamp = String::from_utf8_lossy(&log.stdout).trim().to_string();
    let tags = exec(
        Command::new("git")
            .args(["tag", "--points-at", "HEAD"])
            .current_dir(root),
    )?;
    let release = format!("v{base}");
    let changes = exec(
        Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=no"])
            .current_dir(root),
    )?;
    if changes.stdout.is_empty()
        && String::from_utf8_lossy(&tags.stdout)
            .lines()
            .any(|tag| tag == release)
    {
        return Ok(base);
    }
    Ok(format!("{base}~git{stamp}"))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))
}

fn chmod(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn run(root: &Path) -> Result<(), String> {
    let status = Command::new("cargo")
        .args(["build", "--release", "--bin", "armor"])
        .current_dir(root)
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build failed: {status}"));
    }
    // The window has its own workspace (ADR-0035), built into target/gui.
    let status = Command::new("cargo")
        .args(["build", "--release", "--target-dir", "../target/gui"])
        .current_dir(root.join("gui"))
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "building the window failed: {status} (it needs libgtk-4-dev)"
        ));
    }
    let version = version(root)?;
    let out = root.join("target/package");
    let stage = out.join(format!("armor_{version}_arm64"));
    if stage.exists() {
        fs::remove_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    }
    let binary = stage.join("usr/bin/armor");
    copy(&root.join("target/release/armor"), &binary)?;
    chmod(&binary, 0o755)?;
    let window = stage.join("usr/bin/armor-gui");
    copy(&root.join("target/gui/release/armor-gui"), &window)?;
    write(
        &stage.join("usr/share/applications/armor-install.desktop"),
        INSTALL_ENTRY,
    )?;
    write(
        &stage.join("usr/share/applications/armor.desktop"),
        WINDOW_ENTRY,
    )?;
    let doc = stage.join("usr/share/doc/armor");
    copy(&root.join("LICENSE-MIT"), &doc.join("LICENSE-MIT"))?;
    copy(&root.join("LICENSE-APACHE"), &doc.join("LICENSE-APACHE"))?;
    write(
        &doc.join("copyright"),
        "armor: https://github.com/FrankNat996/armor\n\
         License: MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE in this directory).\n\
         The Windows engine (Wine, FEX, box64, DXVK, Wine Mono) is not part of this package:\n\
         `armor engine fetch` downloads it from its sources, with its own licences.\n",
    )?;
    normalize_modes(&stage)?;
    chmod(&binary, 0o755)?;
    chmod(&window, 0o755)?;
    let size_kib = dir_size(&stage)? / 1024 + 1;
    write(
        &stage.join("DEBIAN/control"),
        &format!(
            "Package: armor\n\
             Version: {version}\n\
             Architecture: arm64\n\
             Maintainer: armor developers <https://github.com/FrankNat996/armor>\n\
             Installed-Size: {size_kib}\n\
             Depends: libc6, libgtk-4-1, curl, tar, xz-utils, coreutils, unzip, gpgv, \
             debian-archive-keyring | ubuntu-keyring\n\
             Recommends: x-terminal-emulator, desktop-file-utils, zstd, bzip2, libglib2.0-bin, \
             libgtk2.0-0t64, libibus-1.0-5, libpipewire-0.3-0t64\n\
             Section: otherosfs\n\
             Priority: optional\n\
             Homepage: https://github.com/FrankNat996/armor\n\
             Description: run and install x86 Linux and Windows programs on ARM64\n \
             armor translates x86-64 and i386 Linux programs to AArch64, and installs\n \
             x86 Windows programs (.exe, .msi) with Wine for ARM64 and FEX, and x86-64\n \
             Linux applications (.deb, AppImage, .tar archives) with the Debian\n \
             libraries they need, each in a place of its own with entries in the\n \
             desktop menu.\n"
        ),
    )?;
    for script in ["postinst", "postrm"] {
        let path = stage.join("DEBIAN").join(script);
        write(&path, MAINTAINER_SCRIPT)?;
        chmod(&path, 0o755)?;
    }
    let deb = out.join(format!("armor_{version}_arm64.deb"));
    let status = Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&stage)
        .arg(&deb)
        .status()
        .map_err(|e| format!("dpkg-deb: {e} (package dpkg-dev or dpkg)"))?;
    if !status.success() {
        return Err(format!("dpkg-deb failed: {status}"));
    }
    println!("{}", deb.display());
    Ok(())
}

/// Directories 0755 and files 0644 under `dir`, whatever the umask was.
fn normalize_modes(dir: &Path) -> Result<(), String> {
    chmod(dir, 0o755)?;
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            normalize_modes(&path)?;
        } else {
            chmod(&path, 0o644)?;
        }
    }
    Ok(())
}

/// The total size of the files under `dir`.
fn dir_size(dir: &Path) -> Result<u64, String> {
    let mut total = 0;
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        total += if path.is_dir() {
            dir_size(&path)?
        } else {
            fs::metadata(&path).map_err(|e| e.to_string())?.len()
        };
    }
    Ok(total)
}
