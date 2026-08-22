#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
control_path="${FACELOCK_DEBIAN_CONTROL:-debian/control}"
workflow_path="${FACELOCK_RELEASE_WORKFLOW:-.github/workflows/release.yml}"

fail() {
    echo "deb source contract: $*" >&2
    exit 1
}

required_metadata=(
    debian/changelog
    debian/control
    debian/copyright
    debian/pam-auth-update
    debian/postinst
    debian/postrm
    debian/prerm
    debian/rules
    debian/source/format
)

for path in "${required_metadata[@]}"; do
    [ -f "$path" ] || fail "missing canonical metadata: $path"
done

grep -Eq '^Default:[[:space:]]*no[[:space:]]*$' debian/pam-auth-update ||
    fail "Debian pam-auth-update profile must be opt-in"
grep -Fq 'pam-auth-update --package' debian/postinst ||
    fail "Debian postinst must register/update the opt-in PAM profile"
if grep -Eq 'systemd-tmpfiles[[:space:]].*--create' debian/postinst; then
    fail "Debian source postinst must leave package-scoped tmpfiles activation to dh_installtmpfiles"
fi
[ "$(grep -Foc '#DEBHELPER#' debian/postinst)" -eq 1 ] ||
    fail "Debian postinst must contain exactly one debhelper substitution marker"
grep -Fq 'systemctl is-active --quiet facelock-daemon.service' debian/postinst ||
    fail "Debian postinst must distinguish an already-active daemon"
grep -Fq 'systemctl try-restart facelock-daemon.service' debian/postinst ||
    fail "Debian postinst must restart only an already-active daemon"
grep -Fq 'facelock pam shared-profile-status' debian/prerm ||
    fail "Debian prerm must refuse removal while the shared profile is selected"
if grep -Fq 'pam-auth-update --remove facelock' debian/prerm; then
    fail "Debian prerm must not silently disable an administrator-selected shared profile"
fi
grep -Fq 'facelock pam remove --all --dry-run' debian/prerm ||
    fail "Debian prerm must preflight direct PAM cleanup before any mutation"
grep -Fq 'facelock pam remove --all' debian/prerm ||
    fail "Debian prerm must delegate direct-edit cleanup to pam remove --all"
if grep -Eq 'FACELOCK_PAM_SERVICES=|sed -i .pam_facelock' debian/prerm; then
    fail "Debian prerm must not carry a fixed PAM service list or raw sed cleanup"
fi
profile_probe_line="$(grep -n -m1 -F 'facelock pam shared-profile-status' debian/prerm | cut -d: -f1)"
cleanup_preflight_line="$(grep -n -m1 -F 'facelock pam remove --all --dry-run' debian/prerm | cut -d: -f1)"
cleanup_line="$(grep -n -F 'facelock pam remove --all' debian/prerm | tail -n1 | cut -d: -f1)"
debhelper_line="$(grep -n -m1 -Fx '#DEBHELPER#' debian/prerm | cut -d: -f1)"
if grep -Eq 'systemctl[[:space:]]+(stop|disable)[[:space:]]+facelock-daemon' debian/prerm; then
    fail "Debian source prerm must delegate service stop and purge state to debhelper"
fi
[ "$(grep -Foc '#DEBHELPER#' debian/prerm)" -eq 1 ] ||
    fail "Debian prerm must contain exactly one debhelper substitution marker"
[ "$profile_probe_line" -lt "$cleanup_preflight_line" ] &&
    [ "$cleanup_preflight_line" -lt "$cleanup_line" ] &&
    [ "$cleanup_line" -lt "$debhelper_line" ] ||
    fail "Debian prerm must probe, preflight and clean PAM before generated service lifecycle handling"

grep -Fq 'dh_installsystemd --no-enable --no-start' debian/rules ||
    fail "Debian build must keep fresh installs disabled and inactive"
grep -Fq "deb-systemd-invoke stop 'facelock-daemon.service'" debian/rules ||
    fail "Debian build must probe for the exact generated daemon stop"
grep -Fq '/usr/share/debhelper/autoscripts/prerm-systemd-restart' debian/rules ||
    fail "Debian build must reuse debhelper's canonical remove-stop template"
grep -Fq 'debian/.debhelper/generated/facelock/prerm.*' debian/rules ||
    fail "Debian build must detect modern generated prerm fragments before adding a compatibility stop"
grep -Fq "s/#UNITFILES#/'facelock-daemon.service'/" debian/rules ||
    fail "Debian compatibility stop must remain scoped to the exact daemon unit"

[ ! -e dist/debian ] || fail "retired dist/debian metadata still exists"
[ ! -e debian/compat ] || fail "debhelper compat must be declared once, in Build-Depends"
if rg -n 'dist/debian' justfile scripts test .github .claude crates docs/security.md docs/releasing.md \
    --glob '!test/deb-source-contract.sh' >/dev/null; then
    fail "active packaging consumer still references retired dist/debian metadata"
fi

compat_count="$(grep -Eic 'debhelper-compat[[:space:]]*\(=[[:space:]]*13\)' "$control_path")"
[ "$compat_count" -eq 1 ] || fail "expected exactly one debhelper-compat (= 13) declaration"
grep -Eq '^Rules-Requires-Root:[[:space:]]*no[[:space:]]*$' "$control_path" ||
    fail "debian/control must declare Rules-Requires-Root: no"
[ "$(tr -d '\r\n' < debian/source/format)" = "3.0 (quilt)" ] ||
    fail "debian/source/format must be 3.0 (quilt)"

