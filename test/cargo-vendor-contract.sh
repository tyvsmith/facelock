#!/usr/bin/env bash
# shellcheck disable=SC2016
# Mutation programs are single-quoted so the nested bash, not this test, expands them.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
helper="$repo_root/scripts/prepare-cargo-vendor.sh"
config="$repo_root/debian/cargo-config.toml"
copyright_file="$repo_root/debian/copyright"
workflow_file="$repo_root/.github/workflows/release.yml"

[ -x "$helper" ] || {
    echo "cargo vendor contract: missing executable scripts/prepare-cargo-vendor.sh" >&2
    exit 1
}
grep -Fqx 'export LC_ALL=C' "$helper" || {
    echo "cargo vendor contract: helper must pin the locale for every pipeline stage" >&2
    exit 1
}
[ -f "$config" ] || {
    echo "cargo vendor contract: missing debian/cargo-config.toml" >&2
    exit 1
}

python3 - "$copyright_file" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
paragraphs = path.read_text(encoding="utf-8").split("\n\n")
vendor_indexes = [
    index
    for index, paragraph in enumerate(paragraphs)
    if paragraph.startswith("Files: cargo-vendor/*\n")
]
catchall_indexes = [
    index
    for index, paragraph in enumerate(paragraphs)
    if paragraph.startswith("Files: *\n")
]
if len(vendor_indexes) != 1 or len(catchall_indexes) != 1:
    raise SystemExit("cargo vendor contract: copyright must contain one vendor stanza and one catch-all stanza")
if vendor_indexes[0] >= catchall_indexes[0]:
    raise SystemExit("cargo vendor contract: vendor copyright coverage must precede the Facelock catch-all")
vendor_stanza = paragraphs[vendor_indexes[0]]
if "Facelock Contributors" in vendor_stanza:
    raise SystemExit("cargo vendor contract: vendor copyright is falsely attributed to Facelock Contributors")
if "cargo-vendor/LEGAL-INVENTORY.json" not in vendor_stanza:
    raise SystemExit("cargo vendor contract: vendor copyright stanza must cite the exact generated legal inventory")
PY

prepare_job="$(awk '
    /^  prepare-cargo-vendor:[[:space:]]*$/ { in_job=1 }
    in_job && /^  [[:alnum:]_-]+:[[:space:]]*$/ && $1 != "prepare-cargo-vendor:" { exit }
    in_job { print }
' "$workflow_file")"
printf '%s\n' "$prepare_job" |
    grep -Fq 'uses: dtolnay/rust-toolchain@4360b52568e2003a75bf9bc1d59f33a8e3fc893c' || {
    echo "cargo vendor contract: source preparation must pin the toolchain action implementation" >&2
    exit 1
}
printf '%s\n' "$prepare_job" | grep -Eq '^[[:space:]]+toolchain:[[:space:]]+1\.95\.0[[:space:]]*$' || {
    echo "cargo vendor contract: source preparation must explicitly select Rust 1.95.0" >&2
    exit 1
}
for producer_contract in \
    'scripts/prepare-cargo-vendor.sh component-tar cargo-vendor cargo-vendor-bundle.tar.xz' \
    'path: cargo-vendor-bundle.tar.xz' \
    'archive: false'; do
    printf '%s\n' "$prepare_job" | grep -Fq "$producer_contract" || {
        echo "cargo vendor contract: source preparation is missing direct archive contract: $producer_contract" >&2
        exit 1
    }
done
if printf '%s\n' "$prepare_job" | grep -Eq 'path:[[:space:]]+cargo-vendor/?[[:space:]]*$'; then
    echo "cargo vendor contract: workflow must not transfer the raw Cargo vendor directory" >&2
    exit 1
fi

build_deb_job="$(awk '
    /^  build-deb:[[:space:]]*$/ { in_job=1 }
    in_job && /^  [[:alnum:]_-]+:[[:space:]]*$/ && $1 != "build-deb:" { exit }
    in_job { print }
' "$workflow_file")"
for consumer_contract in \
    'name: cargo-vendor-bundle.tar.xz' \
    'skip-decompress: true' \
    'scripts/prepare-cargo-vendor.sh extract-component cargo-vendor-bundle.tar.xz cargo-vendor' \
    'scripts/prepare-cargo-vendor.sh verify cargo-vendor'; do
    printf '%s\n' "$build_deb_job" | grep -Fq "$consumer_contract" || {
        echo "cargo vendor contract: Debian consumer is missing direct archive contract: $consumer_contract" >&2
        exit 1
    }
