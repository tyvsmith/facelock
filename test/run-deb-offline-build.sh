#!/usr/bin/env bash
set -euo pipefail

fail() {
    echo "offline Debian build: $*" >&2
    exit 1
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -x "$script_dir/../.github/workflows/scripts/run-networkless.sh" ]; then
    repo_root="$(cd "$script_dir/.." && pwd)"
    networkless="$repo_root/.github/workflows/scripts/run-networkless.sh"
else
    repo_root=""
    networkless="/usr/local/libexec/facelock/run-networkless.sh"
fi

[ -x "$networkless" ] || fail "missing network sandbox helper: $networkless"

export CARGO_HOME="${CARGO_HOME:-/tmp/facelock-empty-cargo-home}"
export RUSTUP_HOME="${RUSTUP_HOME:-/tmp/facelock-empty-rustup-home}"
for empty_home in "$CARGO_HOME" "$RUSTUP_HOME"; do
    install -d -m700 "$empty_home"
    [ -z "$(find "$empty_home" -mindepth 1 -print -quit)" ] ||
        fail "isolated toolchain home is not empty: $empty_home"
done

if [ "${FACELOCK_NETWORKLESS_ACTIVE:-0}" != 1 ]; then
    exec "$networkless" "$0" "$@"
fi
[ "$FACELOCK_NETWORKLESS_ACTIVE" = 1 ] || fail "network sandbox marker is absent"

exact_manifest() {
    local directory="$1"
    local -a manifests=()
    mapfile -d '' -t manifests < <(
        find "$directory" -maxdepth 1 -type f -name 'facelock_*.manifest' -print0
    )
    [ "${#manifests[@]}" -eq 1 ] ||
        fail "expected exactly one generated manifest in $directory"
    printf '%s\n' "${manifests[0]}"
}

compare_packages() {
    local release_deb="$1"
    local rebuilt_deb="$2"
    local field
    for field in Package Version Architecture Depends; do
        [ "$(dpkg-deb --field "$release_deb" "$field")" = \
          "$(dpkg-deb --field "$rebuilt_deb" "$field")" ] ||
            fail "clean rebuild changed binary field: $field"
    done

    local comparison_root release_root rebuilt_root
    comparison_root="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-compare.XXXXXX")"
    release_root="$comparison_root/release"
    rebuilt_root="$comparison_root/rebuilt"
    mkdir "$release_root" "$rebuilt_root"
    dpkg-deb --extract "$release_deb" "$release_root"
    dpkg-deb --extract "$rebuilt_deb" "$rebuilt_root"
    (
        cd "$release_root"
        find . -mindepth 1 -printf '%P %y %l\n' | LC_ALL=C sort
    ) >"$comparison_root/release.paths"
    (
        cd "$rebuilt_root"
        find . -mindepth 1 -printf '%P %y %l\n' | LC_ALL=C sort
    ) >"$comparison_root/rebuilt.paths"
    cmp -s "$comparison_root/release.paths" "$comparison_root/rebuilt.paths" ||
        fail "clean rebuild changed installed path/type/link inventory"
    (
        cd "$release_root"
        find . -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum
    ) >"$comparison_root/release.hashes"
    (
        cd "$rebuilt_root"
        find . -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum
    ) >"$comparison_root/rebuilt.hashes"
    if ! cmp -s "$comparison_root/release.hashes" "$comparison_root/rebuilt.hashes"; then
        echo "offline Debian build: installed hash differences follow" >&2
        diff -u "$comparison_root/release.hashes" "$comparison_root/rebuilt.hashes" >&2 || true
        fail "clean rebuild changed installed file bytes"
    fi
    rm -rf -- "$comparison_root"
}

mode="${1:-}"
case "$mode" in
    assemble)
        [ "$#" -eq 4 ] || fail "usage: $0 assemble <trixie|resolute> <revision> <output-dir>"
        suite="$2"
        revision="$3"
        output_dir="$4"
        case "$suite" in trixie|resolute) ;; *) fail "unsupported suite: $suite" ;; esac
        [ -n "$repo_root" ] || fail "assemble mode must run from the exact source tree"
        [ -d "$output_dir" ] && [ -z "$(find "$output_dir" -mindepth 1 -print -quit)" ] ||
            fail "assemble output must be an existing empty directory: $output_dir"
        "$repo_root/scripts/prepare-ort-bundle.sh" verify "$repo_root/onnxruntime"
        "$repo_root/scripts/prepare-cargo-vendor.sh" verify "$repo_root/cargo-vendor"
        version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_root/Cargo.toml" | head -1)"
        FACELOCK_DEB_OUTPUT_DIR="$output_dir" \
            "$repo_root/.github/workflows/scripts/build-deb.sh" "$version" "$suite" "$revision"
        manifest="$(exact_manifest "$output_dir")"
        bash "$repo_root/test/deb-package-contract.sh" --manifest "$manifest"
        ;;
    rebuild-dsc)
        [ "$#" -eq 3 ] || fail "usage: $0 rebuild-dsc <artifact-dir> <output-dir>"
        artifact_dir="$2"
        output_dir="$3"
        [ -d "$artifact_dir" ] || fail "missing artifact directory: $artifact_dir"
        [ -d "$output_dir" ] && [ -z "$(find "$output_dir" -mindepth 1 -print -quit)" ] ||
            fail "rebuild output must be an existing empty directory: $output_dir"
        manifest="$(exact_manifest "$artifact_dir")"
        dsc_name="$(grep -E '\.dsc$' "$manifest")"
        release_deb_name="$(grep -E '\.deb$' "$manifest")"
        [ "$(printf '%s\n' "$dsc_name" | wc -l)" -eq 1 ] || fail "manifest must name one dsc"
        [ "$(printf '%s\n' "$release_deb_name" | wc -l)" -eq 1 ] || fail "manifest must name one deb"
        rebuild_root="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-rebuild.XXXXXX")"
        trap 'rm -rf -- "$rebuild_root"' EXIT
        dpkg-source -x "$artifact_dir/$dsc_name" "$rebuild_root/source"
        (
            cd "$rebuild_root/source"
            scripts/prepare-ort-bundle.sh verify onnxruntime
            scripts/prepare-cargo-vendor.sh verify cargo-vendor
            dpkg-buildpackage -b -us -uc
        )
        release_deb="$artifact_dir/$release_deb_name"
        package_version="$(dpkg-deb --field "$release_deb" Version)"
        architecture="$(dpkg-deb --field "$release_deb" Architecture)"
        rebuilt_deb="$rebuild_root/facelock_${package_version}_${architecture}.deb"
        [ -f "$rebuilt_deb" ] || fail "clean rebuild did not emit expected deb: $rebuilt_deb"
        compare_packages "$release_deb" "$rebuilt_deb"
        cp -- "$rebuilt_deb" "$output_dir/"
        ;;
    *)
        fail "usage: $0 {assemble|rebuild-dsc} ..."
        ;;
esac
