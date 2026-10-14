//! `armor engine fetch`: downloads the Windows engine (ADR-0033): Wine for
//! AArch64 built as ARM64X with FEX, from a pinned Hangover release, and
//! the DXVK it ships, into `$XDG_DATA_HOME/armor/engines/` — where
//! `armor install` looks for them (`apps.rs`).
//!
//! Every download is pinned by URL, size and SHA-256 (the digests GitHub
//! publishes for the release, copied here on 2026-10-14) and verified
//! before anything is extracted. Hangover's Debian packages are unpacked,
//! never installed: no root, no maintainer scripts, nothing outside the
//! engines directory. The build is chosen from `/etc/os-release`, since
//! Hangover's Wine links the distribution's own libraries.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The Hangover release armor fetches.
const HANGOVER_VERSION: &str = "11.16";

/// One build of the release: the distribution it is for, its file, size
/// and SHA-256.
#[derive(Debug)]
pub struct Build {
    pub distribution: &'static str,
    pub codename: &'static str,
    pub file: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// Hangover 11.16's builds (https://github.com/AndreRH/hangover/releases/
/// tag/hangover-11.16, 2026-08-30).
pub const BUILDS: &[Build] = &[
    Build {
        distribution: "debian",
        codename: "bookworm",
        file: "hangover_11.16_debian12_bookworm_arm64.tar",
        size: 293_027_840,
        sha256: "dd7373202a168b7200cc36627a25b588511f3783756c2661bf4c022be8380aa8",
    },
    Build {
        distribution: "debian",
        codename: "trixie",
        file: "hangover_11.16_debian13_trixie_arm64.tar",
        size: 293_099_520,
        sha256: "b5493f5903ab3c05f78bf4c03b06ac0005eb26f732437748c1592fefa45e861a",
    },
    Build {
        distribution: "ubuntu",
        codename: "jammy",
        file: "hangover_11.16_ubuntu2204_jammy_arm64.tar",
        size: 312_883_200,
        sha256: "ecde5846b2f1b23d394fede04a52cbf31853076b85e98d53a391d9e758345488",
    },
    Build {
        distribution: "ubuntu",
        codename: "noble",
        file: "hangover_11.16_ubuntu2404_noble_arm64.tar",
        size: 307_138_560,
        sha256: "94611dad2b23978d35825a018c81b3947caaec174aa04c052849d37bc6a39e72",
    },
    Build {
        distribution: "ubuntu",
        codename: "questing",
        file: "hangover_11.16_ubuntu2510_questing_arm64.tar",
        size: 305_745_920,
        sha256: "c2cd1530649d8854afc8b86a8fbb581523dc548294b4e77682fe0beeedda6f52",
    },
    Build {
        distribution: "ubuntu",
        codename: "resolute",
        file: "hangover_11.16_ubuntu2604_resolute_arm64.tar",
        size: 305_653_760,
        sha256: "bdf7d91d67367a3dc6e856be3c6ab16e8d3b9049e7cd197844df20a7e4b45c52",
    },
];

/// The packages of a build that make the engine: Wine, FEX for x86-64
/// (ARM64EC) and for 32-bit x86 (WoW64), and box64 as the other WoW64
/// translator (Hangover's default for 32-bit code).
const PACKAGES: &[&str] = &[
    "hangover-wine_",
    "hangover-libarm64ecfex_",
    "hangover-libwow64fex_",
    "hangover-wowbox64_",
];

/// Wine Mono (.NET for Wine), for ARM64 and the version Hangover 11.16's Wine
/// asks for (`appwiz.cpl` names `wine-mono-11.3.0-arm64.msi` and carries
/// its SHA-256, the same as below; checked 2026-10-14).
pub const WINE_MONO_FILE: &str = "wine-mono-11.3.0-arm64.msi";
const WINE_MONO_URL: &str =
    "https://dl.winehq.org/wine/wine-mono/11.3.0/wine-mono-11.3.0-arm64.msi";
const WINE_MONO_SIZE: u64 = 89_109_504;
const WINE_MONO_SHA256: &str = "b0c47d374efaf22cbff76cb10488ac2d1627b067e085aef7acec573884120e0f";

/// Downloads `url` to `file` and checks its size and SHA-256; removes the
/// file when they differ.
pub(crate) fn download_verified(
    url: &str,
    file: &Path,
    size: u64,
    sha256_hex: &str,
) -> Result<(), String> {
    eprintln!("armor: downloading {url} ({} MB)", size / 1_000_000);
    run(Command::new("curl")
        .args(["-fL", "--progress-bar", "-o"])
        .arg(file)
        .arg(url))?;
    let actual_size = fs::metadata(file)
        .map_err(|e| format!("{}: {e}", file.display()))?
        .len();
    let digest = sha256(file)?;
    if actual_size != size || digest != sha256_hex {
        let _ = fs::remove_file(file);
        return Err(format!(
            "{url}: expected {size} bytes with SHA-256 {sha256_hex}, got {actual_size} bytes \
             with {digest}; removed"
        ));
    }
    eprintln!("armor: verified (SHA-256 {digest})");
    Ok(())
}

/// The version of the Microsoft Visual C++ 2015-2022 redistributables armor
/// installs when a program needs a library only they have (`apps.rs`).
pub const VCREDIST_VERSION: &str = "14.44.35211";

/// The redistributables, x64 and x86: file name, URL, size and SHA-256.
/// Versioned URLs on Microsoft's download server (the SHA-256 is part of
/// the path; `aka.ms/vs/17/release/vc_redist.x64.exe` pointed there on
/// 2026-10-14), so that the pin does not break when a newer version comes.
const VCREDISTS: &[(&str, &str, u64, &str)] = &[
    (
        "VC_redist.x64.exe",
        "https://download.visualstudio.microsoft.com/download/pr/\
         990c720a-46ce-4dc7-87b9-c2eb615d34b9/\
         CC0FF0EB1DC3F5188AE6300FAEF32BF5BEEBA4BDD6E8E445A9184072096B713B/VC_redist.x64.exe",
        25_635_768,
        "cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b",
    ),
    (
        "VC_redist.x86.exe",
        "https://download.visualstudio.microsoft.com/download/pr/\
         990c720a-46ce-4dc7-87b9-c2eb615d34b9/\
         0C09F2611660441084CE0DF425C51C11E147E6447963C3690F97E0B25C55ED64/VC_redist.x86.exe",
        13_953_392,
        "0c09f2611660441084ce0df425c51c11e147e6447963c3690f97e0b25c55ed64",
    ),
];

/// The Visual C++ redistributables in `engines`, downloaded and verified if
/// missing (on demand: most programs need only Wine's own runtime).
pub fn fetch_vcredist(engines: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = engines.join(format!("vcredist-{VCREDIST_VERSION}"));
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut files = Vec::new();
    for (name, url, size, sha256_hex) in VCREDISTS {
        let file = dir.join(name);
        if !(file.is_file() && sha256(&file)? == *sha256_hex) {
            let partial = dir.join(format!("{name}.partial"));
            download_verified(url, &partial, *size, sha256_hex)?;
            fs::rename(&partial, &file).map_err(|e| format!("{}: {e}", file.display()))?;
        }
        files.push(file);
    }
    Ok(files)
}

/// Wine Mono's installer in `engines`, downloaded if missing.
fn fetch_mono(engines: &Path) -> Result<(), String> {
    let file = engines.join(WINE_MONO_FILE);
    if file.is_file() && sha256(&file)? == WINE_MONO_SHA256 {
        return Ok(());
    }
    let partial = engines.join(format!("{WINE_MONO_FILE}.partial"));
    download_verified(WINE_MONO_URL, &partial, WINE_MONO_SIZE, WINE_MONO_SHA256)?;
    fs::rename(&partial, &file).map_err(|e| format!("{}: {e}", file.display()))
}

/// The DXVK archive inside the build.
const DXVK_ARCHIVE: &str = "dxvk-v2.7.1.tar.gz";
const DXVK_DIR: &str = "dxvk-v2.7.1";

/// The value of `key` in an `/etc/os-release` text, without quotes.
pub fn os_release_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix('=')?;
        Some(
            value
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string(),
        )
    })
}

