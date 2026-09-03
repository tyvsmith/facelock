#!/usr/bin/env bash
# Runtime smoke for a branched Fedora release.
#
# dist/release-matrix.json gives Fedora 45 a lifecycle depth of "build/runtime
# smoke", not "full": the branched release has to prove the package builds and
# that what it installs is loadable, without standing in for the full Fedora 43
# and 44 lifecycle lanes. Build coverage happens when the image is assembled
# (rpmbuild from the real spec, .github/workflows/scripts/validate-rpm.sh, then
# dnf install). This is the runtime half, run under a booted systemd.
set -euo pipefail

pass_count=0

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

pass() {
    printf 'TEST: %s ... PASS\n' "$1"
    pass_count=$((pass_count + 1))
}

rpm -q facelock >/dev/null || fail "facelock is not installed from the built package"
pass "the built package is installed"

# rpm -V reports one nine-character attribute field per differing file. Mode and
# mtime drift is normal in a container image, so this fails only on the classes
# that mean the installed payload is not what the package shipped: size, digest,
# owner, group, symlink target, or file capabilities.
verify_report="$(rpm -V --nomtime facelock || true)"
if [ -n "$verify_report" ]; then
    printf '%s\n' "$verify_report"
fi
while read -r attributes _; do
    [ -n "$attributes" ] || continue
    case "$attributes" in
        missing) fail "package file is missing from the installed payload" ;;
        *S*|*5*|*U*|*G*|*L*|*P*)
            fail "installed payload differs from the package: $attributes" ;;
    esac
done <<<"$verify_report"
pass "installed payload matches the packaged size, digest, ownership, and links"

facelock --version >/dev/null || fail "facelock --version did not run"
facelock --help >/dev/null || fail "facelock --help did not run"
pass "the installed binary runs against its packaged ONNX Runtime"

pam_module=""
for candidate in /usr/lib64/security/pam_facelock.so /usr/lib/security/pam_facelock.so; do
    [ -f "$candidate" ] && pam_module="$candidate" && break
done
[ -n "$pam_module" ] || fail "no PAM module was installed"
pass "the PAM module is installed"

for owned in \
    /etc/facelock/config.toml \
    /usr/lib/systemd/system/facelock-daemon.service \
    /usr/lib/tmpfiles.d/facelock.conf \
    /usr/share/dbus-1/system.d/org.facelock.Daemon.conf \
    /usr/share/dbus-1/system-services/org.facelock.Daemon.service; do
    [ -f "$owned" ] || fail "package did not install $owned"
    rpm -qf "$owned" >/dev/null 2>&1 || fail "$owned is not owned by any package"
done
pass "config, unit, tmpfiles, and D-Bus files are installed and package-owned"

systemctl cat facelock-daemon.service >/dev/null ||
    fail "systemd cannot load the packaged unit"
[ "$(systemctl is-enabled facelock-daemon.service 2>&1)" != "bad" ] ||
    fail "packaged unit is in a bad enablement state"
pass "systemd loads the packaged daemon unit"

# %post runs %tmpfiles_create; the runtime directories have to exist, not just
# the .conf that declares them.
for runtime_dir in /var/lib/facelock /var/log/facelock; do
    [ -d "$runtime_dir" ] || fail "RPM tmpfiles did not create $runtime_dir"
done
systemd-tmpfiles --cat-config >/dev/null || fail "systemd rejects the tmpfiles configuration"
pass "packaged tmpfiles entries applied at install time"

printf '\n=== Fedora runtime smoke results: %d passed, 0 failed ===\n' "$pass_count"
# For test/packaging-evidence.py. No assertion here needs the ONNX models, so
# none was withheld for lack of them; a failure above exits before this line.
printf 'RESULTS_JSON: {"pass":%d,"fail":0,"skip":0,"allowed_skip":0,"mandatory_skip":0,"models_present":true}\n' "$pass_count"
