#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$repo_root/scripts/release-versions.sh"

if declare -F release_debian_variant >/dev/null; then
    echo "FAIL: release_debian_variant remains in the two-suite single-package contract" >&2
    exit 1
fi

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

cleanup() {
    rm -f "$packit_fixture"
    rm -f "${packit_complex_fixture:-}" "${packit_commented_fixture:-}"
    rm -f "${github_output_fixture:-}"
    if [ -n "${tmp_root:-}" ] && [ -d "$tmp_root" ]; then
        rm -rf "$tmp_root"
    fi
}

assert_eq() {
    local expected="$1"
    local actual="$2"
    local context="$3"
    if [ "$actual" != "$expected" ]; then
        fail "$context: expected '$expected', got '$actual'"
    fi
}

assert_rejected() {
    local kind="$1"
    shift
    if "$kind" "$@" >/dev/null 2>&1; then
        fail "$kind accepted malformed inputs: $*"
    fi
}

assert_file_line() {
    local file="$1"
    local expected="$2"
    if ! grep -Fqx "$expected" "$file"; then
        fail "$file does not contain exact line: $expected"
    fi
}

assert_eq "0.2.0-alpha.1" "$(release_cargo_from_tag v0.2.0-alpha.1)" "tag to Cargo"
assert_eq "v0.2.0-alpha.1" "$(release_tag_from_cargo 0.2.0-alpha.1)" "Cargo to tag"
assert_eq "true" "$(release_github_prerelease 0.2.0-alpha.1)" "GitHub alpha classification"
assert_eq "false" "$(release_github_prerelease 0.2.0)" "GitHub stable classification"
assert_eq "0.2.0~alpha.1" "$(release_debian_upstream 0.2.0-alpha.1)" "Debian upstream"
assert_eq "0.2.0alpha1" "$(release_arch_pkgver 0.2.0-alpha.1)" "Arch pkgver"
assert_eq "0.2.0" "$(release_rpm_version 0.2.0-alpha.1)" "RPM Version"
assert_eq "0.1.alpha.1" "$(release_rpm_release 0.2.0-alpha.1 1)" "RPM prerelease Release"
assert_eq "1" "$(release_rpm_release 0.2.0 99)" "RPM stable Release"

release_validate_transition 0.1.4 0.2.0-alpha.1
release_validate_transition 0.2.0-alpha.1 0.2.0-alpha.1
release_validate_transition 0.2.0-alpha.1 0.2.0-alpha.2
release_validate_transition 0.2.0-alpha.2 0.2.0-beta.1
release_validate_transition 0.2.0-beta.1 0.2.0-rc.1
release_validate_transition 0.2.0-rc.1 0.2.0
assert_rejected release_validate_transition 0.2.0-alpha.2 0.2.0-alpha.1
assert_rejected release_validate_transition 0.2.0-beta.1 0.2.0-alpha.3
assert_rejected release_validate_transition 0.2.0-rc.1 0.2.0-beta.2
assert_rejected release_validate_transition 0.2.0 0.2.0-rc.2
assert_rejected release_validate_transition 0.2.0 0.2.0

assert_eq "0.2.0~alpha.1-1~deb13u1" "$(release_debian_version 0.2.0-alpha.1 1 trixie)" "Debian 13 revision"
assert_eq "0.2.0~alpha.1-1~ubuntu26.04.1" "$(release_debian_version 0.2.0-alpha.1 1 resolute)" "Ubuntu 26.04 revision"
assert_eq "~deb13u1" "$(release_debian_suite_suffix trixie)" "Debian 13 suite suffix"
assert_eq "~ubuntu26.04.1" "$(release_debian_suite_suffix resolute)" "Ubuntu 26.04 suite suffix"
assert_rejected release_debian_suite_suffix bookworm
assert_rejected release_debian_suite_suffix noble
assert_rejected release_debian_version 0.2.0 1 bookworm
assert_rejected release_debian_version 0.2.0 1 noble
assert_eq "facelock_0.2.0~alpha.1-1~deb13u1_amd64" "$(release_debian_binary_basename 0.2.0-alpha.1 1 trixie amd64)" "Debian binary basename"
assert_eq "facelock_0.2.0~alpha.1-1~deb13u1" "$(release_debian_source_basename 0.2.0-alpha.1 1 trixie)" "Debian source basename"

assert_rejected release_validate_cargo_version 0.2.0-alpha1
assert_rejected release_validate_cargo_version 0.2.0-preview.1
assert_rejected release_validate_cargo_version 0.2
assert_rejected release_cargo_from_tag 0.2.0-alpha.1
assert_rejected release_cargo_from_tag v0.2.0-alpha.1-extra
assert_rejected release_github_prerelease invalid
assert_rejected release_debian_common_version invalid 1
assert_rejected release_debian_version 0.2.0 1 sid
assert_rejected release_debian_source_basename invalid 1 trixie
assert_rejected release_debian_binary_basename 0.2.0 1 sid amd64
assert_rejected release_arch_version invalid 1
assert_rejected release_rpm_evr invalid 1

github_output_fixture="$(mktemp)"
assert_rejected release_write_github_outputs v9.9.9-alpha.1 "$github_output_fixture"
if [ -s "$github_output_fixture" ]; then
    fail "release_write_github_outputs left partial output after rejecting inconsistent metadata"
fi

packit_fixture="$(mktemp)"
trap cleanup EXIT
cat > "$packit_fixture" <<'JSON'
{
  "jobs": [
    {
      "job": "copr_build",
      "trigger": "ignore",
      "owner": "tysmith",
      "project": "facelock",
      "targets": ["fedora-44-x86_64"]
    }
  ]
}
JSON
release_validate_packit_channel 0.2.0-alpha.1 "$packit_fixture"
if release_validate_packit_channel 0.2.0 "$packit_fixture" >/dev/null 2>&1; then
    fail "stable preflight accepted a config without deliberate production COPR restoration"
fi
sed -i 's/"trigger": "ignore"/"trigger": "release"/' "$packit_fixture"
if release_validate_packit_channel 0.2.0-alpha.1 "$packit_fixture" >/dev/null 2>&1; then
    fail "prerelease preflight accepted a release-triggered production COPR job"
fi
release_validate_packit_channel 0.2.0 "$packit_fixture"

packit_complex_fixture="$(mktemp)"
cat > "$packit_complex_fixture" <<'JSON'
{
  "jobs": [
    {
      "targets": ["fedora-44-x86_64"],
      "project": "facelock-testing",
      "trigger": "pull_request",
      "job": "copr_build",
      "owner": "tyvsmith"
    },
    {
      "project": "facelock",
      "targets": ["fedora-43-x86_64", "fedora-44-x86_64", "fedora-45-x86_64"],
      "owner": "tyvsmith",
      "trigger": "release",
      "job": "copr_build"
    }
  ]
}
JSON
if release_validate_packit_channel 0.2.0-alpha.1 "$packit_complex_fixture" >/dev/null 2>&1; then
    fail "prerelease preflight accepted a reordered production job after another Packit job"