done

grep -Fqx 'replace-with = "facelock-vendored-sources"' "$config"
grep -Fqx 'directory = "cargo-vendor/vendor"' "$config"
grep -Fqx 'offline = true' "$config"
[ ! -e "$repo_root/.cargo/config.toml" ] || {
    echo "cargo vendor contract: package-only source replacement leaked into the developer root" >&2
    exit 1
}

tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/facelock-cargo-vendor-test.XXXXXX")"
trap 'rm -rf -- "$tmp_root"' EXIT

first="$tmp_root/first/cargo-vendor"
second="$tmp_root/second/cargo-vendor"
mkdir -p "$(dirname "$first")" "$(dirname "$second")"
utf8_locale="$(locale -a | awk 'tolower($0) == "c.utf8" || tolower($0) == "c.utf-8" { print; exit }')"
[ -n "$utf8_locale" ] || utf8_locale="$(locale -a | awk 'tolower($0) ~ /utf[-]?8$/ { print; exit }')"
[ -n "$utf8_locale" ] || {
    echo "cargo vendor contract: no UTF-8 locale is available for determinism coverage" >&2
    exit 1
}
env LC_ALL=C "$helper" prepare "$first"
env LC_ALL="$utf8_locale" "$helper" prepare "$second"
"$helper" verify "$first"
"$helper" verify "$second"
[ -f "$first/LEGAL-INVENTORY.json" ] || {
    echo "cargo vendor contract: generated bundle omits LEGAL-INVENTORY.json" >&2
    exit 1
}
cmp -s "$first/LEGAL-INVENTORY.json" "$second/LEGAL-INVENTORY.json" || {
    echo "cargo vendor contract: repeated legal inventories differ" >&2
    exit 1
}
cmp -s "$first/MANIFEST.sha256" "$second/MANIFEST.sha256" || {
    echo "cargo vendor contract: repeated manifests differ" >&2
    exit 1
}
cmp -s "$first/CARGO_LOCK.sha256" "$second/CARGO_LOCK.sha256" || {
    echo "cargo vendor contract: repeated lock identities differ" >&2
    exit 1
}

python3 - "$repo_root/Cargo.lock" "$first" <<'PY'
import hashlib
import json
import sys
import tomllib
from pathlib import Path

lock_path = Path(sys.argv[1])
bundle = Path(sys.argv[2])
inventory = json.loads((bundle / "LEGAL-INVENTORY.json").read_text(encoding="utf-8"))
lock = tomllib.loads(lock_path.read_text(encoding="utf-8"))
locked = {
    (package["name"], package["version"]): package
    for package in lock["package"]
    if "source" in package
}
if inventory.get("format") != 1:
    raise SystemExit("cargo vendor contract: unsupported legal inventory format")
if inventory.get("cargo_lock_sha256") != hashlib.sha256(lock_path.read_bytes()).hexdigest():
    raise SystemExit("cargo vendor contract: legal inventory is not bound to Cargo.lock")
rows = inventory.get("crates")
if not isinstance(rows, list):
    raise SystemExit("cargo vendor contract: legal inventory crates must be a list")
if [row.get("path") for row in rows] != sorted(row.get("path") for row in rows):
    raise SystemExit("cargo vendor contract: legal inventory is not sorted by exact vendor path")
