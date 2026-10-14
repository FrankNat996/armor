//! x86-64 Debian packages for `armor install` (milestone M7, ADR-0033): the
//! package's dependencies are resolved against the distribution's own amd64
//! index, verified like apt does (the signed `InRelease` checked with
//! `gpgv` and the archive keyring, then the index's SHA-256), and every
//! package is downloaded into a shared cache, checked against the index's
//! SHA-256 and only unpacked — no maintainer scripts, no root — into the
//! application's root.

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::engines;

/// One stanza of a Debian control file or `Packages` index: its fields in
/// order (continuation lines appended to the field they continue).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stanza(pub Vec<(String, String)>);

impl Stanza {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

/// The stanzas of a control file or index, separated by blank lines.
pub fn parse_stanzas(text: &str) -> Vec<Stanza> {
    let mut stanzas = Vec::new();
    let mut current = Stanza::default();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.0.is_empty() {
                stanzas.push(std::mem::take(&mut current));
            }
        } else if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = current.0.last_mut() {
                value.push('\n');
                value.push_str(line.trim());
            }
        } else if let Some((key, value)) = line.split_once(':') {
            current
                .0
                .push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    if !current.0.is_empty() {
        stanzas.push(current);
    }
    stanzas
}

/// The alternatives of each dependency in a `Depends`-style field, as bare
/// package names (version constraints and `:any` qualifiers dropped).
pub fn dependency_alternatives(field: &str) -> Vec<Vec<String>> {
    field
        .split(',')
        .map(|dep| {
            dep.split('|')
                .filter_map(|alt| {
                    let name = alt.split_whitespace().next()?;
                    let name = name.split(':').next()?;
                    (!name.is_empty()).then(|| name.to_string())
                })
                .collect::<Vec<_>>()
        })
        .filter(|alts| !alts.is_empty())
        .collect()
}

/// A `Packages` index: the packages by name, and who provides each
/// virtual package.
pub struct Index {
    pub packages: HashMap<String, Stanza>,
    pub provides: HashMap<String, Vec<String>>,
}

impl Index {
    pub fn new(stanzas: Vec<Stanza>) -> Index {
        let mut packages = HashMap::new();
        let mut provides: HashMap<String, Vec<String>> = HashMap::new();
        for stanza in stanzas {
            let Some(name) = stanza.get("Package").map(str::to_string) else {
                continue;
            };
            for alts in dependency_alternatives(stanza.get("Provides").unwrap_or("")) {
                for virtual_name in alts {
                    provides.entry(virtual_name).or_default().push(name.clone());
                }
            }
            packages.entry(name).or_insert(stanza);
        }
        Index { packages, provides }
    }

    /// The real package for `name`: itself, or its first provider.
    fn resolve(&self, name: &str) -> Option<&str> {
        if let Some((key, _)) = self.packages.get_key_value(name) {
            return Some(key);
        }
        self.provides
            .get(name)
            .and_then(|providers| providers.first())
            .map(String::as_str)
    }
}

/// Packages an application root never needs: the package manager's own
/// machinery, which only maintainer scripts (never run) would use.
pub const NOT_NEEDED: &[&str] = &[
    "debconf",
    "debconf-2.0",
    "cdebconf",
    "dpkg",
    "install-info",
    "init-system-helpers",
    "adduser",
    "passwd",
    "systemd",
    "systemd-sysv",
    "base-passwd",
];

/// The closure of `roots` (dependencies, each a list of alternatives) and
/// of their `Pre-Depends` and `Depends` in `index`, in the
/// order found: for each dependency the first alternative the index has
/// (as a package or a provider). Returns the packages and the
/// dependencies no alternative of which is in the index.
pub fn closure(index: &Index, roots: &[Vec<String>]) -> (Vec<String>, Vec<String>) {
    let mut found: Vec<String> = Vec::new();
    let mut missing = Vec::new();
    let mut todo: Vec<Vec<String>> = roots.to_vec();
    let mut position = 0;
    while position < todo.len() {
        let alternatives = todo[position].clone();
        position += 1;
        if alternatives
            .iter()
            .any(|a| NOT_NEEDED.contains(&a.as_str()))
        {
            continue;
        }
        let Some(name) = alternatives.iter().find_map(|a| index.resolve(a)) else {
            missing.push(alternatives.join(" | "));
            continue;
        };
        if found.iter().any(|f| f == name) {
            continue;
        }
        found.push(name.to_string());
        let stanza = &index.packages[name];
        for field in ["Pre-Depends", "Depends"] {
            todo.extend(dependency_alternatives(stanza.get(field).unwrap_or("")));
        }
    }
    (found, missing)
}

