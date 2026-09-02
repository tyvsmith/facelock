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
    for gnupg_home in "${apt_keygen_home:-}" "${apt_publisher_gnupg:-}"; do
        if [ -d "$gnupg_home" ]; then
            GNUPGHOME="$gnupg_home" gpgconf --kill gpg-agent 2>/dev/null || true
        fi
    done
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
cp "$repo_root/README.md" "$repo_root/CONTRIBUTING.md" "$repo_root/Cargo.toml" "$matrix_root/"
cp "$repo_root/book/src/quickstart.md" "$repo_root/book/src/contributing.md" "$repo_root/book/src/compatibility.md" "$matrix_root/book/src/"
cp "$repo_root/.github/ISSUE_TEMPLATE/bug_report.md" "$matrix_root/.github/ISSUE_TEMPLATE/"
cp "$repo_root/crates/facelock-cli/src/commands/pam.rs" "$matrix_root/crates/facelock-cli/src/commands/"
cp "$repo_root/crates/facelock-cli/src/commands/setup.rs" "$matrix_root/crates/facelock-cli/src/commands/"
cp "$repo_root/website/index.html" "$matrix_root/website/"
cp "$repo_root/test/check-release-matrix.py" "$matrix_root/test/"
cp "$repo_root/test/Containerfile" "$matrix_root/test/"
cp "$repo_root/test/Containerfile.rpm-e2e" "$matrix_root/test/"
# Every Fedora lane Containerfile the checker reads, not just the e2e one. The
# other three were added to check-release-matrix.py without reaching this
# staging list, so the checker died on a missing file here long before it could
# assert anything (#229 wired this recipe into CI, which is how that surfaced).
cp "$repo_root/test/Containerfile.rpm-authselect" "$matrix_root/test/"
cp "$repo_root/test/Containerfile.copr" "$matrix_root/test/"
cp "$repo_root/test/Containerfile.copr-e2e" "$matrix_root/test/"
cp "$repo_root/test/Containerfile.fedora" "$matrix_root/test/"
cp "$repo_root/.dockerignore" "$matrix_root/"
cp "$repo_root/test/fedora-lane-image.sh" "$matrix_root/test/"
cp "$repo_root/.github/workflows/packaging.yml" "$matrix_root/.github/workflows/"
cp "$repo_root/test/copr-build.sh" "$matrix_root/test/"
cp "$repo_root/test/packit-config-validate.sh" "$matrix_root/test/"
cp "$repo_root/test/Containerfile.packit" "$matrix_root/test/"
cp "$repo_root/test/Containerfile.apt-client" "$matrix_root/test/"
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
    local diagnostic="${4:-}"
    local mutation_root="$tmp_root/matrix-mutation-$matrix_mutation_index"
    local checker_output
    matrix_mutation_index=$((matrix_mutation_index + 1))
    cp -R "$matrix_root" "$mutation_root"
    sed -i "$expression" "$mutation_root/$relative_file"
    if cmp -s "$matrix_root/$relative_file" "$mutation_root/$relative_file"; then
        fail "matrix mutation fixture did not change $relative_file: $context"
    fi
    if checker_output=$(RELEASE_MATRIX_VERSION=0.2.0 python3 "$mutation_root/test/check-release-matrix.py" 2>&1); then
        fail "release matrix checker accepted drift: $context"
    fi
    if [ -n "$diagnostic" ]; then
        case "$checker_output" in
            *"$diagnostic"*) ;;
            *) fail "release matrix checker rejected $context for another reason: $checker_output" ;;
        esac
    fi
    echo "release matrix mutation case: $context rejected"
}

assert_matrix_mutation_rejected \
    "CI Debian source-contract target invocation" \
    ".github/workflows/ci.yml" \
    's/run: just test-deb-source-contract/run: just test-deb-source-contract-disabled/'
assert_matrix_mutation_rejected \
    "release Debian source-contract target invocation" \
    ".github/workflows/release.yml" \
    's/just test-deb-source-contract/just test-deb-source-contract-disabled/'
assert_matrix_mutation_rejected \
    "Debian source-contract shell recipe command" \
    "justfile" \
    's@    bash test/deb-source-contract.sh@    bash test/deb-source-contract-disabled.sh@'
assert_matrix_mutation_rejected \
    "Debian source-contract mutation recipe command" \
    "justfile" \
    's@    python3 test/deb-source-contract-test.py@    python3 test/deb-source-contract-test-disabled.py@'
assert_matrix_mutation_rejected \
    "Packit schema gate skipped in preflight" \
    "justfile" \
    's@    bash test/packit-config-validate.sh || failed=1@    echo "SKIP: packit CLI not installed"@'
assert_matrix_mutation_rejected \
    "Packit schema gate run from the host instead of the pinned container" \
    "justfile" \
    's@    bash test/packit-config-validate.sh || failed=1@    packit config validate --offline -c .packit.yaml || failed=1@'
assert_matrix_mutation_rejected \
    "Packit schema gate validator command" \
    "test/packit-config-validate.sh" \
    's@packit config validate --offline -c .packit.yaml@packit config validate -c .packit.yaml@'
assert_matrix_mutation_rejected \
    "Packit schema gate image digest pin" \
    "test/Containerfile.packit" \
    's|@sha256:[0-9a-f]*||'
assert_matrix_mutation_rejected \
    "packaging workflow Fedora lanes uploading no evidence artifact" \
    ".github/workflows/packaging.yml" \
    's/name: packaging-evidence-rpm-/name: packaging-evidence-fedora-/'
assert_matrix_mutation_rejected \
    "packaging workflow tolerating a lane that recorded nothing" \
    ".github/workflows/packaging.yml" \
    's/if-no-files-found: error/if-no-files-found: warn/'
assert_matrix_mutation_rejected \
    "release preflight validating something other than the packaging marker" \
    "justfile" \
    's@validate --commit "\$HEAD_SHA" .packaging-matrix-verified@validate --commit "$HEAD_SHA" .hardware-tiers-verified@'
assert_matrix_mutation_rejected \
    "release preflight skipping the workflow-run evidence download" \
    "justfile" \
    's@packaging-evidence.py ci-run --commit@packaging-evidence.py ci-run-disabled --commit@'
assert_matrix_mutation_rejected \
    "release preflight swallowing the evidence command's own failure" \
    "justfile" \
    's@ci-run --commit "\$HEAD_SHA" --run "\$run_id"; then@ci-run --commit "$HEAD_SHA" --run "$run_id" || true; then@'
assert_matrix_mutation_rejected \
    "packaging workflow keeping the evidence artifact name only in a comment" \
    ".github/workflows/packaging.yml" \
    's/^          name: packaging-evidence-arch$/          # name: packaging-evidence-arch\n          name: packaging-evidence-arch-moved/'
assert_matrix_mutation_rejected \
    "release preflight keeping the validate call only in a comment" \
    "justfile" \
    's@^    elif python3 test/packaging-evidence.py validate --commit "\$HEAD_SHA" .packaging-matrix-verified; then$@    # python3 test/packaging-evidence.py validate --commit "$HEAD_SHA" .packaging-matrix-verified\n    elif true; then@'
assert_matrix_mutation_rejected \
    "packaging matrix keeping the aggregate call only in a comment" \
    "justfile" \
    's@^    python3 test/packaging-evidence.py aggregate --commit "\$commit" --tree-clean \\$@    # python3 test/packaging-evidence.py aggregate --commit "$commit" --tree-clean\n    true \\@'