seen = set()
for row in rows:
    required = {
        "name", "version", "path", "lock_source", "checksum", "license",
        "license_file", "authors", "homepage", "repository", "documentation",
        "license_materials",
    }
    if set(row) != required:
        raise SystemExit(f"cargo vendor contract: malformed legal row keys for {row.get('path')}")
    crate_dir = bundle / row["path"]
    manifest_path = crate_dir / "Cargo.toml"
    if not manifest_path.is_file():
        raise SystemExit(f"cargo vendor contract: legal row references missing crate {row['path']}")
    package = tomllib.loads(manifest_path.read_text(encoding="utf-8"))["package"]
    identity = (package["name"], package["version"])
    if identity in seen:
        raise SystemExit(f"cargo vendor contract: duplicate legal identity {identity}")
    seen.add(identity)
    locked_package = locked.get(identity)
    if locked_package is None:
        raise SystemExit(f"cargo vendor contract: unlocked vendored crate {identity}")
    expected = {
        "name": package["name"],
        "version": package["version"],
        "path": crate_dir.relative_to(bundle).as_posix(),
        "lock_source": locked_package["source"],
        "checksum": locked_package.get("checksum"),
        "license": package.get("license"),
        "license_file": package.get("license-file"),
        "authors": package.get("authors", []),
        "homepage": package.get("homepage"),
        "repository": package.get("repository"),
        "documentation": package.get("documentation"),
    }
    for field, value in expected.items():
        if row[field] != value:
            raise SystemExit(f"cargo vendor contract: {row['path']} {field} differs from vendored metadata")
    if not row["license"] and not row["license_file"]:
        raise SystemExit(f"cargo vendor contract: {row['path']} has no declared licensing metadata")
    materials = row["license_materials"]
    if not isinstance(materials, list) or materials != sorted(set(materials)):
        raise SystemExit(f"cargo vendor contract: {row['path']} license materials are not sorted and unique")
    for relative in materials:
        material = crate_dir / relative
        if not material.is_file() or material.is_symlink():
            raise SystemExit(f"cargo vendor contract: {row['path']} references missing license material {relative}")
    if row["license_file"] and row["license_file"] not in materials:
        raise SystemExit(f"cargo vendor contract: {row['path']} omits its declared license-file")
if seen != set(locked):
    missing = sorted(set(locked) - seen)
    extra = sorted(seen - set(locked))
    raise SystemExit(f"cargo vendor contract: legal inventory coverage differs from Cargo.lock: missing={missing}, extra={extra}")

representative_licenses = {
    ("adler2", "2.0.1"): "0BSD OR MIT OR Apache-2.0",
    ("av1-grain", "0.2.5"): "BSD-2-Clause",
    ("avif-serialize", "0.8.8"): "BSD-3-Clause",
    ("libloading", "0.8.9"): "ISC",
    ("icu_collections", "2.1.1"): "Unicode-3.0",
    ("foldhash", "0.1.5"): "Zlib",
    ("imgref", "1.12.0"): "CC0-1.0 OR Apache-2.0",
    ("aho-corasick", "1.1.4"): "Unlicense OR MIT",
    ("webpki-root-certs", "1.0.7"): "CDLA-Permissive-2.0",
    ("libfuzzer-sys", "0.4.12"): "(MIT OR Apache-2.0) AND NCSA",
}
actual_licenses = {(row["name"], row["version"]): row["license"] for row in rows}
for identity, license_expression in representative_licenses.items():
    if actual_licenses.get(identity) != license_expression:
        raise SystemExit(
            f"cargo vendor contract: representative legal identity drifted: "
            f"{identity} -> {actual_licenses.get(identity)!r}"
        )
PY

cut -d' ' -f4- "$first/MANIFEST.sha256" | LC_ALL=C sort -c
awk '
    NF < 4 || $1 !~ /^[0-7][0-7][0-7]$/ || $2 !~ /^[0-9]+$/ ||
        length($3) != 64 || $3 !~ /^[0-9a-f]+$/ { exit 1 }
' "$first/MANIFEST.sha256" || {
    echo "cargo vendor contract: malformed manifest record" >&2
    exit 1
}