fi
release_validate_packit_channel 0.2.0 "$packit_complex_fixture"

packit_commented_fixture="$(mktemp)"
cat > "$packit_commented_fixture" <<'YAML'
specfile_path: dist/facelock.spec
upstream_package_name: facelock
downstream_package_name: facelock
upstream_tag_template: "v{version}"
jobs:
  # General YAML is valid to Packit but outside the guard's JSON-subset contract.
  - targets:
      - fedora-43-x86_64
      - fedora-44-x86_64
      - fedora-45-x86_64
    owner: "tyvsmith"
    project: "facelock"
    trigger: "release"
    job: "copr_build"
YAML
if release_validate_packit_channel 0.2.0-alpha.1 "$packit_commented_fixture" >/dev/null 2>&1; then
    fail "prerelease preflight accepted a commented config outside the JSON-subset Packit contract"
fi

debian_versions=(
    0.1.4-1
    "$(release_debian_common_version 0.2.0-alpha.1 1)"
    "$(release_debian_common_version 0.2.0-alpha.1 2)"
    "$(release_debian_common_version 0.2.0-alpha.2 1)"
    "$(release_debian_common_version 0.2.0-beta.1 1)"
    "$(release_debian_common_version 0.2.0-rc.1 1)"
    "$(release_debian_common_version 0.2.0 1)"
)
rpm_versions=(
    0.1.4-1
    "$(release_rpm_evr 0.2.0-alpha.1 1)"
    "$(release_rpm_evr 0.2.0-alpha.1 2)"
    "$(release_rpm_evr 0.2.0-alpha.2 3)"
    "$(release_rpm_evr 0.2.0-beta.1 4)"
    "$(release_rpm_evr 0.2.0-rc.1 5)"
    "$(release_rpm_evr 0.2.0 1)"
)
arch_versions=(
    0.1.4-1
    "$(release_arch_version 0.2.0-alpha.1 1)"
    "$(release_arch_version 0.2.0-alpha.1 2)"
    "$(release_arch_version 0.2.0-alpha.2 1)"
    "$(release_arch_version 0.2.0-beta.1 1)"
    "$(release_arch_version 0.2.0-rc.1 1)"
    "$(release_arch_version 0.2.0 1)"
)

assert_eq "0.1.4-1 0.2.0~alpha.1-1 0.2.0~alpha.1-2 0.2.0~alpha.2-1 0.2.0~beta.1-1 0.2.0~rc.1-1 0.2.0-1" "${debian_versions[*]}" "exact Debian order identities"
assert_eq "0.1.4-1 0.2.0-0.1.alpha.1 0.2.0-0.2.alpha.1 0.2.0-0.3.alpha.2 0.2.0-0.4.beta.1 0.2.0-0.5.rc.1 0.2.0-1" "${rpm_versions[*]}" "exact RPM order identities"
assert_eq "0.1.4-1 0.2.0alpha1-1 0.2.0alpha1-2 0.2.0alpha2-1 0.2.0beta1-1 0.2.0rc1-1 0.2.0-1" "${arch_versions[*]}" "exact Arch order identities"

assert_file_line \
    "$repo_root/docs/integrating.md" \
    "for required in is-enrolled pam-if-present pam-json pam-multi-service pam-status setup-no-pam setup-systemd; do"

tmp_root="$(mktemp -d)"
export XDG_RUNTIME_DIR="$tmp_root/runtime"
release_repo="$tmp_root/release-repo"
matrix_root="$tmp_root/matrix-root"
mkdir -p "$release_repo/debian" "$release_repo/dist" "$release_repo/scripts" "$tmp_root/bin" "$XDG_RUNTIME_DIR"
mkdir -p "$matrix_root/.claude/skills/packaging-test" "$matrix_root/.claude/skills/release" "$matrix_root/.github/ISSUE_TEMPLATE" "$matrix_root/.github/workflows/scripts" "$matrix_root/book/src" "$matrix_root/crates/facelock-cli/src/commands" "$matrix_root/dist/apt/conf" "$matrix_root/debian" "$matrix_root/dist/nix" "$matrix_root/docs/adr" "$matrix_root/test" "$matrix_root/website"
cp "$repo_root/.claude/skills/packaging-test/SKILL.md" "$matrix_root/.claude/skills/packaging-test/"

rpm_query_failure_output=
if rpm_query_failure_output=$(
    FACELOCK_TEST_RPM="$tmp_root/missing.rpm" \
        bash "$repo_root/test/rpm-authselect-contract.sh" 2>&1
); then
    fail "RPM authselect contract accepted an unreadable artifact"
fi
case "$rpm_query_failure_output" in
    *"cannot query RPM payload inventory"*) ;;
    *) fail "RPM authselect contract omitted the explicit payload query failure: $rpm_query_failure_output" ;;
esac

rpm_query_bin="$tmp_root/rpm-query-bin"
mkdir -p "$rpm_query_bin"
cat > "$rpm_query_bin/rpm" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = "-qpl" ]; then
    printf '%s\n' /usr/bin/facelock
    exit 0
fi
if [ "${1:-}" = "-qp" ] && [ "${2:-}" = "--requires" ]; then
    exit 42
fi
exit 43
SH
chmod +x "$rpm_query_bin/rpm"
rpm_dependency_failure_output=
if rpm_dependency_failure_output=$(
    PATH="$rpm_query_bin:$PATH" FACELOCK_TEST_RPM="$tmp_root/queryable.rpm" \
        bash "$repo_root/test/rpm-authselect-artifact-contract.sh" 2>&1
); then
    fail "RPM artifact contract accepted a failed dependency query"
fi
case "$rpm_dependency_failure_output" in
    *"cannot query RPM dependency inventory"*) ;;
    *) fail "RPM artifact contract omitted the explicit dependency query failure: $rpm_dependency_failure_output" ;;
