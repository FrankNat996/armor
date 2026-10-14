//! ELF parsing: x86_64 and i386 guest executables for the loader
//! (`Elf::parse`), and any ELF file for `armor inspect` (`ident`,
//! `Elf::parse_any`). Both classes: ELF64 and ELF32 (ADR-0029).
//!
//! Pure code: reads bytes, performs no system I/O. See docs/architecture.md.

#![forbid(unsafe_code)]

use std::fmt;

pub const ET_REL: u16 = 1;
pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;
pub const ET_CORE: u16 = 4;

pub const EM_386: u16 = 3;
pub const EM_X86_64: u16 = 62;
pub const EM_AARCH64: u16 = 183;

pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP: u32 = 3;
pub const PT_PHDR: u32 = 6;

const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;
const DT_STRTAB: i64 = 5;
const DT_STRSZ: i64 = 10;

pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;
const EHDR32_SIZE: usize = 52;
const PHDR32_SIZE: usize = 32;

/// The identification fields every ELF file has at the same offsets,
/// whatever its class (32/64-bit), byte order or machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElfIdent {
    pub class64: bool,
    pub little_endian: bool,
    pub elf_type: u16,
    pub machine: u16,
}

/// Reads the identification of any ELF file.
pub fn ident(file: &[u8]) -> Result<ElfIdent, ElfError> {
    let head = file
        .get(..20)
        .ok_or(ElfError::Truncated { what: "ELF header" })?;
    if head[..4] != *b"\x7fELF" {
        return Err(ElfError::NotElf);
    }
    let little_endian = head[5] == 1;
    let field = |at: usize| {
        let bytes = [head[at], head[at + 1]];
        if little_endian {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        }
    };
    Ok(ElfIdent {
        class64: head[4] == 2,
        little_endian,
        elf_type: field(16),
        machine: field(18),
    })
}

/// A parsed little-endian ELF file (ELF64 or ELF32): header fields plus
/// program headers, with 32-bit fields widened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elf {
    /// ELFCLASS64; false for ELFCLASS32.
    pub class64: bool,
    pub elf_type: u16,
    pub machine: u16,
    pub entry: u64,
    pub phoff: u64,
    pub program_headers: Vec<ProgramHeader>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramHeader {
    pub p_type: u32,
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElfError {
    /// The file is shorter than a structure it must contain.
    Truncated {
        what: &'static str,
    },
    NotElf,
    /// Valid ELF, but not something armor runs (class, endianness, machine...).
    Unsupported(String),
    /// Internally inconsistent headers.
    Malformed(String),
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElfError::Truncated { what } => write!(f, "file too short for the {what}"),
            ElfError::NotElf => write!(f, "not an ELF file"),
            ElfError::Unsupported(why) => write!(f, "unsupported ELF: {why}"),
            ElfError::Malformed(why) => write!(f, "malformed ELF: {why}"),
        }
    }
}

impl std::error::Error for ElfError {}

impl ProgramHeader {
    pub fn is_load(&self) -> bool {
        self.p_type == PT_LOAD
    }

    /// The bytes of this segment that come from the file.
    pub fn file_bytes<'a>(&self, file: &'a [u8]) -> &'a [u8] {
        // `Elf::parse` checked that the range lies inside the file.
        &file[self.offset as usize..(self.offset + self.filesz) as usize]
    }
}

impl Elf {
    /// Parses an executable for the loader: `parse_any` plus the machine
    /// and type checks. x86_64 (ELF64) or i386 (ELF32).
    pub fn parse(file: &[u8]) -> Result<Elf, ElfError> {
        let elf = Elf::parse_any(file)?;
        match (elf.machine, elf.class64) {
            (EM_X86_64, true) | (EM_386, false) => {}
            (machine, class64) => {
                return Err(ElfError::Unsupported(format!(
                    "machine {machine} in a {}-bit ELF, expected x86_64 ({EM_X86_64}, \
                     64-bit) or i386 ({EM_386}, 32-bit)",
                    if class64 { 64 } else { 32 }
                )));
            }
        }
        if elf.elf_type != ET_EXEC && elf.elf_type != ET_DYN {
            return Err(ElfError::Unsupported(format!(
                "ELF type {} is not an executable",
                elf.elf_type
            )));
        }
        Ok(elf)
    }

