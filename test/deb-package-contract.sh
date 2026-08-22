#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
# shellcheck source=/dev/null
source "$repo_root/scripts/release-versions.sh"

fail() {
    echo "deb package contract: $*" >&2
    exit 1
}

control_field() {
    local control_file="$1"
    local field="$2"
    awk -v field="$field" '
        $0 ~ "^" field ":[[:space:]]*" {
            collecting=1
            value=$0
            sub("^" field ":[[:space:]]*", "", value)
            next
        }
        collecting && /^[[:space:]]/ {
            continuation=$0
            sub(/^[[:space:]]*/, "", continuation)
            value=value " " continuation
            next
        }
        collecting { collecting=0; print value; exit }
        END { if (collecting) print value }
    ' "$control_file"
}

checksum_records() {
    local control_file="$1"
    awk '
        /^Checksums-Sha256:[[:space:]]*$/ { collecting=1; next }
        collecting && /^[[:space:]]/ {
            record=$0
            sub(/^[[:space:]]*/, "", record)
            print record
            next
        }
        collecting { exit }
    ' "$control_file"
}

verify_checksum_set() {
    local control_file="$1"
    local label="$2"
    shift 2
    local -a expected_names=("$@")
    local -A expected=()
    local -A seen=()
    local name record_hash record_size extra path actual_hash actual_size
    local -a records=()

    for name in "${expected_names[@]}"; do
        expected["$name"]=1
    done
    mapfile -t records < <(checksum_records "$control_file")
    [ "${#records[@]}" -eq "${#expected_names[@]}" ] ||
        fail "$label must checksum exactly ${#expected_names[@]} artifacts"

    for record in "${records[@]}"; do
        read -r record_hash record_size name extra <<<"$record"
        [ -z "${extra:-}" ] && [[ "$record_hash" =~ ^[0-9a-f]{64}$ ]] &&
            [[ "$record_size" =~ ^[0-9]+$ ]] && [ -n "${name:-}" ] ||
            fail "$label contains a malformed Checksums-Sha256 record: $record"
        case "$name" in
            */*|.|..) fail "$label contains an unsafe checksum artifact: $name" ;;
        esac
        [ -n "${expected[$name]:-}" ] || fail "$label checksums unexpected artifact: $name"
        [ -z "${seen[$name]:-}" ] || fail "$label checksums duplicate artifact: $name"
        seen["$name"]=1
        path="$manifest_dir/$name"
        [ -f "$path" ] && [ ! -L "$path" ] || fail "$label artifact is not a regular file: $path"
        actual_hash="$(sha256sum "$path" | cut -d' ' -f1)"
        actual_size="$(stat -c %s "$path")"
        [ "$actual_hash" = "$record_hash" ] || fail "$label SHA-256 mismatch: $name"
        [ "$actual_size" = "$record_size" ] || fail "$label size mismatch: $name"
    done
}

validate_binary_package() {
    local package="$1"
    local expected_version="${2:-}"
    local expected_architecture="${3:-}"
    local package_name depends package_version package_architecture
    local provides conflicts replaces

    [ -f "$package" ] && [ ! -L "$package" ] || fail "package does not exist as a regular file: $package"
    package_name="$(dpkg-deb --field "$package" Package)"
    depends="$(dpkg-deb --field "$package" Depends)"
    provides="$(dpkg-deb --field "$package" Provides)"
    conflicts="$(dpkg-deb --field "$package" Conflicts)"
    replaces="$(dpkg-deb --field "$package" Replaces)"
    [ "$package_name" = facelock ] || fail "$package has unexpected Package field: $package_name"
    [ -n "$depends" ] || fail "$package_name has no resolved Depends field"
    [ -z "$provides" ] || fail "$package_name must not declare Provides: $provides"
    [ -z "$conflicts" ] || fail "$package_name must not declare Conflicts: $conflicts"
    [ -z "$replaces" ] || fail "$package_name must not declare Replaces: $replaces"

    # Match unresolved literal debhelper variables, not shell expansions.
    # shellcheck disable=SC2016
    case "$depends" in
        *'${'*|*'#DEBHELPER#'*)
            fail "$package_name contains an unresolved substitution token"
            ;;
    esac

    printf '%s\n' "$depends" | grep -Eq '(^|,[[:space:]]*)dbus([[:space:]]*\([^)]*\))?([[:space:]]*,|$)' ||
        fail "$package_name lacks explicit dbus dependency"
    printf '%s\n' "$depends" | grep -Eq '(^|,[[:space:]]*)libpam-runtime([[:space:]]*\([^)]*\))?([[:space:]]*,|$)' ||
        fail "$package_name lacks explicit libpam-runtime dependency"
    printf '%s\n' "$depends" | grep -Eq '(^|,[[:space:]]*)libc6[[:space:]]*\(' ||
        fail "$package_name lacks a generated libc ABI dependency"
    printf '%s\n' "$depends" | grep -Eq '(^|,[[:space:]])libtss2-(esys|tctildr|mu)[^,]*([[:space:]]*,|$)' ||
        fail "$package_name lacks a generated TPM runtime dependency"

    if [ -n "$expected_version" ]; then
        package_version="$(dpkg-deb --field "$package" Version)"
        [ "$package_version" = "$expected_version" ] ||
            fail "$package_name version $package_version does not match manifest version $expected_version"
    fi
    if [ -n "$expected_architecture" ]; then
        package_architecture="$(dpkg-deb --field "$package" Architecture)"
        [ "$package_architecture" = "$expected_architecture" ] ||
            fail "$package_name architecture $package_architecture does not match manifest architecture $expected_architecture"
    fi

    "$script_dir/deb-maintscript-contract.sh" "$package"
}

packages=()
artifacts=()
manifest=""
manifest_dir=""
stage=""
stage_tmp=""
trap 'if [ -n "$stage_tmp" ]; then rm -rf -- "$stage_tmp"; fi' EXIT

if [ "${1:-}" = "--manifest" ]; then
    manifest="${2:-}"
    [ -n "$manifest" ] || fail "--manifest requires an exact manifest path"
    shift 2
    if [ "${1:-}" = "--stage" ]; then
        stage="${2:-}"
        [ -n "$stage" ] || fail "--stage requires a new destination directory"
        shift 2
    fi
    [ "$#" -eq 0 ] || fail "unexpected arguments after --manifest"
    [ -f "$manifest" ] && [ ! -L "$manifest" ] || fail "manifest does not exist as a regular file: $manifest"

    manifest_dir="$(cd "$(dirname "$manifest")" && pwd)"
    manifest_basename="$(basename "$manifest")"
    if [[ "$manifest_basename" =~ ^facelock_(.+)_([a-z0-9][a-z0-9-]*)\.manifest$ ]]; then
        package_version="${BASH_REMATCH[1]}"
        architecture="${BASH_REMATCH[2]}"
    else
        fail "manifest basename does not encode facelock version and architecture: $manifest_basename"
    fi
    upstream_version="${package_version%%-*}"
    [ -n "$upstream_version" ] && [ "$upstream_version" != "$package_version" ] ||
        fail "manifest package version lacks a Debian revision: $package_version"

    source_basename="facelock_$package_version"
    binary_basename="${source_basename}_$architecture"
    expected_artifacts=(
        "facelock_${upstream_version}.orig.tar.gz"
        "facelock_${upstream_version}.orig-onnxruntime.tar.gz"
        "facelock_${upstream_version}.orig-cargo-vendor.tar.xz"
        "${source_basename}.debian.tar.xz"
        "${source_basename}.dsc"
        "${binary_basename}.buildinfo"
        "${binary_basename}.deb"
        "${binary_basename}.changes"
    )
    mapfile -t manifest_artifacts <"$manifest"
    [ "${#manifest_artifacts[@]}" -eq "${#expected_artifacts[@]}" ] ||
        fail "manifest must name exactly eight canonical artifacts"
    for index in "${!expected_artifacts[@]}"; do
        [ "${manifest_artifacts[$index]}" = "${expected_artifacts[$index]}" ] ||
            fail "manifest artifact $((index + 1)) must be ${expected_artifacts[$index]}"
        path="$manifest_dir/${expected_artifacts[$index]}"
        [ -f "$path" ] && [ ! -L "$path" ] || fail "manifest artifact is not a regular file: $path"
        artifacts+=("$path")
    done

    changes="$manifest_dir/${binary_basename}.changes"
    dsc="$manifest_dir/${source_basename}.dsc"
    [ "$(control_field "$changes" Source)" = facelock ] || fail ".changes Source must be facelock"
    [ "$(control_field "$changes" Binary)" = facelock ] || fail ".changes Binary must be facelock"
    [ "$(control_field "$changes" Version)" = "$package_version" ] || fail ".changes Version does not match manifest"
    suite="$(control_field "$changes" Distribution)"
    expected_suffix="$(release_debian_suite_suffix "$suite")" || fail ".changes has unsupported Distribution: $suite"
    [[ "$package_version" == *"$expected_suffix" ]] ||
        fail "manifest version $package_version does not match .changes suite $suite ($expected_suffix)"

    changes_architectures=" $(control_field "$changes" Architecture) "
    [ "$changes_architectures" = " source $architecture " ] ||
        fail ".changes Architecture must be exactly 'source $architecture'"
    [ "$(control_field "$dsc" Source)" = facelock ] || fail ".dsc Source must be facelock"
    [ "$(control_field "$dsc" Version)" = "$package_version" ] || fail ".dsc Version does not match manifest"

    verify_checksum_set "$dsc" .dsc \
        "facelock_${upstream_version}.orig-cargo-vendor.tar.xz" \
        "facelock_${upstream_version}.orig-onnxruntime.tar.gz" \
        "facelock_${upstream_version}.orig.tar.gz" \
        "${source_basename}.debian.tar.xz"
    verify_checksum_set "$changes" .changes \
        "${source_basename}.dsc" \
        "facelock_${upstream_version}.orig-cargo-vendor.tar.xz" \
        "facelock_${upstream_version}.orig-onnxruntime.tar.gz" \
        "facelock_${upstream_version}.orig.tar.gz" \
        "${source_basename}.debian.tar.xz" \
        "${binary_basename}.buildinfo" \
        "${binary_basename}.deb"

    packages=("$manifest_dir/${binary_basename}.deb")
    validate_binary_package "${packages[0]}" "$package_version" "$architecture"
else
    [ "$#" -gt 0 ] || fail "usage: $0 <exact-package.deb> [...] | --manifest <manifest> [--stage <new-dir>]"
    packages=("$@")
    for package in "${packages[@]}"; do
        validate_binary_package "$package"
    done
fi

if [ -n "$stage" ]; then
    stage_parent="$(dirname "$stage")"
    stage_name="$(basename "$stage")"
    case "$stage_name" in
        ''|.|..) fail "unsafe stage destination: $stage" ;;
    esac
    [ -d "$stage_parent" ] || fail "stage parent does not exist: $stage_parent"
    stage_parent="$(cd "$stage_parent" && pwd)"
    stage="$stage_parent/$stage_name"
    [ ! -e "$stage" ] && [ ! -L "$stage" ] || fail "stage destination already exists: $stage"
    stage_tmp="$(mktemp -d "$stage_parent/.${stage_name}.tmp.XXXXXX")"
    for artifact in "${artifacts[@]}"; do
        cp -- "$artifact" "$stage_tmp/"
    done
    cp -- "$manifest" "$stage_tmp/"
    for artifact in "${artifacts[@]}" "$manifest"; do
        staged="$stage_tmp/$(basename "$artifact")"
        cmp -s -- "$artifact" "$staged" || fail "staged artifact differs from validated source: $(basename "$artifact")"
    done
    python3 "$script_dir/publish-directory-atomic.py" "$stage_tmp" "$stage"
    stage_tmp=""
fi

echo "deb package contract: ok"