assert_matrix_mutation_rejected \
    "packaging workflow with every evidence upload step commented out" \
    ".github/workflows/packaging.yml" \
    '/^      - name: Upload packaging evidence$/,/^          overwrite: true$/s/^\( *\)/\1# /'
assert_matrix_mutation_rejected \
    "packaging workflow adding a lane upload without bumping the step count" \
    ".github/workflows/packaging.yml" \
    '/^          name: packaging-evidence-arch$/,/^          overwrite: true$/{/^          overwrite: true$/a\      - name: Upload extra evidence\n        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a\n        with:\n          name: packaging-evidence-extra\n          path: .packaging-evidence/\n          if-no-files-found: error\n          overwrite: true
}'
assert_matrix_mutation_rejected \
    "packaging workflow uploading the hidden evidence directory without opting in" \
    ".github/workflows/packaging.yml" \
    's/^          include-hidden-files: true$//'
assert_matrix_mutation_rejected \
    "release preflight keeping the validate call in a trailing comment" \
    "justfile" \
    's@^    elif python3 test/packaging-evidence.py validate --commit "\$HEAD_SHA" .packaging-matrix-verified; then$@    elif true; then # python3 test/packaging-evidence.py validate --commit "$HEAD_SHA" .packaging-matrix-verified@'
assert_matrix_mutation_rejected \
    "packaging matrix echoing the commit into the marker beside the aggregate" \
    "justfile" \
    's@^        --evidence-dir .packaging-evidence --output .packaging-matrix-verified$@&\n    echo "$commit" > .packaging-matrix-verified@'
assert_matrix_mutation_rejected \
    "packaging matrix teeing the commit into the marker beside the aggregate" \
    "justfile" \
    's@^        --evidence-dir .packaging-evidence --output .packaging-matrix-verified$@&\n    echo "$commit" | tee --append .packaging-matrix-verified@'
# A marker write that exists only in a trailing comment is not a marker write:
# the guard reads executable text, not the whole line.
harmless_comment_root="$tmp_root/matrix-harmless-comment"
cp -R "$matrix_root" "$harmless_comment_root"
sed -i 's@^    echo "Recorded: packaging matrix evidence at $commit"$@&  # never: echo "$commit" > .packaging-matrix-verified@' "$harmless_comment_root/justfile"
grep -q 'never: echo' "$harmless_comment_root/justfile" || fail "harmless-comment fixture did not change the justfile"
RELEASE_MATRIX_VERSION=0.2.0 python3 "$harmless_comment_root/test/check-release-matrix.py" >/dev/null 2>&1 ||
    fail "release matrix checker rejected a marker write that exists only in a trailing comment"
echo "release matrix case: marker write in a trailing comment tolerated"
assert_matrix_mutation_rejected \
    "release matrix granting evidence eligibility outside Rawhide" \
    "dist/release-matrix.json" \
    's/"id": "debian-13",/"id": "debian-13", "evidence_eligibility": {"lifecycle": false},/'
assert_matrix_mutation_rejected \
    "packaging matrix recording without aggregating lane evidence" \
    "justfile" \
    's@packaging-evidence.py aggregate --commit@packaging-evidence.py aggregate-disabled --commit@'
assert_matrix_mutation_rejected \
    "PKGBUILD-git dropping the onnxruntime runtime dependency" \
    "dist/PKGBUILD-git" \
    "s/ 'onnxruntime'//"
assert_matrix_mutation_rejected \
    "PKGBUILD source digest skipped" \
    "dist/PKGBUILD" \
    "s/^sha256sums=.*/sha256sums=('SKIP')/"

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
# The v0.1.4 suite names stay published until 0.3.0 (#310). Every place that
# promise lives must agree, and the window must actually close.
assert_matrix_mutation_rejected \
    "compatibility suite retire_at removed" \
    "dist/release-matrix.json" \
    '0,/"retire_at": "0.3.0"/s//"retired": "0.3.0"/' \
    "has no stable retire_at version"
assert_matrix_mutation_rejected \
    "compatibility suites retire at different versions" \
    "dist/release-matrix.json" \
    '0,/"retire_at": "0.3.0"/s//"retire_at": "0.4.0"/' \
    "must share one retirement version"
assert_matrix_mutation_rejected \
    "compatibility suite main sourced from resolute" \
    "dist/release-matrix.json" \
    's/"source": "trixie"/"source": "resolute"/' \
    "compatibility APT suite main source drifted"
assert_matrix_mutation_rejected \
    "compatibility suite legacy stanza dropped from the APT config" \
    "dist/apt/conf/distributions" \
    's/^Codename: legacy$/Codename: bookworm/' \
    "APT config suites"
assert_matrix_mutation_rejected \
    "compatibility suite main description silent about the deprecation" \
    "dist/apt/conf/distributions" \
    's/^\(Description: .*\)deprecated\(.*trixie.*\)$/\1renamed\2/' \
    "must name the deprecation and the codenamed replacement"
assert_matrix_mutation_rejected \
    "APT suite Origin changed" \
    "dist/apt/conf/distributions" \
    's/^Origin: tysmith$/Origin: facelock/' \
    "Origin drifted"
assert_matrix_mutation_rejected \
    "APT suite Label changed" \
    "dist/apt/conf/distributions" \
    's/^Label: Ty Smith Packages$/Label: Facelock Packages/' \
    "Label drifted"
assert_matrix_mutation_rejected \
    "compatibility suite legacy left unsigned" \
    "dist/apt/conf/distributions" \
    '/^Codename: legacy$/,/^SignWith: default$/s/^SignWith: default$/SignWith: no/' \
    "APT suite legacy SignWith drifted"
assert_matrix_mutation_rejected \
    "APT publisher stops mirroring trixie into main" \
    ".github/workflows/scripts/publish-apt.sh" \
    's/includedeb main/includedeb trixie/' \
    "does not populate compatibility suite main"
assert_matrix_mutation_rejected \
    "APT publisher stops exporting legacy" \
    ".github/workflows/scripts/publish-apt.sh" \
    's/export legacy/export resolute/' \
    "does not populate compatibility suite legacy"
assert_matrix_mutation_rejected \
    "APT repo artifact drops the dists tree" \
    ".github/workflows/release.yml" \
    's/dists pool tysmith-archive-keyring.gpg/pool tysmith-archive-keyring.gpg/' \
    "must carry the whole dists tree"
assert_matrix_mutation_rejected \
    "README migration note dropped" \
    "README.md" \
    's/keep working until 0.3.0/keep working/' \
    "README.md omits the APT compatibility window"
assert_matrix_mutation_rejected \
    "README install instruction configures the main suite" \
    "README.md" \
    's/apt ${APT_SUITE} facelock/apt main facelock/' \
    "README.md still configures a retired APT suite"
assert_matrix_mutation_rejected \
    "quickstart migration note dropped" \
    "book/src/quickstart.md" \
    's/keep working until 0.3.0/keep working/' \
    "book/src/quickstart.md omits the APT compatibility window"
assert_matrix_mutation_rejected \
    "README 0.3.0 failure mode dropped" \
    "README.md" \
    's/fails until the entry is removed/fails/' \
    "README.md omits the APT compatibility window"
assert_matrix_mutation_rejected \
    "website migration note dropped" \
    "website/index.html" \
    's/keep working until 0.3.0/keep working/' \
    "website/index.html omits the APT compatibility window"
assert_matrix_mutation_rejected \
    "release checklist line dropped" \
    "docs/releasing.md" \
    's/compatibility suites present until 0.3.0/compatibility suites present/' \
    "docs/releasing.md omits the APT compatibility window"
