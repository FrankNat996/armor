//! Installed applications (ADR-0033): `armor install`, `armor run`,
//! `armor list`, `armor remove`.
//!
//! Each application lives in `$XDG_DATA_HOME/armor/apps/<name>/`
//! (`~/.local/share/...` without it), with no root privileges and nothing
//! written outside that directory except the menu entries:
//! - `app.conf`: lines `key = value`, `#` comments: `kind = windows`,
//!   `dxvk = yes|no` (DXVK installed in the prefix, `install_dxvk`),
//!   `mono = yes|no` (Wine Mono, .NET, installed in the prefix),
//!   `env = NAME=VALUE` (repeatable: added to Wine's environment, for
//!   example `env = DXVK_FILTER_DEVICE_NAME=llvmpipe` on a GPU whose
//!   Vulkan driver DXVK refuses);
//! - `prefix/`: the Wine prefix, for a Windows program, with a desktop
//!   folder of its own (`keep_desktop_inside`);
//! - `menu/`: Wine's own data directory for menu entries and icons
//!   (`XDG_DATA_HOME` and `XDG_CONFIG_HOME` point there while Wine runs,
//!   so that its shortcuts never reach the user's menu with a `wine` that
//!   is not on the `PATH`);
//! - `entries`: the menu entries armor wrote, one per line: the `.desktop`
//!   file name, the entry's name, then the arguments `armor run` passes to
//!   Wine, separated by tabs.
//!
//! armor writes one `.desktop` file per Wine shortcut (not for Wine's
//! hidden file-association entries) in
//! `$XDG_DATA_HOME/applications`, named `armor-<name>-<n>.desktop`, whose
//! `Exec` is `armor run <name> <arguments>`.
//!
//! Windows programs run under Wine for AArch64 built as ARM64X, with FEX
//! translating the x86 code (Hangover's build; `docs/tech/engines.md`):
//! `ARMOR_WINE` names its `wine`, else `$XDG_DATA_HOME/armor/engines/wine/
//! usr/bin/wine` (the `usr/` of Hangover's packages, extracted there).

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::inspect;
use crate::profile;

/// An application's `app.conf`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppConf {
    pub kind: String,
    pub dxvk: bool,
    /// Wine Mono (.NET) installed in the prefix.
    pub mono: bool,
    /// The Microsoft Visual C++ 2015-2022 runtime installed in the prefix
    /// (`run`, when a program needed it).
    pub vcredist: bool,
    pub env: Vec<(String, String)>,
}

/// The text of an `app.conf`, with a comment for each variable armor chose.
pub fn render_app_conf(conf: &AppConf, reasons: &[(String, String)]) -> String {
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    let mut text = format!(
        "kind = {}\ndxvk = {}\nmono = {}\n",
        conf.kind,
        yes_no(conf.dxvk),
        yes_no(conf.mono)
    );
    if conf.vcredist {
        text.push_str("vcredist = yes\n");
    }
    for (name, value) in &conf.env {
        if let Some((_, why)) = reasons.iter().find(|(n, _)| n == name) {
            text.push_str(&format!("# {why}\n"));
        }
        text.push_str(&format!("env = {name}={value}\n"));
    }
    text
}

/// Reads an `app.conf`; an unknown key or a bad value is an error.
pub fn parse_app_conf(text: &str) -> Result<AppConf, String> {
    let mut conf = AppConf::default();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |what: &str| format!("line {}: {what}: {line:?}", n + 1);
        let (key, value) = line
            .split_once('=')
            .map(|(k, v)| (k.trim(), v.trim()))
            .ok_or_else(|| bad("expected key = value"))?;
        match key {
            "kind" if matches!(value, "windows" | "linux" | "steam") => {
                conf.kind = value.to_string()
            }
            "dxvk" if value == "yes" || value == "no" => conf.dxvk = value == "yes",
            "mono" if value == "yes" || value == "no" => conf.mono = value == "yes",
            "vcredist" if value == "yes" || value == "no" => conf.vcredist = value == "yes",
            "env" => {
                let (name, val) = value
                    .split_once('=')
                    .filter(|(name, _)| !name.is_empty())
                    .ok_or_else(|| bad("expected env = NAME=VALUE"))?;
                conf.env.push((name.to_string(), val.to_string()));
            }
            _ => return Err(bad("unknown key or value")),
        }
    }
    Ok(conf)
}

/// The DLLs DXVK replaces (Direct3D 8 to 11 and DXGI).
const DXVK_DLLS: &str = "d3d8,d3d9,d3d10core,d3d11,dxgi";

/// Names `wine` (Wine for AArch64 with an x86 translator).
pub const WINE_VARIABLE: &str = "ARMOR_WINE";

/// Exit status of these commands when they fail (as armor's own errors).
const EXIT_ERROR: u8 = 125;

/// What `armor install` was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installer {
    /// A Windows executable (an installer, or the program itself).
    WindowsExe,
    /// A Windows Installer package, run with `msiexec /i`.
    WindowsMsi,
    /// A Debian package (`debs.rs`).
    LinuxDeb,
    /// An AppImage of type 2: an x86 ELF runtime followed by a squashfs.
    AppImage,
    /// A tar archive, possibly compressed (gzip, xz, bzip2, zstd).
    Archive,
}

/// The first bytes of an OLE compound file, the container of `.msi`.
const COMPOUND_FILE_MAGIC: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];

/// What kind of installer `file` is, or why armor cannot install it yet.
pub fn installer_kind(file: &[u8]) -> Result<Installer, String> {
    if inspect::pe_machine(file).is_some() {
        return Ok(Installer::WindowsExe);
    }
    if file.starts_with(&COMPOUND_FILE_MAGIC) {
        return Ok(Installer::WindowsMsi);
    }
    if file.starts_with(b"!<arch>\ndebian-binary") {
        return Ok(Installer::LinuxDeb);
    }
    if file.starts_with(b"\x7fELF") {
        // AppImage type 2 marks its runtime with "AI" 2 in e_ident's padding.
        if file.get(8..11) == Some(b"AI\x02") {
            return Ok(Installer::AppImage);
        }
        return Err(
            "a Linux program: run it with `armor <program>` (`armor install` takes \
             installers, packages and AppImages)"
                .to_string(),
        );
    }
    let compressed = [
        &[0x1f, 0x8b][..],
        &[0xfd, b'7', b'z', b'X', b'Z', 0],
        b"BZh",
        &[0x28, 0xb5, 0x2f, 0xfd],
    ];
    if compressed.iter().any(|magic| file.starts_with(magic))
        || file.get(257..262) == Some(b"ustar")
    {
        return Ok(Installer::Archive);
    }
    Err("not an installer armor knows (.exe, .msi, .deb, AppImage, .tar archive)".to_string())
}

/// The application name from an installer's file name: its stem (without
/// `.tar` either, for `x.tar.gz`), with every character a name may not hold
/// replaced by `-`.
pub fn default_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    sanitize_name(stem.strip_suffix(".tar").unwrap_or(&stem))
}

/// The name of an application from the shortcuts its installer made, as
/// paths under Wine's `applications/wine/Programs`, leaving out the
/// uninstallers: the Start Menu folder that directly holds most of them
/// (`7-Zip/7-Zip File Manager.desktop` → `7-Zip`; `hicloud/Ezviz
/// Studio/Ezviz Studio.desktop` → `Ezviz-Studio`, not the publisher's
/// folder), else the first shortcut's own name (`Epic Games
/// Launcher.desktop` → `Epic-Games-Launcher`).
pub fn name_from_shortcuts(shortcuts: &[PathBuf]) -> Option<String> {
    let programs: Vec<&PathBuf> = shortcuts
        .iter()
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| is_uninstaller(&n.to_string_lossy()))
        })
        .collect();
    let shortcuts: Vec<&PathBuf> = if programs.is_empty() {
        shortcuts.iter().collect()
    } else {
        programs
    };
    let mut folders: Vec<(String, usize)> = Vec::new();
    for shortcut in &shortcuts {
        let Some(parent) = shortcut.parent().and_then(Path::file_name) else {
            continue;
        };
        let folder = parent.to_string_lossy().into_owned();
        match folders.iter_mut().find(|(f, _)| *f == folder) {
            Some((_, count)) => *count += 1,
            None => folders.push((folder, 1)),
        }
    }
    let chosen = match folders.iter().max_by_key(|(_, count)| *count) {
        Some((folder, _)) => folder.clone(),
        None => shortcuts
            .first()?
            .file_stem()?
            .to_string_lossy()
            .into_owned(),
    };
    let name = sanitize_name(chosen.trim());
    profile::valid_name(&name).then_some(name)
}

/// `text` with every character a name may not hold replaced by `-`, and
/// no leading dot.
fn sanitize_name(text: &str) -> String {
    let name: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._+-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    name.trim_start_matches('.').to_string()
}

/// `$XDG_DATA_HOME`, else `$HOME/.local/share`.
pub fn data_home(xdg_data_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    match xdg_data_home.filter(|v| !v.is_empty()) {
        Some(data) => Some(PathBuf::from(data)),
        None => Some(
            PathBuf::from(home.filter(|v| !v.is_empty())?)
                .join(".local")
                .join("share"),
        ),
    }
}

/// Quotes one argument of a desktop entry's `Exec` key (Desktop Entry
/// Specification, "The Exec key"): in double quotes when it holds a
/// reserved character, with `"`, `` ` ``, `$` and `\` escaped; `%` doubled
/// (a field code otherwise).
fn quote_exec_arg(arg: &str) -> String {
    if is_field_code(arg) {
        return arg.to_string();
    }
    let reserved = |c: char| " \t\n\"'\\><~|&;$*?#()`".contains(c);
    let arg = arg.replace('%', "%%");
    if arg.is_empty() || arg.chars().any(reserved) {
        let mut quoted = String::from("\"");
        for c in arg.chars() {
            if "\"`$\\".contains(c) {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted.push('"');
        quoted
    } else {
        arg
    }
}

/// A desktop entry field code standing alone (`%f`, `%U`, ...), which the
/// desktop replaces with files or URLs when it starts the entry.
pub fn is_field_code(arg: &str) -> bool {
    matches!(arg, "%f" | "%F" | "%u" | "%U" | "%i" | "%c" | "%k")
}

/// The value of an `Exec` key for `args`: each argument quoted, then the
/// string escapes of the file format (`\` written as `\\`).
pub fn encode_exec(args: &[String]) -> String {
    let line: Vec<String> = args.iter().map(|a| quote_exec_arg(a)).collect();
    line.join(" ").replace('\\', "\\\\")
}

/// The arguments of an `Exec` value: the string escapes undone (`\\`,
/// `\s`, `\n`, `\t`, `\r`), then split at spaces outside double quotes,
/// with `\` escaping the next character inside them. `%%` becomes `%`;
/// other field codes are kept as they are.
pub fn decode_exec(value: &str) -> Result<Vec<String>, String> {
    let mut text = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => text.push('\\'),
                Some('s') => text.push(' '),
                Some('n') => text.push('\n'),
                Some('t') => text.push('\t'),
                Some('r') => text.push('\r'),
                Some(other) => {
                    text.push('\\');
                    text.push(other);
                }
                None => return Err("Exec ends with a lone backslash".to_string()),
            }
        } else {
            text.push(c);
        }
    }
    let mut args = Vec::new();
    let mut current: Option<String> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if let Some(arg) = current.take() {
                    args.push(arg);
                }
            }
            '"' => {
                let arg = current.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped) => arg.push(escaped),
                            None => return Err("Exec: unfinished escape".to_string()),
                        },
                        Some(other) => arg.push(other),
                        None => return Err("Exec: unclosed quote".to_string()),
                    }
                }
            }
            other => current.get_or_insert_with(String::new).push(other),
        }
    }
    if let Some(arg) = current {
        args.push(arg);
    }
    Ok(args.into_iter().map(|a| a.replace("%%", "%")).collect())
}

/// The `[Desktop Entry]` keys of a `.desktop` file.
pub fn desktop_keys(text: &str) -> Vec<(String, String)> {
    let mut keys = Vec::new();
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if let Some((key, value)) = line.split_once('=').filter(|_| in_entry) {
            keys.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    keys
}

fn key<'a>(keys: &'a [(String, String)], name: &str) -> Option<&'a str> {
    keys.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The arguments Wine's own entry passes to `wine`: Wine writes
