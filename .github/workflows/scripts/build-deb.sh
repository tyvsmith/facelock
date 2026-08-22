#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ "${FACELOCK_NETWORKLESS_ACTIVE:-0}" != 1 ]; then
    exec "$SCRIPT_DIR/run-networkless.sh" "$0" "$@"
fi

VERSION="${1:?Usage: build-deb.sh <VERSION> <SUITE> <REVISION>}"
SUITE="${2:?Usage: build-deb.sh <VERSION> <SUITE> <REVISION>}"
REVISION="${3:?Usage: build-deb.sh <VERSION> <SUITE> <REVISION>}"
if [ "$#" -ne 3 ]; then
    echo "Usage: build-deb.sh <VERSION> <SUITE> <REVISION>" >&2
    exit 2
fi

REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
# shellcheck source=/dev/null
source "$REPO_ROOT/scripts/release-versions.sh"

SOURCE_TAG="$(release_tag_from_cargo "$VERSION")"
DEBIAN_UPSTREAM="$(release_debian_upstream "$VERSION")"
PACKAGE_VERSION="$(release_debian_version "$VERSION" "$REVISION" "$SUITE")"
SOURCE_COMMIT="$(git -C "$REPO_ROOT" rev-parse --verify "${SOURCE_TAG}^{commit}")" || {
    echo "ERROR: release tag $SOURCE_TAG does not resolve to a commit" >&2
    exit 1
}
HEAD_COMMIT="$(git -C "$REPO_ROOT" rev-parse --verify 'HEAD^{commit}')"
if [ "$HEAD_COMMIT" != "$SOURCE_COMMIT" ]; then
    echo "ERROR: HEAD $HEAD_COMMIT is not release tag $SOURCE_TAG ($SOURCE_COMMIT)" >&2
    exit 1
fi

SOURCE_DATE_EPOCH="$(git -C "$REPO_ROOT" show -s --format=%ct "$SOURCE_COMMIT")"
export SOURCE_DATE_EPOCH

OUTPUT_DIR="${FACELOCK_DEB_OUTPUT_DIR:-$REPO_ROOT}"
install -d "$OUTPUT_DIR"

BUILD_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-${SUITE}.XXXXXX")"
trap 'rm -rf -- "$BUILD_ROOT"' EXIT

SOURCE_ROOT="$BUILD_ROOT/facelock-$DEBIAN_UPSTREAM"
ORIG_TAR="$BUILD_ROOT/facelock_${DEBIAN_UPSTREAM}.orig.tar.gz"
ORT_HELPER="$REPO_ROOT/scripts/prepare-ort-bundle.sh"
CARGO_VENDOR_HELPER="$REPO_ROOT/scripts/prepare-cargo-vendor.sh"

# The main orig archive is precisely the release tag. Git supplies tracked
# paths and commit timestamps; gzip -n removes the ambient gzip timestamp and
# filename so repeated builds of the same tag produce the same archive bytes.
git -C "$REPO_ROOT" archive \
    --format=tar \
    --prefix="facelock-$DEBIAN_UPSTREAM/" \
    "$SOURCE_COMMIT" |
    gzip -n >"$ORIG_TAR"
tar -xzf "$ORIG_TAR" -C "$BUILD_ROOT"

# ONNX Runtime is independently sourced, checksummed and reviewed. Require the
# canonical eight-file bundle, then keep it outside the exact tag-derived main
# orig tarball as a 3.0 (quilt) upstream component archive.
[ -x "$ORT_HELPER" ] || {
    echo "ERROR: missing executable ORT bundle verifier: $ORT_HELPER" >&2
    exit 1
}
"$ORT_HELPER" verify "$REPO_ROOT/onnxruntime"
ORT_TAR="$BUILD_ROOT/facelock_${DEBIAN_UPSTREAM}.orig-onnxruntime.tar.gz"
tar --sort=name \
    --mtime="@$SOURCE_DATE_EPOCH" \
    --owner=0 --group=0 --numeric-owner \
    -C "$REPO_ROOT" -cf - onnxruntime |
    gzip -n >"$ORT_TAR"
install -d "$SOURCE_ROOT/onnxruntime"
cp -a "$REPO_ROOT/onnxruntime/." "$SOURCE_ROOT/onnxruntime/"

# Cargo dependencies are another independently reviewed quilt component. The
# binary/source build below is forbidden from consulting crates.io: the exact
# Cargo.lock identity and every vendored path are verified before packaging.
[ -x "$CARGO_VENDOR_HELPER" ] || {
    echo "ERROR: missing executable Cargo vendor verifier: $CARGO_VENDOR_HELPER" >&2
    exit 1
}
"$CARGO_VENDOR_HELPER" verify "$REPO_ROOT/cargo-vendor"
CARGO_VENDOR_TAR="$BUILD_ROOT/facelock_${DEBIAN_UPSTREAM}.orig-cargo-vendor.tar.xz"
"$CARGO_VENDOR_HELPER" component-tar "$REPO_ROOT/cargo-vendor" "$CARGO_VENDOR_TAR"
install -d "$SOURCE_ROOT/cargo-vendor"
cp -a "$REPO_ROOT/cargo-vendor/." "$SOURCE_ROOT/cargo-vendor/"

