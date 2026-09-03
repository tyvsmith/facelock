#!/usr/bin/env bash
set -euo pipefail

build_dir=/tmp/facelock-arch-package
install -d -m 0755 -o testuser -g testuser "$build_dir"
install -m 0644 -o testuser -g testuser /arch-package/PKGBUILD "$build_dir/PKGBUILD"
install -m 0644 -o testuser -g testuser /build/dist/facelock.install \
    "$build_dir/facelock.install"

runuser -u testuser -- bash -c \
    'cd /tmp/facelock-arch-package && makepkg --nodeps --noconfirm'
# Same hazard select_main_package() covers in test/build-arch-source-package.sh
# (#212): makepkg's `debug` option can drop a facelock-debug-<pkgver> package
# beside this one, and taking the first match `find` returns would install
# whichever the directory happened to list first. This image does not carry that
# script, so the rule is spelled out again here.
packages=()
present=()
while IFS= read -r -d '' path; do
    present+=("${path##*/}")
    case "${path##*/}" in
        facelock-debug-*) ;;
        facelock-[0-9]*) packages+=("$path") ;;
    esac
done < <(find "$build_dir" -maxdepth 1 -type f -name '*.pkg.tar.zst' -print0 | sort -z)

if [ "${#packages[@]}" -ne 1 ]; then
    echo "expected exactly one facelock package in $build_dir," \
        "found ${#packages[@]} (built: ${present[*]:-none})" >&2
    exit 1
fi
pacman -U --noconfirm --overwrite '*' "${packages[0]}"
