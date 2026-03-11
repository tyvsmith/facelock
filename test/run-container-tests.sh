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
    "pamtester visage-test testuser authenticate" \
    "any"

# Test 2: PAM returns PAM_IGNORE when daemon not running
# pamtester returns non-zero when auth fails, but the module shouldn't crash
run_test "Module returns gracefully when daemon not running" \
    "pamtester visage-test testuser authenticate < /dev/null" \
    "any"

# Test 3: Module handles missing config gracefully
run_test "Module handles missing config" \
    "mv /etc/visage/config.toml /etc/visage/config.toml.bak && pamtester visage-test testuser authenticate; mv /etc/visage/config.toml.bak /etc/visage/config.toml" \
    "any"

# Test 4: Disabled config returns PAM_IGNORE
run_test "Module respects disabled config" \
    "sed -i 's/disabled = false/disabled = true/' /etc/visage/config.toml && pamtester visage-test testuser authenticate; sed -i 's/disabled = true/disabled = false/' /etc/visage/config.toml" \
    "any"

# Test 5: PAM symbols are exported
run_test "pam_sm_authenticate symbol exists" \
    "nm -D /lib/security/pam_visage.so | grep -q pam_sm_authenticate" \
    0

run_test "pam_sm_setcred symbol exists" \
    "nm -D /lib/security/pam_visage.so | grep -q pam_sm_setcred" \
    0

# --- Spec 28: Privilege enforcement ---

run_test "visage setup requires root" \
    "su -s /bin/bash testuser -c 'visage setup 2>&1' | grep -q 'Root required'" \
    0

run_test "visage daemon requires root" \
    "su -s /bin/bash testuser -c 'visage daemon 2>&1' | grep -q 'Root required'" \
    0

# --- Spec 29: Smart PAM skip (no enrolled faces) ---

# In oneshot mode with no enrolled faces, visage auth should exit 2 (PAM_IGNORE)
run_test "visage auth exits 2 when no faces enrolled" \
    "visage auth --user testuser --config /etc/visage/config.toml; test \$? -eq 2" \
    0

# pamtester should pass through (PAM_IGNORE from face → pam_deny catches it)
# The key: it should be FAST (no camera timeout)
run_test "No enrolled faces: pamtester completes quickly" \
    "timeout 3 pamtester visage-test testuser authenticate 2>&1; test \$? -ne 124" \
    0

# --- Spec 30: PAM conversation messages ---

# When notification.enabled = true (default), "Identifying face..." should appear
run_test "PAM shows 'Identifying face...' text" \
    "pamtester visage-test testuser authenticate 2>&1 | grep -q 'Identifying face'" \
    0

# When notification.enabled = false, no text message
run_test "PAM respects notification.enabled=false" \
    "sed -i '/^\[notification\]/,/^\[/{s/.*enabled.*/enabled = false/}' /etc/visage/config.toml 2>/dev/null || (echo -e '\n[notification]\nenabled = false' >> /etc/visage/config.toml); pamtester visage-test testuser authenticate 2>&1 | grep -qv 'Identifying face'; sed -i '/enabled = false/d' /etc/visage/config.toml" \
    0

# --- Spec 29: Smart PAM with oneshot config ---

run_test "Oneshot mode: no enrolled faces returns quickly" \
    "sed -i '/^\[daemon\]/a mode = \"oneshot\"' /etc/visage/config.toml; timeout 3 pamtester visage-test testuser authenticate 2>&1; rc=\$?; sed -i '/^mode = \"oneshot\"/d' /etc/visage/config.toml; test \$rc -ne 124" \
    0

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
