//! Builds the guest process image: maps the ELF segments and creates the
//! initial stack (argc, argv, envp, auxv) as the x86_64 Linux kernel does.
//!
//! Static executables are loaded alone. Dynamically linked ones are loaded
//! together with their interpreter (the x86_64 `ld.so`, from the guest
//! root), which then loads the libraries itself (ADR-0011). Both ET_EXEC
//! and position-independent (ET_DYN) executables are supported.
//!
//! An i386 (ELF32) executable gets the i386 layout (ADR-0029): every
//! mapping below 4 GiB (`AddressSpace::set_ceiling`), a stack of 4-byte
//! words, platform `i686`, and no vDSO, so that glibc makes its syscalls
//! with `int 0x80`.

use std::fmt;
use std::path::Path;

use armor_elf::{ET_DYN, Elf, ElfError, PF_R, PF_W, PF_X};
use armor_mem::{AddressSpace, GUEST_PAGE_SIZE, MapError, MemFault, Prot};
use armor_x86::Mode;

use crate::root::{GuestRoot, ROOT_VARIABLE};

/// Load address of position-independent executables: the x86_64 kernel's
/// `ELF_ET_DYN_BASE` without randomization (2/3 of the 47-bit user address
/// space, page-aligned), which is also where qemu-x86_64 puts them.
pub const PIE_BASE: u64 = 0x5555_5555_4000;

/// The same for an i386 executable: the x86_64 kernel's `ELF_ET_DYN_BASE`
/// for a 32-bit process ⚠️ (remembered from the kernel source; qemu-i386
/// puts a static PIE there too: its first `brk` is 0x4a9000 for a 0xa9000
/// byte image, measured with `-strace`, docs/tech/memory.md).
pub const I386_PIE_BASE: u64 = 0x40_0000;

/// The end of a 32-bit process's address space under a 64-bit kernel
/// (`IA32_PAGE_OFFSET`) ⚠️ (remembered from the kernel source): the
/// mappings armor places go below it.
pub const I386_CEILING: u64 = 0xffff_e000;

/// Size of the guest main-thread stack.
pub const STACK_SIZE: u64 = 8 << 20;

// Auxiliary vector types, from linux/auxvec.h and elf.h.
const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_FLAGS: u64 = 8;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_PLATFORM: u64 = 15;
const AT_HWCAP: u64 = 16;
const AT_CLKTCK: u64 = 17;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_HWCAP2: u64 = 26;
const AT_EXECFN: u64 = 31;

/// Where the guest starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    pub entry: u64,
    pub stack_pointer: u64,
    /// Initial program break: the page-aligned end of the loaded segments.
    pub brk_start: u64,
    /// `Mode::Compat32` for an i386 program.
    pub mode: Mode,
}