/// `env "WINEPREFIX=..." wine <arguments>`.
pub fn wine_arguments(exec: &[String]) -> Option<Vec<String>> {
    let wine = exec.iter().position(|a| a == "wine")?;
    Some(exec[wine + 1..].to_vec())
}

/// A menu entry armor writes for a shortcut of application `app`.
pub struct Entry {
    pub name: String,
    pub arguments: Vec<String>,
    pub icon: Option<String>,
    pub wm_class: Option<String>,
}

/// The `.desktop` file for `entry`: it runs `armor run <app> <arguments>`.
pub fn desktop_file(armor: &str, app: &str, kind: &str, entry: &Entry) -> String {
    let mut exec = vec![armor.to_string(), "run".to_string(), app.to_string()];
    exec.extend(entry.arguments.iter().cloned());
    let comment = match kind {
        "linux" => "x86 Linux program run by armor",
        "steam" => "Valve's ARM64 Steam client, installed by armor",
        _ => "Windows program run by armor (Wine for AArch64 and FEX)",
    };
    let mut text = format!(
        "[Desktop Entry]\nType=Application\nName={}\nComment={comment}\nExec={}\n",
        entry.name,
        encode_exec(&exec)
    );
    if let Some(icon) = &entry.icon {
        text.push_str(&format!("Icon={icon}\n"));
    }
    if let Some(class) = &entry.wm_class {
        text.push_str(&format!("StartupWMClass={class}\n"));
    }
    text.push_str(&format!("StartupNotify=true\nX-Armor-App={app}\n"));
    text
}

/// One line of the `entries` file.
fn entry_line(file: &str, entry: &Entry) -> String {
    let mut fields = vec![file.to_string(), entry.name.clone()];
    fields.extend(entry.arguments.iter().cloned());
    fields.join("\t")
}

/// The lines of the `entries` file: (file, name, arguments).
pub fn parse_entries(text: &str) -> Vec<(String, String, Vec<String>)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let file = fields.next()?.to_string();
            let name = fields.next()?.to_string();
            Some((file, name, fields.map(str::to_string).collect()))
        })
        .collect()
}

/// The largest PNG of icon `name` under `icons/hicolor/<size>x<size>/apps`.
fn best_icon(icons: &Path, name: &str) -> Option<PathBuf> {
    let mut best: Option<(u32, PathBuf)> = None;
    for size in fs::read_dir(icons.join("hicolor")).ok()?.flatten() {
        let dir = size.file_name().to_string_lossy().into_owned();
        let Some(pixels) = dir.split('x').next().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let png = size.path().join("apps").join(format!("{name}.png"));
        if png.is_file() && best.as_ref().is_none_or(|(b, _)| pixels > *b) {
            best = Some((pixels, png));
        }
    }
    best.map(|(_, path)| path)
}

/// Every `.desktop` file under `dir`, sorted.
fn desktop_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for item in read.flatten() {
            let path = item.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "desktop") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Where things are, from the environment.
struct Places {
    data_home: PathBuf,
    wine: PathBuf,
}