    /// Parses and validates the header and program headers of any
    /// little-endian ELF file, 64- or 32-bit, whatever its machine and type.
    pub fn parse_any(file: &[u8]) -> Result<Elf, ElfError> {
        let head = file
            .get(..EHDR32_SIZE)
            .ok_or(ElfError::Truncated { what: "ELF header" })?;
        if head[..4] != *b"\x7fELF" {
            return Err(ElfError::NotElf);
        }
        let class64 = match head[4] {
            1 => false,
            2 => true,
            class => return Err(ElfError::Unsupported(format!("ELF class {class}"))),
        };
        if head[5] != 1 {
            return Err(ElfError::Unsupported("not little-endian".into()));
        }
        if head[6] != 1 {
            return Err(ElfError::Unsupported(format!("ELF version {}", head[6])));
        }
        let ehdr_size = if class64 { EHDR_SIZE } else { EHDR32_SIZE };
        let ehdr = file
            .get(..ehdr_size)
            .ok_or(ElfError::Truncated { what: "ELF header" })?;

        let r = Reader(ehdr);
        let elf_type = r.u16(16);
        let machine = r.u16(18);
        let (entry, phoff, phentsize, phnum) = if class64 {
            (r.u64(24), r.u64(32), r.u16(54), r.u16(56))
        } else {
            (
                u64::from(r.u32(24)),
                u64::from(r.u32(28)),
                r.u16(42),
                r.u16(44),
            )
        };
        let phdr_size = if class64 { PHDR_SIZE } else { PHDR32_SIZE };

        if usize::from(phentsize) != phdr_size && phnum != 0 {
            return Err(ElfError::Malformed(format!(
                "program header size {phentsize}, expected {phdr_size}"
            )));
        }

        let table_len = u64::from(phnum) * phdr_size as u64;
        let table = range(file, phoff, table_len).ok_or(ElfError::Truncated {
            what: "program header table",
        })?;
        let program_headers: Vec<ProgramHeader> = table
            .chunks_exact(phdr_size)
            .map(|raw| {
                let r = Reader(raw);
                if class64 {
                    ProgramHeader {
                        p_type: r.u32(0),
                        flags: r.u32(4),
                        offset: r.u64(8),
                        vaddr: r.u64(16),
                        filesz: r.u64(32),
                        memsz: r.u64(40),
                        align: r.u64(48),
                    }
                } else {
                    // Elf32_Phdr: type, offset, vaddr, paddr, filesz,
                    // memsz, flags, align.
                    ProgramHeader {
                        p_type: r.u32(0),
                        offset: u64::from(r.u32(4)),
                        vaddr: u64::from(r.u32(8)),
                        filesz: u64::from(r.u32(16)),
                        memsz: u64::from(r.u32(20)),
                        flags: r.u32(24),
                        align: u64::from(r.u32(28)),
                    }
                }
            })
            .collect();

        for (i, ph) in program_headers.iter().enumerate() {
            if !ph.is_load() {
                continue;
            }
            if ph.filesz > ph.memsz {
                return Err(ElfError::Malformed(format!(
                    "segment {i}: file size {:#x} exceeds memory size {:#x}",
                    ph.filesz, ph.memsz
                )));
            }
            if range(file, ph.offset, ph.filesz).is_none() {
                return Err(ElfError::Truncated {
                    what: "contents of a PT_LOAD segment",
                });
            }
            // A 32-bit ELF must also end within 4 GiB.
            if ph
                .vaddr
                .checked_add(ph.memsz)
                .is_none_or(|end| !class64 && end > 1 << 32)
            {
                return Err(ElfError::Malformed(format!(
                    "segment {i}: address range overflows"
                )));
            }
        }

        Ok(Elf {
            class64,
            elf_type,
            machine,
            entry,
            phoff,
            program_headers,
        })
    }

    pub fn load_segments(&self) -> impl Iterator<Item = &ProgramHeader> {
        self.program_headers.iter().filter(|ph| ph.is_load())
    }

