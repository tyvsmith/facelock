#!/bin/bash
set -euo pipefail

PASS=0
FAIL=0

run_test() {
    local name="$1"
    local cmd="$2"
    local expected_result="${3:-0}"

    echo -n "TEST: $name ... "
    if bash -c "$cmd" > /tmp/test-output 2>&1; then
        result=0
    else
        result=$?
    fi

    if [ "$expected_result" = "any" ] || [ "$result" -eq "$expected_result" ]; then
        echo "PASS"
        PASS=$((PASS + 1))
    else
        echo "FAIL (exit=$result, expected=$expected_result)"
        cat /tmp/test-output
        FAIL=$((FAIL + 1))
    fi
}

run_warn_check() {
    local name="$1"
    local cmd="$2"

    echo -n "WARN: $name ... "
    if bash -c "$cmd" > /tmp/test-output 2>&1; then
        echo "present"
    else
        echo "missing"
    fi
}

PAM_MODULE_PATH=""
for candidate in /usr/lib/security/pam_facelock.so /usr/lib64/security/pam_facelock.so /lib/security/pam_facelock.so; do
    if [ -f "$candidate" ]; then
        PAM_MODULE_PATH="$candidate"
        break
    fi
done
export PAM_MODULE_PATH

DBUS_POLICY_FILE="/usr/share/dbus-1/system.d/org.facelock.Daemon.conf"
export DBUS_POLICY_FILE

echo "=== Facelock Package Validation ==="
echo ""

run_test "facelock binary exists and is executable" "[ -x /usr/bin/facelock ]"
run_test "PAM module exists in supported path" "[ -n \"$PAM_MODULE_PATH\" ]"
run_test "config exists" "[ -f /etc/facelock/config.toml ]"
run_test "D-Bus policy exists" "[ -f /usr/share/dbus-1/system.d/org.facelock.Daemon.conf ]"
run_test "D-Bus activation exists" "[ -f /usr/share/dbus-1/system-services/org.facelock.Daemon.service ]"
run_test "sysusers file exists" "[ -f /usr/lib/sysusers.d/facelock.conf ] || [ -f /usr/share/sysusers.d/facelock.conf ]"
run_test "tmpfiles file exists" "[ -f /usr/lib/tmpfiles.d/facelock.conf ] || [ -f /usr/share/tmpfiles.d/facelock.conf ]"

run_warn_check "facelock-polkit-agent binary" "[ -x /usr/bin/facelock-polkit-agent ]"
run_warn_check "quirks database files" "ls /usr/share/facelock/quirks.d/*.toml >/dev/null 2>&1"
run_warn_check "bundled ONNX Runtime" "[ -f /usr/lib/facelock/libonnxruntime.so ] || [ -f /usr/lib64/facelock/libonnxruntime.so ]"

run_test "PAM module exports pam_sm_authenticate" "nm -D \"$PAM_MODULE_PATH\" | grep -q pam_sm_authenticate"
run_test "PAM module exports pam_sm_setcred" "nm -D \"$PAM_MODULE_PATH\" | grep -q pam_sm_setcred"
run_test "PAM module avoids heavy dependencies" "! ldd \"$PAM_MODULE_PATH\" | grep -Eqi '(onnxruntime|libort|libv4l|opencv|gstreamer|openvino|cuda|rocm)'"
run_test "PAM module is under 5MB" "test $(stat -c%s $PAM_MODULE_PATH) -lt 5242880"

run_test "facelock --version exits successfully" "/usr/bin/facelock --version >/dev/null"
run_test "facelock --help exits successfully" "/usr/bin/facelock --help >/dev/null"

run_test "D-Bus policy XML is valid" "if command -v xmllint >/dev/null 2>&1; then xmllint --noout \"$DBUS_POLICY_FILE\"; else python3 -c \"import os, xml.etree.ElementTree as ET; ET.parse(os.environ.get(\\\"DBUS_POLICY_FILE\\\"))\"; fi"

run_test "facelock group exists (sysusers)" "if command -v systemd-sysusers >/dev/null 2>&1; then systemd-sysusers >/dev/null 2>&1 || true; fi; getent group facelock >/dev/null"

run_test "facelock runtime directories exist (tmpfiles)" "if command -v systemd-tmpfiles >/dev/null 2>&1; then systemd-tmpfiles --create >/dev/null 2>&1 || true; fi; [ -d /var/lib/facelock ] && [ -d /var/log/facelock ]"

# PAM tests (only if pamtester is available)
if command -v pamtester >/dev/null 2>&1 && [ -f /etc/pam.d/facelock-test ]; then
    run_test "PAM module loads via pamtester" "pamtester facelock-test testuser authenticate < /dev/null 2>&1 | grep -qiE '(successfully|authentication failure)'"
fi

# D-Bus tests (only if dbus-daemon is available)
if command -v dbus-daemon >/dev/null 2>&1; then
    # Start a system bus for testing
    run_test "D-Bus system bus starts" "mkdir -p /run/dbus && dbus-daemon --system --fork --nopidfile 2>/dev/null"

    # Verify the facelock service is visible on the bus
    if command -v busctl >/dev/null 2>&1; then
        run_test "D-Bus facelock service activatable" "busctl --system list --activatable 2>/dev/null | grep -q org.facelock.Daemon"
    elif command -v dbus-send >/dev/null 2>&1; then
        run_test "D-Bus facelock service activatable" "dbus-send --system --dest=org.freedesktop.DBus --print-reply /org/freedesktop/DBus org.freedesktop.DBus.ListActivatableNames 2>/dev/null | grep -q org.facelock.Daemon"
    fi
fi

# Package removal test — must come last since it removes the package
echo ""
echo "=== Package Removal Test ==="

if command -v dpkg >/dev/null 2>&1; then
    run_test "Package removal via dpkg" "dpkg -r facelock"
    run_test "facelock binary removed after dpkg -r" "[ ! -f /usr/bin/facelock ]"
    run_test "PAM module removed after dpkg -r" \
        "[ ! -f /lib/security/pam_facelock.so ] && [ ! -f /usr/lib/security/pam_facelock.so ] && [ ! -f /usr/lib64/security/pam_facelock.so ]"
    run_test "Config preserved after dpkg -r (conffile)" "[ -f /etc/facelock/config.toml ]"
elif command -v rpm >/dev/null 2>&1; then
    # Modify config so RPM treats it as user-edited and preserves it as .rpmsave
    echo "# modified by test" >> /etc/facelock/config.toml
    run_test "Package removal via rpm" "rpm -e facelock"
    run_test "facelock binary removed after rpm -e" "[ ! -f /usr/bin/facelock ]"
    run_test "PAM module removed after rpm -e" \
        "[ ! -f /lib/security/pam_facelock.so ] && [ ! -f /usr/lib/security/pam_facelock.so ] && [ ! -f /usr/lib64/security/pam_facelock.so ]"
    run_test "Config preserved after rpm -e (config(noreplace))" "[ -f /etc/facelock/config.toml ] || [ -f /etc/facelock/config.toml.rpmsave ]"
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