impl Places {
    fn from_env() -> Result<Places, String> {
        let data_home = data_home(
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
        .ok_or("no data directory: set HOME or XDG_DATA_HOME")?;
        let wine = match std::env::var_os(WINE_VARIABLE).filter(|v| !v.is_empty()) {
            Some(wine) => PathBuf::from(wine),
            None => data_home.join("armor/engines/wine/usr/bin/wine"),
        };
        Ok(Places { data_home, wine })
    }

    fn apps(&self) -> PathBuf {
        self.data_home.join("armor").join("apps")
    }

    fn app(&self, name: &str) -> PathBuf {
        self.apps().join(name)
    }

    fn applications(&self) -> PathBuf {
        self.data_home.join("applications")
    }

    fn check_wine(&self) -> Result<(), String> {
        if self.wine.is_file() {
            Ok(())
        } else {
            Err(format!(
                "no Wine for AArch64 at {}: `armor engine fetch` downloads it (Hangover), or set \
                 {WINE_VARIABLE} (docs/tech/engines.md)",
                self.wine.display()
            ))
        }
    }

    /// DXVK's release tree, if extracted there.
    fn dxvk(&self) -> PathBuf {
        self.data_home.join("armor/engines/dxvk")
    }

    /// Wine Mono's installer, fetched with the engine.
    fn mono(&self) -> PathBuf {
        self.data_home
            .join("armor/engines")
            .join(crate::engines::WINE_MONO_FILE)
    }

    /// `wineserver`, next to `wine`.
    fn wineserver(&self) -> PathBuf {
        self.wine.with_file_name("wineserver")
    }
}

/// A Wine command for application directory `app`: its prefix, its menu
/// directory, FEX for 32-bit code (`HODLL`, whose default in Hangover is
/// box64), and no Wine Mono or Gecko dialogs. Variables already set in
/// armor's environment win.
fn wine_command(program: &Path, app: &Path, conf: &AppConf) -> Command {
    let mut command = Command::new(program);
    let menu = app.join("menu");
    command
        .env("WINEPREFIX", app.join("prefix"))
        .env("XDG_DATA_HOME", &menu)
        .env("XDG_CONFIG_HOME", menu.join("config"));
    // Without Wine Mono, Wine would ask to download it (mscoree); Gecko
    // (mshtml) is never installed.
    let disabled = if conf.mono {
        "mshtml="
    } else {
        "mscoree,mshtml="
    };
    let overrides = if conf.dxvk {
        format!("{disabled};{DXVK_DLLS}=n,b")
    } else {
        disabled.to_string()
    };
    let mut defaults = vec![
        ("HODLL".to_string(), "libwow64fex.dll".to_string()),
        ("WINEDLLOVERRIDES".to_string(), overrides),
        // Only the loader's errors: the missing DLLs `armor run` explains.
        ("WINEDEBUG".to_string(), "-all,err+module".to_string()),
        // Wine writes menu entries only under a desktop environment.
        ("XDG_CURRENT_DESKTOP".to_string(), "X-Generic".to_string()),
    ];
    // The application's own variables replace armor's defaults.
    for (name, value) in &conf.env {
        defaults.retain(|(n, _)| n != name);
        defaults.push((name.clone(), value.clone()));
    }
    for (name, value) in defaults {
        if std::env::var_os(&name).is_none_or(|v| v.is_empty()) {
            command.env(name, value);
        }
    }
    command
}

/// Kernel GPU drivers whose Vulkan driver DXVK refuses, so that Direct3D
/// needs the software renderer (llvmpipe), with the reason. Measured, one
/// by one.
const SOFTWARE_VULKAN_DRIVERS: &[(&str, &str)] = &[(
    "v3d",
    "the V3DV Vulkan driver lacks multiDrawIndirect, which DXVK 2.x needs (measured on a \
     Raspberry Pi 5, 2026-10-14, Mesa 26.2.2)",
)];

/// The DXVK settings for a host whose GPUs use the kernel drivers
/// `drivers` (from `/sys/class/drm/renderD*`): the software renderer when
/// every GPU is one DXVK refuses (or there is none), with the reason.
pub fn dxvk_settings(drivers: &[String]) -> Vec<(String, String, String)> {
    let refused = |d: &String| SOFTWARE_VULKAN_DRIVERS.iter().find(|(k, _)| k == d);
    if drivers.is_empty() {
        return vec![(
            "DXVK_FILTER_DEVICE_NAME".to_string(),
            "llvmpipe".to_string(),
            "no GPU render node: Direct3D through the software renderer".to_string(),
        )];
    }
    if drivers.iter().all(|d| refused(d).is_some()) {
        let why = drivers
            .iter()
            .filter_map(|d| refused(d).map(|(_, why)| format!("{d}: {why}")))
            .collect::<Vec<_>>()
            .join("; ");
        return vec![(
            "DXVK_FILTER_DEVICE_NAME".to_string(),
            "llvmpipe".to_string(),
            why,
        )];
    }
    Vec::new()
}

/// The kernel drivers of the host's GPU render nodes.
fn gpu_drivers() -> Vec<String> {
    let mut drivers = Vec::new();
    let Ok(read) = fs::read_dir("/sys/class/drm") else {
        return drivers;
    };
    for node in read.flatten() {
        if !node.file_name().to_string_lossy().starts_with("renderD") {
            continue;
        }
        if let Ok(target) = fs::read_link(node.path().join("device/driver"))
            && let Some(name) = target.file_name()
        {
            let name = name.to_string_lossy().into_owned();
            if !drivers.contains(&name) {
                drivers.push(name);
            }
        }
    }
    drivers.sort();
    drivers
}

/// Copies DXVK (`dxvk`: its release tree, with `x64/` and `x32/`) into
/// the prefix: the x86-64 DLLs into `system32`, the 32-bit ones into
/// `syswow64`. Wine's own Direct3D (wined3d) runs on OpenGL, which on some
/// GPUs (the Raspberry Pi 5's) lacks what Direct3D 11 programs need. The
/// ARM64EC build would run without translation, but with it the Epic Games
/// Launcher 20.3.5 jumped from x86-64 code straight into ARM64EC code of
/// `dxgi.dll` and stopped with EXCEPTION_ILLEGAL_INSTRUCTION (2026-10-14,
/// Hangover 11.16, DXVK 2.7.1); the x86-64 build, translated by FEX, does
/// not.
fn install_dxvk(dxvk: &Path, prefix: &Path) -> Result<bool, String> {
    if !dxvk.join("x64").is_dir() {
        return Ok(false);
    }
    let windows = prefix.join("drive_c").join("windows");
    for (from, to) in [("x64", "system32"), ("x32", "syswow64")] {
        for dll in DXVK_DLLS.split(',') {
            let source = dxvk.join(from).join(format!("{dll}.dll"));
            if source.is_file() && windows.join(to).is_dir() {
                let target = windows.join(to).join(format!("{dll}.dll"));
                fs::copy(&source, &target).map_err(|e| format!("{}: {e}", target.display()))?;
            }
        }
    }
    Ok(true)
}

/// An installed application's `app.conf`.
fn read_conf(app: &Path) -> Result<AppConf, String> {
    let path = app.join("app.conf");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_app_conf(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn status_text(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit status {code}"),
        None => format!("{status}"),
    }
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("armor: {message}");
    ExitCode::from(EXIT_ERROR)
}

/// `armor install steam` (`steam.rs`, ADR-0038): the client into
/// `<app>/Steam`, its `HOME` in `<app>/home`, a menu entry; nothing kept if
/// it fails.
fn install_steam(name: String) -> Result<(), String> {
    let places = Places::from_env()?;
    let app = places.app(&name);
    if app.exists() {
        return Err(format!(
            "{name} is already installed ({}): it updates itself; `armor remove {name}` \
             first to install it again",
            app.display()
        ));
    }
    crate::steam::check_host()?;
    fs::create_dir_all(app.join("home")).map_err(|e| format!("{}: {e}", app.display()))?;
    crate::steam::link_home(&app.join("home"), &app.join("Steam"))?;
    let result = crate::steam::fetch_client(
        &app.join("Steam"),
        &places.data_home.join("armor/cache/steam"),
    )
    .and_then(|()| {
        let conf = AppConf {
            kind: "steam".to_string(),
            ..AppConf::default()
        };
        fs::write(app.join("app.conf"), render_app_conf(&conf, &[]))
            .map_err(|e| format!("{}: {e}", app.display()))?;
        write_entry_files(
            &places,
            &name,
            &app,
            "steam",
            vec![Entry {
                name: "Steam".to_string(),
                arguments: Vec::new(),
                icon: Some("steam".to_string()),
                wm_class: Some("steam".to_string()),
            }],
        )
    });
    match result {
        Ok(_) => {
            eprintln!(
                "armor: {name} installed; in the menu:\n  Steam\narmor: on its first start the \
                 client downloads its update (about 670 MB), then asks you to sign in"
            );
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&app);
            Err(format!("{e}; nothing was kept"))
        }
    }
}

/// How long `armor install` waits, after the installer ends, for the
/// processes it left in the prefix.
const INSTALLER_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// Waits until the prefix's wineserver ends (every Wine process in it
/// ended), at most `grace`; if programs are still running then, says
/// which and leaves them running.
fn wait_for_prefix(places: &Places, app: &Path, conf: &AppConf, grace: std::time::Duration) {
    let Ok(mut waiter) = wine_command(&places.wineserver(), app, conf)
        .arg("-w")
        .spawn()
    else {
        return;
    };
    let start = std::time::Instant::now();
    while start.elapsed() < grace {
        if !matches!(waiter.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    // Only the waiter: the programs and their wineserver go on.
    let _ = waiter.kill();
    let _ = waiter.wait();
    let programs = prefix_programs(&app.join("prefix"));
    eprintln!(
        "armor: the installer ended, but it left programs running in the prefix{}; going on \
         without them",
        if programs.is_empty() {
            String::new()
        } else {
            format!(" ({})", programs.join(", "))
        }
    );
}

/// The Windows programs running in `prefix` (its Wine processes, read
/// from `/proc`), other than Wine's own (`C:\windows\...`).
fn prefix_programs(prefix: &Path) -> Vec<String> {
    let wanted = [b"WINEPREFIX=".as_slice(), prefix.as_os_str().as_bytes()].concat();
    let mut programs = Vec::new();
    let Ok(processes) = fs::read_dir("/proc") else {
        return programs;
    };
    for process in processes.flatten() {
        let dir = process.path();
        let Ok(environ) = fs::read(dir.join("environ")) else {
            continue;
        };
        if !environ.split(|b| *b == 0).any(|v| v == wanted.as_slice()) {
            continue;
        }
        let Ok(cmdline) = fs::read(dir.join("cmdline")) else {
            continue;
        };
        let first = cmdline.split(|b| *b == 0).next().unwrap_or_default();
        let program = String::from_utf8_lossy(first).trim().to_string();
        if program.len() > 3
            && program.as_bytes()[1] == b':'
            && !program.to_ascii_lowercase().starts_with("c:\\windows\\")
            && !programs.contains(&program)
        {
            programs.push(program);
        }
    }
    programs.sort();
    programs
}

/// `armor install [--name NAME] <file> [-- installer arguments]`.
fn install(args: &[OsString]) -> Result<(), String> {
    let usage = "usage: armor install [--name NAME] [--pause] <file.exe|file.msi|file.deb|\
                 file.AppImage|file.tar.*|steam> [-- installer arguments]";
    let (mut name, mut rest) = (None, args);
    if let [flag, value, tail @ ..] = rest
        && flag == "--name"
    {
        name = Some(value.to_string_lossy().into_owned());
        rest = tail;
    }
    let explicit_name = name.is_some();
    let (file, extra) = match rest {
        [file] => (file, &[][..]),
        [file, separator, extra @ ..] if separator == "--" => (file, extra),
        _ => return Err(usage.to_string()),
    };
    // `armor install steam`: Valve's ARM64 client (a file of that name in
    // the current directory wins).
    if file == "steam" && !Path::new(file).exists() {
        return install_steam(name.unwrap_or_else(|| "Steam".to_string()));
    }
    let file = fs::canonicalize(file).map_err(|e| format!("{}: {e}", file.display()))?;
    // The first 64 KiB are enough to tell the kinds apart.
    let mut bytes = Vec::new();
    {
        use std::io::Read;
        fs::File::open(&file)
            .and_then(|f| f.take(64 * 1024).read_to_end(&mut bytes))
            .map_err(|e| format!("{}: {e}", file.display()))?;
    }
    let kind = installer_kind(&bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut name = name.unwrap_or_else(|| default_name(&file));
    if !profile::valid_name(&name) {
        return Err(format!(
            "application name {name:?}: only letters, digits and . _ + - (not starting with .); \
             choose one with --name"
        ));
    }
    let places = Places::from_env()?;
    if kind == Installer::LinuxDeb {
        return install_deb(&places, &file, name, explicit_name);
    }
    if kind == Installer::AppImage {
        return install_appimage(&places, &file, name, explicit_name);
    }
    if kind == Installer::Archive {
        return install_archive(&places, &file, name, explicit_name);
    }
    places.check_wine()?;
    let mut app = places.app(&name);
    if app.exists() {
        return Err(format!(
            "{name} is already installed ({}): `armor remove {name}` first",
            app.display()
        ));
    }
    fs::create_dir_all(app.join("menu").join("config"))
        .map_err(|e| format!("{}: {e}", app.display()))?;
    let mut conf = AppConf {
        kind: "windows".to_string(),
        ..AppConf::default()
    };
    fs::write(app.join("app.conf"), "kind = windows\n")
        .map_err(|e| format!("{}: {e}", app.display()))?;

    eprintln!("armor: preparing the Windows prefix of {name}");
    let status = wine_command(&places.wine, &app, &conf)
        .args(["wineboot", "-i"])
        .status()
        .map_err(|e| format!("{}: {e}", places.wine.display()))?;
    if !status.success() {
        return Err(format!("wineboot: {}", status_text(status)));
    }
    keep_desktop_inside(&app.join("prefix"))?;
    let mut reasons = Vec::new();
    if install_dxvk(&places.dxvk(), &app.join("prefix"))? {
        conf.dxvk = true;
        for (name, value, why) in dxvk_settings(&gpu_drivers()) {
            eprintln!("armor: {name}={value}: {why}");
            conf.env.push((name.clone(), value));
            reasons.push((name, why));
        }
    } else {
        eprintln!(
            "armor: no DXVK in {}: Direct3D runs on Wine's OpenGL backend",
            places.dxvk().display()
        );
    }
    let mono = places.mono();
    if mono.is_file() {
        // As Wine itself does when it creates a prefix: many installers and
        // programs need .NET (Epic Online Services' installer did).
        eprintln!("armor: installing Wine Mono (.NET) in the prefix");
        let status = wine_command(&places.wine, &app, &conf)
            .arg("msiexec")
            .arg("/i")
            .arg(&mono)
            .arg("/qn")
            .status()
            .map_err(|e| format!("{}: {e}", places.wine.display()))?;
        conf.mono = status.success();
        if !conf.mono {
            eprintln!(
                "armor: Wine Mono: {}; going on without .NET",
                status_text(status)
            );
        }
    } else {
        eprintln!(
            "armor: no Wine Mono at {} (`armor engine fetch` gets it): programs that need .NET \
             will not run",
            mono.display()
        );
    }
    fs::write(app.join("app.conf"), render_app_conf(&conf, &reasons))
        .map_err(|e| format!("{}: {e}", app.display()))?;
    eprintln!("armor: running {}", file.display());
    let mut installer = wine_command(&places.wine, &app, &conf);
    if kind == Installer::WindowsMsi {
        installer.args(["msiexec", "/i"]);
    }
    let status = installer
        .arg(&file)
        .args(extra)
        .status()
        .map_err(|e| format!("{}: {e}", places.wine.display()))?;
    // The installer may leave processes behind: msiexec and Wine's menu
    // builder finishing their work (wait for them), or programs that keep
    // running, such as an updater or the program itself started at the end
    // (do not wait for those forever).
    wait_for_prefix(&places, &app, &conf, INSTALLER_GRACE);
    let failed = !status.success();
    if !explicit_name {
        let programs = app.join("menu/applications/wine/Programs");
        let shortcuts: Vec<PathBuf> = desktop_files(&programs)
            .into_iter()
            .filter_map(|p| p.strip_prefix(&programs).ok().map(Path::to_path_buf))
            .collect();
        if let Some(better) = name_from_shortcuts(&shortcuts)
            && better != name
            && !places.app(&better).exists()
        {
            let renamed = places.app(&better);
            fs::rename(&app, &renamed).map_err(|e| format!("{}: {e}", renamed.display()))?;
            eprintln!("armor: named {better} after its menu entries (`--name` chooses another)");
            name = better;
            app = renamed;
        }
    }
    let entries = write_entries(&places, &name, &app)?;
    if failed && entries.is_empty() {
        // Cancelled, or failed before installing anything that shows: keep
        // nothing behind.
        let _ = wine_command(&places.wineserver(), &app, &conf)
            .arg("-k")
            .status();
        fs::remove_dir_all(&app).map_err(|e| format!("{}: {e}", app.display()))?;
        return Err(format!(
            "the installer ended with {} and created no menu entry (cancelled?): nothing was \
             kept; run `armor install` again to retry",
            status_text(status)
        ));
    }
    if failed {
        eprintln!(
            "armor: the installer ended with {}; keeping what it installed",
            status_text(status)
        );
    }
    if entries.is_empty() {
        eprintln!(
            "armor: {name} installed, but its installer created no menu shortcut; run a \
             program with `armor run {name} 'C:\\path\\to\\program.exe'`"
        );
    } else {
        eprintln!("armor: {name} installed; in the menu:");
        for entry in &entries {
            eprintln!("  {entry}");
        }
    }
    Ok(())
}

/// Wine links each user's `Desktop` in the prefix to the real desktop
/// folder (`~/Desktop`), so installers put their shortcuts there, with an
/// `Exec` that needs `wine` on the `PATH`. The link becomes a directory of
/// the prefix; the other folders (`Documents`, `Downloads`, ...) stay
/// linked, so that programs open the user's files.
fn keep_desktop_inside(prefix: &Path) -> Result<(), String> {
    let users = prefix.join("drive_c").join("users");
    let Ok(read) = fs::read_dir(&users) else {
        return Ok(());
    };
    for user in read.flatten() {
        let desktop = user.path().join("Desktop");
        if desktop.is_symlink() {
            fs::remove_file(&desktop)
                .and_then(|()| fs::create_dir(&desktop))
                .map_err(|e| format!("{}: {e}", desktop.display()))?;
        }
    }
    Ok(())
}

/// Writes a menu entry for every shortcut Wine made in the application's
/// menu directory; returns their names.
fn write_entries(places: &Places, name: &str, app: &Path) -> Result<Vec<String>, String> {
    write_entry_files(places, name, app, "windows", wine_entries(app))
}

/// The menu entries for the shortcuts Wine made in the application's menu
/// directory.
/// A shortcut to an uninstaller, by its name (`Uninstall 7-Zip`).
pub fn is_uninstaller(name: &str) -> bool {
    name.trim().to_lowercase().starts_with("uninstall")
}

fn wine_entries(app: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    for source in desktop_files(&app.join("menu").join("applications")) {
        let Ok(text) = fs::read_to_string(&source) else {
            continue;
        };
        let keys = desktop_keys(&text);
        // Wine's hidden entries are file associations (`%f` in Exec), not
        // shortcuts: not menu entries (associations are a later step).
        if key(&keys, "NoDisplay") == Some("true") {
            continue;
        }
        let (Some(entry_name), Some(exec)) = (key(&keys, "Name"), key(&keys, "Exec")) else {
            continue;
        };
        // Uninstallers stay out of the menu: `armor remove` (or Remove in
        // the window) removes the whole application.
        if is_uninstaller(entry_name) {
            continue;
        }
        let Some(arguments) = decode_exec(exec).ok().and_then(|e| wine_arguments(&e)) else {
            eprintln!(
                "armor: skipping {}: unexpected Exec {exec:?}",
                source.display()
            );
            continue;
        };
        let icon = key(&keys, "Icon").map(|icon| {
            best_icon(&app.join("menu").join("icons"), icon)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| icon.to_string())
        });
        entries.push(Entry {
            name: entry_name.to_string(),
            arguments,
            icon,
            wm_class: key(&keys, "StartupWMClass").map(str::to_string),
        });
    }
    entries
}

/// Writes `entries` as armor's menu entries for application `name` and
/// records them in its `entries` file; returns their names.
fn write_entry_files(
    places: &Places,
    name: &str,
    app: &Path,
    kind: &str,
    entries: Vec<Entry>,
) -> Result<Vec<String>, String> {
    let armor = std::env::current_exe()
        .map_err(|e| format!("armor's own path: {e}"))?
        .to_string_lossy()
        .into_owned();
    let applications = places.applications();
    fs::create_dir_all(&applications).map_err(|e| format!("{}: {e}", applications.display()))?;
    let mut lines = Vec::new();
    let mut names = Vec::new();
    for entry in entries {
        let file = format!("armor-{name}-{}.desktop", lines.len() + 1);
        let path = applications.join(&file);
        fs::write(&path, desktop_file(&armor, name, kind, &entry))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        lines.push(entry_line(&file, &entry));
        names.push(entry.name);
    }
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    fs::write(app.join("entries"), text).map_err(|e| format!("{}: {e}", app.display()))?;
    Ok(names)
}

/// Follows `guest` (an absolute guest path) inside `root`: symbolic links
/// with absolute targets point into the root, not into the host.
pub fn resolve_in_root(root: &Path, guest: &str) -> PathBuf {
    let mut current = PathBuf::from(guest);
    for _ in 0..40 {
        let host = root.join(current.strip_prefix("/").unwrap_or(&current));
        match fs::read_link(&host) {
            Ok(target) if target.is_absolute() => current = target,
            Ok(target) => {
                let parent = current.parent().map(Path::to_path_buf).unwrap_or_default();
                current = parent.join(target);
            }
            Err(_) => return host,
        }
    }
    root.join(current.strip_prefix("/").unwrap_or(&current))
}

/// The guest path of `program` from a desktop entry's `Exec`: absolute as
/// it is, else looked up in the root's usual program directories.
fn guest_program(root: &Path, program: &str) -> Option<String> {
    if program.starts_with('/') {
        return Some(program.to_string());
    }
    [
        "/usr/bin",
        "/bin",
        "/usr/games",
        "/usr/local/bin",
        "/usr/sbin",
    ]
    .iter()
    .map(|dir| format!("{dir}/{program}"))
    .find(|guest| resolve_in_root(root, guest).is_file())
}

/// The best icon for `icon` (a name or a guest path) inside `root`; else
/// the largest PNG in an `icons` directory next to the `.desktop` file
/// (`desktop_dir`), where packages that register their icons from a
/// maintainer script keep them.
fn root_icon(root: &Path, icon: &str, desktop_dir: &Path) -> String {
    if icon.starts_with('/') {
        return resolve_in_root(root, icon).to_string_lossy().into_owned();
    }
    let share = root.join("usr/share");
    if let Some(png) = best_icon(&share.join("icons"), icon) {
        return png.to_string_lossy().into_owned();
    }
    for candidate in [
        share.join(format!("icons/hicolor/scalable/apps/{icon}.svg")),
        share.join(format!("pixmaps/{icon}.png")),
        share.join(format!("pixmaps/{icon}.svg")),
        share.join(format!("pixmaps/{icon}.xpm")),
    ] {
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    let largest = fs::read_dir(desktop_dir.join("icons"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "png"))
        .max_by_key(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0));
    match largest {
        Some(png) => png.to_string_lossy().into_owned(),
        None => icon.to_string(),
    }
}

/// The menu entries of a Linux application: the visible `.desktop` files
/// in `files` (guest paths), with `Exec` resolved inside `root`.
fn linux_entries(root: &Path, files: &[String]) -> Vec<Entry> {
    let mut entries = Vec::new();
    for guest in files {
        let Ok(text) = fs::read_to_string(resolve_in_root(root, guest)) else {
            continue;
        };
        let keys = desktop_keys(&text);
        if key(&keys, "NoDisplay") == Some("true") || key(&keys, "Type") != Some("Application") {
            continue;
        }
        let (Some(name), Some(exec)) = (key(&keys, "Name"), key(&keys, "Exec")) else {
            continue;
        };
        let Ok(mut args) = decode_exec(exec) else {
            continue;
        };
        // `env VAR=value program ...`: the variables are dropped (armor's
        // own environment reaches the program).
        if args.first().is_some_and(|a| a == "env") {
            args.remove(0);
            while args
                .first()
                .is_some_and(|a| a.contains('=') && !a.starts_with('/'))
            {
                args.remove(0);
            }
        }
        let Some(program) = args.first().and_then(|p| guest_program(root, p)) else {
            eprintln!("armor: skipping {guest}: its program is not in the package");
            continue;
        };
        args[0] = program;
        entries.push(Entry {
            name: name.to_string(),
            arguments: args,
            icon: key(&keys, "Icon").map(|icon| {
                let host = resolve_in_root(root, guest);
                root_icon(root, icon, host.parent().unwrap_or(root))
            }),
            wm_class: key(&keys, "StartupWMClass").map(str::to_string),
        });
    }
    entries
}

/// The `.desktop` files under the root's `usr/share` (also the ones a
/// package keeps in its own directory and links into `applications` from a
/// maintainer script, which armor does not run), as guest paths.
fn root_desktop_files(root: &Path) -> Vec<String> {
    let dir = root.join("usr/share");
    desktop_files(&dir)
        .into_iter()
        .filter_map(|p| {
            p.strip_prefix(root)
                .ok()
                .map(|rel| format!("/{}", rel.to_string_lossy()))
        })
        .collect()
}

/// `armor install` of an x86 Debian package: its dependencies from the
/// distribution's amd64 archive, all unpacked into the application's root.
fn install_deb(
    places: &Places,
    file: &Path,
    mut name: String,
    explicit_name: bool,
) -> Result<(), String> {
    let scratch = places.data_home.join("armor/cache/scratch");
    fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let control = crate::debs::control_of(file, &scratch)?;
    let package = control.get("Package").unwrap_or("?").to_string();
    match control.get("Architecture") {
        Some("amd64") | Some("all") => {}
        Some("arm64") => {
            return Err(format!(
                "{package} is a native ARM64 package: install it with `sudo apt install {}`",
                file.display()
            ));
        }
        other => {
            return Err(format!(
                "{package}: architecture {other:?}; armor installs amd64 packages"
            ));
        }
    }
    let mut app = places.app(&name);
    if app.exists() {
        return Err(format!(
            "{name} is already installed ({}): `armor remove {name}` first",
            app.display()
        ));
    }
    let os_release =
        fs::read_to_string("/etc/os-release").map_err(|e| format!("/etc/os-release: {e}"))?;
    let archive = crate::debs::archive_for(&os_release)?;
    let cache = places.data_home.join("armor/cache");
    let index = crate::debs::fetch_index(&archive, &cache)?;
    let mut roots: Vec<Vec<String>> = Vec::new();
    for field in ["Pre-Depends", "Depends"] {
        roots.extend(crate::debs::dependency_alternatives(
            control.get(field).unwrap_or(""),
        ));
    }
    // A shell and the basic commands for the programs' launch scripts
    // (`#!/bin/sh`, `#!/usr/bin/env bash`, `dirname`...), which packages do
    // not declare: they are essential on Debian.
    for essential in ["dash", "bash", "coreutils"] {
        roots.push(vec![essential.to_string()]);
    }
    let (packages, missing) = crate::debs::closure(&index, &roots);
    if !missing.is_empty() {
        eprintln!(
            "armor: not in the {} archive, left out: {}",
            archive.suite,
            missing.join(", ")
        );
    }
    let bytes: u64 = packages
        .iter()
        .filter_map(|p| index.packages[p].get("Size")?.parse::<u64>().ok())
        .sum();
    eprintln!(
        "armor: {package} needs {} x86-64 packages from Debian's {} archive ({} MB to download \
         at most, cached for other applications)",
        packages.len(),
        archive.suite,
        bytes / 1_000_000 + 1
    );
    let root = app.join("root");
    fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let conf = AppConf {
        kind: "linux".to_string(),
        env: vec![("QT_XCB_GL_INTEGRATION".to_string(), "none".to_string())],
        ..AppConf::default()
    };
    fs::write(
        app.join("app.conf"),
        render_app_conf(&conf, &[qt_gl_reason()]),
    )
    .map_err(|e| format!("{}: {e}", app.display()))?;
    let result = (|| {
        for (n, dependency) in packages.iter().enumerate() {
            let deb = crate::debs::download(&archive, &index, dependency, &cache)?;
            crate::debs::unpack(&deb, &root, &scratch)?;
            if (n + 1) % 20 == 0 {
                eprintln!("armor: {} of {} packages unpacked", n + 1, packages.len());
            }
        }
        let before = root_desktop_files(&root);
        crate::debs::unpack(file, &root, &scratch)?;
        // Libraries its programs use without declaring them (it may bundle
        // some): the packages that provide them.
        let own: Vec<PathBuf> = crate::debs::file_list(file, &scratch)?
            .iter()
            .map(|p| root.join(p.trim_start_matches("./")))
            .collect();
        let missing = missing_libraries(&root, &own);
        install_providers(
            &missing, &archive, &index, &packages, &cache, &root, &scratch,
        )?;
        let own: Vec<String> = root_desktop_files(&root)
            .into_iter()
            .filter(|f| !before.contains(f))
            .collect();
        Ok::<_, String>(linux_entries(&root, &own))
    })();
    let entries = match result {
        Ok(entries) => entries,
        Err(e) => {
            let _ = fs::remove_dir_all(&app);
            return Err(format!("{e}; nothing was kept"));
        }
    };
    if !explicit_name && let Some(first) = entries.first() {
        let better = sanitize_name(&first.name);
        if profile::valid_name(&better) && better != name && !places.app(&better).exists() {
            let renamed = places.app(&better);
            fs::rename(&app, &renamed).map_err(|e| format!("{}: {e}", renamed.display()))?;
            name = better;
            app = renamed;
        }
    }
    // The icons and programs were resolved under the first name.
    let entries = if app.join("root") != root {
        let files = root_desktop_files(&app.join("root"));
        linux_entries(&app.join("root"), &files)
            .into_iter()
            .filter(|e| entries.iter().any(|old| old.name == e.name))
            .collect()
    } else {
        entries
    };
    let names = write_entry_files(places, &name, &app, "linux", entries)?;
    if names.is_empty() {
        eprintln!(
            "armor: {name} installed, but its package has no menu entry; run a program with \
             `armor run {name} /usr/bin/<program>`"
        );
    } else {
        eprintln!("armor: {name} installed; in the menu:");
        for entry in &names {
            eprintln!("  {entry}");
        }
    }
    Ok(())
}

/// The x86-64 libraries an AppImage expects from the system (it bundles
/// the rest): the C library, X11 and Wayland clients, OpenGL and Vulkan
/// loaders with Mesa, GTK 3, sound, fonts, D-Bus, NSS. Resolved with their
/// dependencies from the host distribution's amd64 archive.
const APPIMAGE_BASE: &[&str] = &[
    "libc6",
    "libstdc++6",
    "zlib1g",
    "libx11-6",
    "libx11-xcb1",
    "libxext6",
    "libxrender1",
    "libxrandr2",
    "libxi6",
    "libxcursor1",
    "libxinerama1",
    "libxfixes3",
    "libxkbcommon0",
    "libxkbcommon-x11-0",
    "libxcb1",
    "libwayland-client0",
    "libgl1",
    "libegl1",
    "libgl1-mesa-dri",
    "libvulkan1",
    "libgbm1",
    "libgtk-3-0",
    "libasound2",
    "libpulse0",
    "libfontconfig1",
    "libfreetype6",
    "libdbus-1-3",
    "libnss3",
    // Launch scripts (AppRun is often `#!/usr/bin/env bash`): without x86
    // ones in the root, the host's would run, and the x86 programs they
    // start would escape armor.
    "dash",
    "bash",
    "coreutils",
];

/// Why x86 Linux applications get `QT_XCB_GL_INTEGRATION=none`.
fn qt_gl_reason() -> (String, String) {
    (
        "QT_XCB_GL_INTEGRATION".to_string(),
        "known armor issue (2026-10-14): Qt's OpenGL probe through Mesa's llvmpipe waits \
         forever on a lock under armor; without it Qt draws in software"
            .to_string(),
    )
}

/// Every file under `dir`.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for item in read.flatten() {
            match item.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(item.path()),
                Ok(_) => found.push(item.path()),
                Err(_) => {}
            }
        }
    }
    found
}