#[derive(Debug)]
pub enum LoadError {
    Elf(ElfError),
    Unsupported(&'static str),
    Map(MapError),
    /// The ELF does not fit where it asks to be loaded.
    Layout(String),
    /// The interpreter (`PT_INTERP`) is missing or not loadable.
    Interpreter(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Elf(e) => write!(f, "{e}"),
            LoadError::Unsupported(what) => write!(f, "not supported yet: {what}"),
            LoadError::Map(e) => write!(f, "cannot map the program: {e}"),
            LoadError::Layout(why) => write!(f, "{why}"),
            LoadError::Interpreter(why) => write!(
                f,
                "cannot load the interpreter {why}; the guest root is set with \
                 {ROOT_VARIABLE} (docs/architecture.md)"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<ElfError> for LoadError {
    fn from(e: ElfError) -> Self {
        LoadError::Elf(e)
    }
}

impl From<MapError> for LoadError {
    fn from(e: MapError) -> Self {
        LoadError::Map(e)
    }
}

impl From<MemFault> for LoadError {
    fn from(e: MemFault) -> Self {
        LoadError::Layout(format!("cannot write the initial image: {e}"))
    }
}

/// The loader's rules beyond `Elf::parse`: what armor can load today. Used
/// by `load` and by `armor inspect`, so that both always agree. Every
/// x86_64 executable `Elf::parse` accepts (ET_EXEC and ET_DYN, static or
/// with an interpreter) is loadable; whether its interpreter exists is only
/// known once the guest root is chosen.
pub fn check(_elf: &Elf) -> Result<(), LoadError> {
    Ok(())
}

/// Loads `file` (and its interpreter, found through `root`) into `mem` and
/// builds the initial stack. `args` includes argv[0]; `env` holds
/// `KEY=VALUE` entries.
pub fn load(
    mem: &AddressSpace,
    file: &[u8],
    args: &[&[u8]],
    env: &[&[u8]],
    execfn: &[u8],
    root: &GuestRoot,
) -> Result<Image, LoadError> {
    let elf = Elf::parse(file)?;
    check(&elf)?;
    let mode = if elf.class64 {
        Mode::Long
    } else {
        mem.set_ceiling(I386_CEILING);
        Mode::Compat32
    };
    let exe = map_elf(mem, &elf, file, Placement::Executable)?;
    if std::env::var_os(crate::syscall::TRACE_MAPS_VARIABLE).is_some() {
        eprintln!(
            "armor: map program {} at base {:#x}",
            String::from_utf8_lossy(execfn),
            exe.base
        );
    }

    let interpreter = match elf.interpreter_path(file) {
        Some(path) => {
            let host_path = root.resolve_path(Path::new(&path));
            let interp_file = std::fs::read(&host_path).map_err(|e| {
                LoadError::Interpreter(format!("{path} ({}): {e}", host_path.display()))
            })?;
            let interp = Elf::parse(&interp_file)
                .map_err(|e| LoadError::Interpreter(format!("{path}: {e}")))?;
            if interp.interpreter().is_some() {
                return Err(LoadError::Interpreter(format!(
                    "{path} itself has an interpreter"
                )));
            }
            if interp.class64 != elf.class64 {
                return Err(LoadError::Interpreter(format!(
                    "{path} is not for the same architecture as the program"
                )));
            }
            let base = map_elf(mem, &interp, &interp_file, Placement::Interpreter)?.base;
            if std::env::var_os(crate::syscall::TRACE_MAPS_VARIABLE).is_some() {
                eprintln!("armor: map interpreter {path} at base {base:#x}");
            }
            Some((base, interp.entry.wrapping_add(base)))
        }
        None => None,
    };

    let phdr = elf.phdr_address().ok_or_else(|| {
        LoadError::Layout("the program headers are not inside a loaded segment".into())
    })?;
    let entry = elf.entry.wrapping_add(exe.base);
    let auxv_values = AuxValues {
        phdr: phdr.wrapping_add(exe.base),
        phent: elf.program_header_size(),
        phnum: elf.program_headers.len() as u64,
        entry,
        interpreter_base: interpreter.map_or(0, |(base, _)| base),
    };
    let stack_pointer = build_stack(mem, args, env, execfn, &auxv_values, mode)?;
    Ok(Image {
        entry: interpreter.map_or(entry, |(_, entry)| entry),
        stack_pointer,
        brk_start: exe.end,
        mode,
    })
}

/// Where a loaded ELF goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// The program: ET_EXEC at its own addresses, ET_DYN at `PIE_BASE`
    /// (`I386_PIE_BASE` for i386; anywhere, if the host uses that range).
    Executable,
    /// The dynamic loader: anywhere.
    Interpreter,
}

/// A loaded ELF: `base` is added to its addresses (0 for ET_EXEC), `end` is
/// the guest-page-aligned end of its last segment.
struct Loaded {
    base: u64,
    end: u64,
}

fn map_elf(
    mem: &AddressSpace,
    elf: &Elf,
    file: &[u8],
    placement: Placement,
) -> Result<Loaded, LoadError> {
    let page_start = |vaddr: u64| vaddr - vaddr % GUEST_PAGE_SIZE;
    let low = elf
        .load_segments()
        .map(|seg| page_start(seg.vaddr))
        .min()
        .ok_or_else(|| LoadError::Layout("no loadable segment".into()))?;
    let high = elf
        .load_segments()
        .map(|seg| seg.vaddr + seg.memsz)
        .max()
        .unwrap_or(low)
        .checked_next_multiple_of(GUEST_PAGE_SIZE)
        .ok_or_else(|| LoadError::Layout("segments end at the top of memory".into()))?;
    let span = high - low;

    // Reserve the whole span for ET_DYN, so that the segments keep their
    // distances; the gaps between them stay inaccessible to the guest.
    let pie_base = if elf.class64 { PIE_BASE } else { I386_PIE_BASE };
    let base = if elf.elf_type != ET_DYN {
        0
    } else if placement == Placement::Executable
        && mem.map_fixed(pie_base, span, Prot::NONE).is_ok()
    {
        pie_base - low
    } else {
        mem.map_anywhere(span, Prot::NONE)?.wrapping_sub(low)
    };

    // Map every segment first, then copy the contents: segments that share
    // a guest page must not erase each other.
    for seg in elf.load_segments() {
        let start = page_start(seg.vaddr) + base;
        mem.map_fixed(start, seg.vaddr + base + seg.memsz - start, prot(seg.flags))?;
    }
    for seg in elf.load_segments() {
        mem.write_privileged(seg.vaddr + base, seg.file_bytes(file))?;
    }
    Ok(Loaded {
        base,
        end: high + base,
    })
}

fn prot(flags: u32) -> Prot {
    let mut prot = Prot::NONE;
    for (flag, p) in [(PF_R, Prot::READ), (PF_W, Prot::WRITE), (PF_X, Prot::EXEC)] {
        if flags & flag != 0 {
            prot = prot | p;
        }
    }
    prot
}

struct AuxValues {
    phdr: u64,
    phent: u64,
    phnum: u64,
    entry: u64,
    /// Load address of the interpreter, 0 without one.
    interpreter_base: u64,
}

/// Lays out the stack top-down: strings and random bytes first, then the
/// pointer table, so that rsp (pointing at argc) is 16-byte aligned. The
/// table has 8-byte words, 4-byte ones for an i386 program.
fn build_stack(
    mem: &AddressSpace,
    args: &[&[u8]],
    env: &[&[u8]],
    execfn: &[u8],
    aux: &AuxValues,
    mode: Mode,
) -> Result<u64, LoadError> {
    let base = mem.map_anywhere(STACK_SIZE, Prot::READ | Prot::WRITE)?;
    let mut top = Stack {
        mem,
        sp: base + STACK_SIZE,
    };

    let execfn_ptr = top.push_cstr(execfn)?;
    let env_ptrs = env
        .iter()
        .map(|s| top.push_cstr(s))
        .collect::<Result<Vec<_>, _>>()?;
    let arg_ptrs = args
        .iter()
        .map(|s| top.push_cstr(s))
        .collect::<Result<Vec<_>, _>>()?;
    let platform_ptr = top.push_cstr(match mode {
        Mode::Long => b"x86_64",
        Mode::Compat32 => b"i686",
    })?;
    let random_ptr = top.push_bytes(&random_bytes())?;

    let auxv: [(u64, u64); 17] = [
        // The x86_64 kernel's ELF_HWCAP: CPUID leaf 1, EDX.
        (AT_HWCAP, u64::from(armor_cpu::cpuid::query(1, 0)[3])),
        (AT_PAGESZ, GUEST_PAGE_SIZE),
        (AT_CLKTCK, 100),
        (AT_PHDR, aux.phdr),
        (AT_PHENT, aux.phent),
        (AT_PHNUM, aux.phnum),
        (AT_BASE, aux.interpreter_base),
        (AT_FLAGS, 0),
        (AT_ENTRY, aux.entry),
        (AT_UID, u64::from(host_id(libc::getuid))),
        (AT_EUID, u64::from(host_id(libc::geteuid))),
        (AT_GID, u64::from(host_id(libc::getgid))),
        (AT_EGID, u64::from(host_id(libc::getegid))),
        (AT_SECURE, 0),
        (AT_RANDOM, random_ptr),
        (AT_HWCAP2, 0),
        (AT_EXECFN, execfn_ptr),
    ];

    let mut table = vec![args.len() as u64];
    table.extend(&arg_ptrs);
    table.push(0);
    table.extend(&env_ptrs);
    table.push(0);
    for (key, value) in auxv
        .into_iter()
        .chain([(AT_PLATFORM, platform_ptr), (AT_NULL, 0)])
    {
        table.extend([key, value]);
    }

    let bytes: Vec<u8> = match mode {
        Mode::Long => table.iter().flat_map(|w| w.to_le_bytes()).collect(),
        Mode::Compat32 => table
            .iter()
            .flat_map(|&w| (w as u32).to_le_bytes())
            .collect(),
    };
    let sp = (top.sp - bytes.len() as u64) & !15;
    top.mem.write_privileged(sp, &bytes)?;
    Ok(sp)
}

/// Downward-growing writer for the initial stack contents.
struct Stack<'a> {
    mem: &'a AddressSpace,
    sp: u64,
}

impl Stack<'_> {
    fn push_bytes(&mut self, bytes: &[u8]) -> Result<u64, LoadError> {
        self.sp -= bytes.len() as u64;
        self.mem.write_privileged(self.sp, bytes)?;
        Ok(self.sp)
    }

    fn push_cstr(&mut self, s: &[u8]) -> Result<u64, LoadError> {
        self.sp -= 1;
        self.mem.write_privileged(self.sp, &[0])?;
        self.push_bytes(s)
    }
}

fn host_id(get: unsafe extern "C" fn() -> libc::uid_t) -> libc::uid_t {
    // SAFETY: getuid/geteuid/getgid/getegid cannot fail and take no arguments.
    unsafe { get() }
}

/// The 16 bytes for AT_RANDOM (used by glibc for stack protector canaries).
fn random_bytes() -> [u8; 16] {
    let mut buf = [0u8; 16];
    // SAFETY: buf is valid for writes of its length.
    let n = unsafe { libc::getrandom(buf.as_mut_ptr().cast(), buf.len(), 0) };
    assert_eq!(n, buf.len() as isize, "getrandom failed");
    buf
}
