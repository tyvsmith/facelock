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

# Write container config with correct paths
# device.path omitted — daemon auto-detects the camera
cat > /etc/visage/config.toml <<'CONF'
[device]
max_height = 480

[recognition]
threshold = 0.45
timeout_secs = 5

[daemon]
socket_path = "/tmp/visage.sock"
model_dir = "/models"

[storage]
db_path = "/tmp/visage-test.db"

[security]
disabled = false
require_ir = false
require_frame_variance = false

[snapshots]
dir = "/tmp/visage-snapshots"
CONF

# Start daemon in background
visage-daemon &
DAEMON_PID=$!
sleep 2

# Verify daemon is running
run_test "Daemon responds to ping" \
    "visage status"

# Test device listing
run_test "Device listing works" \
    "visage devices"

# Test enrollment (will capture faces from camera)
run_test "Enroll a face" \
    "visage enroll --user testuser --label test-face"

# Test listing enrolled models
run_test "List enrolled models" \
    "visage list --user testuser"

# Test authentication via CLI
run_test "Authenticate enrolled face (CLI)" \
    "visage test --user testuser"

# Test authentication via PAM (the real auth path)
run_test "Authenticate enrolled face (PAM)" \
    "pamtester visage-test testuser authenticate"

# Clean up
run_test "Clear enrolled models" \
    "visage clear --user testuser --yes"

# Stop daemon
kill $DAEMON_PID 2>/dev/null || true
wait $DAEMON_PID 2>/dev/null || true

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
