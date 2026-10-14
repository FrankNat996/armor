//! The x86_64 guest root for dynamically linked Debian programs (M5,
//! ADR-0011, ADR-0013).
//!
//! `cargo xtask fetch-rootfs` downloads Debian's amd64 `coreutils`, `grep`,
//! `sed`, `gawk` and `findutils` and every package they depend on (computed from the Packages index of Debian trixie
//! on 2026-10-12), verifies their SHA-256, and unpacks them into
//! `target/guest/rootfs`, with the merged-/usr links of a Debian system
//! (`/lib` -> `usr/lib`, ...). It is a root, not an installed system:
//! maintainer scripts do not run, so there is no `/etc/ld.so.cache` and the
//! dynamic loader uses its default search paths.
//!
//! The cases `difftest` runs are listed in `tests/guest/coreutils.txt`,
//! `tools.txt` and `interpreters.txt`.
//!
//! `cargo xtask fetch-rootfs --i386` makes the same kind of root for i386
//! programs (ADR-0029) in `target/guest/rootfs-i386`: Debian's i386
//! `coreutils` and its closure (`PACKAGES_I386`). The coreutils cases run
//! there too, named `coreutils32/…`.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use crate::Paths;
use crate::busybox::{Case, parse_cases};
use crate::deb::{self, Package, fresh_dir};

