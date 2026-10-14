//! `armor install steam`: Valve's native ARM64 Steam client as an installed
//! application (M8, ADR-0038), in `$XDG_DATA_HOME/armor/apps/Steam/`:
//! `Steam/` (the client, which updates itself), `home/` (its `HOME`, so that
//! its files stay in the application's directory) and the menu entry
//! "Steam". Windows games then go through the Proton that the client
//! installs (with Valve's FEX layer); x86 Linux games run under armor.
//!
//! The client comes from Valve's update server: the manifest
//! `steam_client_linuxarm64` (over HTTPS) lists its packages with their
//! size and SHA-256; armor downloads the three that start it (the
//! bootstrapper, the x86 and the ARM64 binaries), checks each against the
//! manifest and unpacks them. The client fetches the rest itself on its
//! first start, as it does on any machine.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Valve's manifest of the ARM64 Linux client.
const MANIFEST_URL: &str = "https://client-update.steamstatic.com/steam_client_linuxarm64";

/// Where the manifest's packages are.
const PACKAGES_URL: &str = "https://client-update.steamstatic.com/";

/// The packages that start the client (measured on 2026-10-14: with these
/// three it updated itself and opened).
const PACKAGES: &[&str] = &[
    "steam_linuxarm64",
    "bins_linuxarm64",
    "bins_linuxarm64_linuxarm64",
];

/// Host libraries the ARM64 client loads (`ldd` of `steamui.so` and
/// `steamclient.so`, 2026-10-14) and the Debian packages that carry them.
const HOST_LIBRARIES: &[(&str, &str)] = &[
    ("libgtk-x11-2.0.so.0", "libgtk2.0-0t64"),
    ("libibus-1.0.so.5", "libibus-1.0-5"),
    ("libpipewire-0.3.so.0", "libpipewire-0.3-0t64"),
];

/// One package of the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

/// The packages of a manifest (Valve's text VDF: `"name" { "key" "value"
/// … }` blocks inside the platform's block). Only the keys `file`, `size`
/// and `sha2` are read.
pub fn parse_manifest(text: &str) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut current: Option<(String, Vec<(String, String)>)> = None;
    let mut pending_name: Option<String> = None;
    let mut depth = 0;
    for line in text.lines() {
        let line = line.trim();
        if line == "{" {
            depth += 1;
            if depth == 2 {
                current = pending_name.take().map(|n| (n, Vec::new()));
            }
            continue;
        }
        if line == "}" {
            if depth == 2
                && let Some((name, keys)) = current.take()
            {
                let get = |k: &str| keys.iter().find(|(key, _)| key == k).map(|(_, v)| v);
                if let (Some(file), Some(size), Some(sha256)) =
                    (get("file"), get("size"), get("sha2"))
                    && let Ok(size) = size.parse()
                {
                    packages.push(Package {
                        name,
                        file: file.clone(),
                        size,
                        sha256: sha256.to_ascii_lowercase(),
                    });
                }
            }
            depth -= 1;
            continue;
        }
        let fields: Vec<&str> = line.split('"').collect();
        // `"key"		"value"` → ["", key, tabs, value, ""]; `"name"` → ["", name, ""].
        match fields.as_slice() {
            ["", key, _, value, ""] => {
                if let Some((_, keys)) = &mut current {
                    keys.push((key.to_string(), value.to_string()));
                }
            }
            ["", name, ""] => pending_name = Some(name.to_string()),
            _ => {}
        }
    }
    packages
}

/// The host libraries of `HOST_LIBRARIES` that are missing, with their
/// Debian packages.
fn missing_host_libraries() -> Vec<(&'static str, &'static str)> {
    let dirs = [
        "/lib/aarch64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/lib64",
        "/usr/lib64",
        "/usr/lib",
    ];
    HOST_LIBRARIES
        .iter()
        .filter(|(library, _)| !dirs.iter().any(|d| Path::new(d).join(library).exists()))
        .copied()
        .collect()
}

/// Checks the host before downloading: 4K pages, the host libraries.
pub fn check_host() -> Result<(), String> {
    // The client needs 4K pages (its segments are 4K-aligned: `tech/steam.md`).
    let page = armor_mem::host_page_size();
    if page != 4096 {
        return Err(format!(
            "Valve's ARM64 Steam client needs a kernel with 4K pages, and this one uses {}K \
             pages. On a Raspberry Pi 5: add `kernel=kernel8.img` to \
             /boot/firmware/config.txt and reboot",
            page / 1024
        ));
    }
    let missing = missing_host_libraries();
    if !missing.is_empty() {
        let packages: Vec<&str> = missing.iter().map(|(_, p)| *p).collect();
        let libraries: Vec<&str> = missing.iter().map(|(l, _)| *l).collect();
        return Err(format!(
            "the Steam client needs {} from this system: install them with `sudo apt install \
             {}` and run `armor install steam` again",
            libraries.join(", "),
            packages.join(" ")
        ));
    }
    Ok(())
}

