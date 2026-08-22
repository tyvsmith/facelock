#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract="$repo_root/test/deb-package-contract.sh"
publisher="$repo_root/test/publish-directory-atomic.py"

fail() {
    echo "deb package contract test: $*" >&2
    exit 1
}

assert_rejected() {
    local context="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$context"
    fi
}

fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-package-contract.XXXXXX")"
trap 'rm -rf -- "$fixture_root"' EXIT
mkdir -p "$fixture_root/bin"

cat >"$fixture_root/bin/dpkg-deb" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
    --field|-f)
        package="${2:?}"
        field="${3:?}"
        case "$field" in
            Package) printf '%s\n' facelock ;;
            Depends)
                if [ -f "$package.depends" ]; then
                    sed -n '1p' "$package.depends"
                else
                    printf '%s\n' 'dbus, libpam-runtime, libc6 (>= 2.36), libtss2-esys-3.0.2-0t64 (>= 4.1.3)'
                fi
                ;;
            Provides|Conflicts|Replaces)
                field_path="$package.${field,,}"
                [ ! -f "$field_path" ] || sed -n '1p' "$field_path"
                ;;
            Version)
                if [ -f "$package.version" ]; then
                    sed -n '1p' "$package.version"
                else
                    printf '%s\n' '0.1.4-1~ubuntu26.04.1'
                fi
                ;;
            Architecture)
                if [ -f "$package.architecture" ]; then
                    sed -n '1p' "$package.architecture"
                else
                    printf '%s\n' amd64
                fi
                ;;
            *) exit 2 ;;
        esac
        ;;
    --control)
        destination="${3:?}"
        mkdir -p "$destination"
        if [ -f "${2:?}.postinst" ]; then
            cp "${2:?}.postinst" "$destination/postinst"
        else
            printf '%s\n' '#!/bin/sh' 'exit 0' >"$destination/postinst"
        fi
        ;;
    *) exit 2 ;;
esac
SH
chmod +x "$fixture_root/bin/dpkg-deb"

write_checksum_record() {
    local path="$1"
    printf ' %s %s %s\n' \
        "$(sha256sum "$path" | cut -d' ' -f1)" \
        "$(stat -c %s "$path")" \
        "$(basename "$path")"
}

create_fixture() {
    local root="$1"
    local version='0.1.4-1~ubuntu26.04.1'
    local source_basename="facelock_$version"
    local binary_basename="${source_basename}_amd64"
    mkdir -p "$root"

    printf '%s\n' source >"$root/facelock_0.1.4.orig.tar.gz"
    printf '%s\n' ort >"$root/facelock_0.1.4.orig-onnxruntime.tar.gz"
    printf '%s\n' cargo >"$root/facelock_0.1.4.orig-cargo-vendor.tar.xz"
    printf '%s\n' delta >"$root/${source_basename}.debian.tar.xz"
    {
        printf '%s\n' \
            'Format: 3.0 (quilt)' \
            'Source: facelock' \
            "Version: $version" \
            'Checksums-Sha256:'
        write_checksum_record "$root/facelock_0.1.4.orig.tar.gz"
        write_checksum_record "$root/facelock_0.1.4.orig-onnxruntime.tar.gz"
        write_checksum_record "$root/facelock_0.1.4.orig-cargo-vendor.tar.xz"
        write_checksum_record "$root/${source_basename}.debian.tar.xz"
        printf '%s\n' 'Files:'
    } >"$root/${source_basename}.dsc"

    printf '%s\n' buildinfo >"$root/${binary_basename}.buildinfo"
    printf '%s\n' package >"$root/${binary_basename}.deb"
    {
        printf '%s\n' \
            'Format: 1.8' \
            'Source: facelock' \
            'Binary: facelock' \
            'Architecture: source amd64' \
            "Version: $version" \
            'Distribution: resolute' \
            'Checksums-Sha256:'
        write_checksum_record "$root/facelock_0.1.4.orig.tar.gz"
        write_checksum_record "$root/facelock_0.1.4.orig-onnxruntime.tar.gz"
        write_checksum_record "$root/facelock_0.1.4.orig-cargo-vendor.tar.xz"
        write_checksum_record "$root/${source_basename}.debian.tar.xz"
        write_checksum_record "$root/${source_basename}.dsc"
        write_checksum_record "$root/${binary_basename}.buildinfo"
        write_checksum_record "$root/${binary_basename}.deb"
        printf '%s\n' 'Files:'
    } >"$root/${binary_basename}.changes"

    printf '%s\n' \
        'facelock_0.1.4.orig.tar.gz' \
        'facelock_0.1.4.orig-onnxruntime.tar.gz' \
        'facelock_0.1.4.orig-cargo-vendor.tar.xz' \
        "${source_basename}.debian.tar.xz" \
        "${source_basename}.dsc" \
        "${binary_basename}.buildinfo" \
        "${binary_basename}.deb" \
        "${binary_basename}.changes" \
        >"$root/${binary_basename}.manifest"
}

