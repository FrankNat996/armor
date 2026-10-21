//! xsave and xrstor in the standard (not compacted) format, and the XSAVE
//! images of signal frames (Intel SDM vol. 1, chapter 13; docs/tech/avx.md).
//!
//! The area: the 512-byte legacy region (the FXSAVE image, `sse.rs`), the
//! 64-byte header (XSTATE_BV, XCOMP_BV, reserved), then the extended
//! components at the offsets of CPUID leaf 0xD. The instructions handle
//! the components requested in edx:eax that XCR0 enables (RFBM).

use alloc::vec;
use alloc::vec::Vec;

use armor_guest::GuestMemory;

use crate::sse::{restore_x87_state, saved_mxcsr, saved_xmm};
use crate::{Cpu, Exception, Exec, RAX, RDX, Trap, cpuid, x87};

/// State components: XSTATE_BV and XCR0 bits.
pub const X87_COMPONENT: u64 = 1 << 0;
pub const SSE_COMPONENT: u64 = 1 << 1;
pub const AVX_COMPONENT: u64 = 1 << 2;

/// The components the legacy region holds (MXCSR belongs to SSE and AVX).
const LEGACY_COMPONENTS: u64 = X87_COMPONENT | SSE_COMPONENT | AVX_COMPONENT;
/// Where the header starts: XSTATE_BV (8 bytes), XCOMP_BV (8), then 48
/// reserved bytes.
pub const HEADER: usize = 512;
/// YMM_Hi128, the AVX component: bits 255:128 of ymm0–ymm15.
const AVX_OFFSET: usize = 576;

impl Cpu {
    /// xsave: the components of RFBM into the area at `addr`, then their
    /// bits in XSTATE_BV. Bytes of the area that hold none of them are
    /// not written.
    pub(crate) fn xsave(&self, addr: u64, mem: &impl GuestMemory) -> Exec<()> {
        check_alignment(addr)?;
        let rfbm = self.requested_components();
        let (start, mut area) = self.area(rfbm);
        mem.read(addr.wrapping_add(start as u64), &mut area[start..])?;
        self.save_components(&mut area, rfbm);
        mem.write(addr.wrapping_add(start as u64), &area[start..])?;
        Ok(())
    }

    /// xrstor: each component of RFBM from the area if its XSTATE_BV bit
    /// is set, otherwise its initial state; the others unchanged. #GP,
    /// with nothing changed, for an XSTATE_BV bit outside XCR0, a
    /// compacted area (XCOMP_BV, which needs XSAVEC) or a reserved header
    /// byte set, or reserved MXCSR bits.
    pub(crate) fn xrstor(&mut self, addr: u64, mem: &impl GuestMemory) -> Exec<()> {
        check_alignment(addr)?;
        let rfbm = self.requested_components();
        let (start, mut area) = self.area(rfbm);
        mem.read(addr.wrapping_add(start as u64), &mut area[start..])?;
        self.restore_components(&area, rfbm)
    }

    /// The XSAVE image Linux writes into a signal frame: every component
    /// of XCR0, `cpuid::xsave_size` bytes, those that hold none of them
    /// zero (the frame's code fills in the software-reserved bytes of the
    /// legacy region, 464..512).
    pub fn xsave_image(&self) -> Vec<u8> {
        let features = cpuid::declared();
        let mut image = vec![0u8; cpuid::xsave_size(features) as usize];
        self.save_components(&mut image, cpuid::xcr0(features));
        image
    }