esac
cp "$repo_root/.claude/skills/release/SKILL.md" "$matrix_root/.claude/skills/release/"
cp "$repo_root/.packit.yaml" "$matrix_root/"
cp "$repo_root/justfile" "$matrix_root/"
cp "$repo_root/.github/workflows/ci.yml" "$matrix_root/.github/workflows/"
cp "$repo_root/.github/workflows/release.yml" "$matrix_root/.github/workflows/"
cp "$repo_root/.github/workflows/scripts/build-deb.sh" "$matrix_root/.github/workflows/scripts/"
cp "$repo_root/.github/workflows/scripts/publish-apt.sh" "$matrix_root/.github/workflows/scripts/"
cp "$repo_root/.github/workflows/scripts/publish-aur.sh" "$matrix_root/.github/workflows/scripts/"
cp "$repo_root/.github/workflows/scripts/validate-rpm.sh" "$matrix_root/.github/workflows/scripts/"
cp "$repo_root/dist/PKGBUILD" "$repo_root/dist/PKGBUILD-bin" "$repo_root/dist/PKGBUILD-git" "$repo_root/dist/facelock.spec" "$repo_root/dist/release-matrix.json" "$matrix_root/dist/"
cp "$repo_root/dist/apt/conf/distributions" "$matrix_root/dist/apt/conf/"
cp "$repo_root/debian/rules" "$matrix_root/debian/"
cp "$repo_root/dist/nix/default.nix" "$matrix_root/dist/nix/"
cp "$repo_root/docs/releasing.md" "$repo_root/docs/contracts.md" "$repo_root/docs/integrating.md" "$repo_root/docs/security.md" "$repo_root/docs/compatibility.md" "$repo_root/docs/quickstart.md" "$matrix_root/docs/"
cp "$repo_root/docs/adr/009-cli-verb-noun-shape.md" "$matrix_root/docs/adr/"
cp "$repo_root/docs/testing-roadmap.md" "$matrix_root/docs/"
cp "$repo_root/README.md" "$repo_root/CONTRIBUTING.md" "$matrix_root/"
cp "$repo_root/book/src/quickstart.md" "$repo_root/book/src/contributing.md" "$repo_root/book/src/compatibility.md" "$matrix_root/book/src/"
cp "$repo_root/.github/ISSUE_TEMPLATE/bug_report.md" "$matrix_root/.github/ISSUE_TEMPLATE/"
cp "$repo_root/crates/facelock-cli/src/commands/pam.rs" "$matrix_root/crates/facelock-cli/src/commands/"
cp "$repo_root/crates/facelock-cli/src/commands/setup.rs" "$matrix_root/crates/facelock-cli/src/commands/"
cp "$repo_root/website/index.html" "$matrix_root/website/"
cp "$repo_root/test/check-release-matrix.py" "$matrix_root/test/"
cp "$repo_root/test/Containerfile" "$matrix_root/test/"
cp "$repo_root/test/Containerfile.rpm-e2e" "$matrix_root/test/"
cp "$repo_root/test/copr-build.sh" "$matrix_root/test/"
cp "$repo_root/test/run-pkg-validate-systemd.sh" "$matrix_root/test/"

apt_publisher_root="$tmp_root/apt-publisher-root"
mkdir -p "$apt_publisher_root/.github/workflows/scripts" "$apt_publisher_root/scripts" "$apt_publisher_root/debs"
cp "$repo_root/.github/workflows/scripts/publish-apt.sh" "$apt_publisher_root/.github/workflows/scripts/"
cp "$repo_root/scripts/release-versions.sh" "$apt_publisher_root/scripts/"
sed -i "s/resolute) printf '~ubuntu26.04.1/resolute) printf '~ubuntu26.04.99/" "$apt_publisher_root/scripts/release-versions.sh"
for suite in trixie resolute; do
    : > "$apt_publisher_root/debs/$suite.deb"
done
cat > "$tmp_root/bin/dpkg-deb" <<'SH'
#!/usr/bin/env bash
case "$2" in
    */trixie.deb) printf '%s\n' '0.2.0-1~deb13u1' ;;
    */resolute.deb) printf '%s\n' '0.2.0-1~ubuntu26.04.1' ;;
    *) exit 1 ;;
esac
SH
chmod +x "$tmp_root/bin/dpkg-deb"
if apt_guard_output=$(
    env -u APT_GPG_PRIVATE_KEY -u APT_GPG_PASSPHRASE PATH="$tmp_root/bin:$PATH" \
        bash "$apt_publisher_root/.github/workflows/scripts/publish-apt.sh" \
        "$apt_publisher_root/repo" \
        "trixie=$apt_publisher_root/debs/trixie.deb" \
        "resolute=$apt_publisher_root/debs/resolute.deb" 2>&1
); then
    fail "APT publisher accepted resolute suffix drift in the central release contract"
fi
case "$apt_guard_output" in
    *"does not match stable APT suite resolute (~ubuntu26.04.99)"*) ;;
    *) fail "APT publisher did not consume the mutated central resolute suffix: $apt_guard_output" ;;
esac

sed -i 's/"trigger": "ignore"/"trigger": "release"/' "$matrix_root/.packit.yaml"
env -u RELEASE_MATRIX_VERSION python3 "$matrix_root/test/check-release-matrix.py"
if RELEASE_MATRIX_VERSION=0.2.0-alpha.1 python3 "$matrix_root/test/check-release-matrix.py" >/dev/null 2>&1; then
    fail "release matrix checker accepted a production COPR release job for a prerelease identity"
fi
RELEASE_MATRIX_VERSION=0.2.0 python3 "$matrix_root/test/check-release-matrix.py"

matrix_mutation_index=0
assert_matrix_mutation_rejected() {
    local context="$1"
    local relative_file="$2"
    local expression="$3"
    local mutation_root="$tmp_root/matrix-mutation-$matrix_mutation_index"
    matrix_mutation_index=$((matrix_mutation_index + 1))
    cp -R "$matrix_root" "$mutation_root"
    sed -i "$expression" "$mutation_root/$relative_file"
    if cmp -s "$matrix_root/$relative_file" "$mutation_root/$relative_file"; then
        fail "matrix mutation fixture did not change $relative_file: $context"
    fi
    if RELEASE_MATRIX_VERSION=0.2.0 python3 "$mutation_root/test/check-release-matrix.py" >/dev/null 2>&1; then
        fail "release matrix checker accepted drift: $context"
    fi
    echo "release matrix mutation case: $context rejected"
}

assert_matrix_payload_mutation_rejected_with_diagnostic() {
    local context="$1"
    local relative_file="$2"
    local payload="$3"
    local expected_diagnostic="$4"
    local mutation_root="$tmp_root/matrix-mutation-$matrix_mutation_index"
    local checker_output
    local marker
    local prefix
    matrix_mutation_index=$((matrix_mutation_index + 1))
    cp -R "$matrix_root" "$mutation_root"
    case "$relative_file" in
        dist/PKGBUILD|dist/PKGBUILD-git)
            marker="    # Licenses"
            prefix="    "
            ;;
        dist/PKGBUILD-bin)
            marker="    install -Dm644 LICENSE-MIT"
            prefix="    "
            ;;
        dist/facelock.spec)
            marker="# The direct release RPM"
            prefix=""
            ;;
        debian/rules)
            marker=$'\t# Required reviewed CPU ONNX Runtime component and its legal/provenance set'
            prefix=$'\t'
            ;;
        .github/workflows/scripts/build-deb.sh)
            marker="# ONNX Runtime is independently sourced"
            prefix=""
            ;;
        dist/nix/default.nix)
            marker="    # Bundled ONNX Runtime for non-NixOS use"
            prefix="    "
            ;;
        *) fail "no package payload mutation marker for $relative_file" ;;
    esac
    python3 - "$mutation_root/$relative_file" "$marker" "$prefix" "$payload" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
