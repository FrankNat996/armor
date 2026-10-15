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
//!
//! `cargo xtask fetch-rootfs --games` makes `target/guest/rootfs-games`:
//! the x86-64 root plus the libraries of games and graphical programs
//! (`PACKAGES_GAMES`: Mesa, X11, SDL2, ALSA, PulseAudio, Vulkan,
//! `libstdc++`), for running games by hand and for `cargo xtask gl-check`.

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

/// The games root (`--games`, M8): the packages of `PACKAGES` plus these,
/// the closure (Pre-Depends and Depends, first alternative) of `libgl1`,
/// `libgl1-mesa-dri`, `libglx-mesa0`, `libegl1`, `libegl-mesa0`,
/// `libgles2`, `libopengl0`, `libglu1-mesa`, `libx11-6`, `libxext6`,
/// `libxrandr2`, `libxi6`, `libxcursor1`, `libxinerama1`, `libxxf86vm1`,
/// `libxrender1`, `libxfixes3`, `libxss1`, `libxmu6`, `libxkbcommon0`,
/// `libasound2t64`, `libpulse0`, `libudev1`, `libdbus-1-3`, `libvulkan1`,
/// `mesa-vulkan-drivers`, `libsdl2-2.0-0` and `libstdc++6` (index of
/// Debian trixie amd64, 2026-10-14): what x86-64 Linux games ask for
/// (X11, OpenGL, SDL2, sound) and C++ programs.
const PACKAGES_GAMES: &[Package] = &[
    Package {
        pool_path: "pool/main/a/alsa-lib/libasound2-data_1.2.14-1+deb13u1_all.deb",
        sha256: "04688afdff3769c0f685541daed7b2f6f0cb946799ddf1d5847ddf11fe245559",
    },
    Package {
        pool_path: "pool/main/a/alsa-lib/libasound2t64_1.2.14-1+deb13u1_amd64.deb",
        sha256: "5495496142d57e5ad6d581ac5456ec3c87f872c109a2bfb815d154b673eebc67",
    },
    Package {
        pool_path: "pool/main/liba/libasyncns/libasyncns0_0.8-6+b5_amd64.deb",
        sha256: "e7958777b0e3586349c57ac759a95b68f80d28e5bd1c4225705198b39b436115",
    },
    Package {
        pool_path: "pool/main/libb/libbsd/libbsd0_0.12.2-2_amd64.deb",
        sha256: "e5a85986fa6bec3307ab1bc860736b478b331882bc45e17675a7bdf88eecb43a",
    },
    Package {
        pool_path: "pool/main/d/dbus/libdbus-1-3_1.16.2-2_amd64.deb",
        sha256: "bbcb711daff7e104b5f80f9b05475b6142674bfe65518a36a56e96a91068a3f5",
    },
    Package {
        pool_path: "pool/main/libd/libdecor-0/libdecor-0-0_0.2.2-2_amd64.deb",
        sha256: "e4f228a9d4a38af23fab00f8e20219790b567e2e1f7804b3a215a26294350f82",
    },
    Package {
        pool_path: "pool/main/libd/libdrm/libdrm-amdgpu1_2.4.124-2_amd64.deb",
        sha256: "c1d97a5e32e2bc68e833b8abeae026b6306198e87ee4396a7ffecd4779caefbf",
    },
    Package {
        pool_path: "pool/main/libd/libdrm/libdrm-common_2.4.124-2_all.deb",
        sha256: "9a8a6c65c165e9964f106fb4ac710959b5d33e0790227e3ab6b27c4742d1254a",
    },
    Package {
        pool_path: "pool/main/libd/libdrm/libdrm-intel1_2.4.124-2_amd64.deb",
        sha256: "188ab1fd74c838b3c8055d62107351e16a4bb7a25d39c30bbb9aac30ddd37238",
    },
    Package {
        pool_path: "pool/main/libd/libdrm/libdrm2_2.4.124-2_amd64.deb",
        sha256: "fe2276901c7cd7b8079de63072d37fe1cbeb4eb001a3bc1f1d662ad89aa0890e",
    },
    Package {
        pool_path: "pool/main/libe/libedit/libedit2_3.1-20250104-1_amd64.deb",
        sha256: "b002ea172b9c1e34a67bc497c523c67bb74c3f0a4e98113cb083990a1f1d3bfe",
    },
    Package {
        pool_path: "pool/main/m/mesa/libegl-mesa0_25.0.7-2+deb13u1_amd64.deb",
        sha256: "0ddebd7e91b048aac812611d6bd008343386cb140b51e5e66df74c6b6263be65",
    },
    Package {
        pool_path: "pool/main/libg/libglvnd/libegl1_1.7.0-1+b2_amd64.deb",
        sha256: "62e7bc8ccd47a60e55aad17f83d8d474310d782eb8589bbce2ab714c31712406",
    },
    Package {
        pool_path: "pool/main/e/elfutils/libelf1t64_0.192-4_amd64.deb",
        sha256: "94497b7e17b6f574a0605b380d454e20d3f01a9c63b70c2a2263f679d30053e1",
    },
    Package {
        pool_path: "pool/main/libf/libffi/libffi8_3.4.8-2_amd64.deb",
        sha256: "0ebdc340de33333639c3c63874cd4b15ac2e83dfa1ef3053b7eefaf4919f4f68",
    },
    Package {
        pool_path: "pool/main/f/flac/libflac14_1.5.0+ds-2_amd64.deb",
        sha256: "658b9f77d35638caf75dfaea4f6d5cde196104d5662f0776e23d7cc450bd4cb5",
    },
    Package {
        pool_path: "pool/main/m/mesa/libgbm1_25.0.7-2+deb13u1_amd64.deb",
        sha256: "31fb6d76b9ceaf13848fa617df53f85f62626b4fe7464a93811c720af6d5f2dd",
    },
    Package {
        pool_path: "pool/main/m/mesa/libgl1-mesa-dri_25.0.7-2+deb13u1_amd64.deb",
        sha256: "722fad9944bdfd29eb6421576331a2697f83bc4b3551d693edb7aefdde6bb198",
    },
    Package {
        pool_path: "pool/main/libg/libglvnd/libgl1_1.7.0-1+b2_amd64.deb",
        sha256: "87fa2f6e5abaed4ed385fac879c8dd735af719ee2300222d901793c66e041678",
    },
    Package {
        pool_path: "pool/main/libg/libglvnd/libgles2_1.7.0-1+b2_amd64.deb",
        sha256: "e12c18aa77939d122ee1dea520ba0440b97dc19200b7396e03dff03a3ece74c1",
    },
    Package {
        pool_path: "pool/main/libg/libglvnd/libglvnd0_1.7.0-1+b2_amd64.deb",
        sha256: "887f74008166549ce9e100c906aa937e95d6e5ce1c8d86efe8c95fd953359b9c",
    },
    Package {
        pool_path: "pool/main/m/mesa/libglx-mesa0_25.0.7-2+deb13u1_amd64.deb",
        sha256: "2e6ab57de9931e890621d3ade04213b06e7142a4ae8a72a1756c44d0271aaeb7",
    },
    Package {
        pool_path: "pool/main/libg/libglvnd/libglx0_1.7.0-1+b2_amd64.deb",
        sha256: "2721fdca0fe3bd963cb39482eabc253af52b88f4a7f6dbb69e475549daf5af3b",
    },
    Package {
        pool_path: "pool/main/l/llvm-toolchain-19/libllvm19_19.1.7-3+b1_amd64.deb",
        sha256: "db0d614d61345ca710fb73e75105f1ec6e38898fa7b3aa99fa7eb7d8b0a11d57",
    },
    Package {
        pool_path: "pool/main/libm/libmd/libmd0_1.1.0-2+b1_amd64.deb",
        sha256: "7244ec3839b61fac0c1884fe08aaa040f26e8f1f35f1f5d3482eacefd30d1b44",
    },
    Package {
        pool_path: "pool/main/l/lame/libmp3lame0_3.100-6+b3_amd64.deb",
        sha256: "3867319c9fbbc8cc25f2d1dc1d75d13548c84c37fa709c1ac9ca02ac051b63ab",
    },
    Package {
        pool_path: "pool/main/m/mpg123/libmpg123-0t64_1.32.10-1+deb13u1_amd64.deb",
        sha256: "31fa6ef638f1fdbe198faee0b6fe50d9dd468077f8247cc3dbf6b2250abb6101",
    },
    Package {
        pool_path: "pool/main/libo/libogg/libogg0_1.3.5-3+b2_amd64.deb",
        sha256: "911aec914f5bda0788164ff8068b682537378bf8d407b1d6c1b377522f96b83b",
    },
    Package {
        pool_path: "pool/main/o/opus/libopus0_1.5.2-2_amd64.deb",
        sha256: "794056db33d71b2ac4bd8b5a4eb23b627bcb8a49d123c33b6e841a996253a067",
    },
    Package {
        pool_path: "pool/main/libp/libpciaccess/libpciaccess0_0.17-3+b3_amd64.deb",
        sha256: "d9a0091071635a84e837051e4813005ac445071731becea28fba4d9806df5252",
    },
    Package {
        pool_path: "pool/main/p/pulseaudio/libpulse0_17.0+dfsg1-2+b1_amd64.deb",
        sha256: "786aca79119cb249562708fd24dcdf817604cfe75bbfcb8cda3464042e17f40e",
    },
    Package {
        pool_path: "pool/main/libs/libsamplerate/libsamplerate0_0.2.2-4+b2_amd64.deb",
        sha256: "2665417fa996c1e249dbaa65c32305ac666e5ae865fad0f109b629935ec0bd18",
    },
    Package {
        pool_path: "pool/main/libs/libsdl2/libsdl2-2.0-0_2.32.4+dfsg-1_amd64.deb",
        sha256: "c61af1033d79fa6a393c49fac987165876de8ff27ebbd6a36cf19f5fc84d7a7a",
    },
    Package {
        pool_path: "pool/main/l/lm-sensors/libsensors-config_3.6.2-2_all.deb",
        sha256: "3056da80c7d963af795dab480ab6f6f4b154ad4ac39f522dc52d17c834fea253",
    },
    Package {
        pool_path: "pool/main/l/lm-sensors/libsensors5_3.6.2-2_amd64.deb",
        sha256: "f0a994a6d7cfa695dea5343d0d1ba7eed796c0ad920c7282998b95f60049c4f6",
    },
    Package {
        pool_path: "pool/main/libs/libsndfile/libsndfile1_1.2.2-2+deb13u1_amd64.deb",
        sha256: "d92263a045554e6f39c6b1c2ee4cca44a3248d9b9339301ff195969bf84b75eb",
    },
    Package {
        pool_path: "pool/main/g/gcc-14/libstdc++6_14.2.0-19_amd64.deb",
        sha256: "ab1fa05837aa7a92aae748fd07a18a35f7d18bb4a71c4724fe2bbf0e32089de0",
    },
    Package {
        pool_path: "pool/main/s/systemd/libudev1_257.13-1~deb13u1_amd64.deb",
        sha256: "5d41c284f5a93b05bc7d648b61a02dd2bb9ff05b2261ad1a8b7d96044a0cfa88",
    },
    Package {
        pool_path: "pool/main/libv/libvorbis/libvorbis0a_1.3.7-3_amd64.deb",
        sha256: "0465f7f602f7b00965e58e7273b5876060ab7946f0452a9f95d979046932cc42",
    },
    Package {
        pool_path: "pool/main/libv/libvorbis/libvorbisenc2_1.3.7-3_amd64.deb",
        sha256: "5fbbf06a5b196235aa2753cd12bbe7f843328571d368f15b1c337578e29f73a2",
    },
    Package {
        pool_path: "pool/main/v/vulkan-loader/libvulkan1_1.4.309.0-1_amd64.deb",
        sha256: "f47da79cd140264fe21cceb08bc87a71bc7fec05819e4a421f3f518d21101a37",
    },
    Package {
        pool_path: "pool/main/w/wayland/libwayland-client0_1.23.1-3_amd64.deb",
        sha256: "d1607e1db1a7c5378a2e5c6ae7b20f64dc6acf134f953d84a7f5204b74339f33",
    },
    Package {
        pool_path: "pool/main/w/wayland/libwayland-cursor0_1.23.1-3_amd64.deb",
        sha256: "26fccae2b1228744de0cad6c688550b8d13cd4e8d4ccf0cfdae015634ece673f",
    },
    Package {
        pool_path: "pool/main/w/wayland/libwayland-egl1_1.23.1-3_amd64.deb",
        sha256: "7c5c3429028b39b9432792db08cb218c43891df439ebc648b83ebee676a80ec5",
    },
    Package {
        pool_path: "pool/main/w/wayland/libwayland-server0_1.23.1-3_amd64.deb",
        sha256: "2967212bd582e0dffca443fdc44f4c660e7368d41f7ee3a7f6314e0c3abfe9ea",
    },
    Package {
        pool_path: "pool/main/libx/libx11/libx11-6_1.8.12-1_amd64.deb",
        sha256: "b5a3fd3bf8c8fd0364bfb9bea00dcba7fc301229bd02dded084632d31f5b0fb3",
    },
    Package {
        pool_path: "pool/main/libx/libx11/libx11-data_1.8.12-1_all.deb",
        sha256: "c54f87069888f80ba4da586da6147d74c7598ccdd8b90906dbc4271fa414c738",
    },
    Package {
        pool_path: "pool/main/libx/libx11/libx11-xcb1_1.8.12-1_amd64.deb",
        sha256: "e05f94d21a932fba5b09b9b13d99df776b155d3bc792c0e294451df9ffe1ba25",
    },
    Package {
        pool_path: "pool/main/libx/libxau/libxau6_1.0.11-1_amd64.deb",
        sha256: "689a9f0e0ba3e2c65431f864871e303ee904de69dd28abfc462663fae030227f",
    },
    Package {
        pool_path: "pool/main/libx/libxcb/libxcb-dri3-0_1.17.0-2+b1_amd64.deb",
        sha256: "f446d42fb5fcebbb3e347368ba83616769fdb271d85b2f49048e337f3163d267",
    },
    Package {
        pool_path: "pool/main/libx/libxcb/libxcb-glx0_1.17.0-2+b1_amd64.deb",
        sha256: "cc59a3fa1c4ce376c3a32798179950810bd7b9be9a35bc76de1f5b5e7cc3925e",
    },
    Package {
        pool_path: "pool/main/libx/libxcb/libxcb-present0_1.17.0-2+b1_amd64.deb",
        sha256: "db95ea4630c55bd7f3281cb60ccf75b1627ef2f4399e1939f8f6161e584a92fb",
    },
    Package {
        pool_path: "pool/main/libx/libxcb/libxcb-randr0_1.17.0-2+b1_amd64.deb",
        sha256: "f5d9fe5fdf797918f81f0abf6cfd4270bd54659540cddb4da709ab7154523214",
    },
    Package {
        pool_path: "pool/main/libx/libxcb/libxcb-shm0_1.17.0-2+b1_amd64.deb",
        sha256: "d1777a813f484e89f3dcaadd1d5f2b1a13e9313b6bf70eded16d8ce743208fef",
    },
    Package {
        pool_path: "pool/main/libx/libxcb/libxcb-sync1_1.17.0-2+b1_amd64.deb",
        sha256: "0ce4770ed1505be9ddc6473045e4662d062aff2f3077ee684265f01cfb543559",
    },
    Package {
        pool_path: "pool/main/libx/libxcb/libxcb-xfixes0_1.17.0-2+b1_amd64.deb",
        sha256: "f9c1aafc18bf9e4662e34bbaf3bb00f1f4e8c2fc1dd786fdfb6cb9cb6c64fae7",
    },
    Package {
        pool_path: "pool/main/libx/libxcb/libxcb1_1.17.0-2+b1_amd64.deb",
        sha256: "5c222a72d11b866447da31693254f738430726e3e065a384e82687b2fd2f978b",
    },
    Package {
        pool_path: "pool/main/libx/libxcursor/libxcursor1_1.2.3-1_amd64.deb",
        sha256: "bb0dbf58b804faaa74ed9efdae61c4b14bb0b4855680798f63e5ca6a4954976b",
    },
    Package {
        pool_path: "pool/main/libx/libxdmcp/libxdmcp6_1.1.5-1_amd64.deb",
        sha256: "0740dc760916b2008b45417a42a8fd7dd5de370fb57d31373f15034cda8acf0b",
    },
    Package {
        pool_path: "pool/main/libx/libxext/libxext6_1.3.4-1+b3_amd64.deb",
        sha256: "fc618ec40465e5ce48622606299cb47833efc3fb235ba15543b81f850722f443",
    },
    Package {
        pool_path: "pool/main/libx/libxfixes/libxfixes3_6.0.0-2+b4_amd64.deb",
        sha256: "3fdd95d86d8e9d63e11f52070935c8f9f912c36aa3b17338d969f7685f3ed4fb",
    },
    Package {
        pool_path: "pool/main/libx/libxi/libxi6_1.8.2-1_amd64.deb",
        sha256: "093d0903f35bb7a9f6815180ee040e6951fecf9b66c128cd72f064710210606e",
    },
    Package {
        pool_path: "pool/main/libx/libxinerama/libxinerama1_1.1.4-3+b4_amd64.deb",
        sha256: "49d4e1628960407b8be2880a42428e7629801715a8a5697d42c0b0ca0a479911",
    },
    Package {
        pool_path: "pool/main/libx/libxkbcommon/libxkbcommon0_1.7.0-2_amd64.deb",
        sha256: "f75ee544f55acc6a271debfab3ea4ae0458afc89d81cfe1a71137e07d4895b86",
    },
    Package {
        pool_path: "pool/main/libx/libxml2/libxml2_2.12.7+dfsg+really2.9.14-2.1+deb13u3_amd64.deb",
        sha256: "e0c6b63ce4602a036a526f60fe5e6c1586710688058d98fc1001b9b3147b7efd",
    },
    Package {
        pool_path: "pool/main/libx/libxrandr/libxrandr2_1.5.4-1+b3_amd64.deb",
        sha256: "11e3490de93a8bbee3daba719cb8e1325a26fb3c125525c34bdcb7deb05eb9b2",
    },
    Package {
        pool_path: "pool/main/libx/libxrender/libxrender1_0.9.12-1_amd64.deb",
        sha256: "9d042dfd5e613be1e02e6ddd0c5c4adef19c5eb08f6db838c2eba672c496dca4",
    },
    Package {
        pool_path: "pool/main/libx/libxshmfence/libxshmfence1_1.3.3-1_amd64.deb",
        sha256: "7b339e9e5b2349723d35af4df89bcc7aa456bbdf8ba1754358f9b44c3fe1f964",
    },
    Package {
        pool_path: "pool/main/libx/libxss/libxss1_1.2.3-1+b3_amd64.deb",
        sha256: "302d4a97800429fa0d67ae2e0e8e3adc1228b12d25d3d3cca6c9d24eeff40a6c",
    },
    Package {
        pool_path: "pool/main/libx/libxxf86vm/libxxf86vm1_1.1.4-1+b4_amd64.deb",
        sha256: "f9f8487e536293e7ecf91fe7feb14e97e02a3aa652e207c661442dce775d3b14",
    },
    Package {
        pool_path: "pool/main/z/z3/libz3-4_4.13.3-1_amd64.deb",
        sha256: "71383373523ef62d47eccf660cf6535c3febcbb3f88e54cb7a43124014b57359",
    },
    Package {
        pool_path: "pool/main/l/lsb/lsb-base_11.6_all.deb",
        sha256: "f8bedd167280e76636df3a1bc023cd2906d458916c1af4c1d7912c5b971fc642",
    },
    Package {
        pool_path: "pool/main/m/mesa/mesa-libgallium_25.0.7-2+deb13u1_amd64.deb",
        sha256: "3e610f29321cdcc61337c86e6b4031ff60f0c9915fe19e6a862ac06480620ff4",
    },
    Package {
        pool_path: "pool/main/m/mesa/mesa-vulkan-drivers_25.0.7-2+deb13u1_amd64.deb",
        sha256: "caf2f7d296b7522efe74e40a74bce762a3ce84c33b53b5e54e1248ac2e0a13a5",
    },
    Package {
        pool_path: "pool/main/s/sysvinit/sysvinit-utils_3.14-4_amd64.deb",
        sha256: "c7e957360dff89675b491e501b00047cf750695178cabab167c92fc260d358ec",
    },
    Package {
        pool_path: "pool/main/x/xorg/x11-common_7.7+24+deb13u1_all.deb",
        sha256: "57a981938506b26dc552f19cafeaa8c04e9b59dc7508509151f8507a9d4e5f24",
    },
    Package {
        pool_path: "pool/main/x/xkeyboard-config/xkb-data_2.42-1_all.deb",
        sha256: "196ff18533382f64e057ea49df2bb486bd4275a4cc0917361edb560b8756dada",
    },
    Package {
        pool_path: "pool/main/libx/libxmu/libxmu6_1.1.3-3+b4_amd64.deb",
        sha256: "cfdf1dee8f9abd87f503222fc8b04a30ce820fb1255247b7c3a28ff9c244431a",
    },
    Package {
        pool_path: "pool/main/libg/libglu/libglu1-mesa_9.0.2-1.1+b3_amd64.deb",
        sha256: "a834280c698c17ddaf5ad69ed699dd47e337f034a97a6a08285025d85743306b",
    },
    Package {
        pool_path: "pool/main/libx/libxt/libxt6t64_1.2.1-1.2+b2_amd64.deb",
        sha256: "a1f4557c03113681ee24863495adc244cecec95fe3ab7256ce76e87320157300",
    },
    Package {
        pool_path: "pool/main/libs/libsm/libsm6_1.2.6-1_amd64.deb",
        sha256: "03038b3002e6f44322554e335d60d8ff87acfcf2e21c904b6866610a9913ee00",
    },
    Package {
        pool_path: "pool/main/libi/libice/libice6_1.1.1-1_amd64.deb",
        sha256: "38185997162793b5dfb768644badc57852879784a2f9d83c6c4e47efa5e36698",
    },
    Package {
        pool_path: "pool/main/libg/libglvnd/libopengl0_1.7.0-1+b2_amd64.deb",
        sha256: "50ea65a1141cbf6b02e41492963656ad0ba71d257cc4066ee9756bca7a6c1ac3",
    },
    Package {
        pool_path: "pool/main/u/util-linux/libuuid1_2.41.5-0+deb13u1_amd64.deb",
        sha256: "c1bf4c4c3ff48c57fabf93307dfb56996b60cfa33927afc4158b5db36fb2721e",
    },
];

/// A guest root of `fetch-rootfs`: x86-64, or i386 (`--i386`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Amd64,
    I386,
    /// x86-64, with the libraries of games (`PACKAGES_GAMES`).
    Games,
}

impl Arch {
    fn packages(self) -> Vec<&'static Package> {
        match self {
            Arch::Amd64 => PACKAGES.iter().collect(),
            Arch::I386 => PACKAGES_I386.iter().collect(),
            Arch::Games => PACKAGES.iter().chain(PACKAGES_GAMES).collect(),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Arch::Amd64 => "rootfs",
            Arch::I386 => "rootfs-i386",
            Arch::Games => "rootfs-games",
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