    /// Loads the components in `components` of an XSAVE image, as xrstor
    /// does, and sets the other components of XCR0 to their initial state:
    /// what Linux does at signal return with the features a frame says it
    /// holds. `Err` names what is wrong (the header, MXCSR, or an x87
    /// control state armor does not support); nothing is changed then.
    pub fn restore_signal_xstate(
        &mut self,
        image: &[u8],
        components: u64,
    ) -> Result<(), &'static str> {
        let xcr0 = cpuid::xcr0(cpuid::declared());
        // `restore_components` changes nothing when it fails.
        self.restore_components(image, components & xcr0)
            .map_err(|trap| match trap {
                Trap::Unsupported(what) => what,
                _ => "an XSAVE header or MXCSR that xrstor refuses in a signal frame",
            })?;
        self.init_components(xcr0 & !components);
        Ok(())
    }

    /// RFBM: XCR0 and edx:eax.
    fn requested_components(&self) -> u64 {
        let mask = (self.regs[RDX] & 0xffff_ffff) << 32 | (self.regs[RAX] & 0xffff_ffff);
        mask & cpuid::xcr0(cpuid::declared())
    }

    /// A buffer for the area of the XCR0 components, and the first byte
    /// an instruction with `rfbm` accesses: the header only, when no
    /// component of the legacy region is requested.
    fn area(&self, rfbm: u64) -> (usize, Vec<u8>) {
        let size = cpuid::xsave_size(cpuid::declared()) as usize;
        let start = if rfbm & LEGACY_COMPONENTS != 0 {
            0
        } else {
            HEADER
        };
        (start, vec![0u8; size])
    }

    fn save_components(&self, area: &mut [u8], rfbm: u64) {
        if rfbm & X87_COMPONENT != 0 {
            self.save_x87_state(area);
        }
        if rfbm & (SSE_COMPONENT | AVX_COMPONENT) != 0 {
            self.save_mxcsr(area);
        }
        if rfbm & SSE_COMPONENT != 0 {
            self.save_xmm(area);
        }
        if rfbm & AVX_COMPONENT != 0 {
            for (i, high) in self.ymm_hi.iter().enumerate() {
                let at = AVX_OFFSET + 16 * i;
                area[at..at + 16].copy_from_slice(&high.to_le_bytes());
            }
        }
        // armor does not track which components are in their initial
        // state (XINUSE): every saved one is marked in use, as qemu does
        // and the SDM allows (13.6). The other bits stay as they were.
        let bv = header_word(area, 0);
        let bv = (bv & !rfbm) | rfbm;
        area[HEADER..HEADER + 8].copy_from_slice(&bv.to_le_bytes());
    }

    fn restore_components(&mut self, area: &[u8], rfbm: u64) -> Exec<()> {
        let xcr0 = cpuid::xcr0(cpuid::declared());
        let bv = header_word(area, 0);
        // Bytes 8..24 of the header: XCOMP_BV and the first reserved word.
        if bv & !xcr0 != 0 || header_word(area, 8) != 0 || header_word(area, 16) != 0 {
            return Err(Trap::Exception(Exception::GeneralProtection));
        }
        // Everything is read and checked before anything changes.
        let mut x87 = self.x87;
        let mut mxcsr = self.mxcsr;
        let mut xmm = self.xmm;
        let mut ymm_hi = self.ymm_hi;
        if rfbm & X87_COMPONENT != 0 {
            if bv & X87_COMPONENT != 0 {
                restore_x87_state(&mut x87, area)?;
            } else {
                x87 = x87::X87::default();
            }
        }
        // MXCSR comes from the area whenever SSE or AVX is requested, even
        // with both XSTATE_BV bits clear (SDM 13.8.1).
        if rfbm & (SSE_COMPONENT | AVX_COMPONENT) != 0 {
            mxcsr = saved_mxcsr(area)?;
        }
        if rfbm & SSE_COMPONENT != 0 {
            xmm = if bv & SSE_COMPONENT != 0 {
                saved_xmm(area)
            } else {
                [0; 16]
            };
        }
        if rfbm & AVX_COMPONENT != 0 {
            ymm_hi = if bv & AVX_COMPONENT != 0 {
                core::array::from_fn(|i| {
                    let at = AVX_OFFSET + 16 * i;
                    u128::from_le_bytes(area[at..at + 16].try_into().expect("16 bytes"))
                })
            } else {
                [0; 16]
            };
        }
        self.x87 = x87;
        self.mxcsr = mxcsr;
        self.xmm = xmm;
        self.ymm_hi = ymm_hi;
        Ok(())
    }

    /// The initial state of `components`: x87 as after fninit with the
    /// registers zeroed, xmm zeroed (MXCSR stays), upper ymm halves
    /// zeroed.
    fn init_components(&mut self, components: u64) {
        if components & X87_COMPONENT != 0 {
            self.x87 = x87::X87::default();
        }
        if components & SSE_COMPONENT != 0 {
            self.xmm = [0; 16];
        }
        if components & AVX_COMPONENT != 0 {
            self.ymm_hi = [0; 16];
        }
    }
}

/// The 64-bit word at `offset` in the header of `area`.
fn header_word(area: &[u8], offset: usize) -> u64 {
    let at = HEADER + offset;
    u64::from_le_bytes(area[at..at + 8].try_into().expect("8 bytes"))
}

/// The XSAVE area must be 64-byte aligned, or the CPU raises #GP.
fn check_alignment(addr: u64) -> Exec<()> {
    if addr.is_multiple_of(64) {
        Ok(())
    } else {
        Err(Trap::Exception(Exception::GeneralProtection))
    }
}
