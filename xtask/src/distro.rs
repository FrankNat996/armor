//! `cargo xtask distro-check`: armor on distributions other than Debian
//! and Ubuntu (ADR-0043), in podman containers on this machine.
//!
//! The archive of `cargo xtask package` is installed with its `install.sh`
//! in a container of each distribution, then, as a user would:
//! - an x86-64 program (Debian's Python, from the root of `fetch-rootfs`)
//!   and an i386 one (coreutils' `sort`, `fetch-rootfs --i386`) run under
//!   armor; their output must be the one armor gives on this machine;
//! - `armor install` of an x86-64 `.deb` (Debian's `hello`, pinned): its
//!   libraries come from Debian's archive, checked with the keyring armor
//!   carries, and `armor run` runs it;
//! - `armor engine fetch` downloads the Windows engine (Hangover's Debian
//!   13 build, about 380 MB) and its Wine runs `cmd /c ver`.
//!
//! Fedora comes from Fedora's registry; Arch Linux ARM from its official
//! root archive, whose signature is checked against the build key's
//! pinned fingerprint before it is imported. Downloads go to
//! `target/distros/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Paths, exec, rootfs};

/// Debian's `hello` for amd64 (trixie), pinned by the SHA-256 of trixie's
/// `Packages` index (2026-10-15).
const HELLO_URL: &str = "http://deb.debian.org/debian/pool/main/h/hello/hello_2.10-5_amd64.deb";
const HELLO_SHA256: &str = "4536aabbb75ec21ffe161099ee4b97274945770bdb0682e25ec322421211ca5e";

/// Arch Linux ARM's root archive and the fingerprint of its build key
/// ("Arch Linux ARM Build System <builder@archlinuxarm.org>").
const ALARM_URL: &str = "http://os.archlinuxarm.org/os/ArchLinuxARM-aarch64-latest.tar.gz";
const ALARM_KEY: &str = "68B3537F39A313B3E574D06777193F152BDBE6A6";
const ALARM_IMAGE: &str = "localhost/armor-archlinuxarm:latest";

/// A distribution: its image, and the shell command that installs what
/// armor needs from the host (the tools of ADR-0043 and the X11 and
/// graphics libraries Wine links).
struct Distribution {
    name: &'static str,
    image: &'static str,
    prepare: &'static str,
}

const DISTRIBUTIONS: &[Distribution] = &[
    Distribution {
        name: "Fedora 42",
        image: "registry.fedoraproject.org/fedora:42",
        prepare: "dnf -q -y install tar gzip xz findutils gnupg2 unzip curl freetype fontconfig \
                  gnutls libX11 libXext libXrender libXrandr libXi libXcursor libXcomposite \
                  libXfixes libXinerama mesa-libGL vulkan-loader",
    },
    Distribution {
        name: "Arch Linux ARM",
        image: ALARM_IMAGE,
        prepare: "pacman-key --init && pacman-key --populate archlinuxarm && \
                  pacman -Syu --noconfirm --needed --disable-sandbox tar gzip xz findutils gnupg unzip curl \
                  freetype2 fontconfig gnutls libx11 libxext libxrender libxrandr libxi \
                  libxcursor libxcomposite libxfixes libxinerama mesa vulkan-icd-loader",
    },
];

/// What runs inside each container, after `prepare`: one `ok <check>` or
/// `FAIL <check>` line per check. `/armor.tar.gz`, `/guest` (the guest
/// roots) and `/debs` are mounted read-only.
const CHECKS: &str = r#"
set -u
check() { if [ "$2" = "$3" ]; then echo "ok $1"; else echo "FAIL $1: got [$2], want [$3]"; fi; }
cd /tmp && tar -xzf /armor.tar.gz && sh armor_*/install.sh >/dev/null
A=$HOME/.local/bin/armor
check install "$(sed -n 's/^kind=//p' $HOME/.local/share/armor/installed)" archive
check x86-64 "$($A --root /guest/rootfs /guest/rootfs/usr/bin/python3.13 -c 'import sys, hashlib; print(sys.version.split()[0], hashlib.sha256(b"armor").hexdigest())' 2>&1)" "$EXPECT_PYTHON"
check i386 "$($A --root /guest/rootfs-i386 /guest/rootfs-i386/usr/bin/sort --version 2>&1 | head -1)" "$EXPECT_SORT"
$A install /debs/hello_2.10-5_amd64.deb >/tmp/install.log 2>&1 || tail -5 /tmp/install.log
check deb "$($A run hello_2.10-5_amd64 /usr/bin/hello -g 'hello from armor' 2>&1)" "hello from armor"
$A engine fetch >/tmp/engine.log 2>&1 || tail -5 /tmp/engine.log
mkdir -p $HOME/wine
ver=$(WINEPREFIX=$HOME/wine/prefix WINEDEBUG=-all timeout 600 $HOME/.local/share/armor/engines/wine/usr/bin/wine cmd /c ver 2>/dev/null | tr -d '\r' | grep -o 'Microsoft Windows')
check wine "$ver" "Microsoft Windows"
"#;