/// The build for the host described by `os_release`: a distribution based
/// on Ubuntu by its `UBUNTU_CODENAME`, else Debian or Ubuntu by `ID` (or
/// `ID_LIKE`) and `VERSION_CODENAME`.
pub fn build_for(os_release: &str) -> Result<&'static Build, String> {
    let get = |key| os_release_value(os_release, key).unwrap_or_default();
    let (distribution, codename) = if !get("UBUNTU_CODENAME").is_empty() {
        ("ubuntu".to_string(), get("UBUNTU_CODENAME"))
    } else {
        let id = get("ID");
        let like = get("ID_LIKE");
        let family = if id == "debian" || id == "ubuntu" {
            id
        } else if like.split_whitespace().any(|l| l == "debian") {
            "debian".to_string()
        } else {
            id
        };
        (family, get("VERSION_CODENAME"))
    };
    BUILDS
        .iter()
        .find(|b| b.distribution == distribution && b.codename == codename)
        .ok_or_else(|| {
            let known: Vec<String> = BUILDS
                .iter()
                .map(|b| format!("{} {}", b.distribution, b.codename))
                .collect();
            format!(
                "Hangover {HANGOVER_VERSION} has no build for {distribution} {codename:?}; it \
                 has builds for: {}",
                known.join(", ")
            )
        })
}