build_depends="$(awk '
    /^Build-Depends:[[:space:]]*/ {
        collecting=1
        value=$0
        sub(/^Build-Depends:[[:space:]]*/, "", value)
        next
    }
    collecting && /^[[:space:]]/ { value=value " " $0; next }
    collecting { collecting=0; print value; exit }
    END { if (collecting) print value }
' "$control_path")"
[ "$(printf '%s\n' "$build_depends" | sed '/^[[:space:]]*$/d' | wc -l)" -eq 1 ] ||
    fail "Build-Depends parser must emit exactly one dependency record"
printf '%s\n' "$build_depends" | grep -Eq '(^|,[[:space:]]*)pkg-config([[:space:]]*\([^)]*\))?([[:space:]]*,|$)' ||
    fail "debian/control Build-Depends must include pkg-config for the default Wayland source build"
for exact_dependency in 'cargo (>= 1.88)' 'rustc (>= 1.88)' 'libtss2-dev (>= 4.1.3)' python3; do
    count="$(printf '%s\n' "$build_depends" | tr ',' '\n' | sed -E 's/^[[:space:]]*//; s/[[:space:]]*$//' | grep -Fxc "$exact_dependency" || true)"
    [ "$count" -eq 1 ] || fail "debian/control must declare exactly one $exact_dependency"
done
grep -Fqx 'rust-version = "1.88"' Cargo.toml ||
    fail "workspace rust-version must match the packaged Rust 1.88 floor"

debian_builders=(test/Containerfile.deb-assemble)
for builder_path in "${debian_builders[@]}"; do
    [ -f "$builder_path" ] || fail "missing canonical Debian builder: $builder_path"
    if rg -n 'dtolnay/rust-toolchain|sh\.rustup\.rs|\$HOME/\.cargo/bin|(^|[[:space:]/])rustup([[:space:]]|$)' "$builder_path" >/dev/null; then
        fail "$builder_path must use only distro-packaged Rust and Cargo"
    fi
done
workflow_deb_job="$(awk '
    /^  build-deb:[[:space:]]*$/ { in_job=1 }
    in_job && NR > 1 && /^  [[:alnum:]_-]+:[[:space:]]*$/ && $1 != "build-deb:" { exit }
    in_job { print }
' "$workflow_path")"
[ -n "$workflow_deb_job" ] || fail "release workflow is missing the build-deb job"
if printf '%s\n' "$workflow_deb_job" | rg -n 'dtolnay/rust-toolchain|sh\.rustup\.rs|\$HOME/\.cargo/bin|(^|[[:space:]/])rustup([[:space:]]|$)' >/dev/null; then
    fail "release workflow build-deb job must use only distro-packaged Rust and Cargo"
fi
workflow_deb_steps="$(printf '%s\n' "$workflow_deb_job" | awk '
    /^[[:space:]]+steps:[[:space:]]*$/ { in_steps=1; next }
    in_steps { print }
')"
workflow_bootstrap_line="$(printf '%s\n' "$workflow_deb_steps" | grep -n -m1 -- '- name: Bootstrap checkout dependencies' | cut -d: -f1 || true)"
workflow_checkout_line="$(printf '%s\n' "$workflow_deb_steps" | grep -n -m1 -- '- uses: actions/checkout@' | cut -d: -f1 || true)"
[ -n "$workflow_bootstrap_line" ] || fail "release workflow must bootstrap checkout dependencies inside the stock Debian containers"
[ -n "$workflow_checkout_line" ] || fail "release workflow build-deb job must check out the tagged source"
[ "$workflow_bootstrap_line" -lt "$workflow_checkout_line" ] ||
    fail "release workflow must install Git before actions/checkout in the stock Debian containers"
workflow_bootstrap_block="$(printf '%s\n' "$workflow_deb_steps" | awk '
    /^[[:space:]]*- name: Bootstrap checkout dependencies[[:space:]]*$/ { in_step=1; next }
    in_step && /^[[:space:]]*- (name:|uses:)/ { exit }
    in_step { print }
')"
for checkout_dependency in git ca-certificates; do
    printf '%s\n' "$workflow_bootstrap_block" |
        grep -Eq "^[[:space:]]*${checkout_dependency}([[:space:]]*\\\\)?[[:space:]]*$" ||
        fail "release workflow checkout bootstrap must install $checkout_dependency"
done
grep -Fq 'trixie-backports' test/Containerfile.deb-assemble ||
    fail "Trixie builder must provision cargo and rustc from trixie-backports"
for provenance_probe in \
    "dpkg-query -W -f='\${Package} \${Version}\\n' cargo rustc" \
    'cargo --version' \
    'rustc --version' \
    'dpkg-checkbuilddeps'; do
    grep -Fq "$provenance_probe" test/Containerfile.deb-assemble ||
        fail "canonical Debian builder must record/prove: $provenance_probe"
done

workflow_install_block="$(awk '
    /^  build-deb:[[:space:]]*$/ { in_job=1; next }
    in_job && /^  [[:alnum:]_-]+:[[:space:]]*$/ { exit }
    in_job && /^[[:space:]]*- name: Install system dependencies[[:space:]]*$/ { in_step=1; next }
    in_step && /^[[:space:]]*- name:/ { exit }
    in_step { print }
' "$workflow_path")"
[ -n "$workflow_install_block" ] || fail "release workflow is missing its Debian system dependency step"
while IFS= read -r dependency; do
    [ -n "$dependency" ] || continue
    case "$dependency" in
        debhelper-compat) apt_package=debhelper ;;
        *) apt_package="$dependency" ;;
    esac
    printf '%s\n' "$workflow_install_block" |
        grep -Eq "^[[:space:]]*${apt_package}([[:space:]]*\\\\)?[[:space:]]*$" ||
        fail "release workflow dependency install must include canonical Build-Depends package: $apt_package"