marker, prefix, payload = sys.argv[2:]
content = path.read_text()
if content.count(marker) != 1:
    raise SystemExit(f"payload mutation marker is not unique in {path}: {marker!r}")
path.write_text(content.replace(marker, f"{prefix}{payload}\n{marker}", 1))
PY
    if checker_output=$(RELEASE_MATRIX_VERSION=0.2.0 python3 "$mutation_root/test/check-release-matrix.py" 2>&1); then
        fail "release matrix checker accepted drift: $context"
    fi
    assert_eq "$expected_diagnostic" "$checker_output" "$context diagnostic"
}

for required_capability in \
    is-enrolled \
    pam-if-present \
    pam-json \
    pam-multi-service \
    pam-status \
    setup-no-pam \
    setup-systemd; do
    mutation_root="$tmp_root/matrix-mutation-$matrix_mutation_index"
    matrix_mutation_index=$((matrix_mutation_index + 1))
    cp -R "$matrix_root" "$mutation_root"
    sed -i "/^for required in /s/$required_capability//" "$mutation_root/docs/integrating.md"
    if checker_output=$(RELEASE_MATRIX_VERSION=0.2.0 python3 "$mutation_root/test/check-release-matrix.py" 2>&1); then
        fail "release matrix checker accepted a capability gate without $required_capability"
    fi
    assert_eq \
        "FAIL: docs/integrating.md capability gate does not match invoked commands: missing $required_capability; extra none" \
        "$checker_output" \
        "missing $required_capability capability diagnostic"
done

package_assemblers=(
    dist/PKGBUILD
    dist/PKGBUILD-bin
    dist/PKGBUILD-git
    dist/facelock.spec
    debian/rules
    .github/workflows/scripts/build-deb.sh
    dist/nix/default.nix
)
for package_assembler in "${package_assemblers[@]}"; do
    assert_matrix_payload_mutation_rejected_with_diagnostic \
        "split Omarchy assignment in $package_assembler" \
        "$package_assembler" \
        'retired_dir=o"mar"chy' \
        "FAIL: $package_assembler still contains retired downstream-integration component omarchy"
    assert_matrix_payload_mutation_rejected_with_diagnostic \
        "split setup helper assignment in $package_assembler" \
        "$package_assembler" \
        'retired_helper=setup-"security"-face' \
        "FAIL: $package_assembler still contains retired downstream-integration component setup-security-face"
    assert_matrix_payload_mutation_rejected_with_diagnostic \
        "split removal helper assignment in $package_assembler" \
        "$package_assembler" \
        "retired_helper=remove-'security'-face" \
        "FAIL: $package_assembler still contains retired downstream-integration component remove-security-face"
    # Literal mutation payload; expansion belongs to the generated fixture.
    # shellcheck disable=SC2016
    assert_matrix_payload_mutation_rejected_with_diagnostic \
        "split generic helper install in $package_assembler" \
        "$package_assembler" \
        'install -Dm755 "$source_root/security"-face "$dest_root"' \
        "FAIL: $package_assembler still contains retired downstream-integration component security-face"
done

current_integration_docs=(
    README.md
    docs/contracts.md
    docs/integrating.md
    docs/adr/009-cli-verb-noun-shape.md
)
retired_helper_references=(
    dist/omarchy/
    omarchy-setup-security-face
    omarchy-remove-security-face
)
for integration_doc in "${current_integration_docs[@]}"; do
    for retired_reference in "${retired_helper_references[@]}"; do
        mutation_root="$tmp_root/matrix-mutation-$matrix_mutation_index"
        matrix_mutation_index=$((matrix_mutation_index + 1))
        cp -R "$matrix_root" "$mutation_root"
        printf '\n%s\n' "$retired_reference" >> "$mutation_root/$integration_doc"
        if checker_output=$(RELEASE_MATRIX_VERSION=0.2.0 python3 "$mutation_root/test/check-release-matrix.py" 2>&1); then
            fail "release matrix checker accepted retired helper reference $retired_reference in $integration_doc"
        fi
        assert_eq \
            "FAIL: $integration_doc still presents retired downstream-integration helper $retired_reference" \
            "$checker_output" \
            "retired helper reference $retired_reference in $integration_doc diagnostic"
    done
done

append_packit_job() {
    local config_path="$1"
    local job_json="$2"
    python3 - "$config_path" "$job_json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
config = json.loads(path.read_text())
config["jobs"].append(json.loads(sys.argv[2]))
path.write_text(json.dumps(config, indent=2) + "\n")
PY
}

packit_extra_job_index=0
assert_extra_packit_job_rejected() {
    local context="$1"
    local job_json="$2"
    local mutation_root="$tmp_root/packit-extra-job-$packit_extra_job_index"
    local checker_output
    packit_extra_job_index=$((packit_extra_job_index + 1))
    cp -R "$matrix_root" "$mutation_root"
    append_packit_job "$mutation_root/.packit.yaml" "$job_json"
    if checker_output=$(RELEASE_MATRIX_VERSION=0.2.0 python3 "$mutation_root/test/check-release-matrix.py" 2>&1); then
        printf '%s\n' "$checker_output"
        fail "release matrix checker accepted an extra Packit job: $context"
    fi
    echo "release matrix Packit case: $context rejected"
}

valid_packit_staging_root="$tmp_root/packit-valid-staging"
cp -R "$matrix_root" "$valid_packit_staging_root"
append_packit_job \
    "$valid_packit_staging_root/.packit.yaml" \
    '{"job":"copr_build","trigger":"pull_request","owner":"tyvsmith","project":"facelock-testing","targets":["fedora-43-x86_64","fedora-44-x86_64","fedora-45-x86_64"]}'
RELEASE_MATRIX_VERSION=0.2.0 python3 "$valid_packit_staging_root/test/check-release-matrix.py" >/dev/null
echo "release matrix Packit case: exact facelock-testing staging targets accepted"

assert_extra_packit_job_rejected \
    "canonical Rawhide target outside production and staging" \
    '{"job":"copr_build","trigger":"release","owner":"other-owner","project":"facelock-scratch","targets":["fedora-rawhide"]}'
assert_extra_packit_job_rejected \
    "fedora-all mutable alias" \
    '{"job":"copr_build","trigger":"release","owner":"other-owner","project":"facelock-scratch","targets":["fedora-all"]}'
assert_extra_packit_job_rejected \
    "fedora-development mutable alias" \
    '{"job":"copr_build","trigger":"release","owner":"other-owner","project":"facelock-scratch","targets":["fedora-development"]}'
assert_extra_packit_job_rejected \
    "architecture-suffixed mutable alias" \
    '{"job":"copr_build","trigger":"release","owner":"other-owner","project":"facelock-scratch","targets":["fedora-development-aarch64"]}'
