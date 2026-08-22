#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
    echo "cargo vendor bundle: $*" >&2
    exit 1
}

require_output_name() {
    local output="$1"
    [ "${output##*/}" = cargo-vendor ] ||
        fail "output directory basename must be cargo-vendor"
}

validate_tree() {
    local output="$1"
    [ -d "$output/vendor" ] || fail "missing vendor directory: $output/vendor"
    [ -f "$output/CARGO_LOCK.sha256" ] || fail "missing CARGO_LOCK.sha256"
    [ -f "$output/LEGAL-INVENTORY.json" ] || fail "missing LEGAL-INVENTORY.json"
    [ ! -L "$output" ] || fail "bundle root must not be a symlink"

    local entry relative
    while IFS= read -r -d '' entry; do
        relative="${entry#"$output"/}"
        case "$relative" in
            *[[:space:]]*) fail "whitespace is not allowed in bundle paths: $relative" ;;
        esac
        if [ -L "$entry" ] || { [ ! -d "$entry" ] && [ ! -f "$entry" ]; }; then
            fail "non-regular bundle entry: $relative"
        fi
        if [ -d "$entry" ]; then
            case "$relative" in
                .git|*/.git|.hg|*/.hg|.svn|*/.svn|target)
                    fail "forbidden bundle directory: $relative"
                    ;;
            esac
        fi
        case "$relative" in
            .cargo/credentials|*/.cargo/credentials|.cargo/credentials.toml|*/.cargo/credentials.toml)
                fail "forbidden bundle entry: $relative"
                ;;
        esac
        case "$relative" in
            *.crate|*.tmp|*.swp|*~|*/.DS_Store|.DS_Store)
                fail "cache or editor debris in bundle: $relative"
                ;;
        esac
    done < <(find "$output" -mindepth 1 -print0 | sort -z)
}

normalize_tree_modes() {
    local output="$1"
    find "$output" -type d -exec chmod 0755 {} +
    find "$output" -type f -perm /0111 -exec chmod 0755 {} +
    find "$output" -type f ! -perm /0111 -exec chmod 0644 {} +
}

