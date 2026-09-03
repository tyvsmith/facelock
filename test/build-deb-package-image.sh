#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
suite="${1:?usage: build-deb-package-image.sh <trixie|resolute> <image> <package-output>}"
image="${2:?usage: build-deb-package-image.sh <trixie|resolute> <image> <package-output>}"
package_output="${3:?usage: build-deb-package-image.sh <trixie|resolute> <image> <package-output>}"
[ "$#" -eq 3 ] || {
    echo "usage: build-deb-package-image.sh <trixie|resolute> <image> <package-output>" >&2
    exit 2
}

case "$suite" in
    trixie|resolute) ;;
    *) echo "unsupported Debian package suite: $suite" >&2; exit 2 ;;
esac

# Read the harness Containerfile before building anything from it. The suite
# package record the lifecycle gate reasons about can only be trusted if it is
# taken before that stage installs anything, and no run-time check can prove
# that about an instruction placed above the record.
"$repo_root/test/deb-runtime-image-contract.sh"

package_parent="$(dirname "$package_output")"
[ -d "$package_parent" ] || {
    echo "package output parent does not exist: $package_parent" >&2
    exit 1
}
package_parent="$(cd "$package_parent" && pwd)"
package_output="$package_parent/$(basename "$package_output")"
[ ! -e "$package_output" ] && [ ! -L "$package_output" ] || {
    echo "package output already exists: $package_output" >&2
    exit 1
}

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
cleanup() {
    local status=$?
    # The context holds a Git repository; retry once so that a straggling writer
    # cannot turn scratch cleanup into a spurious lane failure. Cleanup must never
    # decide the exit status, so restore whatever the script had already reached.
    rm -rf -- "$context" "$artifact_dir" "$rebuild_dir" 2>/dev/null ||
        rm -rf -- "$context" "$artifact_dir" "$rebuild_dir" || true
    exit "$status"
}
trap cleanup EXIT
"$repo_root/test/prepare-deb-test-context.sh" "$context" >/dev/null

# Container engines omit .git from COPY even when an alternate empty ignore
# file is supplied. Transport this isolated one-commit repository explicitly;
# the Containerfile restores it before invoking the exact-tag source builder.
tar -C "$context" -cf "$context/facelock-git-metadata.tar" .git
assembler_image="${image}-assembler"
rebuild_image="${image}-rebuild"
dependency_image="${image}-dependency-closure"
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
install -m 0444 -- "$artifact_dir/$package_name" "$package_output"
podman build \
    --build-arg "BASE_IMAGE=$base_image" \
    --target dependency-closure \
    -t "$dependency_image" \
    -f "$context/test/Containerfile.deb-runtime" \
    "$context"
podman run --rm \
    -v "$package_output:/facelock-test-package.deb:ro,Z" \
    "$dependency_image" /deb-dependency-closure.sh
podman build \
    --build-arg "BASE_IMAGE=$base_image" \
    --target runtime \
    -t "$image" \
    -f "$context/test/Containerfile.deb-runtime" \
    "$context"