/// coreutils 9.7-3, grep 3.11-4, sed 4.9-2+deb13u1, gawk 1:5.2.1-2+b1,
/// findutils 4.10.0-3, perl-base 5.40.1-6+deb13u1, python3.13-minimal
/// 3.13.5-2+deb13u5, and (since 2026-10-13) gzip, tar, xz-utils, bzip2,
/// diffutils, make and dash, with their dependency closure (Pre-Depends
/// and Depends, first alternative), Debian trixie amd64.
const PACKAGES: &[Package] = &[
    Package {
        pool_path: "pool/main/c/coreutils/coreutils_9.7-3_amd64.deb",
        sha256: "1299ab6f9389a288eb2f5f3dd222c26cc777b9a2d5ecb6ee4cbd340cebcdada2",
    },
    Package {
        pool_path: "pool/main/a/acl/libacl1_2.3.2-2+b1_amd64.deb",
        sha256: "08074f01e384bc07c0c2d79a58cf4a6523f71cf75d1808101c79617656c9a39d",
    },
    Package {
        pool_path: "pool/main/a/attr/libattr1_2.5.2-3_amd64.deb",
        sha256: "606b5ee12ea2786be607a17f40c1fb5e65c76ceaff66665bdf8f8c6c1b71d1fb",
    },
    Package {
        pool_path: "pool/main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb",
        sha256: "967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9",
    },
    Package {
        pool_path: "pool/main/libc/libcap2/libcap2_2.75-10+deb13u1+b3_amd64.deb",
        sha256: "89fc4d34fc7a28ad6f0fcd0c561ab253b9dedf6f77f5a000b47c276c8295bf67",
    },
    Package {
        pool_path: "pool/main/g/gmp/libgmp10_6.3.0+dfsg-3_amd64.deb",
        sha256: "d0d0265eb01770f17afd0f7c8c0622f80479dcfbbe13653a0debeec61464e622",
    },
    Package {
        pool_path: "pool/main/libs/libselinux/libselinux1_3.8.1-1_amd64.deb",
        sha256: "68bb8d32bd8d6d7d2f5952a169db03d1484b46ae1e52abccdec42a19dccea5d5",
    },
    Package {
        pool_path: "pool/main/o/openssl/libssl3t64_3.5.7-1~deb13u2_amd64.deb",
        sha256: "916f7f40b34a06e6ebfaefcdab331bff458328411da672598f126a760472467d",
    },
    Package {
        pool_path: "pool/main/s/systemd/libsystemd0_257.13-1~deb13u1_amd64.deb",
        sha256: "ab0d4127b5e46e6f8c015a1db15a62ba9ae274cdefa150083adf90ada0600ea1",
    },
    Package {
        pool_path: "pool/main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb",
        sha256: "3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150",
    },
    Package {
        pool_path: "pool/main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_amd64.deb",
        sha256: "1252b96a5bc44bb5db982bef8eb18e54f5047cede2aff641bce4f8e1edb91c3e",
    },
    Package {
        pool_path: "pool/main/libz/libzstd/libzstd1_1.5.7+dfsg-1_amd64.deb",
        sha256: "2f6a2aeacfc925eba8b00ac9139bc4bfccf8cacb09eb93de067074b26948eef9",
    },
    Package {
        pool_path: "pool/main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_amd64.deb",
        sha256: "015be740d6236ad114582dea500c1d907f29e16d6db00566ca32fb68d71ac90d",
    },
    Package {
        pool_path: "pool/main/o/openssl/openssl-provider-legacy_3.5.7-1~deb13u2_amd64.deb",
        sha256: "f155c8191ae6d41da73d792f4182680aeafeb85c3dd223934ee9fdd115c4f1fa",
    },
    Package {
        pool_path: "pool/main/g/gcc-14/gcc-14-base_14.2.0-19_amd64.deb",
        sha256: "5b6825de4263824b78c4c51f6476414f3b4e89c2ab63e81dc8b9b5501e867cf6",
    },
    // grep, sed, gawk, findutils and the packages they add to the closure
    // (same index, same date).
    Package {
        pool_path: "pool/main/g/grep/grep_3.11-4_amd64.deb",
        sha256: "2d78ed07d86a530ebf538f05cb2415f37c8daf908596bc32fc2563cc7e88c55a",
    },
    Package {
        pool_path: "pool/main/s/sed/sed_4.9-2+deb13u1_amd64.deb",
        sha256: "7071270ed4f6adda55bc4f926347fb847bacaffbdee3dff917bc6006ed7e3775",
    },
    Package {
        pool_path: "pool/main/g/gawk/gawk_5.2.1-2+b1_amd64.deb",
        sha256: "0e3b376e74cadfa5049bc8248c563efe89fd45e31466f8891c094c65c0879f5c",
    },
    Package {
        pool_path: "pool/main/f/findutils/findutils_4.10.0-3_amd64.deb",
        sha256: "3edf02b016456b6a98cefdbab99c5ce6f829b786f63affe970badea221aecfc7",
    },
    Package {
        pool_path: "pool/main/m/mpfr4/libmpfr6_4.2.2-1_amd64.deb",
        sha256: "75dddce11dabc7fc543712c33dc27b7f2ee66a111763eb5eac654d010b42cd92",
    },
    Package {
        pool_path: "pool/main/r/readline/libreadline8t64_8.2-6_amd64.deb",
        sha256: "eeadf2b5e755c9f183883feea2d9b5b28560284275d5f54d3e55d0923b1d0967",
    },
    Package {
        pool_path: "pool/main/libs/libsigsegv/libsigsegv2_2.14-1+b2_amd64.deb",
        sha256: "bac603b965f303ea88dc39cd723b3d88cd86e3be9c948783f0d5e0d00e971302",
    },
    Package {
        pool_path: "pool/main/r/readline/readline-common_8.2-6_all.deb",
        sha256: "f77e8aa0bf0de618f11cb0b21658a4ed60f177d63b3cf26d7fc3706b6d0eaaa9",
    },
    Package {
        pool_path: "pool/main/n/ncurses/libtinfo6_6.5+20250216-2_amd64.deb",
        sha256: "8b9f6a7983e9418564e48a627518de4c03917b56efe68d7f3e93bd8fffa1cc10",
    },
    // Interpreters: perl-base and python3.13-minimal with their closure.
    Package {
        pool_path: "pool/main/p/perl/perl-base_5.40.1-6+deb13u1_amd64.deb",
        sha256: "b795464137a0f4d443fc9284f4b93e883fb83883cb533adf300ac660807a352a",
    },
    Package {
        pool_path: "pool/main/libx/libxcrypt/libcrypt1_4.4.38-1_amd64.deb",
        sha256: "0ebc144d662e3197982d1bf3a7b8b35ca845e54c68811de0328b1f0d7c67585c",
    },
    Package {
        pool_path: "pool/main/p/python3.13/python3.13-minimal_3.13.5-2+deb13u5_amd64.deb",
        sha256: "c1fda9e7b1bf15b2fa75a238d8a8710e142fe14c722fc650c5fc614f078c9073",
    },
    Package {
        pool_path: "pool/main/p/python3.13/libpython3.13-minimal_3.13.5-2+deb13u5_amd64.deb",
        sha256: "a4aa6a9a8c77f87bfe545e1d14ec42c001d189ace90e3aa75deec882b39dee49",
    },
    Package {
        pool_path: "pool/main/e/expat/libexpat1_2.8.3-1~deb13u1_amd64.deb",
        sha256: "38abe0e710a07688e9c149d74536e67cfee0364bdb64dd6d644c32a1cfad389f",
    },
    // Added on 2026-10-13 (index of that day): gzip 1.13-1+deb13u1, tar
    // 1.35+dfsg-3.1, xz-utils 5.8.1-1+deb13u1, bzip2 1.0.8-6, diffutils
    // 1:3.10-4, make 4.4.1-2, dash 0.5.12-12 and the new dependencies.
    Package {
        pool_path: "pool/main/g/gzip/gzip_1.13-1+deb13u1_amd64.deb",
        sha256: "74cf12212beee4ab8d473bdc0107abf9e8cef737492198e2f4944a19f142b0ac",
    },
    Package {
        pool_path: "pool/main/t/tar/tar_1.35+dfsg-3.1_amd64.deb",
        sha256: "214c02e1aa291076a3147b8a4c4cae02fadf4c67df629186e3e313726faef1de",
    },
    Package {
        pool_path: "pool/main/x/xz-utils/liblzma5_5.8.1-1+deb13u1_amd64.deb",
        sha256: "1cfcc6e0dc36f438a79b6e2189facdb9d150b08f57d190a60e01c98075c7f896",
    },
    Package {
        pool_path: "pool/main/x/xz-utils/xz-utils_5.8.1-1+deb13u1_amd64.deb",
        sha256: "9e0a39d95373afdaad3c9dfaab3c8b0beb9a2c4654e6be10b4bcf458509a3217",
    },
    Package {
        pool_path: "pool/main/b/bzip2/libbz2-1.0_1.0.8-6_amd64.deb",
        sha256: "cba4cda04244b5e481bb15524bc3c983a7d1b6f330013b9b381706a2fcb65310",
    },
    Package {
        pool_path: "pool/main/b/bzip2/bzip2_1.0.8-6_amd64.deb",
        sha256: "6b5fbe71f0fc41e85919bc4b3c514e6c47577c5005dbf1f5af6e4a49cc221aab",
    },
    Package {
        pool_path: "pool/main/d/diffutils/diffutils_3.10-4_amd64.deb",
        sha256: "d3412669f7ae53ba895c3a509620ea7494ec1a36faa204c5215442ced381a479",
    },
    Package {
        pool_path: "pool/main/m/make-dfsg/make_4.4.1-2_amd64.deb",
        sha256: "70a9709e665383b06068ccd423d62ea191b65355f318a25565dbf324b254cec6",
    },
    Package {
        pool_path: "pool/main/d/debianutils/debianutils_5.23.2_amd64.deb",
        sha256: "720e43fda316676076498b4224ad3318049deafe60cc7c601011048de03ee97d",
    },
    Package {
        pool_path: "pool/main/d/dash/dash_0.5.12-12_amd64.deb",
        sha256: "a8902cb6d8650134764a25fb80aec8589d858ef71ece1680a62e84816c37bb04",
    },
];

