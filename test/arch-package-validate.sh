#!/usr/bin/env bash
set -uo pipefail

PASS=0
FAIL=0

run_test() {
    local name="$1"
    local cmd="$2"
    echo -n "TEST: $name ... "
    if bash -c "$cmd" > /tmp/arch-package-output 2>&1; then
        echo "PASS"
        PASS=$((PASS + 1))
    else
        echo "FAIL"
        cat /tmp/arch-package-output
        FAIL=$((FAIL + 1))
    fi
}

owned=/etc/pam.d/facelock-package-owned
blocker=/etc/pam.d/facelock-package-blocker
drifted=/etc/pam.d/polkit-1
rm -f /etc/pam.d/facelock-test "$owned" "$blocker" "$drifted"
cat > "$owned" <<'EOF'
#%PAM-1.0
auth      sufficient pam_facelock.so
auth      include system-auth
EOF
cat > "$blocker" <<'EOF'
#%PAM-1.0
auth required pam_facelock.so debug
auth include system-auth
EOF
chmod 644 "$owned" "$blocker"
sha256sum "$owned" > /tmp/facelock-package-owned.sha
sha256sum "$blocker" > /tmp/facelock-package-blocker.sha

# The reported shape: an Omarchy-style /etc/pam.d/polkit-1 that shadows the
# package's own /usr/lib/pam.d/polkit-1, carries Facelock's exact canonical
# line, and has no Facelock header or provenance -- machine-wide removal must
# not treat that drift as a blocker.
test -f /usr/lib/pam.d/polkit-1 || { echo "FAIL: image lacks /usr/lib/pam.d/polkit-1"; exit 1; }
cat > "$drifted" <<'EOF'
auth      sufficient pam_facelock.so
auth      [success=1 default=ignore] pam_exec.so quiet /usr/bin/omarchy-hw-laptop-closed
auth      sufficient pam_fprintd.so
auth      required pam_unix.so

account   required pam_unix.so
password  required pam_unix.so
session   required pam_unix.so
EOF
chmod 644 "$drifted"
sha256sum "$drifted" > /tmp/facelock-package-drifted.sha

printf '[invalid\n' > /etc/facelock/config.toml
install -Dm600 /dev/null /var/lib/facelock/facelock.db
rm -rf /var/lib/facelock/models
export ORT_DYLIB_PATH=/facelock-test-missing-onnxruntime.so

run_test "pacman removal aborts on an unmanaged PAM reference" \
    "! pacman -R --noconfirm facelock"
run_test "pacman keeps the package installed after aborted removal" \
    "pacman -Q facelock"
run_test "PAM module remains after aborted package removal" \
    "test -f /usr/lib/security/pam_facelock.so"
run_test "recognized PAM edit remains after aborted package removal" \
    "sha256sum -c --status /tmp/facelock-package-owned.sha"
run_test "unmanaged PAM reference remains after aborted package removal" \
    "sha256sum -c --status /tmp/facelock-package-blocker.sha"
run_test "drifted vendor override remains after aborted package removal" \
    "sha256sum -c --status /tmp/facelock-package-drifted.sha"

rm -f "$blocker"
run_test "pacman removal succeeds after the blocker is cleared" \
    "pacman -R --noconfirm facelock"
run_test "recognized arbitrary PAM edit is cleaned before module removal" \
    "test -f $owned && ! grep -q pam_facelock.so $owned"
run_test "drifted vendor override keeps its guard lines and loses the module rule" \
    "test -f $drifted && ! grep -q pam_facelock.so $drifted && grep -q omarchy-hw-laptop-closed $drifted && grep -q pam_fprintd.so $drifted"
run_test "pacman removed the cleanup binary and PAM module" \
    "! test -e /usr/bin/facelock && ! test -e /usr/lib/security/pam_facelock.so"
run_test "pacman no longer reports the package installed" \
    "! pacman -Q facelock"

rm -f "$owned" "$drifted" /tmp/facelock-package-owned.sha \
    /tmp/facelock-package-blocker.sha /tmp/facelock-package-drifted.sha \
    /tmp/arch-package-output

echo ""
echo "=== Arch package results: $PASS passed, $FAIL failed ==="
# For test/packaging-evidence.py: both halves in one line. The e2e half hands
# its count over in ARCH_E2E_PASS before exec'ing this one, and neither needs
# the ONNX models, so none was withheld for lack of them.
printf 'RESULTS_JSON: {"pass":%d,"fail":%d,"skip":0,"allowed_skip":0,"mandatory_skip":0,"models_present":true}\n' \
    "$((PASS + ${ARCH_E2E_PASS:-0}))" "$FAIL"
test "$FAIL" -eq 0