baseline="$fixture_root/baseline"
create_fixture "$baseline"
manifest_name='facelock_0.1.4-1~ubuntu26.04.1_amd64.manifest'
PATH="$fixture_root/bin:$PATH" bash "$contract" --manifest "$baseline/$manifest_name" >/dev/null

lexical_order="$fixture_root/lexical-order"
cp -a "$baseline" "$lexical_order"
printf '%s\n' \
    'facelock_0.1.4-1~ubuntu26.04.1.dsc' \
    'facelock_0.1.4.orig-cargo-vendor.tar.xz' \
    'facelock_0.1.4.orig-onnxruntime.tar.gz' \
    'facelock_0.1.4.orig.tar.gz' \
    'facelock_0.1.4-1~ubuntu26.04.1.debian.tar.xz' \
    'facelock_0.1.4-1~ubuntu26.04.1_amd64.buildinfo' \
    'facelock_0.1.4-1~ubuntu26.04.1_amd64.deb' \
    'facelock_0.1.4-1~ubuntu26.04.1_amd64.changes' \
    >"$lexical_order/$manifest_name"
assert_rejected "accepted the former .changes/lexical artifact order" \
    env PATH="$fixture_root/bin:$PATH" bash "$contract" \
    --manifest "$lexical_order/$manifest_name"

canonical_stage="$fixture_root/canonical-stage"
PATH="$fixture_root/bin:$PATH" bash "$contract" \
    --manifest "$baseline/$manifest_name" --stage "$canonical_stage" >/dev/null
cmp -s "$baseline/$manifest_name" "$canonical_stage/$manifest_name" ||
    fail "atomic stage changed the canonical artifact order"