done < <(
    printf '%s\n' "$build_depends" |
        tr ',' '\n' |
        sed -E 's/^[[:space:]]*//; s/[[:space:]]*\([^)]*\)//g; s/[[:space:]]*$//'
)
grep -Eq '^[[:space:]]*dpkg-checkbuilddeps[[:space:]]*$' "$workflow_path" ||
    fail "release workflow must prove canonical Build-Depends with dpkg-checkbuilddeps"
for containerfile in "${debian_builders[@]}"; do
    grep -Eq '^[[:space:]]*dpkg-checkbuilddeps([[:space:]]|&&|\\|$)' "$containerfile" ||
        fail "$containerfile must prove canonical Build-Depends with dpkg-checkbuilddeps"
done

for containerfile in "${debian_builders[@]}"; do
    if grep -Fq 'COPY target/release/' "$containerfile"; then
        fail "$containerfile must build from the complete tagged source, not host binaries"
    fi
    if grep -Fq '"0.0.0"' "$containerfile"; then
        fail "$containerfile must derive the exact version/tag from the candidate source"
    fi
    grep -Fq 'COPY . /build/' "$containerfile" ||
        fail "$containerfile must copy the complete candidate source tree"
done

for helper in \
    .github/workflows/scripts/run-networkless.sh \
    scripts/prepare-cargo-vendor.sh \
    scripts/prepare-ort-bundle.sh \
    test/prepare-deb-test-context.sh \
    test/run-deb-offline-build.sh \
    test/build-deb-package-image.sh \
    test/verify-deb-test-context.sh \
    test/deb-maintscript-contract.sh \
    test/cargo-vendor-contract.sh \
    test/deb-package-contract-test.sh \
    test/publish-directory-atomic.py; do
    [ -x "$helper" ] || fail "$helper must be an executable package-gate helper"
done

for containerfile in \
    test/Containerfile.deb-assemble \
    test/Containerfile.deb-rebuild \
    test/Containerfile.deb-runtime; do
    [ -f "$containerfile" ] || fail "missing split Debian package fixture: $containerfile"
done

grep -Fq 'path-include=/usr/share/doc/facelock/**' test/Containerfile.deb-runtime ||
    fail "Debian runtime fixture must preserve Facelock legal/provenance documents"
grep -Fq '> /etc/dpkg/dpkg.cfg.d/zz-facelock-test-docs' test/Containerfile.deb-runtime ||
    fail "Debian runtime fixture's Facelock include must sort after base-image exclusions"
grep -Fq '/facelock-common-auth-install-invariant' test/Containerfile.deb-runtime ||
    fail "Debian runtime fixture must prove fresh install leaves common-auth unchanged"
grep -Fq 'PAM module executes through the synthetic service' test/pkg-validate.sh ||
    fail "package validation must prove pam_facelock executes, not accept a generic PAM failure"
grep -Fq 'missing PAM module control is rejected' test/pkg-validate.sh ||
    fail "package validation must prove its PAM execution assertion rejects a missing module"
grep -Fq 'packaged opt-in PAM profile survives reinstall, falls back to password, and restores common-auth' test/pkg-validate.sh ||
    fail "Debian package validation must exercise the shipped pam-auth-update profile"
grep -Fq 'active administrator-selected profile blocks removal, preserves PAM, and allows verified migration retry' test/pkg-validate.sh ||
    fail "Debian package validation must preserve selected shared profiles across removal refusal"
active_profile_guard_line="$(grep -n -m1 -F '"active administrator-selected profile blocks removal, preserves PAM, and allows verified migration retry"' test/pkg-validate.sh | cut -d: -f1)"
active_profile_guard_invocation_line="$(grep -n -m1 -Fx \
    '        "verify_debian_active_profile_removal_guard"' test/pkg-validate.sh |
    cut -d: -f1 || true)"
[ -n "$active_profile_guard_invocation_line" ] &&
    [ "$active_profile_guard_invocation_line" -eq "$((active_profile_guard_line + 1))" ] ||
    fail "active-profile removal PASS label must invoke the checked guard directly"
# Match the runner's literal variable spelling, not this contract's value.
# shellcheck disable=SC2016
blocker_create_line="$(grep -n -m1 -F 'cat > "$PACKAGE_BLOCKER_PAM"' test/pkg-validate.sh | cut -d: -f1)"
[ "$active_profile_guard_invocation_line" -lt "$blocker_create_line" ] ||
    fail "active-profile removal validation must run before creating the unmanaged blocker"
removal_guard_body="$(sed -n '/^verify_debian_active_profile_removal_guard()/,/^}/p' test/pkg-validate.sh)"
printf '%s\n' "$removal_guard_body" | grep -Fq 'facelock pam shared-profile-status' ||
    fail "active-profile removal validation must assert the profile-specific status"
printf '%s\n' "$removal_guard_body" | grep -Fq 'refusing package removal because the pam-auth-update profile is active' ||
    fail "active-profile removal validation must assert the profile-specific diagnostic"
printf '%s\n' "$removal_guard_body" | grep -Fq 'common-auth-removal.active.metadata' ||
    fail "active-profile removal validation must preserve common-auth metadata"
printf '%s\n' "$removal_guard_body" | grep -Fq 'pam-state-removal.active.metadata' ||
    fail "active-profile removal validation must preserve pam-auth-update state metadata"
disable_profile_line="$(printf '%s\n' "$removal_guard_body" |
    grep -n -m1 -F 'pam-auth-update --disable facelock --force' | cut -d: -f1)"
verify_password_line="$(printf '%s\n' "$removal_guard_body" |
    grep -n -m1 -F 'pamtester facelock-profile-removal-test testuser authenticate' | cut -d: -f1)"
