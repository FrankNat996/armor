//! Per-application profiles (ADR-0028): `~/.config/armor/profiles/<name>.conf`
//! (`$XDG_CONFIG_HOME/armor/profiles` if set), lines `key = value`, `#`
//! comments. `--profile NAME` picks one; otherwise the one named after the
//! program's file, if it exists. Options on the command line win over the
//! profile. An unknown key or a bad value is an error, never ignored.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The engine a profile asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Jit,
    Interpreter,
    Lockstep,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub engine: Option<Engine>,
    /// The guest root; empty for none.
    pub root: Option<String>,
    pub argv0: Option<String>,
    /// `stderr` or a file, as `--diagnose[=FILE]`.
    pub diagnose: Option<String>,
    /// Variables added to (or replaced in) the guest's environment.
    pub env: Vec<(String, String)>,
    /// Variables removed from the guest's environment.
    pub unset: Vec<String>,
    /// `--sandbox` and its paths (ADR-0027).
    pub sandbox: Option<bool>,
    pub allow: Vec<String>,
    pub allow_ro: Vec<String>,
    pub allow_net: Option<bool>,
    /// `limit-*` keys, as the `--limit-*` options (name without `--`).
    pub limits: Vec<(String, String)>,
    /// `--host-gl`: OpenGL through the host's libGL (ADR-0041).
    pub host_gl: Option<bool>,
}

