//! `cargo xtask decoder-check <elf>...`: decodes every instruction that
//! objdump finds in the executable sections and compares lengths (the
//! decoder oracle of ADR-0004). Reports coverage, and fails on any length
//! mismatch or on an instruction armor calls invalid but objdump decodes.
//! The length-only decoder (`armor_x86::length`) is checked the same way,
//! on every instruction, and must agree with `decode` wherever that one
//! succeeds; the report counts the instruction-set extensions.
//! Two of objdump's ways of splitting or joining instructions are not
//! mismatches (`same_split`). See docs/testing.md.

use std::collections::BTreeMap;
use std::process::Command;

use armor_x86::{DecodeError, Extension, MAX_INSTRUCTION_LEN, Mode, decode_in, length_in};

use crate::exec;

const OBJDUMP: &str = "x86_64-linux-gnu-objdump";

/// Words objdump prints before the mnemonic.
const PREFIX_WORDS: &[&str] = &[
    "rep", "repz", "repe", "repnz", "repne", "lock", "cs", "ds", "es", "ss", "fs", "gs", "data16",
    "addr32", "notrack", "bnd", "rex", "rex.W",
];

/// One instruction as listed by objdump.
struct Listed {
    addr: u64,
    bytes: Vec<u8>,
    text: String,
}

#[derive(Default)]
struct Report {
    total: usize,
    decoded: usize,
    mismatches: Vec<String>,
    wrongly_invalid: Vec<String>,
    unsupported: BTreeMap<String, (usize, String)>,
    /// Instructions the length-only decoder measures as objdump does.
    lengths: usize,
    length_mismatches: Vec<String>,
    extensions: BTreeMap<Extension, usize>,
}

impl Report {
    fn failed(&self) -> bool {
        !self.mismatches.is_empty()
            || !self.wrongly_invalid.is_empty()
            || !self.length_mismatches.is_empty()
    }
}

pub fn run(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("usage: cargo xtask decoder-check <x86_64 ELF>...".to_string());
    }
    let mut failed = false;
    for path in paths {
        let listing = objdump(path)?;
        let report = check(&listing, mode_of(path)?);
        print_report(path, &report);
        failed |= report.failed();
    }
    if failed {
        Err("the decoder disagrees with objdump".to_string())
    } else {
        Ok(())
    }
}

/// Instructions decoded with objdump's length, and the total checked.
pub(crate) fn decoded_fraction(path: &str) -> Result<(usize, usize), String> {
    let report = check(&objdump(path)?, mode_of(path)?);
    if report.failed() {
        return Err(format!(
            "the decoder disagrees with objdump on {path}: run `cargo xtask decoder-check {path}`"
        ));
    }
    Ok((report.decoded, report.total))
}

