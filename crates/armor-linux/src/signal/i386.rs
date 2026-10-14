//! Signal frames of an i386 guest (ADR-0029): what a 64-bit Linux kernel
//! writes for a 32-bit process (`ia32_setup_frame`, `ia32_setup_rt_frame`)
//! and reads back (`sigreturn`, `rt_sigreturn`).
//!
//! Layouts, from the kernel's `struct sigframe_ia32`, `struct
//! rt_sigframe_ia32`, `struct ucontext_ia32` and `struct sigcontext_32`
//! (docs/tech/signals.md):
//! - rt frame (SA_SIGINFO): pretcode, sig, pinfo, puc, a 128-byte compat
//!   `siginfo`, the `ucontext` (flags, link, stack, sigcontext, mask), and
//!   8 bytes of return code;
//! - old frame: pretcode, sig, sigcontext, an unused 624-byte FPU area,
//!   the high half of the mask, and 8 bytes of return code;
//! - FPU state above the frame: a 112-byte FSAVE header converted from
//!   FXSAVE, then the 64-byte-aligned 512-byte FXSAVE image, whose
//!   software-reserved bytes say how large the state is.

use armor_cpu::{Cpu, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP, USER_DS, USER32_CS};
use armor_mem::{AddressSpace, GuestMemory};

use super::{
    ENTRY_CLEARED_FLAGS, FIX_EFLAGS, FXSAVE_SIZE, FaultContext, Pending, SA_NODEFER, SA_RESETHAND,
    SA_RESTORER, SIGINFO_SIZE, SS_DISABLE, SS_ONSTACK, Signals, bit, field, update_host,
};

/// SA_SIGINFO: the handler takes a siginfo and a ucontext (rt frame).
const SA_SIGINFO: u64 = 4;

const SIGCONTEXT_SIZE: usize = 88;
/// The FSAVE-format header before the FXSAVE image (`struct fregs_state`).
const FREGS_SIZE: u64 = 112;
/// `struct rt_sigframe_ia32`.
const RT_FRAME_SIZE: u64 = 268;
const RT_INFO: u64 = 16;
const RT_UC: u64 = 144;
const RT_RETCODE: u64 = 260;
/// In `struct ucontext_ia32`.
const UC_STACK: u64 = 8;
const UC_MCONTEXT: u64 = 20;
const UC_SIGMASK: u64 = 108;
/// `struct sigframe_ia32`.
const FRAME_SIZE: u64 = 732;
const FRAME_SC: u64 = 8;
const FRAME_EXTRAMASK: u64 = 720;
const FRAME_RETCODE: u64 = 724;
/// `movl $__NR_ia32_rt_sigreturn, %eax; int $0x80`, and `popl %eax; movl
/// $__NR_ia32_sigreturn, %eax; int $0x80`: never run by glibc, which
/// passes its own restorer, but there for debuggers and old programs.
const RT_RETCODE_BYTES: [u8; 8] = [0xb8, 173, 0, 0, 0, 0xcd, 0x80, 0];
const RETCODE_BYTES: [u8; 8] = [0x58, 0xb8, 119, 0, 0, 0, 0xcd, 0x80];
/// `_fpx_sw_bytes` in the FXSAVE image of an i386 frame on a CPU without
/// XSAVE: magic, extended size (512 + 4 + the FSAVE header), features 0,
/// state size 512 (✅ measured on the Xeon X5675, docs/tech/signals.md).
const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
const SW_RESERVED: usize = 464;
const EXTENDED_SIZE: u32 = 512 + 4 + FREGS_SIZE as u32;

fn mask32(value: u64) -> u64 {
    value & 0xffff_ffff
}