/// Copies the member of the `ar` archive `deb` whose name starts with
/// `prefix` into `out`; returns the member's name. `ar` format: the magic
/// `!<arch>\n`, then for each member a 60-byte header (name in bytes 0–15,
/// decimal size in bytes 48–57) and the data, padded to an even length.
pub fn extract_ar_member(
    deb: &mut (impl Read + Seek),
    prefix: &str,
    out: &mut impl Write,
) -> Result<String, String> {
    let mut magic = [0u8; 8];
    deb.read_exact(&mut magic)
        .map_err(|e| format!("not an ar archive: {e}"))?;
    if &magic != b"!<arch>\n" {
        return Err("not an ar archive".to_string());
    }
    loop {
        let mut header = [0u8; 60];
        match deb.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(format!("no member starting with {prefix:?}"));
            }
            Err(e) => return Err(e.to_string()),
        }
        let name = String::from_utf8_lossy(&header[..16])
            .trim_end()
            .trim_end_matches('/')
            .to_string();
        let size: u64 = String::from_utf8_lossy(&header[48..58])
            .trim()
            .parse()
            .map_err(|_| format!("member {name:?}: bad size"))?;
        if name.starts_with(prefix) {
            let copied = io::copy(&mut deb.take(size), out).map_err(|e| e.to_string())?;
            if copied != size {
                return Err(format!("member {name:?}: truncated"));
            }
            return Ok(name);
        }
        deb.seek(SeekFrom::Current((size + size % 2) as i64))
            .map_err(|e| e.to_string())?;
    }
}

fn run(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|e| format!("{program}: {e} (is it installed?)"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed: {status}"))
    }
}

