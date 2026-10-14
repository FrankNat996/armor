//! `armor inspect <file>`: what a file is and whether armor can load it.
//!
//! Three levels (docs/vision.md):
//! - **not recognized**: not an executable format armor knows;
//! - **recognized**: format and architecture identified, but armor's loader
//!   would refuse it (the reason names the milestone that adds support);
//! - **loadable**: the loader's own checks (`loader::check`) accept it.
//!
//! "Runs correctly" cannot be decided by looking at a file: it is only
//! established by running the program, and recorded in
//! docs/COMPATIBILITY_MATRIX.md.

use std::collections::BTreeMap;

use armor_elf::{EM_386, EM_AARCH64, EM_X86_64, ET_CORE, ET_DYN, ET_EXEC, ET_REL, Elf, ident};
use armor_linux::loader;
use armor_x86::{Extension, Mode, length_in};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    NotRecognized,
    Recognized,
    Loadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// `(field, value)` lines describing the file.
    pub facts: Vec<(&'static str, String)>,
    pub verdict: Verdict,
    /// Why, for the first two verdicts; what "loadable" means for the last.
    pub reason: String,
}

pub fn inspect(file: &[u8]) -> Report {
    if file.starts_with(b"\x7fELF") {
        inspect_elf(file)
    } else if file.starts_with(AR_MAGIC) {
        inspect_ar(file)
    } else if let Some(machine) = pe_machine(file) {
        inspect_pe(machine)
    } else if let Some(interpreter) = script_interpreter(file) {
        Report {
            facts: vec![
                ("format", "script".to_string()),
                ("interpreter", interpreter.clone()),
            ],
            verdict: Verdict::Recognized,
            reason: format!(
                "a script: run its interpreter ({interpreter}), which must itself be a program \
                 armor (or the host) can run"
            ),
        }
    } else {
        Report {
            facts: vec![("format", "unknown".to_string())],
            verdict: Verdict::NotRecognized,
            reason: "not an ELF executable, a Windows PE executable, a #! script or a Debian \
                     package"
                .to_string(),
        }
    }
}

fn inspect_elf(file: &[u8]) -> Report {
    let id = match ident(file) {
        Ok(id) => id,
        Err(e) => {
            return Report {
                facts: vec![("format", "ELF".to_string())],
                verdict: Verdict::NotRecognized,
                reason: e.to_string(),
            };
        }
    };
    let mut facts = vec![
        (
            "format",
            format!(
                "ELF {}-bit {}",
                if id.class64 { 64 } else { 32 },
                if id.little_endian {
                    "little-endian"
                } else {
                    "big-endian"
                }
            ),
        ),
        ("machine", machine_name(id.machine)),
        ("type", type_name(id.elf_type)),
    ];
    let recognized = |facts, reason: &str| Report {
        facts,
        verdict: Verdict::Recognized,
        reason: reason.to_string(),
    };

    if id.machine == EM_AARCH64 && id.class64 {
        return recognized(
            facts,
            "native AArch64 program: run it directly, armor is not needed",
        );
    }
    let i386 = id.machine == EM_386 && !id.class64;
    if !(i386 || id.machine == EM_X86_64 && id.class64) || !id.little_endian {
        return recognized(
            facts,
            "not an x86 program: armor runs x86-64 and i386 (32-bit x86) code",
        );
    }

    let elf = match Elf::parse(file) {
        Ok(elf) => elf,
        Err(e) => return recognized(facts, &format!("armor's loader refuses it: {e}")),
    };
    let dynamic = match elf.interpreter_path(file) {
        Some(path) => {
            facts.push(("linking", format!("dynamic, interpreter {path}")));
            true
        }
        None => {
            facts.push(("linking", "static".to_string()));
            false
        }
    };
    match elf.needed_libraries(file) {
        Ok(libraries) if !libraries.is_empty() => facts.push(("libraries", libraries.join(", "))),
        Ok(_) => {}
        Err(e) => facts.push(("libraries", format!("unreadable: {e}"))),
    }
    let mode = if i386 { Mode::Compat32 } else { Mode::Long };
    if let Some(code) = instruction_sets(file, mode) {
        facts.extend(code.facts(mode));
    }
    match loader::check(&elf) {
        Ok(()) => Report {
            facts,
            verdict: Verdict::Loadable,
            reason: format!(
                "armor's loader accepts it{}{}. Whether it runs correctly is only known by \
                 running it: see docs/COMPATIBILITY_MATRIX.md",
                if dynamic {
                    "; the interpreter and libraries come from the guest root (ARMOR_ROOT)"
                } else {
                    ""
                },
                if i386 {
                    "; 32-bit code runs in compatibility mode (ADR-0029, M9)"
                } else {
                    ""
                }
            ),
        },
        Err(e) => recognized(facts, &format!("armor's loader refuses it: {e}")),
    }
}