/// Merged-/usr links of a Debian system, created by the base system rather
/// than by these packages.
/// coreutils 9.7-3 and its dependency closure (Pre-Depends and Depends,
/// first alternative), Debian trixie i386, from the Packages index of
/// 2026-10-14 (SHA-256 checked against the Release file): the same
/// packages and versions as the amd64 ones.
const PACKAGES_I386: &[Package] = &[
    Package {
        pool_path: "pool/main/c/coreutils/coreutils_9.7-3_i386.deb",
        sha256: "2413c33e0aea7167793062d6c07c5114b903c0b47d9df827764ff67b2cec7149",
    },
    Package {
        pool_path: "pool/main/a/acl/libacl1_2.3.2-2+b1_i386.deb",
        sha256: "978d20b5c4e5de55d15d2116be07279ac5e4f06745a7c0d33f1c9fd4ff945dd3",
    },
    Package {
        pool_path: "pool/main/a/attr/libattr1_2.5.2-3_i386.deb",
        sha256: "9e04b4b90e681e93bf05807a17769a00b22a7a1fbb079ebc095eb05d3e4ccb54",
    },
    Package {
        pool_path: "pool/main/g/glibc/libc6_2.41-12+deb13u4_i386.deb",
        sha256: "67bb4b54cf4bb6a380449dfa1852cd9135659f85360caa0e41d312f4b30e573a",
    },
    Package {
        pool_path: "pool/main/libc/libcap2/libcap2_2.75-10+deb13u1+b3_i386.deb",
        sha256: "15cdfa6ca972c7c81e48e0c030a2b3ae16acb146cca45659bf72919f4af94aeb",
    },
    Package {
        pool_path: "pool/main/g/gmp/libgmp10_6.3.0+dfsg-3_i386.deb",
        sha256: "f005fef33ce70d6775070c60017f0d29a0c415f1b657b4cc622d1a556561615c",
    },
    Package {
        pool_path: "pool/main/libs/libselinux/libselinux1_3.8.1-1_i386.deb",
        sha256: "9d5628efff228a00a804638409e3c265ada5cf5ccc6eb25cf37865179c945f74",
    },
    Package {
        pool_path: "pool/main/o/openssl/libssl3t64_3.5.7-1~deb13u2_i386.deb",
        sha256: "30cd19b3f34b9d688d3a373082ccc8907bff1e52efb7f344266eceed100bcd18",
    },
    Package {
        pool_path: "pool/main/s/systemd/libsystemd0_257.13-1~deb13u1_i386.deb",
        sha256: "e034259827e162c4c01ef4412bc6a2c4b4c3e2744767e5c08338132a0e2cf0ad",
    },
    Package {
        pool_path: "pool/main/g/gcc-14/libgcc-s1_14.2.0-19_i386.deb",
        sha256: "a4c71fd856d2a48a7505a087b4186e3cca23f94603c05e3fb7c799b27e72f761",
    },
    Package {
        pool_path: "pool/main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_i386.deb",
        sha256: "d73a12ec13f4bbd963ac737ee14773a2f89d4289b25c125e6642b0e3b159ed2e",
    },
    Package {
        pool_path: "pool/main/libz/libzstd/libzstd1_1.5.7+dfsg-1_i386.deb",
        sha256: "305f0ab5f16552c44906b3f3f6e9ad3eb33f6f857c3347fdf3bd987abd6989a9",
    },
    Package {
        pool_path: "pool/main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_i386.deb",
        sha256: "8950c79b1ade51f53b2f37863aec532cfaca3dff315e28f0c5b53902425c41ed",
    },
    Package {
        pool_path: "pool/main/o/openssl/openssl-provider-legacy_3.5.7-1~deb13u2_i386.deb",
        sha256: "8ec8d6aa3b4a7307601d973d3c11a75026a2a953367f6b12a68e389ec7a49ee2",
    },
    Package {
        pool_path: "pool/main/g/gcc-14/gcc-14-base_14.2.0-19_i386.deb",
        sha256: "cab3cc6782d6cd3445d184ad317bbd8cc46395eb2675ccd75f4b57147469887a",
    },
];

