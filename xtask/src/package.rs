//! `cargo xtask package`: a Debian package of armor for arm64
//! (`target/package/armor_<version>_arm64.deb`), so that armor installs
//! without a Rust toolchain (milestone M7).
//!
//! The package holds `/usr/bin/armor`, the window `/usr/bin/armor-gui`
//! with its menu entry "armor" (ADR-0035), the desktop entry "Install with
//! armor" (a double click on a Windows `.exe` or `.msi`, an x86-64 `.deb` or
//! an AppImage installs it, in a terminal window that shows the progress) and
//! the licenses, and armor's x86-64 `libGL.so.1` in
//! `/usr/lib/armor/x86_64-linux-gnu/` (ADR-0041, for `--host-gl`). Nothing runs
//! at installation except refreshing the desktop's MIME cache. The Windows
//! engine is not in the package: `armor engine fetch` downloads it.
//!
//! The same files also go into `armor_<version>_arm64.tar.gz` (ADR-0043),
//! for distributions other than Debian and Ubuntu: `bin/`, `lib/armor/`,
//! `share/`, with `install.sh` (into `~/.local`, or `$PREFIX`, without root,
//! with the menu entries) and `uninstall.sh`. Both carry Debian's archive
//! keyring in `share/armor/keyrings/`, for hosts that have none.

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

/// Debian's archive keyring on the build host (package
/// `debian-archive-keyring`), carried by armor for other distributions.
const DEBIAN_KEYRING: &str = "/usr/share/keyrings/debian-archive-keyring.gpg";

/// The archive's installer: armor for this user, without root (ADR-0043).
/// `@VERSION@` is replaced with the version.
const INSTALL_SCRIPT: &str = r#"#!/bin/sh
# Installs armor @VERSION@ for this user, without root: bin/, lib/armor/ and
# share/ into $PREFIX (default ~/.local), with the menu entries "armor" and
# "Install with armor". Run it again to reinstall; uninstall.sh removes it.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
prefix=${PREFIX:-$HOME/.local}
version=@VERSION@
cd "$here"
for f in $(find bin lib share -type f ! -path 'share/applications/*'); do
    mkdir -p "$prefix/$(dirname "$f")"
    # A new copy renamed over the old one: a running armor keeps its file.
    cp "$f" "$prefix/$f.new"
    mv -f "$prefix/$f.new" "$prefix/$f"
done
mkdir -p "$prefix/share/applications"
for entry in armor.desktop armor-install.desktop; do
    sed -e "s|^Exec=armor-gui|Exec="$prefix/bin/armor-gui"|"         -e "s|^Exec=armor |Exec="$prefix/bin/armor" |"         "share/applications/$entry" > "$prefix/share/applications/$entry"
done
printf 'kind=archive
version=%s
' "$version" > "$prefix/share/armor/installed"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$prefix/share/applications" || true
fi
echo "armor $version installed in $prefix: $prefix/bin/armor, and "armor" in the menu"
case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) echo "note: $prefix/bin is not in PATH; add it to use the armor command by name" ;;
esac
"#;

/// Removes what `install.sh` installed; the installed applications and the
/// Windows engine (`~/.local/share/armor/apps`, `engines`) stay.
const UNINSTALL_SCRIPT: &str = r#"#!/bin/sh
# Removes armor installed by install.sh from $PREFIX (default ~/.local).
# The applications installed with it and the Windows engine stay in
# ~/.local/share/armor (apps/, engines/): remove that folder to free them.
set -eu
prefix=${PREFIX:-$HOME/.local}
rm -f "$prefix/bin/armor" "$prefix/bin/armor-gui"     "$prefix/share/applications/armor.desktop" "$prefix/share/applications/armor-install.desktop"     "$prefix/share/armor/installed"
rm -rf "$prefix/lib/armor" "$prefix/share/armor/keyrings" "$prefix/share/doc/armor"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$prefix/share/applications" || true
fi
echo "armor removed from $prefix"
"#;

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
    let (deb, archive) = build(root)?;
    println!("{}", deb.display());
    println!("{}", archive.display());
    Ok(())
}

/// Builds the package and the archive; returns their paths.
pub fn build(root: &Path) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
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
    // The x86-64 libGL that `--host-gl` gives to programs (built with the
    // x86-64 cross compiler).
    let libgl = crate::gl::build_libgl(&crate::Paths::new())?;
    copy(
        &libgl,
        &stage.join("usr/lib/armor/x86_64-linux-gnu/libGL.so.1"),
    )?;
    if !Path::new(DEBIAN_KEYRING).is_file() {
        return Err(format!(
            "{DEBIAN_KEYRING} is missing: the package carries it (install debian-archive-keyring)"
        ));
    }
    copy(
        Path::new(DEBIAN_KEYRING),
        &stage.join("usr/share/armor/keyrings/debian-archive-keyring.gpg"),
    )?;
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
         /usr/lib/armor/x86_64-linux-gnu/libGL.so.1 is generated from the Khronos OpenGL\n\
         registry (gl.xml, glx.xml; Copyright The Khronos Group Inc., Apache-2.0).\n\
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
             Depends: libc6 (>= 2.39), libgtk-4-1, curl, tar, xz-utils, coreutils, unzip, gpgv, \
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
    let tarball = archive(&stage.join("usr"), &out, &version)?;
    Ok((deb, tarball))
}

/// `armor_<version>_arm64.tar.gz`: the package's `usr/` as `bin/`, `lib/`
/// and `share/`, with `install.sh` and `uninstall.sh` (ADR-0043).
fn archive(usr: &Path, out: &Path, version: &str) -> Result<std::path::PathBuf, String> {
    let name = format!("armor_{version}_arm64");
    let parent = out.join("archive");
    let stage = parent.join(&name);
    if stage.exists() {
        fs::remove_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    }
    fs::create_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    let status = Command::new("cp")
        .args(["-a", "--"])
        .arg(usr.join("bin"))
        .arg(usr.join("lib"))
        .arg(usr.join("share"))
        .arg(&stage)
        .status()
        .map_err(|e| format!("cp: {e}"))?;
    if !status.success() {
        return Err(format!("copying the package's files failed: {status}"));
    }
    for (file, text) in [
        ("install.sh", INSTALL_SCRIPT.replace("@VERSION@", version)),
        ("uninstall.sh", UNINSTALL_SCRIPT.to_string()),
    ] {
        let path = stage.join(file);
        write(&path, &text)?;
        chmod(&path, 0o755)?;
    }
    let tarball = out.join(format!("{name}.tar.gz"));
    let status = Command::new("tar")
        .args([
            "--owner=0",
            "--group=0",
            "--numeric-owner",
            "--sort=name",
            "-czf",
        ])
        .arg(&tarball)
        .arg("-C")
        .arg(&parent)
        .arg(&name)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err(format!("tar failed: {status}"));
    }
    Ok(tarball)
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
