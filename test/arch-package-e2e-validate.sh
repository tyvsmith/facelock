#!/usr/bin/env bash
# Validate the facelock package that build-arch-source-package.sh built from
# dist/PKGBUILD and installed with pacman.
#
# Scope is what only a real package can show: what package() actually put on
# disk, what pacman thinks it owns, what the facelock.install scriptlet did on
# post_install, whether depends alone is enough to run the binaries, and what a
# user meets when they type the first documented commands on a clean machine.
#
# The run ends by handing off to arch-package-validate.sh, which removes the
# package and covers the libalpm hook and PAM cleanup. That half is destructive,
# so nothing may follow it.
set -uo pipefail

PASS=0
FAIL=0

run_test() {
    local name="$1"
    local cmd="$2"
    echo -n "TEST: $name ... "
    if bash -c "$cmd" >/tmp/arch-e2e-output 2>&1; then
        echo "PASS"
        PASS=$((PASS + 1))
    else
        echo "FAIL"
        cat /tmp/arch-e2e-output
        FAIL=$((FAIL + 1))
    fi
}

# A first command on a clean install may legitimately fail — no camera, no
# models, nothing enrolled. It may not panic, and it may not say nothing.
runs_cleanly() {
    local output
    output="$(timeout 60 "$@" 2>&1)"
    grep -qiE 'panicked at|RUST_BACKTRACE|core dumped' <<<"$output" && return 1
    [ -n "$output" ]
}
export -f runs_cleanly

owned_by_facelock() {
    [ "$(pacman -Qoq -- "$1" 2>/dev/null)" = facelock ]
}
export -f owned_by_facelock

mode_is() {
    [ "$(stat -c '%a %U %G' -- "$1" 2>/dev/null)" = "$2" ]
}
export -f mode_is

echo "=== Arch package validation (built from dist/PKGBUILD) ==="
echo ""

# --- what pacman thinks it installed ------------------------------------------

run_test "pacman reports the package installed" "pacman -Q facelock"
run_test "package metadata carries a description, url and licences" \
    "pacman -Qi facelock | grep -q '^Description *: .' &&
     pacman -Qi facelock | grep -q '^URL *: http' &&
     pacman -Qi facelock | grep -q '^Licenses *: .'"
run_test "no installed file drifted from what package() staged" "pacman -Qkk facelock"

# --- every path package() installs --------------------------------------------

for path in \
    /usr/bin/facelock \
    /usr/bin/facelock-polkit-agent \
    /usr/lib/security/pam_facelock.so \
    /etc/facelock/config.toml \
    /usr/lib/systemd/system/facelock-daemon.service \
    /usr/share/dbus-1/system.d/org.facelock.Daemon.conf \
    /usr/share/dbus-1/system-services/org.facelock.Daemon.service \
    /usr/lib/tmpfiles.d/facelock.conf \
    /usr/share/libalpm/hooks/facelock-pam-remove.hook \
    /usr/share/licenses/facelock/LICENSE-MIT \
    /usr/share/licenses/facelock/LICENSE-APACHE
do
    run_test "package owns $path" \
        "[ -f '$path' ] && owned_by_facelock '$path'"
done
run_test "package owns the quirks database" \
    "find /usr/share/facelock/quirks.d -maxdepth 1 -type f -name '*.toml' -print -quit | grep -q . &&
     pacman -Ql facelock | grep -q '/usr/share/facelock/quirks.d/'"
run_test "config.toml is registered as a backup file" \
    "pacman -Qii facelock | grep -qE '/etc/facelock/config.toml \[(un)?modified\]'"

# po/ holds only .pot templates, so the recipe's locale stanza must install
# nothing and must not leave the package owning an empty /usr/share/locale.
run_test "package owns no locale root while po/ holds only templates" \
    "! pacman -Ql facelock | grep -q '/usr/share/locale'"

# --- the binaries the package shipped -----------------------------------------

run_test "facelock binary is executable" "[ -x /usr/bin/facelock ]"
run_test "PAM module exports pam_sm_authenticate" \
    "nm -D /usr/lib/security/pam_facelock.so | grep -q pam_sm_authenticate"
run_test "PAM module exports pam_sm_setcred" \
    "nm -D /usr/lib/security/pam_facelock.so | grep -q pam_sm_setcred"
run_test "PAM module avoids heavy dependencies" \
    "! ldd /usr/lib/security/pam_facelock.so | grep -Eqi '(onnxruntime|libort|libv4l|opencv|gstreamer|openvino|cuda|rocm)'"

# --- what the facelock.install scriptlet did on post_install ------------------

run_test "post_install created the state root at 0711 root:root" \
    "mode_is /var/lib/facelock '711 root root'"