/// A guest root of `fetch-rootfs`: x86-64, or i386 (`--i386`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Amd64,
    I386,
}

impl Arch {
    fn packages(self) -> &'static [Package] {
        match self {
            Arch::Amd64 => PACKAGES,
            Arch::I386 => PACKAGES_I386,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Arch::Amd64 => "rootfs",
            Arch::I386 => "rootfs-i386",
        }
    }
}

const USR_LINKS: &[(&str, &str)] = &[
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib64", "usr/lib64"),
];

pub fn dir(paths: &Paths) -> PathBuf {
    dir_of(paths, Arch::Amd64)
}

pub fn dir_of(paths: &Paths, arch: Arch) -> PathBuf {
    paths.root.join("target/guest").join(arch.name())
}

/// Whether the root has been fetched.
pub fn available(paths: &Paths) -> bool {
    available_of(paths, Arch::Amd64)
}

pub fn available_of(paths: &Paths, arch: Arch) -> bool {
    dir_of(paths, arch).join("usr/bin/ls").is_file()
}

pub fn fetch(arch: Arch) -> Result<(), String> {
    let paths = Paths::new();
    let work = paths
        .root
        .join("target/guest")
        .join(format!("{}-download", arch.name()));
    let scratch = work.join("deb");
    let staging = work.join("root");
    fresh_dir(&work)?;
    fresh_dir(&scratch)?;
    fresh_dir(&staging)?;
    for package in arch.packages() {
        let deb = deb::download(package, &work)?;
        deb::extract(&deb, &scratch, &staging, &[])?;
        println!("unpacked {}", package.pool_path);
    }
    for (link, target) in USR_LINKS {
        symlink(target, staging.join(link)).map_err(|e| format!("cannot create /{link}: {e}"))?;
    }

    let target = dir_of(&paths, arch);
    if target.exists() {
        fs::remove_dir_all(&target)
            .map_err(|e| format!("cannot clean {}: {e}", target.display()))?;
    }
    fs::rename(&staging, &target).map_err(|e| format!("cannot move the root: {e}"))?;
    fs::remove_dir_all(&work).map_err(|e| format!("cannot clean {}: {e}", work.display()))?;
    println!("guest root ready: {}", target.display());
    Ok(())
}

/// Reads `tests/guest/coreutils.txt`, `tools.txt` and `interpreters.txt`
/// (same format as busybox.txt; the first argument is the program in
/// /usr/bin of the root). Case names get the file's name as prefix.
pub fn cases(paths: &Paths) -> Result<Vec<Case>, String> {
    let mut cases = Vec::new();
    for (file, prefix) in [
        ("coreutils.txt", "coreutils/"),
        ("tools.txt", "tools/"),
        ("interpreters.txt", "interpreters/"),
    ] {
        let list = paths.root.join("tests/guest").join(file);
        let text = fs::read_to_string(&list)
            .map_err(|e| format!("cannot read {}: {e}", list.display()))?;
        cases.extend(parse_cases(&text, prefix)?);
    }
    Ok(cases)
}

/// The coreutils cases again, for the i386 root: `coreutils32/…`.
pub fn cases_i386(paths: &Paths) -> Result<Vec<Case>, String> {
    let list = paths.root.join("tests/guest/coreutils.txt");
    let text =
        fs::read_to_string(&list).map_err(|e| format!("cannot read {}: {e}", list.display()))?;
    parse_cases(&text, "coreutils32/")
}
