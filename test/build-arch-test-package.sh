#!/usr/bin/env bash
set -euo pipefail

build_dir=/tmp/facelock-arch-package
install -d -m 0755 -o testuser -g testuser "$build_dir"
install -m 0644 -o testuser -g testuser /arch-package/PKGBUILD "$build_dir/PKGBUILD"
install -m 0644 -o testuser -g testuser /build/dist/facelock.install \
    "$build_dir/facelock.install"

runuser -u testuser -- bash -c \
    'cd /tmp/facelock-arch-package && makepkg --nodeps --noconfirm'
package=$(find "$build_dir" -maxdepth 1 -type f -name 'facelock-*.pkg.tar.zst' -print -quit)
test -n "$package"
pacman -U --noconfirm --overwrite '*' "$package"