assert_matrix_mutation_rejected \
    "contract retirement version dropped" \
    "docs/contracts.md" \
    's/removed at 0.3.0/removed later/' \
    "docs/contracts.md omits the APT compatibility window"

for retired_version in 0.3.0 0.3.0-alpha.1 1.0.0; do
    if checker_output=$(RELEASE_MATRIX_VERSION="$retired_version" python3 "$matrix_root/test/check-release-matrix.py" 2>&1); then
        fail "release matrix checker kept the APT compatibility suites at $retired_version"
    fi
    case "$checker_output" in
        *"reached retire_at 0.3.0"*) ;;
        *) fail "release matrix checker rejected $retired_version for another reason: $checker_output" ;;
    esac
    echo "release matrix expiry case: compatibility suites rejected at $retired_version"
done
RELEASE_MATRIX_VERSION=0.2.9 python3 "$matrix_root/test/check-release-matrix.py" >/dev/null
echo "release matrix expiry case: compatibility suites accepted at 0.2.9"

# The terminal state is reachable: with the stanzas and the window claims
# gone, and the publisher untouched, 0.3.0 passes.
retired_root="$tmp_root/matrix-retired"
cp -R "$matrix_root" "$retired_root"
awk 'BEGIN { RS = ""; ORS = "\n\n" } !/Codename: (main|legacy)\n/' \
    "$matrix_root/dist/apt/conf/distributions" > "$retired_root/dist/apt/conf/distributions"
sed -i 's/`main` and `legacy` are compatibility suites/`main` and `legacy` were compatibility suites/; s/removed at 0.3.0/removed in 0.3.0/' \
    "$retired_root/docs/contracts.md"
sed -i 's/compatibility suites present until 0.3.0/compatibility suites removed in 0.3.0/' "$retired_root/docs/releasing.md"
sed -i 's/keep working until 0.3.0/stopped working in 0.3.0/' \
    "$retired_root/README.md" "$retired_root/book/src/quickstart.md" "$retired_root/website/index.html"
if ! checker_output=$(RELEASE_MATRIX_VERSION=0.3.0 python3 "$retired_root/test/check-release-matrix.py" 2>&1); then
    fail "release matrix checker rejected the retired compatibility-suite state at 0.3.0: $checker_output"
fi
echo "release matrix expiry case: retired state accepted at 0.3.0"
if RELEASE_MATRIX_VERSION=0.2.0 python3 "$retired_root/test/check-release-matrix.py" >/dev/null 2>&1; then
    fail "release matrix checker accepted the retired compatibility-suite state inside the window"
fi
echo "release matrix expiry case: retired state rejected at 0.2.0"

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

# The APT client lane replays a v0.1.4 client from this fixture; it must stay
# byte-identical to what v0.1.4 published.
if git -C "$repo_root" rev-parse -q --verify 'v0.1.4^{commit}' >/dev/null 2>&1; then
    git -C "$repo_root" show v0.1.4:dist/apt/conf/distributions \
        | cmp -s - "$repo_root/test/fixtures/apt-distributions-v0.1.4" \
        || fail "test/fixtures/apt-distributions-v0.1.4 differs from v0.1.4:dist/apt/conf/distributions"
    echo "APT fixture case: v0.1.4 distributions fixture matches the tag"
else
    echo "APT fixture case: skipped, v0.1.4 not reachable in this checkout"
fi

# Run to completion under an ephemeral signing key, the publisher must fill
# every suite the config declares: the codenamed pair, `main` with trixie's
# exact package, and `legacy` exported with signed empty indexes (#310).
# reprepro is a recording stand-in; gpg is real, in throwaway homes. Both
# homes differ from $HOME/.gnupg, which is what gives them their own agent
# socket rather than the user's.
apt_keygen_home="$tmp_root/apt-keygen"
apt_publisher_gnupg="$tmp_root/apt-publisher-gnupg"
apt_tree_root="$tmp_root/apt-tree"
apt_tree_debs="$tmp_root/apt-tree-debs"
mkdir -p "$apt_tree_debs"
mkdir -m 700 "$apt_keygen_home"
GNUPGHOME="$apt_keygen_home" gpg --batch --quiet --pinentry-mode loopback --passphrase contract-passphrase \
    --quick-generate-key "Facelock contract test <apt-contract@example.invalid>" ed25519 sign never
apt_private_key="$(GNUPGHOME="$apt_keygen_home" gpg --batch --quiet --pinentry-mode loopback \
    --passphrase contract-passphrase --armor --export-secret-keys)"
for suite in trixie resolute; do
    : > "$apt_tree_debs/$suite.deb"
done
cat > "$tmp_root/bin/dpkg-deb" <<'SH'
#!/usr/bin/env bash
case "$2" in
    */trixie.deb) printf '%s\n' '0.2.0-1~deb13u1' ;;
    */resolute.deb) printf '%s\n' '0.2.0-1~ubuntu26.04.1' ;;
    *) exit 1 ;;
esac
SH
cat > "$tmp_root/bin/reprepro" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = -b ] || exit 2
repo_dir="${2:-}"
command="${3:-}"
suite="${4:-}"
printf '%s\n' "$*" >> "$repo_dir/reprepro-calls"
index_dir="$repo_dir/dists/$suite/facelock/binary-amd64"
mkdir -p "$index_dir"
case "$command" in
    includedeb)
        package="${5:-}"
        mkdir -p "$repo_dir/pool/facelock"
        cp -- "$package" "$repo_dir/pool/facelock/"
        printf 'Package: facelock\nFilename: pool/facelock/%s\n\n' "$(basename "$package")" >> "$index_dir/Packages"
        ;;
    export) : >> "$index_dir/Packages" ;;
    *) exit 2 ;;
esac
: > "$repo_dir/dists/$suite/Release"
: > "$repo_dir/dists/$suite/InRelease"
SH
chmod +x "$tmp_root/bin/dpkg-deb" "$tmp_root/bin/reprepro"
if ! apt_tree_output=$(
    cd "$repo_root" && \
        GNUPGHOME="$apt_publisher_gnupg" APT_GPG_PRIVATE_KEY="$apt_private_key" APT_GPG_PASSPHRASE=contract-passphrase \
        PATH="$tmp_root/bin:$PATH" \
        bash .github/workflows/scripts/publish-apt.sh "$apt_tree_root" \
        "trixie=$apt_tree_debs/trixie.deb" "resolute=$apt_tree_debs/resolute.deb" 2>&1
); then
    printf '%s\n' "$apt_tree_output"
    fail "stable APT publisher did not build the compatibility tree"
fi
for suite in trixie resolute main legacy; do
    for index in Release InRelease; do
        [ -f "$apt_tree_root/dists/$suite/$index" ] || fail "stable APT publisher left dists/$suite/$index unpublished"
    done
done
grep -Fqx -- "-b $apt_tree_root includedeb main $apt_tree_debs/trixie.deb" "$apt_tree_root/reprepro-calls" \
    || fail "stable APT publisher did not place trixie's exact package in main"
grep -Fqx -- "-b $apt_tree_root export legacy" "$apt_tree_root/reprepro-calls" \
    || fail "stable APT publisher did not export legacy"
[ ! -s "$apt_tree_root/dists/legacy/facelock/binary-amd64/Packages" ] \
    || fail "stable APT publisher listed a package in legacy"
cmp -s "$apt_tree_root/dists/main/facelock/binary-amd64/Packages" "$apt_tree_root/dists/trixie/facelock/binary-amd64/Packages" \
    || fail "main index differs from the trixie index"
