#!/usr/bin/env bash
# Build the Arch end-to-end package image and stage this working tree as the
# source the recipe will build.
#
# The image carries only the recipe and the test scripts. The candidate tree is
# staged into a directory the caller then mounts at /staged-source, so editing
# a source file does not rebuild the image, and the tarball the recipe consumes
# is assembled inside the container from the file name the recipe declares.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image="${1:?usage: build-arch-package-image.sh <image> <staging-dir>}"
staging="${2:?usage: build-arch-package-image.sh <image> <staging-dir>}"
[ "$#" -eq 2 ] || {
    echo "usage: build-arch-package-image.sh <image> <staging-dir>" >&2
    exit 2
}

if [ ! -d "$staging" ] || [ -n "$(find "$staging" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
    echo "staging directory must exist and be empty: $staging" >&2
    exit 1
fi
staging="$(cd "$staging" && pwd)"
source_root="$staging/source"
mkdir -- "$source_root"

# The candidate's tracked and intentionally untracked files, as
# prepare-deb-test-context.sh selects them: this tests the tree about to be
# pushed, not the last commit, while leaving build outputs and the downloaded
# models out — a release tarball has neither.
git -C "$repo_root" ls-files --cached --others --exclude-standard -z |
    while IFS= read -r -d '' path; do
        [ -e "$repo_root/$path" ] && printf '%s\0' "$path"
    done |
    tar -C "$repo_root" --null -T - -cf - |
    tar -C "$source_root" -xf -

# build() and check() are --frozen, so a missing or stale lock file fails deep
# inside a long container run rather than here.
[ -f "$source_root/dist/PKGBUILD" ] && [ -f "$source_root/Cargo.lock" ] || {
    echo "staged tree is missing the recipe or the lock file: $source_root" >&2
    exit 1
}
# package() calls this by relative path, so the mode has to survive staging.
[ -x "$source_root/scripts/install-locale-catalogs.sh" ] || {
    echo "staged tree lost the executable bit on scripts/install-locale-catalogs.sh" >&2
    exit 1
}

podman build -t "$image" -f "$repo_root/test/Containerfile.arch-e2e" "$repo_root"
