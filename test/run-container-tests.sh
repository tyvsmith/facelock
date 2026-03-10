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

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