impl Signals {
    /// The i386 version of `build_frame`: the rt frame with SA_SIGINFO,
    /// the old one without.
    pub(super) fn build_frame32(
        &mut self,
        cpu: &mut Cpu,
        mem: &AddressSpace,
        pending: &Pending,
        fault: FaultContext,
    ) -> Result<(), ()> {
        let signal = pending.signal;
        let action = self.action(signal);
        let (handler, flags, restorer, sa_mask) = (
            field(&action, 0),
            field(&action, 1),
            field(&action, 2),
            field(&action, 3),
        );
        if flags & SA_RESTORER == 0 {
            // Linux would return through the vDSO, which armor does not
            // give an i386 guest.
            return Err(());
        }
        let rt = flags & SA_SIGINFO != 0;
        let esp = mask32(cpu.regs[RSP]);
        let on_stack = self.on_alt_stack(esp);
        let use_alt = flags & super::SA_ONSTACK != 0 && !self.alt_stack.disabled && !on_stack;
        let sp = if use_alt {
            mask32(self.alt_stack.sp.wrapping_add(self.alt_stack.size))
        } else {
            esp
        };
        // FXSAVE image 64-byte aligned, the FSAVE header right below it,
        // then the frame, aligned so that ((sp + 4) & 15) == 0 on entry,
        // as after a call (Linux's get_sigframe).
        let fx = mask32(sp.wrapping_sub(FXSAVE_SIZE) & !63);
        let fpstate = mask32(fx.wrapping_sub(FREGS_SIZE));
        let size = if rt { RT_FRAME_SIZE } else { FRAME_SIZE };
        let frame = mask32((fpstate.wrapping_sub(size).wrapping_add(4) & !15).wrapping_sub(4));

        let old_mask = self.saved_mask.take().unwrap_or(self.mask);
        let image = cpu.fx_image();
        let sigcontext = sigcontext(cpu, esp, fault, fpstate, old_mask);
        let mut bytes = vec![0u8; size as usize];
        let mut put = |at: u64, data: &[u8]| {
            bytes[at as usize..at as usize + data.len()].copy_from_slice(data);
        };
        put(0, &(restorer as u32).to_le_bytes());
        put(4, &(signal as u32).to_le_bytes());
        if rt {
            put(8, &((frame + RT_INFO) as u32).to_le_bytes());
            put(12, &((frame + RT_UC) as u32).to_le_bytes());
            put(RT_INFO, &siginfo32(&pending.info));
            // uc_flags 0 (no XSAVE), uc_link 0, then the alternate stack.
            let stack_flags = if on_stack {
                SS_ONSTACK
            } else if self.alt_stack.disabled {
                SS_DISABLE
            } else {
                0
            };
            put(RT_UC + UC_STACK, &(self.alt_stack.sp as u32).to_le_bytes());
            put(RT_UC + UC_STACK + 4, &stack_flags.to_le_bytes());
            put(
                RT_UC + UC_STACK + 8,
                &(self.alt_stack.size as u32).to_le_bytes(),
            );
            put(RT_UC + UC_MCONTEXT, &sigcontext);
            put(RT_UC + UC_SIGMASK, &old_mask.to_le_bytes());
            put(RT_RETCODE, &RT_RETCODE_BYTES);
        } else {
            put(FRAME_SC, &sigcontext);
            put(FRAME_EXTRAMASK, &((old_mask >> 32) as u32).to_le_bytes());
            put(FRAME_RETCODE, &RETCODE_BYTES);
        }

        mem.write(fpstate, &fregs_header(&image)).map_err(|_| ())?;
        let mut image = image;
        image[SW_RESERVED..SW_RESERVED + 4].copy_from_slice(&FP_XSTATE_MAGIC1.to_le_bytes());
        image[SW_RESERVED + 4..SW_RESERVED + 8].copy_from_slice(&EXTENDED_SIZE.to_le_bytes());
        image[SW_RESERVED + 16..SW_RESERVED + 20]
            .copy_from_slice(&(FXSAVE_SIZE as u32).to_le_bytes());
        mem.write(fx, &image).map_err(|_| ())?;
        mem.write(frame, &bytes).map_err(|_| ())?;
        cpu.reset_fpu();

        // -mregparm=3 handlers take the arguments in eax, edx, ecx.
        cpu.regs[RAX] = signal;
        (cpu.regs[RDX], cpu.regs[RCX]) = if rt {
            (frame + RT_INFO, frame + RT_UC)
        } else {
            (0, 0)
        };
        cpu.regs[RSP] = frame;
        cpu.rip = mask32(handler);
        cpu.rflags &= !ENTRY_CLEARED_FLAGS;

        let mut mask = self.mask | sa_mask;
        if flags & SA_NODEFER == 0 {
            mask |= bit(signal);
        }
        if flags & SA_RESETHAND != 0 {
            let mut actions = self.actions();
            actions[(signal - 1) as usize][..8].fill(0);
            update_host(&actions, signal);
        }
        self.set_mask(mask);
        Ok(())
    }

