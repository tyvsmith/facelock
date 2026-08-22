#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
suite="${1:?usage: build-deb-package-image.sh <trixie|resolute> <image>}"
image="${2:?usage: build-deb-package-image.sh <trixie|resolute> <image>}"
[ "$#" -eq 2 ] || {
    echo "usage: build-deb-package-image.sh <trixie|resolute> <image>" >&2
    exit 2
}

case "$suite" in
    trixie|resolute) ;;
    *) echo "unsupported Debian package suite: $suite" >&2; exit 2 ;;
esac

base_image="$(python3 - "$repo_root/dist/release-matrix.json" "$suite" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    matrix = json.load(handle)
print(matrix["apt_suites"][sys.argv[2]]["image"])
PY
)"
case "$base_image" in
    *@sha256:????????????????????????????????????????????????????????????????) ;;
    *) echo "suite image is not digest-pinned: $base_image" >&2; exit 1 ;;
esac

context="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-context.XXXXXX")"
artifact_dir="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-artifacts.XXXXXX")"
rebuild_dir="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-rebuild-output.XXXXXX")"
trap 'rm -rf -- "$context" "$artifact_dir" "$rebuild_dir"' EXIT
"$repo_root/test/prepare-deb-test-context.sh" "$context" >/dev/null

# Container engines omit .git from COPY even when an alternate empty ignore
# file is supplied. Transport this isolated one-commit repository explicitly;
# the Containerfile restores it before invoking the exact-tag source builder.
tar -C "$context" -cf "$context/facelock-git-metadata.tar" .git
assembler_image="${image}-assembler"
rebuild_image="${image}-rebuild"
podman build \
    --build-arg "BASE_IMAGE=$base_image" \
    --build-arg "SUITE=$suite" \
    -t "$assembler_image" \
    -f "$context/test/Containerfile.deb-assemble" \
    "$context"

podman run --rm --network=none \
    -v "$artifact_dir:/artifacts:Z" \
    "$assembler_image" assemble "$suite" 1 /artifacts

manifest="$(find "$artifact_dir" -maxdepth 1 -type f -name 'facelock_*.manifest' -print -quit)"
[ -n "$manifest" ] || {
    echo "Debian assembler did not emit an exact manifest" >&2
    exit 1
}

podman build \
    --build-arg "BASE_IMAGE=$base_image" \
    --build-arg "SUITE=$suite" \
    -t "$rebuild_image" \
    -f "$context/test/Containerfile.deb-rebuild" \
    "$context"
podman run --rm --network=none \
    -v "$artifact_dir:/artifacts:ro,Z" \
    -v "$rebuild_dir:/rebuild:Z" \
    "$rebuild_image" rebuild-dsc /artifacts /rebuild

package_name="$(grep -E '\.deb$' "$manifest")"
[ "$(printf '%s\n' "$package_name" | wc -l)" -eq 1 ] || {
    echo "Debian manifest must name exactly one binary package" >&2
    exit 1
}
cp -- "$artifact_dir/$package_name" "$context/facelock-test-package.deb"
podman build \
    --build-arg "BASE_IMAGE=$base_image" \
    -t "$image" \
    -f "$context/test/Containerfile.deb-runtime" \
    "$context"
