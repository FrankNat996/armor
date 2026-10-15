//! The system's own calls (ADR-0044), done by the host kernel for the
//! guest: those of administrators (mounts and the new mount API, `chroot`,
//! `pivot_root`, swap, `reboot`, host and domain names, kernel modules,
//! `kexec`, accounting, quotas, the clock, `syslog`), which an
//! unprivileged process sees refused with EPERM as it would natively, and
//! keys, NUMA policies, `perf_event_open`, `bpf`, fanotify, file handles,
//! `cachestat`, `statmount`, `listmount` and the LSM calls.
//!
//! Strings and structures are copied (their layouts are the same on both
//! architectures: ✅ `sizeof` of `struct timex` 208, `if_dqblk` 72,
//! `if_dqinfo` 24, `perf_event_attr` 136, `mount_attr` 32, `kexec_segment`
//! 32 under both compilers, 2026-10-15), paths resolved in the guest
//! root. Pointers nested in `perf_event_attr`, `bpf`'s attributes and the
//! kexec segments are the guest's addresses, which are armor's
//! identity-mapped memory: these calls are for privileged programs.

use std::ffi::CString;

use armor_mem::{AddressSpace, GuestMemory};

use super::{
    Process, SysResult, copy_in, copy_out, err, host_errno, open_flags_to_host, read_path,
};

pub mod nr {
    pub const SYSLOG: u64 = 103;
    pub const PIVOT_ROOT: u64 = 155;
    pub const ADJTIMEX: u64 = 159;
    pub const CHROOT: u64 = 161;
    pub const ACCT: u64 = 163;
    pub const SETTIMEOFDAY: u64 = 164;
    pub const MOUNT: u64 = 165;
    pub const UMOUNT2: u64 = 166;
    pub const SWAPON: u64 = 167;
    pub const SWAPOFF: u64 = 168;
    pub const REBOOT: u64 = 169;
    pub const SETHOSTNAME: u64 = 170;
    pub const SETDOMAINNAME: u64 = 171;
    pub const INIT_MODULE: u64 = 175;
    pub const DELETE_MODULE: u64 = 176;
    pub const QUOTACTL: u64 = 179;
    pub const CLOCK_SETTIME: u64 = 227;
    pub const MBIND: u64 = 237;
    pub const SET_MEMPOLICY: u64 = 238;
    pub const GET_MEMPOLICY: u64 = 239;
    pub const KEXEC_LOAD: u64 = 246;
    pub const ADD_KEY: u64 = 248;
    pub const REQUEST_KEY: u64 = 249;
    pub const KEYCTL: u64 = 250;
    pub const MIGRATE_PAGES: u64 = 256;
    pub const MOVE_PAGES: u64 = 279;
    pub const PERF_EVENT_OPEN: u64 = 298;
    pub const FANOTIFY_INIT: u64 = 300;
    pub const FANOTIFY_MARK: u64 = 301;
    pub const NAME_TO_HANDLE_AT: u64 = 303;
    pub const OPEN_BY_HANDLE_AT: u64 = 304;
    pub const CLOCK_ADJTIME: u64 = 305;
    pub const FINIT_MODULE: u64 = 313;
    pub const KEXEC_FILE_LOAD: u64 = 320;
    pub const BPF: u64 = 321;
    pub const OPEN_TREE: u64 = 428;
    pub const MOVE_MOUNT: u64 = 429;
    pub const FSOPEN: u64 = 430;
    pub const FSCONFIG: u64 = 431;
    pub const FSMOUNT: u64 = 432;
    pub const FSPICK: u64 = 433;
    pub const MOUNT_SETATTR: u64 = 442;
    pub const QUOTACTL_FD: u64 = 443;
    pub const SET_MEMPOLICY_HOME_NODE: u64 = 450;
    pub const CACHESTAT: u64 = 451;
    pub const STATMOUNT: u64 = 457;
    pub const LISTMOUNT: u64 = 458;
    pub const LSM_GET_SELF_ATTR: u64 = 459;
    pub const LSM_SET_SELF_ATTR: u64 = 460;
    pub const LSM_LIST_MODULES: u64 = 461;
}