fn yes_no(value: &str) -> Option<bool> {
    match value {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

/// The directory of the profiles, from the environment.
pub fn dir(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let base = match xdg_config_home.filter(|v| !v.is_empty()) {
        Some(config) => PathBuf::from(config),
        None => PathBuf::from(home.filter(|v| !v.is_empty())?).join(".config"),
    };
    Some(base.join("armor").join("profiles"))
}

/// A profile name is a file name without a directory: letters, digits and
/// `. _ + -`, not starting with a dot.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

/// The profile to use: `explicit` (`--profile`, which must exist), or the
/// one named after `program`'s file name, if there is one.
pub fn find(
    dir: Option<&Path>,
    explicit: Option<&str>,
    program: &Path,
) -> Result<Option<Profile>, String> {
    let name = match explicit {
        Some(name) => {
            if !valid_name(name) {
                return Err(format!(
                    "profile name {name:?}: only letters, digits and . _ + - (not starting with .)"
                ));
            }
            name.to_string()
        }
        None => match program.file_name().and_then(OsStr::to_str) {
            Some(name) if valid_name(name) => name.to_string(),
            _ => return Ok(None),
        },
    };
    let Some(dir) = dir else {
        return match explicit {
            Some(_) => Err("no profile directory: set HOME or XDG_CONFIG_HOME".to_string()),
            None => Ok(None),
        };
    };
    let path = dir.join(format!("{name}.conf"));
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&name, &text)
            .map(Some)
            .map_err(|e| format!("profile {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => Ok(None),
        Err(e) => Err(format!("profile {}: {e}", path.display())),
    }
}

pub fn parse(name: &str, text: &str) -> Result<Profile, String> {
    let mut profile = Profile {
        name: name.to_string(),
        ..Profile::default()
    };
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |what: String| format!("line {}: {what}", n + 1);
        let (key, value) = line
            .split_once('=')
            .map(|(k, v)| (k.trim(), v.trim()))
            .ok_or_else(|| at(format!("{line:?} is not `key = value`")))?;
        match key {
            "engine" => {
                profile.engine = Some(match value {
                    "jit" => Engine::Jit,
                    "interpreter" => Engine::Interpreter,
                    "lockstep" => Engine::Lockstep,
                    _ => {
                        return Err(at(format!(
                            "engine {value:?}: jit, interpreter or lockstep"
                        )));
                    }
                })
            }
            "root" => profile.root = Some(value.to_string()),
            "argv0" => profile.argv0 = Some(value.to_string()),
            "diagnose" => {
                if value.is_empty() {
                    return Err(at("diagnose: stderr or a file".to_string()));
                }
                profile.diagnose = Some(value.to_string())
            }
            "env" => {
                let (var, val) = value
                    .split_once('=')
                    .filter(|(var, _)| !var.is_empty())
                    .ok_or_else(|| at(format!("env {value:?}: NAME=VALUE")))?;
                profile.env.push((var.to_string(), val.to_string()));
            }
            "unset" => {
                if value.is_empty() || value.contains('=') {
                    return Err(at(format!("unset {value:?}: a variable name")));
                }
                profile.unset.push(value.to_string());
            }
            "sandbox" | "allow-net" | "host-gl" => {
                let flag =
                    yes_no(value).ok_or_else(|| at(format!("{key} {value:?}: yes or no")))?;
                match key {
                    "sandbox" => profile.sandbox = Some(flag),
                    "allow-net" => profile.allow_net = Some(flag),
                    _ => profile.host_gl = Some(flag),
                }
            }
            "limit-files" | "limit-processes" | "limit-file-size" | "limit-cpu"
            | "limit-memory" => {
                crate::limits::parse(key, value).map_err(at)?;
                profile.limits.push((key.to_string(), value.to_string()));
            }
            "allow" | "allow-ro" => {
                if !value.starts_with('/') {
                    return Err(at(format!("{key} {value:?}: an absolute path")));
                }
                if key == "allow" {
                    profile.allow.push(value.to_string());
                } else {
                    profile.allow_ro.push(value.to_string());
                }
            }
            _ => return Err(at(format!("unknown key {key:?}"))),
        }
    }
    Ok(profile)
}

/// The guest environment `env` (`NAME=VALUE` entries) after the profile's
/// `unset` and `env` lines.
pub fn apply_env(profile: &Profile, env: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let name_of =
        |entry: &[u8]| -> Vec<u8> { entry.split(|&b| b == b'=').next().unwrap_or(&[]).to_vec() };
    let removed = |entry: &Vec<u8>| {
        let name = name_of(entry);
        profile.unset.iter().any(|u| u.as_bytes() == name)
            || profile.env.iter().any(|(var, _)| var.as_bytes() == name)
    };
    let mut env: Vec<Vec<u8>> = env.into_iter().filter(|e| !removed(e)).collect();
    env.extend(
        profile
            .env
            .iter()
            .map(|(var, val)| format!("{var}={val}").into_bytes()),
    );
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_parse_every_key() {
        let profile = parse(
            "python3",
            "# python with its own root\n\
             engine = interpreter\n\
             root = /opt/x86_64-root\n\
             argv0 = python3\n\
             diagnose = /tmp/python.report\n\
             env = PYTHONDONTWRITEBYTECODE=1\n\
             env = A==b\n\
             unset = LD_PRELOAD\n\
             sandbox = yes\n\
             allow = /data\n\
             allow-ro = /opt/models\n\
             allow-net = no\n\
             host-gl = yes\n",
        )
        .unwrap();
        assert_eq!(profile.engine, Some(Engine::Interpreter));
        assert_eq!(profile.root.as_deref(), Some("/opt/x86_64-root"));
        assert_eq!(profile.argv0.as_deref(), Some("python3"));
        assert_eq!(profile.diagnose.as_deref(), Some("/tmp/python.report"));
        assert_eq!(
            profile.env,
            [
                ("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string()),
                ("A".to_string(), "=b".to_string())
            ]
        );
        assert_eq!(profile.unset, ["LD_PRELOAD"]);
        assert_eq!(profile.sandbox, Some(true));
        assert_eq!(profile.allow, ["/data"]);
        assert_eq!(profile.allow_ro, ["/opt/models"]);
        assert_eq!(profile.allow_net, Some(false));
        assert_eq!(profile.host_gl, Some(true));
    }

    #[test]
    fn mistakes_are_errors() {
        for (text, error) in [
            ("engine = fast", "line 1: engine \"fast\""),
            ("\n\nspeed = 3", "line 3: unknown key \"speed\""),
            ("just words", "not `key = value`"),
            ("env = NOVALUE", "NAME=VALUE"),
            ("unset = A=1", "a variable name"),
            ("sandbox = maybe", "yes or no"),
            ("host-gl = 1", "yes or no"),
            ("allow = relative/path", "an absolute path"),
        ] {
            let e = parse("p", text).unwrap_err();
            assert!(e.contains(error), "{text}: {e}");
        }
    }

    #[test]
    fn names_cannot_leave_the_directory() {
        let dir = Path::new("/nonexistent");
        for name in ["../x", "a/b", ".hidden", ""] {
            assert!(
                find(Some(dir), Some(name), Path::new("p")).is_err(),
                "{name}"
            );
        }
        // No profile named after the program: none, not an error.
        assert_eq!(find(Some(dir), None, Path::new("/bin/prog")), Ok(None));
        // An explicit profile must exist.
        assert!(find(Some(dir), Some("prog"), Path::new("p")).is_err());
    }

    #[test]
    fn the_environment_follows_the_profile() {
        let profile = parse("p", "unset = SECRET\nenv = LANG=C\n").unwrap();
        let env = apply_env(
            &profile,
            vec![
                b"SECRET=x".to_vec(),
                b"LANG=it".to_vec(),
                b"HOME=/h".to_vec(),
            ],
        );
        assert_eq!(env, [b"HOME=/h".to_vec(), b"LANG=C".to_vec()]);
    }

    #[test]
    fn the_directory_follows_xdg() {
        assert_eq!(
            dir(Some(OsStr::new("/c")), Some(OsStr::new("/h"))),
            Some(PathBuf::from("/c/armor/profiles"))
        );
        assert_eq!(
            dir(None, Some(OsStr::new("/h"))),
            Some(PathBuf::from("/h/.config/armor/profiles"))
        );
        assert_eq!(dir(None, None), None);
    }
}