[ "$disable_profile_line" -lt "$verify_password_line" ] ||
    fail "Debian removal validation must test passwords after disabling the shared profile"
grep -Fq 'Debian reinstall restarts only active daemons and preserves enabled state' test/pkg-validate.sh ||
    fail "Debian package validation must exercise active/inactive and enabled/disabled reinstall state"
grep -Fq 'ordinary Debian remove preserves enabled state across reinstall' test/pkg-validate.sh ||
    fail "Debian package validation must prove ordinary removal preserves service enablement"
grep -Fq 'pam-auth-update --enable facelock --force' test/pkg-validate.sh ||
    fail "Debian package validation must enable the packaged profile through pam-auth-update"
grep -Fq 'pam-auth-update --disable facelock --force' test/pkg-validate.sh ||
    fail "Debian package validation must disable the packaged profile through pam-auth-update"

# Match literal source text in the runner, not this contract's working tree.
# shellcheck disable=SC2016
if grep -Fq '$PWD/models:/var/lib/facelock/models' test/run-pkg-validate-systemd.sh; then
    fail "package runner must never mount checkout models at the mutable runtime path"
fi
# shellcheck disable=SC2016
grep -Fq '$PWD/models:/facelock-test-models:ro' test/run-pkg-validate-systemd.sh ||
    fail "package runner must mount checkout models read-only outside runtime storage"
grep -Fq '/var/lib/facelock/models' test/run-pkg-validate-systemd.sh ||
    fail "package runner must copy models into disposable runtime storage after boot"

grep -Fq 'package assembly' .github/workflows/scripts/run-networkless.sh ||
    fail "network sandbox diagnostics must describe shared package assembly"
if grep -Fq 'networkless RPM' .github/workflows/scripts/run-networkless.sh; then
    fail "network sandbox diagnostics must not claim RPM-only scope"
fi
grep -Fq 'FACELOCK_NETWORKLESS_ACTIVE=1' .github/workflows/scripts/run-networkless.sh ||
    fail "network sandbox must export its active marker"

grep -Fq 'FACELOCK_NETWORKLESS_ACTIVE' .github/workflows/scripts/build-deb.sh ||
    fail "canonical Debian builder must require the networkless package boundary"
grep -Fq 'run-networkless.sh' .github/workflows/scripts/build-deb.sh ||
    fail "canonical Debian builder must enter the shared network sandbox"
for cache_home in CARGO_HOME RUSTUP_HOME; do
    grep -Fq "$cache_home" test/run-deb-offline-build.sh ||
        fail "offline Debian runner must isolate $cache_home"
done
grep -Fq 'FACELOCK_NETWORKLESS_ACTIVE' test/run-deb-offline-build.sh ||
    fail "offline Debian runner must verify the active sandbox marker"
grep -Fq -- '--network=none' test/build-deb-package-image.sh ||
    fail "Debian package gate must also disable the container network"
grep -Fq 'rebuild-dsc' test/build-deb-package-image.sh ||
    fail "Debian package gate must rebuild the emitted dsc in a clean image"
grep -Fq 'Containerfile.deb-rebuild' test/build-deb-package-image.sh ||
    fail "Debian package gate must use a source-rebuild-only image"
grep -Fq 'Containerfile.deb-runtime' test/build-deb-package-image.sh ||
    fail "Debian package gate must install the release package in a separate runtime image"
# The assembler already validates the exact manifest with Debian tooling. The
# host orchestrator must remain portable and must not require dpkg-deb.
# shellcheck disable=SC2016
if grep -Fq 'bash "$repo_root/test/deb-package-contract.sh"' test/build-deb-package-image.sh; then
    fail "Debian package gate must not duplicate dpkg-deb validation on the host"
fi

workflow_offline_block="$(awk '
    /^  build-deb:[[:space:]]*$/ { in_job=1 }
    in_job && /^  [[:alnum:]_-]+:[[:space:]]*$/ && $1 != "build-deb:" { exit }
    in_job { print }
' "$workflow_path")"
for required in \
    '/tmp/facelock-empty-cargo-home' \
    '/tmp/facelock-empty-rustup-home' \
    'CARGO_HOME=' \
    'RUSTUP_HOME=' \
    '.github/workflows/scripts/run-networkless.sh'; do
    grep -Fq "$required" <<<"$workflow_offline_block" ||
        fail "release workflow Debian build is missing offline boundary: $required"
done

cargo_vendor_config=debian/cargo-config.toml
[ -f "$cargo_vendor_config" ] || fail "missing package-only Cargo source configuration"
grep -Fqx 'replace-with = "facelock-vendored-sources"' "$cargo_vendor_config" ||
    fail "Debian Cargo config must replace crates.io with the reviewed vendor bundle"
grep -Fqx 'directory = "cargo-vendor/vendor"' "$cargo_vendor_config" ||
    fail "Debian Cargo config must use the cargo-vendor source component"
grep -Fqx 'offline = true' "$cargo_vendor_config" ||
    fail "Debian Cargo config must force offline dependency resolution"
[ ! -e .cargo/config.toml ] ||
    fail "package-only source replacement must not leak into the developer root"

grep -Fq 'prepare-cargo-vendor.sh prepare cargo-vendor' test/Containerfile.deb-assemble ||
    fail "Debian source assembler must prepare the exact Cargo vendor component"
grep -Fq 'prepare-cargo-vendor.sh verify cargo-vendor' test/Containerfile.deb-assemble ||
    fail "Debian source assembler must verify the exact Cargo vendor component"
grep -Fq 'prepare-cargo-vendor:' .github/workflows/release.yml ||
    fail "release workflow must prepare the Cargo vendor component separately"