/// What to do about a file armor cannot run (ADR-0028): one concrete
/// action, after `report.reason` has said why.
pub fn suggestion(file: &[u8], report: &Report) -> String {
    if report.verdict == Verdict::NotRecognized {
        return "armor runs ELF executables and #! scripts: check that this is the program \
                itself, not a data file or a wrapper"
            .to_string();
    }
    if pe_machine(file).is_some() {
        return "install it with `armor install FILE` (an installer or the program itself): it \
                runs under Wine for AArch64 with FEX (ADR-0033)"
            .to_string();
    }
    if ar_members(file).is_some_and(|members| is_deb(file, &members)) {
        return "extract the package (`dpkg-deb -x PACKAGE DIR`, without installing it) and run \
                its programs with armor; `armor inspect` tells which ones it can load"
            .to_string();
    }
    match ident(file) {
        Ok(id) if id.machine != EM_X86_64 && id.machine != EM_386 => {
            "armor runs x86-64 and i386 programs: use a build of the program for x86, or for \
             AArch64 to run it natively"
                .to_string()
        }
        _ => "run `armor inspect` on it for the details".to_string(),
    }
}

/// The instructions of a file's executable sections, by extension.
#[derive(Debug, Default, PartialEq, Eq)]
struct Code {
    extensions: BTreeMap<Extension, usize>,
    /// Bytes no instruction starts with (data in code, padding).
    undecodable: usize,
}

impl Code {
    fn facts(&self, mode: Mode) -> Vec<(&'static str, String)> {
        let total: usize = self.extensions.values().sum();
        let list = |implemented: bool| {
            let parts: Vec<String> = self
                .extensions
                .iter()
                .filter(|(e, _)| e.implemented() == implemented)
                .map(|(e, n)| match (e, mode) {
                    (Extension::Base, Mode::Compat32) => format!("x86 base {n}"),
                    _ => format!("{e} {n}"),
                })
                .collect();
            parts.join("; ")
        };
        let mut facts = vec![(
            "code",
            format!(
                "{total} instructions in the executable sections{}",
                if self.undecodable > 0 {
                    format!(", {} bytes that do not decode", self.undecodable)
                } else {
                    String::new()
                }
            ),
        )];
        facts.push(("extensions", list(true)));
        let missing = list(false);
        if !missing.is_empty() {
            facts.push((
                "missing",
                format!(
                    "{missing}: armor's CPU model does not report these extensions, so code \
                     that checks CPUID first (as glibc does) never runs them; code that does \
                     not stops armor"
                ),
            ));
        }
        facts
    }
}

