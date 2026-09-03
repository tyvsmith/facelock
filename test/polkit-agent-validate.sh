#!/bin/bash
# Validates the facelock polkit agent's D-Bus boundary:
#   - a NON-allowlisted polkit action_id is declined ("not eligible"),
#     so polkit falls through to the password dialog;
#   - an ALLOWLISTED action_id passes the allowlist gate (it is declined
#     later only because no daemon/camera exists in-container, never by the
#     allowlist).
#
# Full interactive polkit is not feasible in a container, so we assert at the
# agent's own D-Bus interface. The agent is launched with
# FACELOCK_POLKIT_SKIP_REGISTER=1, which skips polkit registration and claims a
# well-known session-bus name so busctl can address it.
#
# Env overrides (for running outside the package container):
#   FACELOCK_POLKIT_AGENT_BIN  path to the agent binary
#
# Exit 77 means the check could not reach its subject. pkg-validate.sh counts
# that as a mandatory skip, never a pass (#313).
set -uo pipefail

AGENT_BIN="${FACELOCK_POLKIT_AGENT_BIN:-}"
if [ -z "$AGENT_BIN" ]; then
    for c in /usr/bin/facelock-polkit-agent /usr/local/bin/facelock-polkit-agent; do
        [ -x "$c" ] && AGENT_BIN="$c" && break
    done
fi

if [ -z "$AGENT_BIN" ] || [ ! -x "$AGENT_BIN" ]; then
    echo "SKIP: facelock-polkit-agent binary not found"
    exit 77
fi
if ! command -v dbus-daemon >/dev/null 2>&1; then
    echo "SKIP: dbus-daemon not available"
    exit 77
fi
if ! command -v busctl >/dev/null 2>&1; then
    echo "SKIP: busctl not available"
    exit 77
fi

echo "=== Polkit Agent D-Bus Boundary Validation ==="

FAIL=0
BUS_SOCK="$(mktemp -u /tmp/facelock-polkit-session-bus.XXXXXX)"
SESSION_ADDR="unix:path=${BUS_SOCK}"
AGENT_PID=""
BUS_PID=""

cleanup() {
    [ -n "$AGENT_PID" ] && kill "$AGENT_PID" 2>/dev/null || true
    [ -n "$BUS_PID" ] && kill "$BUS_PID" 2>/dev/null || true
    rm -f "$BUS_SOCK"
}
trap cleanup EXIT

# Private session bus for the agent + probe.
dbus-daemon --session --address="$SESSION_ADDR" --nofork --nopidfile &
BUS_PID=$!
export DBUS_SESSION_BUS_ADDRESS="$SESSION_ADDR"

# Wait for the session bus socket.
for _ in $(seq 1 50); do
    [ -S "$BUS_SOCK" ] && break
    sleep 0.1
done

# Launch the agent in test mode (skip polkit registration, claim well-known name).
FACELOCK_POLKIT_SKIP_REGISTER=1 "$AGENT_BIN" >/tmp/polkit-agent.log 2>&1 &
AGENT_PID=$!

# Wait for the agent to own its well-known name.
OWNED=0
for _ in $(seq 1 100); do
    if busctl --user --address="$SESSION_ADDR" list 2>/dev/null | grep -q org.facelock.PolkitAgent; then
        OWNED=1
        break
    fi
    sleep 0.1
done

if [ "$OWNED" -ne 1 ]; then
    echo "SKIP: agent did not claim its session-bus name (no session/system bus?)"
    echo "--- agent log ---"
    cat /tmp/polkit-agent.log 2>/dev/null || true
    exit 77
fi

call_begin() {
    local action_id="$1"
    # Bound the call: an allowlisted action proceeds to daemon activation,
    # which has no camera in-container and may block; we only care that the
    # allowlist gate let it through, so a timeout is an acceptable outcome.
    timeout 10 busctl --user --address="$SESSION_ADDR" call \
        org.facelock.PolkitAgent /org/facelock/PolkitAgent \
        org.freedesktop.PolicyKit1.AuthenticationAgent \
        BeginAuthentication "sssa{ss}sa(sa{sv})" \
        "$action_id" "pkg-validate probe" "" 0 "cookie-${action_id}" 0 2>&1
}

# 1. Non-allowlisted action MUST be declined with "not eligible".
OUT_DENY="$(call_begin "org.freedesktop.policykit.exec")"
echo -n "TEST: non-allowlisted action_id declined (falls through to password) ... "
if echo "$OUT_DENY" | grep -qi "not eligible"; then
    echo "PASS"
else
    echo "FAIL"
    echo "  expected an error containing 'not eligible', got:"
    echo "  $OUT_DENY"
    FAIL=$((FAIL + 1))
fi

# 2. Allowlisted action MUST pass the allowlist gate (never "not eligible").
OUT_ALLOW="$(call_begin "org.freedesktop.login1.lock-sessions")"
echo -n "TEST: allowlisted action_id accepted by allowlist gate ... "
if echo "$OUT_ALLOW" | grep -qi "not eligible"; then
    echo "FAIL"
    echo "  allowlisted action was wrongly rejected by the allowlist:"
    echo "  $OUT_ALLOW"
    FAIL=$((FAIL + 1))
else
    echo "PASS"
fi

echo ""
if [ "$FAIL" -gt 0 ]; then
    echo "=== Polkit Agent Validation: $FAIL failed ==="
    exit 1
fi
echo "=== Polkit Agent Validation: OK ==="