/// The SHA-256 and size of `path` in a (verified) `Release` file.
pub fn release_entry(release: &str, path: &str) -> Option<(String, u64)> {
    let stanza = parse_stanzas(release).into_iter().next()?;
    let list = stanza.get("SHA256")?;
    list.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.as_slice() {
            [hash, size, name] if *name == path => Some((hash.to_string(), size.parse().ok()?)),
            _ => None,
        }
    })
}

/// The archive armor takes x86-64 libraries from, after the host's
/// distribution (`/etc/os-release`).
pub struct Archive {
    pub mirror: &'static str,
    pub suite: String,
    pub components: &'static [&'static str],
    pub keyring: &'static str,
}

pub fn archive_for(os_release: &str) -> Result<Archive, String> {
    let get = |key| engines::os_release_value(os_release, key).unwrap_or_default();
    let (id, like, codename) = (get("ID"), get("ID_LIKE"), get("VERSION_CODENAME"));
    let ubuntu = get("UBUNTU_CODENAME");
    if id == "ubuntu" || !ubuntu.is_empty() {
        let suite = if ubuntu.is_empty() { codename } else { ubuntu };
        return Ok(Archive {
            mirror: "http://archive.ubuntu.com/ubuntu",
            suite,
            components: &["main", "universe"],
            keyring: "/usr/share/keyrings/ubuntu-archive-keyring.gpg",
        });
    }
    if id == "debian" || like.split_whitespace().any(|l| l == "debian") {
        if !matches!(codename.as_str(), "bookworm" | "trixie" | "forky") {
            return Err(format!(
                "Debian {codename:?}: armor knows the amd64 archive of bookworm, trixie and forky"
            ));
        }
        return Ok(Archive {
            mirror: "https://deb.debian.org/debian",
            suite: codename,
            components: &["main"],
            keyring: "/usr/share/keyrings/debian-archive-keyring.gpg",
        });
    }
    Err(format!(
        "{id}: installing x86-64 .deb packages needs a Debian or Ubuntu host (the libraries come \
         from their amd64 archive)"
    ))
}

fn curl(url: &str, file: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(file)
        .arg(url)
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("download of {url} failed: {status}"))
    }
}

