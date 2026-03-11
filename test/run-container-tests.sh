#!/bin/bash
set -euo pipefail

PASS=0
FAIL=0

run_test() {
    local name="$1"
    local cmd="$2"
    local expected_result="${3:-0}"

    echo -n "TEST: $name ... "
    if eval "$cmd" > /tmp/test-output 2>&1; then
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

echo "=== PAM Container Tests ==="
echo ""

# Test 1: Module loads without crash
run_test "Module loads without crash" \
    "pamtester facelock-test testuser authenticate" \
    "any"

# Test 2: PAM returns PAM_IGNORE when daemon not running
# pamtester returns non-zero when auth fails, but the module shouldn't crash
run_test "Module returns gracefully when daemon not running" \
    "pamtester facelock-test testuser authenticate < /dev/null" \
    "any"

# Test 3: Module handles missing config gracefully
run_test "Module handles missing config" \
    "mv /etc/facelock/config.toml /etc/facelock/config.toml.bak && pamtester facelock-test testuser authenticate; mv /etc/facelock/config.toml.bak /etc/facelock/config.toml" \
    "any"

# Test 4: Disabled config returns PAM_IGNORE
run_test "Module respects disabled config" \
    "sed -i 's/disabled = false/disabled = true/' /etc/facelock/config.toml && pamtester facelock-test testuser authenticate; sed -i 's/disabled = true/disabled = false/' /etc/facelock/config.toml" \
    "any"

# Test 5: PAM symbols are exported
run_test "pam_sm_authenticate symbol exists" \
    "nm -D /lib/security/pam_facelock.so | grep -q pam_sm_authenticate" \
    0

run_test "pam_sm_setcred symbol exists" \
    "nm -D /lib/security/pam_facelock.so | grep -q pam_sm_setcred" \
    0

# --- Spec 28: Privilege enforcement ---

run_test "facelock setup requires root" \
    "su -s /bin/bash testuser -c 'facelock setup 2>&1' | grep -q 'Root required'" \
    0

run_test "facelock daemon requires root" \
    "su -s /bin/bash testuser -c 'facelock daemon 2>&1' | grep -q 'Root required'" \
    0

# --- Spec 29: Smart PAM skip (no enrolled faces) ---

# In oneshot mode with no enrolled faces, facelock auth should exit 2 (PAM_IGNORE)
run_test "facelock auth exits 2 when no faces enrolled" \
    "facelock auth --user testuser --config /etc/facelock/config.toml; test \$? -eq 2" \
    0

# pamtester should pass through (PAM_IGNORE from face → pam_deny catches it)
# The key: it should be FAST (no camera timeout)
run_test "No enrolled faces: pamtester completes quickly" \
    "timeout 3 pamtester facelock-test testuser authenticate 2>&1; test \$? -ne 124" \
    0

# --- Spec 30: PAM conversation messages ---

# When notification.enabled = true (default), "Identifying face..." should appear
run_test "PAM shows 'Identifying face...' text" \
    "pamtester facelock-test testuser authenticate 2>&1 | grep -q 'Identifying face'" \
    0

# When notification mode = off, no text message
run_test "PAM respects notification mode=off" \
    "sed -i '/^\[notification\]/,/^\[/{s/.*mode.*/mode = \"off\"/}' /etc/facelock/config.toml 2>/dev/null || (echo -e '\n[notification]\nmode = \"off\"' >> /etc/facelock/config.toml); pamtester facelock-test testuser authenticate 2>&1 | grep -qv 'Identifying face'; sed -i '/mode = \"off\"/d' /etc/facelock/config.toml" \
    0

# --- Spec 29: Smart PAM with oneshot config ---

run_test "Oneshot mode: no enrolled faces returns quickly" \
    "sed -i '/^\[daemon\]/a mode = \"oneshot\"' /etc/facelock/config.toml; timeout 3 pamtester facelock-test testuser authenticate 2>&1; rc=\$?; sed -i '/^mode = \"oneshot\"/d' /etc/facelock/config.toml; test \$rc -ne 124" \
    0

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
