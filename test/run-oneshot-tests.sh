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

echo "=== Oneshot Mode Tests (fully daemonless, with camera) ==="
echo ""

# Write oneshot config — NO daemon anywhere
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

# Verify no daemon is running
run_test "No daemon socket exists" \
    "test ! -S /tmp/visage.sock" \
    0

# --- CLI commands in oneshot mode (no daemon) ---

# Device listing
run_test "visage devices (oneshot)" \
    "visage devices"

# Enrollment (direct, no daemon)
run_test "visage enroll (oneshot)" \
    "visage enroll --user testuser --label test-face"

# List enrolled models (direct DB access)
run_test "visage list (oneshot)" \
    "visage list --user testuser"

# Test auth via CLI (direct)
run_test "visage test (oneshot)" \
    "visage test --user testuser"

# visage-auth binary (used by PAM module)
run_test "visage-auth authenticates (oneshot)" \
    "visage-auth --user testuser --config /etc/visage/config.toml"

# PAM authentication (the real deal — no daemon)
run_test "pamtester authenticates (oneshot, no daemon)" \
    "pamtester visage-test testuser authenticate"

# visage-auth rejects unknown user
run_test "visage-auth rejects unknown user" \
    "visage-auth --user nobody --config /etc/visage/config.toml" \
    1

# Clear models (direct DB access)
run_test "visage clear (oneshot)" \
    "visage clear --user testuser --yes"

# Verify models cleared
run_test "visage list empty after clear (oneshot)" \
    "visage list --user testuser 2>&1 | grep -q 'No face models'"

# Still no daemon socket
run_test "Still no daemon socket" \
    "test ! -S /tmp/visage.sock" \
    0

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