assert_extra_packit_job_rejected \
    "facelock-testing Rawhide target" \
    '{"job":"copr_build","trigger":"pull_request","owner":"tyvsmith","project":"facelock-testing","targets":["fedora-rawhide-x86_64"]}'
assert_extra_packit_job_rejected \
    "Rawhide target outside production and staging" \
    '{"job":"copr_build","trigger":"release","owner":"other-owner","project":"facelock-scratch","targets":["fedora-rawhide-x86_64"]}'
assert_extra_packit_job_rejected \
    "facelock-testing incomplete staging targets" \
    '{"job":"copr_build","trigger":"pull_request","owner":"tyvsmith","project":"facelock-testing","targets":["fedora-43-x86_64","fedora-44-x86_64"]}'
assert_extra_packit_job_rejected \
    "duplicate targets outside production and staging" \
    '{"job":"copr_build","trigger":"pull_request","owner":"tyvsmith","project":"facelock-scratch","targets":["fedora-43-x86_64","fedora-43-x86_64"]}'
assert_extra_packit_job_rejected \
    "invalid targets outside production and staging" \
    '{"job":"copr_build","trigger":"pull_request","owner":"tyvsmith","project":"facelock-scratch","targets":"fedora-43-x86_64"}'

assert_matrix_mutation_rejected \
    "RPM authselect scriptlet dependency" \
    "dist/facelock.spec" \
    's/Requires(pre):  coreutils/Requires(pre):  authselect/'
assert_matrix_mutation_rejected \
    "RPM service-scoped PAM lifecycle omitted from package fixture" \
    "test/Containerfile.rpm-e2e" \
    's@COPY test/rpm-service-pam-lifecycle.sh /rpm-service-pam-lifecycle.sh@COPY test/rpm-service-pam-lifecycle.sh /rpm-service-pam-lifecycle-disabled.sh@'
# The variable is literal fixture text inside the sed expression.
# shellcheck disable=SC2016
assert_matrix_mutation_rejected \
    "RPM service-scoped PAM lifecycle omitted from booted runner" \
    "test/run-pkg-validate-systemd.sh" \
    's@podman exec "$cid" /rpm-service-pam-lifecycle.sh@podman exec "$cid" /rpm-service-pam-lifecycle-disabled.sh@'
assert_matrix_mutation_rejected \
    "sensitive shared stack offered as a setup candidate" \
    "crates/facelock-cli/src/commands/setup.rs" \
    '/const PAM_CANDIDATES/,/^];/s/service: "sudo"/service: "system-auth"/'
assert_matrix_mutation_rejected \
    "bare PAM service default drifted from sudo" \
    "crates/facelock-cli/src/commands/pam.rs" \
    '/pub const DEFAULT_PAM_SERVICE/s/"sudo"/"doas"/'
assert_matrix_mutation_rejected \
    "docs compatibility extra Debian tested row" \
    "docs/compatibility.md" \
    '/| Debian 13 (Trixie) |/a\| Debian 12 (Bookworm) | systemd | daemon + D-Bus activation | Booted package gate |'
assert_matrix_mutation_rejected \
    "book compatibility extra Ubuntu tested row" \
    "book/src/compatibility.md" \
    '/| Ubuntu 26.04 LTS (Resolute) |/a\| Ubuntu Noble | systemd | daemon + D-Bus activation | Booted package gate |'
assert_matrix_mutation_rejected \
    "trixie workflow variant axis reintroduced" \
    ".github/workflows/release.yml" \
    '0,/- suite: trixie/a\            variant: legacy'
assert_matrix_mutation_rejected \
    "Debian platform variant axis reintroduced" \
    "dist/release-matrix.json" \
    '/"id": "debian-13"/,/"channel":/s/"channel":/"variant": "legacy",\n      "channel":/'
assert_matrix_mutation_rejected \
    "Debian mandatory TPM capability removed" \
    "dist/release-matrix.json" \
    '/"id": "debian-13"/,/"channel":/s/"tpm"/"software-only"/'
assert_matrix_mutation_rejected \
    "trixie revision suffix" \
    "dist/release-matrix.json" \
    '0,/"revision_suffix": "~deb13u1"/s//"revision_suffix": "~deb99u1"/'
assert_matrix_mutation_rejected \
    "trixie suite architecture" \
    "dist/release-matrix.json" \
    '0,/"architecture": "amd64"/s//"architecture": "arm64"/'
assert_matrix_mutation_rejected \
    "trixie duplicated platform mapping" \
    "dist/release-matrix.json" \
    '0,/"platform": "Debian 13"/s//"platform": "Debian 12"/'
# Literal sed mutation; command substitutions belong to the workflow fixture.
# shellcheck disable=SC2016
assert_matrix_mutation_rejected \
    "stable publication suite input resolute to duplicate trixie" \
    ".github/workflows/release.yml" \
    's/"resolute=$(exact_deb_from_manifest resolute)"/"trixie=$(exact_deb_from_manifest resolute)"/'
assert_matrix_mutation_rejected \
    "production required supported chroot missing" \
    "dist/release-matrix.json" \
    '/"required_supported_chroots"/,/]/s/        "fedora-43-x86_64",//'
assert_matrix_mutation_rejected \
    "production optional experimental chroot overlaps a supported chroot" \
    "dist/release-matrix.json" \
    '/"optional_experimental_chroots"/,/]/s/"fedora-rawhide-x86_64"/"fedora-45-x86_64"/'
assert_matrix_mutation_rejected \
    "Rawhide support tier promoted to supported" \
    "dist/release-matrix.json" \
    '/"id": "fedora-rawhide"/,/"lifecycle_depth"/s/"support_tier": "experimental"/"support_tier": "supported"/'
assert_matrix_mutation_rejected \
    "Rawhide marked as a release target" \
    "dist/release-matrix.json" \
    '/"id": "fedora-rawhide"/,/"lifecycle_depth"/s/"release_target": false/"release_target": true/'
assert_matrix_mutation_rejected \
    "Rawhide served-version evidence enabled" \
    "dist/release-matrix.json" \
    's/"served_version": false/"served_version": true/'
assert_matrix_mutation_rejected \
    "Packit release target Fedora 45 to Rawhide" \
    ".packit.yaml" \
    's/"fedora-45-x86_64"/"fedora-rawhide-x86_64"/'

