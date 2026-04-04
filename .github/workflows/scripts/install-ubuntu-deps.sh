#!/usr/bin/env bash
set -euo pipefail

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

echo "=== Ubuntu dependencies installed ==="
