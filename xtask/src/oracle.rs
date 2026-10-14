//! The hardware oracle (ADR-0025, docs/testing.md): a real x86-64 Linux
//! machine reached over SSH, where static test programs run natively.
//! `cargo xtask difftest --oracle` compares armor with it instead of qemu.
//!
//! The machine comes from the environment, never from the repository:
//! `ARMOR_ORACLE` is the SSH destination (`user@host`), `ARMOR_ORACLE_KEY`
//! an optional identity file. Programs and the busybox data directory are
//! copied to `REMOTE_DIR` with scp.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Output, Stdio};

use crate::exec;

const REMOTE_DIR: &str = "/tmp/armor-oracle";
/// Printed by the remote shell on stderr after the program, with its
/// status: ssh alone cannot tell an exit code from a signal.
const STATUS_MARKER: &str = "ARMOR_ORACLE_STATUS:";

pub struct Oracle {
    destination: String,
    key: Option<String>,
}

impl Oracle {
    pub fn from_env() -> Result<Oracle, String> {
        let destination = std::env::var("ARMOR_ORACLE").map_err(|_| {
            "ARMOR_ORACLE is not set: the SSH destination of the x86-64 machine \
             (docs/testing.md, hardware oracle)"
                .to_string()
        })?;
        Ok(Oracle {
            destination,
            key: std::env::var("ARMOR_ORACLE_KEY").ok(),
        })
    }

    fn options(&self) -> Vec<String> {
        let mut options: Vec<String> = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if let Some(key) = &self.key {
            options.push("-i".to_string());
            options.push(key.clone());
        }
        options
    }

    fn ssh(&self, command: &str) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.args(self.options()).arg(&self.destination).arg(command);
        cmd
    }

    fn checked(&self, mut cmd: Command, what: &str) -> Result<(), String> {
        let output = exec(&mut cmd)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{what} on the oracle failed: {}",
                String::from_utf8_lossy(&output.stderr).trim_end()
            ))
        }
    }

    /// An empty working directory with a `bin/<group>` directory for each
    /// of `names`, and the data directory of the busybox cases copied as
    /// `data` (with its permissions: `ls -l` shows them).
    pub fn prepare(&self, data_dir: &Path, names: &[String]) -> Result<(), String> {
        let mut dirs: Vec<String> = names
            .iter()
            .filter_map(|name| name.rsplit_once('/').map(|(dir, _)| quote(dir)))
            .collect();
        dirs.sort();
        dirs.dedup();
        let reset = format!(
            "rm -rf {REMOTE_DIR} && mkdir -p {REMOTE_DIR}/bin && cd {REMOTE_DIR}/bin && mkdir -p {}",
            dirs.join(" ")
        );
        self.checked(self.ssh(&reset), "creating the working directory")?;
        let mut scp = Command::new("scp");
        scp.arg("-q")
            .arg("-p")
            .arg("-r")
            .args(self.options())
            .arg(data_dir)
            .arg(format!("{}:{REMOTE_DIR}/data", self.destination));
        self.checked(scp, "copying the data directory")
    }

    /// Copies `local` into `bin/<group of name>`, keeping its file name
    /// (tests print it, and busybox picks the applet from it); returns its
    /// remote path.
    pub fn upload(&self, local: &Path, name: &str) -> Result<String, String> {
        let group = name.rsplit_once('/').map_or("", |(group, _)| group);
        let file = local
            .file_name()
            .ok_or_else(|| format!("{}: no file name", local.display()))?
            .to_string_lossy();
        let remote = format!("{REMOTE_DIR}/bin/{group}/{file}");
        let mut scp = Command::new("scp");
        scp.arg("-q")
            .args(self.options())
            .arg(local)
            .arg(format!("{}:{remote}", self.destination));
        self.checked(scp, &format!("copying {}", local.display()))?;
        Ok(remote)
    }

    /// Runs `bin` with `args`, in the data directory with `stdin` as input
    /// if `in_data`, under `timeout` (120 s, as in difftest).
    pub fn run(
        &self,
        bin: &str,
        args: &[String],
        in_data: bool,
        stdin: Option<&Path>,
    ) -> Result<Output, String> {
        let words: Vec<String> = std::iter::once(bin.to_string())
            .chain(args.iter().cloned())
            .map(|w| quote(&w))
            .collect();
        let dir = if in_data {
            format!("{REMOTE_DIR}/data")
        } else {
            REMOTE_DIR.to_string()
        };
        let command = format!(
            "cd {dir} && timeout --kill-after=5 120 {}; echo {STATUS_MARKER}$? >&2",
            words.join(" ")
        );
        let mut cmd = self.ssh(&command);
        match stdin {
            Some(file) => {
                let file = std::fs::File::open(file)
                    .map_err(|e| format!("cannot open {}: {e}", file.display()))?;
                cmd.stdin(file);
            }
            None => {
                cmd.stdin(Stdio::null());
            }
        }
        let mut output = exec(&mut cmd)?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let status = stderr
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix(STATUS_MARKER))
            .and_then(|s| s.trim().parse::<i32>().ok())
            .ok_or_else(|| format!("no status from the oracle (ssh: {})", stderr.trim_end()))?;
        output.status = shell_status(status);
        Ok(output)
    }
}

/// A shell's `$?` as a wait status: 128 + N is death by signal N (a
/// program exiting with such a code would be misread; none of the tests
/// does).
fn shell_status(status: i32) -> ExitStatus {
    if (129..=192).contains(&status) {
        ExitStatus::from_raw(status - 128)
    } else {
        ExitStatus::from_raw(status << 8)
    }
}

/// `word` quoted for the remote shell.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_statuses_map_to_exit_codes_and_signals() {
        assert_eq!(shell_status(0).code(), Some(0));
        assert_eq!(shell_status(125).code(), Some(125));
        assert_eq!(shell_status(139).signal(), Some(11));
        assert_eq!(shell_status(134).signal(), Some(6));
    }

    #[test]
    fn words_are_quoted_for_the_shell() {
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), r"'it'\''s'");
    }
}