    pub fn interpreter(&self) -> Option<&ProgramHeader> {
        self.program_headers
            .iter()
            .find(|ph| ph.p_type == PT_INTERP)
    }

    /// The path in `PT_INTERP` (the dynamic loader), if any.
    pub fn interpreter_path(&self, file: &[u8]) -> Option<String> {
        let ph = self.interpreter()?;
        let bytes = range(file, ph.offset, ph.filesz)?;
        let path = bytes.split(|&b| b == 0).next()?;
        Some(String::from_utf8_lossy(path).into_owned())
    }

    /// The `DT_NEEDED` libraries of the dynamic section, in order. Empty for
    /// a file without `PT_DYNAMIC`.
    pub fn needed_libraries(&self, file: &[u8]) -> Result<Vec<String>, ElfError> {
        let Some(dynamic) = self
            .program_headers
            .iter()
            .find(|ph| ph.p_type == PT_DYNAMIC)
        else {
            return Ok(Vec::new());
        };
        let table = range(file, dynamic.offset, dynamic.filesz).ok_or(ElfError::Truncated {
            what: "dynamic section",
        })?;
        let mut needed = Vec::new();
        let (mut strtab, mut strsz) = (None, None);
        // Elf64_Dyn: two 8-byte fields; Elf32_Dyn: two 4-byte fields.
        let entry_size = if self.class64 { 16 } else { 8 };
        for entry in table.chunks_exact(entry_size) {
            let r = Reader(entry);
            let (tag, value) = if self.class64 {
                (r.u64(0) as i64, r.u64(8))
            } else {
                (i64::from(r.u32(0) as i32), u64::from(r.u32(4)))
            };
            match tag {
                DT_NULL => break,
                DT_NEEDED => needed.push(value),
                DT_STRTAB => strtab = Some(value),
                DT_STRSZ => strsz = Some(value),
                _ => {}
            }
        }
        if needed.is_empty() {
            return Ok(Vec::new());
        }
        let malformed = || ElfError::Malformed("dynamic string table not found".into());
        let strtab_offset = self
            .file_offset(strtab.ok_or_else(malformed)?)
            .ok_or_else(malformed)?;
        let strings = range(file, strtab_offset, strsz.ok_or_else(malformed)?).ok_or(
            ElfError::Truncated {
                what: "dynamic string table",
            },
        )?;
        needed
            .into_iter()
            .map(|offset| {
                let name = strings
                    .get(offset as usize..)
                    .and_then(|rest| rest.split(|&b| b == 0).next())
                    .ok_or_else(|| {
                        ElfError::Malformed("library name outside the string table".into())
                    })?;
                Ok(String::from_utf8_lossy(name).into_owned())
            })
            .collect()
    }

    /// File offset of the byte loaded at `vaddr`, through the `PT_LOAD` table.
    fn file_offset(&self, vaddr: u64) -> Option<u64> {
        self.load_segments()
            .find(|ph| vaddr >= ph.vaddr && vaddr - ph.vaddr < ph.filesz)
            .map(|ph| ph.offset + (vaddr - ph.vaddr))
    }

    /// Guest address of the program header table, for `AT_PHDR`: taken from
    /// `PT_PHDR` if present, otherwise from the `PT_LOAD` segment that
    /// contains the table in the file.
    pub fn phdr_address(&self) -> Option<u64> {
        if let Some(ph) = self.program_headers.iter().find(|ph| ph.p_type == PT_PHDR) {
            return Some(ph.vaddr);
        }
        let table_len = self.program_headers.len() as u64 * self.program_header_size();
        self.load_segments()
            .find(|ph| ph.offset <= self.phoff && self.phoff + table_len <= ph.offset + ph.filesz)
            .map(|ph| ph.vaddr + (self.phoff - ph.offset))
    }

    /// The size of a program header (`AT_PHENT`).
    pub fn program_header_size(&self) -> u64 {
        if self.class64 { PHDR_SIZE } else { PHDR32_SIZE }
            .try_into()
            .unwrap()
    }
}

/// `file[offset..offset + len]`, or `None` if it does not fit.
fn range(file: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    file.get(start..end)
}

