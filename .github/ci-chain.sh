#!/bin/bash
# armor's verification chain in a Debian 13 container (the development
# machine's system: the same gcc, glibc and qemu 10), run by
# .github/workflows/arm64.yml with the repository at /src. The tools are
# installed as root; the chain runs as a normal user, since some tests check
# permissions. The container is privileged, to register qemu with
# binfmt_misc and for --sandbox. Every step runs, and the script fails if
# any did.
set -u
export DEBIAN_FRONTEND=noninteractive
apt-get update -q >/dev/null
apt-get install -y -q --no-install-recommends ca-certificates curl git build-essential \
    gcc-x86-64-linux-gnu libc6-dev-amd64-cross binutils-x86-64-linux-gnu \
    gcc-i686-linux-gnu libc6-dev-i386-cross nasm qemu-user xz-utils bzip2 lld llvm >/dev/null
qemu-x86_64 --version | head -1
# qemu runs the x86 programs that a test program starts with execve only
# through binfmt_misc, as on the development machine (qemu-user-binfmt,
# with the F flag: the kernel keeps this qemu open for every namespace).
mountpoint -q /proc/sys/fs/binfmt_misc || mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc
apt-get install -y -q --no-install-recommends qemu-user-binfmt >/dev/null
# Without systemd in the container nothing reads binfmt.d: register them.
for f in /usr/lib/binfmt.d/qemu-x86_64.conf /usr/lib/binfmt.d/qemu-i386.conf; do
    name=$(basename "$f" .conf)
    [ -e "/proc/sys/fs/binfmt_misc/$name" ] || cat "$f" > /proc/sys/fs/binfmt_misc/register
done
grep -E "interpreter|flags" /proc/sys/fs/binfmt_misc/qemu-x86_64 /proc/sys/fs/binfmt_misc/qemu-i386
useradd -m builder
cp -a /src /home/builder/armor
mkdir -p /out && chmod 777 /out
chown -R builder: /home/builder/armor
runuser -u builder -- bash -c '
  set -u
  cd ~/armor
  curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null 2>&1
  export PATH=$HOME/.cargo/bin:$PATH
  # armor-wow64.dll (ADR-0047): the Windows target, linked with lld-link.
  rustup target add aarch64-pc-windows-msvc >/dev/null 2>&1
  failed=0
  for step in "cargo fmt --all --check" \
      "cargo clippy --workspace --all-targets -- -D warnings" \
      "cargo test --workspace" \
      "cargo xtask matrix --check" \
      "cargo xtask fetch-busybox" "cargo xtask fetch-rootfs" "cargo xtask fetch-rootfs --i386" \
      "cargo xtask build-guest" \
      "cargo xtask difftest --report" \
      "cargo xtask difftest --jit" \
      "cargo xtask difftest --lockstep --report" \
      "env ARMOR_HOST_FEATURES=baseline cargo xtask difftest --report" \
      "cargo xtask difftest --native --report" \
      "cargo xtask encoder-check" \
      "cargo xtask sandbox-check" \
      "cargo xtask wow64"; do
    echo "::group::$step"
    bash -c "$step"
    rc=$?
    echo "::endgroup::"
    echo "=== $step: exit $rc"
    [ $rc -ne 0 ] && failed=1
  done
  # The results of this machine (ADR-0045), for the workflow to publish.
  cp docs/results/*.tsv /out/ 2>/dev/null
  exit $failed
'