grep -Fq 'name: cargo-vendor-bundle.tar.xz' .github/workflows/release.yml ||
    fail "release workflow must download the Cargo vendor archive by its actual filename"
grep -Fq 'scripts/prepare-cargo-vendor.sh verify cargo-vendor' .github/workflows/release.yml ||
    fail "release workflow must verify the downloaded Cargo vendor component"

ort_helper=scripts/prepare-ort-bundle.sh
for pin in \
    '1.20.1' \
    '67db4dc1561f1e3fd42e619575c82c601ef89849afc7ea85a003abbac1a1a105' \
    'a5faaf78a37590d3fe640f887620e74f6022d34550172b91ad2131bf0ad77d64' \
    '5c1b7ccbff7e5141c1da7a9d963d660e5741c319'; do
    grep -Fq "$pin" "$ort_helper" || fail "ORT bundle helper is missing reviewed pin: $pin"
done
for bundle_path in \
    GIT_COMMIT_ID \
    LICENSE \
    PROVENANCE.md \
    ThirdPartyNotices.txt \
    VERSION_NUMBER \
    lib/libonnxruntime.so \
    manifest.json \
    SHA256SUMS; do
    grep -Fq "$bundle_path" "$ort_helper" ||
        fail "ORT bundle helper is missing exact bundle member: $bundle_path"
done
# Match literal helper variables, not this contract's shell variables.
# shellcheck disable=SC2016
grep -Fq 'require_mode "$bundle/lib/libonnxruntime.so" 755' "$ort_helper" ||
    fail "ORT bundle verification must retain the executable library mode identity"
# shellcheck disable=SC2016
grep -Fq 'require_mode "$bundle/$entry" 644' "$ort_helper" ||
    fail "ORT bundle verification must retain the non-executable metadata mode identity"

grep -Fq 'prepare-ort-bundle.sh source-url' .github/workflows/release.yml ||
    fail "release workflow must obtain the pinned ORT URL from the shared helper"
grep -Fq 'prepare-ort-bundle.sh prepare' .github/workflows/release.yml ||
    fail "release workflow must prepare ORT through the shared helper"
grep -Fq 'prepare-ort-bundle.sh verify' .github/workflows/release.yml ||
    fail "release workflow must verify downloaded ORT through the shared helper"
ort_producer_job="$(awk '
    /^  download-ort:[[:space:]]*$/ { in_job=1 }
    in_job && /^  [[:alnum:]_-]+:[[:space:]]*$/ && $1 != "download-ort:" { exit }
    in_job { print }
' "$workflow_path")"
for producer_contract in \
    'scripts/prepare-ort-bundle.sh component-tar onnxruntime onnxruntime-bundle.tar.xz' \
    'path: onnxruntime-bundle.tar.xz' \
    'archive: false'; do
    printf '%s\n' "$ort_producer_job" | grep -Fq "$producer_contract" ||
        fail "ORT producer is missing direct archive contract: $producer_contract"
done
if printf '%s\n' "$ort_producer_job" | grep -Eq 'path:[[:space:]]+onnxruntime/?[[:space:]]*$'; then
    fail "release workflow must not transfer the raw ORT directory"
fi
for consumer_job_name in build-deb build-rpm; do
    consumer_job="$(awk -v expected="$consumer_job_name:" '
        $1 == expected { in_job=1 }
        in_job && /^  [[:alnum:]_-]+:[[:space:]]*$/ && $1 != expected { exit }
        in_job { print }
    ' "$workflow_path")"
    for consumer_contract in \
        'name: onnxruntime-bundle.tar.xz' \
        'skip-decompress: true' \
        'scripts/prepare-ort-bundle.sh extract-component onnxruntime-bundle.tar.xz onnxruntime' \
        'scripts/prepare-ort-bundle.sh verify onnxruntime'; do
        printf '%s\n' "$consumer_job" | grep -Fq "$consumer_contract" ||
            fail "$consumer_job_name is missing direct ORT archive contract: $consumer_contract"
    done
done

(
    unsafe_root="$(mktemp -d "${TMPDIR:-/tmp}/facelock-ort-archive-test.XXXXXX")"
    trap 'rm -rf -- "$unsafe_root"' EXIT
    mkdir -p "$unsafe_root/payload"
    printf 'escape\n' >"$unsafe_root/payload/file"
    tar -C "$unsafe_root" --transform='s#^payload#../facelock-ort-archive-escape#' \
        -cJf "$unsafe_root/traversal.tar.xz" payload
    if "$ort_helper" extract-component "$unsafe_root/traversal.tar.xz" \
        "$unsafe_root/onnxruntime" >/dev/null 2>&1; then
        fail "ORT extraction accepted a path-traversing component archive"
    fi
    [ ! -e "$(dirname "$unsafe_root")/facelock-ort-archive-escape" ] ||
        fail "unsafe ORT extraction wrote outside its destination"
)
for containerfile in "${debian_builders[@]}"; do
    if grep -Eq '^ADD[[:space:]]+https://github.com/microsoft/onnxruntime' "$containerfile"; then
        fail "$containerfile must not use an unchecked remote ADD for ORT"
    fi
    grep -Fq 'scripts/prepare-ort-bundle.sh source-url' "$containerfile" ||
        fail "$containerfile must obtain the pinned ORT URL from the shared helper"
    grep -Fq 'scripts/prepare-ort-bundle.sh prepare' "$containerfile" ||
        fail "$containerfile must prepare and verify the exact ORT bundle"
done

builder=.github/workflows/scripts/build-deb.sh
# Match literal runtime variable references in the builder source.
# shellcheck disable=SC2016
grep -Fq '"$ORT_HELPER" verify "$REPO_ROOT/onnxruntime"' "$builder" ||
    fail "Debian builder must require and verify the exact ORT bundle"