/// Little-endian field reader over a slice already known to be long enough.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn u16(&self, at: usize) -> u16 {
        u16::from_le_bytes(self.0[at..at + 2].try_into().unwrap())
    }
    fn u32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.0[at..at + 4].try_into().unwrap())
    }
    fn u64(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.0[at..at + 8].try_into().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal ELF64 x86_64 file with the given program headers,
    /// placed right after the ELF header, followed by `payload`.
    fn build(elf_type: u16, phdrs: &[ProgramHeader], payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; EHDR_SIZE];
        f[..4].copy_from_slice(b"\x7fELF");
        f[4] = 2;
        f[5] = 1;
        f[6] = 1;
        f[16..18].copy_from_slice(&elf_type.to_le_bytes());
        f[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
        f[24..32].copy_from_slice(&0x401000u64.to_le_bytes());
        f[32..40].copy_from_slice(&(EHDR_SIZE as u64).to_le_bytes());
        f[54..56].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
        f[56..58].copy_from_slice(&(phdrs.len() as u16).to_le_bytes());
        for ph in phdrs {
            let mut raw = [0u8; PHDR_SIZE];
            raw[0..4].copy_from_slice(&ph.p_type.to_le_bytes());
            raw[4..8].copy_from_slice(&ph.flags.to_le_bytes());
            raw[8..16].copy_from_slice(&ph.offset.to_le_bytes());
            raw[16..24].copy_from_slice(&ph.vaddr.to_le_bytes());
            raw[24..32].copy_from_slice(&ph.vaddr.to_le_bytes());
            raw[32..40].copy_from_slice(&ph.filesz.to_le_bytes());
            raw[40..48].copy_from_slice(&ph.memsz.to_le_bytes());
            raw[48..56].copy_from_slice(&ph.align.to_le_bytes());
            f.extend_from_slice(&raw);
        }
        f.extend_from_slice(payload);
        f
    }

    fn load(offset: u64, vaddr: u64, filesz: u64, memsz: u64, flags: u32) -> ProgramHeader {
        ProgramHeader {
            p_type: PT_LOAD,
            flags,
            offset,
            vaddr,
            filesz,
            memsz,
            align: 0x1000,
        }
    }

    #[test]
    fn parses_header_and_load_segments() {
        // One header: the segment covers the ELF header and the table (0x78 bytes).
        let headers = [load(0, 0x400000, 0x78, 0x78, PF_R)];
        let file = build(ET_EXEC, &headers, &[]);
        let elf = Elf::parse(&file).unwrap();
        assert_eq!(elf.elf_type, ET_EXEC);
        assert_eq!(elf.entry, 0x401000);
        assert_eq!(elf.load_segments().count(), 1);
        assert_eq!(elf.program_headers[0], headers[0]);
        assert_eq!(elf.interpreter(), None);
    }

    #[test]
    fn phdr_address_comes_from_the_segment_containing_the_table() {
        let file = build(ET_EXEC, &[load(0, 0x400000, 0x78, 0x78, PF_R)], &[]);
        assert_eq!(Elf::parse(&file).unwrap().phdr_address(), Some(0x400040));
    }

    #[test]
    fn segment_file_bytes_are_sliced_from_the_file() {
        let payload = b"code";
        let offset = (EHDR_SIZE + PHDR_SIZE) as u64;
        let file = build(
            ET_EXEC,
            &[load(offset, 0x401000, 4, 8, PF_R | PF_X)],
            payload,
        );
        let elf = Elf::parse(&file).unwrap();
        assert_eq!(elf.program_headers[0].file_bytes(&file), payload);
    }

    #[test]
    fn rejects_non_elf_and_wrong_machine() {
        assert_eq!(Elf::parse(&[0u8; 64]), Err(ElfError::NotElf));
        let mut file = build(ET_EXEC, &[], &[]);
        file[18] = 183; // EM_AARCH64
        assert!(matches!(Elf::parse(&file), Err(ElfError::Unsupported(_))));
    }

    #[test]
    fn rejects_truncated_files() {
        assert!(matches!(
            Elf::parse(b"\x7fELF"),
            Err(ElfError::Truncated { .. })
        ));
        let mut file = build(ET_EXEC, &[load(0, 0x400000, 0x10, 0x10, PF_R)], &[]);
        file.truncate(EHDR_SIZE + 10);
        assert!(matches!(Elf::parse(&file), Err(ElfError::Truncated { .. })));
    }

    #[test]
    fn rejects_segments_outside_the_file_or_with_filesz_above_memsz() {
        let beyond = build(ET_EXEC, &[load(0, 0x400000, 0x10000, 0x10000, PF_R)], &[]);
        assert!(matches!(
            Elf::parse(&beyond),
            Err(ElfError::Truncated { .. })
        ));
        let inverted = build(ET_EXEC, &[load(0, 0x400000, 0x20, 0x10, PF_R)], &[]);
        assert!(matches!(Elf::parse(&inverted), Err(ElfError::Malformed(_))));
    }

    #[test]
    fn ident_reads_any_class_and_machine() {
        let mut elf32 = vec![0u8; 52];
        elf32[..4].copy_from_slice(b"\x7fELF");
        elf32[4] = 1; // ELFCLASS32
        elf32[5] = 1;
        elf32[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
        elf32[18..20].copy_from_slice(&EM_386.to_le_bytes());
        assert_eq!(
            ident(&elf32),
            Ok(ElfIdent {
                class64: false,
                little_endian: true,
                elf_type: ET_EXEC,
                machine: EM_386
            })
        );
        assert_eq!(
            ident(b"MZ\x90\x00"),
            Err(ElfError::Truncated { what: "ELF header" })
        );
        assert_eq!(ident(&[0u8; 64]), Err(ElfError::NotElf));
    }

    #[test]
    fn parse_any_accepts_other_machines_but_parse_does_not() {
        let mut file = build(ET_EXEC, &[load(0, 0x400000, 0x78, 0x78, PF_R)], &[]);
        file[18..20].copy_from_slice(&EM_AARCH64.to_le_bytes());
        assert_eq!(Elf::parse_any(&file).unwrap().machine, EM_AARCH64);
        assert!(matches!(Elf::parse(&file), Err(ElfError::Unsupported(_))));
    }

    /// A dynamically linked executable: PT_LOAD over the whole file,
    /// PT_INTERP and PT_DYNAMIC with two DT_NEEDED libraries.
    fn dynamic_executable() -> Vec<u8> {
        // Layout after the headers (3 program headers at 0x40):
        //   0x100: "/lib64/ld.so\0"   (PT_INTERP)
        //   0x110: "\0libc.so.6\0libm.so.6\0"   (string table, 21 bytes)
        //   0x130: dynamic entries
        let base = 0x400000;
        let mut tail = vec![0u8; 0x100 - (EHDR_SIZE + 3 * PHDR_SIZE)];
        let mut put = |data: &[u8]| tail.extend_from_slice(data);
        let mut interp = b"/lib64/ld.so\0".to_vec();
        interp.resize(0x10, 0);
        put(&interp);
        let mut strings = b"\0libc.so.6\0libm.so.6\0".to_vec();
        strings.resize(0x20, 0);
        put(&strings);
        for (tag, value) in [
            (DT_NEEDED, 1u64),
            (DT_NEEDED, 11),
            (DT_STRTAB, base + 0x110),
            (DT_STRSZ, 21),
            (DT_NULL, 0),
        ] {
            put(&tag.to_le_bytes());
            put(&value.to_le_bytes());
        }
        let total = (0x130 + 5 * 16) as u64;
        let headers = [
            load(0, base, total, total, PF_R),
            ProgramHeader {
                p_type: PT_INTERP,
                flags: PF_R,
                offset: 0x100,
                vaddr: base + 0x100,
                filesz: 13,
                memsz: 13,
                align: 1,
            },
            ProgramHeader {
                p_type: PT_DYNAMIC,
                flags: PF_R,
                offset: 0x130,
                vaddr: base + 0x130,
                filesz: 5 * 16,
                memsz: 5 * 16,
                align: 8,
            },
        ];
        build(ET_DYN, &headers, &tail)
    }

    #[test]
    fn interpreter_and_needed_libraries_are_read() {
        let file = dynamic_executable();
        let elf = Elf::parse(&file).unwrap();
        assert_eq!(elf.interpreter_path(&file).as_deref(), Some("/lib64/ld.so"));
        assert_eq!(
            elf.needed_libraries(&file).unwrap(),
            ["libc.so.6", "libm.so.6"]
        );
    }

    /// The ELF32 i386 version of `build`.
    fn build32(elf_type: u16, phdrs: &[ProgramHeader], payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; EHDR32_SIZE];
        f[..4].copy_from_slice(b"\x7fELF");
        f[4] = 1;
        f[5] = 1;
        f[6] = 1;
        f[16..18].copy_from_slice(&elf_type.to_le_bytes());
        f[18..20].copy_from_slice(&EM_386.to_le_bytes());
        f[24..28].copy_from_slice(&0x0804_9000u32.to_le_bytes());
        f[28..32].copy_from_slice(&(EHDR32_SIZE as u32).to_le_bytes());
        f[42..44].copy_from_slice(&(PHDR32_SIZE as u16).to_le_bytes());
        f[44..46].copy_from_slice(&(phdrs.len() as u16).to_le_bytes());
        for ph in phdrs {
            for field in [
                ph.p_type,
                ph.offset as u32,
                ph.vaddr as u32,
                ph.vaddr as u32,
                ph.filesz as u32,
                ph.memsz as u32,
                ph.flags,
                ph.align as u32,
            ] {
                f.extend_from_slice(&field.to_le_bytes());
            }
        }
        f.extend_from_slice(payload);
        f
    }

    /// An i386 dynamically linked executable, as `dynamic_executable`.
    fn dynamic_executable32() -> Vec<u8> {
        // Layout: 3 program headers at 0x34, then at 0x100 the
        // interpreter, at 0x110 the strings, at 0x130 the dynamic entries.
        let base = 0x0804_8000;
        let mut tail = vec![0u8; 0x100 - (EHDR32_SIZE + 3 * PHDR32_SIZE)];
        let mut put = |data: &[u8]| tail.extend_from_slice(data);
        let mut interp = b"/lib/ld.so.2\0".to_vec();
        interp.resize(0x10, 0);
        put(&interp);
        let mut strings = b"\0libc.so.6\0libm.so.6\0".to_vec();
        strings.resize(0x20, 0);
        put(&strings);
        for (tag, value) in [
            (DT_NEEDED as i32, 1u32),
            (DT_NEEDED as i32, 11),
            (DT_STRTAB as i32, base + 0x110),
            (DT_STRSZ as i32, 21),
            (DT_NULL as i32, 0),
        ] {
            put(&tag.to_le_bytes());
            put(&value.to_le_bytes());
        }
        let total = (0x130 + 5 * 8) as u64;
        let base = u64::from(base);
        let headers = [
            load(0, base, total, total, PF_R),
            ProgramHeader {
                p_type: PT_INTERP,
                flags: PF_R,
                offset: 0x100,
                vaddr: base + 0x100,
                filesz: 0x10,
                memsz: 0x10,
                align: 1,
            },
            ProgramHeader {
                p_type: PT_DYNAMIC,
                flags: PF_R,
                offset: 0x130,
                vaddr: base + 0x130,
                filesz: 5 * 8,
                memsz: 5 * 8,
                align: 4,
            },
        ];
        build32(ET_DYN, &headers, &tail)
    }

    #[test]
    fn elf32_i386_files_are_parsed() {
        let file = dynamic_executable32();
        let elf = Elf::parse(&file).unwrap();
        assert!(!elf.class64);
        assert_eq!(elf.machine, EM_386);
        assert_eq!(elf.entry, 0x0804_9000);
        assert_eq!(elf.program_header_size(), 32);
        assert_eq!(elf.program_headers[2].p_type, PT_DYNAMIC);
        assert_eq!(elf.program_headers[2].align, 4);
        assert_eq!(elf.phdr_address(), Some(0x0804_8034));
        assert_eq!(elf.interpreter_path(&file).as_deref(), Some("/lib/ld.so.2"));
        assert_eq!(
            elf.needed_libraries(&file).unwrap(),
            ["libc.so.6", "libm.so.6"]
        );
    }

    #[test]
    fn class_and_machine_must_agree() {
        // x86_64 in an ELF32, i386 in an ELF64.
        let mut file = build32(ET_EXEC, &[], &[]);
        file[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
        assert!(matches!(Elf::parse(&file), Err(ElfError::Unsupported(_))));
        let mut file = build(ET_EXEC, &[], &[]);
        file[18..20].copy_from_slice(&EM_386.to_le_bytes());
        assert!(matches!(Elf::parse(&file), Err(ElfError::Unsupported(_))));
        // A 32-bit segment cannot end beyond 4 GiB.
        let file = build32(ET_EXEC, &[load(0, 0xffff_f000, 0, 0x2000, PF_R)], &[]);
        assert!(matches!(Elf::parse(&file), Err(ElfError::Malformed(_))));
    }

    #[test]
    fn rejects_overflowing_address_ranges() {
        let file = build(ET_EXEC, &[load(0, u64::MAX - 4, 0, 0x10, PF_R)], &[]);
        assert!(matches!(Elf::parse(&file), Err(ElfError::Malformed(_))));
    }

    /// xorshift64: reproducible random numbers without a dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Iterations and seed: `ARMOR_FUZZ_ITERATIONS` and `ARMOR_FUZZ_SEED`
    /// (`cargo xtask fuzz`), else a quick run with a fixed seed.
    fn fuzz_config(default_iterations: u64) -> (u64, u64) {
        let var = |name: &str| std::env::var(name).ok().and_then(|v| v.parse().ok());
        let iterations = var("ARMOR_FUZZ_ITERATIONS").unwrap_or(default_iterations);
        let seed = var("ARMOR_FUZZ_SEED").unwrap_or(0x9e37_79b9_7f4a_7c15) | 1;
        (iterations, seed)
    }

    /// One random change: a few bytes, a header field set to an extreme
    /// value, or the file cut or extended.
    fn mutate(file: &mut Vec<u8>, rng: &mut Rng) {
        const EXTREMES: [u64; 8] = [
            0,
            1,
            0x38,
            0xffff,
            0x7fff_ffff,
            0xffff_ffff,
            u64::MAX - 7,
            u64::MAX,
        ];
        match rng.below(5) {
            0 | 1 => {
                for _ in 0..1 + rng.below(4) {
                    let at = rng.below(file.len() as u64) as usize;
                    file[at] = rng.next() as u8;
                }
            }
            2 => {
                // A field of the ELF header or of a program header.
                let width = [2usize, 4, 8][rng.below(3) as usize];
                if file.len() <= width {
                    return;
                }
                let at = (rng.below((file.len() - width) as u64) as usize) & !(width - 1);
                let value = EXTREMES[rng.below(EXTREMES.len() as u64) as usize];
                file[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
            }
            3 => file.truncate(rng.below(file.len() as u64) as usize),
            _ => file.extend((0..rng.below(64)).map(|_| rng.next() as u8)),
        }
    }

    /// The parser reads files the user runs: a malformed one must give an
    /// error, never a panic, and whatever `parse_any` accepts must be safe
    /// to use with every accessor.
    #[test]
    fn fuzz_parser() {
        let (iterations, seed) = fuzz_config(100_000);
        let mut rng = Rng(seed);
        let originals = [dynamic_executable(), dynamic_executable32()];
        for i in 0..iterations {
            let mut file = originals[(i % 2) as usize].clone();
            for _ in 0..1 + rng.below(3) {
                if file.len() < 8 {
                    break;
                }
                mutate(&mut file, &mut rng);
            }
            let context = format!("iteration {i}, seed {seed:#x}");
            let _ = ident(&file);
            let Ok(elf) = Elf::parse_any(&file) else {
                continue;
            };
            for ph in elf.load_segments() {
                assert_eq!(ph.file_bytes(&file).len() as u64, ph.filesz, "{context}");
            }
            let _ = elf.interpreter_path(&file);
            let _ = elf.needed_libraries(&file);
            let _ = elf.phdr_address();
            let _ = Elf::parse(&file);
        }
    }
}
