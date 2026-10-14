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

/// Whether the CPU armor presents reports `extension` in CPUID. An
/// instruction of an extension it does not report raises #UD, as on a CPU
/// without it (ADR-0026); one it reports but armor does not implement yet
/// (MMX) stops armor instead.
pub fn reports(extension: armor_x86::Extension) -> bool {
    use armor_x86::Extension::*;
    match extension {
        Base | X87 | Mmx | Sse | Sse3 | Ssse3 | Sse41 | Sse42 | Popcnt => true,
        Aes | Pclmul | Sha | Amd3dNow | Vex | Evex | Xop | Padlock => false,
    }
}

/// Brand string returned by leaves 0x8000_0002..0x8000_0004 (48 bytes).
const BRAND: &[u8; 48] = b"armor virtual x86-64 CPU (qemu64 features)\0\0\0\0\0\0";

/// `(eax, ebx, ecx, edx)` for `leaf` and `subleaf` (ecx).
pub fn query(leaf: u32, subleaf: u32) -> [u32; 4] {
    match leaf {
        // Max basic leaf 0xd, vendor "AuthenticAMD".
        0 => [0xd, 0x6874_7541, 0x444d_4163, 0x6974_6e65],
        // Family/model/stepping, CLFLUSH size 64, features: SSE2 baseline
        // (FPU ... SSE SSE2) in edx, SSE3 (bit 0), SSSE3 (bit 9), CX16
        // (bit 13), SSE4.1 (bit 19), SSE4.2 (bit 20), POPCNT (bit 23) and
        // "hypervisor present" in ecx.
        1 => [0x0006_0fb1, 0x0000_0800, 0x8098_2201, 0x078b_fbfd],
        // MONITOR/MWAIT leaf: only the extension bits.
        5 => [0, 0, 3, 0],
        // Extended topology: one thread per core, one core.
        0xb => match subleaf {
            0 => [0, 1, 0x100, 0],
            1 => [0, 1, 0x201, 0],
            n => [0, 0, n & 0xff, 0],
        },
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
        _ => [0; 4],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_is_authentic_amd() {
        let [_, ebx, ecx, edx] = query(0, 0);
        let vendor: Vec<u8> = [ebx, edx, ecx]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        assert_eq!(vendor, b"AuthenticAMD");
    }

    #[test]
    fn reported_extensions_match_the_feature_bits() {
        use armor_x86::Extension;
        let [_, _, ecx, edx] = query(1, 0);
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
        assert_eq!(reports(Extension::Vex), bit(ecx, 28));
        assert_eq!(reports(Extension::Evex), query(7, 0)[1] >> 16 & 1 == 1);
        assert_eq!(reports(Extension::Sha), query(7, 0)[1] >> 29 & 1 == 1);
        let ext = query(0x8000_0001, 0);
        assert_eq!(reports(Extension::Xop), bit(ext[2], 11));
        assert_eq!(reports(Extension::Amd3dNow), bit(ext[3], 31));
    }

    #[test]
    fn only_implemented_features_are_reported() {
        let [_, _, ecx, edx] = query(1, 0);
        assert_eq!(edx & (1 << 26), 1 << 26, "SSE2");
        assert_eq!(ecx & 1, 1, "SSE3");
        assert_eq!(ecx & (1 << 9), 1 << 9, "SSSE3");
        assert_eq!(ecx & (1 << 19), 1 << 19, "SSE4.1");
        assert_eq!(ecx & (1 << 20), 1 << 20, "SSE4.2");
        assert_eq!(ecx & (1 << 23), 1 << 23, "POPCNT");
        assert_eq!(ecx & (1 << 28), 0, "no AVX");
        assert_eq!(ecx & (1 << 27), 0, "no OSXSAVE, so no AVX");
        assert_eq!(query(7, 0), [0; 4], "no BMI/AVX2");
        assert_eq!(ecx & (1 << 13), 1 << 13, "CX16: cmpxchg16b");
        assert_eq!(
            query(0x8000_0001, 0)[2],
            1,
            "LAHF-LM: lahf/sahf in 64-bit mode"
        );
    }

    #[test]
    fn brand_string_names_armor() {
        let brand: Vec<u8> = (0x8000_0002..=0x8000_0004)
            .flat_map(|leaf| query(leaf, 0))
            .flat_map(|w| w.to_le_bytes())
            .collect();
        assert!(brand.starts_with(b"armor virtual x86-64 CPU"));
    }
}