live_copr_supported_only="$tmp_root/live-copr-supported-only.json"
live_copr_supported_with_rawhide="$tmp_root/live-copr-supported-with-rawhide.json"
live_copr_missing_supported="$tmp_root/live-copr-missing-supported.json"
live_copr_unknown_extra="$tmp_root/live-copr-unknown-extra.json"
live_copr_wrong_project="$tmp_root/live-copr-wrong-project.json"
cat > "$live_copr_supported_only" <<'JSON'
{
  "ownername": "tyvsmith",
  "name": "facelock",
  "full_name": "tyvsmith/facelock",
  "chroot_repos": {
    "fedora-43-x86_64": "https://example.invalid/43/",
    "fedora-44-x86_64": "https://example.invalid/44/",
    "fedora-45-x86_64": "https://example.invalid/45/"
  }
}
JSON
cat > "$live_copr_supported_with_rawhide" <<'JSON'
{
  "ownername": "tyvsmith",
  "name": "facelock",
  "full_name": "tyvsmith/facelock",
  "chroot_repos": {
    "fedora-43-x86_64": "https://example.invalid/43/",
    "fedora-44-x86_64": "https://example.invalid/44/",
    "fedora-45-x86_64": "https://example.invalid/45/",
    "fedora-rawhide-x86_64": "https://example.invalid/rawhide/"
  }
}
JSON
cat > "$live_copr_missing_supported" <<'JSON'
{
  "ownername": "tyvsmith",
  "name": "facelock",
  "full_name": "tyvsmith/facelock",
  "chroot_repos": {
    "fedora-43-x86_64": "https://example.invalid/43/",
    "fedora-44-x86_64": "https://example.invalid/44/",
    "fedora-rawhide-x86_64": "https://example.invalid/rawhide/"
  }
}
JSON
cat > "$live_copr_unknown_extra" <<'JSON'
{
  "ownername": "tyvsmith",
  "name": "facelock",
  "full_name": "tyvsmith/facelock",
  "chroot_repos": {
    "fedora-43-x86_64": "https://example.invalid/43/",
    "fedora-44-x86_64": "https://example.invalid/44/",
    "fedora-45-x86_64": "https://example.invalid/45/",
    "fedora-rawhide-x86_64": "https://example.invalid/rawhide/",
    "fedora-46-x86_64": "https://example.invalid/46/"
  }
}
JSON
cat > "$live_copr_wrong_project" <<'JSON'
{
  "ownername": "tyvsmith",
  "name": "facelock-testing",
  "full_name": "tyvsmith/facelock-testing",
  "chroot_repos": {
    "fedora-43-x86_64": "https://example.invalid/43/",
    "fedora-44-x86_64": "https://example.invalid/44/",
    "fedora-45-x86_64": "https://example.invalid/45/"
  }
}
JSON
python3 "$repo_root/test/check-live-release-channels.py" --response-file "$live_copr_supported_only"
echo "release channel case: supported-only accepted"
python3 "$repo_root/test/check-live-release-channels.py" --response-file "$live_copr_supported_with_rawhide"
echo "release channel case: supported-plus-optional-Rawhide accepted"
if python3 "$repo_root/test/check-live-release-channels.py" --response-file "$live_copr_missing_supported" >/dev/null 2>&1; then
    fail "live COPR checker accepted a missing supported production chroot"
fi
echo "release channel case: missing-supported rejected"
if python3 "$repo_root/test/check-live-release-channels.py" --response-file "$live_copr_unknown_extra" >/dev/null 2>&1; then
    fail "live COPR checker accepted an unknown extra production chroot"
fi
echo "release channel case: unknown-extra rejected"
if python3 "$repo_root/test/check-live-release-channels.py" --response-file "$live_copr_wrong_project" >/dev/null 2>&1; then
    fail "live COPR checker accepted the wrong project identity"
fi
echo "release channel case: wrong-project rejected"
echo "release channel case: Rawhide-in-Packit-targets rejected"

prerelease_deb="$tmp_root/facelock-prerelease.deb"
: > "$prerelease_deb"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "0.2.0-1~deb13u1"\n' > "$tmp_root/bin/dpkg-deb"
chmod +x "$tmp_root/bin/dpkg-deb"
if apt_guard_output=$(
    PATH="$tmp_root/bin:$PATH" \
        bash "$repo_root/.github/workflows/scripts/publish-apt.sh" \
        "$tmp_root/apt-repo" "trixie=$prerelease_deb" 2>&1
); then
    fail "stable APT publisher accepted an incomplete suite set"
fi
case "$apt_guard_output" in
    *"requires exactly one package for each stable suite"*) ;;
    *) fail "stable APT publisher did not reject the incomplete suite set before signing setup: $apt_guard_output" ;;
esac

if apt_guard_output=$(
    PATH="$tmp_root/bin:$PATH" \
        bash "$repo_root/.github/workflows/scripts/publish-apt.sh" \
        "$tmp_root/apt-repo" \
        "trixie=$prerelease_deb" "trixie=$prerelease_deb" \
        "resolute=$prerelease_deb" 2>&1
); then
    fail "stable APT publisher accepted a duplicate suite"
fi
case "$apt_guard_output" in
    *"duplicate stable APT suite 'trixie'"*) ;;
    *) fail "stable APT publisher did not reject the duplicate suite before signing setup: $apt_guard_output" ;;
esac

printf '#!/usr/bin/env bash\nprintf "%%s\\n" "0.2.0~alpha.1-1~deb13u1"\n' > "$tmp_root/bin/dpkg-deb"
if apt_guard_output=$(
    PATH="$tmp_root/bin:$PATH" \
        bash "$repo_root/.github/workflows/scripts/publish-apt.sh" \
        "$tmp_root/apt-repo" \
        "trixie=$prerelease_deb" "resolute=$prerelease_deb" 2>&1
); then
    fail "stable APT publisher accepted a prerelease package"
fi
case "$apt_guard_output" in
    *"refusing prerelease"*) ;;
    *) fail "stable APT publisher did not reject the prerelease before signing setup: $apt_guard_output" ;;
esac

printf '#!/usr/bin/env bash\nprintf "%%s\\n" "0.2.0-1~deb13u99"\n' > "$tmp_root/bin/dpkg-deb"
if apt_guard_output=$(
    PATH="$tmp_root/bin:$PATH" \
        bash "$repo_root/.github/workflows/scripts/publish-apt.sh" \
        "$tmp_root/apt-repo" \
        "trixie=$prerelease_deb" "resolute=$prerelease_deb" 2>&1
); then
    fail "stable APT publisher accepted a package built for a different suite"
fi
case "$apt_guard_output" in
    *"does not match stable APT suite"*) ;;
    *) fail "stable APT publisher did not reject the suite/version mismatch before signing setup: $apt_guard_output" ;;
esac

assert_rejected bash "$repo_root/.github/workflows/scripts/publish-aur.sh" 0.2.0-alpha.1 unused

apt_recipe_root="$tmp_root/apt-recipe-root"
mkdir -p "$apt_recipe_root/dist/apt/conf" "$apt_recipe_root/scripts" "$apt_recipe_root/test"
cp "$repo_root/justfile" "$apt_recipe_root/"
cp "$repo_root/dist/apt/conf/distributions" "$apt_recipe_root/dist/apt/conf/"
cp "$repo_root/scripts/release-versions.sh" "$apt_recipe_root/scripts/"
cp "$repo_root/test/deb-package-contract.sh" \
    "$repo_root/test/deb-maintscript-contract.sh" \
    "$repo_root/test/publish-directory-atomic.py" \
    "$apt_recipe_root/test/"