first_tar="$tmp_root/first.tar.xz"
second_tar="$tmp_root/second.tar.xz"
"$helper" component-tar "$first" "$first_tar"
"$helper" component-tar "$second" "$second_tar"
cmp -s "$first_tar" "$second_tar" || {
    echo "cargo vendor contract: repeated component archives differ" >&2
    exit 1
}
extracted_root="$tmp_root/extracted"
mkdir "$extracted_root"
"$helper" extract-component "$first_tar" "$extracted_root/cargo-vendor"
"$helper" verify "$extracted_root/cargo-vendor" || {
    echo "cargo vendor contract: normalized component extraction does not match its manifest" >&2
    exit 1
}
hidden_checksum="$(find "$first/vendor" -name .cargo-checksum.json -type f -print -quit)"
[ -n "$hidden_checksum" ] || {
    echo "cargo vendor contract: prepared bundle lacks hidden Cargo checksum metadata" >&2
    exit 1
}
hidden_relative="${hidden_checksum#"$first/"}"
cmp -s "$hidden_checksum" "$extracted_root/cargo-vendor/$hidden_relative" || {
    echo "cargo vendor contract: component archive lost hidden Cargo checksum metadata" >&2
    exit 1
}
executable_relative="$(awk '$1 == "755" { print $4; exit }' "$first/MANIFEST.sha256")"
[ -n "$executable_relative" ] || {
    echo "cargo vendor contract: prepared bundle lacks a reviewed executable manifest entry" >&2
    exit 1
}
[ "$(stat -c %a "$extracted_root/cargo-vendor/$executable_relative")" = 755 ] || {
    echo "cargo vendor contract: component archive lost an executable manifest mode" >&2
    exit 1
}

unsafe_root="$tmp_root/unsafe-archive"
mkdir -p "$unsafe_root/payload" "$unsafe_root/symlink/cargo-vendor" \
    "$unsafe_root/extra/cargo-vendor" "$unsafe_root/extra/unexpected"
printf 'escape\n' >"$unsafe_root/payload/file"
tar -C "$unsafe_root" --transform='s#^payload#../facelock-archive-escape#' \
    -cJf "$unsafe_root/traversal.tar.xz" payload
if "$helper" extract-component "$unsafe_root/traversal.tar.xz" \
    "$unsafe_root/traversal-destination" >/dev/null 2>&1; then
    echo "cargo vendor contract: accepted path-traversing component archive" >&2
    exit 1
fi
[ ! -e "$tmp_root/facelock-archive-escape" ] || {
    echo "cargo vendor contract: unsafe extraction wrote outside its destination" >&2
    exit 1
}
ln -s ../outside "$unsafe_root/symlink/cargo-vendor/link"
tar -C "$unsafe_root/symlink" -cJf "$unsafe_root/symlink.tar.xz" cargo-vendor
if "$helper" extract-component "$unsafe_root/symlink.tar.xz" \
    "$unsafe_root/symlink-destination" >/dev/null 2>&1; then
    echo "cargo vendor contract: accepted symlinked component archive member" >&2
    exit 1
fi
printf 'extra\n' >"$unsafe_root/extra/unexpected/file"
tar -C "$unsafe_root/extra" -cJf "$unsafe_root/extra-root.tar.xz" cargo-vendor unexpected
if "$helper" extract-component "$unsafe_root/extra-root.tar.xz" \
    "$unsafe_root/extra-destination" >/dev/null 2>&1; then
    echo "cargo vendor contract: accepted a component archive with an extra root" >&2
    exit 1
fi

expect_rejected() {
    local label="$1"
    local mutation="$2"
    local case_root="$tmp_root/mutation-$label"
    local bundle="$case_root/cargo-vendor"
    mkdir -p "$case_root"
    cp -a --reflink=auto "$first" "$bundle"
    bash -c "$mutation" _ "$bundle"
    if "$helper" verify "$bundle" >/dev/null 2>&1; then
        echo "cargo vendor contract: accepted $label mutation" >&2
        exit 1
    fi
}

expect_rejected changed-file \
    'file=$(find "$1/vendor" -type f -print -quit); printf x >>"$file"'
expect_rejected missing-file \
    'file=$(find "$1/vendor" -type f -print -quit); rm -f -- "$file"'
expect_rejected extra-file \
    'printf extra >"$1/vendor/facelock-extra"'
expect_rejected symlink \
    'ln -s CARGO_LOCK.sha256 "$1/facelock-link"'
expect_rejected mode-drift \
    'file=$(find "$1/vendor" -type f -print -quit); chmod 600 "$file"'
expect_rejected lock-drift \
    'printf "%064d\n" 0 >"$1/CARGO_LOCK.sha256"'
expect_rejected vcs-metadata \
    'mkdir -p "$1/vendor/facelock-extra/.git"; printf x >"$1/vendor/facelock-extra/.git/config"'
expect_rejected credentials \
    'mkdir -p "$1/vendor/facelock-extra/.cargo"; printf x >"$1/vendor/facelock-extra/.cargo/credentials.toml"'