/// The shared libraries the ELF files among `files` (host paths) need
/// (`DT_NEEDED`) that are neither among those files nor in the root's
/// library directories, sorted.
fn missing_libraries(root: &Path, files: &[PathBuf]) -> Vec<String> {
    let mut have: std::collections::HashSet<String> = files
        .iter()
        .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let mut needed = std::collections::BTreeSet::new();
    for path in files {
        if path.is_file()
            && !path.is_symlink()
            && let Ok(bytes) = fs::read(path)
            && bytes.starts_with(b"\x7fELF")
            && let Ok(elf) = armor_elf::Elf::parse_any(&bytes)
            && let Ok(libraries) = elf.needed_libraries(&bytes)
        {
            needed.extend(libraries);
        }
    }
    for dir in [
        "usr/lib/x86_64-linux-gnu",
        "lib/x86_64-linux-gnu",
        "usr/lib",
        "usr/lib64",
        "lib",
    ] {
        if let Ok(read) = fs::read_dir(resolve_in_root(root, &format!("/{dir}"))) {
            have.extend(
                read.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned()),
            );
        }
    }
    needed.into_iter().filter(|n| !have.contains(n)).collect()
}

/// Installs, with their dependencies, the packages that provide the
/// libraries `missing` (from the archive's Contents index) and are not
/// among `installed`.
fn install_providers(
    missing: &[String],
    archive: &crate::debs::Archive,
    index: &crate::debs::Index,
    installed: &[String],
    cache: &Path,
    root: &Path,
    scratch: &Path,
) -> Result<(), String> {
    if missing.is_empty() {
        return Ok(());
    }
    let providers = crate::debs::library_providers(archive, cache)?;
    let mut extra: Vec<Vec<String>> = Vec::new();
    for library in missing {
        match providers.get(library) {
            Some(package) => extra.push(vec![package.clone()]),
            None => eprintln!("armor: no package in the archive provides {library}"),
        }
    }
    let (more, _) = crate::debs::closure(index, &extra);
    let new: Vec<&String> = more.iter().filter(|p| !installed.contains(p)).collect();
    eprintln!(
        "armor: it also needs {} ({} more packages)",
        missing.join(", "),
        new.len()
    );
    for package in new {
        let deb = crate::debs::download(archive, index, package, cache)?;
        crate::debs::unpack(&deb, root, scratch)?;
    }
    Ok(())
}

