//! `armor update`: armor updates itself from its GitHub releases (ADR-0039).
//!
//! `armor update --check` tells whether a newer release than the installed
//! package exists (`--porcelain`: one line `update\t<yes|no>\t<installed>\t
//! <newest>` for the window); `armor update` downloads the newest release's
//! `armor_<version>_arm64.deb`, checks its size and the SHA-256 that GitHub
//! publishes for it, and installs it with `pkexec apt-get install` (the
//! desktop asks for the administrator's password), or says which `sudo`
//! command to run where `pkexec` is missing. Pre-releases count: every
//! release so far is one. The installed applications are not touched.
//!
//! armor installed from its archive (`armor_<version>_arm64.tar.gz` and its
//! `install.sh`, on distributions other than Debian and Ubuntu: ADR-0043)
//! is updated from the newest release's archive instead, checked the same
//! way and installed by its own `install.sh` into the same place, without
//! the administrator. Versions are compared as dpkg does, in armor itself
//! (`compare_versions`), since only Debian and Ubuntu have dpkg.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The releases of armor's repository, newest first.
const RELEASES_URL: &str = "https://api.github.com/repos/FrankNat996/armor/releases?per_page=20";

/// A JSON value: just enough of JSON to read GitHub's answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Json::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// Parses a JSON text (RFC 8259); `None` if it is not valid.
pub fn parse_json(text: &str) -> Option<Json> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let value = json_value(bytes, &mut at)?;
    skip_space(bytes, &mut at);
    (at == bytes.len()).then_some(value)
}

fn skip_space(bytes: &[u8], at: &mut usize) {
    while *at < bytes.len() && matches!(bytes[*at], b' ' | b'\t' | b'\n' | b'\r') {
        *at += 1;
    }
}

