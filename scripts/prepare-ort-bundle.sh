#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Canonical reviewed ONNX Runtime bundle identity. Keep download, source-package,
# and local package gates on this single trust root.
ORT_VERSION='1.20.1'
ORT_SOURCE_URL="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-x64-${ORT_VERSION}.tgz"
ORT_ARCHIVE_SHA256='67db4dc1561f1e3fd42e619575c82c601ef89849afc7ea85a003abbac1a1a105'
ORT_LIBRARY_SHA256='a5faaf78a37590d3fe640f887620e74f6022d34550172b91ad2131bf0ad77d64'
ORT_GIT_COMMIT='5c1b7ccbff7e5141c1da7a9d963d660e5741c319'
ORT_LICENSE='MIT'
ORT_BUNDLE_CHECKSUMS_SHA256='e1b3397670dcabfea8b0d0608409b8409488267185fa82c99442d7c694486225'

fail() {
    echo "ORT bundle: $*" >&2
    exit 1
}

require_mode() {
    local path="$1"
    local expected="$2"
    local actual
    actual="$(stat -c %a "$path")"
    [ "$actual" = "$expected" ] ||
        fail "unexpected mode for $path: expected $expected, got $actual"
}

require_exact_bundle() {
    local bundle="$1"
    local actual_hash actual_version actual_commit entry index
    local -a expected_entries=(
        GIT_COMMIT_ID
        LICENSE
        PROVENANCE.md
        SHA256SUMS
        ThirdPartyNotices.txt
        VERSION_NUMBER
        lib
        lib/libonnxruntime.so
        manifest.json
    )
    local -a actual_entries=()

    [ -d "$bundle" ] && [ ! -L "$bundle" ] || fail "bundle is not a real directory: $bundle"
    mapfile -t actual_entries < <(find "$bundle" -mindepth 1 -printf '%P\n' | LC_ALL=C sort)
    [ "${#actual_entries[@]}" -eq "${#expected_entries[@]}" ] ||
        fail "bundle must contain exactly the reviewed eight files and lib directory"
    for index in "${!expected_entries[@]}"; do
        [ "${actual_entries[$index]}" = "${expected_entries[$index]}" ] ||
            fail "unexpected bundle entry: ${actual_entries[$index]}"
    done
    for entry in "${expected_entries[@]}"; do
        [ "$entry" = lib ] && continue
        [ -f "$bundle/$entry" ] && [ ! -L "$bundle/$entry" ] ||
            fail "bundle member is not a regular file: $entry"
    done
    require_mode "$bundle" 755
    require_mode "$bundle/lib" 755
    require_mode "$bundle/lib/libonnxruntime.so" 755
    for entry in GIT_COMMIT_ID LICENSE PROVENANCE.md SHA256SUMS \
        ThirdPartyNotices.txt VERSION_NUMBER manifest.json; do
        require_mode "$bundle/$entry" 644
    done

    actual_hash="$(sha256sum "$bundle/SHA256SUMS" | cut -d' ' -f1)"
    [ "$actual_hash" = "$ORT_BUNDLE_CHECKSUMS_SHA256" ] ||
        fail "SHA256SUMS does not match the reviewed bundle"
    (cd "$bundle" && sha256sum --strict --check SHA256SUMS >/dev/null) ||
        fail "bundle member checksum verification failed"
    actual_version="$(tr -d '\r\n' <"$bundle/VERSION_NUMBER")"
    [ "$actual_version" = "$ORT_VERSION" ] || fail "unexpected VERSION_NUMBER: $actual_version"
    actual_commit="$(tr -d '\r\n' <"$bundle/GIT_COMMIT_ID")"
    [ "$actual_commit" = "$ORT_GIT_COMMIT" ] || fail "unexpected GIT_COMMIT_ID: $actual_commit"
}