! grep -q resolute "$apt_tree_root/dists/main/facelock/binary-amd64/Packages" \
    || fail "main index carries the resolute package"
[ -s "$apt_tree_root/tysmith-archive-keyring.gpg" ] || fail "stable APT publisher exported no public keyring"
case "$apt_tree_output" in
    *"Release file (main)"*"Release file (legacy)"*) ;;
    *) fail "stable APT publisher did not print the compatibility suite Release files" ;;
esac
echo "stable APT publisher case: codenamed and compatibility suites published"

# With the stanzas deleted and the publisher untouched, the compatibility
# steps must not run: an undeclared codename would make reprepro fail the
# release under set -e (#320).
apt_retired_publisher_root="$tmp_root/apt-retired-publisher"
apt_retired_tree_root="$tmp_root/apt-retired-tree"
mkdir -p "$apt_retired_publisher_root/.github/workflows/scripts" "$apt_retired_publisher_root/scripts" "$apt_retired_publisher_root/dist/apt/conf"
cp "$repo_root/.github/workflows/scripts/publish-apt.sh" "$apt_retired_publisher_root/.github/workflows/scripts/"
cp "$repo_root/scripts/release-versions.sh" "$apt_retired_publisher_root/scripts/"
cp "$retired_root/dist/apt/conf/distributions" "$apt_retired_publisher_root/dist/apt/conf/"
if ! apt_tree_output=$(
    cd "$apt_retired_publisher_root" && \
        GNUPGHOME="$apt_publisher_gnupg" APT_GPG_PRIVATE_KEY="$apt_private_key" APT_GPG_PASSPHRASE=contract-passphrase \
        PATH="$tmp_root/bin:$PATH" \
        bash .github/workflows/scripts/publish-apt.sh "$apt_retired_tree_root" \
        "trixie=$apt_tree_debs/trixie.deb" "resolute=$apt_tree_debs/resolute.deb" 2>&1
); then
    printf '%s\n' "$apt_tree_output"
    fail "stable APT publisher failed once the compatibility stanzas were retired"
fi
for suite in trixie resolute; do
    [ -f "$apt_retired_tree_root/dists/$suite/Release" ] || fail "retired-stanza publisher left dists/$suite/Release unpublished"
done
for suite in main legacy; do
    ! grep -q -- " $suite" "$apt_retired_tree_root/reprepro-calls" \
        || fail "retired-stanza publisher still ran reprepro for $suite"
    [ ! -e "$apt_retired_tree_root/dists/$suite" ] || fail "retired-stanza publisher still published dists/$suite"
done
echo "stable APT publisher case: retired stanzas leave the compatibility steps unrun"

assert_rejected bash "$repo_root/.github/workflows/scripts/publish-aur.sh" 0.2.0-alpha.1 unused
# A stable version with an implausible source digest must be refused before any
# publish step: a malformed value or the sha256 of empty input is what a silent
# download failure produces, and neither may become the published pin (#283).
assert_rejected bash "$repo_root/.github/workflows/scripts/publish-aur.sh" 0.2.0 not-a-digest
assert_rejected bash "$repo_root/.github/workflows/scripts/publish-aur.sh" 0.2.0 \
    e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855

apt_recipe_root="$tmp_root/apt-recipe-root"
mkdir -p \
    "$apt_recipe_root/debian" \
    "$apt_recipe_root/dist/apt/conf" \
    "$apt_recipe_root/scripts" \
    "$apt_recipe_root/test"
cp "$repo_root/justfile" "$apt_recipe_root/"
cp "$repo_root/debian/postrm" "$apt_recipe_root/debian/"
cat >"$apt_recipe_root/debian/generated-postrm-helper" <<'SCRIPT'
if [ "$1" = "purge" ]; then
    deb-systemd-helper purge 'facelock-daemon.service' >/dev/null || true
fi
SCRIPT
cp "$repo_root/dist/apt/conf/distributions" "$apt_recipe_root/dist/apt/conf/"
cp "$repo_root/dist/release-matrix.json" "$apt_recipe_root/dist/"
cp "$repo_root/scripts/release-versions.sh" "$apt_recipe_root/scripts/"
cp "$repo_root/test/deb-package-contract.sh" \
    "$repo_root/test/deb-maintscript-contract.sh" \
    "$repo_root/test/publish-directory-atomic.py" \
    "$repo_root/test/Containerfile.apt-client" \
    "$repo_root/test/apt-client-lane.sh" \
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
cat > "$tmp_root/bin/podman" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${FACELOCK_TEST_PODMAN_LOG:?}"
SH
chmod +x "$tmp_root/bin/podman"
apt_recipe_podman_log="$tmp_root/apt-recipe-podman.log"
if ! (
    cd "$apt_recipe_root"
    FACELOCK_TEST_PODMAN_LOG="$apt_recipe_podman_log" PATH="$tmp_root/bin:$PATH" just test-apt-repo >/dev/null 2>&1
); then
    fail "test-apt-repo rejected config-only validation without a manifest"
fi
grep -q -- '^build .*-t facelock-apt-client ' "$apt_recipe_podman_log" \
    || fail "test-apt-repo did not build the APT client lane image"
grep -q -- '^run .*--network=none .*facelock-apt-client ' "$apt_recipe_podman_log" \
    || fail "test-apt-repo did not run the APT client lane offline"
! grep -q -- '/manifests/' "$apt_recipe_podman_log" \
    || fail "test-apt-repo mounted a manifest it was not given"
rm -f "$apt_recipe_podman_log"
if ! (
    cd "$apt_recipe_root"
    FACELOCK_TEST_PODMAN_LOG="$apt_recipe_podman_log" PATH="$tmp_root/bin:$PATH" just test-apt-repo "${apt_recipe_manifests[@]}" >/dev/null 2>&1
); then
    fail "test-apt-repo rejected the complete set of two exact generated manifests"
fi
for suite in trixie resolute; do
    grep -q -- "^run .*:/manifests/$suite:ro.* --manifest $suite=/manifests/$suite/facelock_0.2.0-1${apt_recipe_suffix[$suite]}_amd64.manifest" "$apt_recipe_podman_log" \
        || fail "test-apt-repo did not hand the exact $suite manifest to the APT client lane"
done
rm -f "$apt_recipe_podman_log"
if (
    cd "$apt_recipe_root"
    FACELOCK_TEST_PODMAN_LOG="$apt_recipe_podman_log" PATH="$tmp_root/bin:$PATH" just test-apt-repo "${apt_recipe_manifests[0]}" >/dev/null 2>&1
); then
    fail "test-apt-repo accepted an incomplete generated-manifest set"
fi
[ ! -e "$apt_recipe_podman_log" ] || fail "test-apt-repo reached the container with an incomplete manifest set"

# Packaging matrix evidence (#313). The recipes and runners run for real; only
# the containers are answered for by a stub podman, and the stages that build
# images or run this very contract are stubbed to succeed. The stub keys the
# model-dependent branch off what the runner mounted, as the real images do.
evidence_root="$tmp_root/evidence-root"
mkdir -p "$evidence_root/dist" "$evidence_root/test" "$evidence_root/models" "$evidence_root/target/release" "$evidence_root/.github/workflows"
cp "$repo_root/justfile" "$evidence_root/"
cp "$repo_root/.github/workflows/packaging.yml" "$evidence_root/.github/workflows/"
cp "$repo_root/dist/release-matrix.json" "$evidence_root/dist/"
cp "$repo_root/test/packaging-evidence.py" \
    "$repo_root/test/run-pkg-validate-systemd.sh" \
    "$repo_root/test/run-rpm-smoke-systemd.sh" \
    "$evidence_root/test/"