fn run_ok(cmd: &mut Command, what: &str) -> Result<String, String> {
    let output = exec(cmd)?;
    if !output.status.success() {
        return Err(format!(
            "{what} failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Downloads `url` into `dir` unless it is there.
fn download(url: &str, dir: &Path) -> Result<PathBuf, String> {
    let file = dir.join(url.rsplit('/').next().expect("a URL has a last part"));
    if !file.is_file() {
        run_ok(
            Command::new("curl")
                .args(["-fsSL", "-o"])
                .arg(&file)
                .arg(url),
            &format!("downloading {url}"),
        )?;
    }
    Ok(file)
}

/// The Arch Linux ARM image, imported from the official root archive once
/// its signature checks out against `ALARM_KEY`.
fn arch_image(dir: &Path) -> Result<(), String> {
    let exists = Command::new("podman")
        .args(["image", "exists", ALARM_IMAGE])
        .status()
        .is_ok_and(|s| s.success());
    if exists {
        return Ok(());
    }
    let archive = download(ALARM_URL, dir)?;
    let signature = download(&format!("{ALARM_URL}.sig"), dir)?;
    let home = dir.join("gnupg");
    fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    run_ok(
        Command::new("gpg").env("GNUPGHOME", &home).args([
            "--batch",
            "--keyserver",
            "hkps://keyserver.ubuntu.com",
            "--recv-keys",
            ALARM_KEY,
        ]),
        "fetching Arch Linux ARM's build key",
    )?;
    let status = run_ok(
        Command::new("gpg")
            .env("GNUPGHOME", &home)
            .args(["--batch", "--status-fd", "1", "--verify"])
            .arg(&signature)
            .arg(&archive),
        "checking Arch Linux ARM's signature",
    )?;
    if !status
        .lines()
        .any(|l| l.starts_with("[GNUPG:] VALIDSIG ") && l.contains(ALARM_KEY))
    {
        return Err(format!(
            "{}: no valid signature by {ALARM_KEY}",
            archive.display()
        ));
    }
    run_ok(
        Command::new("podman")
            .arg("import")
            .arg(&archive)
            .arg(ALARM_IMAGE),
        "importing Arch Linux ARM",
    )?;
    Ok(())
}

pub fn check(paths: &Paths) -> Result<(), String> {
    let root = &paths.root;
    let amd64 = rootfs::dir_of(paths, rootfs::Arch::Amd64);
    let i386 = rootfs::dir_of(paths, rootfs::Arch::I386);
    if !rootfs::available_of(paths, rootfs::Arch::Amd64)
        || !rootfs::available_of(paths, rootfs::Arch::I386)
    {
        return Err(
            "the guest roots are missing: run `cargo xtask fetch-rootfs` and `cargo xtask \
             fetch-rootfs --i386`"
                .to_string(),
        );
    }
    let (_, tarball) = crate::package::build(root)?;
    let dir = root.join("target/distros");
    let debs = dir.join("debs");
    fs::create_dir_all(&debs).map_err(|e| format!("{}: {e}", debs.display()))?;
    let hello = download(HELLO_URL, &debs)?;
    let digest = run_ok(Command::new("sha256sum").arg(&hello), "sha256sum")?;
    if digest.split_whitespace().next() != Some(HELLO_SHA256) {
        let _ = fs::remove_file(&hello);
        return Err(format!(
            "{}: SHA-256 is not {HELLO_SHA256}",
            hello.display()
        ));
    }
    arch_image(&dir)?;
    // The outputs armor gives here, which every distribution must give.
    let armor = root.join("target/release/armor");
    let python = run_ok(
        Command::new(&armor)
            .arg("--root")
            .arg(&amd64)
            .arg(amd64.join("usr/bin/python3.13"))
            .args([
                "-c",
                "import sys, hashlib; print(sys.version.split()[0], hashlib.sha256(b\"armor\").hexdigest())",
            ]),
        "python under armor",
    )?;
    let sort = run_ok(
        Command::new(&armor)
            .arg("--root")
            .arg(&i386)
            .arg(i386.join("usr/bin/sort"))
            .arg("--version"),
        "sort under armor",
    )?;
    let sort = sort.lines().next().unwrap_or_default().to_string();
    let mut failed = 0;
    for distribution in DISTRIBUTIONS {
        println!("== {} ({})", distribution.name, distribution.image);
        let script = format!(
            "( {} ) >/tmp/prepare.log 2>&1 || {{ echo 'FAIL prepare'; tail -5 /tmp/prepare.log; }}\n{CHECKS}",
            distribution.prepare
        );
        let mount = |from: &Path, to: &str| format!("{}:{to}:ro", from.display());
        let output = exec(
            Command::new("podman")
                .args(["run", "--rm"])
                .args(["-v", &mount(&tarball, "/armor.tar.gz")])
                .args(["-v", &mount(&root.join("target/guest"), "/guest")])
                .args(["-v", &mount(&debs, "/debs")])
                .args(["-e", &format!("EXPECT_PYTHON={}", python.trim_end())])
                .args(["-e", &format!("EXPECT_SORT={sort}")])
                .arg(distribution.image)
                .args(["sh", "-c", &script]),
        )?;
        let text = String::from_utf8_lossy(&output.stdout);
        print!("{text}");
        let fails = text.lines().filter(|l| l.starts_with("FAIL")).count();
        let oks = text.lines().filter(|l| l.starts_with("ok ")).count();
        if fails > 0 || oks != 5 || !output.status.success() {
            failed += 1;
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
    }
    if failed == 0 {
        println!(
            "distro-check: every check passed on {} distributions",
            DISTRIBUTIONS.len()
        );
        Ok(())
    } else {
        Err(format!("distro-check: {failed} distribution(s) failed"))
    }
}