# shellcheck disable=SC2016
if grep -Fq 'if [ -d "$REPO_ROOT/onnxruntime" ]' "$builder"; then
    fail "Debian builder must not make the ORT source component optional"
fi
grep -Fq 'scripts/prepare-ort-bundle.sh verify onnxruntime' debian/rules ||
    fail "debian/rules must verify the exact ORT bundle before building"
grep -Fq 'scripts/prepare-cargo-vendor.sh verify cargo-vendor' debian/rules ||
    fail "debian/rules must verify the exact Cargo vendor component before building"
grep -Eq '^override_dh_clean:[[:space:]]*$' debian/rules ||
    fail "debian/rules must protect the verified Cargo vendor component during dh_clean"
grep -Eq 'dh_clean[[:space:]]+-Xcargo-vendor([[:space:]]|$)' debian/rules ||
    fail "dh_clean must exclude only the Cargo vendor source component"
grep -Eq 'cargo[[:space:]]+--config[[:space:]]+debian/cargo-config.toml[[:space:]]+build.*--locked.*--offline.*--features[[:space:]]+tpm' debian/rules ||
    fail "debian/rules must perform a locked offline TPM build through package-only Cargo configuration"
# Match literal Make syntax; command substitution is intentionally not wanted.
# shellcheck disable=SC2016
grep -Fqx 'export RUSTFLAGS := --remap-path-prefix=$(CURDIR)=/usr/src/facelock' debian/rules ||
    fail "debian/rules must remap its disposable source path for reproducible Rust binaries"
if grep -Fq 'if [ -f onnxruntime/lib/libonnxruntime.so ]' debian/rules; then
    fail "debian/rules must not make the bundled ORT runtime optional"
fi
for installed_path in \
    usr/lib/facelock/libonnxruntime.so \
    usr/share/doc/facelock/onnxruntime/GIT_COMMIT_ID \
    usr/share/doc/facelock/onnxruntime/LICENSE \
    usr/share/doc/facelock/onnxruntime/PROVENANCE.md \
    usr/share/doc/facelock/onnxruntime/ThirdPartyNotices.txt \
    usr/share/doc/facelock/onnxruntime/VERSION_NUMBER \
    usr/share/doc/facelock/onnxruntime/manifest.json \
    usr/share/doc/facelock/onnxruntime/SHA256SUMS; do
    grep -Fq "$installed_path" debian/rules ||
        fail "debian/rules does not install required ORT payload: $installed_path"
    grep -Fq "$installed_path" .github/workflows/scripts/validate-deb.sh ||
        fail "Debian archive validation does not require ORT payload: $installed_path"
done
grep -Fq 'ORT_LIBRARY_FILE="/usr/lib/facelock/libonnxruntime.so"' test/pkg-validate.sh ||
    fail "installed package validation does not pin the ORT library path"
grep -Fq 'ORT_DOCUMENT_ROOT="/usr/share/doc/facelock/onnxruntime"' test/pkg-validate.sh ||
    fail "installed package validation does not pin the ORT metadata root"
grep -Fq "dpkg-query -W -f='\${db:Status-Abbrev}' facelock" test/pkg-validate.sh ||
    fail "installed package validation must identify a genuinely installed dpkg facelock"
if grep -Fq 'RPM bundled ONNX Runtime and exact legal/provenance set are hash-verified' test/pkg-validate.sh; then
    fail "shared installed-package validation must not apply a Debian document layout to RPM"
fi
for rpm_contract in \
    '/facelock/libonnxruntime\.so\.1\.20\.1$' \
    '/onnxruntime-manifest\.json$' \
    '/onnxruntime-SHA256SUMS$' \
    'a5faaf78a37590d3fe640f887620e74f6022d34550172b91ad2131bf0ad77d64'; do
    grep -Fq "$rpm_contract" .github/workflows/scripts/validate-rpm.sh ||
        fail "RPM format-specific ORT validation is missing: $rpm_contract"
done
python3 - test/pkg-validate.sh <<'PY'
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
format_branch = text.find('case "$PACKAGE_FORMAT" in')
if format_branch < 0:
    raise SystemExit("deb source contract: installed legal/ORT checks must branch on the installed package format")
prefix = text[:format_branch]
for label in (
    'run_test "Debian copyright exists"',
    'run_test "Debian bundled ONNX Runtime and exact legal/provenance set are hash-verified"',
):
    if label in prefix:
        raise SystemExit(f"deb source contract: format-specific package assertion is unconditional: {label}")
PY
for metadata_name in GIT_COMMIT_ID LICENSE PROVENANCE.md SHA256SUMS \
    ThirdPartyNotices.txt VERSION_NUMBER manifest.json; do
    grep -Fq "$metadata_name" test/pkg-validate.sh ||
        fail "installed package validation does not require ORT metadata: $metadata_name"
done
grep -Fq 'Files: onnxruntime/*' debian/copyright ||
    fail "debian/copyright must identify the independently sourced ORT component"
grep -Fq 'Copyright: Microsoft Corporation' debian/copyright ||
    fail "debian/copyright must retain the ORT upstream copyright holder"
grep -Eq '^override_dh_installsystemd:[[:space:]]*$' debian/rules ||
    fail "debian/rules must override dh_installsystemd for explicit activation"
grep -Eq 'dh_installsystemd([[:space:]]+--no-enable[[:space:]]+--no-start|[[:space:]]+--no-start[[:space:]]+--no-enable)([[:space:]]|$)' \
    debian/rules ||
    fail "debian/rules must keep facelock-daemon disabled and inactive after install"
