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

echo "=== Oneshot Mode Tests (daemonless, with camera) ==="
echo ""

# Write config in oneshot mode — NO daemon running
cat > /etc/visage/config.toml <<'CONF'
[device]
max_height = 480

[recognition]
threshold = 0.45
timeout_secs = 5

[daemon]
mode = "oneshot"
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

# --- Phase 1: Enroll via daemon (enrollment still needs the daemon) ---
echo "--- Enrolling face via daemon (required for enrollment) ---"

# Write daemon-mode config temporarily for enrollment
cat > /tmp/enroll-config.toml <<'CONF'
[device]
max_height = 480

[recognition]
threshold = 0.45
timeout_secs = 5

[daemon]
mode = "daemon"
socket_path = "/tmp/visage.sock"
model_dir = "/models"

[storage]
db_path = "/tmp/visage-test.db"

[security]
disabled = false
require_ir = false
require_frame_variance = false
CONF

VISAGE_CONFIG=/tmp/enroll-config.toml visage-daemon &
DAEMON_PID=$!
sleep 2

run_test "Enroll face via daemon" \
    "VISAGE_CONFIG=/tmp/enroll-config.toml visage enroll --user testuser --label test-face"

# Stop daemon — from here on, NO daemon running
kill $DAEMON_PID 2>/dev/null || true
wait $DAEMON_PID 2>/dev/null || true
sleep 1

echo ""
echo "--- Daemon stopped. All remaining tests are daemonless. ---"
echo ""

# Verify daemon is NOT running
run_test "Daemon socket does not exist" \
    "test ! -S /tmp/visage.sock" \
    0

# --- Phase 2: Oneshot authentication (no daemon) ---

# Test visage-auth directly
run_test "visage-auth authenticates enrolled face" \
    "visage-auth --user testuser --config /etc/visage/config.toml" \
    0

# Test visage-auth rejects unknown user
run_test "visage-auth rejects unknown user" \
    "visage-auth --user nobody --config /etc/visage/config.toml" \
    1

# Test visage-auth requires --user
run_test "visage-auth fails without --user" \
    "visage-auth --config /etc/visage/config.toml" \
    2

# Test PAM oneshot path (the real deal)
run_test "pamtester authenticates via oneshot (no daemon)" \
    "pamtester visage-test testuser authenticate"

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