/// A linear sweep of the executable sections (`SHF_EXECINSTR`) of an
/// ELF64 file (`Mode::Long`) or an ELF32 one (`Mode::Compat32`) with the
/// length-only decoder, as objdump disassembles them. None without
/// section headers.
fn instruction_sets(file: &[u8], mode: Mode) -> Option<Code> {
    const SHT_NOBITS: u32 = 8;
    const SHF_EXECINSTR: u64 = 4;
    let u16_at = |at: usize| Some(u16::from_le_bytes(file.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(file.get(at..at + 4)?.try_into().ok()?));
    let u64_at = |at: usize| Some(u64::from_le_bytes(file.get(at..at + 8)?.try_into().ok()?));
    // Header and section header fields: (offset in ELF64, in ELF32).
    let wide = mode == Mode::Long;
    let word_at = |at64: usize, at32: usize| {
        if wide {
            u64_at(at64)
        } else {
            u32_at(at32).map(u64::from)
        }
    };
    let shoff = word_at(0x28, 0x20)? as usize;
    let (entsize, count) = if wide {
        (u16_at(0x3a)?, u16_at(0x3c)?)
    } else {
        (u16_at(0x2e)?, u16_at(0x30)?)
    };
    let (entsize, count) = (usize::from(entsize), usize::from(count));
    if shoff == 0 || count == 0 || entsize < if wide { 0x40 } else { 0x28 } {
        return None;
    }
    let mut code = Code::default();
    for i in 0..count {
        let header = shoff.checked_add(i.checked_mul(entsize)?)?;
        let (kind, flags) = (u32_at(header + 4)?, word_at(header + 8, header + 8)?);
        if flags & SHF_EXECINSTR == 0 || kind == SHT_NOBITS {
            continue;
        }
        let offset = word_at(header + 0x18, header + 0x10)? as usize;
        let size = word_at(header + 0x20, header + 0x14)? as usize;
        let section = file.get(offset..offset.checked_add(size)?)?;
        let mut at = 0;
        while at < section.len() {
            match length_in(&section[at..], mode) {
                Ok(insn) => {
                    *code.extensions.entry(insn.extension).or_insert(0) += 1;
                    at += insn.len;
                }
                Err(_) => {
                    code.undecodable += 1;
                    at += 1;
                }
            }
        }
    }
    Some(code)
}

/// The global header of an `ar` archive: Debian packages and static
/// libraries.
const AR_MAGIC: &[u8] = b"!<arch>\n";

/// One member of an `ar` archive.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ArMember {
    name: String,
    /// Offset of the contents in the file.
    offset: usize,
    size: usize,
}

/// The members of an `ar` archive: after the global header, each one has
/// a 60-byte header (name, dates, owner, mode, decimal size, "`\n") and
/// its contents, padded to an even length. GNU `ar` ends names with '/'.
/// None if the file is not a well-formed archive.
fn ar_members(file: &[u8]) -> Option<Vec<ArMember>> {
    let mut at = AR_MAGIC.len();
    file.starts_with(AR_MAGIC).then_some(())?;
    let mut members = Vec::new();
    while at < file.len() {
        let header = file.get(at..at.checked_add(60)?)?;
        if header[58..60] != *b"`\n" {
            return None;
        }
        let text = |range: std::ops::Range<usize>| {
            String::from_utf8_lossy(&header[range]).trim().to_string()
        };
        let name = text(0..16).trim_end_matches('/').to_string();
        let size: usize = text(48..58).parse().ok()?;
        let offset = at + 60;
        file.get(offset..offset.checked_add(size)?)?;
        members.push(ArMember { name, offset, size });
        at = offset + size + size % 2;
    }
    Some(members)
}

/// A Debian package is an `ar` archive whose first member is
/// `debian-binary` (the format version), as dpkg-deb(1) describes.
fn is_deb(file: &[u8], members: &[ArMember]) -> bool {
    members.first().is_some_and(|m| m.name == "debian-binary")
        && file
            .get(members[0].offset..members[0].offset + members[0].size)
            .is_some_and(|v| v.starts_with(b"2."))
}