grep -Fq 'prepare-deb-test-context.sh' test/build-deb-package-image.sh ||
    fail "Debian image builder must prepare an exact tagged candidate context"
# Match the literal transport command in the helper source.
# shellcheck disable=SC2016
grep -Fq 'tar -C "$context" -cf "$context/facelock-git-metadata.tar" .git' \
    test/build-deb-package-image.sh ||
    fail "Debian image builder must transport the isolated context Git metadata explicitly"
for containerfile in "${debian_builders[@]}"; do
    grep -Fq 'tar -xf /build/facelock-git-metadata.tar -C /build' "$containerfile" ||
        fail "$containerfile must restore the exact tagged Git metadata before source packaging"
    grep -Fq 'git config --global --add safe.directory /build' "$containerfile" ||
        fail "$containerfile must trust only the restored disposable Git worktree"
done
if grep -Eq 'podman build .*Containerfile\.deb-assemble' justfile; then
    fail "just Debian package gates must use the exact tagged context helper"
fi
for recipe in test-deb-trixie-pkg test-deb-resolute-pkg test-deb-dev-shell test-deb-release-shell; do
    recipe_header="$(grep -E "^${recipe}:" justfile)"
    if grep -Fq 'build-release' <<<"$recipe_header"; then
        fail "$recipe must not consume host-built release binaries"
    fi
done
test_deb_header="$(grep -E '^test-deb:' justfile)"
[ "$test_deb_header" = 'test-deb: test-deb-trixie-pkg test-deb-resolute-pkg' ] ||
    fail "just test-deb must delegate to both exact supported-suite package gates"
[ ! -e test/Containerfile.ubuntu ] ||
    fail "obsolete Ubuntu 24.04 host-binary package fixture must remain retired"
if rg -n 'Containerfile\.ubuntu' justfile test .github .claude docs README.md book \
    --glob '!test/deb-source-contract.sh' >/dev/null; then
    fail "active packaging consumer still references the retired Ubuntu host-binary fixture"
fi

grep -Fq -- '--manifest' test/deb-package-contract.sh ||
    fail "binary package contract must accept the exact generated manifest"
# Match a retired literal builder expression.
# shellcheck disable=SC2016
if grep -Fq 'sed "s#^#$OUTPUT_DIR/#"' .github/workflows/scripts/build-deb.sh; then
    fail "generated manifest must contain portable exact basenames, not build-host paths"
fi
grep -Fq 'bash test/deb-source-contract.sh' .github/workflows/release.yml ||
    fail "release workflow must run the Debian source contract before packaging"
grep -Fq 'bash test/deb-package-contract.sh --manifest' .github/workflows/release.yml ||
    fail "release workflow must validate the exact generated package manifest"
# Match the literal workflow staging expression.
# shellcheck disable=SC2016
grep -Fq -- '--stage "$PWD/debian-upload"' .github/workflows/release.yml ||
    fail "release workflow must stage uploads only from the exact generated manifest"
grep -Eq '^test-deb-source-contract:' justfile ||
    fail "justfile must expose the Debian source contract"
grep -Eq '^test-deb-package-contract[[:space:]]+manifest:' justfile ||
    fail "justfile must expose manifest-driven Debian package validation"
grep -Eq '^test-deb-package-contract-test:' justfile ||
    fail "justfile must expose exact manifest contract mutation tests"
grep -Eq '^check:.*test-deb-package-contract-test([[:space:]]|$)' justfile ||
    fail "just check must run exact manifest contract mutation tests"

if rg -n -F 'facelock_*.deb' \
    .github/workflows/release.yml justfile \
    test/Containerfile.deb-assemble >/dev/null; then
    fail "active Debian gates must not discover packages through broad facelock_*.deb globs"
fi

# Every binary stanza must let debhelper resolve ELF and helper dependencies,
# while retaining the two runtime services Facelock needs explicitly.
# The case diagnostics name literal debhelper substitution tokens.
# shellcheck disable=SC2016
awk '
    function finish() {
        if (!in_binary) return
        if (depends !~ /\$\{shlibs:Depends\}/) exit 21
        if (depends !~ /\$\{misc:Depends\}/) exit 22
        if (depends !~ /(^|[,[:space:]])dbus([,[:space:]]|$)/) exit 23
        if (depends !~ /(^|[,[:space:]])libpam-runtime([,[:space:]]|$)/) exit 24
    }
    /^Package:[[:space:]]*/ { finish(); in_binary=1; depends=""; in_depends=0; next }
    /^[^[:space:]]/ {
        if (in_binary && in_depends) in_depends=0
    }
    /^Depends:[[:space:]]*/ {
        if (in_binary) {
            in_depends=1
            depends=$0
            sub(/^Depends:[[:space:]]*/, "", depends)
        }
        next
    }
    /^[[:space:]]/ {
        if (in_binary && in_depends) depends=depends " " $0
        next
    }
    END { finish() }
' "$control_path" || case "$?" in
    21) fail 'binary stanza missing ${shlibs:Depends}' ;;
    22) fail 'binary stanza missing ${misc:Depends}' ;;
    23) fail 'binary stanza missing explicit dbus dependency' ;;
    24) fail 'binary stanza missing explicit libpam-runtime dependency' ;;
    *) fail "could not parse debian/control" ;;
esac

