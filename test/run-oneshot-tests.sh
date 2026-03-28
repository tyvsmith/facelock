#!/bin/bash
set -euo pipefail

PASS=0
FAIL=0
LIVE_TIMEOUT="${FACELOCK_LIVE_TIMEOUT:-90s}"

run_test() {
    local name="$1"
    local cmd="$2"
    local expected_result="${3:-0}"

    echo -n "TEST: $name ... "
    set +o pipefail
    if eval "$cmd" > /tmp/test-output 2>&1; then
        result=0
    else
        result=$?
    fi
    set -o pipefail

    if [ "$expected_result" = "any" ] || [ "$result" -eq "$expected_result" ]; then
        echo "PASS"
        PASS=$((PASS + 1))
        return 0
    else
        echo "FAIL (exit=$result, expected=$expected_result)"
        cat /tmp/test-output
        FAIL=$((FAIL + 1))
        return 1
    fi
}

run_test_contains() {
    local name="$1"
    local cmd="$2"
    local pattern="$3"

    echo -n "TEST: $name ... "
    set +o pipefail
    if eval "$cmd" > /tmp/test-output 2>&1; then
        result=0
    else
        result=$?
    fi
    set -o pipefail

    if [ "$result" -eq 0 ] && grep -q -- "$pattern" /tmp/test-output; then
        echo "PASS"
        PASS=$((PASS + 1))
        return 0
    fi

    echo "FAIL (exit=$result, pattern=$pattern)"
    cat /tmp/test-output
    FAIL=$((FAIL + 1))
    return 1
}

echo "=== Oneshot Mode Tests (fully daemonless, with camera) ==="
echo ""

# Use installed config, set oneshot mode and writable paths
sed -i 's|db_path.*|db_path = "/tmp/facelock-test.db"|' /etc/facelock/config.toml 2>/dev/null || true
# Force oneshot mode — no daemon for these tests
sed -i '/^\[daemon\]/a mode = "oneshot"' /etc/facelock/config.toml

# Verify no daemon is running
run_test "No daemon socket exists" \
    "test ! -S /tmp/facelock.sock" \
    0

# --- CLI commands in oneshot mode (no daemon) ---

# Device listing
run_test_contains "facelock devices (oneshot)" \
    "facelock devices" \
    "/dev/video" || exit 1

# Enrollment (direct, no daemon)
run_test_contains "facelock enroll (oneshot)" \
    "timeout --foreground $LIVE_TIMEOUT facelock enroll --user testuser --label test-face --skip-setup-check" \
    "Face enrolled successfully" || exit 1

# List enrolled models (direct DB access)
run_test_contains "facelock list (oneshot)" \
    "facelock list --user testuser" \
    "test-face"

# Test auth via CLI (direct)
run_test_contains "facelock test (oneshot)" \
    "timeout --foreground $LIVE_TIMEOUT facelock test --user testuser" \
    "Matched in"

# facelock auth binary (used by PAM module)
run_test "facelock auth authenticates (oneshot)" \
    "timeout --foreground $LIVE_TIMEOUT facelock auth --user testuser --config /etc/facelock/config.toml"

# PAM authentication (the real deal — no daemon)
run_test "pamtester authenticates (oneshot, no daemon)" \
    "timeout --foreground $LIVE_TIMEOUT pamtester facelock-test testuser authenticate"

# facelock auth rejects unknown user
run_test "facelock auth rejects unknown user" \
    "facelock auth --user nobody --config /etc/facelock/config.toml" \
    2

# Clear models (direct DB access)
run_test "facelock clear (oneshot)" \
    "facelock clear --user testuser --yes"

# Verify models cleared
run_test_contains "facelock list empty after clear (oneshot)" \
    "facelock list --user testuser" \
    "No face models"

# Still no daemon socket
run_test "Still no daemon socket" \
    "test ! -S /tmp/facelock.sock" \
    0

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