pub const HANDLED: &[u64] = &[
    nr::SYSLOG,
    nr::PIVOT_ROOT,
    nr::ADJTIMEX,
    nr::CHROOT,
    nr::ACCT,
    nr::SETTIMEOFDAY,
    nr::MOUNT,
    nr::UMOUNT2,
    nr::SWAPON,
    nr::SWAPOFF,
    nr::REBOOT,
    nr::SETHOSTNAME,
    nr::SETDOMAINNAME,
    nr::INIT_MODULE,
    nr::DELETE_MODULE,
    nr::QUOTACTL,
    nr::CLOCK_SETTIME,
    nr::MBIND,
    nr::SET_MEMPOLICY,
    nr::GET_MEMPOLICY,
    nr::KEXEC_LOAD,
    nr::ADD_KEY,
    nr::REQUEST_KEY,
    nr::KEYCTL,
    nr::MIGRATE_PAGES,
    nr::MOVE_PAGES,
    nr::PERF_EVENT_OPEN,
    nr::FANOTIFY_INIT,
    nr::FANOTIFY_MARK,
    nr::NAME_TO_HANDLE_AT,
    nr::OPEN_BY_HANDLE_AT,
    nr::CLOCK_ADJTIME,
    nr::FINIT_MODULE,
    nr::KEXEC_FILE_LOAD,
    nr::BPF,
    nr::OPEN_TREE,
    nr::MOVE_MOUNT,
    nr::FSOPEN,
    nr::FSCONFIG,
    nr::FSMOUNT,
    nr::FSPICK,
    nr::MOUNT_SETATTR,
    nr::QUOTACTL_FD,
    nr::SET_MEMPOLICY_HOME_NODE,
    nr::CACHESTAT,
    nr::STATMOUNT,
    nr::LISTMOUNT,
    nr::LSM_GET_SELF_ATTR,
    nr::LSM_SET_SELF_ATTR,
    nr::LSM_LIST_MODULES,
];

// AArch64 numbers the libc crate does not name (asm-generic/unistd.h).
const SYS_MOUNT_SETATTR: libc::c_long = 442;
const SYS_QUOTACTL_FD: libc::c_long = 443;
const SYS_SET_MEMPOLICY_HOME_NODE: libc::c_long = 450;
const SYS_CACHESTAT: libc::c_long = 451;
const SYS_STATMOUNT: libc::c_long = 457;
const SYS_LISTMOUNT: libc::c_long = 458;
const SYS_LSM_GET_SELF_ATTR: libc::c_long = 459;
const SYS_LSM_SET_SELF_ATTR: libc::c_long = 460;
const SYS_LSM_LIST_MODULES: libc::c_long = 461;

/// The largest buffer armor copies for one of these calls (a kernel module
/// image, a key payload, a syslog read).
const BUFFER_MAX: u64 = 64 << 20;