builder=.github/workflows/scripts/build-deb.sh
[ -x "$builder" ] || fail "$builder must be executable"
# Match the builder's literal variable-based artifact identities.
# shellcheck disable=SC2016
expected_builder_artifact_order='"facelock_${DEBIAN_UPSTREAM}.orig.tar.gz"
"facelock_${DEBIAN_UPSTREAM}.orig-onnxruntime.tar.gz"
"facelock_${DEBIAN_UPSTREAM}.orig-cargo-vendor.tar.xz"
"facelock_${PACKAGE_VERSION}.debian.tar.xz"
"facelock_${PACKAGE_VERSION}.dsc"
"facelock_${PACKAGE_VERSION}_${ARCHITECTURE}.buildinfo"
"facelock_${PACKAGE_VERSION}_${ARCHITECTURE}.deb"
"facelock_${PACKAGE_VERSION}_${ARCHITECTURE}.changes"'
builder_artifact_order="$(awk '
    /^MANIFEST_ARTIFACTS=\([[:space:]]*$/ { in_order=1; next }
    in_order && /^\)[[:space:]]*$/ { exit }
    in_order {
        sub(/^[[:space:]]*/, "")
        print
    }
' "$builder")"
[ "$builder_artifact_order" = "$expected_builder_artifact_order" ] ||
    fail "builder must emit the approved main/ORT/Cargo/delta/dsc/buildinfo/deb/changes order"
grep -Eq 'git([[:space:]]+-C[[:space:]]+[^[:space:]]+)?[[:space:]]+archive' "$builder" ||
    fail "builder must create the upstream tar from Git"
grep -Fq '^{commit}' "$builder" || fail "builder must resolve the release tag to a commit"
grep -Fq 'gzip -n' "$builder" || fail "orig tar gzip header must be deterministic"
grep -Fq 'dpkg-source -b' "$builder" || fail "builder must explicitly build a source package"
# Match literal builder variable references.
# shellcheck disable=SC2016
if grep -Fq -- '-C "$REPO_ROOT/onnxruntime"' "$builder"; then
    fail "ONNX component archive must retain its top-level directory for dpkg-source extraction"
fi
# shellcheck disable=SC2016
grep -Fq -- '-C "$REPO_ROOT" -cf - onnxruntime' "$builder" ||
    fail "ONNX component archive must unpack as onnxruntime/lib inside the source tree"
# Match literal builder variable references.
# shellcheck disable=SC2016
grep -Fq '"$CARGO_VENDOR_HELPER" verify "$REPO_ROOT/cargo-vendor"' "$builder" ||
    fail "Debian builder must require and verify the exact Cargo vendor bundle"
grep -Fq 'orig-cargo-vendor.tar.xz' "$builder" ||
    fail "Debian builder must emit the Cargo vendor bundle as a quilt source component"
# Match literal builder variable references.
# shellcheck disable=SC2016
grep -Fq '"$CARGO_VENDOR_HELPER" component-tar' "$builder" ||
    fail "Debian builder must use the shared helper for the Cargo vendor component archive"
grep -Fq 'dpkg-buildpackage -us -uc' "$builder" ||
    fail "builder must run a full unsigned dpkg-buildpackage"
grep -Eq 'dpkg-buildpackage[[:space:]].*-sa([[:space:]]|$)' "$builder" ||
    fail "builder must include the exact orig archive in its source/binary manifest"
if grep -Fq 'dpkg-deb --build' "$builder"; then
    fail "manual dpkg-deb assembly is forbidden"
fi
if grep -Eq 'DEBIAN/(control|conffiles)' "$builder"; then
    fail "builder must not synthesize binary control metadata"
fi

if [ -z "${FACELOCK_DEBIAN_CONTROL:-}" ] && [ -z "${FACELOCK_RELEASE_WORKFLOW:-}" ]; then
    mutation_root="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-control-mutation.XXXXXX")"
    trap 'rm -rf -- "$mutation_root"' EXIT
    sed -E 's/(^|,)[[:space:]]*pkg-config([[:space:]]*\([^)]*\))?([[:space:]]*,|$)/\1\3/' \
        debian/control >"$mutation_root/control"
    if cmp -s debian/control "$mutation_root/control"; then
        fail "pkg-config mutation fixture did not change debian/control"
    fi
    if FACELOCK_DEBIAN_CONTROL="$mutation_root/control" bash "$0" >"$mutation_root/output" 2>&1; then
        fail "source contract accepted removal of pkg-config from Build-Depends"
    fi
    grep -Fq 'Build-Depends must include pkg-config' "$mutation_root/output" ||
        fail "pkg-config mutation failed for an unrelated reason"

    for apt_package in cargo rustc libtss2-dev python3; do
        mutated_workflow="$mutation_root/release-without-$apt_package.yml"
        awk -v dependency="$apt_package" '
            {
                normalized=$0
                sub(/^[[:space:]]*/, "", normalized)
                sub(/[[:space:]]*\\[[:space:]]*$/, "", normalized)
                sub(/[[:space:]]*$/, "", normalized)
                if (normalized != dependency) print
            }
        ' .github/workflows/release.yml >"$mutated_workflow"
        if cmp -s .github/workflows/release.yml "$mutated_workflow"; then
            fail "$apt_package release-workflow mutation did not change the dependency step"
        fi
        if FACELOCK_RELEASE_WORKFLOW="$mutated_workflow" bash "$0" >"$mutation_root/output" 2>&1; then
            fail "source contract accepted removal of $apt_package from release workflow provisioning"
        fi
        grep -Fq "dependency install must include canonical Build-Depends package: $apt_package" \
            "$mutation_root/output" ||
            fail "$apt_package release-workflow mutation failed for an unrelated reason"
    done

    context_fixture="$mutation_root/exact-context"
    mkdir "$context_fixture"
    test/prepare-deb-test-context.sh "$context_fixture" >/dev/null
    test/verify-deb-test-context.sh "$repo_root" "$context_fixture" >/dev/null ||
        fail "prepared Debian test context does not match every candidate path, mode, and blob"
fi

echo "deb source contract: ok"