/// Libraries only a graphical program links: the programs of an archive
/// that need one get a menu entry, the others (command-line tools) not.
const GRAPHICAL_LIBRARIES: &[&str] = &[
    "libX11.so",
    "libxcb.so",
    "libwayland-client.so",
    "libgtk-",
    "libgdk-",
    "libQt",
    "libSDL",
    "libGL.so",
    "libEGL.so",
    "libvulkan.so",
];

/// The x86-64 graphical programs of an unpacked archive, worth a menu
/// entry: the executable ELF files with an interpreter that need one of
/// `GRAPHICAL_LIBRARIES`, at most three directories deep, sorted by depth
/// then name.
fn archive_programs(top: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut level = vec![top.to_path_buf()];
    for _depth in 0..3 {
        let mut next = Vec::new();
        for dir in level {
            let Ok(read) = fs::read_dir(&dir) else {
                continue;
            };
            let mut items: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
            items.sort();
            for path in items {
                if path.is_dir() && !path.is_symlink() {
                    next.push(path);
                    continue;
                }
                use std::os::unix::fs::PermissionsExt;
                let executable = fs::metadata(&path)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
                if executable
                    && let Ok(bytes) = fs::read(&path)
                    && let Ok(elf) = armor_elf::Elf::parse_any(&bytes)
                    && elf.class64
                    && elf.interpreter().is_some()
                    && !path.to_string_lossy().contains(".so")
                    && elf.needed_libraries(&bytes).is_ok_and(|needed| {
                        needed
                            .iter()
                            .any(|n| GRAPHICAL_LIBRARIES.iter().any(|g| n.starts_with(g)))
                    })
                {
                    found.push(path);
                }
            }
        }
        level = next;
    }
    found
}

/// `armor install` of an archive of an x86-64 Linux program (`.tar.gz`,
/// `.tar.xz`, ...): the system libraries (`APPIMAGE_BASE`) and the ones its
/// programs need into the application's root, the archive unpacked into
/// `/opt` (its single top directory there, where such archives usually
/// expect to be so that the absolute paths of their `.desktop` files hold;
/// else `/opt/<name>`); a menu entry for each `.desktop` file it brings, or
/// else for each of its graphical programs (`archive_programs`).
fn install_archive(
    places: &Places,
    file: &Path,
    mut name: String,
    explicit_name: bool,
) -> Result<(), String> {
    let app = places.app(&name);
    if app.exists() {
        return Err(format!(
            "{name} is already installed ({}): `armor remove {name}` first",
            app.display()
        ));
    }
    let root = app.join("root");
    let unpack = app.join("unpack");
    for dir in [&root, &unpack] {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(app.join("app.conf"), "kind = linux\n")
        .map_err(|e| format!("{}: {e}", app.display()))?;
    let result = (|| {
        let os_release =
            fs::read_to_string("/etc/os-release").map_err(|e| format!("/etc/os-release: {e}"))?;
        let archive = crate::debs::archive_for(&os_release)?;
        let cache = places.data_home.join("armor/cache");
        let scratch = cache.join("scratch");
        fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
        let index = crate::debs::fetch_index(&archive, &cache)?;
        let roots: Vec<Vec<String>> = APPIMAGE_BASE.iter().map(|p| vec![p.to_string()]).collect();
        let (packages, _) = crate::debs::closure(&index, &roots);
        eprintln!(
            "armor: adding {} x86-64 system packages from Debian's {} archive",
            packages.len(),
            archive.suite
        );
        for package in &packages {
            let deb = crate::debs::download(&archive, &index, package, &cache)?;
            crate::debs::unpack(&deb, &root, &scratch)?;
        }
        eprintln!("armor: unpacking {}", file.display());
        let status = Command::new("tar")
            .arg("-xf")
            .arg(file)
            .arg("-C")
            .arg(&unpack)
            .args(["--no-same-owner"])
            .status()
            .map_err(|e| format!("tar: {e}"))?;
        if !status.success() {
            return Err(format!("{}: tar could not unpack it", file.display()));
        }
        let items: Vec<PathBuf> = fs::read_dir(&unpack)
            .map_err(|e| format!("{}: {e}", unpack.display()))?
            .flatten()
            .map(|e| e.path())
            .collect();
        let opt = root.join("opt");
        fs::create_dir_all(&opt).map_err(|e| format!("{}: {e}", opt.display()))?;
        let top = match items.as_slice() {
            [single] if single.is_dir() && !single.is_symlink() => {
                let top = opt.join(single.file_name().unwrap_or_default());
                fs::rename(single, &top).map_err(|e| format!("{}: {e}", top.display()))?;
                fs::remove_dir(&unpack).map_err(|e| format!("{}: {e}", unpack.display()))?;
                top
            }
            _ => {
                let top = opt.join(&name);
                fs::rename(&unpack, &top).map_err(|e| format!("{}: {e}", top.display()))?;
                top
            }
        };
        let missing = missing_libraries(&root, &files_under(&top));
        install_providers(
            &missing, &archive, &index, &packages, &cache, &root, &scratch,
        )?;
        let guest = |path: &Path| {
            format!(
                "/{}",
                path.strip_prefix(&root).unwrap_or(path).to_string_lossy()
            )
        };
        let desktop: Vec<String> = files_under(&top)
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
            .map(|p| guest(p))
            .collect();
        let mut entries = linux_entries(&root, &desktop);
        // A `.desktop` file written for another directory: its program by
        // name among the archive's own.
        let unpacked = files_under(&top);
        for entry in &mut entries {
            let program = &entry.arguments[0];
            if resolve_in_root(&root, program).is_file() {
                continue;
            }
            let base = Path::new(program).file_name();
            if let Some(found) = unpacked.iter().find(|p| p.file_name() == base) {
                entry.arguments[0] = guest(found);
            }
        }
        // An icon named but kept in the archive's own directories: its
        // largest PNG of that name.
        for entry in &mut entries {
            let Some(icon) = &entry.icon else { continue };
            if Path::new(icon).is_file() {
                continue;
            }
            let wanted = Path::new(icon).file_name().map(|n| n.to_os_string());
            let largest = unpacked
                .iter()
                .filter(|p| {
                    p.extension().is_some_and(|e| e == "png")
                        && p.file_stem().map(|s| s.to_os_string()) == wanted
                })
                .max_by_key(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0));
            if let Some(png) = largest {
                entry.icon = Some(png.to_string_lossy().into_owned());
            }
        }
        if entries.is_empty() {
            for program in archive_programs(&top) {
                entries.push(Entry {
                    name: program
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    arguments: vec![guest(&program)],
                    icon: None,
                    wm_class: None,
                });
            }
        }
        Ok::<_, String>((entries, guest(&top)))
    })();
    let (entries, top) = match result {
        Ok(found) => found,
        Err(e) => {
            let _ = fs::remove_dir_all(&app);
            return Err(format!("{e}; nothing was kept"));
        }
    };
    let conf = AppConf {
        kind: "linux".to_string(),
        env: vec![("QT_XCB_GL_INTEGRATION".to_string(), "none".to_string())],
        ..AppConf::default()
    };
    fs::write(
        app.join("app.conf"),
        render_app_conf(&conf, &[qt_gl_reason()]),
    )
    .map_err(|e| format!("{}: {e}", app.display()))?;
    let first_root = app.join("root");
    let mut app = app;
    if !explicit_name && let Some(first) = entries.first() {
        let better = sanitize_name(&first.name);
        if profile::valid_name(&better) && better != name && !places.app(&better).exists() {
            let renamed = places.app(&better);
            fs::rename(&app, &renamed).map_err(|e| format!("{}: {e}", renamed.display()))?;
            name = better;
            app = renamed;
        }
    }
    let entries = entries
        .into_iter()
        .map(|mut e| {
            if let Some(icon) = &e.icon
                && let Ok(rest) = Path::new(icon).strip_prefix(&first_root)
            {
                e.icon = Some(app.join("root").join(rest).to_string_lossy().into_owned());
            }
            e
        })
        .collect();
    let names = write_entry_files(places, &name, &app, "linux", entries)?;
    if names.is_empty() {
        eprintln!(
            "armor: {name} installed, but it has no menu entry or graphical program; its \
             files are in {top}, run one with `armor run {name} {top}/<program>`"
        );
    } else {
        eprintln!("armor: {name} installed; in the menu:");
        for entry in &names {
            eprintln!("  {entry}");
        }
    }
    Ok(())
}