pub(super) fn dispatch(
    mem: &AddressSpace,
    process: &Process,
    number: u64,
    a: [u64; 6],
) -> Option<Result<SysResult, String>> {
    let c = Copies { mem, process };
    let result = match number {
        nr::SYSLOG => c.syslog(a[0], a[1], a[2]),
        nr::PIVOT_ROOT => c.paths(libc::SYS_pivot_root, &[Arg::Path(a[0]), Arg::Path(a[1])]),
        nr::CHROOT => c.paths(libc::SYS_chroot, &[Arg::Path(a[0])]),
        nr::ACCT => c.paths(libc::SYS_acct, &[Arg::OptPath(a[0])]),
        nr::SWAPON => c.paths(libc::SYS_swapon, &[Arg::Path(a[0]), Arg::Int(a[1])]),
        nr::SWAPOFF => c.paths(libc::SYS_swapoff, &[Arg::Path(a[0])]),
        nr::UMOUNT2 => c.paths(libc::SYS_umount2, &[Arg::Path(a[0]), Arg::Int(a[1])]),
        nr::MOUNT => c.paths(
            libc::SYS_mount,
            &[
                Arg::OptString(a[0]),
                Arg::Path(a[1]),
                Arg::OptString(a[2]),
                Arg::Int(a[3]),
                Arg::OptData(a[4]),
            ],
        ),
        nr::ADJTIMEX => c.in_out(libc::SYS_adjtimex, &[], a[0], 208, &[]),
        nr::CLOCK_ADJTIME => c.in_out(libc::SYS_clock_adjtime, &[a[0]], a[1], 208, &[]),
        nr::SETTIMEOFDAY => c.settimeofday(a[0], a[1]),
        nr::CLOCK_SETTIME => c.copied(
            libc::SYS_clock_settime,
            &[Arg::Int(a[0]), Arg::Bytes(a[1], 16)],
        ),
        nr::REBOOT => c.reboot(a),
        // The kernel checks the privilege before the length: the name is
        // copied up to a page and the host decides.
        nr::SETHOSTNAME | nr::SETDOMAINNAME => {
            let number = if number == nr::SETHOSTNAME {
                libc::SYS_sethostname
            } else {
                libc::SYS_setdomainname
            };
            c.copied(number, &[Arg::Bytes(a[0], a[1].min(4096)), Arg::Int(a[1])])
        }
        nr::INIT_MODULE => c.copied(
            libc::SYS_init_module,
            &[Arg::Bytes(a[0], a[1]), Arg::Int(a[1]), Arg::OptString(a[2])],
        ),
        nr::FINIT_MODULE => c.copied(
            libc::SYS_finit_module,
            &[Arg::Int(a[0]), Arg::OptString(a[1]), Arg::Int(a[2])],
        ),
        nr::DELETE_MODULE => c.copied(
            libc::SYS_delete_module,
            &[Arg::OptString(a[0]), Arg::Int(a[1])],
        ),
        nr::QUOTACTL | nr::QUOTACTL_FD => return c.quotactl(number, a),
        nr::KEXEC_LOAD => c.copied(
            libc::SYS_kexec_load,
            &[
                Arg::Int(a[0]),
                Arg::Int(a[1]),
                Arg::Bytes(a[2], a[1].min(16) * 32),
                Arg::Int(a[3]),
            ],
        ),
        nr::KEXEC_FILE_LOAD => c.copied(
            libc::SYS_kexec_file_load,
            &[
                Arg::Int(a[0]),
                Arg::Int(a[1]),
                Arg::Int(a[2]),
                Arg::Bytes(a[3], a[2]),
                Arg::Int(a[4]),
            ],
        ),
        nr::MBIND => c.copied(
            libc::SYS_mbind,
            &[
                Arg::Int(a[0]),
                Arg::Int(a[1]),
                Arg::Int(a[2]),
                Arg::OptBytes(a[3], nodemask_bytes(a[4])),
                Arg::Int(a[4]),
                Arg::Int(a[5]),
            ],
        ),
        nr::SET_MEMPOLICY => c.copied(
            libc::SYS_set_mempolicy,
            &[
                Arg::Int(a[0]),
                Arg::OptBytes(a[1], nodemask_bytes(a[2])),
                Arg::Int(a[2]),
            ],
        ),
        nr::GET_MEMPOLICY => c.get_mempolicy(a),
        nr::MIGRATE_PAGES => c.copied(
            libc::SYS_migrate_pages,
            &[
                Arg::Int(a[0]),
                Arg::Int(a[1]),
                Arg::OptBytes(a[2], nodemask_bytes(a[1])),
                Arg::OptBytes(a[3], nodemask_bytes(a[1])),
            ],
        ),
        nr::MOVE_PAGES => c.move_pages(a),
        nr::SET_MEMPOLICY_HOME_NODE => c.copied(
            SYS_SET_MEMPOLICY_HOME_NODE,
            &[
                Arg::Int(a[0]),
                Arg::Int(a[1]),
                Arg::Int(a[2]),
                Arg::Int(a[3]),
            ],
        ),
        nr::ADD_KEY => c.copied(
            libc::SYS_add_key,
            &[
                Arg::OptString(a[0]),
                Arg::OptString(a[1]),
                Arg::OptBytes(a[2], a[3]),
                Arg::Int(a[3]),
                Arg::Int(a[4]),
            ],
        ),
        nr::REQUEST_KEY => c.copied(
            libc::SYS_request_key,
            &[
                Arg::OptString(a[0]),
                Arg::OptString(a[1]),
                Arg::OptString(a[2]),
                Arg::Int(a[3]),
            ],
        ),
        nr::KEYCTL => return c.keyctl(a),
        nr::PERF_EVENT_OPEN => c.perf_event_open(a),
        nr::FANOTIFY_INIT => c.copied(
            libc::SYS_fanotify_init,
            &[Arg::Int(a[0]), Arg::Int(open_flags_to_host(a[1]))],
        ),
        nr::FANOTIFY_MARK => c.paths(
            libc::SYS_fanotify_mark,
            &[
                Arg::Int(a[0]),
                Arg::Int(a[1]),
                Arg::Int(a[2]),
                Arg::Int(a[3]),
                Arg::OptPath(a[4]),
            ],
        ),
        nr::NAME_TO_HANDLE_AT => c.name_to_handle_at(a),
        nr::OPEN_BY_HANDLE_AT => c.open_by_handle_at(a),
        nr::BPF => c.bpf(a[0], a[1], a[2]),
        nr::OPEN_TREE => c.paths(
            libc::SYS_open_tree,
            &[Arg::Int(a[0]), Arg::Path(a[1]), Arg::Int(a[2])],
        ),
        nr::MOVE_MOUNT => c.paths(
            libc::SYS_move_mount,
            &[
                Arg::Int(a[0]),
                Arg::Path(a[1]),
                Arg::Int(a[2]),
                Arg::Path(a[3]),
                Arg::Int(a[4]),
            ],
        ),
        nr::FSOPEN => c.copied(libc::SYS_fsopen, &[Arg::OptString(a[0]), Arg::Int(a[1])]),
        nr::FSCONFIG => return c.fsconfig(a),
        nr::FSMOUNT => c.copied(
            libc::SYS_fsmount,
            &[Arg::Int(a[0]), Arg::Int(a[1]), Arg::Int(a[2])],
        ),
        nr::FSPICK => c.paths(
            libc::SYS_fspick,
            &[Arg::Int(a[0]), Arg::Path(a[1]), Arg::Int(a[2])],
        ),
        nr::MOUNT_SETATTR => {
            if a[4] > 4096 {
                Err(err(libc::E2BIG))
            } else if a[4] < 32 {
                Err(err(libc::EINVAL))
            } else {
                c.paths(
                    SYS_MOUNT_SETATTR,
                    &[
                        Arg::Int(a[0]),
                        Arg::Path(a[1]),
                        Arg::Int(a[2]),
                        Arg::Bytes(a[3], a[4]),
                        Arg::Int(a[4]),
                    ],
                )
            }
        }
        nr::CACHESTAT => c.in_out(
            SYS_CACHESTAT,
            &[a[0]],
            a[2],
            40,
            &[Arg::Bytes(a[1], 16), Arg::Int(a[3])],
        ),
        nr::STATMOUNT => c.statmount(a),
        nr::LISTMOUNT => c.listmount(a),
        nr::LSM_GET_SELF_ATTR => c.lsm_get_self_attr(a),
        nr::LSM_SET_SELF_ATTR => c.copied(
            SYS_LSM_SET_SELF_ATTR,
            &[
                Arg::Int(a[0]),
                Arg::Bytes(a[1], a[2]),
                Arg::Int(a[2]),
                Arg::Int(a[3]),
            ],
        ),
        nr::LSM_LIST_MODULES => c.lsm_list_modules(a),
        _ => return None,
    };
    Some(Ok(result))
}