fn inspect_ar(file: &[u8]) -> Report {
    let Some(members) = ar_members(file) else {
        return Report {
            facts: vec![("format", "ar archive".to_string())],
            verdict: Verdict::NotRecognized,
            reason: "a malformed ar archive".to_string(),
        };
    };
    if !is_deb(file, &members) {
        return Report {
            facts: vec![
                ("format", "ar archive (a static library?)".to_string()),
                ("members", members.len().to_string()),
            ],
            verdict: Verdict::NotRecognized,
            reason: "an ar archive, not an executable".to_string(),
        };
    }
    let version = String::from_utf8_lossy(&file[members[0].offset..][..members[0].size])
        .trim()
        .to_string();
    let compression = |name: &str| match name.rsplit('.').next() {
        Some("gz") => " (gzip)",
        Some("xz") => " (xz)",
        Some("zst") => " (zstd)",
        Some("bz2") => " (bzip2)",
        Some("tar") => " (uncompressed)",
        _ => "",
    };
    let list: Vec<String> = members
        .iter()
        .map(|m| format!("{}{}, {} bytes", m.name, compression(&m.name), m.size))
        .collect();
    let data = members
        .iter()
        .find(|m| m.name.starts_with("data.tar"))
        .map_or("its data archive".to_string(), |m| m.name.clone());
    Report {
        facts: vec![
            ("format", format!("Debian package, format {version}")),
            ("members", list.join("; ")),
        ],
        verdict: Verdict::Recognized,
        reason: format!(
            "a Debian package, not a program: the programs are inside {data}, which armor \
             does not unpack"
        ),
    }
}

/// Machine of a Windows PE executable: "MZ", then "PE\0\0" at the offset
/// stored at 0x3c, then the COFF machine field.
pub(crate) fn pe_machine(file: &[u8]) -> Option<u16> {
    if !file.starts_with(b"MZ") {
        return None;
    }
    let pe_offset = u32::from_le_bytes(file.get(0x3c..0x40)?.try_into().ok()?) as usize;
    let header = file.get(pe_offset..pe_offset.checked_add(6)?)?;
    (header[..4] == *b"PE\0\0").then(|| u16::from_le_bytes([header[4], header[5]]))
}

fn inspect_pe(machine: u16) -> Report {
    // COFF machine values from the PE/COFF specification.
    let name = match machine {
        0x8664 => "x86-64",
        0x014c => "x86 (i386)",
        0xaa64 => "AArch64",
        0x01c4 => "ARM (32-bit, Thumb-2)",
        _ => "unknown",
    };
    Report {
        facts: vec![
            ("format", "Windows PE".to_string()),
            ("machine", format!("{name} ({machine:#06x})")),
        ],
        verdict: Verdict::Recognized,
        reason: "Windows program: runs under Wine for AArch64 with an x86 translator, through \
                 `armor install` (ADR-0033)"
            .to_string(),
    }
}

