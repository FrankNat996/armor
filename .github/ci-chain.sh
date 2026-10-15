#!/bin/bash
# armor's verification chain in a Debian 13 container (the development
# machine's system: the same gcc, glibc and qemu 10), run by
# .github/workflows/arm64.yml with the repository at /src. The tools are
# installed as root; the chain runs as a normal user, since some tests check
# permissions. Every step runs, and the script fails if any did.
set -u
export DEBIAN_FRONTEND=noninteractive
apt-get update -q >/dev/null
apt-get install -y -q --no-install-recommends ca-certificates curl git build-essential \
    gcc-x86-64-linux-gnu libc6-dev-amd64-cross binutils-x86-64-linux-gnu \
    gcc-i686-linux-gnu libc6-dev-i386-cross nasm qemu-user xz-utils bzip2 >/dev/null
qemu-x86_64 --version | head -1
useradd -m builder
cp -a /src /home/builder/armor
chown -R builder: /home/builder/armor
runuser -u builder -- bash -c '
  set -u
  cd ~/armor
  curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null 2>&1
  export PATH=$HOME/.cargo/bin:$PATH
  failed=0
  for step in "cargo fmt --all --check" \
      "cargo clippy --workspace --all-targets -- -D warnings" \
      "cargo test --workspace" \
      "cargo xtask fetch-busybox" "cargo xtask fetch-rootfs" "cargo xtask fetch-rootfs --i386" \
      "cargo xtask build-guest" \
      "cargo xtask difftest" \
      "cargo xtask difftest --lockstep" \
      "env ARMOR_HOST_FEATURES=baseline cargo xtask difftest" \
      "cargo xtask encoder-check" \
      "cargo xtask sandbox-check"; do
    echo "::group::$step"
    bash -c "$step"
    rc=$?
    echo "::endgroup::"
    echo "=== $step: exit $rc"
    [ $rc -ne 0 ] && failed=1
  done
  exit $failed
'