    /// rt_sigreturn (`rt`) or sigreturn of an i386 guest: the frame is 4
    /// bytes below esp (the handler's `ret` popped pretcode), 8 for the old
    /// one (the return code also popped the signal number).
    pub fn sigreturn32(
        &mut self,
        cpu: &mut Cpu,
        mem: &AddressSpace,
        rt: bool,
    ) -> Result<(), String> {
        let esp = mask32(cpu.regs[RSP]);
        let name = if rt { "rt_sigreturn" } else { "sigreturn" };
        let (sc_at, mask) = if rt {
            let uc = mask32(esp.wrapping_sub(4).wrapping_add(RT_UC));
            let mut raw = [0u8; 8];
            mem.read(uc + UC_SIGMASK, &mut raw)
                .map_err(|e| format!("{name}: unreadable frame ({e})"))?;
            (uc + UC_MCONTEXT, u64::from_le_bytes(raw))
        } else {
            let frame = mask32(esp.wrapping_sub(8));
            let mut high = [0u8; 4];
            mem.read(frame + FRAME_EXTRAMASK, &mut high)
                .map_err(|e| format!("{name}: unreadable frame ({e})"))?;
            (frame + FRAME_SC, u64::from(u32::from_le_bytes(high)) << 32)
        };
        let mut sc = [0u8; SIGCONTEXT_SIZE];
        mem.read(sc_at, &mut sc)
            .map_err(|e| format!("{name}: unreadable frame ({e})"))?;
        let word = |at: usize| {
            u64::from(u32::from_le_bytes(
                sc[at..at + 4].try_into().expect("4 bytes"),
            ))
        };
        let mask = if rt { mask } else { mask | word(80) };
        let fpstate = word(76);
        if fpstate != 0 {
            // The FXSAVE image after the FSAVE header ⚠️: Linux also takes
            // the x87 environment and registers of the header, which a
            // handler could have changed.
            let mut image = [0u8; FXSAVE_SIZE as usize];
            mem.read(fpstate + FREGS_SIZE, &mut image)
                .map_err(|e| format!("{name}: unreadable FPU state ({e})"))?;
            cpu.load_fx_image(&image)
                .map_err(|what| format!("{name}: {what}"))?;
        }
        // gs and fs as the frame has them: a selector that names no segment
        // makes the return fail, as loading it fails in Linux.
        for (at, gs) in [(0, true), (4, false)] {
            let selector = word(at) as u16;
            let base = cpu
                .segment_base(selector)
                .ok_or_else(|| format!("{name}: invalid selector {selector:#x} in the frame"))?;
            if gs {
                (cpu.segments.gs, cpu.gs_base) = (selector, base);
            } else {
                (cpu.segments.fs, cpu.fs_base) = (selector, base);
            }
        }
        for (r, at) in [
            (RDI, 16),
            (RSI, 20),
            (RBP, 24),
            (RSP, 28),
            (RBX, 32),
            (RDX, 36),
            (RCX, 40),
            (RAX, 44),
        ] {
            cpu.regs[r] = word(at);
        }
        cpu.rip = word(56);
        cpu.rflags = (cpu.rflags & !FIX_EFLAGS) | (word(64) & FIX_EFLAGS);
        self.set_mask(mask);
        Ok(())
    }
}