fn objdump(path: &str) -> Result<Vec<Listed>, String> {
    let output = exec(Command::new(OBJDUMP).args(["-d", "--insn-width=16", "-M", "intel", path]))?;
    if !output.status.success() {
        return Err(format!(
            "{OBJDUMP} failed on {path}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_line)
        .collect())
}

/// `  401000:\t48 83 ec 08   \tsub    rsp,0x8`
fn parse_line(line: &str) -> Option<Listed> {
    let mut fields = line.split('\t');
    let addr = u64::from_str_radix(fields.next()?.trim().strip_suffix(':')?, 16).ok()?;
    let bytes = fields
        .next()?
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    let text = fields.next().unwrap_or("").trim().to_string();
    (!bytes.is_empty()).then_some(Listed { addr, bytes, text })
}

/// The mode of an ELF file's code: compatibility mode for ELFCLASS32
/// (i386, ADR-0029).
fn mode_of(path: &str) -> Result<Mode, String> {
    let mut header = [0u8; 5];
    let mut file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    std::io::Read::read_exact(&mut file, &mut header).map_err(|e| format!("{path}: {e}"))?;
    Ok(if header[4] == 1 {
        Mode::Compat32
    } else {
        Mode::Long
    })
}

fn check(listing: &[Listed], mode: Mode) -> Report {
    let decode = |bytes: &[u8]| decode_in(bytes, mode);
    let length = |bytes: &[u8]| length_in(bytes, mode);
    let mut report = Report::default();
    for (i, insn) in listing.iter().enumerate() {
        if insn.text.contains("(bad)") {
            continue;
        }
        report.total += 1;
        // The instruction followed by the next ones, as a CPU would see it.
        let window: Vec<u8> = listing[i..]
            .iter()
            .take_while(|next| next.addr - insn.addr < MAX_INSTRUCTION_LEN as u64)
            .flat_map(|next| next.bytes.iter().copied())
            .take(MAX_INSTRUCTION_LEN)
            .collect();
        let describe = || format!("{:#x}: {:02x?}  {}", insn.addr, insn.bytes, insn.text);
        let is_ud = mnemonic(&insn.text).starts_with("ud");
        let measured = length(&window);
        match measured {
            Ok(l) if l.len == insn.bytes.len() || same_split(&listing[i..], l.len, mode) => {
                report.lengths += 1;
                *report.extensions.entry(l.extension).or_insert(0) += 1;
            }
            Err(DecodeError::Invalid) if is_ud || undefined(&insn.bytes, &insn.text, mode) => {
                report.lengths += 1
            }
            Ok(l) => {
                report
                    .length_mismatches
                    .push(format!("{}  (length: {} bytes)", describe(), l.len))
            }
            Err(error) => report
                .length_mismatches
                .push(format!("{}  (length: {error})", describe())),
        }
        if let (Ok(decoded), Ok(l)) = (decode(&window), measured)
            && decoded.len != l.len
        {
            report.length_mismatches.push(format!(
                "{}  (decode: {} bytes, length: {} bytes)",
                describe(),
                decoded.len,
                l.len
            ));
        }
        match decode(&window) {
            Ok(decoded) if decoded.len == insn.bytes.len() => report.decoded += 1,
            Ok(decoded) if same_split(&listing[i..], decoded.len, mode) => report.decoded += 1,
            Ok(decoded) => {
                report
                    .mismatches
                    .push(format!("{}  (armor: {} bytes)", describe(), decoded.len))
            }
            Err(DecodeError::Unsupported(why)) => {
                let entry = report
                    .unsupported
                    .entry(mnemonic(&insn.text))
                    .or_insert((0, why.to_string()));
                entry.0 += 1;
            }
            // ud0/ud1/ud2 exist precisely to raise #UD.
            Err(DecodeError::Invalid) if is_ud || undefined(&insn.bytes, &insn.text, mode) => {
                report.decoded += 1
            }
            Err(error) => report
                .wrongly_invalid
                .push(format!("{}  (armor: {error})", describe())),
        }
    }
    report
}

/// Whether objdump prints a decoding of bytes the CPU rejects with #UD
/// (usually data in a code section, which objdump disassembles anyway):
/// LOCK on an instruction that does not take it (armor decodes the rest
/// with the same length), or a load of cs or of a segment register that
/// does not exist (`mov cs,` / `mov ?,`).
fn undefined(bytes: &[u8], text: &str, mode: Mode) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.first() == Some(&"mov")
        && words
            .get(1)
            .is_some_and(|w| w.starts_with("cs,") || w.starts_with("?,"))
    {
        return true;
    }
    let prefixed_lock = words
        .iter()
        .take_while(|w| PREFIX_WORDS.contains(w))
        .any(|w| *w == "lock");
    if prefixed_lock && let Some(at) = bytes.iter().position(|&b| b == 0xf0) {
        let without: Vec<u8> = bytes[..at]
            .iter()
            .chain(&bytes[at + 1..])
            .copied()
            .collect();
        return length_in(&without, mode).is_ok_and(|l| l.len == without.len());
    }
    false
}

/// Whether armor's `len` for the first of `listing` is the same decoding
/// as objdump's, split differently:
/// - **repeated REX prefixes:** objdump lists a REX byte followed by
///   another prefix as an instruction of its own (`rex.RXB`), while the
///   CPU, and armor, take the prefixes and the next instruction as one
///   (only the last REX counts). Armor's length is then the sum of the
///   listed REX bytes and the instruction after them;
/// - **fwait with an x87 instruction:** objdump joins `9b` and the next
///   control instruction (`fstcw` = `fwait` + `fnstcw`). armor decodes
///   `fwait` alone (1 byte), and the rest must then decode with the
///   remaining length.
fn same_split(listing: &[Listed], len: usize, mode: Mode) -> bool {
    let first = &listing[0];
    if first.text.starts_with("rex") && first.bytes.len() == 1 {
        let mut total = 0;
        for listed in listing {
            total += listed.bytes.len();
            if total == len {
                return true;
            }
            if !(listed.text.starts_with("rex") && listed.bytes.len() == 1) {
                return false;
            }
        }
        return false;
    }
    if first.bytes[0] == 0x9b && len == 1 && first.bytes.len() > 1 {
        let rest = &first.bytes[1..];
        // Measured with the length-only decoder: armor may not execute
        // the x87 instruction (fbstp), and the split is about lengths.
        return length_in(rest, mode).is_ok_and(|insn| insn.len == rest.len());
    }
    false
}

/// The mnemonic with its prefixes, e.g. `rep stos` or `lock cmpxchg`.
fn mnemonic(text: &str) -> String {
    let mut words = Vec::new();
    for word in text.split_whitespace() {
        words.push(word);
        if !PREFIX_WORDS.contains(&word) {
            break;
        }
    }
    words.join(" ")
}

/// How many unsupported mnemonics to list (`DECODER_CHECK_LIMIT`, default 25).
fn limit() -> usize {
    std::env::var("DECODER_CHECK_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(25)
}

fn print_report(path: &str, report: &Report) {
    let pct = if report.total == 0 {
        0.0
    } else {
        report.decoded as f64 * 100.0 / report.total as f64
    };
    println!("{path}");
    println!(
        "  decoded with objdump's length: {}/{} ({pct:.1}%)",
        report.decoded, report.total
    );
    println!("  length mismatches: {}", report.mismatches.len());
    for line in report.mismatches.iter().take(20) {
        println!("    {line}");
    }
    println!(
        "  invalid for armor, valid for objdump: {}",
        report.wrongly_invalid.len()
    );
    for line in report.wrongly_invalid.iter().take(20) {
        println!("    {line}");
    }
    let lpct = if report.total == 0 {
        0.0
    } else {
        report.lengths as f64 * 100.0 / report.total as f64
    };
    println!(
        "  length-only decoder, objdump's length: {}/{} ({lpct:.1}%)",
        report.lengths, report.total
    );
    println!(
        "  length-only decoder mismatches: {}",
        report.length_mismatches.len()
    );
    for line in report.length_mismatches.iter().take(20) {
        println!("    {line}");
    }
    let extensions: Vec<String> = report
        .extensions
        .iter()
        .map(|(e, n)| format!("{e} {n}"))
        .collect();
    println!("  extensions: {}", extensions.join(", "));
    let mut unsupported: Vec<_> = report.unsupported.iter().collect();
    unsupported.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    let count: usize = unsupported.iter().map(|(_, (n, _))| n).sum();
    println!("  not supported yet: {count} instructions");
    for (mnemonic, (n, why)) in unsupported.iter().take(limit()) {
        println!("    {n:6}  {mnemonic:24} ({why})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objdump_lines_are_parsed() {
        let listed = parse_line("  401000:\t48 83 ec 08          \tsub    rsp,0x8").unwrap();
        assert_eq!(listed.addr, 0x401000);
        assert_eq!(listed.bytes, [0x48, 0x83, 0xec, 0x08]);
        assert_eq!(listed.text, "sub    rsp,0x8");
        assert!(parse_line("0000000000401000 <_init>:").is_none());
        assert!(parse_line("Disassembly of section .init:").is_none());
    }

    #[test]
    fn mnemonics_keep_their_prefixes() {
        assert_eq!(mnemonic("rep stos QWORD PTR es:[rdi],rax"), "rep stos");
        assert_eq!(mnemonic("lock cmpxchg QWORD PTR [rdx],rbx"), "lock cmpxchg");
        assert_eq!(mnemonic("movaps xmm0,xmm1"), "movaps");
    }

    #[test]
    fn lengths_are_compared() {
        let listing = vec![
            Listed {
                addr: 0x1000,
                bytes: vec![0x31, 0xc0],
                text: "xor eax,eax".into(),
            },
            Listed {
                addr: 0x1002,
                bytes: vec![0xc3],
                text: "ret".into(),
            },
            Listed {
                addr: 0x1003,
                bytes: vec![0xc5, 0xf8, 0x77],
                text: "vzeroupper".into(),
            },
        ];
        let report = check(&listing, Mode::Long);
        assert_eq!(report.total, 3);
        assert_eq!(report.decoded, 2);
        assert!(report.mismatches.is_empty());
        assert_eq!(report.unsupported["vzeroupper"].0, 1);
    }

    fn listed(addr: u64, bytes: &[u8], text: &str) -> Listed {
        Listed {
            addr,
            bytes: bytes.to_vec(),
            text: text.into(),
        }
    }

    /// objdump's split REX prefixes and fwait-joined x87 instructions
    /// (libcrypto.so.3 and libm.so.6) are the same decoding as armor's; a
    /// real length difference is still one.
    #[test]
    fn objdump_splits_are_not_mismatches() {
        // 4f 47 41 01 c0: three REX prefixes, then add eax, eax.
        let listing = vec![
            listed(0x1000, &[0x4f], "rex.WRXB"),
            listed(0x1001, &[0x47], "rex.RXB"),
            listed(0x1002, &[0x41, 0x01, 0xc0], "add r8d,eax"),
        ];
        let report = check(&listing, Mode::Long);
        assert!(report.mismatches.is_empty(), "{:?}", report.mismatches);
        assert_eq!(report.decoded, 3);
        // 9b d9 7c 24 02: fstcw [rsp+2] = fwait; fnstcw [rsp+2].
        let listing = vec![listed(
            0x2000,
            &[0x9b, 0xd9, 0x7c, 0x24, 0x02],
            "fstcw  WORD PTR [rsp+0x2]",
        )];
        assert!(check(&listing, Mode::Long).mismatches.is_empty());
        // A real difference: objdump says 2 bytes, armor decodes 3.
        let listing = vec![listed(0x3000, &[0x48, 0x01], "add rax,rax")];
        let mut bytes = listing;
        bytes.push(listed(0x3002, &[0xc0], "?"));
        assert_eq!(check(&bytes, Mode::Long).mismatches.len(), 1);
    }
}