declare -A apt_recipe_suffix=(
    [trixie]='~deb13u1'
    [resolute]='~ubuntu26.04.1'
)
apt_recipe_manifests=()
apt_recipe_checksum_record() {
    local path="$1"
    printf ' %s %s %s\n' \
        "$(sha256sum "$path" | cut -d' ' -f1)" \
        "$(stat -c %s "$path")" \
        "$(basename "$path")"
}
for suite in trixie resolute; do
    artifact_dir="$apt_recipe_root/artifacts/$suite"
    version="0.2.0-1${apt_recipe_suffix[$suite]}"
    source_basename="facelock_${version}"
    binary_basename="${source_basename}_amd64"
    manifest="$artifact_dir/${binary_basename}.manifest"
    mkdir -p "$artifact_dir"
    printf '%s\n' source >"$artifact_dir/facelock_0.2.0.orig.tar.gz"
    printf '%s\n' ort >"$artifact_dir/facelock_0.2.0.orig-onnxruntime.tar.gz"
    printf '%s\n' cargo >"$artifact_dir/facelock_0.2.0.orig-cargo-vendor.tar.xz"
    printf '%s\n' delta >"$artifact_dir/${source_basename}.debian.tar.xz"
    {
        printf '%s\n' \
            'Format: 3.0 (quilt)' \
            'Source: facelock' \
            "Version: $version" \
            'Checksums-Sha256:'
        apt_recipe_checksum_record "$artifact_dir/facelock_0.2.0.orig.tar.gz"
        apt_recipe_checksum_record "$artifact_dir/facelock_0.2.0.orig-onnxruntime.tar.gz"
        apt_recipe_checksum_record "$artifact_dir/facelock_0.2.0.orig-cargo-vendor.tar.xz"
        apt_recipe_checksum_record "$artifact_dir/${source_basename}.debian.tar.xz"
        printf '%s\n' 'Files:'
    } >"$artifact_dir/${source_basename}.dsc"
    printf '%s\n' buildinfo >"$artifact_dir/${binary_basename}.buildinfo"
    printf '%s\n' package >"$artifact_dir/${binary_basename}.deb"
    {
        printf '%s\n' \
            'Format: 1.8' \
            'Source: facelock' \
            'Binary: facelock' \
            'Architecture: source amd64' \
            "Version: $version" \
            "Distribution: $suite" \
            'Checksums-Sha256:'
        apt_recipe_checksum_record "$artifact_dir/facelock_0.2.0.orig.tar.gz"
        apt_recipe_checksum_record "$artifact_dir/facelock_0.2.0.orig-onnxruntime.tar.gz"
        apt_recipe_checksum_record "$artifact_dir/facelock_0.2.0.orig-cargo-vendor.tar.xz"
        apt_recipe_checksum_record "$artifact_dir/${source_basename}.debian.tar.xz"
        apt_recipe_checksum_record "$artifact_dir/${source_basename}.dsc"
        apt_recipe_checksum_record "$artifact_dir/${binary_basename}.buildinfo"
        apt_recipe_checksum_record "$artifact_dir/${binary_basename}.deb"
        printf '%s\n' 'Files:'
    } >"$artifact_dir/${binary_basename}.changes"
    artifacts=(
        'facelock_0.2.0.orig.tar.gz'
        'facelock_0.2.0.orig-onnxruntime.tar.gz'
        'facelock_0.2.0.orig-cargo-vendor.tar.xz'
        "${source_basename}.debian.tar.xz"
        "${source_basename}.dsc"
        "${binary_basename}.buildinfo"
        "${binary_basename}.deb"
        "${binary_basename}.changes"
    )
    printf '%s\n' "${artifacts[@]}" > "$manifest"
    apt_recipe_manifests+=("artifacts/$suite/${binary_basename}.manifest")
done
cat > "$tmp_root/bin/dpkg-deb" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
    --field|-f)
        case "${3:-}" in
            Package) printf '%s\n' facelock ;;
            Depends) printf '%s\n' 'dbus, libpam-runtime, libc6 (>= 2.36), libtss2-esys-3.0.2-0t64, libtss2-mu-4.0.1-0t64, libtss2-tctildr0t64' ;;
            Provides|Conflicts|Replaces) printf '\n' ;;
            Version)
                version="${2##*/}"
                version="${version#facelock_}"
                printf '%s\n' "${version%_amd64.deb}"
                ;;
            Architecture) printf '%s\n' amd64 ;;
            *) exit 2 ;;
        esac
        ;;
    --control)
        mkdir -p "${3:?}"
        cat >"$3/postinst" <<'SCRIPT'
#!/bin/sh
if [ -d /run/systemd/system ] && \
           systemctl is-active --quiet facelock-daemon.service; then
            systemctl try-restart facelock-daemon.service 2>/dev/null || true
fi
systemd-tmpfiles ${DPKG_ROOT:+--root="$DPKG_ROOT"} --create facelock.conf || true
SCRIPT
        cat >"$3/prerm" <<'SCRIPT'
#!/bin/sh
facelock pam shared-profile-status
facelock pam remove --all --dry-run
facelock pam remove --all
if [ -z "$DPKG_ROOT" ] && [ "$1" = remove ] && [ -d /run/systemd/system ]; then
    deb-systemd-invoke stop 'facelock-daemon.service' >/dev/null || true
fi
SCRIPT
        cat >"$3/postrm" <<'SCRIPT'
#!/bin/sh
if [ "$1" = "purge" ]; then
    deb-systemd-helper purge 'facelock-daemon.service' >/dev/null || true
fi
SCRIPT
        ;;
    *) exit 2 ;;
esac
SH
cat > "$tmp_root/bin/reprepro" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = -b ] || exit 2
repo_dir="${2:-}"
case "${3:-}" in
    check) exit 0 ;;
    includedeb)
        suite="${4:-}"
        package="${5:-}"
        mkdir -p \
            "$repo_dir/dists/$suite/facelock/binary-amd64" \
            "$repo_dir/pool/facelock"
        : > "$repo_dir/dists/$suite/Release"
        : > "$repo_dir/dists/$suite/facelock/binary-amd64/Packages"
        cp -- "$package" "$repo_dir/pool/facelock/"
        ;;
    *) exit 2 ;;
esac
SH
chmod +x "$tmp_root/bin/dpkg-deb" "$tmp_root/bin/reprepro"
if ! (
    cd "$apt_recipe_root"
    PATH="$tmp_root/bin:$PATH" just test-apt-repo >/dev/null 2>&1
); then
    fail "test-apt-repo rejected config-only validation without a manifest"
fi
if ! (
    cd "$apt_recipe_root"
    PATH="$tmp_root/bin:$PATH" just test-apt-repo "${apt_recipe_manifests[@]}" >/dev/null 2>&1
); then
    fail "test-apt-repo rejected the complete set of two exact generated manifests"