write_legal_inventory() {
    local output="$1"
    local destination="$2"
    python3 - "$output" "$destination" "$repo_root/Cargo.lock" <<'PY'
import hashlib
import json
import sys
import tomllib
from pathlib import Path

bundle = Path(sys.argv[1])
destination = Path(sys.argv[2])
lock_path = Path(sys.argv[3])
lock = tomllib.loads(lock_path.read_text(encoding="utf-8"))
locked = {}
for package in lock["package"]:
    if "source" not in package:
        continue
    identity = (package["name"], package["version"])
    if identity in locked:
        raise SystemExit(f"duplicate external Cargo.lock identity: {identity}")
    locked[identity] = package

rows = []
seen = set()
vendor_root = bundle / "vendor"
for crate_dir in sorted(path for path in vendor_root.iterdir() if path.is_dir()):
    manifest_path = crate_dir / "Cargo.toml"
    if not manifest_path.is_file():
        raise SystemExit(f"vendored crate has no Cargo.toml: {crate_dir}")
    package = tomllib.loads(manifest_path.read_text(encoding="utf-8"))["package"]
    identity = (package["name"], package["version"])
    if identity in seen:
        raise SystemExit(f"duplicate vendored crate identity: {identity}")
    seen.add(identity)
    locked_package = locked.get(identity)
    if locked_package is None:
        raise SystemExit(f"vendored crate is absent from Cargo.lock: {identity}")

    checksum_path = crate_dir / ".cargo-checksum.json"
    if not checksum_path.is_file():
        raise SystemExit(f"vendored crate has no Cargo checksum record: {crate_dir.name}")
    vendor_checksum = json.loads(checksum_path.read_text(encoding="utf-8")).get("package")
    if vendor_checksum != locked_package.get("checksum"):
        raise SystemExit(f"vendored crate checksum differs from Cargo.lock: {identity}")

    license_expression = package.get("license")
    license_file = package.get("license-file")
    if not license_expression and not license_file:
        raise SystemExit(f"vendored crate declares neither license nor license-file: {identity}")

    materials = {
        path.relative_to(crate_dir).as_posix()
        for path in crate_dir.rglob("*")
        if path.is_file()
        and path.name.casefold().startswith(("license", "copying", "copyright", "notice"))
    }
    if license_file:
        if not isinstance(license_file, str):
            raise SystemExit(f"license-file is not a string for {identity}")
        license_path = Path(license_file)
        if license_path.is_absolute() or ".." in license_path.parts:
            raise SystemExit(f"license-file escapes vendored crate {identity}: {license_file}")
        declared_material = crate_dir / license_path
        current = crate_dir
        for component in license_path.parts:
            current /= component
            if current.is_symlink():
                raise SystemExit(f"declared license-file uses a symlink for {identity}: {license_file}")
        try:
            resolved_crate = crate_dir.resolve(strict=True)
            resolved_material = declared_material.resolve(strict=True)
        except FileNotFoundError as error:
            raise SystemExit(f"declared license-file is missing for {identity}: {license_file}") from error
        except RuntimeError as error:
            raise SystemExit(f"license-file escapes vendored crate {identity}: {license_file}") from error
        try:
            resolved_material.relative_to(resolved_crate)
        except ValueError as error:
            raise SystemExit(f"license-file escapes vendored crate {identity}: {license_file}") from error
        if not resolved_material.is_file():
            raise SystemExit(f"declared license-file is missing for {identity}: {license_file}")
        materials.add(license_path.as_posix())

    rows.append(
        {
            "authors": package.get("authors", []),
            "checksum": locked_package.get("checksum"),
            "documentation": package.get("documentation"),
            "homepage": package.get("homepage"),
            "license": license_expression,
            "license_file": license_file,
            "license_materials": sorted(materials),
            "lock_source": locked_package["source"],
            "name": package["name"],
            "path": crate_dir.relative_to(bundle).as_posix(),
            "repository": package.get("repository"),
            "version": package["version"],
        }
    )

if seen != set(locked):
    missing = sorted(set(locked) - seen)
    extra = sorted(seen - set(locked))
    raise SystemExit(f"vendored crates differ from Cargo.lock: missing={missing}, extra={extra}")

inventory = {
    "cargo_lock_sha256": hashlib.sha256(lock_path.read_bytes()).hexdigest(),
    "crates": sorted(rows, key=lambda row: row["path"]),
    "format": 1,
}
destination.write_text(
    json.dumps(inventory, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
PY
}

write_manifest() {
    local output="$1"
    local destination="$2"
    local scratch
    scratch="$(mktemp -d "${TMPDIR:-/tmp}/facelock-cargo-records.XXXXXX")"
    find "$output" -type f ! -path "$output/MANIFEST.sha256" \
        -printf '%m %s %p\n' | sort -k3,3 >"$scratch/metadata"
    find "$output" -type f ! -path "$output/MANIFEST.sha256" \
        -print0 | sort -z | xargs -0 -r sha256sum -- >"$scratch/hashes"
    awk -v prefix="$output/" '
        NR == FNR { mode[$3]=$1; size[$3]=$2; next }
        {
            digest=$1
            path=substr($0, 67)
            if (!(path in mode) || index(path, prefix) != 1) exit 1
            relative=substr(path, length(prefix) + 1)
            print mode[path], size[path], digest, relative
            seen[path]=1
        }
        END {
            for (path in mode) if (!(path in seen)) exit 1
        }
    ' "$scratch/metadata" "$scratch/hashes" >"$destination" || {
        rm -rf -- "$scratch"
        fail "could not assemble exact bundle manifest"
    }
    rm -rf -- "$scratch"
}

verify_bundle() {
    local output="$1"
    require_output_name "$output"
    [ -f "$output/MANIFEST.sha256" ] || fail "missing MANIFEST.sha256"
    validate_tree "$output"

    local expected_lock actual_lock recomputed recomputed_legal
    expected_lock="$(tr -d '\r\n' <"$output/CARGO_LOCK.sha256")"
    actual_lock="$(sha256sum Cargo.lock | cut -d' ' -f1)"
    [ "$expected_lock" = "$actual_lock" ] || fail "Cargo.lock hash mismatch"

    recomputed_legal="$(mktemp "${TMPDIR:-/tmp}/facelock-cargo-legal.XXXXXX")"
    write_legal_inventory "$output" "$recomputed_legal"
    cmp -s "$output/LEGAL-INVENTORY.json" "$recomputed_legal" || {
        rm -f -- "$recomputed_legal"
        fail "bundle legal inventory mismatch"
    }
    rm -f -- "$recomputed_legal"

    recomputed="$(mktemp "${TMPDIR:-/tmp}/facelock-cargo-manifest.XXXXXX")"
    trap 'rm -f -- "$recomputed"' RETURN
    write_manifest "$output" "$recomputed"
    cmp -s "$output/MANIFEST.sha256" "$recomputed" || fail "bundle manifest mismatch"
    rm -f -- "$recomputed"
    trap - RETURN
}

command_name="${1:-}"
case "$command_name" in
    prepare)
        [ "$#" -eq 2 ] || fail "usage: $0 prepare OUTPUT_DIR"
        output="$2"
        require_output_name "$output"
        [ ! -e "$output" ] && [ ! -L "$output" ] || fail "output already exists: $output"
        parent="$(dirname "$output")"
        mkdir -p "$parent"
        temporary="$(mktemp -d "$parent/.cargo-vendor.XXXXXX")"
        trap 'rm -rf -- "$temporary"' EXIT
        cargo vendor --quiet --locked --versioned-dirs --manifest-path Cargo.toml \
            "$temporary/vendor" >/dev/null
        sha256sum Cargo.lock | awk '{print $1}' >"$temporary/CARGO_LOCK.sha256"
        write_legal_inventory "$temporary" "$temporary/LEGAL-INVENTORY.json"
        normalize_tree_modes "$temporary"
        validate_tree "$temporary"
        write_manifest "$temporary" "$temporary/MANIFEST.sha256"
        chmod 0644 "$temporary/MANIFEST.sha256"
        mv -- "$temporary" "$output"
        trap - EXIT
        verify_bundle "$output"
        ;;
    verify)
        [ "$#" -eq 2 ] || fail "usage: $0 verify OUTPUT_DIR"
        verify_bundle "$2"
        ;;
    component-tar)
        [ "$#" -eq 3 ] || fail "usage: $0 component-tar OUTPUT_DIR ARCHIVE_PATH"
        output="$2"
        archive="$3"
        verify_bundle "$output"
        [ ! -e "$archive" ] && [ ! -L "$archive" ] || fail "archive already exists: $archive"
        tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
            --mode='u+rwX,go+rX,go-w' -C "$(dirname "$output")" \
            -cJf "$archive" "$(basename "$output")"
        ;;
    extract-component)
        [ "$#" -eq 3 ] || fail "usage: $0 extract-component ARCHIVE_PATH OUTPUT_DIR"
        output="$3"
        require_output_name "$output"
        python3 "$repo_root/scripts/extract-component-archive.py" "$2" "$output"
        verify_bundle "$output"
        ;;
    *)
        fail "usage: $0 {prepare|verify|component-tar|extract-component} ..."
        ;;
esac