/// `armor install` of an x86-64 AppImage: the system libraries AppImages
/// expect (`APPIMAGE_BASE`) go into the application's root first; then its
/// own runtime, run under armor with that root (it may be dynamic), unpacks
/// it (`--appimage-extract`: no FUSE, no squashfs tools) into `/opt/appimage`
/// of the root; the entry runs its `AppRun` with `APPDIR` set, as the
/// AppImage runtime does.
fn install_appimage(
    places: &Places,
    file: &Path,
    mut name: String,
    explicit_name: bool,
) -> Result<(), String> {
    let app = places.app(&name);
    if app.exists() {
        return Err(format!(
            "{name} is already installed ({}): `armor remove {name}` first",
            app.display()
        ));
    }
    let root = app.join("root");
    fs::create_dir_all(root.join("opt")).map_err(|e| format!("{}: {e}", root.display()))?;
    fs::write(app.join("app.conf"), "kind = linux\n")
        .map_err(|e| format!("{}: {e}", app.display()))?;
    let result = (|| {
        let os_release =
            fs::read_to_string("/etc/os-release").map_err(|e| format!("/etc/os-release: {e}"))?;
        let archive = crate::debs::archive_for(&os_release)?;
        let cache = places.data_home.join("armor/cache");
        let scratch = cache.join("scratch");
        fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
        let index = crate::debs::fetch_index(&archive, &cache)?;
        let roots: Vec<Vec<String>> = APPIMAGE_BASE.iter().map(|p| vec![p.to_string()]).collect();
        let (packages, missing) = crate::debs::closure(&index, &roots);
        if !missing.is_empty() {
            eprintln!(
                "armor: not in the archive, left out: {}",
                missing.join(", ")
            );
        }
        eprintln!(
            "armor: adding {} x86-64 system packages from Debian's {} archive",
            packages.len(),
            archive.suite
        );
        for package in &packages {
            let deb = crate::debs::download(&archive, &index, package, &cache)?;
            crate::debs::unpack(&deb, &root, &scratch)?;
        }
        eprintln!("armor: unpacking {}", file.display());
        let armor = std::env::current_exe().map_err(|e| format!("armor's own path: {e}"))?;
        let unpack = app.join(".unpack");
        fs::create_dir_all(&unpack).map_err(|e| format!("{}: {e}", unpack.display()))?;
        let status = Command::new(&armor)
            .arg("--root")
            .arg(&root)
            .arg(file)
            .arg("--appimage-extract")
            .current_dir(&unpack)
            .stdout(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("{}: {e}", armor.display()))?;
        let appdir = unpack.join("squashfs-root");
        if !status.success() || !appdir.join("AppRun").exists() {
            return Err(format!(
                "{}: its runtime did not unpack it ({}, no AppRun)",
                file.display(),
                status_text(status)
            ));
        }
        let guest_dir = "/opt/appimage".to_string();
        fs::rename(&appdir, root.join("opt/appimage"))
            .map_err(|e| format!("{}: {e}", root.display()))?;
        let _ = fs::remove_dir_all(&unpack);
        // Libraries its programs need that neither it nor the root has: the
        // packages that provide them, from the archive's Contents index.
        let missing = missing_libraries(&root, &files_under(&root.join("opt/appimage")));
        install_providers(
            &missing, &archive, &index, &packages, &cache, &root, &scratch,
        )?;
        // The AppImage's own entry: a .desktop file at the top of the AppDir.
        let top: Vec<String> = fs::read_dir(root.join("opt/appimage"))
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".desktop"))
            .map(|n| format!("{guest_dir}/{n}"))
            .collect();
        let mut entries = Vec::new();
        for guest in top {
            let Ok(text) = fs::read_to_string(resolve_in_root(&root, &guest)) else {
                continue;
            };
            let keys = desktop_keys(&text);
            let Some(entry_name) = key(&keys, "Name") else {
                continue;
            };
            let mut arguments = vec![format!("{guest_dir}/AppRun")];
            if let Some(exec) = key(&keys, "Exec")
                && let Ok(args) = decode_exec(exec)
            {
                arguments.extend(args.into_iter().skip(1));
            }
            let icon = key(&keys, "Icon").map(|icon| {
                let dir = root.join("opt/appimage");
                [
                    format!("{icon}.png"),
                    format!("{icon}.svg"),
                    ".DirIcon".to_string(),
                ]
                .iter()
                .map(|f| dir.join(f))
                .find(|p| p.is_file())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| root_icon(&root, icon, &dir))
            });
            entries.push(Entry {
                name: entry_name.to_string(),
                arguments,
                icon,
                wm_class: key(&keys, "StartupWMClass").map(str::to_string),
            });
        }
        if entries.is_empty() {
            entries.push(Entry {
                name: name.clone(),
                arguments: vec![format!("{guest_dir}/AppRun")],
                icon: None,
                wm_class: None,
            });
        }
        Ok::<_, String>((entries, guest_dir))
    })();
    let (entries, guest_dir) = match result {
        Ok(found) => found,
        Err(e) => {
            let _ = fs::remove_dir_all(&app);
            return Err(format!("{e}; nothing was kept"));
        }
    };
    let conf = AppConf {
        kind: "linux".to_string(),
        env: vec![
            ("APPDIR".to_string(), guest_dir.clone()),
            ("APPIMAGE".to_string(), file.to_string_lossy().into_owned()),
            ("QT_XCB_GL_INTEGRATION".to_string(), "none".to_string()),
        ],
        ..AppConf::default()
    };
    fs::write(
        app.join("app.conf"),
        render_app_conf(&conf, &[qt_gl_reason()]),
    )
    .map_err(|e| format!("{}: {e}", app.display()))?;
    let first_root = app.join("root");
    let mut app = app;
    if !explicit_name && let Some(first) = entries.first() {
        let better = sanitize_name(&first.name);
        if profile::valid_name(&better) && better != name && !places.app(&better).exists() {
            let renamed = places.app(&better);
            fs::rename(&app, &renamed).map_err(|e| format!("{}: {e}", renamed.display()))?;
            name = better;
            app = renamed;
        }
    }
    // Icon paths point into the root under its final name.
    let entries = entries
        .into_iter()
        .map(|mut e| {
            if let Some(icon) = &e.icon
                && let Ok(rest) = Path::new(icon).strip_prefix(&first_root)
            {
                e.icon = Some(app.join("root").join(rest).to_string_lossy().into_owned());
            }
            e
        })
        .collect();
    let names = write_entry_files(places, &name, &app, "linux", entries)?;
    eprintln!("armor: {name} installed; in the menu:");
    for entry in &names {
        eprintln!("  {entry}");
    }
    Ok(())
}

fn read_entries(app: &Path) -> Vec<(String, String, Vec<String>)> {
    fs::read_to_string(app.join("entries"))
        .map(|text| parse_entries(&text))
        .unwrap_or_default()
}

fn installed(places: &Places, name: &str) -> Result<PathBuf, String> {
    let app = places.app(name);
    if profile::valid_name(name) && app.join("app.conf").is_file() {
        Ok(app)
    } else {
        Err(format!(
            "{name} is not installed (`armor list` shows what is)"
        ))
    }
}

/// `armor run <name> [arguments for wine]`: without arguments, the
/// application's only menu entry; with one argument that is the name of
/// one of its menu entries, that entry.
fn run(args: &[OsString]) -> Result<ExitCode, String> {
    let Some((name, arguments)) = args.split_first() else {
        return Err("usage: armor run <application> [program and arguments]".to_string());
    };
    let name = name.to_string_lossy();
    let places = Places::from_env()?;
    let app = installed(&places, &name)?;
    let mut arguments: Vec<OsString> = arguments.to_vec();
    let entries = read_entries(&app);
    if let [only] = arguments.as_slice()
        && let Some((_, _, args)) = entries.iter().find(|(_, entry, _)| only == entry.as_str())
    {
        arguments = args.iter().map(OsString::from).collect();
    }
    if arguments.is_empty() {
        match entries.as_slice() {
            [(_, _, only)] => arguments = only.iter().map(OsString::from).collect(),
            _ => {
                let mut message = format!("{name} has {} menu entries; run one of:", entries.len());
                for (_, entry, _) in &entries {
                    message.push_str(&format!("\n  armor run {name} {}", quote_exec_arg(entry)));
                }
                return Err(message);
            }
        }
    }
    let conf = read_conf(&app)?;
    // The desktop replaces field codes; a stored entry run directly drops
    // them.
    arguments.retain(|a| !is_field_code(&a.to_string_lossy()));
    if conf.kind == "steam" {
        let steam_dir = app.join("Steam");
        crate::steam::link_home(&app.join("home"), &steam_dir)?;
        // After updating itself the client exits with 42 to be started
        // again, as Valve's `steam.sh` does.
        for _ in 0..crate::steam::MAX_RESTARTS {
            let mut command = Command::new(crate::steam::client(&steam_dir));
            // Its own HOME keeps the client's files in the application's
            // directory; the desktop's X authority stays in the real home.
            if std::env::var_os("XAUTHORITY").is_none()
                && let Some(home) = std::env::var_os("HOME")
            {
                command.env("XAUTHORITY", PathBuf::from(home).join(".Xauthority"));
            }
            command
                .env("HOME", app.join("home"))
                .current_dir(&steam_dir)
                .args(&arguments);
            let run = run_watched(command, &name, &app)?;
            if run.status != Some(crate::steam::RESTART_STATUS) {
                report_run(&name, &app, &run);
                return Ok(run.code);
            }
            eprintln!("armor: the Steam client updated itself; starting it again");
        }
        return Err("the Steam client kept asking to be restarted".to_string());
    }
    if conf.kind == "linux" {
        let root = app.join("root");
        let Some(program) = arguments.first().map(|p| p.to_string_lossy().into_owned()) else {
            return Err(format!(
                "usage: armor run {name} /usr/bin/<program> [arguments]"
            ));
        };
        let armor = std::env::current_exe().map_err(|e| format!("armor's own path: {e}"))?;
        let mut command = Command::new(armor);
        for (variable, value) in &conf.env {
            if std::env::var_os(variable).is_none_or(|v| v.is_empty()) {
                command.env(variable, value);
            }
        }
        command
            .arg("--root")
            .arg(&root)
            .arg("--argv0")
            .arg(&program)
            .arg(resolve_in_root(&root, &program))
            .args(&arguments[1..]);
        let run = run_watched(command, &name, &app)?;
        report_run(&name, &app, &run);
        return Ok(run.code);
    }
    places.check_wine()?;
    let mut command = wine_command(&places.wine, &app, &conf);
    command.args(&arguments);
    let first = run_watched(command, &name, &app)?;
    // Only when the program could not start (not for an optional DLL a
    // running program probed).
    let needed = start_blockers(&first.failures)
        .into_iter()
        .find(|(library, _)| vcredist_library(library))
        .filter(|_| first.status != Some(0))
        .map(|(library, needed_by)| (library.to_string(), needed_by.to_string()));
    let Some((library, needed_by)) = needed.filter(|_| !conf.vcredist) else {
        report_run(&name, &app, &first);
        return Ok(first.code);
    };
    // A library of the Visual C++ runtime that Wine does not have (MFC,
    // for one): install Microsoft's redistributable in the prefix, then
    // open the program again.
    let message = format!(
        "{needed_by} needs {library}, part of the Microsoft Visual C++ runtime: installing it \
         (version {}) and opening {name} again",
        crate::engines::VCREDIST_VERSION
    );
    eprintln!("armor: {message}");
    notify_unless_terminal(&format!("{name}: installing a runtime"), &message);
    if let Err(error) = install_vcredist(&places, &app, &conf, &needed_by, &library) {
        eprintln!("armor: the Visual C++ runtime could not be installed: {error}");
        report_run(&name, &app, &first);
        return Ok(first.code);
    }
    let conf = read_conf(&app)?;
    let mut command = wine_command(&places.wine, &app, &conf);
    command.args(&arguments);
    let second = run_watched(command, &name, &app)?;
    report_run(&name, &app, &second);
    Ok(second.code)
}

/// Libraries of the Microsoft Visual C++ 2015-2022 runtime (by name);
/// Wine has its own `msvcp140`, `vcruntime140` and the like, not MFC.
pub fn vcredist_library(library: &str) -> bool {
    let library = library.to_ascii_lowercase();
    [
        "mfc140",
        "mfcm140",
        "vccorlib140",
        "vcruntime140",
        "msvcp140",
        "concrt140",
        "vcomp140",
        "vcamp140",
    ]
    .iter()
    .any(|prefix| library.starts_with(prefix))
}