run_test "post_install created the model directory at 0755 root:root" \
    "mode_is /var/lib/facelock/models '755 root root'"
run_test "post_install created the enrollment marker directory at 0711 root:root" \
    "mode_is /var/lib/facelock/enrolled '711 root root'"
run_test "post_install created the PAM backup directory at 0700 root:root" \
    "mode_is /var/lib/facelock/pam-backups '700 root root'"
run_test "post_install created the log directory at 0700 root:root" \
    "mode_is /var/log/facelock '700 root root'"
run_test "post_install created the snapshot directory at 0700 root:root" \
    "mode_is /var/log/facelock/snapshots '700 root root'"
run_test "post_install creates no facelock group (ADR 010 retired it)" \
    "! getent group facelock"
run_test "post_install left no database or audit log behind" \
    "[ ! -e /var/lib/facelock/facelock.db ] && [ ! -e /var/log/facelock/audit.jsonl ]"

# --- depends alone must be enough ---------------------------------------------
#
# makepkg --syncdeps installed makedepends too, so up to here a runtime library
# that only makedepends supplies would look fine. Drop everything nothing
# depends on any more and the remaining closure is exactly what `depends`
# bought. #209 was a name error in this list; an omission is the other half of
# the same failure and reaches users the same way.

echo ""
echo "==> removing makedepends and every other orphan"
while mapfile -t orphans < <(pacman -Qdtq 2>/dev/null) && [ "${#orphans[@]}" -gt 0 ]; do
    pacman -Rns --noconfirm -- "${orphans[@]}" >/dev/null 2>&1 || break
done
echo ""

# The ldd checks below are only meaningful if the removal actually happened. A
# failed `pacman -Rns` leaves makedepends installed, and then every library the
# recipe forgot to declare is still on disk and every check passes for the wrong
# reason. rust and clang are build-only under any reading of this recipe, so
# their survival is the signal that the removal did not take.
run_test "makedepends are gone, so the checks below are not vacuous" \
    "! pacman -Q rust && ! pacman -Q clang && [ -z \"\$(pacman -Qdtq 2>/dev/null)\" ]"
run_test "facelock survives makedepends removal" "pacman -Q facelock"
for binary in /usr/bin/facelock /usr/bin/facelock-polkit-agent /usr/lib/security/pam_facelock.so; do
    run_test "runtime depends resolve every library $binary needs" \
        "! ldd '$binary' 2>&1 | grep -q 'not found'"
done

# --- the first commands a user types ------------------------------------------

run_test "facelock --version reports the packaged version" \
    "/usr/bin/facelock --version | grep -q \"\$(pacman -Q facelock | cut -d' ' -f2 | cut -d- -f1)\""
run_test "facelock --help exits successfully" "/usr/bin/facelock --help >/dev/null"
run_test "facelock setup --help exits successfully" "/usr/bin/facelock setup --help >/dev/null"
run_test "facelock tpm --help exits successfully" "/usr/bin/facelock tpm --help >/dev/null"
run_test "facelock status runs without panicking" "runs_cleanly /usr/bin/facelock status"
run_test "facelock devices runs without panicking with no camera" \
    "runs_cleanly /usr/bin/facelock devices"
run_test "facelock list runs without panicking with nothing enrolled" \
    "runs_cleanly /usr/bin/facelock list"
run_test "facelock capabilities runs without panicking" \
    "runs_cleanly /usr/bin/facelock capabilities"
run_test "facelock is-enrolled reports root not enrolled" \
    "/usr/bin/facelock is-enrolled --user root; [ \$? -eq 1 ]"
run_test "facelock pam status --all runs without panicking" \
    "runs_cleanly /usr/bin/facelock pam status --all"
run_test "the packaged config is valid TOML" \
    "python3 -c 'import tomllib; tomllib.load(open(\"/etc/facelock/config.toml\", \"rb\"))'"
run_test "D-Bus policy XML is valid" \
    "python3 -c 'import xml.etree.ElementTree as E; E.parse(\"/usr/share/dbus-1/system.d/org.facelock.Daemon.conf\")'"
run_test "the packaged unit file parses" \
    "command -v systemd-analyze >/dev/null &&
     ! systemd-analyze verify /usr/lib/systemd/system/facelock-daemon.service 2>&1 |
       grep -q 'Failed to parse'"
run_test "the packaged tmpfiles config is accepted" \
    "systemd-tmpfiles --create --dry-run /usr/lib/tmpfiles.d/facelock.conf"

echo ""
echo "=== Arch package results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1

# The removal half. It edits /etc/pam.d and uninstalls the package, so it runs
# last and nothing may be added after it.
echo ""
echo "=== libalpm hook and PAM cleanup on removal ==="
exec /arch-package-validate.sh
