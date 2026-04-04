#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?Usage: publish-aur.sh <VERSION> <CHECKSUM>}"
CHECKSUM="${2:?Usage: publish-aur.sh <VERSION> <CHECKSUM>}"

echo "=== Publishing to AUR ==="

if [ -z "${AUR_SSH_KEY:-}" ]; then
  echo "AUR_SSH_KEY secret not configured. Skipping AUR publish."
  echo "See docs/releasing.md for setup instructions."
  exit 0
fi

# Set up SSH for AUR
mkdir -p ~/.ssh
echo "$AUR_SSH_KEY" > ~/.ssh/aur
chmod 600 ~/.ssh/aur
echo "Host aur.archlinux.org" >> ~/.ssh/config
echo "  IdentityFile ~/.ssh/aur" >> ~/.ssh/config
echo "  User aur" >> ~/.ssh/config
ssh-keyscan aur.archlinux.org >> ~/.ssh/known_hosts 2>/dev/null

# Clone AUR repo
git clone ssh://aur@aur.archlinux.org/facelock.git aur-facelock

# Copy and update PKGBUILD
cp dist/PKGBUILD aur-facelock/PKGBUILD
cp dist/facelock.install aur-facelock/facelock.install
sed -i "s/^pkgver=.*/pkgver=${VERSION}/" aur-facelock/PKGBUILD
sed -i "s/sha256sums=('SKIP')/sha256sums=('${CHECKSUM}')/" aur-facelock/PKGBUILD

# Generate .SRCINFO via Arch container
cd aur-facelock
docker run --rm -v "$(pwd):/pkg" archlinux:base-devel bash -c \
  "pacman -Sy --noconfirm pacman-contrib && cd /pkg && makepkg --printsrcinfo > .SRCINFO"

# Commit and push if changed
git config user.name "facelock-bot"
git config user.email "facelock@users.noreply.github.com"
git add PKGBUILD facelock.install .SRCINFO
git diff --cached --quiet || git commit -m "Update to v${VERSION}"
git push

echo "=== AUR publish complete ==="