/// Downloads and unpacks the client into `steam_dir`; the packages are kept
/// in `cache`.
pub fn fetch_client(steam_dir: &Path, cache: &Path) -> Result<(), String> {
    fs::create_dir_all(cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    eprintln!("armor: reading Valve's manifest of the ARM64 Steam client");
    let output = Command::new("curl")
        .args(["-fsSL", MANIFEST_URL])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{MANIFEST_URL}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let manifest = parse_manifest(&String::from_utf8_lossy(&output.stdout));
    fs::create_dir_all(steam_dir).map_err(|e| format!("{}: {e}", steam_dir.display()))?;
    for wanted in PACKAGES {
        let package = manifest
            .iter()
            .find(|p| p.name == *wanted)
            .ok_or_else(|| format!("{MANIFEST_URL}: no package {wanted}"))?;
        let file = cache.join(&package.file);
        if !(file.is_file() && crate::engines::sha256(&file)? == package.sha256) {
            let partial = cache.join(format!("{}.partial", package.file));
            crate::engines::download_verified(
                &format!("{PACKAGES_URL}{}", package.file),
                &partial,
                package.size,
                &package.sha256,
            )?;
            fs::rename(&partial, &file).map_err(|e| format!("{}: {e}", file.display()))?;
        }
        let status = Command::new("unzip")
            .args(["-o", "-q"])
            .arg(&file)
            .arg("-d")
            .arg(steam_dir)
            .status()
            .map_err(|e| format!("unzip: {e} (package unzip)"))?;
        if !status.success() {
            return Err(format!("{}: unzip could not unpack it", file.display()));
        }
    }
    make_executable(steam_dir)
}

/// Valve's zips carry no permissions: the ELF files and the scripts under
/// `dir` become executable.
fn make_executable(dir: &Path) -> Result<(), String> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let read = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for item in read.flatten() {
            let path = item.path();
            let Ok(kind) = item.file_type() else { continue };
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let mut magic = [0u8; 4];
            let read = fs::File::open(&path).and_then(|mut f| f.read(&mut magic));
            if matches!(read, Ok(n) if n >= 2) && (magic == *b"\x7fELF" || magic.starts_with(b"#!"))
            {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// The links the client looks for in its `HOME` (Valve's `steam.sh` makes
/// most of them; the client refuses to start without `~/.steam/steam`):
/// `steam` and `root` to the client's directory, `bin64` to its ARM64
/// programs, `bin32` to the i386 ones, and `sdk32`, `sdk64` and `sdkarm64`
/// to the Steam API libraries for games (`sdkarm64` holds the
/// `steam-launch-wrapper` every game is started with; the x86 `steam.sh`
/// does not know it: without it no game starts, measured 2026-10-15).
pub fn link_home(home: &Path, steam_dir: &Path) -> Result<(), String> {
    let dot = home.join(".steam");
    fs::create_dir_all(&dot).map_err(|e| format!("{}: {e}", dot.display()))?;
    for (link, target) in [
        ("steam", steam_dir.to_path_buf()),
        ("root", steam_dir.to_path_buf()),
        ("bin64", steam_dir.join("steamrtarm64")),
        ("bin32", steam_dir.join("ubuntu12_32")),
        ("sdk32", steam_dir.join("linux32")),
        ("sdk64", steam_dir.join("linux64")),
        ("sdkarm64", steam_dir.join("linuxarm64")),
    ] {
        let path = dot.join(link);
        let _ = fs::remove_file(&path);
        std::os::unix::fs::symlink(&target, &path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// The exit status with which the client asks to be started again (after
/// an update; `steam.sh`).
pub const RESTART_STATUS: i32 = 42;

/// How many times in a row `armor run` starts it again.
pub const MAX_RESTARTS: usize = 5;

/// The client's icon inside `steam_dir` (an `.ico` with sizes up to 256
/// pixels), there once the client has updated itself.
pub fn icon(steam_dir: &Path) -> PathBuf {
    steam_dir.join("public/steam_tray.ico")
}

/// The client's program inside `steam_dir`.
pub fn client(steam_dir: &Path) -> PathBuf {
    steam_dir.join("steamrtarm64/steam")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_packages_are_read() {
        // The shape of Valve's manifest (2026-10-14), shortened.
        let text = "\"linuxarm64\"\n{\n\t\"version\"\t\t\"1791918634\"\n\t\"steam_linuxarm64\"\n\t{\n\
                    \t\t\"file\"\t\t\"steam_linuxarm64.zip.a9742e15\"\n\t\t\"size\"\t\t\"4551279\"\n\
                    \t\t\"sha2\"\t\t\"7A3FC1C5\"\n\t\t\"IsBootstrapperPackage\"\t\t\"1\"\n\t}\n\
                    \t\"strings_en_all\"\n\t{\n\t\t\"file\"\t\t\"strings_en_all.zip.e716\"\n\
                    \t\t\"size\"\t\t\"114150\"\n\t\t\"sha2\"\t\t\"0c56\"\n\t}\n\
                    \t\"broken\"\n\t{\n\t\t\"file\"\t\t\"x\"\n\t}\n}\n";
        assert_eq!(
            parse_manifest(text),
            [
                Package {
                    name: "steam_linuxarm64".to_string(),
                    file: "steam_linuxarm64.zip.a9742e15".to_string(),
                    size: 4551279,
                    sha256: "7a3fc1c5".to_string(),
                },
                Package {
                    name: "strings_en_all".to_string(),
                    file: "strings_en_all.zip.e716".to_string(),
                    size: 114150,
                    sha256: "0c56".to_string(),
                },
            ]
        );
    }
}
