//! `--limit-*` (ADR-0027, docs/tech/security.md): resource limits for the
//! guest. Files, processes, file size and CPU time are the kernel's
//! (`setrlimit`); memory is the guest's address space, counted by armor
//! (`AddressSpace::set_limit`), which is not a security boundary.

use armor_linux::sandbox::{self, Limit};
use armor_mem::AddressSpace;

/// The options, by name without `--`.
pub const OPTIONS: [&str; 5] = [
    "limit-files",
    "limit-processes",
    "limit-file-size",
    "limit-cpu",
    "limit-memory",
];

/// The value of `option`: a count, seconds, or bytes with an optional K,
/// M or G (powers of 1024).
pub fn parse(option: &str, value: &str) -> Result<u64, String> {
    let bytes = matches!(option, "limit-file-size" | "limit-memory");
    let (digits, scale) = match value.as_bytes().last() {
        Some(b'K') if bytes => (&value[..value.len() - 1], 1 << 10),
        Some(b'M') if bytes => (&value[..value.len() - 1], 1 << 20),
        Some(b'G') if bytes => (&value[..value.len() - 1], 1 << 30),
        _ => (value, 1),
    };
    digits
        .parse::<u64>()
        .ok()
        .filter(|&n| n > 0)
        .and_then(|n| n.checked_mul(scale))
        .ok_or_else(|| {
            format!(
                "{option} {value:?}: a positive {}",
                if bytes {
                    "size (bytes, or with K, M, G)"
                } else {
                    "number"
                }
            )
        })
}

/// Applies the kernel's limits, before armor chooses the path: a native
/// program run directly gets them too.
pub fn apply_kernel(limits: &[(String, u64)]) -> Result<(), String> {
    for (option, value) in limits {
        let limit = match option.as_str() {
            "limit-files" => Limit::Files,
            "limit-processes" => Limit::Processes,
            "limit-file-size" => Limit::FileSize,
            "limit-cpu" => Limit::Cpu,
            _ => continue,
        };
        sandbox::set_limit(limit, *value)?;
    }
    Ok(())
}

/// Applies the memory limit to the guest's address space (x86-64 guests
/// only: armor counts it).
pub fn apply_memory(limits: &[(String, u64)], mem: &AddressSpace) {
    for (option, value) in limits {
        if option == "limit-memory" {
            mem.set_limit(*value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_parse_with_units() {
        assert_eq!(parse("limit-memory", "512M"), Ok(512 << 20));
        assert_eq!(parse("limit-file-size", "4096"), Ok(4096));
        assert_eq!(parse("limit-files", "64"), Ok(64));
        assert!(parse("limit-files", "64K").is_err());
        assert!(parse("limit-cpu", "0").is_err());
        assert!(parse("limit-memory", "lots").is_err());
    }
}