/// `struct sigcontext_32` of the interrupted state.
fn sigcontext(
    cpu: &Cpu,
    esp: u64,
    fault: FaultContext,
    fpstate: u64,
    old_mask: u64,
) -> [u8; SIGCONTEXT_SIZE] {
    let mut sc = [0u8; SIGCONTEXT_SIZE];
    let reg = |r: usize| mask32(cpu.regs[r]);
    for (at, value) in [
        (0, u64::from(cpu.segments.gs)),
        (4, u64::from(cpu.segments.fs)),
        (8, u64::from(USER_DS)),
        (12, u64::from(USER_DS)),
        (16, reg(RDI)),
        (20, reg(RSI)),
        (24, reg(RBP)),
        (28, esp),
        (32, reg(RBX)),
        (36, reg(RDX)),
        (40, reg(RCX)),
        (44, reg(RAX)),
        (48, fault.trapno),
        (52, fault.err),
        (56, mask32(cpu.rip)),
        (60, u64::from(USER32_CS)),
        (64, cpu.rflags),
        (68, esp),
        (72, u64::from(USER_DS)),
        (76, fpstate),
        (80, old_mask),
        (84, fault.cr2),
    ] {
        sc[at..at + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }
    sc
}

/// The FSAVE-format header an x86-64 kernel writes before the FXSAVE image
/// of an i386 frame (`convert_from_fxsr`): control and status words with
/// the high half set, the full tag word, the code selector and the data
/// selector (ds) in place of the last instruction's, the eight registers
/// in 10 bytes each, the status word again and the FXSR magic 0 (✅ Xeon
/// X5675). FIP and FDP are 0: armor does not track them.
fn fregs_header(fx: &[u8; FXSAVE_SIZE as usize]) -> [u8; FREGS_SIZE as usize] {
    let mut header = [0u8; FREGS_SIZE as usize];
    let fcw = u16::from_le_bytes([fx[0], fx[1]]);
    let fsw = u16::from_le_bytes([fx[2], fx[3]]);
    let top = (fsw >> 11) & 7;
    let mut tag = 0xffff_0000u32;
    for physical in 0..8u16 {
        let value = if fx[4] >> physical & 1 == 0 {
            3 // empty
        } else {
            let st = 32 + 16 * usize::from((physical.wrapping_sub(top)) & 7);
            let exponent = u16::from_le_bytes([fx[st + 8], fx[st + 9]]) & 0x7fff;
            let significand = u64::from_le_bytes(fx[st..st + 8].try_into().expect("8 bytes"));
            match exponent {
                0x7fff => 2,
                0 if significand == 0 => 1,
                0 => 2,
                _ if significand >> 63 == 1 => 0,
                _ => 2,
            }
        };
        tag |= value << (2 * physical);
    }
    for (at, value) in [
        (0, 0xffff_0000 | u32::from(fcw)),
        (4, 0xffff_0000 | u32::from(fsw)),
        (8, tag),
        (
            12,
            u32::from_le_bytes(fx[8..12].try_into().expect("4 bytes")),
        ),
        (16, u32::from(USER32_CS)),
        (
            20,
            u32::from_le_bytes(fx[16..20].try_into().expect("4 bytes")),
        ),
        (24, 0xffff_0000 | u32::from(USER_DS)),
    ] {
        header[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    for i in 0..8 {
        header[28 + 10 * i..38 + 10 * i].copy_from_slice(&fx[32 + 16 * i..42 + 16 * i]);
    }
    header[108..110].copy_from_slice(&fsw.to_le_bytes());
    header
}

// si_code values that choose the compat layout (Linux's siginfo_layout).
const SI_USER: i32 = 0;
const SI_KERNEL: i32 = 0x80;
const SI_TIMER: i32 = -2;
const SI_SIGIO: i32 = -5;
/// The largest code of each kernel-generated class (NSIGILL, NSIGFPE,
/// NSIGSEGV, NSIGBUS, NSIGTRAP, NSIGCHLD, NSIGPOLL, NSIGSYS).
const FAULT_LIMITS: [(i32, i32); 5] = [
    (libc::SIGILL, 11),
    (libc::SIGFPE, 15),
    (libc::SIGSEGV, 10),
    (libc::SIGBUS, 5),
    (libc::SIGTRAP, 6),
];
const NSIGCHLD: i32 = 6;
const NSIGPOLL: i32 = 6;
const NSIGSYS: i32 = 2;

/// The compat (i386) `siginfo` of a 64-bit one: the same first three ints,
/// then the union at offset 12 instead of 16, with 32-bit `long`s and
/// pointers (Linux's `copy_siginfo_to_external32`).
pub fn siginfo32(info: &[u8; SIGINFO_SIZE]) -> [u8; SIGINFO_SIZE] {
    let mut out = [0u8; SIGINFO_SIZE];
    out[..12].copy_from_slice(&info[..12]);
    let int = |at: usize| i32::from_le_bytes(info[at..at + 4].try_into().expect("4 bytes"));
    let long = |at: usize| i64::from_le_bytes(info[at..at + 8].try_into().expect("8 bytes"));
    let (signal, code) = (int(0), int(8));
    let mut put = |at: usize, value: i64| {
        out[at..at + 4].copy_from_slice(&(value as i32).to_le_bytes());
    };
    let kill = |put: &mut dyn FnMut(usize, i64)| {
        put(12, i64::from(int(16)));
        put(16, i64::from(int(20)));
    };
    if code > SI_USER && code < SI_KERNEL {
        let fault = FAULT_LIMITS
            .iter()
            .any(|&(s, limit)| s == signal && code <= limit);
        if fault {
            put(12, long(16));
        } else if signal == libc::SIGCHLD && code <= NSIGCHLD {
            kill(&mut put);
            put(20, i64::from(int(24)));
            put(24, long(32));
            put(28, long(40));
        } else if signal == libc::SIGSYS && code <= NSIGSYS {
            put(12, long(16));
            put(16, i64::from(int(24)));
            put(20, i64::from(int(28)));
        } else if code <= NSIGPOLL {
            put(12, long(16));
            put(16, i64::from(int(24)));
        } else {
            kill(&mut put);
        }
    } else if code == SI_TIMER {
        put(12, i64::from(int(16)));
        put(16, i64::from(int(20)));
        put(20, long(24));
    } else if code == SI_SIGIO {
        put(12, long(16));
        put(16, i64::from(int(24)));
    } else if code < 0 {
        // pid, uid, sigval.
        kill(&mut put);
        put(20, long(24));
    } else {
        kill(&mut put);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(signal: i32, code: i32, words: &[(usize, u64)]) -> [u8; SIGINFO_SIZE] {
        let mut info = [0u8; SIGINFO_SIZE];
        info[..4].copy_from_slice(&signal.to_le_bytes());
        info[8..12].copy_from_slice(&code.to_le_bytes());
        for &(at, value) in words {
            info[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        info
    }

    fn ints(info: &[u8; SIGINFO_SIZE], n: usize) -> Vec<i32> {
        (0..n)
            .map(|i| i32::from_le_bytes(info[4 * i..4 * i + 4].try_into().unwrap()))
            .collect()
    }

    #[test]
    fn siginfo_union_moves_to_offset_12() {
        // kill: pid and uid.
        let kill = info(10, 0, &[(16, 1234 | 1000 << 32)]);
        assert_eq!(ints(&siginfo32(&kill), 5), [10, 0, 0, 1234, 1000]);
        // tgkill (SI_TKILL, rt layout): pid, uid, sigval.
        let tkill = info(10, -6, &[(16, 77 | 5 << 32), (24, 0xabcd)]);
        assert_eq!(ints(&siginfo32(&tkill), 6), [10, 0, -6, 77, 5, 0xabcd]);
        // A fault: the address, 32 bits.
        let segv = info(11, 1, &[(16, 0x10)]);
        assert_eq!(ints(&siginfo32(&segv), 5), [11, 0, 1, 0x10, 0]);
        // SIGCHLD: pid, uid, status, utime, stime.
        let chld = info(17, 1, &[(16, 9 | 1000 << 32), (24, 3), (32, 11), (40, 12)]);
        assert_eq!(ints(&siginfo32(&chld), 8), [17, 0, 1, 9, 1000, 3, 11, 12]);
        // A timer: tid, overrun, sigval.
        let timer = info(14, -2, &[(16, 4 | 2 << 32), (24, 99)]);
        assert_eq!(ints(&siginfo32(&timer), 6), [14, 0, -2, 4, 2, 99]);
    }

    #[test]
    fn fsave_header_is_converted_from_fxsave() {
        // Two registers on the stack (TOP = 6): 1.5 in ST0 (physical 6),
        // zero in ST1 (physical 7).
        let mut fx = [0u8; FXSAVE_SIZE as usize];
        fx[0..2].copy_from_slice(&0x037fu16.to_le_bytes());
        fx[2..4].copy_from_slice(&0x3000u16.to_le_bytes());
        fx[4] = 0b1100_0000;
        fx[32..40].copy_from_slice(&0xc000_0000_0000_0000u64.to_le_bytes());
        fx[40..42].copy_from_slice(&0x3fffu16.to_le_bytes());
        let header = fregs_header(&fx);
        let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        assert_eq!(word(0), 0xffff_037f);
        assert_eq!(word(4), 0xffff_3000);
        // Physical 6 valid (00), physical 7 zero (01), the others empty.
        assert_eq!(word(8), 0xffff_4fff);
        assert_eq!(word(16), 0x23);
        assert_eq!(word(24), 0xffff_002b);
        assert_eq!(&header[28..38], &fx[32..42]);
        assert_eq!(&header[108..112], &[0x00, 0x30, 0, 0]);
    }
}