/// One argument of a call whose guest memory is copied before it runs.
enum Arg {
    Int(u64),
    /// A path, resolved in the guest root.
    Path(u64),
    /// A path, or null.
    OptPath(u64),
    /// A string, or null.
    OptString(u64),
    /// `len` bytes.
    Bytes(u64, u64),
    /// `len` bytes, or null.
    OptBytes(u64, u64),
    /// mount's data: a string of at most a page, or null.
    OptData(u64),
}

/// The bytes of a node mask of `maxnode` bits (as the kernel reads it, in
/// whole `unsigned long`s), at most a page.
fn nodemask_bytes(maxnode: u64) -> u64 {
    maxnode.div_ceil(64).min(512) * 8
}

/// A host value: an integer, or a pointer to a copy.
enum Held {
    Int(u64),
    Str(CString),
    Bytes(Vec<u8>),
}

impl Held {
    fn value(&self) -> u64 {
        match self {
            Held::Int(v) => *v,
            Held::Str(s) => s.as_ptr() as u64,
            Held::Bytes(b) => b.as_ptr() as u64,
        }
    }
}

fn raw(number: libc::c_long, args: &[u64]) -> SysResult {
    let arg = |i: usize| args.get(i).copied().unwrap_or(0);
    // SAFETY: callers pass integers and pointers to local copies (or, for
    // the calls the module's description names, guest addresses).
    let r = unsafe { libc::syscall(number, arg(0), arg(1), arg(2), arg(3), arg(4), arg(5)) };
    if r < 0 { Err(host_errno()) } else { Ok(r) }
}

struct Copies<'a> {
    mem: &'a AddressSpace,
    process: &'a Process,
}