expect_rejected cache-debris \
    'printf x >"$1/vendor/facelock-extra.crate"'

license_crate_relative="$(python3 - "$first" <<'PY'
import sys
import tomllib
from pathlib import Path

bundle = Path(sys.argv[1])
for crate in sorted((bundle / "vendor").iterdir()):
    manifest = crate / "Cargo.toml"
    if not manifest.is_file():
        continue
    package = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]
    if "license-file" not in package:
        print(crate.relative_to(bundle).as_posix())
        break
else:
    raise SystemExit("cargo vendor contract: no crate is available for license-file path mutations")
PY
)"

expect_license_file_path_rejected() {
    local label="$1"
    local license_file="$2"
    local create_outside="${3:-false}"
    local case_root="$tmp_root/mutation-$label"
    local bundle="$case_root/cargo-vendor"
    local crate="$bundle/$license_crate_relative"
    mkdir -p "$case_root"
    cp -a --reflink=auto "$first" "$bundle"
    if [ "$create_outside" = true ]; then
        printf 'outside license material\n' >"$(dirname "$crate")/facelock-license-outside"
    fi
    python3 - "$crate/Cargo.toml" "$license_file" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
license_file = sys.argv[2]
text = path.read_text(encoding="utf-8")
marker = "[package]\n"
if marker not in text:
    raise SystemExit("cargo vendor contract: license-file mutation lacks [package]")
path.write_text(
    text.replace(marker, marker + f'license-file = "{license_file}"\n', 1),
    encoding="utf-8",
)
PY
    if "$helper" verify "$bundle" >"$case_root/output" 2>&1; then
        echo "cargo vendor contract: accepted $label license-file mutation" >&2
        exit 1
    fi
    grep -Fq 'license-file escapes vendored crate' "$case_root/output" || {
        echo "cargo vendor contract: $label was rejected for an unrelated reason" >&2
        cat "$case_root/output" >&2
        exit 1
    }
}

expect_license_file_path_rejected license-file-parent-escape \
    '../facelock-license-outside' true
expect_license_file_path_rejected license-file-absolute \
    '/tmp/facelock-license-outside'

license_symlink_case="$tmp_root/mutation-license-file-symlink/cargo-vendor"
mkdir -p "$(dirname "$license_symlink_case")"
cp -a --reflink=auto "$first" "$license_symlink_case"
license_symlink_crate="$license_symlink_case/$license_crate_relative"
python3 - "$license_symlink_crate/Cargo.toml" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
path.write_text(
    text.replace("[package]\n", '[package]\nlicense-file = "LICENSE-FACELOCK-ESCAPE"\n', 1),
    encoding="utf-8",
)
PY
printf 'outside license material\n' >"$(dirname "$license_symlink_crate")/facelock-license-outside"
ln -s ../facelock-license-outside "$license_symlink_crate/LICENSE-FACELOCK-ESCAPE"
if "$helper" verify "$license_symlink_case" >/dev/null 2>&1; then
    echo "cargo vendor contract: accepted symlinked license-file mutation" >&2
    exit 1
fi

legal_case="$tmp_root/mutation-legal-omission/cargo-vendor"
mkdir -p "$(dirname "$legal_case")"
cp -a --reflink=auto "$first" "$legal_case"
python3 - "$legal_case/LEGAL-INVENTORY.json" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
inventory = json.loads(path.read_text(encoding="utf-8"))
inventory["crates"].pop()
path.write_text(json.dumps(inventory, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
legal_size="$(stat -c %s "$legal_case/LEGAL-INVENTORY.json")"
legal_digest="$(sha256sum "$legal_case/LEGAL-INVENTORY.json" | cut -d' ' -f1)"
awk -v size="$legal_size" -v digest="$legal_digest" '
    $4 == "LEGAL-INVENTORY.json" { $1="644"; $2=size; $3=digest }
    { print }
' "$legal_case/MANIFEST.sha256" >"$legal_case/MANIFEST.sha256.new"
mv "$legal_case/MANIFEST.sha256.new" "$legal_case/MANIFEST.sha256"
if "$helper" verify "$legal_case" >/dev/null 2>&1; then
    echo "cargo vendor contract: accepted lock-bound legal inventory omission" >&2
    exit 1
fi

echo "cargo vendor contract: ok"
