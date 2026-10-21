//! CPUID: the CPU armor presents to the guest.
//!
//! The values reproduce qemu-x86_64's `qemu64` model with the features armor
//! does not implement removed and those it does added: `-cpu
//! qemu64,-svm,+ssse3,+sse4.1,+sse4.2,+popcnt` (measured on 2026-10-12;
//! CX16 and LAHF-LM reported since 2026-10-13, ADR-0010, SSE3 to SSE4.2
//! and POPCNT since 2026-10-13, M4; docs/tech/cpuid.md). `cargo xtask difftest` runs
//! qemu with the same model, so glibc picks the same code paths under both.
//! Two deliberate differences: the brand string names armor, and the
//! hypervisor leaves (0x4000_0000) are empty instead of qemu's "TCGTCGTCGTCG".
//!
//! Features on the way to x86-64-v3 (docs/tech/avx.md) are declared only
//! when `ARMOR_CPU_FEATURES` names them (`Features::parse`, `declare`), as
//! qemu adds them to its model with `-cpu qemu64,...,+xsave`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

/// CPUID features armor declares only on request, beyond the default
/// model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Features(u32);

impl Features {
    pub const NONE: Features = Features(0);
    /// XSAVE and OSXSAVE (leaf 1 ecx bits 26 and 27), leaf 0xD, XCR0 = x87
    /// | SSE: xgetbv, xsave, xrstor, and XSAVE-format signal frames.
    pub const XSAVE: Features = Features(1 << 0);

    /// The names `parse` accepts, qemu's (`qemu-x86_64 -cpu help`): only
    /// features armor implements.
    const NAMES: [(&'static str, Features); 1] = [("xsave", Features::XSAVE)];

    pub fn contains(self, other: Features) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Features) -> Features {
        Features(self.0 | other.0)
    }

    pub fn bits(self) -> u32 {
        self.0
    }

    /// Parses a list of feature names separated by commas, each with an
    /// optional `+`, as qemu's `-cpu` takes them: `+xsave` or `xsave`. An
    /// empty list declares nothing more.
    pub fn parse(list: &str) -> Result<Features, String> {
        let mut features = Features::NONE;
        for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            let bare = name.strip_prefix('+').unwrap_or(name);
            let Some((_, feature)) = Features::NAMES.iter().find(|(n, _)| *n == bare) else {
                let known: Vec<&str> = Features::NAMES.iter().map(|(n, _)| *n).collect();
                return Err(format!(
                    "unknown or unimplemented CPU feature `{name}` (armor can declare: {})",
                    known.join(", ")
                ));
            };
            features = features.union(*feature);
        }
        Ok(features)
    }

    /// The names of the features, as `parse` reads them.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Features::NAMES
            .into_iter()
            .filter(move |(_, f)| self.contains(*f))
            .map(|(n, _)| n)
    }
}

/// The features declared beyond the default model, for the whole process
/// (`declare`).
static DECLARED: AtomicU32 = AtomicU32::new(0);

/// Declares `features` in CPUID from now on. Called once, before the
/// guest runs: what CPUID says must not change under a running program.
pub fn declare(features: Features) {
    DECLARED.store(features.0, Ordering::Relaxed);
}

/// The features `declare` added.
pub fn declared() -> Features {
    Features(DECLARED.load(Ordering::Relaxed))
}

/// XCR0, the state components the XSAVE instructions handle: x87 and SSE
/// with XSAVE declared, as qemu's user mode enables every component its
/// model has (✅ `xgetbv` under `-cpu qemu64,+xsave` gives 3); none
/// without.
pub fn xcr0(features: Features) -> u64 {
    if features.contains(Features::XSAVE) {
        0b11
    } else {
        0
    }
}

/// The size of the XSAVE area for the components of XCR0: the 512-byte
/// legacy region and the 64-byte header (leaf 0xD, ebx).
pub fn xsave_size(features: Features) -> u32 {
    if features.contains(Features::XSAVE) {
        512 + 64
    } else {
        0
    }
}

/// Whether the CPU armor presents reports `extension` in CPUID. An
/// instruction of an extension it does not report raises #UD, as on a CPU
/// without it (ADR-0026); one it reports but armor does not implement yet
/// (MMX) stops armor instead.
pub fn reports(extension: armor_x86::Extension) -> bool {
    reports_with(declared(), extension)
}

/// `reports` with `features` declared.
pub fn reports_with(features: Features, extension: armor_x86::Extension) -> bool {
    use armor_x86::Extension::*;
    match extension {
        Base | X87 | Mmx | Sse | Sse3 | Ssse3 | Sse41 | Sse42 | Popcnt => true,
        Xsave => features.contains(Features::XSAVE),
        Aes | Pclmul | Sha | Amd3dNow | Vex | Evex | Xop | Padlock => false,
    }
}

