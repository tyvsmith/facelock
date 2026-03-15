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

echo "=== Integration Tests (with camera) ==="
echo ""

# Use the installed config (written by Containerfile), override db path
# to a writable location since default may not be writable in containers
sed -i 's|db_path.*|db_path = "/tmp/facelock-test.db"|' /etc/facelock/config.toml 2>/dev/null || true

# Start daemon in background
facelock daemon &
DAEMON_PID=$!
sleep 2

# Verify daemon is running
run_test "Daemon responds to ping" \
    "facelock status"

# Test device listing
run_test "Device listing works" \
    "facelock devices"

# Test enrollment (will capture faces from camera)
run_test "Enroll a face" \
    "facelock enroll --user testuser --label test-face"

# Test listing enrolled models
run_test "List enrolled models" \
    "facelock list --user testuser"

# Test authentication via CLI
run_test "Authenticate enrolled face (CLI)" \
    "facelock test --user testuser"

# Test authentication via PAM (the real auth path)
run_test "Authenticate enrolled face (PAM)" \
    "pamtester facelock-test testuser authenticate"

# Clean up
run_test "Clear enrolled models" \
    "facelock clear --user testuser --yes"

# Stop daemon
kill $DAEMON_PID 2>/dev/null || true
wait $DAEMON_PID 2>/dev/null || true

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