require_installed_bundle() {
    local library="$1"
    local documents="$2"
    local actual_hash entry expected_hash extra path
    local -a expected_documents=(
        GIT_COMMIT_ID
        LICENSE
        PROVENANCE.md
        SHA256SUMS
        ThirdPartyNotices.txt
        VERSION_NUMBER
        manifest.json
    )
    local -a actual_documents=()

    [ -f "$library" ] && [ ! -L "$library" ] ||
        fail "installed library is not a regular file: $library"
    [ -d "$documents" ] && [ ! -L "$documents" ] ||
        fail "installed document root is not a real directory: $documents"
    mapfile -t actual_documents < <(find "$documents" -mindepth 1 -printf '%P\n' | LC_ALL=C sort)
    [ "${#actual_documents[@]}" -eq "${#expected_documents[@]}" ] ||
        fail "installed bundle must contain exactly the reviewed seven metadata files"
    for entry in "${!expected_documents[@]}"; do
        [ "${actual_documents[$entry]}" = "${expected_documents[$entry]}" ] ||
            fail "unexpected installed metadata entry: ${actual_documents[$entry]}"
    done
    for entry in "${expected_documents[@]}"; do
        [ -f "$documents/$entry" ] && [ ! -L "$documents/$entry" ] ||
            fail "installed metadata is not a regular file: $entry"
    done

    actual_hash="$(sha256sum "$documents/SHA256SUMS" | cut -d' ' -f1)"
    [ "$actual_hash" = "$ORT_BUNDLE_CHECKSUMS_SHA256" ] ||
        fail "installed SHA256SUMS does not match the reviewed bundle"
    while read -r expected_hash entry extra; do
        [ -z "${extra:-}" ] || fail "malformed installed checksum record"
        [ -n "${entry:-}" ] || fail "malformed installed checksum record"
        case "$entry" in
            lib/libonnxruntime.so) path="$library" ;;
            *) path="$documents/$entry" ;;
        esac
        actual_hash="$(sha256sum "$path" | cut -d' ' -f1)"
        [ "$actual_hash" = "$expected_hash" ] || fail "installed checksum mismatch: $entry"
    done <"$documents/SHA256SUMS"
}

