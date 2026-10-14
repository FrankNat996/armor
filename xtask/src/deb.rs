//! Downloading and unpacking official Debian packages (`fetch-busybox`,
//! `fetch-rootfs`). Every package is pinned by version and SHA-256 and
//! verified before anything is extracted. Packages are only unpacked with
//! `ar` and `tar`: maintainer scripts never run, and nothing from a package
//! is executed outside the emulators.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::exec;

/// Base of every package URL: the `Filename` field of a Packages index is
/// relative to it.
pub const MIRROR: &str = "https://deb.debian.org/debian/";

/// A pinned package: its path in the archive (`pool/...`) and SHA-256.
pub struct Package {
    pub pool_path: &'static str,
    pub sha256: &'static str,
}

/// Downloads `package` into `work` and checks its SHA-256. Returns the path
/// of the `.deb`.
pub fn download(package: &Package, work: &Path) -> Result<PathBuf, String> {
    let url = format!("{MIRROR}{}", package.pool_path);
    let name = package
        .pool_path
        .rsplit('/')
        .next()
        .expect("rsplit yields at least one item");
    let deb = work.join(name);
    run(Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&deb)
        .arg(&url))?;
    let sum = exec(Command::new("sha256sum").arg(&deb))?;
    let actual = String::from_utf8_lossy(&sum.stdout);
    if actual.split_whitespace().next() != Some(package.sha256) {
        return Err(format!(
            "SHA-256 mismatch for {url}: expected {}, got {}",
            package.sha256,
            actual.trim()
        ));
    }
    Ok(deb)
}

/// Extracts the data archive of `deb` into `dest` (only `members`, if not
/// empty), using `scratch`, an empty directory, for the outer `ar` archive.
pub fn extract(deb: &Path, scratch: &Path, dest: &Path, members: &[&str]) -> Result<(), String> {
    run(Command::new("ar").arg("x").arg(deb).current_dir(scratch))?;
    let data = fs::read_dir(scratch)
        .map_err(|e| format!("cannot list {}: {e}", scratch.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("data.tar"))
        })
        .ok_or_else(|| format!("{} has no data.tar archive", deb.display()))?;
    run(Command::new("tar")
        .arg("-xf")
        .arg(&data)
        .arg("-C")
        .arg(dest)
        .args(members))?;
    for entry in
        fs::read_dir(scratch).map_err(|e| format!("cannot list {}: {e}", scratch.display()))?
    {
        let path = entry.map_err(|e| e.to_string())?.path();
        fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    }
    Ok(())
}

/// An empty directory at `dir`, removing what was there.
pub fn fresh_dir(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| format!("cannot clean {}: {e}", dir.display()))?;
    }
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))
}

pub fn run(cmd: &mut Command) -> Result<(), String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let output = exec(cmd)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{program}` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}