/// The interpreter of a `#!` script, up to the end of the first line.
fn script_interpreter(file: &[u8]) -> Option<String> {
    let rest = file.strip_prefix(b"#!")?;
    let line = rest.split(|&b| b == b'\n').next()?;
    let text = String::from_utf8_lossy(line).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn machine_name(machine: u16) -> String {
    let name = match machine {
        2 => "SPARC",
        EM_386 => "x86 (i386)",
        8 => "MIPS",
        20 => "PowerPC",
        21 => "PowerPC64",
        22 => "S390",
        40 => "ARM (32-bit)",
        43 => "SPARC V9",
        EM_X86_64 => "x86-64",
        EM_AARCH64 => "AArch64",
        243 => "RISC-V",
        258 => "LoongArch",
        _ => "unknown",
    };
    format!("{name} ({machine})")
}

fn type_name(elf_type: u16) -> String {
    match elf_type {
        ET_REL => "relocatable object (ET_REL)".to_string(),
        ET_EXEC => "executable at a fixed address (ET_EXEC)".to_string(),
        ET_DYN => "position-independent executable or shared library (ET_DYN)".to_string(),
        ET_CORE => "core dump (ET_CORE)".to_string(),
        other => format!("unknown ({other})"),
    }
}

pub fn render(path: &str, report: &Report) -> String {
    let mut out = format!("{:<12} {path}\n", "file:");
    for (field, value) in &report.facts {
        out.push_str(&format!("{:<12} {value}\n", format!("{field}:")));
    }
    let verdict = match report.verdict {
        Verdict::NotRecognized => "not recognized",
        Verdict::Recognized => "recognized, not loadable",
        Verdict::Loadable => "loadable",
    };
    out.push_str(&format!("{:<12} {verdict}\n", "verdict:"));
    out.push_str(&format!("{:<12} {}\n", "why:", report.reason));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ELF header (class, machine, type) with no program headers.
    fn elf_header(class64: bool, machine: u16, elf_type: u16) -> Vec<u8> {
        let mut f = vec![0u8; 64];
        f[..4].copy_from_slice(b"\x7fELF");
        f[4] = if class64 { 2 } else { 1 };
        f[5] = 1;
        f[6] = 1;
        f[16..18].copy_from_slice(&elf_type.to_le_bytes());
        f[18..20].copy_from_slice(&machine.to_le_bytes());
        // e_phentsize for ELF64, with zero program headers.
        f[54..56].copy_from_slice(&56u16.to_le_bytes());
        f
    }

    fn pe(machine: u16) -> Vec<u8> {
        let mut f = vec![0u8; 0x90];
        f[..2].copy_from_slice(b"MZ");
        f[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        f[0x80..0x84].copy_from_slice(b"PE\0\0");
        f[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        f
    }

    #[test]
    fn x86_64_static_executable_is_loadable() {
        let report = inspect(&elf_header(true, EM_X86_64, ET_EXEC));
        assert_eq!(report.verdict, Verdict::Loadable);
        assert!(report.facts.contains(&("linking", "static".to_string())));
    }

    #[test]
    fn position_independent_executable_is_loadable() {
        let report = inspect(&elf_header(true, EM_X86_64, ET_DYN));
        assert_eq!(report.verdict, Verdict::Loadable);
    }

    #[test]
    fn i386_executables_are_loadable() {
        // ELF32: e_phentsize 32 at 42, zero program headers.
        let mut f = elf_header(false, EM_386, ET_EXEC);
        f.truncate(52);
        f[42..44].copy_from_slice(&32u16.to_le_bytes());
        let report = inspect(&f);
        assert_eq!(report.verdict, Verdict::Loadable, "{}", report.reason);
        assert!(report.reason.contains("compatibility mode"));
        assert!(
            report
                .facts
                .contains(&("format", "ELF 32-bit little-endian".to_string()))
        );
    }

    #[test]
    fn other_architectures_are_recognized_with_a_reason() {
        let native = inspect(&elf_header(true, EM_AARCH64, ET_DYN));
        assert!(native.reason.contains("native AArch64"));
        let riscv = inspect(&elf_header(true, 243, ET_EXEC));
        assert!(riscv.reason.contains("not an x86 program"));
        assert!(
            riscv
                .facts
                .contains(&("machine", "RISC-V (243)".to_string()))
        );
    }

    #[test]
    fn relocatable_objects_are_refused_by_the_loader() {
        let report = inspect(&elf_header(true, EM_X86_64, ET_REL));
        assert_eq!(report.verdict, Verdict::Recognized);
        assert!(
            report.reason.contains("not an executable"),
            "{}",
            report.reason
        );
    }

    #[test]
    fn windows_pe_files_are_recognized() {
        let report = inspect(&pe(0x8664));
        assert_eq!(report.verdict, Verdict::Recognized);
        assert!(
            report
                .facts
                .contains(&("machine", "x86-64 (0x8664)".to_string()))
        );
        assert!(report.reason.contains("Wine"));
        // "MZ" without a PE header is not a PE file.
        assert_eq!(inspect(b"MZ just text").verdict, Verdict::NotRecognized);
    }

    #[test]
    fn scripts_and_unknown_files() {
        let script = inspect(b"#!/bin/sh -e\necho hi\n");
        assert_eq!(script.verdict, Verdict::Recognized);
        assert!(
            script
                .facts
                .contains(&("interpreter", "/bin/sh -e".to_string()))
        );
        assert_eq!(inspect(b"hello").verdict, Verdict::NotRecognized);
        assert_eq!(inspect(b"").verdict, Verdict::NotRecognized);
        assert_eq!(inspect(b"\x7fELF").verdict, Verdict::NotRecognized);
    }

    #[test]
    fn executable_sections_are_swept_by_extension() {
        // An ELF64 header with two section headers: an empty one and an
        // executable section with `nop; addps xmm0, xmm1; vzeroupper`,
        // then a byte that starts no instruction (06, removed in 64-bit
        // mode).
        let code = [0x90, 0x0f, 0x58, 0xc1, 0xc5, 0xf8, 0x77, 0x06];
        let mut f = elf_header(true, EM_X86_64, ET_EXEC);
        let shoff = f.len();
        f[0x28..0x30].copy_from_slice(&(shoff as u64).to_le_bytes());
        f[0x3a..0x3c].copy_from_slice(&0x40u16.to_le_bytes());
        f[0x3c..0x3e].copy_from_slice(&2u16.to_le_bytes());
        let mut section = vec![0u8; 0x40];
        section[4..8].copy_from_slice(&1u32.to_le_bytes());
        section[8..16].copy_from_slice(&6u64.to_le_bytes());
        let offset = shoff + 0x80;
        section[0x18..0x20].copy_from_slice(&(offset as u64).to_le_bytes());
        section[0x20..0x28].copy_from_slice(&(code.len() as u64).to_le_bytes());
        f.extend([0u8; 0x40]);
        f.extend(section);
        f.extend(code);
        let swept = instruction_sets(&f, Mode::Long).unwrap();
        assert_eq!(swept.undecodable, 1);
        assert_eq!(
            swept.extensions.into_iter().collect::<Vec<_>>(),
            [
                (Extension::Base, 1),
                (Extension::Sse, 1),
                (Extension::Vex, 1)
            ]
        );
        let facts = inspect(&f).facts;
        assert!(
            facts
                .iter()
                .any(|(k, v)| *k == "missing" && v.starts_with("VEX")),
            "{facts:?}"
        );
    }

    /// An `ar` archive with these members, as GNU ar writes it.
    fn ar(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut f = AR_MAGIC.to_vec();
        for (name, data) in members {
            let header = format!(
                "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                format!("{name}/"),
                0,
                0,
                0,
                100644,
                data.len()
            );
            f.extend(header.as_bytes());
            f.extend(*data);
            if data.len() % 2 == 1 {
                f.push(b'\n');
            }
        }
        f
    }

    #[test]
    fn debian_packages_are_recognized() {
        let deb = ar(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar.gz", b"abc"),
            ("data.tar.xz", b"defgh"),
        ]);
        let report = inspect(&deb);
        assert_eq!(report.verdict, Verdict::Recognized);
        assert!(
            report
                .facts
                .contains(&("format", "Debian package, format 2.0".to_string()))
        );
        assert!(
            report.facts.contains(&(
                "members",
                "debian-binary, 4 bytes; control.tar.gz (gzip), 3 bytes; data.tar.xz (xz), \
                 5 bytes"
                    .to_string()
            )),
            "{:?}",
            report.facts
        );
        assert!(report.reason.contains("data.tar.xz"));
        assert!(suggestion(&deb, &report).contains("dpkg-deb -x"));
        // A static library, and a truncated archive.
        let library = ar(&[("hello.o", b"\x7fELF")]);
        assert_eq!(inspect(&library).verdict, Verdict::NotRecognized);
        assert!(inspect(&library).reason.contains("not an executable"));
        assert_eq!(
            inspect(&deb[..deb.len() - 2]).reason,
            "a malformed ar archive"
        );
    }

    #[test]
    fn rendering_lists_facts_and_verdict() {
        let text = render("prog", &inspect(&elf_header(true, EM_X86_64, ET_EXEC)));
        assert!(text.starts_with("file:        prog\n"), "{text}");
        assert!(text.contains("verdict:     loadable\n"), "{text}");
    }
}