fn json_value(bytes: &[u8], at: &mut usize) -> Option<Json> {
    skip_space(bytes, at);
    match *bytes.get(*at)? {
        b'{' => {
            *at += 1;
            let mut fields = Vec::new();
            skip_space(bytes, at);
            if bytes.get(*at) == Some(&b'}') {
                *at += 1;
                return Some(Json::Object(fields));
            }
            loop {
                skip_space(bytes, at);
                let key = json_string(bytes, at)?;
                skip_space(bytes, at);
                if bytes.get(*at) != Some(&b':') {
                    return None;
                }
                *at += 1;
                fields.push((key, json_value(bytes, at)?));
                skip_space(bytes, at);
                match bytes.get(*at)? {
                    b',' => *at += 1,
                    b'}' => {
                        *at += 1;
                        return Some(Json::Object(fields));
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            *at += 1;
            let mut items = Vec::new();
            skip_space(bytes, at);
            if bytes.get(*at) == Some(&b']') {
                *at += 1;
                return Some(Json::Array(items));
            }
            loop {
                items.push(json_value(bytes, at)?);
                skip_space(bytes, at);
                match bytes.get(*at)? {
                    b',' => *at += 1,
                    b']' => {
                        *at += 1;
                        return Some(Json::Array(items));
                    }
                    _ => return None,
                }
            }
        }
        b'"' => json_string(bytes, at).map(Json::Text),
        b't' if bytes[*at..].starts_with(b"true") => {
            *at += 4;
            Some(Json::Bool(true))
        }
        b'f' if bytes[*at..].starts_with(b"false") => {
            *at += 5;
            Some(Json::Bool(false))
        }
        b'n' if bytes[*at..].starts_with(b"null") => {
            *at += 4;
            Some(Json::Null)
        }
        _ => {
            let start = *at;
            while *at < bytes.len()
                && matches!(bytes[*at], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
            {
                *at += 1;
            }
            std::str::from_utf8(&bytes[start..*at])
                .ok()?
                .parse()
                .ok()
                .map(Json::Number)
        }
    }
}

fn json_string(bytes: &[u8], at: &mut usize) -> Option<String> {
    if bytes.get(*at) != Some(&b'"') {
        return None;
    }
    *at += 1;
    let mut out: Vec<u16> = Vec::new();
    let mut text = String::new();
    let flush = |out: &mut Vec<u16>, text: &mut String| {
        text.push_str(&String::from_utf16_lossy(out));
        out.clear();
    };
    loop {
        let byte = *bytes.get(*at)?;
        match byte {
            b'"' => {
                *at += 1;
                flush(&mut out, &mut text);
                return Some(text);
            }
            b'\\' => {
                let escape = *bytes.get(*at + 1)?;
                *at += 2;
                let simple = match escape {
                    b'"' => Some('"'),
                    b'\\' => Some('\\'),
                    b'/' => Some('/'),
                    b'b' => Some('\u{8}'),
                    b'f' => Some('\u{c}'),
                    b'n' => Some('\n'),
                    b'r' => Some('\r'),
                    b't' => Some('\t'),
                    b'u' => None,
                    _ => return None,
                };
                match simple {
                    Some(c) => {
                        flush(&mut out, &mut text);
                        text.push(c);
                    }
                    None => {
                        let hex = std::str::from_utf8(bytes.get(*at..*at + 4)?).ok()?;
                        out.push(u16::from_str_radix(hex, 16).ok()?);
                        *at += 4;
                    }
                }
            }
            _ => {
                flush(&mut out, &mut text);
                // A whole UTF-8 sequence at once.
                let len = match byte {
                    0x00..=0x7f => 1,
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    _ => 4,
                };
                text.push_str(std::str::from_utf8(bytes.get(*at..*at + len)?).ok()?);
                *at += len;
            }
        }
    }
}

/// How armor was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The `.deb`, with apt.
    Package,
    /// The archive's `install.sh` (ADR-0043).
    Archive,
}

impl Kind {
    /// The release asset this kind of installation is updated from.
    fn asset(self, version: &str) -> String {
        match self {
            Kind::Package => format!("armor_{version}_arm64.deb"),
            Kind::Archive => format!("armor_{version}_arm64.tar.gz"),
        }
    }
}

/// The newest release of armor that carries an arm64 package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub file: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// The newest non-draft release in GitHub's list (newest first) with the
/// asset for `kind` (`armor_<version>_arm64.deb` or `.tar.gz`) and its
/// SHA-256 digest.
pub fn newest_release(releases: &Json, kind: Kind) -> Option<Release> {
    let Json::Array(releases) = releases else {
        return None;
    };
    releases.iter().find_map(|release| {
        if release.get("draft") == Some(&Json::Bool(true)) {
            return None;
        }
        let tag = release.get("tag_name")?.text()?;
        let version = tag.strip_prefix('v').unwrap_or(tag);
        let file = kind.asset(version);
        let Json::Array(assets) = release.get("assets")? else {
            return None;
        };
        let asset = assets
            .iter()
            .find(|a| a.get("name").and_then(Json::text) == Some(file.as_str()))?;
        let sha256 = asset.get("digest")?.text()?.strip_prefix("sha256:")?;
        let Json::Number(size) = asset.get("size")? else {
            return None;
        };
        Some(Release {
            version: version.to_string(),
            url: asset.get("browser_download_url")?.text()?.to_string(),
            file,
            size: *size as u64,
            sha256: sha256.to_ascii_lowercase(),
        })
    })
}

/// The installed armor: how, which version, and for an archive the
/// prefix `install.sh` used (`~/.local` by default).
struct Installed {
    kind: Kind,
    version: String,
    prefix: Option<PathBuf>,
}

/// The `armor` package's version, if armor was installed from it.
fn package_version() -> Option<String> {
    let output = Command::new("dpkg-query")
        .args(["-W", "-f=${Version}", "armor"])
        .output()
        .ok()?;
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !version.is_empty()).then_some(version)
}

/// The version in an archive installation's `share/armor/installed`
/// (written by `install.sh`: lines `key=value`).
pub fn archive_version(installed: &str) -> Option<String> {
    installed
        .lines()
        .find_map(|line| line.strip_prefix("version="))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// How this armor was installed: the package, else an archive
/// installation whose `bin/armor` this is.
fn installed() -> Option<Installed> {
    if let Some(version) = package_version() {
        return Some(Installed {
            kind: Kind::Package,
            version,
            prefix: None,
        });
    }
    let exe = std::env::current_exe().ok()?;
    let prefix = exe.parent()?.parent()?.to_path_buf();
    let text = std::fs::read_to_string(prefix.join("share/armor/installed")).ok()?;
    Some(Installed {
        kind: Kind::Archive,
        version: archive_version(&text)?,
        prefix: Some(prefix),
    })
}

/// Compares two Debian versions as dpkg does (`[epoch:]upstream[-revision]`;
/// in each part runs of non-digits compare character by character, with
/// `~` before everything, even the end, and letters before other
/// characters, and runs of digits compare as numbers).
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    fn split(v: &str) -> (u64, &str, &str) {
        let (epoch, rest) = match v.split_once(':') {
            Some((e, rest)) if e.bytes().all(|c| c.is_ascii_digit()) => {
                (e.parse().unwrap_or(0), rest)
            }
            _ => (0, v),
        };
        match rest.rsplit_once('-') {
            Some((upstream, revision)) => (epoch, upstream, revision),
            None => (epoch, rest, ""),
        }
    }
    fn order(c: Option<u8>) -> i32 {
        match c {
            None => 0,
            Some(c) if c.is_ascii_digit() => 0,
            Some(c) if c.is_ascii_alphabetic() => i32::from(c),
            Some(b'~') => -1,
            Some(c) => i32::from(c) + 256,
        }
    }
    fn part(a: &[u8], b: &[u8]) -> Ordering {
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            while (i < a.len() && !a[i].is_ascii_digit()) || (j < b.len() && !b[j].is_ascii_digit())
            {
                let (x, y) = (order(a.get(i).copied()), order(b.get(j).copied()));
                if x != y {
                    return x.cmp(&y);
                }
                i += 1;
                j += 1;
            }
            while a.get(i) == Some(&b'0') {
                i += 1;
            }
            while b.get(j) == Some(&b'0') {
                j += 1;
            }
            let mut first_difference = Ordering::Equal;
            while i < a.len() && a[i].is_ascii_digit() && j < b.len() && b[j].is_ascii_digit() {
                if first_difference == Ordering::Equal {
                    first_difference = a[i].cmp(&b[j]);
                }
                i += 1;
                j += 1;
            }
            if i < a.len() && a[i].is_ascii_digit() {
                return Ordering::Greater;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                return Ordering::Less;
            }
            if first_difference != Ordering::Equal {
                return first_difference;
            }
        }
        Ordering::Equal
    }
    let (ea, ua, ra) = split(a);
    let (eb, ub, rb) = split(b);
    ea.cmp(&eb)
        .then_with(|| part(ua.as_bytes(), ub.as_bytes()))
        .then_with(|| part(ra.as_bytes(), rb.as_bytes()))
}

/// What `armor update --check` found.
struct Check {
    installed: Installed,
    release: Release,
    available: bool,
}

fn check() -> Result<Check, String> {
    let installed = installed().ok_or(
        "armor was not installed from its package (`apt install ./armor_…_arm64.deb`) or its \
         archive (`install.sh`), so it cannot update itself; build it again from the source \
         instead",
    )?;
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github+json",
            RELEASES_URL,
        ])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot read armor's releases ({RELEASES_URL}): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let releases = parse_json(&String::from_utf8_lossy(&output.stdout))
        .ok_or("GitHub's answer is not valid JSON")?;
    let release = newest_release(&releases, installed.kind).ok_or(match installed.kind {
        Kind::Package => "no release of armor has an arm64 package",
        Kind::Archive => "no release of armor has an arm64 archive",
    })?;
    let available = compare_versions(&release.version, &installed.version) == Ordering::Greater;
    Ok(Check {
        installed,
        release,
        available,
    })
}