/// Installs the Visual C++ redistributables (x64 and x86, fetched and
/// verified by `engines::fetch_vcredist`) silently in the application's
/// prefix, and records it in `app.conf` with the reason.
fn install_vcredist(
    places: &Places,
    app: &Path,
    conf: &AppConf,
    needed_by: &str,
    library: &str,
) -> Result<(), String> {
    let installers = crate::engines::fetch_vcredist(&places.data_home.join("armor/engines"))?;
    for installer in &installers {
        let status = wine_command(&places.wine, app, conf)
            .arg(installer)
            .args(["/install", "/quiet", "/norestart"])
            .status()
            .map_err(|e| format!("{}: {e}", places.wine.display()))?;
        // 3010: done, a restart wanted; 1638: a newer version is there.
        if !matches!(status.code(), Some(0 | 3010 | 1638)) {
            return Err(format!(
                "{} ended with {}",
                installer.display(),
                status_text(status)
            ));
        }
    }
    let path = app.join("app.conf");
    let mut text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.push_str(&format!(
        "# Microsoft Visual C++ 2015-2022 runtime {}, installed by armor: {needed_by} needed \
         {library}\nvcredist = yes\n",
        crate::engines::VCREDIST_VERSION
    ));
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The most of a run's standard error kept in `last-run.log`.
const RUN_LOG_LIMIT: usize = 4 << 20;

/// Why a run of an installed program went wrong, from a line of its
/// standard error (Wine's or armor's own messages).
#[derive(Debug, PartialEq)]
pub enum Failure {
    /// Wine's loader did not find a DLL that `needed_by` imports. Alone
    /// it is no failure: Wine also says so when a running program probes
    /// an optional DLL (a plugin) with `LoadLibrary`.
    MissingLibrary { library: String, needed_by: String },
    /// Wine's loader could not import the DLLs of the program itself: the
    /// process did not start.
    StartFailed,
    /// Wine's report of an unhandled exception (a crash).
    Crash(String),
    /// armor stopped the x86 Linux program, with its reason.
    Armor(String),
}

/// The failure a line of standard error reports, if any.
pub fn failure_in_line(line: &str) -> Option<Failure> {
    // `0024:err:module:import_dll Library X.dll (which is needed by
    // L"Z:\\path\\program.exe") not found`
    if let Some(rest) = line.split("err:module:import_dll Library ").nth(1) {
        let (library, rest) = rest.split_once(" (which is needed by L\"")?;
        let needed_by = rest.split('"').next()?;
        let needed_by = needed_by.rsplit('\\').next().unwrap_or(needed_by);
        return Some(Failure::MissingLibrary {
            library: library.to_string(),
            needed_by: needed_by.to_string(),
        });
    }
    // `0024:err:module:loader_init Importing dlls for L"Z:\\...\\x.exe"
    // failed, status c0000135` (older Wine: from `LdrInitializeThunk`).
    if line.contains("Importing dlls for L\"") && line.contains("\" failed") {
        return Some(Failure::StartFailed);
    }
    // `wine: Unhandled page fault on write access to 0000000000000000 at
    // address 0000000140001000 (thread 0024), starting debugger...`
    if let Some(rest) = line.strip_prefix("wine: Unhandled ") {
        let what = rest.split(" (thread ").next().unwrap_or(rest);
        return Some(Failure::Crash(what.trim_end_matches(',').to_string()));
    }
    let reason = line.strip_prefix("armor: ")?;
    if reason.starts_with("suggestion:") {
        return None;
    }
    Some(Failure::Armor(reason.to_string()))
}

/// The libraries that kept a program from starting: the missing DLLs
/// Wine reported, when it also reported that the program's own imports
/// failed (`StartFailed`); none otherwise.
pub fn start_blockers(failures: &[Failure]) -> Vec<(&str, &str)> {
    if !failures.contains(&Failure::StartFailed) {
        return Vec::new();
    }
    let mut blockers: Vec<(&str, &str)> = Vec::new();
    for failure in failures {
        if let Failure::MissingLibrary { library, needed_by } = failure
            && !blockers.iter().any(|(l, _)| l == library)
        {
            blockers.push((library, needed_by));
        }
    }
    blockers
}

/// The message for the user about a run of `name` that exited with
/// `status` or ended by `signal`, after the `failures` found in its
/// standard error; `None` when nothing went wrong that armor can explain:
/// exit status 0 (whatever was printed), a plain non-zero exit (programs
/// use them on purpose), or a signal someone sends to stop a program
/// (`SIGINT`, `SIGTERM`, `SIGHUP`).
pub fn explain_run(
    name: &str,
    failures: &[Failure],
    status: Option<i32>,
    signal: Option<i32>,
) -> Option<String> {
    if status == Some(0) || matches!(signal, Some(1 | 2 | 15)) {
        return None;
    }
    let blockers = start_blockers(failures);
    if let Some((_, needed_by)) = blockers.first() {
        let libraries: Vec<&str> = blockers.iter().map(|(l, _)| *l).collect();
        return Some(format!(
            "{name} could not start: {needed_by} needs {}, which neither the program nor \
             Wine provides (often a runtime the installer did not install, such as Visual C++ \
             for msvc*, vcruntime* and mfc* libraries)",
            libraries.join(", ")
        ));
    }
    if let Some(Failure::Crash(what)) = failures.iter().find(|f| matches!(f, Failure::Crash(_))) {
        return Some(format!("{name} crashed: unhandled {what}"));
    }
    if let Some(Failure::Armor(reason)) = failures.iter().find(|f| matches!(f, Failure::Armor(_))) {
        return Some(format!("{name} stopped: {reason}"));
    }
    let signal = signal?;
    let what = match signal {
        4 => "SIGILL, an illegal instruction",
        6 => "SIGABRT, it aborted",
        7 => "SIGBUS, a bus error",
        8 => "SIGFPE, an arithmetic error",
        9 => "SIGKILL",
        11 => "SIGSEGV, an invalid memory access",
        _ => "",
    };
    Some(if what.is_empty() {
        format!("{name} was stopped by signal {signal}")
    } else {
        format!("{name} was stopped by signal {signal} ({what})")
    })
}

/// How a run of an installed program ended.
struct Run {
    /// The program's exit status, for armor's own.
    code: ExitCode,
    /// The signal that ended it, if one did.
    signal: Option<i32>,
    /// Its exit status, if it exited.
    status: Option<i32>,
    failures: Vec<Failure>,
}

/// Runs an installed program, passing its standard error through and
/// keeping it in the application's `last-run.log`, with the failures
/// found in it (`failure_in_line`).
fn run_watched(mut command: Command, name: &str, app: &Path) -> Result<Run, String> {
    use std::io::{BufRead, Write};
    let mut child = command
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", command.get_program().to_string_lossy()))?;
    let log_path = app.join("last-run.log");
    let mut log = fs::File::create(&log_path).ok();
    let mut written = 0;
    let mut failures = Vec::new();
    if let Some(stderr) = child.stderr.take() {
        let mut reader = std::io::BufReader::new(stderr);
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line).unwrap_or(0) > 0 {
            let _ = std::io::stderr().write_all(&line);
            if let Some(log) = &mut log
                && written < RUN_LOG_LIMIT
            {
                let _ = log.write_all(&line);
                written += line.len();
            }
            if let Some(failure) = failure_in_line(String::from_utf8_lossy(&line).trim_end()) {
                failures.push(failure);
            }
            line.clear();
        }
    }
    let status = child
        .wait()
        .map_err(|e| format!("waiting for {name}: {e}"))?;
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    let code = match (status.code(), signal) {
        (Some(code), _) => ExitCode::from(code as u8),
        (None, Some(signal)) => ExitCode::from(128u8.wrapping_add(signal as u8)),
        (None, None) => ExitCode::from(EXIT_ERROR),
    };
    Ok(Run {
        code,
        signal,
        status: status.code(),
        failures,
    })
}

/// Says why a run failed, if armor recognises it (`explain_run`): on
/// standard error and, when armor was not started from a terminal (the
/// desktop menu), in a desktop notification.
fn report_run(name: &str, app: &Path, run: &Run) {
    if let Some(message) = explain_run(name, &run.failures, run.status, run.signal) {
        eprintln!("armor: {message}");
        eprintln!(
            "armor: its messages are in {}",
            app.join("last-run.log").display()
        );
        notify_unless_terminal(&format!("{name} stopped"), &message);
    } else if let Some(status) = run.status.filter(|s| *s != 0) {
        eprintln!("armor: {name} ended with exit status {status}");
    }
}

/// A desktop notification when standard error is not a terminal (armor
/// started from the desktop menu, where nobody reads it).
fn notify_unless_terminal(summary: &str, body: &str) {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        notify(summary, body);
    }
}

/// A desktop notification (the freedesktop Notifications service, through
/// `gdbus`); nothing if there is no such service.
fn notify(summary: &str, body: &str) {
    // GVariant text strings: double quotes, with `\` and `"` escaped.
    let quote = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
    let _ = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.Notifications",
            "--object-path",
            "/org/freedesktop/Notifications",
            "--method",
            "org.freedesktop.Notifications.Notify",
            "\"armor\"",
            "0",
            "\"dialog-error\"",
        ])
        .arg(quote(summary))
        .arg(quote(body))
        // Typed, or gdbus would take `-1` for an option.
        .args(["[]", "{}", "int32 -1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// `armor list`: the installed applications and their menu entries.
///
/// `--porcelain`: a stable format for programs (the window, ADR-0035), one
/// tab-separated record per line: `engine\t<wine path>\t<yes|no present>`,
/// then for each application `app\t<name>\t<kind>\t<dxvk yes|no>\t<mono
/// yes|no>` followed by its `entry\t<name>\t<entry name>\t<icon>` lines
/// (`<icon>`: the entry's icon, a path or a theme name, empty if none;
/// since 2026-10-14). Readers must ignore fields they do not know, so that
/// more can be appended.
fn list(porcelain: bool) -> Result<(), String> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    // A reader that stops early (`armor list | head`) ends the listing.
    macro_rules! out {
        ($($arg:tt)*) => {
            if writeln!(stdout, $($arg)*).is_err() {
                return Ok(());
            }
        };
    }
    let places = Places::from_env()?;
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    if porcelain {
        out!(
            "engine\t{}\t{}",
            places.wine.display(),
            yes_no(places.wine.is_file())
        );
    }
    let mut names: Vec<String> = match fs::read_dir(places.apps()) {
        Ok(read) => read
            .flatten()
            .filter(|d| d.path().join("app.conf").is_file())
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("{}: {e}", places.apps().display())),
    };
    names.sort();
    for name in names {
        let app = places.app(&name);
        if porcelain {
            let conf = read_conf(&app).unwrap_or_default();
            out!(
                "app\t{name}\t{}\t{}\t{}",
                conf.kind,
                yes_no(conf.dxvk),
                yes_no(conf.mono)
            );
            for (file, entry, _) in read_entries(&app) {
                // The icon of the entry's `.desktop` file, empty if none.
                let icon = fs::read_to_string(places.applications().join(&file))
                    .ok()
                    .and_then(|text| key(&desktop_keys(&text), "Icon").map(str::to_string))
                    .unwrap_or_default();
                out!("entry\t{name}\t{entry}\t{icon}");
            }
            continue;
        }
        match read_conf(&app) {
            Ok(conf) => out!(
                "{name} ({}{}{})",
                conf.kind,
                if conf.dxvk { ", DXVK" } else { "" },
                if conf.mono { ", .NET" } else { "" }
            ),
            Err(e) => out!("{name} (unreadable: {e})"),
        }
        for (_, entry, _) in read_entries(&app) {
            out!("  {entry}");
        }
    }
    Ok(())
}