impl Copies<'_> {
    fn bytes(&self, at: u64, len: u64) -> Result<Vec<u8>, i64> {
        if len > BUFFER_MAX {
            return Err(err(libc::EINVAL));
        }
        let mut out = vec![0u8; len as usize];
        self.mem.read(at, &mut out).map_err(|_| err(libc::EFAULT))?;
        Ok(out)
    }

    /// A string of at most `limit` bytes, cut there if it has no NUL.
    fn data(&self, at: u64, limit: usize) -> Result<Vec<u8>, i64> {
        let mut out = Vec::new();
        while out.len() < limit {
            let mut byte = [0u8];
            if self.mem.read(at + out.len() as u64, &mut byte).is_err() {
                if out.is_empty() {
                    return Err(err(libc::EFAULT));
                }
                break;
            }
            out.push(byte[0]);
            if byte[0] == 0 {
                break;
            }
        }
        if out.last() != Some(&0) {
            out.push(0);
        }
        Ok(out)
    }

    fn hold(&self, arg: &Arg) -> Result<Held, i64> {
        Ok(match *arg {
            Arg::Int(v) => Held::Int(v),
            Arg::Path(at) => Held::Str(self.process.root.resolve_c(read_path(self.mem, at)?)),
            Arg::OptPath(0) | Arg::OptString(0) | Arg::OptBytes(0, _) | Arg::OptData(0) => {
                Held::Int(0)
            }
            Arg::OptPath(at) => Held::Str(self.process.root.resolve_c(read_path(self.mem, at)?)),
            Arg::OptString(at) => Held::Str(read_path(self.mem, at)?),
            Arg::Bytes(at, len) | Arg::OptBytes(at, len) => Held::Bytes(self.bytes(at, len)?),
            Arg::OptData(at) => Held::Bytes(self.data(at, 4096)?),
        })
    }

    /// The call with every argument held on the host.
    fn copied(&self, number: libc::c_long, args: &[Arg]) -> SysResult {
        let held = args
            .iter()
            .map(|a| self.hold(a))
            .collect::<Result<Vec<_>, _>>()?;
        let values: Vec<u64> = held.iter().map(Held::value).collect();
        raw(number, &values)
    }

    /// `copied`, for calls whose arguments include paths.
    fn paths(&self, number: libc::c_long, args: &[Arg]) -> SysResult {
        self.copied(number, args)
    }

    /// A call whose structure at `at` (`size` bytes) is read and written
    /// back: `before` come first, then the structure, then `after`.
    fn in_out(
        &self,
        number: libc::c_long,
        before: &[u64],
        at: u64,
        size: u64,
        after: &[Arg],
    ) -> SysResult {
        let mut structure = self.bytes(at, size)?;
        let held = after
            .iter()
            .map(|a| self.hold(a))
            .collect::<Result<Vec<_>, _>>()?;
        let mut values = before.to_vec();
        // cachestat takes its range before the result: the caller lists it
        // in `after`, which goes first then.
        if number == SYS_CACHESTAT {
            values.push(held[0].value());
            values.push(structure.as_mut_ptr() as u64);
            values.extend(held[1..].iter().map(Held::value));
        } else {
            values.push(structure.as_mut_ptr() as u64);
            values.extend(held.iter().map(Held::value));
        }
        let r = raw(number, &values);
        // adjtimex writes the clock's state back even when it returns an
        // error state such as TIME_ERROR (a positive value).
        if r.is_ok() {
            copy_out(self.mem, at, &structure)?;
        }
        r
    }

    fn settimeofday(&self, tv: u64, tz: u64) -> SysResult {
        self.copied(
            libc::SYS_settimeofday,
            &[Arg::OptBytes(tv, 16), Arg::OptBytes(tz, 8)],
        )
    }

    /// syslog(2): the read actions (2, 3, 4) fill `buf`.
    fn syslog(&self, action: u64, buf: u64, len: u64) -> SysResult {
        if matches!(action, 2..=4) {
            let len = len.min(BUFFER_MAX);
            let mut out = vec![0u8; len as usize];
            let n = raw(libc::SYS_syslog, &[action, out.as_mut_ptr() as u64, len])?;
            copy_out(self.mem, buf, &out[..n as usize])?;
            return Ok(n);
        }
        raw(libc::SYS_syslog, &[action, 0, len])
    }

    /// reboot(2): LINUX_REBOOT_CMD_RESTART2 takes a command string.
    fn reboot(&self, a: [u64; 6]) -> SysResult {
        const RESTART2: u64 = 0xa1b2_c3d4;
        if a[2] as u32 as u64 == RESTART2 {
            self.copied(
                libc::SYS_reboot,
                &[
                    Arg::Int(a[0]),
                    Arg::Int(a[1]),
                    Arg::Int(a[2]),
                    Arg::OptString(a[3]),
                ],
            )
        } else {
            raw(libc::SYS_reboot, &[a[0], a[1], a[2], 0])
        }
    }

    /// quotactl(2) and quotactl_fd(2): the VFS quota commands, with the
    /// structure each one reads or writes; XFS commands are not supported.
    fn quotactl(&self, number: u64, a: [u64; 6]) -> Option<Result<SysResult, String>> {
        let (cmd, id, addr) = if number == nr::QUOTACTL {
            (a[0], a[2], a[3])
        } else {
            (a[1], a[2], a[3])
        };
        let sub = (cmd as u32 >> 8) as u64;
        // (structure size, read from the guest, written back)
        let (size, input, output) = match sub {
            0x0080_0001 | 0x0080_0003 => (0, false, false), // Q_SYNC, Q_QUOTAOFF
            0x0080_0002 => (u64::MAX, false, false),        // Q_QUOTAON: a path
            0x0080_0004 => (4, false, true),                // Q_GETFMT
            0x0080_0005 => (24, false, true),               // Q_GETINFO
            0x0080_0006 => (24, true, false),               // Q_SETINFO
            0x0080_0007 => (72, false, true),               // Q_GETQUOTA
            0x0080_0008 => (72, true, false),               // Q_SETQUOTA
            0x0080_0009 => (80, false, true),               // Q_GETNEXTQUOTA
            _ => return Some(Err(format!("quotactl command {cmd:#x}"))),
        };
        let result = (|| {
            let path_held;
            let mut structure = vec![0u8; if size == u64::MAX { 0 } else { size as usize }];
            if input {
                structure = self.bytes(addr, size)?;
            }
            let addr_value = if size == u64::MAX {
                path_held = self.process.root.resolve_c(read_path(self.mem, addr)?);
                path_held.as_ptr() as u64
            } else if size == 0 {
                0
            } else {
                structure.as_mut_ptr() as u64
            };
            let r = if number == nr::QUOTACTL {
                let special = self.hold(&Arg::OptPath(a[1]))?;
                raw(libc::SYS_quotactl, &[cmd, special.value(), id, addr_value])
            } else {
                raw(SYS_QUOTACTL_FD, &[a[0], cmd, id, addr_value])
            }?;
            if output {
                copy_out(self.mem, addr, &structure)?;
            }
            Ok(r)
        })();
        Some(Ok(result))
    }

    /// get_mempolicy(2): mode (out), nodemask (out), maxnode, addr, flags.
    fn get_mempolicy(&self, a: [u64; 6]) -> SysResult {
        let mut mode = 0i32;
        let mut mask = vec![0u8; nodemask_bytes(a[2]) as usize];
        let mode_ptr = if a[0] != 0 { &raw mut mode as u64 } else { 0 };
        let mask_ptr = if a[1] != 0 {
            mask.as_mut_ptr() as u64
        } else {
            0
        };
        let r = raw(
            libc::SYS_get_mempolicy,
            &[mode_ptr, mask_ptr, a[2], a[3], a[4]],
        )?;
        if a[0] != 0 {
            copy_out(self.mem, a[0], &mode.to_le_bytes())?;
        }
        if a[1] != 0 {
            copy_out(self.mem, a[1], &mask)?;
        }
        Ok(r)
    }

    /// move_pages(2): pid, count, pages (guest addresses), nodes, status
    /// (out), flags.
    fn move_pages(&self, a: [u64; 6]) -> SysResult {
        let count = a[1].min(1 << 16);
        let pages = self.bytes(a[2], count * 8)?;
        let nodes = if a[3] != 0 {
            Some(self.bytes(a[3], count * 4)?)
        } else {
            None
        };
        let mut status = vec![0u8; count as usize * 4];
        let r = raw(
            libc::SYS_move_pages,
            &[
                a[0],
                count,
                pages.as_ptr() as u64,
                nodes.as_ref().map_or(0, |n| n.as_ptr() as u64),
                status.as_mut_ptr() as u64,
                a[5],
            ],
        )?;
        copy_out(self.mem, a[4], &status)?;
        Ok(r)
    }

    /// keyctl(2), operation by operation: plain arguments, strings,
    /// payloads and output buffers. The Diffie-Hellman and public-key
    /// operations (23 to 28), with nested structures, are not supported;
    /// unknown operations are EOPNOTSUPP, as in Linux.
    fn keyctl(&self, a: [u64; 6]) -> Option<Result<SysResult, String>> {
        let op = a[0];
        let int = |i: usize| Arg::Int(a[i]);
        let result = match op {
            // GET_KEYRING_ID, REVOKE, CHOWN, SETPERM, CLEAR, LINK, UNLINK,
            // NEGATE, SET_REQKEY_KEYRING, SET_TIMEOUT, ASSUME_AUTHORITY,
            // SESSION_TO_PARENT, REJECT, INVALIDATE, GET_PERSISTENT, MOVE,
            // WATCH_KEY: integers only.
            0 | 3 | 4 | 5 | 7 | 8 | 9 | 13 | 14 | 15 | 16 | 18 | 19 | 21 | 22 | 30 | 32 => {
                raw(libc::SYS_keyctl, &a[..5])
            }
            // JOIN_SESSION_KEYRING (name or null).
            1 => self.copied(libc::SYS_keyctl, &[int(0), Arg::OptString(a[1])]),
            // UPDATE (id, payload, plen), INSTANTIATE (+ keyring).
            2 | 12 => self.copied(
                libc::SYS_keyctl,
                &[int(0), int(1), Arg::OptBytes(a[2], a[3]), int(3), int(4)],
            ),
            // SEARCH (keyring, type, description, destination).
            10 => self.copied(
                libc::SYS_keyctl,
                &[
                    int(0),
                    int(1),
                    Arg::OptString(a[2]),
                    Arg::OptString(a[3]),
                    int(4),
                ],
            ),
            // RESTRICT_KEYRING (keyring, type, restriction).
            29 => self.copied(
                libc::SYS_keyctl,
                &[int(0), int(1), Arg::OptString(a[2]), Arg::OptString(a[3])],
            ),
            // DESCRIBE, READ, GET_SECURITY (id, buffer, length) and
            // CAPABILITIES (buffer, length): the needed size is returned.
            6 | 11 | 17 | 31 => {
                let (buf, len) = if op == 31 { (a[1], a[2]) } else { (a[2], a[3]) };
                let len = len.min(BUFFER_MAX);
                let mut out = vec![0u8; len as usize];
                let ptr = if buf != 0 { out.as_mut_ptr() as u64 } else { 0 };
                let r = if op == 31 {
                    raw(libc::SYS_keyctl, &[op, ptr, len])
                } else {
                    raw(libc::SYS_keyctl, &[op, a[1], ptr, len])
                };
                r.and_then(|n| {
                    if buf != 0 {
                        copy_out(self.mem, buf, &out[..(n as u64).min(len) as usize])?;
                    }
                    Ok(n)
                })
            }
            // INSTANTIATE_IOV (id, iovec, count, keyring): the segments are
            // gathered into one payload and instantiated as one.
            20 => {
                let iov = (|| {
                    let count = a[3].min(1024);
                    let raw_iov = self.bytes(a[2], count * 16)?;
                    let mut payload = Vec::new();
                    for e in raw_iov.chunks(16) {
                        let base = u64::from_le_bytes(e[..8].try_into().expect("8 bytes"));
                        let len = u64::from_le_bytes(e[8..].try_into().expect("8 bytes"));
                        payload.extend(self.bytes(base, len)?);
                    }
                    Ok(payload)
                })();
                iov.and_then(|payload| {
                    raw(
                        libc::SYS_keyctl,
                        &[
                            12,
                            a[1],
                            payload.as_ptr() as u64,
                            payload.len() as u64,
                            a[4],
                        ],
                    )
                })
            }
            23..=28 => return Some(Err(format!("keyctl operation {op}"))),
            // Operations the kernel does not know.
            _ => Err(err(libc::EOPNOTSUPP)),
        };
        Some(Ok(result))
    }

    /// perf_event_open(2): `struct perf_event_attr`, whose size is at
    /// offset 4 (0 means the first version, 64 bytes).
    fn perf_event_open(&self, a: [u64; 6]) -> SysResult {
        // The kernel checks permissions before the size: armor copies at
        // least the first version and lets the host judge the size field.
        let size = u32::from_le_bytes(
            copy_in::<8>(self.mem, a[0])?[4..8]
                .try_into()
                .expect("4 bytes"),
        );
        let copy = u64::from(size).clamp(64, 4096);
        let attr = self.bytes(a[0], copy)?;
        raw(
            libc::SYS_perf_event_open,
            &[attr.as_ptr() as u64, a[1], a[2], a[3], a[4]],
        )
    }

    /// name_to_handle_at(2): dirfd, path, handle (`handle_bytes`, type,
    /// bytes), mount id (int, or u64 with AT_HANDLE_MNT_ID_UNIQUE), flags.
    fn name_to_handle_at(&self, a: [u64; 6]) -> SysResult {
        const MAX_HANDLE_SZ: u64 = 128;
        const AT_HANDLE_MNT_ID_UNIQUE: u64 = 1;
        let room = u64::from(u32::from_le_bytes(copy_in::<4>(self.mem, a[2])?)).min(MAX_HANDLE_SZ);
        let mut handle = vec![0u8; 8 + room as usize];
        handle[..4].copy_from_slice(&(room as u32).to_le_bytes());
        let path = self.process.root.resolve_c(read_path(self.mem, a[1])?);
        let mut mount_id = [0u8; 8];
        let r = raw(
            libc::SYS_name_to_handle_at,
            &[
                a[0],
                path.as_ptr() as u64,
                handle.as_mut_ptr() as u64,
                mount_id.as_mut_ptr() as u64,
                a[4],
            ],
        );
        // EOVERFLOW still tells the size needed, in handle_bytes.
        let needed = u32::from_le_bytes(handle[..4].try_into().expect("4 bytes")) as usize;
        match r {
            Ok(_) => copy_out(self.mem, a[2], &handle[..8 + needed.min(room as usize)])?,
            Err(e) if e == err(libc::EOVERFLOW) => copy_out(self.mem, a[2], &handle[..4])?,
            Err(_) => {}
        }
        if r.is_ok() {
            let id_size = if a[4] & AT_HANDLE_MNT_ID_UNIQUE != 0 {
                8
            } else {
                4
            };
            copy_out(self.mem, a[3], &mount_id[..id_size])?;
        }
        r
    }

    /// open_by_handle_at(2): mount fd, handle, flags (translated).
    fn open_by_handle_at(&self, a: [u64; 6]) -> SysResult {
        let bytes = u64::from(u32::from_le_bytes(copy_in::<4>(self.mem, a[1])?));
        if bytes == 0 || bytes > 128 {
            return Err(err(libc::EINVAL));
        }
        let handle = self.bytes(a[1], 8 + bytes)?;
        raw(
            libc::SYS_open_by_handle_at,
            &[a[0], handle.as_ptr() as u64, open_flags_to_host(a[2])],
        )
    }

    /// bpf(2): the attributes are copied in and back (some commands write
    /// results into them); pointers inside them stay the guest's.
    fn bpf(&self, cmd: u64, attr: u64, size: u64) -> SysResult {
        if size > 4096 {
            return Err(err(libc::E2BIG));
        }
        let mut copy = self.bytes(attr, size)?;
        let r = raw(libc::SYS_bpf, &[cmd, copy.as_mut_ptr() as u64, size]);
        if r.is_ok() {
            copy_out(self.mem, attr, &copy)?;
        }
        r
    }

    /// fsconfig(2): fd, command, key, value, aux; what value is depends on
    /// the command.
    fn fsconfig(&self, a: [u64; 6]) -> Option<Result<SysResult, String>> {
        let key = Arg::OptString(a[2]);
        let args = match a[1] {
            // SET_FLAG, SET_FD, CMD_CREATE, CMD_RECONFIGURE, CMD_CREATE_EXCL
            0 | 5..=8 => vec![
                Arg::Int(a[0]),
                Arg::Int(a[1]),
                key,
                Arg::Int(a[3]),
                Arg::Int(a[4]),
            ],
            // SET_STRING
            1 => vec![
                Arg::Int(a[0]),
                Arg::Int(a[1]),
                key,
                Arg::OptString(a[3]),
                Arg::Int(a[4]),
            ],
            // SET_BINARY: aux is the length
            2 => vec![
                Arg::Int(a[0]),
                Arg::Int(a[1]),
                key,
                Arg::OptBytes(a[3], a[4]),
                Arg::Int(a[4]),
            ],
            // SET_PATH, SET_PATH_EMPTY: aux is a directory descriptor
            3 | 4 => vec![
                Arg::Int(a[0]),
                Arg::Int(a[1]),
                key,
                Arg::OptPath(a[3]),
                Arg::Int(a[4]),
            ],
            _ => return Some(Ok(Err(err(libc::EOPNOTSUPP)))),
        };
        Some(Ok(self.copied(libc::SYS_fsconfig, &args)))
    }

    /// statmount(2): request (`struct mnt_id_req`, its size first), buffer
    /// (out), size, flags.
    fn statmount(&self, a: [u64; 6]) -> SysResult {
        let request = self.request(a[0])?;
        let size = a[2].min(BUFFER_MAX);
        let mut out = vec![0u8; size as usize];
        let r = raw(
            SYS_STATMOUNT,
            &[request.as_ptr() as u64, out.as_mut_ptr() as u64, size, a[3]],
        )?;
        copy_out(self.mem, a[1], &out)?;
        Ok(r)
    }

    /// listmount(2): request, ids (u64s, out), count, flags.
    fn listmount(&self, a: [u64; 6]) -> SysResult {
        let request = self.request(a[0])?;
        let count = a[2].min(1 << 20);
        let mut ids = vec![0u8; count as usize * 8];
        let n = raw(
            SYS_LISTMOUNT,
            &[
                request.as_ptr() as u64,
                ids.as_mut_ptr() as u64,
                count,
                a[3],
            ],
        )?;
        copy_out(self.mem, a[1], &ids[..n as usize * 8])?;
        Ok(n)
    }

    /// A `struct mnt_id_req`: its first u32 is its size (24 or 32).
    fn request(&self, at: u64) -> Result<Vec<u8>, i64> {
        let size = u64::from(u32::from_le_bytes(copy_in::<4>(self.mem, at)?));
        if !(24..=4096).contains(&size) {
            return Err(err(libc::EINVAL));
        }
        self.bytes(at, size)
    }

    /// lsm_get_self_attr(2): attribute, contexts (out), size (in and out,
    /// u32), flags.
    fn lsm_get_self_attr(&self, a: [u64; 6]) -> SysResult {
        let size = u32::from_le_bytes(copy_in::<4>(self.mem, a[2])?);
        let mut size_now = size;
        let mut out = vec![0u8; (size as u64).min(BUFFER_MAX) as usize];
        let ctx = if a[1] != 0 {
            out.as_mut_ptr() as u64
        } else {
            0
        };
        let r = raw(
            SYS_LSM_GET_SELF_ATTR,
            &[a[0], ctx, &raw mut size_now as u64, a[3]],
        );
        copy_out(self.mem, a[2], &size_now.to_le_bytes())?;
        if r.is_ok() && a[1] != 0 {
            copy_out(self.mem, a[1], &out[..(size_now.min(size)) as usize])?;
        }
        r
    }

    /// lsm_list_modules(2): ids (u64s, out), size (in and out), flags.
    fn lsm_list_modules(&self, a: [u64; 6]) -> SysResult {
        let size = u32::from_le_bytes(copy_in::<4>(self.mem, a[1])?);
        let mut size_now = size;
        let mut ids = vec![0u8; (size as u64).min(4096) as usize];
        let r = raw(
            SYS_LSM_LIST_MODULES,
            &[ids.as_mut_ptr() as u64, &raw mut size_now as u64, a[2]],
        );
        copy_out(self.mem, a[1], &size_now.to_le_bytes())?;
        if let Ok(n) = r {
            copy_out(self.mem, a[0], &ids[..(n as usize * 8).min(ids.len())])?;
        }
        r
    }
}