/// The signed `InRelease` of `archive`, checked with `gpgv` and cached for a
/// day in `dir`; returns its verified text.
fn verified_release(archive: &Archive, dir: &Path, base: &str) -> Result<String, String> {
    let in_release = dir.join("InRelease");
    let fresh = fs::metadata(&in_release)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < 24 * 3600);
    if !fresh {
        eprintln!("armor: reading the amd64 index of {base}");
        curl(&format!("{base}/InRelease"), &in_release)?;
    }
    // gpgv prints the signed text only when the signature is good.
    let verified = Command::new("gpgv")
        .arg("--keyring")
        .arg(archive.keyring)
        .args(["--output", "-"])
        .arg(&in_release)
        .output()
        .map_err(|e| format!("gpgv: {e} (package gpgv)"))?;
    if !verified.status.success() {
        let _ = fs::remove_file(&in_release);
        return Err(format!(
            "{base}/InRelease: the signature does not verify with {}: {}",
            archive.keyring,
            String::from_utf8_lossy(&verified.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&verified.stdout).into_owned())
}

/// The file `path` of the release (as `InRelease` lists it), downloaded into
/// `dir` as `local` unless the copy there already has the listed SHA-256.
fn verified_file(
    release: &str,
    base: &str,
    dir: &Path,
    path: &str,
    local: &str,
) -> Result<PathBuf, String> {
    let (hash, size) = release_entry(release, path)
        .ok_or_else(|| format!("{base}/InRelease does not list {path}"))?;
    let file = dir.join(local);
    let current = file.is_file() && engines::sha256(&file)? == hash;
    if !current {
        curl(&format!("{base}/{path}"), &file)?;
        let actual = engines::sha256(&file)?;
        let actual_size = fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        if actual != hash || actual_size != size {
            let _ = fs::remove_file(&file);
            return Err(format!(
                "{base}/{path}: SHA-256 or size differ from InRelease"
            ));
        }
    }
    Ok(file)
}

/// A compressed file's text, decompressed by the host's `xz` or `gzip`.
fn decompress(tool: &str, file: &Path) -> Result<String, String> {
    let text = Command::new(tool)
        .arg("-dc")
        .arg(file)
        .output()
        .map_err(|e| format!("{tool}: {e}"))?;
    if !text.status.success() {
        return Err(format!("{tool} {}: failed", file.display()));
    }
    Ok(String::from_utf8_lossy(&text.stdout).into_owned())
}

fn index_dir(archive: &Archive, cache: &Path) -> Result<(PathBuf, String), String> {
    let dir = cache.join(format!("index-{}-amd64", archive.suite));
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok((dir, format!("{}/dists/{}", archive.mirror, archive.suite)))
}

/// The amd64 index of `archive`, verified, cached for a day in `cache`.
pub fn fetch_index(archive: &Archive, cache: &Path) -> Result<Index, String> {
    let (dir, base) = index_dir(archive, cache)?;
    let release = verified_release(archive, &dir, &base)?;
    let mut stanzas = Vec::new();
    for component in archive.components {
        let path = format!("{component}/binary-amd64/Packages.xz");
        let file = verified_file(
            &release,
            &base,
            &dir,
            &path,
            &format!("{component}-Packages.xz"),
        )?;
        stanzas.extend(parse_stanzas(&decompress("xz", &file)?));
    }
    Ok(Index::new(stanzas))
}

/// Shared libraries by file name (`libusb-1.0.so.0`) and the package that
/// installs each in a library directory, from the archive's amd64
/// `Contents` index (verified like the package index): what an AppImage
/// expects from the system without saying which package that is.
pub fn library_providers(
    archive: &Archive,
    cache: &Path,
) -> Result<HashMap<String, String>, String> {
    let (dir, base) = index_dir(archive, cache)?;
    let release = verified_release(archive, &dir, &base)?;
    let mut providers = HashMap::new();
    for component in archive.components {
        // Debian lists it per component, Ubuntu once for the suite.
        let file = verified_file(
            &release,
            &base,
            &dir,
            &format!("{component}/Contents-amd64.gz"),
            &format!("{component}-Contents-amd64.gz"),
        )
        .or_else(|_| {
            verified_file(
                &release,
                &base,
                &dir,
                "Contents-amd64.gz",
                "Contents-amd64.gz",
            )
        })?;
        for (name, package) in library_lines(&decompress("gzip", &file)?) {
            providers.entry(name).or_insert(package);
        }
    }
    Ok(providers)
}

/// The (file name, package) pairs of a `Contents` index for files in the
/// library directories (`usr/lib/x86_64-linux-gnu`, `lib/x86_64-linux-gnu`,
/// `usr/lib`, `lib`) whose names hold `.so`.
pub fn library_lines(contents: &str) -> Vec<(String, String)> {
    const DIRS: [&str; 4] = [
        "usr/lib/x86_64-linux-gnu/",
        "lib/x86_64-linux-gnu/",
        "usr/lib/",
        "lib/",
    ];
    contents
        .lines()
        .filter_map(|line| {
            let (path, packages) = line.rsplit_once(char::is_whitespace)?;
            let path = path.trim_end();
            let dir = DIRS.iter().find(|d| path.starts_with(*d))?;
            let name = &path[dir.len()..];
            if name.contains('/') || !name.contains(".so") {
                return None;
            }
            let package = packages.split(',').next()?.rsplit('/').next()?;
            Some((name.to_string(), package.to_string()))
        })
        .collect()
}

/// The control stanza of the `.deb` file `deb`.
pub fn control_of(deb: &Path, scratch: &Path) -> Result<Stanza, String> {
    let control = scratch.join("control.tar");
    {
        let mut file = File::open(deb).map_err(|e| format!("{}: {e}", deb.display()))?;
        let mut out = File::create(&control).map_err(|e| format!("{}: {e}", control.display()))?;
        engines::extract_ar_member(&mut file, "control.tar", &mut out)
            .map_err(|e| format!("{}: {e}", deb.display()))?;
    }
    let text = Command::new("tar")
        .arg("-xOf")
        .arg(&control)
        .arg("./control")
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    let _ = fs::remove_file(&control);
    if !text.status.success() {
        return Err(format!("{}: no control file", deb.display()));
    }
    parse_stanzas(&String::from_utf8_lossy(&text.stdout))
        .into_iter()
        .next()
        .ok_or_else(|| format!("{}: empty control file", deb.display()))
}

/// The paths (relative, `./usr/bin/x`) of the files in the `.deb` `deb`.
pub fn file_list(deb: &Path, scratch: &Path) -> Result<Vec<String>, String> {
    let data = scratch.join("list.tar");
    {
        let mut file = File::open(deb).map_err(|e| format!("{}: {e}", deb.display()))?;
        let mut out = File::create(&data).map_err(|e| format!("{}: {e}", data.display()))?;
        engines::extract_ar_member(&mut file, "data.tar", &mut out)
            .map_err(|e| format!("{}: {e}", deb.display()))?;
    }
    let listing = Command::new("tar")
        .arg("-tf")
        .arg(&data)
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    let _ = fs::remove_file(&data);
    Ok(String::from_utf8_lossy(&listing.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

/// Unpacks the data of the `.deb` file `deb` into `root`.
pub fn unpack(deb: &Path, root: &Path, scratch: &Path) -> Result<(), String> {
    let data = scratch.join("data.tar");
    {
        let mut file = File::open(deb).map_err(|e| format!("{}: {e}", deb.display()))?;
        let mut out = File::create(&data).map_err(|e| format!("{}: {e}", data.display()))?;
        engines::extract_ar_member(&mut file, "data.tar", &mut out)
            .map_err(|e| format!("{}: {e}", deb.display()))?;
    }
    let status = Command::new("tar")
        .args(["-xf"])
        .arg(&data)
        .arg("-C")
        .arg(root)
        .args([
            "--no-same-owner",
            "--no-same-permissions",
            "--delay-directory-restore",
        ])
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    let _ = fs::remove_file(&data);
    if status.success() {
        Ok(())
    } else {
        Err(format!("{}: unpacking failed", deb.display()))
    }
}

/// Downloads `package` of `index` into the shared `cache` (checked against
/// the index's SHA-256), and returns its path.
pub fn download(
    archive: &Archive,
    index: &Index,
    package: &str,
    cache: &Path,
) -> Result<PathBuf, String> {
    let stanza = &index.packages[package];
    let filename = stanza
        .get("Filename")
        .ok_or_else(|| format!("{package}: no Filename in the index"))?;
    let hash = stanza
        .get("SHA256")
        .ok_or_else(|| format!("{package}: no SHA256 in the index"))?;
    let name = filename.rsplit('/').next().unwrap_or(filename);
    let dir = cache.join("debs");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join(name);
    if file.is_file() && engines::sha256(&file)? == hash {
        return Ok(file);
    }
    curl(&format!("{}/{filename}", archive.mirror), &file)?;
    if engines::sha256(&file)? != hash {
        let _ = fs::remove_file(&file);
        return Err(format!(
            "{filename}: SHA-256 differs from the verified index"
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "Package: app
Version: 1.0
Depends: libc6 (>= 2.36), libgtk-3-0 | libgtk-4-1, mail-transport-agent, debconf (>= 0.5) | debconf-2.0
Pre-Depends: dpkg (>= 1.15)

Package: libc6
Version: 2.41-12

Package: libgtk-3-0t64
Provides: libgtk-3-0 (= 3.24.49)
Depends: libc6, libglib2.0-0t64:any
Description: GTK
 continued line

Package: libglib2.0-0t64
Depends: libc6
";

    #[test]
    fn index_and_closure() {
        let stanzas = parse_stanzas(INDEX);
        assert_eq!(stanzas.len(), 4);
        assert_eq!(stanzas[2].get("Description"), Some("GTK\ncontinued line"));
        let index = Index::new(stanzas);
        let (found, missing) = closure(&index, &[vec!["app".to_string()]]);
        assert_eq!(found, ["app", "libc6", "libgtk-3-0t64", "libglib2.0-0t64"]);
        assert_eq!(missing, ["mail-transport-agent"]);
    }

    #[test]
    fn alternatives_drop_versions_and_qualifiers() {
        assert_eq!(
            dependency_alternatives("a (>= 1), b:any | c [amd64], , d"),
            [
                vec!["a".to_string()],
                vec!["b".to_string(), "c".to_string()],
                vec!["d".to_string()]
            ]
        );
    }

    #[test]
    fn release_entries() {
        let release = "Origin: Debian\nSuite: stable\nSHA256:\n abc 123 main/binary-amd64/Packages.xz\n def 9 main/binary-arm64/Packages.xz\n";
        assert_eq!(
            release_entry(release, "main/binary-amd64/Packages.xz"),
            Some(("abc".to_string(), 123))
        );
        assert_eq!(release_entry(release, "contrib/x"), None);
    }

    #[test]
    fn libraries_from_contents() {
        let contents = "usr/lib/x86_64-linux-gnu/libusb-1.0.so.0          libs/libusb-1.0-0
usr/lib/x86_64-linux-gnu/libz.so.1                 libs/zlib1g,libs/zlib1g-dev
usr/share/doc/libusb/README                        doc/libusb-1.0-doc
usr/lib/x86_64-linux-gnu/gconv/IBM850.so           libs/libc6
usr/bin/usb-devices                                admin/usbutils
";
        assert_eq!(
            library_lines(contents),
            [
                ("libusb-1.0.so.0".to_string(), "libusb-1.0-0".to_string()),
                ("libz.so.1".to_string(), "zlib1g".to_string()),
            ]
        );
    }

    #[test]
    fn archives_follow_the_host() {
        let pi = "ID=debian\nVERSION_CODENAME=trixie\n";
        let archive = archive_for(pi).unwrap();
        assert_eq!(
            (archive.mirror, archive.suite.as_str()),
            ("https://deb.debian.org/debian", "trixie")
        );
        let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\nVERSION_CODENAME=xia\nUBUNTU_CODENAME=noble\n";
        assert_eq!(archive_for(mint).unwrap().suite, "noble");
        assert!(archive_for("ID=fedora\n").is_err());
        assert!(archive_for("ID=debian\nVERSION_CODENAME=buster\n").is_err());
    }
}