/// `armor update [--check [--porcelain]]`.
pub fn main(args: &[std::ffi::OsString], data_home: &Path) -> Result<(), String> {
    let flags: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    let check_only = match flags.as_slice() {
        [] => false,
        ["--check"] | ["--check", "--porcelain"] | ["--porcelain", "--check"] => true,
        _ => return Err("usage: armor update [--check [--porcelain]]".to_string()),
    };
    let found = check()?;
    if flags.contains(&"--porcelain") {
        println!(
            "update\t{}\t{}\t{}",
            if found.available { "yes" } else { "no" },
            found.installed.version,
            found.release.version
        );
        return Ok(());
    }
    if !found.available {
        eprintln!("armor: armor {} is up to date", found.installed.version);
        return Ok(());
    }
    eprintln!(
        "armor: armor {} is available (installed: {})",
        found.release.version, found.installed.version
    );
    if check_only {
        return Ok(());
    }
    let dir = data_home.join("armor/cache/updates");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join(&found.release.file);
    let partial = dir.join(format!("{}.partial", found.release.file));
    crate::engines::download_verified(
        &found.release.url,
        &partial,
        found.release.size,
        &found.release.sha256,
    )?;
    std::fs::rename(&partial, &file).map_err(|e| format!("{}: {e}", file.display()))?;
    match &found.installed.prefix {
        Some(prefix) => install_archive(&file, &found.release.version, prefix, &dir),
        None => install(&file, &found.release.version),
    }
}

/// Installs the downloaded archive with its own `install.sh` into
/// `prefix`, where the running armor was installed, as this user.
fn install_archive(file: &Path, version: &str, prefix: &Path, dir: &Path) -> Result<(), String> {
    let unpacked = dir.join(format!("armor_{version}_arm64.unpacked"));
    if unpacked.exists() {
        std::fs::remove_dir_all(&unpacked).map_err(|e| format!("{}: {e}", unpacked.display()))?;
    }
    std::fs::create_dir_all(&unpacked).map_err(|e| format!("{}: {e}", unpacked.display()))?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(file)
        .arg("-C")
        .arg(&unpacked)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err(format!("cannot unpack {} ({status})", file.display()));
    }
    let script = unpacked.join(format!("armor_{version}_arm64/install.sh"));
    eprintln!(
        "armor: installing armor {version} into {}",
        prefix.display()
    );
    let status = Command::new("sh")
        .arg(&script)
        .env("PREFIX", prefix)
        .status()
        .map_err(|e| format!("{}: {e}", script.display()))?;
    let _ = std::fs::remove_dir_all(&unpacked);
    if !status.success() {
        return Err(format!(
            "the installation did not finish ({status}); the verified archive is {}",
            file.display()
        ));
    }
    eprintln!("armor: armor updated to {version}");
    Ok(())
}