/// `armor remove <name>`: stops its Wine processes, removes its menu
/// entries and its directory.
fn remove(args: &[OsString]) -> Result<(), String> {
    let [name] = args else {
        return Err("usage: armor remove <application>".to_string());
    };
    let name = name.to_string_lossy();
    let places = Places::from_env()?;
    let app = installed(&places, &name)?;
    let conf = read_conf(&app).unwrap_or_default();
    if conf.kind == "windows" && places.wineserver().is_file() {
        let _ = wine_command(&places.wineserver(), &app, &conf)
            .arg("-k")
            .status();
    }
    for (file, _, _) in read_entries(&app) {
        // Only armor's own files, never a path from elsewhere.
        if file.starts_with("armor-") && !file.contains('/') {
            let path = places.applications().join(&file);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
    }
    fs::remove_dir_all(&app).map_err(|e| format!("{}: {e}", app.display()))?;
    eprintln!("armor: {name} removed");
    Ok(())
}

/// Runs `armor <command> ...` for the application commands; `None` when
/// `command` is not one of them.
pub fn main(command: &OsStr, args: &[OsString]) -> Option<ExitCode> {
    // `--pause` (the desktop's "Install with armor" entry, which runs in a
    // terminal window): wait for Enter at the end, so the window stays open.
    let pause = command.as_bytes() == b"install" && args.iter().any(|a| a == "--pause");
    let without_pause: Vec<OsString> = args.iter().filter(|a| *a != "--pause").cloned().collect();
    let args = without_pause.as_slice();
    let result = match command.as_bytes() {
        b"install" => install(args),
        b"run" => {
            return Some(match run(args) {
                Ok(code) => code,
                Err(message) => fail(message),
            });
        }
        b"list" if args.is_empty() => list(false),
        b"list" if args.len() == 1 && args[0] == "--porcelain" => list(true),
        b"list" => Err("usage: armor list [--porcelain]".to_string()),
        b"remove" => remove(args),
        b"engine" => Places::from_env().and_then(|p| crate::engines::main(args, &p.data_home)),
        b"update" => Places::from_env().and_then(|p| crate::update::main(args, &p.data_home)),
        _ => return None,
    };
    let code = match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => fail(message),
    };
    if pause {
        eprintln!("armor: press Enter to close this window");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_failures_are_recognized() {
        // Lines measured under Hangover 11.16 (2026-10-14).
        let missing = r#"0024:err:module:import_dll Library armor_missing_runtime.dll (which is needed by L"Z:\\tmp\\probe\\needs.exe") not found"#;
        assert_eq!(
            failure_in_line(missing),
            Some(Failure::MissingLibrary {
                library: "armor_missing_runtime.dll".to_string(),
                needed_by: "needs.exe".to_string(),
            })
        );
        let crash = "wine: Unhandled page fault on write access to 0000000000000000 at address \
                     0000000140001000 (thread 0024), starting debugger...";
        assert_eq!(
            failure_in_line(crash),
            Some(Failure::Crash(
                "page fault on write access to 0000000000000000 at address 0000000140001000"
                    .to_string()
            ))
        );
        assert_eq!(
            failure_in_line("armor: x86_64 syscall 999 is not supported yet"),
            Some(Failure::Armor(
                "x86_64 syscall 999 is not supported yet".to_string()
            ))
        );
        assert_eq!(
            failure_in_line("armor: suggestion: try --interpreter"),
            None
        );
        assert_eq!(failure_in_line("0024:err:module:other thing"), None);
        assert_eq!(failure_in_line("starting FEX (Hangover 11.16)"), None);
        assert!(vcredist_library("mfc140u.dll") && vcredist_library("MSVCP140_1.DLL"));
        assert!(!vcredist_library("mfc120u.dll") && !vcredist_library("d3d11.dll"));

        let started = r#"0024:err:module:loader_init Importing dlls for L"Z:\\tmp\\probe\\needs.exe" failed, status c0000135"#;
        assert_eq!(failure_in_line(started), Some(Failure::StartFailed));
        let failures = [
            failure_in_line(missing).unwrap(),
            failure_in_line(started).unwrap(),
        ];
        let message = explain_run("Probe", &failures, Some(53), None).unwrap();
        assert!(message.starts_with(
            "Probe could not start: needs.exe needs armor_missing_runtime.dll, which neither"
        ));
        assert_eq!(
            explain_run("Probe", &[failure_in_line(crash).unwrap()], Some(84), None).unwrap(),
            "Probe crashed: unhandled page fault on write access to 0000000000000000 at address \
             0000000140001000"
        );
        // A missing optional DLL in a program that started (no
        // `StartFailed`), or any message before exit status 0: nothing.
        let probed = [failure_in_line(missing).unwrap()];
        assert!(start_blockers(&probed).is_empty());
        assert_eq!(explain_run("Probe", &probed, Some(1), None), None);
        assert_eq!(explain_run("Probe", &failures, Some(0), None), None);
        // A plain exit status is the program's business; so is a signal
        // sent to stop it.
        assert_eq!(explain_run("Probe", &[], Some(3), None), None);
        assert_eq!(explain_run("Probe", &[], None, Some(15)), None);
        assert_eq!(
            explain_run("Probe", &[], None, Some(11)).unwrap(),
            "Probe was stopped by signal 11 (SIGSEGV, an invalid memory access)"
        );
    }

    #[test]
    fn installers_are_recognized() {
        let mut pe = vec![0u8; 0x100];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        assert_eq!(installer_kind(&pe), Ok(Installer::WindowsExe));
        let mut msi = COMPOUND_FILE_MAGIC.to_vec();
        msi.extend([0; 32]);
        assert_eq!(installer_kind(&msi), Ok(Installer::WindowsMsi));
        assert_eq!(
            installer_kind(b"!<arch>\ndebian-binary   "),
            Ok(Installer::LinuxDeb)
        );
        assert!(
            installer_kind(b"\x7fELF\x02\x01\x01\0\0\0\0")
                .unwrap_err()
                .contains("armor <program>")
        );
        // An AppImage of type 2: "AI" 2 in e_ident's padding.
        assert_eq!(
            installer_kind(b"\x7fELF\x02\x01\x01\0AI\x02\0"),
            Ok(Installer::AppImage)
        );
        assert!(installer_kind(b"plain text").is_err());
        assert_eq!(
            installer_kind(&[0xfd, b'7', b'z', b'X', b'Z', 0, 0]),
            Ok(Installer::Archive)
        );
        assert_eq!(installer_kind(&[0x1f, 0x8b, 8, 0]), Ok(Installer::Archive));
        let mut tar = vec![0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(installer_kind(&tar), Ok(Installer::Archive));
    }

    #[test]
    fn names_come_from_the_file() {
        assert_eq!(default_name(Path::new("/x/7z2604-x64.exe")), "7z2604-x64");
        assert_eq!(
            default_name(Path::new("Epic Games (Setup).msi")),
            "Epic-Games--Setup-"
        );
        assert_eq!(default_name(Path::new("/x/.hidden.exe")), "hidden");
        assert_eq!(
            default_name(Path::new("/x/7z2604-linux-x64.tar.xz")),
            "7z2604-linux-x64"
        );
    }

    #[test]
    fn the_data_directory_follows_xdg() {
        let home = Some(OsStr::new("/home/u"));
        assert_eq!(
            data_home(None, home),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(
            data_home(Some(OsStr::new("/d")), home),
            Some(PathBuf::from("/d"))
        );
        assert_eq!(
            data_home(Some(OsStr::new("")), home),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(data_home(None, None), None);
    }

    /// The `Exec` line Hangover's Wine 11.16 wrote for 7-Zip's shortcut
    /// (2026-10-14), read back into arguments.
    #[test]
    fn wine_entries_are_decoded() {
        let exec = r#"env "WINEPREFIX=/home/u/prefix" wine "C:\\\\ProgramData\\\\Microsoft\\\\Windows\\\\Start Menu\\\\Programs\\\\7-Zip\\\\7-Zip File Manager.lnk""#;
        let args = decode_exec(exec).unwrap();
        assert_eq!(
            args,
            [
                "env",
                "WINEPREFIX=/home/u/prefix",
                "wine",
                r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\7-Zip\7-Zip File Manager.lnk"
            ]
        );
        assert_eq!(
            wine_arguments(&args).unwrap(),
            [r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\7-Zip\7-Zip File Manager.lnk"]
        );
        assert_eq!(wine_arguments(&["env".to_string()]), None);
        assert!(decode_exec(r#"wine "open"#).is_err());
    }

    #[test]
    fn exec_lines_round_trip() {
        let args: Vec<String> = [
            "/usr/bin/armor",
            "run",
            "7zip",
            r"C:\Program Files\7-Zip\7zFM.exe",
            "100%",
            "a\"b$c`d",
            "",
            "plain",
        ]
        .map(str::to_string)
        .to_vec();
        let exec = encode_exec(&args);
        assert!(exec.contains("100%%"));
        assert!(exec.contains(r#""C:\\\\Program Files\\\\7-Zip\\\\7zFM.exe""#));
        assert_eq!(decode_exec(&exec).unwrap(), args);
    }

    #[test]
    fn desktop_files_run_armor() {
        let entry = Entry {
            name: "7-Zip File Manager".to_string(),
            arguments: vec![r"C:\x\7-Zip File Manager.lnk".to_string()],
            icon: Some("/apps/7zip/menu/icons/hicolor/48x48/apps/EA23_7zFM.0.png".to_string()),
            wm_class: Some("7zfm.exe".to_string()),
        };
        let text = desktop_file("/usr/bin/armor", "7zip", "windows", &entry);
        let keys = desktop_keys(&text);
        assert_eq!(key(&keys, "Name"), Some("7-Zip File Manager"));
        assert_eq!(key(&keys, "StartupWMClass"), Some("7zfm.exe"));
        assert_eq!(key(&keys, "X-Armor-App"), Some("7zip"));
        assert_eq!(
            decode_exec(key(&keys, "Exec").unwrap()).unwrap(),
            [
                "/usr/bin/armor",
                "run",
                "7zip",
                r"C:\x\7-Zip File Manager.lnk"
            ]
        );
        let line = entry_line("armor-7zip-1.desktop", &entry);
        assert_eq!(
            parse_entries(&format!("{line}\n")),
            [(
                "armor-7zip-1.desktop".to_string(),
                "7-Zip File Manager".to_string(),
                vec![r"C:\x\7-Zip File Manager.lnk".to_string()]
            )]
        );
    }

    #[test]
    fn app_conf_keys() {
        let conf = parse_app_conf(
            "# 7-Zip\nkind = windows\ndxvk = yes\nenv = DXVK_FILTER_DEVICE_NAME=llvmpipe\nenv = A=b=c\n",
        )
        .unwrap();
        assert_eq!(conf.kind, "windows");
        assert!(conf.dxvk);
        assert_eq!(
            conf.env,
            [
                (
                    "DXVK_FILTER_DEVICE_NAME".to_string(),
                    "llvmpipe".to_string()
                ),
                ("A".to_string(), "b=c".to_string())
            ]
        );
        assert_eq!(parse_app_conf("kind = linux\n").unwrap().kind, "linux");
        assert!(parse_app_conf("kind = macos\n").is_err());
        assert!(parse_app_conf("dxvk = maybe\n").is_err());
        assert!(parse_app_conf("env = =x\n").is_err());
        assert!(
            parse_app_conf("colour = red\n")
                .unwrap_err()
                .contains("line 1")
        );
    }

    #[test]
    fn names_come_from_the_shortcuts() {
        let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            name_from_shortcuts(&paths(&[
                "7-Zip/7-Zip File Manager.desktop",
                "7-Zip/7-Zip Help.desktop"
            ])),
            Some("7-Zip".to_string())
        );
        assert_eq!(
            name_from_shortcuts(&paths(&["Epic Games Launcher.desktop"])),
            Some("Epic-Games-Launcher".to_string())
        );
        assert_eq!(
            name_from_shortcuts(&paths(&[
                "Tools/a.desktop",
                "App/b.desktop",
                "App/c.desktop"
            ])),
            Some("App".to_string())
        );
        assert_eq!(
            name_from_shortcuts(&paths(&[
                "hicloud/update_server/Uninstall update_server.desktop",
                "hicloud/Ezviz Studio/Uninstall Uninstall Ezviz Studio.desktop",
                "hicloud/Ezviz Studio/Ezviz Studio.desktop"
            ])),
            Some("Ezviz-Studio".to_string())
        );
        assert_eq!(name_from_shortcuts(&[]), None);
        assert!(is_uninstaller("Uninstall update_server") && !is_uninstaller("Ezviz Studio"));
    }

    #[test]
    fn dxvk_follows_the_gpu() {
        let v3d = vec!["v3d".to_string()];
        let settings = dxvk_settings(&v3d);
        assert_eq!(settings.len(), 1);
        assert_eq!(
            (settings[0].0.as_str(), settings[0].1.as_str()),
            ("DXVK_FILTER_DEVICE_NAME", "llvmpipe")
        );
        assert!(settings[0].2.contains("multiDrawIndirect"));
        // A GPU DXVK accepts, alone or next to a refused one: the hardware.
        assert!(dxvk_settings(&["amdgpu".to_string()]).is_empty());
        assert!(dxvk_settings(&["amdgpu".to_string(), "v3d".to_string()]).is_empty());
        assert_eq!(dxvk_settings(&[]).len(), 1);
    }

    #[test]
    fn app_conf_round_trips() {
        let conf = AppConf {
            kind: "windows".to_string(),
            dxvk: true,
            mono: true,
            vcredist: true,
            env: vec![(
                "DXVK_FILTER_DEVICE_NAME".to_string(),
                "llvmpipe".to_string(),
            )],
        };
        let text = render_app_conf(
            &conf,
            &[("DXVK_FILTER_DEVICE_NAME".to_string(), "the GPU".to_string())],
        );
        assert!(text.contains("# the GPU\nenv = DXVK_FILTER_DEVICE_NAME=llvmpipe\n"));
        assert_eq!(parse_app_conf(&text).unwrap(), conf);
    }

    #[test]
    fn the_desktop_link_becomes_a_directory() {
        let dir = std::env::temp_dir().join(format!("armor-apps-test-{}", std::process::id()));
        let user = dir.join("drive_c/users/someone");
        fs::create_dir_all(&user).unwrap();
        let outside = dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, user.join("Desktop")).unwrap();
        std::os::unix::fs::symlink(&outside, user.join("Documents")).unwrap();
        keep_desktop_inside(&dir).unwrap();
        assert!(user.join("Desktop").is_dir() && !user.join("Desktop").is_symlink());
        assert!(user.join("Documents").is_symlink());
        assert!(outside.is_dir());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_the_desktop_entry_group_counts() {
        let text = "[Desktop Entry]\nName=A\nExec=a\n[Desktop Action x]\nName=B\n";
        let keys = desktop_keys(text);
        assert_eq!(key(&keys, "Name"), Some("A"));
        assert_eq!(keys.len(), 2);
    }
}
