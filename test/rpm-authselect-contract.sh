#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
spec="$repo_root/dist/facelock.spec"
guard="$repo_root/dist/rpm/facelock-authselect-retirement-guard"
profile_root="$repo_root/dist/authselect/facelock"
fixture="$repo_root/test/Containerfile.rpm-authselect"
fixture_builder="$repo_root/test/build-rpm-authselect-fixtures.sh"
package_validator="$repo_root/test/pkg-validate.sh"
package_fixture="$repo_root/test/Containerfile.rpm-e2e"
package_runner="$repo_root/test/run-pkg-validate-systemd.sh"
service_lifecycle="$repo_root/test/rpm-service-pam-lifecycle.sh"
rpm_validator="$repo_root/.github/workflows/scripts/validate-rpm.sh"
contracts="$repo_root/docs/contracts.md"
security="$repo_root/docs/security.md"
cli="$repo_root/docs/cli.md"
releasing="$repo_root/docs/releasing.md"
changelog="$repo_root/CHANGELOG.md"
packit_source_contract="$repo_root/test/packit-srpm-source-contract.py"
artifact_contract="$repo_root/test/rpm-authselect-artifact-contract.sh"
failures=0

fail() {
    echo "FAIL: $*" >&2
    failures=$((failures + 1))
}

require_file() {
    local path="$1"
    [ -f "$path" ] || fail "missing file: ${path#"$repo_root/"}"
}

reject_path() {
    local path="$1"
    [ ! -e "$path" ] && [ ! -L "$path" ] || \
        fail "retired path remains: ${path#"$repo_root/"}"
}

require_text() {
    local path="$1"
    local expected="$2"
    grep -Fq -- "$expected" "$path" || \
        fail "${path#"$repo_root/"} must contain: $expected"
}

reject_text() {
    local path="$1"
    local forbidden="$2"
    if grep -Fq -- "$forbidden" "$path"; then
        fail "${path#"$repo_root/"} must not contain: $forbidden"
    fi
}

require_file "$guard"
require_file "$service_lifecycle"
require_file "$packit_source_contract"
require_file "$artifact_contract"
reject_path "$profile_root"
reject_path "$repo_root/dist/rpm/facelock-authselect-migrate"
reject_path "$repo_root/dist/rpm/facelock-authselect-known-profiles"
reject_path "$repo_root/test/rpm-authselect/authselect-fault-shim"
reject_path "$repo_root/test/rpm-authselect/legacy-README"

require_text "$fixture" \
    "https://github.com/tyvsmith/facelock/releases/download/v0.1.4/facelock-0.1.4-1.fc44.x86_64.rpm"
require_text "$fixture" \
    "e8d3858adbf001676cc1d25171702c396e6ed22dd2a8c4f0d064a8c2febb3a0b"
require_text "$fixture" \
    "COPY test/rpm-authselect-artifact-contract.sh /build/test/rpm-authselect-artifact-contract.sh"
# These variables are literal fixture source text.
# shellcheck disable=SC2016
require_text "$fixture_builder" \
    'FACELOCK_TEST_RPM="$artifacts/facelock-new.rpm"'
require_text "$fixture_builder" \
    "test/rpm-authselect-artifact-contract.sh"
reject_text "$fixture_builder" "rpm -qpl"
reject_text "$fixture_builder" "rpm -qp --requires"
reject_text "$fixture_builder" "rpm -qp --scripts"
reject_text "$fixture_builder" "new.scripts"
reject_text "$fixture_builder" "new.requires"
require_text "$service_lifecycle" \
    "service-scoped PAM setup leaves authselect selection unchanged"
require_text "$service_lifecycle" \
    "service-scoped PAM setup leaves shared authselect PAM files unchanged"
require_text "$service_lifecycle" \
    "vendor-only leaf setup leaves the vendor service unchanged"
require_text "$service_lifecycle" \
    "vendor-only leaf removal retires the unchanged Facelock override"
require_text "$service_lifecycle" \
    "outbound PAM service symlink is refused"
require_text "$service_lifecycle" \
    "correct password falls through after Facelock rejection"
require_text "$service_lifecycle" \
    "service-scoped PAM removal preserves password success and rejection"
# The variable is literal source text required in the lifecycle assertion.
# shellcheck disable=SC2016
require_text "$repo_root/test/rpm-authselect-lifecycle.sh" \
    'rejected $label upgrade mutated authselect state or legacy profile identity'
# The command substitution is literal lifecycle source text.
# shellcheck disable=SC2016
require_text "$repo_root/test/rpm-authselect-lifecycle.sh" \
    'assert_eq facelock "$(head -n 1 /etc/authselect/authselect.conf)"'
require_text "$repo_root/test/rpm-authselect-lifecycle.sh" \
    "the retired Facelock authselect profile is still selected"
require_text "$package_fixture" \
    "COPY test/rpm-service-pam-lifecycle.sh /rpm-service-pam-lifecycle.sh"
require_text "$package_fixture" \
    "COPY .github/workflows/scripts/validate-rpm.sh /validate-rpm.sh"
