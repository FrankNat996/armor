//! The guest root: a directory holding x86_64 files (the dynamic loader,
//! libraries) that the guest sees at their usual absolute paths.
//!
//! Resolution follows `qemu-x86_64 -L <root>`, the difftest oracle
//! (measured with strace on the host, docs/architecture.md): an absolute
//! guest path `/p` becomes `<root>/p` if that exists, otherwise it stays
//! `/p` on the host. Relative paths are never changed. The guest therefore
//! still sees every host file the root does not override (its working
//! directory, /etc/passwd, ...). The host's kernel and runtime directories
//! (`HOST_ONLY`: /proc, /sys, /dev, /run, /tmp) are never taken from the
//! root: a package can bring an empty `/proc` or `/run`, and the guest would
//! then miss /proc/self/task (Chromium's sandbox stopped on it) or the
//! system's sockets.

use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Directories that always come from the host.
const HOST_ONLY: [&[u8]; 5] = [b"/proc", b"/sys", b"/dev", b"/run", b"/tmp"];

fn host_only(path: &[u8]) -> bool {
    HOST_ONLY.iter().any(|dir| {
        path.strip_prefix(*dir)
            .is_some_and(|rest| rest.is_empty() || rest[0] == b'/')
    })
}

/// Environment variable that names the guest root.
pub const ROOT_VARIABLE: &str = "ARMOR_ROOT";

/// The root used when `ARMOR_ROOT` is not set: the x86_64 libraries that
/// Debian's `libc6-amd64-cross` installs (ADR-0011).
pub const DEFAULT_ROOT: &str = "/usr/x86_64-linux-gnu";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestRoot {
    dir: Option<PathBuf>,
    /// With `--host-gl` (ADR-0041): armor's x86-64 `libGL.so.1` of stubs,
    /// which the guest gets in place of every `libGL.so.1` and
    /// `libOpenGL.so.0` in a library directory; the guest's own `libGL.so.1`
    /// is then `libGL.so.1.armor-guest`.
    host_gl: Option<PathBuf>,
}

/// The name under which the guest's own `libGL.so.1` is reached with
/// `--host-gl` (the stubs' fallback).
pub const GUEST_LIBGL: &[u8] = b"libGL.so.1.armor-guest";

impl GuestRoot {
    /// No root: guest paths are host paths.
    pub fn none() -> GuestRoot {
        GuestRoot {
            dir: None,
            host_gl: None,
        }
    }

    pub fn new(dir: impl Into<PathBuf>) -> GuestRoot {
        GuestRoot {
            dir: Some(dir.into()),
            host_gl: None,
        }
    }

    /// The same root, with armor's OpenGL stubs at `stubs` (`--host-gl`).
    pub fn with_host_gl(mut self, stubs: impl Into<PathBuf>) -> GuestRoot {
        self.host_gl = Some(stubs.into());
        self
    }

    /// armor's OpenGL stubs, with `--host-gl`.
    pub fn host_gl(&self) -> Option<&Path> {
        self.host_gl.as_deref()
    }

    /// `ARMOR_ROOT` if set (an empty value means no root), otherwise
    /// `DEFAULT_ROOT` if it is a directory, otherwise no root.
    pub fn from_env(value: Option<OsString>) -> GuestRoot {
        match value {
            Some(dir) if dir.is_empty() => GuestRoot::none(),
            Some(dir) => GuestRoot::new(dir),
            None if Path::new(DEFAULT_ROOT).is_dir() => GuestRoot::new(DEFAULT_ROOT),
            None => GuestRoot::none(),
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The host path for guest path `path`.
    pub fn resolve(&self, path: &[u8]) -> Vec<u8> {
        if let Some(stubs) = &self.host_gl
            && path.first() == Some(&b'/')
            && path.windows(5).any(|w| w == b"/lib/" || w == b"/lib6")
        {
            let (dir, name) = path.split_at(path.iter().rposition(|b| *b == b'/').unwrap_or(0) + 1);
            // libOpenGL.so.0 (glvnd's OpenGL without GLX, which libGLU
            // calls through) gets the same stubs.
            if name == b"libGL.so.1" || name == b"libOpenGL.so.0" {
                return stubs.as_os_str().as_bytes().to_vec();
            }
            if name == GUEST_LIBGL {
                return GuestRoot {
                    dir: self.dir.clone(),
                    host_gl: None,
                }
                .resolve(&[dir, b"libGL.so.1"].concat());
            }
        }
        if let (Some(dir), Some(b'/')) = (&self.dir, path.first())
            && !host_only(path)
        {
            let mut inside = dir.as_os_str().as_bytes().to_vec();
            inside.extend_from_slice(path);
            if exists(&inside) {
                return inside;
            }
        }
        path.to_vec()
    }

    /// `resolve` for a path read from guest memory.
    pub fn resolve_c(&self, path: CString) -> CString {
        match &self.dir {
            _ if self.host_gl.is_some() && path.as_bytes().first() == Some(&b'/') => {
                CString::new(self.resolve(path.as_bytes())).expect("no interior NUL")
            }
            Some(_) if path.as_bytes().first() == Some(&b'/') => {
                CString::new(self.resolve(path.as_bytes())).expect("no interior NUL")
            }
            _ => path,
        }
    }

    /// `resolve` for a host-side `Path` (the loader's interpreter).
    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        PathBuf::from(OsString::from_vec(
            self.resolve(path.as_os_str().as_bytes()),
        ))
    }
}

/// Whether `path` exists, following symlinks (`faccessat(F_OK)`, as qemu).
fn exists(path: &[u8]) -> bool {
    Path::new(OsStr::from_bytes(path)).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("armor-root-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/libx.so"), b"").unwrap();
        dir
    }

    #[test]
    fn files_in_the_root_override_host_files() {
        let dir = temp_root();
        let root = GuestRoot::new(&dir);
        let inside = dir.join("lib/libx.so");
        assert_eq!(root.resolve(b"/lib/libx.so"), inside.as_os_str().as_bytes());
        // Missing from the root: the host path.
        assert_eq!(root.resolve(b"/lib/liby.so"), b"/lib/liby.so");
    }

    #[test]
    fn kernel_and_runtime_directories_stay_on_the_host() {
        let dir = temp_root();
        for sub in ["proc/self", "run", "tmp", "procedures"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let root = GuestRoot::new(&dir);
        assert_eq!(root.resolve(b"/proc/"), b"/proc/");
        assert_eq!(root.resolve(b"/proc/self"), b"/proc/self");
        assert_eq!(root.resolve(b"/run"), b"/run");
        assert_eq!(root.resolve(b"/tmp"), b"/tmp");
        // Only whole path components count.
        let mut procedures = dir.as_os_str().as_bytes().to_vec();
        procedures.extend_from_slice(b"/procedures");
        assert_eq!(root.resolve(b"/procedures"), procedures);
    }

    #[test]
    fn relative_paths_and_no_root_are_unchanged() {
        let root = GuestRoot::new(temp_root());
        assert_eq!(root.resolve(b"lib/libx.so"), b"lib/libx.so");
        assert_eq!(GuestRoot::none().resolve(b"/lib/libx.so"), b"/lib/libx.so");
    }

    #[test]
    fn root_from_the_environment() {
        assert_eq!(GuestRoot::from_env(Some("".into())), GuestRoot::none());
        assert_eq!(GuestRoot::from_env(Some("/r".into())), GuestRoot::new("/r"));
    }
}