/// The SHA-256 of `file`, with the host's `sha256sum`.
pub(crate) fn sha256(file: &Path) -> Result<String, String> {
    let output = Command::new("sha256sum")
        .arg(file)
        .output()
        .map_err(|e| format!("sha256sum: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| format!("sha256sum {}: no result", file.display()))
}

/// Points the link `link` at `target` (a name in the same directory),
/// replacing an older link but never a real directory.
fn point_link(link: &Path, target: &str) -> Result<(), String> {
    if link.is_symlink() {
        fs::remove_file(link).map_err(|e| format!("{}: {e}", link.display()))?;
    } else if link.exists() {
        return Err(format!(
            "{} exists and is not a link: move it away first",
            link.display()
        ));
    }
    std::os::unix::fs::symlink(target, link).map_err(|e| format!("{}: {e}", link.display()))
}

/// The host libraries Wine's Unix side needs and the host lacks (`ldd`).
fn missing_libraries(wine_lib: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    let Ok(read) = fs::read_dir(wine_lib) else {
        return missing;
    };
    for item in read.flatten() {
        let path = item.path();
        if !path.extension().is_some_and(|e| e == "so") {
            continue;
        }
        let Ok(output) = Command::new("ldd").arg(&path).output() else {
            continue;
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some((library, _)) = line.trim().split_once(" => not found") {
                // Wine's own libraries, found next to each other at run time.
                if !matches!(library, "ntdll.so" | "win32u.so")
                    && !missing.iter().any(|m| m == library)
                {
                    missing.push(library.to_string());
                }
            }
        }
    }
    missing.sort();
    missing
}

/// `armor engine fetch`.
fn fetch(engines: &Path) -> Result<(), String> {
    let os_release =
        fs::read_to_string("/etc/os-release").map_err(|e| format!("/etc/os-release: {e}"))?;
    let build = build_for(&os_release)?;
    let target_name = format!("hangover-{HANGOVER_VERSION}");
    let target = engines.join(&target_name);
    if target.join("usr/bin/wine").is_file() && engines.join(DXVK_DIR).is_dir() {
        point_link(&engines.join("wine"), &target_name)?;
        point_link(&engines.join("dxvk"), DXVK_DIR)?;
        fetch_mono(engines)?;
        eprintln!(
            "armor: Hangover {HANGOVER_VERSION}, DXVK and Wine Mono are in {}",
            engines.display()
        );
        return Ok(());
    }
    let work = engines.join(".download");
    if work.exists() {
        fs::remove_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    }
    fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let url = format!(
        "https://github.com/AndreRH/hangover/releases/download/hangover-{HANGOVER_VERSION}/{}",
        build.file
    );
    let archive = work.join(build.file);
    download_verified(&url, &archive, build.size, build.sha256)?;
    eprintln!("armor: extracting");
    run(Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&work))?;
    let partial = engines.join(format!("{target_name}.partial"));
    if partial.exists() {
        fs::remove_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    }
    fs::create_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    let debs: Vec<PathBuf> = fs::read_dir(&work)
        .map_err(|e| format!("{}: {e}", work.display()))?
        .flatten()
        .map(|d| d.path())
        .filter(|p| p.extension().is_some_and(|e| e == "deb"))
        .collect();
    for package in PACKAGES {
        let deb = debs
            .iter()
            .find(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(package))
            })
            .ok_or_else(|| format!("{} has no {package}*.deb", build.file))?;
        let mut file = File::open(deb).map_err(|e| format!("{}: {e}", deb.display()))?;
        let data = work.join("data.tar");
        let name = {
            let mut out = File::create(&data).map_err(|e| format!("{}: {e}", data.display()))?;
            extract_ar_member(&mut file, "data.tar", &mut out)
                .map_err(|e| format!("{}: {e}", deb.display()))?
        };
        // tar recognizes the compression (xz, zstd, gzip) by itself.
        run(Command::new("tar")
            .arg("-xf")
            .arg(&data)
            .arg("-C")
            .arg(&partial))
        .map_err(|e| format!("{} ({name}): {e}", deb.display()))?;
    }
    if target.exists() {
        fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    fs::rename(&partial, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    let dxvk = engines.join(DXVK_DIR);
    if !dxvk.is_dir() {
        run(Command::new("tar")
            .arg("-xzf")
            .arg(work.join(DXVK_ARCHIVE))
            .arg("-C")
            .arg(engines))?;
    }
    point_link(&engines.join("wine"), &target_name)?;
    point_link(&engines.join("dxvk"), DXVK_DIR)?;
    fs::remove_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    fetch_mono(engines)?;
    eprintln!(
        "armor: Hangover {HANGOVER_VERSION} ({} {}), DXVK and Wine Mono are in {}; Wine is \
         LGPL-2.1, FEX and box64 MIT, DXVK zlib (their notices are in usr/share/doc); Wine \
         Mono's licenses come with its installer",
        build.distribution,
        build.codename,
        engines.display()
    );
    let missing = missing_libraries(&target.join("usr/lib/wine/aarch64-unix"));
    if !missing.is_empty() {
        eprintln!(
            "armor: Wine also uses these host libraries, which are missing (needed only for \
             what they do: scanners, the Wayland driver, ...): {}",
            missing.join(", ")
        );
    }
    Ok(())
}

/// `armor engine <subcommand>`.
pub fn main(args: &[OsString], data_home: &Path) -> Result<(), String> {
    let engines = data_home.join("armor").join("engines");
    match args {
        [command] if command == "fetch" => fetch(&engines),
        _ => Err("usage: armor engine fetch".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn the_build_follows_the_distribution() {
        let pi_os =
            "PRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nID=debian\nVERSION_CODENAME=trixie\n";
        assert_eq!(build_for(pi_os).unwrap().codename, "trixie");
        let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\nVERSION_CODENAME=xia\nUBUNTU_CODENAME=noble\n";
        assert_eq!(
            build_for(mint).unwrap().file,
            "hangover_11.16_ubuntu2404_noble_arm64.tar"
        );
        let derivative = "ID=raspbian\nID_LIKE=debian\nVERSION_CODENAME=bookworm\n";
        assert_eq!(build_for(derivative).unwrap().codename, "bookworm");
        let fedora = "ID=fedora\nVERSION_ID=43\n";
        assert!(build_for(fedora).unwrap_err().contains("debian trixie"));
        assert_eq!(
            os_release_value("ID='arch'\n", "ID").as_deref(),
            Some("arch")
        );
    }

    fn ar(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = b"!<arch>\n".to_vec();
        for (name, data) in members {
            let header = format!(
                "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                name,
                0,
                0,
                0,
                100644,
                data.len()
            );
            assert_eq!(header.len(), 60);
            archive.extend_from_slice(header.as_bytes());
            archive.extend_from_slice(data);
            if data.len() % 2 == 1 {
                archive.push(b'\n');
            }
        }
        archive
    }

    #[test]
    fn ar_members_are_found() {
        let deb = ar(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar.xz", b"odd"),
            ("data.tar.xz", b"the data"),
        ]);
        let mut out = Vec::new();
        let name = extract_ar_member(&mut Cursor::new(&deb), "data.tar", &mut out).unwrap();
        assert_eq!(
            (name.as_str(), out.as_slice()),
            ("data.tar.xz", &b"the data"[..])
        );
        let mut out = Vec::new();
        assert!(extract_ar_member(&mut Cursor::new(&deb), "nothing", &mut out).is_err());
        assert!(extract_ar_member(&mut Cursor::new(b"PK\x03\x04"), "data", &mut out).is_err());
        let truncated = &deb[..deb.len() - 3];
        assert!(
            extract_ar_member(&mut Cursor::new(truncated), "data.tar", &mut Vec::new()).is_err()
        );
    }
}