cat >"$evidence_root/test/build-deb-package-image.sh" <<'SH'
#!/usr/bin/env bash
install -m 0444 /dev/null "${3:?}"
SH
cat >"$evidence_root/test/build-arch-package-image.sh" <<'SH'
#!/usr/bin/env bash
mkdir -p "${2:?}/source"
SH
cat >"$evidence_root/test/fedora-lane-image.sh" <<'SH'
#!/usr/bin/env bash
printf 'stub-fedora:%s\n' "${1:?}"
SH
printf '#!/usr/bin/env bash\nexit 0\n' >"$evidence_root/test/release-version-contract.sh"
printf 'raise SystemExit(0)\n' >"$evidence_root/test/check-release-matrix.py"
chmod +x "$evidence_root"/test/*.sh
# A required model no candidate source holds keeps `_link-models auto` from
# quietly refilling models/ from a host install during the opt-out case.
cat >"$evidence_root/models/manifest.toml" <<'TOML'
[[models]]
filename = "fixture-only.onnx"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
TOML
printf '%s\n' '/target' '*.onnx' '/.packaging-matrix-verified' '/.packaging-evidence/' >"$evidence_root/.gitignore"
touch "$evidence_root/target/release/facelock" "$evidence_root/target/release/facelock-polkit-agent" "$evidence_root/target/release/libpam_facelock.so"
touch "$evidence_root/models/scrfd_2.5g_bnkps.onnx" "$evidence_root/models/w600k_r50.onnx"
git -C "$evidence_root" init -q
git -C "$evidence_root" config user.name evidence-test
git -C "$evidence_root" config user.email evidence-test@example.invalid
git -C "$evidence_root" add .
git -C "$evidence_root" -c commit.gpgsign=false commit -qm baseline
evidence_head="$(git -C "$evidence_root" rev-parse HEAD)"
cat >"$tmp_root/bin/podman" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
    build|rmi|rm) exit 0 ;;
    run)
        shift
        detached=0
        models=nomodels
        image=""
        out=""
        while [ $# -gt 0 ]; do
            case "$1" in
                -d) detached=1; shift ;;
                -v)
                    case "$2" in
                        *:/facelock-test-models:*) models=models ;;
                        *:/out:*) out="${2%%:*}" ;;
                    esac
                    shift 2 ;;
                # Two-token options, or the value is read as the image name.
                -e|--security-opt|-w) shift 2 ;;
                -*) shift ;;
                *) image="$1"; break ;;
            esac
        done
        if [ "$detached" = 1 ]; then
            printf 'stub-%s-%s\n' "$image" "$models"
            exit 0
        fi
        case "$image" in
            facelock-arch-pkg-*)
                printf 'RESULTS_JSON: {"pass":29,"fail":0,"skip":0,"allowed_skip":0,"mandatory_skip":0,"models_present":true}\n' ;;
            facelock-copr-test-*)
                # The mock source rebuild hands its RPM back through /out.
                [ -n "$out" ] || { echo "copr build with no /out mount" >&2; exit 1; }
                : >"$out/facelock.rpm" ;;
        esac
        exit 0 ;;
    exec)
        shift
        opt_out=0
        while [ $# -gt 0 ]; do
            case "$1" in
                -e)
                    case "$2" in FACELOCK_ALLOW_MISSING_MODELS=1) opt_out=1 ;; esac
                    shift 2 ;;
                -*) shift ;;
                *) break ;;
            esac
        done
        cid="$1"
        shift
        case "$cid" in stub-facelock-deb-*) format=deb ;; *) format=rpm ;; esac
        case "$cid" in *-models) models=1 ;; *) models=0 ;; esac
        case "${1:-}" in
            systemctl) echo running ;;
            test)
                case "${2:-} ${3:-}" in
                    "-x /deb-package-lifecycle.sh") [ "$format" = deb ] ;;
                    "-x /rpm-service-pam-lifecycle.sh") [ "$format" = rpm ] ;;
                    # Both RPM images carry the config-upgrade stage. The
                    # override drops it from the COPR image so the runner's
                    # depth downgrade can be exercised.
                    "-x /rpm-config-lifecycle.sh")
                        case "$cid" in
                            stub-facelock-copr-*)
                                [ "${STUB_COPR_WITHOUT_CONFIG_LIFECYCLE:-0}" != 1 ] ;;
                            *) [ "$format" = rpm ] ;;
                        esac ;;
                    *) exit 1 ;;
                esac ;;
            /pkg-validate.sh)
                if [ "${STUB_PKG_VALIDATE_MANDATORY_SKIP:-0}" = 1 ]; then
                    printf 'RESULTS_JSON: {"pass":39,"fail":0,"skip":1,"allowed_skip":0,"mandatory_skip":1,"models_present":true}\n'
                elif [ "$models" = 1 ]; then
                    printf 'RESULTS_JSON: {"pass":40,"fail":0,"skip":0,"allowed_skip":0,"mandatory_skip":0,"models_present":true}\n'
                elif [ "$opt_out" = 1 ]; then
                    printf 'RESULTS_JSON: {"pass":37,"fail":0,"skip":3,"allowed_skip":3,"mandatory_skip":0,"models_present":false}\n'
                else
                    printf 'RESULTS_JSON: {"pass":37,"fail":1,"skip":0,"allowed_skip":0,"mandatory_skip":0,"models_present":false}\n'
                    exit 1
                fi ;;
            /rpm-runtime-smoke.sh)
                printf 'RESULTS_JSON: {"pass":8,"fail":0,"skip":0,"allowed_skip":0,"mandatory_skip":0,"models_present":true}\n' ;;
            /rpm-config-lifecycle.sh)
                if [ "${STUB_RPM_CONFIG_LIFECYCLE_FAIL:-0}" = 1 ]; then exit 1; fi ;;
        esac
        exit 0 ;;
    *) exit 2 ;;
esac
SH
chmod +x "$tmp_root/bin/podman"
run_packaging_matrix() {
    (
        cd "$evidence_root"
        PATH="$tmp_root/bin:$PATH" env FACELOCK_RELEASE_BINARIES_PREBUILT=1 "$@" just test-packaging-matrix
    )
}
evidence_validate() {
    python3 "$evidence_root/test/packaging-evidence.py" validate --commit "$evidence_head" "$@"
}

# A complete run replaces whatever marker and lane records were there before
# it started, and writes evidence preflight accepts.
printf '%s\n' "$evidence_head" >"$evidence_root/.packaging-matrix-verified"
mkdir -p "$evidence_root/.packaging-evidence"
printf '%s\n' '{"stale": true}' >"$evidence_root/.packaging-evidence/stale-lane.json"
run_packaging_matrix >"$tmp_root/evidence-complete.log" 2>&1 ||
    fail "test-packaging-matrix refused a complete run: $(cat "$tmp_root/evidence-complete.log")"
[ ! -e "$evidence_root/.packaging-evidence/stale-lane.json" ] ||
    fail "test-packaging-matrix kept a lane record from before the run"
validate_output=$(evidence_validate "$evidence_root/.packaging-matrix-verified" 2>&1) ||
    fail "release preflight refused the marker a complete matrix run wrote: $validate_output"
case "$validate_output" in
    *"9 lanes"*) ;;
    *) fail "validate accepted the marker without summarising it: $validate_output" ;;
esac
python3 - "$evidence_root/.packaging-matrix-verified" "$evidence_head" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    marker = json.load(handle)
assert marker["schema"] == 1, marker
assert marker["commit"] == sys.argv[2], marker["commit"]
assert marker["tree_clean"] is True
assert marker["started_at"] and marker["finished_at"] and marker["started_at"] <= marker["finished_at"]
expected = [
    "test-arch-pkg",
    "test-copr-pkg-43",
    "test-copr-pkg-44",
    "test-copr-smoke-45",
    "test-deb-resolute-pkg",
    "test-deb-trixie-pkg",
    "test-rpm-pkg-43",
    "test-rpm-pkg-44",
    "test-rpm-smoke-45",
]
assert sorted(marker["required_lanes"]) == expected, marker["required_lanes"]
lanes = {lane["name"]: lane for lane in marker["lanes"]}
assert sorted(lanes) == expected, sorted(lanes)
for lane in lanes.values():
    assert lane["status"] == "pass" and lane["models_present"] is True, lane
    assert lane["fail"] == lane["skip"] == lane["allowed_skip"] == lane["mandatory_skip"] == 0, lane
    assert lane["pass"] > 0 and lane["commit"] == sys.argv[2], lane
assert lanes["test-deb-trixie-pkg"]["target"] == "debian-trixie"
assert lanes["test-deb-trixie-pkg"]["channel"] == "apt"
assert lanes["test-deb-resolute-pkg"]["target"] == "ubuntu-resolute"
assert lanes["test-rpm-pkg-43"]["channel"] == "direct-rpm"
assert lanes["test-rpm-pkg-43"]["build_origin"] == "host-binaries"
assert lanes["test-rpm-pkg-43"]["runtime_policy"] == "bundled-ort"
assert lanes["test-rpm-smoke-45"]["depth"] == "smoke"
assert lanes["test-arch-pkg"]["channel"] == "aur"
assert lanes["test-arch-pkg"]["runtime_policy"] == "system-ort"
# #230: every declared Packit/COPR target carries its own record, built from
# source in mock and run against Fedora's system ONNX Runtime. Same target as
# the direct-RPM lane beside it, different channel, origin and runtime.
for release in ("43", "44"):
    copr = lanes[f"test-copr-pkg-{release}"]
    assert copr["target"] == f"fedora-{release}", copr
    assert copr["channel"] == "copr", copr
    assert copr["build_origin"] == "mock-source-rebuild", copr
    assert copr["runtime_policy"] == "system-ort", copr
    assert copr["depth"] == "full", copr
    assert lanes[f"test-rpm-pkg-{release}"]["target"] == copr["target"], release
assert lanes["test-copr-smoke-45"]["channel"] == "copr"
assert lanes["test-copr-smoke-45"]["depth"] == "smoke"
PY
cp "$evidence_root/.packaging-matrix-verified" "$tmp_root/evidence-good.json"
cp -R "$evidence_root/.packaging-evidence" "$tmp_root/evidence-good-records"
rm -f "$tmp_root/evidence-good-records/started-at"

# The model opt-out still runs, for local diagnostics, but the partial records
# it leaves are refused and no marker is written.
rm "$evidence_root"/models/*.onnx
if run_packaging_matrix FACELOCK_ALLOW_MISSING_MODELS=1 >"$tmp_root/evidence-optout.log" 2>&1; then
    fail "test-packaging-matrix recorded a FACELOCK_ALLOW_MISSING_MODELS=1 run"
fi
[ ! -e "$evidence_root/.packaging-matrix-verified" ] ||
    fail "the opt-out run left a marker behind"
grep -q 'test-deb-trixie-pkg' "$tmp_root/evidence-optout.log" ||
    fail "the opt-out refusal did not name the partial lane: $(cat "$tmp_root/evidence-optout.log")"
grep -q 'models_present' "$tmp_root/evidence-optout.log" ||
    fail "the opt-out refusal did not name the missing models: $(cat "$tmp_root/evidence-optout.log")"
python3 - "$evidence_root/.packaging-evidence" <<'PY'
import json
import sys
from pathlib import Path

records = {path.stem: json.loads(path.read_text()) for path in Path(sys.argv[1]).glob("*.json")}
trixie = records["test-deb-trixie-pkg"]
# Three pkg-validate.sh skips plus the runner's own active-upgrade skip.
assert trixie["models_present"] is False and trixie["status"] == "partial", trixie
assert trixie["skip"] == trixie["allowed_skip"] == 4, trixie
fedora = records["test-rpm-pkg-43"]
assert fedora["models_present"] is False and fedora["skip"] == fedora["allowed_skip"] == 3, fedora
assert records["test-arch-pkg"]["status"] == "pass"
PY
touch "$evidence_root/models/scrfd_2.5g_bnkps.onnx" "$evidence_root/models/w600k_r50.onnx"

# A lane that exits 0 while reporting a mandatory skip is refused by the
# aggregate itself, not only by its own exit status.
if run_packaging_matrix STUB_PKG_VALIDATE_MANDATORY_SKIP=1 >"$tmp_root/evidence-mandatory.log" 2>&1; then
    fail "test-packaging-matrix recorded a run with a mandatory skip"
fi
[ ! -e "$evidence_root/.packaging-matrix-verified" ] ||
    fail "the mandatory-skip run left a marker behind"
grep -q 'mandatory_skip' "$tmp_root/evidence-mandatory.log" ||
    fail "the mandatory-skip refusal did not name the skip class: $(cat "$tmp_root/evidence-mandatory.log")"

# A stage after pkg-validate.sh that fails fails the lane, and its record says
# so: the record is written last, so it covers the whole runner.
if run_packaging_matrix STUB_RPM_CONFIG_LIFECYCLE_FAIL=1 >"$tmp_root/evidence-late-stage.log" 2>&1; then
    fail "test-packaging-matrix recorded a run whose config lifecycle stage failed"
fi
[ ! -e "$evidence_root/.packaging-matrix-verified" ] ||
    fail "the failed config-lifecycle run left a marker behind"
python3 - "$evidence_root/.packaging-evidence/test-rpm-pkg-43.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    lane = json.load(handle)
assert lane["status"] == "fail", lane
PY

# A lane whose image cannot run every stage its declared depth names records
# what it actually ran. `partial` is a depth the release matrix requires of
# nothing, so the aggregate refuses it rather than accepting a short lifecycle
# as a full one (#230).
if run_packaging_matrix STUB_COPR_WITHOUT_CONFIG_LIFECYCLE=1 >"$tmp_root/evidence-short-depth.log" 2>&1; then
    fail "test-packaging-matrix recorded a COPR lane that skipped the config lifecycle"
fi
[ ! -e "$evidence_root/.packaging-matrix-verified" ] ||
    fail "the short-depth run left a marker behind"
grep -q 'rpm-config-lifecycle.sh' "$tmp_root/evidence-short-depth.log" ||
    fail "the short-depth run did not name the stage it could not run: $(cat "$tmp_root/evidence-short-depth.log")"
grep -q "depth is 'partial'" "$tmp_root/evidence-short-depth.log" ||
    fail "the short-depth refusal did not name the depth: $(cat "$tmp_root/evidence-short-depth.log")"
python3 - "$evidence_root/.packaging-evidence/test-copr-pkg-43.json" <<'DEPTH'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    lane = json.load(handle)
assert lane["depth"] == "partial", lane
DEPTH

evidence_case_index=0
assert_evidence_refused() {
    local context="$1"
    local mutation="$2"
    local expected="$3"
    local marker="$tmp_root/evidence-case-$evidence_case_index.json"
    local output
    evidence_case_index=$((evidence_case_index + 1))
    python3 - "$tmp_root/evidence-good.json" "$marker" "$mutation" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    marker = json.load(handle)
lanes = {lane["name"]: lane for lane in marker["lanes"]}
exec(sys.argv[3])
with open(sys.argv[2], "w", encoding="utf-8") as handle:
    json.dump(marker, handle)
PY
    if output=$(evidence_validate "$marker" 2>&1); then
        fail "packaging evidence accepted: $context"
    fi
    case "$output" in
        *"$expected"*) ;;
        *) fail "packaging evidence refusal for '$context' did not say '$expected': $output" ;;
    esac
    echo "packaging evidence case: $context refused"
}
assert_evidence_refused "stale commit" \
    'marker["commit"] = "0" * 40' "commit"
assert_evidence_refused "interrupted run without finished_at" \
    'del marker["finished_at"]' "finished_at"
assert_evidence_refused "missing required lane record" \
    'marker["lanes"] = [lane for lane in marker["lanes"] if lane["name"] != "test-rpm-pkg-44"]' "test-rpm-pkg-44"
assert_evidence_refused "required lane set narrowed" \
    'marker["required_lanes"].remove("test-rpm-pkg-44")' "required lane set"
assert_evidence_refused "allowed skip" \
    'lanes["test-deb-trixie-pkg"].update(skip=1, allowed_skip=1)' "skip"
assert_evidence_refused "mandatory skip" \
    'lanes["test-rpm-pkg-43"].update(skip=1, mandatory_skip=1)' "mandatory_skip"
assert_evidence_refused "models absent" \
    'lanes["test-deb-resolute-pkg"]["models_present"] = False' "models_present"
assert_evidence_refused "partial status" \
    'lanes["test-deb-resolute-pkg"]["status"] = "partial"' "status"
assert_evidence_refused "failed assertion" \
    'lanes["test-arch-pkg"]["fail"] = 1' "fail"
assert_evidence_refused "lane that asserted nothing" \
    'lanes["test-arch-pkg"]["pass"] = 0' "no assertion"
assert_evidence_refused "lane record from another commit" \
    'lanes["test-rpm-smoke-45"]["commit"] = "0" * 40' "commit"
assert_evidence_refused "direct RPM record claiming another channel" \
    'lanes["test-rpm-pkg-43"]["channel"] = "copr"' "channel"
# #230's core refusal: the direct-RPM lanes prove a bundled-runtime package
# built from host binaries, so their success can never stand in for the COPR
# delivery path the release matrix declares for the same Fedora target.
assert_evidence_refused "direct RPM record offered as the COPR lane" \
    'marker["lanes"] = [lane for lane in marker["lanes"] if lane["name"] != "test-copr-pkg-44"] + [{**lanes["test-rpm-pkg-44"], "name": "test-copr-pkg-44"}]' \
    "channel"
assert_evidence_refused "COPR record built from host binaries" \
    'lanes["test-copr-pkg-43"]["build_origin"] = "host-binaries"' "build_origin"
assert_evidence_refused "COPR record run against a bundled runtime" \
    'lanes["test-copr-pkg-43"]["runtime_policy"] = "bundled-ort"' "runtime_policy"
assert_evidence_refused "missing COPR lane record" \
    'marker["lanes"] = [lane for lane in marker["lanes"] if lane["name"] != "test-copr-smoke-45"]' \
    "test-copr-smoke-45"
assert_evidence_refused "lane depth downgraded" \
    'lanes["test-rpm-pkg-44"]["depth"] = "smoke"' "depth"
assert_evidence_refused "duplicate lane records" \
    'marker["lanes"].append(dict(lanes["test-arch-pkg"]))' "more than once"
assert_evidence_refused "dirty tree" \
    'marker["tree_clean"] = False' "tree_clean"
assert_evidence_refused "unknown schema" \
    'marker["schema"] = 2' "schema"
assert_evidence_refused "skip count that does not add up" \
    'lanes["test-deb-trixie-pkg"]["skip"] = 2' "skip"
assert_evidence_refused "record without a schema" \
    'del lanes["test-arch-pkg"]["schema"]' "schema"
assert_evidence_refused "record with another schema" \
    'lanes["test-arch-pkg"]["schema"] = 2' "schema"
assert_evidence_refused "record with schema true" \
    'lanes["test-arch-pkg"]["schema"] = True' "schema"
assert_evidence_refused "record with schema 1.0" \
    'lanes["test-arch-pkg"]["schema"] = 1.0' "schema"
assert_evidence_refused "marker with schema true" \
    'marker["schema"] = True' "schema"
assert_evidence_refused "marker with schema 1.0" \
    'marker["schema"] = 1.0' "schema"
assert_evidence_refused "required lane listed twice" \
    'marker["required_lanes"].append("test-arch-pkg")' "required lane set"
assert_evidence_refused "record for a lane the matrix does not require" \
    'marker["lanes"].append({**lanes["test-arch-pkg"], "name": "test-extra-lane"})' "not a lane the release matrix requires"
assert_evidence_refused "finished before it started" \
    'marker["finished_at"] = "2020-01-01T00:00:00Z"' "before started_at"
assert_evidence_refused "timestamp that is not ISO 8601" \
    'marker["started_at"] = "yesterday"' "ISO 8601"
assert_evidence_refused "timestamp without an offset" \
    'marker["started_at"] = marker["started_at"].rstrip("Z")' "UTC offset"

# Outside a git checkout, HEAD cannot be resolved; that is a refusal, not a
# traceback.
nogit_root="$tmp_root/nogit"
mkdir -p "$nogit_root/test" "$nogit_root/dist"
cp "$repo_root/test/packaging-evidence.py" "$nogit_root/test/"
cp "$repo_root/dist/release-matrix.json" "$nogit_root/dist/"
if output=$(cd / && python3 "$nogit_root/test/packaging-evidence.py" validate "$tmp_root/evidence-good.json" 2>&1); then
    fail "packaging evidence resolved HEAD outside a git checkout"
fi
case "$output" in
    *Traceback*) fail "packaging evidence crashed outside a git checkout: $output" ;;
    *"cannot resolve HEAD"*) ;;
    *) fail "packaging evidence did not say HEAD is unresolvable: $output" ;;
esac

# A real APT suite missing platform_id (not the compat block, which carries
# none) is refused with a diagnostic, not a KeyError (#320 added `compat`
# alongside the real suites).
platform_id_root="$tmp_root/platform-id-root"
mkdir -p "$platform_id_root/test" "$platform_id_root/dist"
cp "$repo_root/test/packaging-evidence.py" "$platform_id_root/test/"
python3 - "$repo_root/dist/release-matrix.json" "$platform_id_root/dist/release-matrix.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    matrix = json.load(handle)
del matrix["apt_suites"]["trixie"]["platform_id"]
with open(sys.argv[2], "w", encoding="utf-8") as handle:
    json.dump(matrix, handle)
PY
if output=$(python3 "$platform_id_root/test/packaging-evidence.py" validate --commit "$evidence_head" "$tmp_root/evidence-good.json" 2>&1); then
    fail "packaging evidence accepted a release matrix APT suite missing platform_id"
fi
case "$output" in
    *Traceback*) fail "packaging evidence crashed on an APT suite missing platform_id: $output" ;;
    *"trixie has no platform_id"*) ;;
    *) fail "packaging evidence did not name the APT suite missing platform_id: $output" ;;
esac
echo "packaging evidence case: APT suite missing platform_id refused"

if output=$(evidence_validate "$tmp_root/evidence-missing.json" 2>&1); then
    fail "packaging evidence accepted a missing marker"
fi
case "$output" in
    *"no packaging evidence"*) ;;
    *) fail "missing marker refusal did not say so: $output" ;;
esac
printf '%s\n' '{"schema": 1, "commit": ' >"$tmp_root/evidence-malformed.json"
if output=$(evidence_validate "$tmp_root/evidence-malformed.json" 2>&1); then
    fail "packaging evidence accepted malformed JSON"
fi
case "$output" in
    *"not JSON"*) ;;
    *) fail "malformed marker refusal did not say so: $output" ;;
esac
printf '%s\n' "$evidence_head" >"$tmp_root/evidence-legacy.json"
if output=$(evidence_validate "$tmp_root/evidence-legacy.json" 2>&1); then
    fail "packaging evidence accepted the legacy one-line marker"
fi
case "$output" in
    *"legacy"*"schema 1"*|*"schema 1"*"legacy"*) ;;
    *) fail "legacy marker refusal did not name the new format: $output" ;;
esac

# Aggregation from lane records alone, as the CI path does: a record short is a
# missing lane, never a smaller lane set.
partial_records="$tmp_root/evidence-partial-records"
cp -R "$tmp_root/evidence-good-records" "$partial_records"
rm "$partial_records/test-deb-resolute-pkg.json"
if output=$(python3 "$evidence_root/test/packaging-evidence.py" aggregate \
        --commit "$evidence_head" --evidence-dir "$partial_records" --tree-clean \
        --started-at 2026-09-01T00:00:00Z --output "$tmp_root/evidence-partial.json" 2>&1); then
    fail "packaging evidence aggregated a lane set with a record missing"
fi
[ ! -e "$tmp_root/evidence-partial.json" ] || fail "a refused aggregate still wrote a marker"
case "$output" in
    *"test-deb-resolute-pkg"*) ;;
    *) fail "aggregate refusal did not name the missing lane: $output" ;;
esac

# The CI path: a successful packaging.yml run is evidence only when its
# evidence artifacts aggregate into an accepted marker for this commit.
cat >"$tmp_root/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
    "run view")
        printf '%s\n' "${STUB_GH_RUN_VIEW:?}" ;;
    "run download")
        [ "${STUB_GH_DOWNLOAD_FAIL:-0}" != 1 ] || { echo "HTTP 401: Bad credentials (https://api.github.com/repos/x/y/actions/runs/1/artifacts)" >&2; exit 1; }
        [ -n "${STUB_GH_ARTIFACTS:-}" ] || { echo "no artifacts match any of the names or patterns provided" >&2; exit 1; }
        dir=""
        while [ $# -gt 0 ]; do
            case "$1" in --dir) dir="$2"; shift 2 ;; *) shift ;; esac
        done
        for record in "$STUB_GH_ARTIFACTS"/*.json; do
            name="$(basename "$record" .json)"
            mkdir -p "$dir/packaging-evidence-$name"
            cp "$record" "$dir/packaging-evidence-$name/"
        done ;;
    *) exit 2 ;;
esac
SH
chmod +x "$tmp_root/bin/gh"
ci_run_view="{\"conclusion\":\"success\",\"event\":\"workflow_dispatch\",\"workflowName\":\"Packaging\",\"headSha\":\"$evidence_head\",\"createdAt\":\"2026-09-01T07:00:00Z\",\"updatedAt\":\"2026-09-01T08:00:00Z\"}"
PATH="$tmp_root/bin:$PATH" STUB_GH_RUN_VIEW="$ci_run_view" STUB_GH_ARTIFACTS="$tmp_root/evidence-good-records" \
    python3 "$evidence_root/test/packaging-evidence.py" ci-run --commit "$evidence_head" --run 12345 ||
    fail "release preflight refused a packaging.yml run carrying complete evidence"
if output=$(PATH="$tmp_root/bin:$PATH" STUB_GH_RUN_VIEW="$ci_run_view" \
        python3 "$evidence_root/test/packaging-evidence.py" ci-run --commit "$evidence_head" --run 12345 2>&1); then
    fail "release preflight accepted a packaging.yml run without evidence artifacts"
fi
case "$output" in
    *"no packaging evidence artifact"*) ;;
    *) fail "artifact-less run refusal did not say so: $output" ;;
esac
if PATH="$tmp_root/bin:$PATH" STUB_GH_RUN_VIEW="$ci_run_view" STUB_GH_ARTIFACTS="$partial_records" \
        python3 "$evidence_root/test/packaging-evidence.py" ci-run --commit "$evidence_head" --run 12345 >/dev/null 2>&1; then
    fail "release preflight accepted a packaging.yml run missing a lane artifact"
fi
if PATH="$tmp_root/bin:$PATH" STUB_GH_RUN_VIEW="${ci_run_view/success/failure}" STUB_GH_ARTIFACTS="$tmp_root/evidence-good-records" \
        python3 "$evidence_root/test/packaging-evidence.py" ci-run --commit "$evidence_head" --run 12345 >/dev/null 2>&1; then
    fail "release preflight accepted a failed packaging.yml run"
fi
if PATH="$tmp_root/bin:$PATH" STUB_GH_RUN_VIEW="${ci_run_view/$evidence_head/$(printf '0%.0s' $(seq 40))}" STUB_GH_ARTIFACTS="$tmp_root/evidence-good-records" \
        python3 "$evidence_root/test/packaging-evidence.py" ci-run --commit "$evidence_head" --run 12345 >/dev/null 2>&1; then
    fail "release preflight accepted a packaging.yml run at another commit"
fi
if output=$(PATH="$tmp_root/bin:$PATH" STUB_GH_RUN_VIEW="${ci_run_view/workflow_dispatch/pull_request}" STUB_GH_ARTIFACTS="$tmp_root/evidence-good-records" \
        python3 "$evidence_root/test/packaging-evidence.py" ci-run --commit "$evidence_head" --run 12345 2>&1); then
    fail "release preflight accepted a pull-request packaging.yml run"
fi
case "$output" in
    *"pull-request runs build the merge commit"*) ;;
    *) fail "pull-request run refusal did not explain the merge commit: $output" ;;
esac
if output=$(PATH="$tmp_root/bin:$PATH" STUB_GH_RUN_VIEW="${ci_run_view/Packaging/CI}" STUB_GH_ARTIFACTS="$tmp_root/evidence-good-records" \
        python3 "$evidence_root/test/packaging-evidence.py" ci-run --commit "$evidence_head" --run 12345 2>&1); then
    fail "release preflight accepted a run of another workflow as packaging evidence"
fi
case "$output" in
    *"workflow is 'CI'"*) ;;
    *) fail "other-workflow refusal did not name the workflow: $output" ;;
esac
if output=$(PATH="$tmp_root/bin:$PATH" STUB_GH_RUN_VIEW="$ci_run_view" STUB_GH_DOWNLOAD_FAIL=1 \
        python3 "$evidence_root/test/packaging-evidence.py" ci-run --commit "$evidence_head" --run 12345 2>&1); then
    fail "release preflight accepted a run whose artifacts could not be downloaded"
fi
case "$output" in
    *"gh run download failed"*"Bad credentials"*) ;;
    *) fail "download failure was not distinguished from a missing artifact: $output" ;;
esac
echo "packaging evidence: recipe, marker, and workflow-run cases OK"

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
# The release bump must leave the source digest placeholder for publish-aur.sh
# to finalize — never SKIP, never a stale digest for the old tag (#283).
assert_file_line "$release_repo/dist/PKGBUILD" "sha256sums=('__SRC_SHA256__')"
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