CHANGELOG_DATE="$(git -C "$REPO_ROOT" show -s --format=%cD "$SOURCE_COMMIT")"
CHANGELOG_TMP="$SOURCE_ROOT/debian/changelog.new"
{
    printf 'facelock (%s) %s; urgency=medium\n\n' "$PACKAGE_VERSION" "$SUITE"
    printf '  * Release %s.\n\n' "$SOURCE_TAG"
    printf ' -- Ty Smith <ty@tysmith.me>  %s\n\n' "$CHANGELOG_DATE"
    cat "$SOURCE_ROOT/debian/changelog"
} >"$CHANGELOG_TMP"
mv "$CHANGELOG_TMP" "$SOURCE_ROOT/debian/changelog"

(
    cd "$BUILD_ROOT"
    dpkg-source -b "$SOURCE_ROOT"
)
(
    cd "$SOURCE_ROOT"
    dpkg-buildpackage -us -uc -sa
)

ARCHITECTURE="$(dpkg --print-architecture)"
CHANGES_BASENAME="facelock_${PACKAGE_VERSION}_${ARCHITECTURE}.changes"
CHANGES_PATH="$BUILD_ROOT/$CHANGES_BASENAME"
[ -f "$CHANGES_PATH" ] || {
    echo "ERROR: dpkg-buildpackage did not create $CHANGES_BASENAME" >&2
    exit 1
}

MANIFEST_PATH="$OUTPUT_DIR/facelock_${PACKAGE_VERSION}_${ARCHITECTURE}.manifest"
MANIFEST_TMP="$BUILD_ROOT/artifact.manifest"
MANIFEST_ARTIFACTS=(
    "facelock_${DEBIAN_UPSTREAM}.orig.tar.gz"
    "facelock_${DEBIAN_UPSTREAM}.orig-onnxruntime.tar.gz"
    "facelock_${DEBIAN_UPSTREAM}.orig-cargo-vendor.tar.xz"
    "facelock_${PACKAGE_VERSION}.debian.tar.xz"
    "facelock_${PACKAGE_VERSION}.dsc"
    "facelock_${PACKAGE_VERSION}_${ARCHITECTURE}.buildinfo"
    "facelock_${PACKAGE_VERSION}_${ARCHITECTURE}.deb"
    "facelock_${PACKAGE_VERSION}_${ARCHITECTURE}.changes"
)
mapfile -t CHANGES_ARTIFACTS < <(awk '
    /^Files:$/ { in_files=1; next }
    in_files && /^[^ ]/ { in_files=0 }
    in_files && /^ / { print $5 }
' "$CHANGES_PATH")
[ "${#CHANGES_ARTIFACTS[@]}" -eq 7 ] || {
    echo "ERROR: $CHANGES_BASENAME must list exactly seven payload artifacts" >&2
    exit 1
}
declare -A EXPECTED_CHANGES_ARTIFACTS=()
declare -A SEEN_CHANGES_ARTIFACTS=()
for index in {0..6}; do
    EXPECTED_CHANGES_ARTIFACTS["${MANIFEST_ARTIFACTS[$index]}"]=1
done
for artifact in "${CHANGES_ARTIFACTS[@]}"; do
    case "$artifact" in
        ''|*/*|.|..)
            echo "ERROR: unsafe artifact name in $CHANGES_BASENAME: $artifact" >&2
            exit 1
            ;;
    esac
    [ -n "${EXPECTED_CHANGES_ARTIFACTS[$artifact]:-}" ] || {
        echo "ERROR: unexpected artifact in $CHANGES_BASENAME: $artifact" >&2
        exit 1
    }
    [ -z "${SEEN_CHANGES_ARTIFACTS[$artifact]:-}" ] || {
        echo "ERROR: duplicate artifact in $CHANGES_BASENAME: $artifact" >&2
        exit 1
    }
    SEEN_CHANGES_ARTIFACTS["$artifact"]=1
done
for index in {0..6}; do
    artifact="${MANIFEST_ARTIFACTS[$index]}"
    [ -n "${SEEN_CHANGES_ARTIFACTS[$artifact]:-}" ] || {
        echo "ERROR: $CHANGES_BASENAME omits expected artifact: $artifact" >&2
        exit 1
    }
done
printf '%s\n' "${MANIFEST_ARTIFACTS[@]}" >"$MANIFEST_TMP"

while IFS= read -r artifact; do
    case "$artifact" in
        ''|*/*|.|..)
            echo "ERROR: unsafe artifact name in $CHANGES_BASENAME: $artifact" >&2
            exit 1
            ;;
    esac
    [ -f "$BUILD_ROOT/$artifact" ] || {
        echo "ERROR: listed artifact does not exist: $artifact" >&2
        exit 1
    }
    [ ! -e "$OUTPUT_DIR/$artifact" ] || {
        echo "ERROR: refusing to overwrite existing artifact: $OUTPUT_DIR/$artifact" >&2
        exit 1
    }
    cp "$BUILD_ROOT/$artifact" "$OUTPUT_DIR/$artifact"
done <"$MANIFEST_TMP"

[ ! -e "$MANIFEST_PATH" ] || {
    echo "ERROR: refusing to overwrite existing manifest: $MANIFEST_PATH" >&2
    exit 1
}
cp "$MANIFEST_TMP" "$MANIFEST_PATH"
printf 'Debian artifact manifest: %s\n' "$MANIFEST_PATH"