for artifact in "$canonical_stage"/*; do
    [ -f "$artifact" ] || fail "atomic stage published a non-file entry"
done

conditional_enable="$fixture_root/conditional-enable"
cp -a "$baseline" "$conditional_enable"
cat >"$conditional_enable/facelock_0.1.4-1~ubuntu26.04.1_amd64.deb.postinst" <<'SH'
#!/bin/sh
if [ "$1" = configure ]; then
    if deb-systemd-helper debian-installed 'facelock-daemon.service'; then
        if deb-systemd-helper --quiet was-enabled 'facelock-daemon.service'; then
            deb-systemd-helper enable 'facelock-daemon.service' >/dev/null || true
        fi
    fi
fi
SH
PATH="$fixture_root/bin:$PATH" \
    bash "$contract" --manifest "$conditional_enable/$manifest_name" >/dev/null

unconditional_enable="$fixture_root/unconditional-enable"
cp -a "$baseline" "$unconditional_enable"
cat >"$unconditional_enable/facelock_0.1.4-1~ubuntu26.04.1_amd64.deb.postinst" <<'SH'
#!/bin/sh
deb-systemd-helper enable 'facelock-daemon.service' >/dev/null || true
SH
assert_rejected "accepted unconditional service enablement" \
    env PATH="$fixture_root/bin:$PATH" bash "$contract" \
    --manifest "$unconditional_enable/$manifest_name"

automatic_start="$fixture_root/automatic-start"
cp -a "$baseline" "$automatic_start"
cat >"$automatic_start/facelock_0.1.4-1~ubuntu26.04.1_amd64.deb.postinst" <<'SH'
#!/bin/sh
deb-systemd-invoke start 'facelock-daemon.service' >/dev/null || true
SH
assert_rejected "accepted automatic service startup" \
    env PATH="$fixture_root/bin:$PATH" bash "$contract" \
    --manifest "$automatic_start/$manifest_name"

extra="$fixture_root/extra"
cp -a "$baseline" "$extra"
printf '%s\n' extra >"$extra/unexpected.txt"
printf '%s\n' unexpected.txt >>"$extra/$manifest_name"
assert_rejected "accepted a manifest with a ninth artifact" \
    env PATH="$fixture_root/bin:$PATH" bash "$contract" --manifest "$extra/$manifest_name"

wrong_suite="$fixture_root/wrong-suite"
cp -a "$baseline" "$wrong_suite"
sed -i 's/^Distribution: resolute$/Distribution: trixie/' "$wrong_suite"/*.changes
assert_rejected "accepted .changes metadata for the wrong suite" \
    env PATH="$fixture_root/bin:$PATH" bash "$contract" --manifest "$wrong_suite/$manifest_name"

bad_checksum="$fixture_root/bad-checksum"
cp -a "$baseline" "$bad_checksum"
printf '%s\n' corrupt >>"$bad_checksum"/*.dsc
assert_rejected "accepted an artifact whose .changes checksum no longer matches" \
    env PATH="$fixture_root/bin:$PATH" bash "$contract" --manifest "$bad_checksum/$manifest_name"

wrong_version="$fixture_root/wrong-version"
cp -a "$baseline" "$wrong_version"
printf '%s\n' '9.9.9-1~ubuntu26.04.1' >"$wrong_version/facelock_0.1.4-1~ubuntu26.04.1_amd64.deb.version"
assert_rejected "accepted a binary package with the wrong version" \
    env PATH="$fixture_root/bin:$PATH" bash "$contract" --manifest "$wrong_version/$manifest_name"

wrong_architecture="$fixture_root/wrong-architecture"
cp -a "$baseline" "$wrong_architecture"
printf '%s\n' arm64 >"$wrong_architecture/facelock_0.1.4-1~ubuntu26.04.1_amd64.deb.architecture"
assert_rejected "accepted a binary package with the wrong architecture" \
    env PATH="$fixture_root/bin:$PATH" bash "$contract" --manifest "$wrong_architecture/$manifest_name"

missing_tpm_dependency="$fixture_root/missing-tpm-dependency"
cp -a "$baseline" "$missing_tpm_dependency"
printf '%s\n' 'dbus, libpam-runtime, libc6 (>= 2.36)' \
    >"$missing_tpm_dependency/facelock_0.1.4-1~ubuntu26.04.1_amd64.deb.depends"
assert_rejected "accepted a binary package without a generated TPM dependency" \
    env PATH="$fixture_root/bin:$PATH" bash "$contract" \
    --manifest "$missing_tpm_dependency/$manifest_name"

for relation in provides conflicts replaces; do
    related="$fixture_root/nonempty-$relation"
    cp -a "$baseline" "$related"
    printf '%s\n' facelock-legacy \
        >"$related/facelock_0.1.4-1~ubuntu26.04.1_amd64.deb.$relation"
    assert_rejected "accepted a binary package with a nonempty $relation field" \
        env PATH="$fixture_root/bin:$PATH" bash "$contract" \
        --manifest "$related/$manifest_name"
done

preexisting_stage="$fixture_root/preexisting-stage"
mkdir "$preexisting_stage"
assert_rejected "accepted a pre-existing stage destination" \
    env PATH="$fixture_root/bin:$PATH" bash "$contract" \
    --manifest "$baseline/$manifest_name" --stage "$preexisting_stage"
[ -z "$(find "$preexisting_stage" -mindepth 1 -print -quit)" ] ||
    fail "wrote partial artifacts into a rejected pre-existing destination"

copy_failure_bin="$fixture_root/copy-failure-bin"
mkdir "$copy_failure_bin"
cat >"$copy_failure_bin/cp" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
count=0
if [ -f "$FACELOCK_COPY_COUNT" ]; then
    count="$(sed -n '1p' "$FACELOCK_COPY_COUNT")"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$FACELOCK_COPY_COUNT"
if [ "$count" -eq 2 ]; then
    exit 73
fi
exec /usr/bin/cp "$@"
SH
chmod +x "$copy_failure_bin/cp"
partial_stage="$fixture_root/partial-stage"
assert_rejected "reported success after an injected staging copy failure" \
    env FACELOCK_COPY_COUNT="$fixture_root/copy-count" \
    PATH="$copy_failure_bin:$fixture_root/bin:$PATH" \
    bash "$contract" --manifest "$baseline/$manifest_name" --stage "$partial_stage"
[ ! -e "$partial_stage" ] || fail "published partial artifacts after a staging copy failure"
if find "$fixture_root" -maxdepth 1 -name '.partial-stage.tmp.*' -print -quit | grep -q .; then
    fail "left a partial temporary staging directory after copy failure"
fi

atomic_source="$fixture_root/atomic-source"
atomic_destination="$fixture_root/atomic-destination"
mkdir "$atomic_source" "$atomic_destination"
printf '%s\n' complete >"$atomic_source/artifact"
printf '%s\n' collision >"$atomic_destination/collision-marker"
assert_rejected "atomic publisher replaced a colliding destination" \
    python3 "$publisher" "$atomic_source" "$atomic_destination"
[ -f "$atomic_destination/collision-marker" ] || fail "collision marker was replaced"
[ ! -e "$atomic_destination/artifact" ] || fail "published into a colliding destination"
[ -d "$atomic_source" ] || fail "atomic publisher consumed source after collision"

echo "deb package contract test: ok"