fi
if (
    cd "$apt_recipe_root"
    PATH="$tmp_root/bin:$PATH" just test-apt-repo "${apt_recipe_manifests[0]}" >/dev/null 2>&1
); then
    fail "test-apt-repo accepted an incomplete generated-manifest set"
fi

cp "$repo_root/justfile" "$repo_root/Cargo.toml" "$repo_root/Cargo.lock" "$release_repo/"
cp "$repo_root/dist/PKGBUILD" "$repo_root/dist/PKGBUILD-bin" "$repo_root/dist/PKGBUILD-git" "$repo_root/dist/facelock.spec" "$release_repo/dist/"
cp "$repo_root/debian/changelog" "$release_repo/debian/"
cp "$repo_root/scripts/release-versions.sh" "$release_repo/scripts/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$tmp_root/bin/cargo"
chmod +x "$tmp_root/bin/cargo"

git -C "$release_repo" init -q
git -C "$release_repo" config user.name release-test
git -C "$release_repo" config user.email release-test@example.invalid
git -C "$release_repo" add .
git -C "$release_repo" -c commit.gpgsign=false commit -qm baseline

(
    cd "$release_repo"
    PATH="$tmp_root/bin:$PATH" just release 0.2.0-alpha.1 >/dev/null
)
assert_file_line "$release_repo/Cargo.toml" 'version = "0.2.0-alpha.1"'
assert_file_line "$release_repo/dist/PKGBUILD" '_tag=0.2.0-alpha.1'
assert_file_line "$release_repo/dist/PKGBUILD" 'pkgver=0.2.0alpha1'
assert_file_line "$release_repo/dist/PKGBUILD" 'pkgrel=1'
assert_file_line "$release_repo/dist/PKGBUILD-bin" '_tag=0.2.0-alpha.1'
assert_file_line "$release_repo/dist/PKGBUILD-bin" 'pkgver=0.2.0alpha1'
assert_file_line "$release_repo/dist/facelock.spec" 'Version:        0.2.0'
assert_file_line "$release_repo/dist/facelock.spec" 'Release:        0.1.alpha.1%{?dist}'
grep -Fq 'facelock (0.2.0~alpha.1-1) unstable;' "$release_repo/debian/changelog" || fail "first alpha Debian revision missing"

git -C "$release_repo" add .
git -C "$release_repo" -c commit.gpgsign=false commit -qm alpha-1-build-1
(
    cd "$release_repo"
    PATH="$tmp_root/bin:$PATH" just release 0.2.0-alpha.1 >/dev/null
)
assert_file_line "$release_repo/dist/PKGBUILD" 'pkgrel=2'
assert_file_line "$release_repo/dist/facelock.spec" 'Release:        0.2.alpha.1%{?dist}'
grep -Fq 'facelock (0.2.0~alpha.1-2) unstable;' "$release_repo/debian/changelog" || fail "rebuilt alpha Debian revision missing"

git -C "$release_repo" add .
git -C "$release_repo" -c commit.gpgsign=false commit -qm alpha-1-build-2
(
    cd "$release_repo"
    PATH="$tmp_root/bin:$PATH" just release 0.2.0-alpha.2 >/dev/null
)
assert_file_line "$release_repo/dist/PKGBUILD" 'pkgrel=1'
assert_file_line "$release_repo/dist/facelock.spec" 'Release:        0.3.alpha.2%{?dist}'
grep -Fq 'facelock (0.2.0~alpha.2-1) unstable;' "$release_repo/debian/changelog" || fail "successive alpha Debian revision missing"

git -C "$release_repo" add .
git -C "$release_repo" -c commit.gpgsign=false commit -qm alpha-2-build-1
(
    cd "$release_repo"
    PATH="$tmp_root/bin:$PATH" just release 0.2.0-beta.1 >/dev/null
)
assert_file_line "$release_repo/dist/PKGBUILD" 'pkgver=0.2.0beta1'
assert_file_line "$release_repo/dist/PKGBUILD" 'pkgrel=1'
assert_file_line "$release_repo/dist/facelock.spec" 'Release:        0.4.beta.1%{?dist}'
grep -Fq 'facelock (0.2.0~beta.1-1) unstable;' "$release_repo/debian/changelog" || fail "beta Debian revision missing"

git -C "$release_repo" add .
git -C "$release_repo" -c commit.gpgsign=false commit -qm beta-1-build-1
if (
    cd "$release_repo"
    PATH="$tmp_root/bin:$PATH" just release 0.2.0-alpha.3 >/dev/null 2>&1
); then
    fail "just release accepted an alpha after the same base reached beta"
fi
git -C "$release_repo" diff --quiet || fail "rejected beta-to-alpha transition changed release metadata"
(
    cd "$release_repo"
    PATH="$tmp_root/bin:$PATH" just release 0.2.0-rc.1 >/dev/null
)
assert_file_line "$release_repo/dist/PKGBUILD" 'pkgver=0.2.0rc1'
assert_file_line "$release_repo/dist/PKGBUILD" 'pkgrel=1'
assert_file_line "$release_repo/dist/facelock.spec" 'Release:        0.5.rc.1%{?dist}'
grep -Fq 'facelock (0.2.0~rc.1-1) unstable;' "$release_repo/debian/changelog" || fail "release candidate Debian revision missing"

git -C "$release_repo" add .
git -C "$release_repo" -c commit.gpgsign=false commit -qm rc-1-build-1
(
    cd "$release_repo"
    PATH="$tmp_root/bin:$PATH" just release 0.2.0 >/dev/null
)
assert_file_line "$release_repo/dist/PKGBUILD" '_tag=0.2.0'
assert_file_line "$release_repo/dist/PKGBUILD" 'pkgver=0.2.0'
assert_file_line "$release_repo/dist/PKGBUILD" 'pkgrel=1'
assert_file_line "$release_repo/dist/facelock.spec" 'Version:        0.2.0'
assert_file_line "$release_repo/dist/facelock.spec" 'Release:        1%{?dist}'
grep -Fq 'facelock (0.2.0-1) unstable;' "$release_repo/debian/changelog" || fail "stable Debian revision missing"

git -C "$release_repo" add .
git -C "$release_repo" -c commit.gpgsign=false commit -qm stable
if (
    cd "$release_repo"
    PATH="$tmp_root/bin:$PATH" just release 0.2.0 >/dev/null 2>&1
); then
    fail "just release accepted a repeated stable version"
fi
git -C "$release_repo" diff --quiet || fail "rejected repeated stable release changed release metadata"
if (
    cd "$release_repo"
    PATH="$tmp_root/bin:$PATH" just release 0.2.0-alpha.3 >/dev/null 2>&1
); then
    fail "just release accepted a prerelease after the same RPM Version reached stable"
fi
git -C "$release_repo" diff --quiet || fail "rejected stable-to-prerelease transition changed release metadata"

echo "release version contract: OK"