/// Brand string returned by leaves 0x8000_0002..0x8000_0004 (48 bytes).
const BRAND: &[u8; 48] = b"armor virtual x86-64 CPU (qemu64 features)\0\0\0\0\0\0";

/// `(eax, ebx, ecx, edx)` for `leaf` and `subleaf` (ecx).
pub fn query(leaf: u32, subleaf: u32) -> [u32; 4] {
    query_with(declared(), leaf, subleaf)
}

/// `query` with `features` declared.
pub fn query_with(features: Features, leaf: u32, subleaf: u32) -> [u32; 4] {
    let xsave = features.contains(Features::XSAVE);
    match leaf {
        // Max basic leaf 0xd, vendor "AuthenticAMD".
        0 => [0xd, 0x6874_7541, 0x444d_4163, 0x6974_6e65],
        // Family/model/stepping, CLFLUSH size 64, features: SSE2 baseline
        // (FPU ... SSE SSE2) in edx, SSE3 (bit 0), SSSE3 (bit 9), CX16
        // (bit 13), SSE4.1 (bit 19), SSE4.2 (bit 20), POPCNT (bit 23) and
        // "hypervisor present" in ecx; with XSAVE, XSAVE (bit 26) and
        // OSXSAVE (bit 27: the operating system has enabled it, as Linux
        // does).
        1 => {
            let xsave_bits = if xsave { 3 << 26 } else { 0 };
            [
                0x0006_0fb1,
                0x0000_0800,
                0x8098_2201 | xsave_bits,
                0x078b_fbfd,
            ]
        }
        // MONITOR/MWAIT leaf: only the extension bits.
        5 => [0, 0, 3, 0],
        // Extended topology: one thread per core, one core.
        0xb => match subleaf {
            0 => [0, 1, 0x100, 0],
            1 => [0, 1, 0x201, 0],
            n => [0, 0, n & 0xff, 0],
        },
        // XSAVE. Subleaf 0: the components XCR0 may enable (eax, edx), the
        // area's size for those enabled (ebx) and for all of them (ecx).
        // Subleaf 1: no XSAVEOPT, XSAVEC, XGETBV with ecx = 1 or XSAVES
        // (eax 0), and the size again in ebx, as qemu reports it.
        0xd if xsave => {
            let size = xsave_size(features);
            match subleaf {
                0 => [xcr0(features) as u32, size, size, 0],
                1 => [0, size, 0, 0],
                _ => [0; 4],
            }
        }
        // Max extended leaf 0x8000000a, vendor again.
        0x8000_0000 => [0x8000_000a, 0x6874_7541, 0x444d_4163, 0x6974_6e65],
        // LAHF-LM (ecx bit 0); SYSCALL, NX, long mode, and the mirrored
        // baseline bits.
        0x8000_0001 => [0x0006_0fb1, 0, 1, 0x2193_fbfd],
        0x8000_0002..=0x8000_0004 => {
            let chunk = &BRAND[(leaf - 0x8000_0002) as usize * 16..][..16];
            let word = |i: usize| u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
            [word(0), word(1), word(2), word(3)]
        }
        // L1 cache and TLB information.
        0x8000_0005 => [0x01ff_01ff, 0x01ff_01ff, 0x4002_0140, 0x4002_0140],
        // L2/L3 cache information.
        0x8000_0006 => [0, 0x4200_4200, 0x0200_8140, 0x0080_8140],
        // 48-bit virtual, 40-bit physical addresses.
        0x8000_0008 => [0x0000_3028, 0, 0, 0],
        // The other leaves up to the highest basic (0xd) and extended
        // (0x8000_000a) ones, and the hypervisor leaves armor leaves empty.
        0..=0xd | 0x4000_0000..=0x4000_0010 | 0x8000_0000..=0x8000_000a => [0; 4],
        // A leaf past them gives the highest basic leaf, with the same
        // subleaf, as Intel CPUs and qemu do (✅ qemu 10.0.13 with
        // `+xsave`: leaves 0xe, 0x3fff_ffff, 0x4000_0011, 0x8000_000b,
        // 0xc000_0000 and 0xffff_ffff all give leaf 0xd).
        _ => query_with(features, 0xd, subleaf),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_is_authentic_amd() {
        let [_, ebx, ecx, edx] = query_with(Features::NONE, 0, 0);
        let vendor: Vec<u8> = [ebx, edx, ecx]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        assert_eq!(vendor, b"AuthenticAMD");
    }

    #[test]
    fn reported_extensions_match_the_feature_bits() {
        use armor_x86::Extension;
        for features in [Features::NONE, Features::XSAVE] {
            let query = |leaf| query_with(features, leaf, 0);
            let reports = |extension| reports_with(features, extension);
            let [_, _, ecx, edx] = query(1);
            let bit = |word: u32, n: u32| word >> n & 1 == 1;
            assert_eq!(reports(Extension::Mmx), bit(edx, 23));
            assert_eq!(reports(Extension::Sse), bit(edx, 25) && bit(edx, 26));
            assert_eq!(reports(Extension::Sse3), bit(ecx, 0));
            assert_eq!(reports(Extension::Pclmul), bit(ecx, 1));
            assert_eq!(reports(Extension::Ssse3), bit(ecx, 9));
            assert_eq!(reports(Extension::Sse41), bit(ecx, 19));
            assert_eq!(reports(Extension::Sse42), bit(ecx, 20));
            assert_eq!(reports(Extension::Popcnt), bit(ecx, 23));
            assert_eq!(reports(Extension::Aes), bit(ecx, 25));
            assert_eq!(reports(Extension::Xsave), bit(ecx, 26) && bit(ecx, 27));
            assert_eq!(reports(Extension::Vex), bit(ecx, 28));
            assert_eq!(reports(Extension::Evex), query(7)[1] >> 16 & 1 == 1);
            assert_eq!(reports(Extension::Sha), query(7)[1] >> 29 & 1 == 1);
            let ext = query(0x8000_0001);
            assert_eq!(reports(Extension::Xop), bit(ext[2], 11));
            assert_eq!(reports(Extension::Amd3dNow), bit(ext[3], 31));
        }
    }

    #[test]
    fn only_implemented_features_are_reported() {
        let query = |leaf| query_with(Features::NONE, leaf, 0);
        let [_, _, ecx, edx] = query(1);
        assert_eq!(edx & (1 << 26), 1 << 26, "SSE2");
        assert_eq!(ecx & 1, 1, "SSE3");
        assert_eq!(ecx & (1 << 9), 1 << 9, "SSSE3");
        assert_eq!(ecx & (1 << 19), 1 << 19, "SSE4.1");
        assert_eq!(ecx & (1 << 20), 1 << 20, "SSE4.2");
        assert_eq!(ecx & (1 << 23), 1 << 23, "POPCNT");
        assert_eq!(ecx & (1 << 28), 0, "no AVX");
        assert_eq!(ecx & (1 << 27), 0, "no OSXSAVE, so no AVX");
        assert_eq!(ecx & (1 << 26), 0, "no XSAVE by default");
        assert_eq!(query(7), [0; 4], "no BMI/AVX2");
        assert_eq!(query(0xd), [0; 4], "no XSAVE leaf by default");
        assert_eq!(ecx & (1 << 13), 1 << 13, "CX16: cmpxchg16b");
        assert_eq!(
            query(0x8000_0001)[2],
            1,
            "LAHF-LM: lahf/sahf in 64-bit mode"
        );
    }

    #[test]
    fn xsave_declares_x87_and_sse_in_leaf_0xd() {
        let f = Features::XSAVE;
        assert_eq!(query_with(f, 1, 0)[2], 0x8c98_2201);
        assert_eq!(query_with(f, 0xd, 0), [3, 0x240, 0x240, 0]);
        assert_eq!(query_with(f, 0xd, 1), [0, 0x240, 0, 0]);
        assert_eq!(query_with(f, 0xd, 2), [0; 4]);
        assert_eq!(xcr0(f), 3);
        // Past the highest leaves: leaf 0xd, the subleaf kept.
        assert_eq!(query_with(f, 0xe, 1), [0, 0x240, 0, 0]);
        assert_eq!(query_with(f, 0x8000_000b, 0), [3, 0x240, 0x240, 0]);
        assert_eq!(query_with(f, 0x4000_0000, 0), [0; 4]);
        assert_eq!(query_with(f, 0x8000_0007, 0), [0; 4]);
        assert_eq!(query_with(Features::NONE, 0x8000_000b, 0), [0; 4]);
    }

    #[test]
    fn features_parse_like_qemu_additions() {
        assert_eq!(Features::parse(""), Ok(Features::NONE));
        assert_eq!(Features::parse("+xsave"), Ok(Features::XSAVE));
        assert_eq!(Features::parse("xsave, +xsave"), Ok(Features::XSAVE));
        let err = Features::parse("+xsave,+avx512f").unwrap_err();
        assert!(err.contains("avx512f"), "{err}");
        assert_eq!(Features::XSAVE.names().collect::<Vec<_>>(), ["xsave"]);
    }

    #[test]
    fn brand_string_names_armor() {
        let brand: Vec<u8> = (0x8000_0002..=0x8000_0004)
            .flat_map(|leaf| query_with(Features::NONE, leaf, 0))
            .flat_map(|w| w.to_le_bytes())
            .collect();
        assert!(brand.starts_with(b"armor virtual x86-64 CPU"));
    }
}
