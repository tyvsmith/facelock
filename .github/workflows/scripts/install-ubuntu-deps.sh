#!/usr/bin/env bash
set -euo pipefail

# The justfile needs just >= 1.36 (hyphenated variable names); Ubuntu noble
# packages 1.21.0, which fails to parse it before building anything. Install
# the same release the other builders and the maintainer's machine run,
# pinned by digest from the project's published SHA256SUMS.
JUST_VERSION="1.46.0"
JUST_SHA256="79966e6e353f535ee7d1c6221641bcc8e3381c55b0d0a6dc6e54b34f9db36eaa"

echo "=== Installing Ubuntu build dependencies ==="

sudo apt-get update
sudo apt-get install -y \
  libv4l-dev \
  libpam0g-dev \
  pkg-config \
  libssl-dev \
  clang \
  libxkbcommon-dev \
  libwayland-dev \
  libtss2-dev \
  libtss2-tcti-tabrmd-dev

echo "=== Installing just ${JUST_VERSION} ==="
tarball="just-${JUST_VERSION}-x86_64-unknown-linux-musl.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL --retry 3 -o "${tmp}/${tarball}" \
  "https://github.com/casey/just/releases/download/${JUST_VERSION}/${tarball}"
echo "${JUST_SHA256}  ${tmp}/${tarball}" | sha256sum -c -
tar -xzf "${tmp}/${tarball}" -C "${tmp}" just
sudo install -m 0755 "${tmp}/just" /usr/local/bin/just
just --version

echo "=== Ubuntu dependencies installed ==="