prepare_bundle() {
    local archive="$1"
    local destination="$2"
    local destination_parent destination_name work_root upstream_root bundle
    local archive_hash

    [ -f "$archive" ] && [ ! -L "$archive" ] || fail "archive is not a regular file: $archive"
    archive_hash="$(sha256sum "$archive" | cut -d' ' -f1)"
    [ "$archive_hash" = "$ORT_ARCHIVE_SHA256" ] || fail "upstream archive SHA-256 mismatch"

    destination_parent="$(dirname "$destination")"
    destination_name="$(basename "$destination")"
    case "$destination_name" in
        ''|.|..) fail "unsafe bundle destination: $destination" ;;
    esac
    [ -d "$destination_parent" ] || fail "bundle destination parent does not exist: $destination_parent"
    destination_parent="$(cd "$destination_parent" && pwd)"
    destination="$destination_parent/$destination_name"
    [ ! -e "$destination" ] && [ ! -L "$destination" ] ||
        fail "bundle destination already exists: $destination"

    work_root="$(mktemp -d "$destination_parent/.onnxruntime-prepare.XXXXXX")"
    trap 'rm -rf -- "$work_root"' RETURN
    tar -xzf "$archive" -C "$work_root"
    upstream_root="$work_root/onnxruntime-linux-x64-$ORT_VERSION"
    [ -d "$upstream_root" ] && [ ! -L "$upstream_root" ] ||
        fail "archive lacks the reviewed ONNX Runtime root"
    for entry in LICENSE ThirdPartyNotices.txt VERSION_NUMBER GIT_COMMIT_ID \
        "lib/libonnxruntime.so.$ORT_VERSION"; do
        [ -f "$upstream_root/$entry" ] && [ ! -L "$upstream_root/$entry" ] ||
            fail "archive member is not a regular file: $entry"
    done

    bundle="$work_root/bundle"
    install -d "$bundle/lib"
    install -m755 "$upstream_root/lib/libonnxruntime.so.$ORT_VERSION" \
        "$bundle/lib/libonnxruntime.so"
    install -m644 "$upstream_root/LICENSE" "$upstream_root/ThirdPartyNotices.txt" \
        "$upstream_root/VERSION_NUMBER" "$upstream_root/GIT_COMMIT_ID" "$bundle/"
    printf '%s\n' \
        "ONNX Runtime $ORT_VERSION ($ORT_LICENSE)" \
        '' \
        "Upstream archive: $ORT_SOURCE_URL" \
        "Archive SHA-256: $ORT_ARCHIVE_SHA256" \
        "Upstream commit: $ORT_GIT_COMMIT" \
        "Library SHA-256: $ORT_LIBRARY_SHA256" \
        >"$bundle/PROVENANCE.md"
    printf '%s\n' \
        '{' \
        '  "component": "onnxruntime",' \
        "  \"library_sha256\": \"$ORT_LIBRARY_SHA256\"," \
        "  \"license\": \"$ORT_LICENSE\"," \
        '  "provenance": "GitHub release archive tied to the reviewed upstream commit",' \
        "  \"purl\": \"pkg:github/microsoft/onnxruntime@$ORT_VERSION\"," \
        "  \"source_sha256\": \"$ORT_ARCHIVE_SHA256\"," \
        "  \"source_url\": \"$ORT_SOURCE_URL\"," \
        "  \"upstream_commit\": \"$ORT_GIT_COMMIT\"," \
        "  \"version\": \"$ORT_VERSION\"" \
        '}' \
        >"$bundle/manifest.json"
    (
        cd "$bundle"
        sha256sum \
            GIT_COMMIT_ID LICENSE PROVENANCE.md ThirdPartyNotices.txt VERSION_NUMBER \
            lib/libonnxruntime.so manifest.json >SHA256SUMS
    )
    require_exact_bundle "$bundle"
    mv -T -- "$bundle" "$destination"
    trap - RETURN
    rm -rf -- "$work_root"
}

case "${1:-}" in
    source-url)
        [ "$#" -eq 1 ] || fail "usage: $0 source-url"
        printf '%s\n' "$ORT_SOURCE_URL"
        ;;
    prepare)
        [ "$#" -eq 3 ] || fail "usage: $0 prepare <pinned-archive> <new-bundle-dir>"
        prepare_bundle "$2" "$3"
        ;;
    verify)
        [ "$#" -eq 2 ] || fail "usage: $0 verify <bundle-dir>"
        require_exact_bundle "$2"
        ;;
    verify-installed)
        [ "$#" -eq 3 ] || fail "usage: $0 verify-installed <library> <metadata-dir>"
        require_installed_bundle "$2" "$3"
        ;;
    component-tar)
        [ "$#" -eq 3 ] || fail "usage: $0 component-tar <bundle-dir> <new-archive>"
        [ "${2##*/}" = onnxruntime ] || fail "bundle directory basename must be onnxruntime"
        require_exact_bundle "$2"
        [ ! -e "$3" ] && [ ! -L "$3" ] || fail "archive already exists: $3"
        tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
            --mode='u+rwX,go+rX,go-w' -C "$(dirname "$2")" \
            -cJf "$3" "$(basename "$2")"
        ;;
    extract-component)
        [ "$#" -eq 3 ] || fail "usage: $0 extract-component <archive> <new-bundle-dir>"
        [ "${3##*/}" = onnxruntime ] || fail "bundle directory basename must be onnxruntime"
        python3 "$repo_root/scripts/extract-component-archive.py" "$2" "$3"
        require_exact_bundle "$3"
        ;;
    *)
        fail "usage: $0 {source-url|prepare|verify|verify-installed|component-tar|extract-component}"
        ;;
esac
