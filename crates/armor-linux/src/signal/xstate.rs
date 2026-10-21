//! The FPU state of a signal frame, x86_64 and i386 (docs/tech/signals.md):
//! what Linux's `copy_fpstate_to_sigframe` writes and `__fpu_restore_sig`
//! reads back, with the layout of the x86 uapi header `asm/sigcontext.h`.
//!
//! Without XSAVE in CPUID: a 512-byte FXSAVE image. With XSAVE declared
//! (`ARMOR_CPU_FEATURES=+xsave`, docs/tech/avx.md): the XSAVE image of the
//! XCR0 components, `_fpx_sw_bytes` in the legacy region's
//! software-reserved bytes (464..512) saying how large it is and what it
//! holds, and FP_XSTATE_MAGIC2 right after it.

use armor_cpu::Cpu;
use armor_cpu::cpuid::{self, Features};
use armor_cpu::xsave::{HEADER, SSE_COMPONENT, X87_COMPONENT};
use armor_mem::{AddressSpace, GuestMemory};

pub(super) const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
const FP_XSTATE_MAGIC2: u32 = 0x4650_5845;
/// `_fpx_sw_bytes` in the legacy region: magic1, extended_size,
/// xfeatures (8 bytes), xstate_size.
const SW_RESERVED: usize = 464;
const FXSAVE_SIZE: usize = 512;
/// The smallest XSAVE image a frame may say it holds: the legacy region
/// and the header (Linux's `min_xstate_size`).
const MIN_XSTATE_SIZE: usize = HEADER + 64;

fn xsave() -> bool {
    cpuid::declared().contains(Features::XSAVE)
}

/// The FPU state of `cpu` as a frame holds it: the FXSAVE image, or the
/// XSAVE image and MAGIC2. The i386
/// frame has the 112-byte FSAVE header (`fregs`) below it, which its
/// extended size counts.
pub(super) fn image(cpu: &Cpu, ia32: bool, fregs: u32) -> Vec<u8> {
    let put = |image: &mut [u8], at: usize, value: &[u8]| {
        image[SW_RESERVED + at..SW_RESERVED + at + value.len()].copy_from_slice(value);
    };
    if !xsave() {
        let mut image = cpu.fx_image().to_vec();
        // On a CPU without XSAVE, Linux marks an i386 frame's FXSAVE image
        // with the magic, an extended size of 512 + 4 + the FSAVE header,
        // no features and a state size of 512 (✅ Xeon X5675,
        // c32/signal_context); the x86_64 one keeps zeros there in armor
        // (⚠️ not measured on hardware).
        if ia32 {
            put(&mut image, 0, &FP_XSTATE_MAGIC1.to_le_bytes());
            put(&mut image, 4, &(512 + 4 + fregs).to_le_bytes());
            put(&mut image, 16, &(FXSAVE_SIZE as u32).to_le_bytes());
        }
        return image;
    }
    let features = cpuid::declared();
    let xcr0 = cpuid::xcr0(features);
    let xstate_size = cpuid::xsave_size(features);
    let mut image = cpu.xsave_image();
    let extended_size = xstate_size + 4 + if ia32 { fregs } else { 0 };
    put(&mut image, 0, &FP_XSTATE_MAGIC1.to_le_bytes());
    put(&mut image, 4, &extended_size.to_le_bytes());
    put(&mut image, 8, &xcr0.to_le_bytes());
    put(&mut image, 16, &xstate_size.to_le_bytes());
    // XSTATE_BV: every component of XCR0 (armor does not track their
    // initial state; Linux always adds x87 and SSE).
    image[HEADER..HEADER + 8]
        .copy_from_slice(&(xcr0 | X87_COMPONENT | SSE_COMPONENT).to_le_bytes());
    image.extend(FP_XSTATE_MAGIC2.to_le_bytes());
    image
}

/// rt_sigreturn's restore of the FPU state whose image is at `at`. With
/// XSAVE declared, a frame whose `_fpx_sw_bytes` or MAGIC2 do not match
/// (Linux's `check_xstate_in_sigframe`) is taken as an FXSAVE image, the
/// other components set to their initial state; otherwise the components
/// its xfeatures name are restored as xrstor does, and the others set to
/// their initial state. `Err` (SIGSEGV) for unreadable memory, a header
/// xrstor refuses, reserved MXCSR bits, or an x87 control state armor does
/// not support.
pub(super) fn restore(
    cpu: &mut Cpu,
    mem: &AddressSpace,
    at: u64,
    name: &str,
) -> Result<(), String> {
    let unreadable = |e| format!("{name}: unreadable FPU state ({e})");
    let mut legacy = [0u8; FXSAVE_SIZE];
    mem.read(at, &mut legacy).map_err(unreadable)?;
    if !xsave() {
        return cpu
            .load_fx_image(&legacy)
            .map_err(|what| format!("{name}: {what}"));
    }
    let word = |offset: usize| {
        u32::from_le_bytes(
            legacy[SW_RESERVED + offset..SW_RESERVED + offset + 4]
                .try_into()
                .expect("4 bytes"),
        )
    };
    let (magic1, extended_size, xstate_size) = (word(0), word(4), word(16) as usize);
    let xfeatures = u64::from(word(8)) | u64::from(word(12)) << 32;
    let max = cpuid::xsave_size(cpuid::declared()) as usize;
    let mut xstate = magic1 == FP_XSTATE_MAGIC1
        && (MIN_XSTATE_SIZE..=max).contains(&xstate_size)
        && xstate_size <= extended_size as usize;
    if xstate {
        let mut magic2 = [0u8; 4];
        mem.read(at + xstate_size as u64, &mut magic2)
            .map_err(unreadable)?;
        xstate = u32::from_le_bytes(magic2) == FP_XSTATE_MAGIC2;
    }
    let mut image = vec![0u8; max];
    let components = if xstate {
        mem.read(at, &mut image[..xstate_size])
            .map_err(unreadable)?;
        xfeatures
    } else {
        // Only the FXSAVE image: x87 and SSE from it.
        image[..FXSAVE_SIZE].copy_from_slice(&legacy);
        let both = X87_COMPONENT | SSE_COMPONENT;
        image[HEADER..HEADER + 8].copy_from_slice(&both.to_le_bytes());
        both
    };
    cpu.restore_signal_xstate(&image, components)
        .map_err(|what| format!("{name}: {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Declares XSAVE for the whole test process: no test of this crate
    /// relies on its absence.
    #[test]
    fn xsave_frames_carry_the_software_bytes_and_magic2() {
        cpuid::declare(Features::XSAVE);
        let cpu = Cpu::new(0, 0);
        let image = image(&cpu, false, 112);
        assert_eq!(image.len(), 580);
        let word = |at: usize| u32::from_le_bytes(image[at..at + 4].try_into().unwrap());
        assert_eq!(word(464), FP_XSTATE_MAGIC1);
        assert_eq!(word(468), 580, "extended_size");
        assert_eq!((word(472), word(476)), (3, 0), "xfeatures");
        assert_eq!(word(480), 576, "xstate_size");
        assert_eq!(word(512), 3, "XSTATE_BV");
        assert_eq!(word(576), FP_XSTATE_MAGIC2);
        let image32 = super::image(&cpu, true, 112);
        assert_eq!(
            u32::from_le_bytes(image32[468..472].try_into().unwrap()),
            580 + 112
        );
    }
}