# The variable is literal Containerfile source text.
# shellcheck disable=SC2016
require_text "$package_fixture" '/validate-rpm.sh "$RPM_FILE" direct'
require_text "$package_fixture" "authselect"
require_text "$package_runner" "/rpm-service-pam-lifecycle.sh"
reject_text "$package_validator" "facelock-rpm-service"
reject_text "$rpm_validator" "authselect/vendor/facelock"
require_text "$rpm_validator" "RPM depends on retired authselect"
require_text "$contracts" \
    "The RPM does not ship or select an authselect profile"
require_text "$contracts" \
    "custom/facelock"
require_text "$contracts" \
    "#226 owns only RPM payload retirement and the read-only upgrade guard"
require_text "$contracts" \
    "Shared-stack migration, regeneration, editing and rollback are explicitly rejected"
require_text "$contracts" \
    "already-installed v0.1.4 RPM cannot be retroactively guarded"
require_text "$contracts" \
    "install a guarded release before a later uninstall"
require_text "$releasing" \
    "retired-profile upgrade guard"
require_text "$releasing" \
    "already-installed v0.1.4 RPM cannot be retroactively guarded"
require_text "$releasing" \
    "install a guarded release before a later uninstall"
require_text "$changelog" \
    "Retired the packaged Fedora authselect profile"
require_text "$changelog" \
    "already-installed v0.1.4 RPM cannot be retroactively guarded"
require_text "$changelog" \
    "install a guarded release before a later uninstall"
# The backticks are literal documentation text, not command substitution.
# shellcheck disable=SC2016
require_text "$security" \
    'The RPM retirement guard reads only `/etc/authselect/authselect.conf`'
require_text "$cli" \
    "Fedora RPMs support the same service-scoped leaf-file setup"
# The backticks are literal documentation text, not command substitution.
# shellcheck disable=SC2016
require_text "$cli" \
    'as `sudo`, `polkit-1`, or another explicit service'
require_text "$cli" \
    "pam add --service sshd --allow-sensitive"
# The backticks are literal documentation text, not command substitution.
# shellcheck disable=SC2016
reject_text "$cli" \
    'as `sudo`, `sshd`, or another explicit service'
reject_text "$contracts" "facelock-authselect-migrate"
reject_text "$contracts" "authselect-known-profiles"
reject_text "$contracts" \
    "authselect profile selection, regeneration and rollback are #226 scope"
reject_text "$releasing" "facelock-authselect-migrate"
reject_text "$changelog" "Safe migration for a selected legacy authselect profile"

require_text "$spec" "Source1:        facelock-authselect-retirement-guard"
require_text "$spec" "Requires(pre):  coreutils"
require_text "$spec" "%pre -f %{SOURCE1}"
reject_text "$spec" "%pretrans"
reject_text "$spec" "Source2:"
reject_text "$spec" "Requires:       authselect"
reject_text "$spec" "Requires(pre):  authselect"
reject_text "$spec" "Requires(preun): authselect"
reject_text "$spec" "%undefine __brp_linkdupes"
reject_text "$spec" "authselect/vendor/facelock"
reject_text "$spec" "facelock-authselect-migrate"
reject_text "$spec" "facelock-authselect-known-profiles"

for builder in \
    "$repo_root/.github/workflows/scripts/build-rpm.sh" \
    "$repo_root/test/build-rpm-prebuilt.sh"; do
    require_text "$builder" "facelock-authselect-retirement-guard"
    reject_text "$builder" "facelock-authselect-migrate"
    reject_text "$builder" "facelock-authselect-known-profiles"
done
# The parameter expansion is literal builder source text.
# shellcheck disable=SC2016
require_text "$repo_root/test/build-rpm-prebuilt.sh" 'CHANNEL="${2:-direct}"'
require_text "$repo_root/test/build-rpm-authselect-fixtures.sh" \
    'bash test/build-rpm-prebuilt.sh 0.2.0 copr'
# The variables are literal fixture source text.
# shellcheck disable=SC2016
require_text "$repo_root/test/build-rpm-authselect-fixtures.sh" \
    '"$repo_root/.github/workflows/scripts/validate-rpm.sh" "$new_rpm" copr'
require_text "$repo_root/test/Containerfile.rpm-authselect" \
    'COPY .github/workflows/scripts/run-networkless.sh /run-networkless.sh'
require_text "$repo_root/test/Containerfile.rpm-authselect" "python3"
require_text "$repo_root/test/Containerfile.rpm-authselect" \
    '/run-networkless.sh /build/test/build-rpm-authselect-fixtures.sh'

if [ -f "$guard" ]; then
    require_text "$guard" "state=/etc/authselect/authselect.conf"
    require_text "$guard" "16384"
    require_text "$guard" "od -An -tx1 -v"
    # This is the literal guard source expression.
    # shellcheck disable=SC2016
    require_text "$guard" '"$profile" != facelock'
    reject_text "$guard" "/usr/bin/authselect"
    reject_text "$guard" "authselect current"
    reject_text "$guard" "FACELOCK_TEST"
    reject_text "$guard" "TEST_ROOT"
fi

if [ -n "${FACELOCK_TEST_RPM:-}" ]; then
    bash "$artifact_contract" || fail "new RPM artifact contract failed"
fi

if [ -f "$packit_source_contract" ]; then
    python3 "$packit_source_contract" || fail "Packit SRPM source staging contract failed"
fi

if [ "$failures" -ne 0 ]; then
    echo "rpm authselect retirement contract: $failures failure(s)" >&2
    exit 1
fi

echo "rpm authselect retirement contract: OK"