/// Installs the downloaded package as the administrator.
fn install(file: &Path, version: &str) -> Result<(), String> {
    let pkexec = PathBuf::from("/usr/bin/pkexec");
    if !pkexec.is_file() {
        return Err(format!(
            "downloaded and verified {}; install it with `sudo apt install {}`",
            file.display(),
            file.display()
        ));
    }
    eprintln!("armor: installing armor {version} (the system asks for your password)");
    let status = Command::new(&pkexec)
        .args(["/usr/bin/apt-get", "install", "-y"])
        .arg(file)
        .status()
        .map_err(|e| format!("pkexec: {e}"))?;
    if !status.success() {
        return Err(format!(
            "the installation did not finish ({status}); the verified package is {}",
            file.display()
        ));
    }
    eprintln!("armor: armor updated to {version}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_read() {
        assert_eq!(
            parse_json(r#" {"a": [1, -2.5e1, true, null], "bè": "x\"è\n"} "#),
            Some(Json::Object(vec![
                (
                    "a".to_string(),
                    Json::Array(vec![
                        Json::Number(1.0),
                        Json::Number(-25.0),
                        Json::Bool(true),
                        Json::Null
                    ])
                ),
                ("bè".to_string(), Json::Text("x\"è\n".to_string())),
            ]))
        );
        assert_eq!(parse_json("{\"a\": 1,}"), None);
        assert_eq!(parse_json("[1 2]"), None);
        assert_eq!(parse_json("\"é\""), Some(Json::Text("é".to_string())));
    }

    #[test]
    fn the_newest_release_with_a_package_is_chosen() {
        // The shape of GitHub's list of releases (2026-10-15), shortened.
        let text = r#"[
          {"tag_name": "v0.3.0", "draft": true, "assets": []},
          {"tag_name": "v0.2.0", "draft": false, "prerelease": true, "assets": [
            {"name": "armor_0.2.0_arm64.deb", "size": 704560,
             "digest": "sha256:7D79D0DE",
             "browser_download_url": "https://github.com/FrankNat996/armor/releases/download/v0.2.0/armor_0.2.0_arm64.deb"}
          ]},
          {"tag_name": "v0.1.0", "draft": false, "assets": []}
        ]"#;
        let json = parse_json(text).unwrap();
        assert_eq!(
            newest_release(&json, Kind::Package),
            Some(Release {
                version: "0.2.0".to_string(),
                file: "armor_0.2.0_arm64.deb".to_string(),
                url: "https://github.com/FrankNat996/armor/releases/download/v0.2.0/armor_0.2.0_arm64.deb"
                    .to_string(),
                size: 704560,
                sha256: "7d79d0de".to_string(),
            })
        );
        assert_eq!(
            newest_release(&parse_json("[]").unwrap(), Kind::Package),
            None
        );
        // An archive installation looks for the archive, which v0.2.0 lacks.
        assert_eq!(newest_release(&json, Kind::Archive), None);
    }

    #[test]
    fn versions_compare_as_dpkg_does() {
        use Ordering::*;
        // Each pair checked with `dpkg --compare-versions` on Debian 13.
        let cases = [
            ("0.4.0", "0.3.0", Greater),
            ("0.4.0", "0.4.0~git20261015101027.4172c8d", Greater),
            ("0.5.0~git20261015.e8550c1", "0.4.0", Greater),
            (
                "0.4.0~git20261015050406.da651a9",
                "0.4.0~git20261015101027.4172c8d",
                Less,
            ),
            ("0.10.0", "0.9.9", Greater),
            ("1.0", "1.0", Equal),
            ("1.0-1", "1.0", Greater),
            ("1:0.1", "2.0", Greater),
            ("1.0a", "1.0", Greater),
            ("1.0~rc1", "1.0", Less),
            ("1.0+b1", "1.0a", Greater),
            ("1.01", "1.1", Equal),
        ];
        for (a, b, expected) in cases {
            assert_eq!(compare_versions(a, b), expected, "{a} vs {b}");
            assert_eq!(compare_versions(b, a), expected.reverse(), "{b} vs {a}");
        }
    }

    #[test]
    fn the_archive_installation_records_its_version() {
        assert_eq!(
            archive_version("kind=archive\nversion=0.5.0\n"),
            Some("0.5.0".to_string())
        );
        assert_eq!(archive_version("kind=archive\n"), None);
    }
}
