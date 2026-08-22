#!/bin/bash
set -euo pipefail

PASS=0
FAIL=0
# Per-step budget for the live camera steps (someone has to be in frame). Read
# inside the container, so `just test-arch-integration` forwards the variable
# with -e; a `podman run` by hand must do the same. Interpolated straight into
# `timeout --foreground`, so it is a timeout(1) duration: 300s, 5m.
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

# ADR 008 §9: like run_test, but a check may report that there was nothing to
# observe (exit 3) instead of passing or failing. The camera-lifecycle
# assertions below depend on a live face being in front of the camera; when one
# is not, "no successful authentication happened" is not a lifecycle bug and
# must not be reported as one. Anything the check printed is echoed either way,
# so a SKIP always says which precondition was missing.
run_test_or_skip() {
    local name="$1"
    local cmd="$2"

    echo -n "TEST: $name ... "
    local result=0
    set +o pipefail
    # `|| result=$?` rather than `set +e`: errexit stays exactly as the script
    # set it, so a check that returns non-zero reports a FAIL here instead of
    # aborting the run.
    eval "$cmd" > /tmp/test-output 2>&1 || result=$?
    set -o pipefail

    case "$result" in
        0)
            echo "PASS"
            PASS=$((PASS + 1))
            sed 's/^/      /' /tmp/test-output
            ;;
        3)
            echo "SKIP"
            sed 's/^/      /' /tmp/test-output
            ;;
        *)
            echo "FAIL (exit=$result)"
            cat /tmp/test-output
            FAIL=$((FAIL + 1))
            ;;
    esac
}

# The daemon is up when `status --json` reports the bus round trip completed.
#
# Parsed, not grepped. `    [ok] responding` is one line of a report written for
# a person, and this loop pinned it by accident for as long as it existed; the
# document is the surface that promises to keep its shape
# (docs/contracts.md, "facelock status Semantics"). stdout and stderr are
# captured apart because merging them is what makes a JSON document
# unparseable the moment RUST_LOG has something to say.
wait_for_daemon() {
    local deadline=$((SECONDS + 30))
    local document=""
    local errors="/tmp/facelock-status-stderr"

    : > "$errors"
    while [ "$SECONDS" -lt "$deadline" ]; do
        document="$(facelock status --json 2>"$errors" || true)"
        if printf '%s' "$document" \
            | jq -e '.daemon.reachability == "responding"' > /dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done

    printf '%s\n' "$document"
    cat "$errors" >&2 || true
    return 1
}

echo "=== Integration Tests (with camera) ==="
echo ""

# Use the installed config (written by Containerfile), override db path
# to a writable location since default may not be writable in containers
sed -i 's|db_path.*|db_path = "/tmp/facelock-test.db"|' /etc/facelock/config.toml 2>/dev/null || true

# This camera container starts a standalone dbus-daemon, not systemd-logind,
# so it has no authoritative login sessions for the daemon's ProcessFD gate.
# Disable only that gate here; unit/service tests cover its fail-closed and
# local/remote behavior, while the rest of this script keeps exercising the
# real daemon authentication/capture path.
sed -i '/^\[security\]/a abort_if_ssh = false' /etc/facelock/config.toml

# Start a real system bus so CLI commands use the D-Bus daemon path.
mkdir -p /run/dbus
dbus-uuidgen --ensure=/etc/machine-id >/dev/null 2>&1 || true
dbus-daemon --system --fork --nopidfile

# ADR 008 §9: the camera-lifecycle assertions at the end of this script read
# the daemon's own log, so it goes to a file instead of the console — and at
# debug level on the facelock_daemon target, because `releasing camera`, the
# warm-hold line and the frame discards are debug events. The file is dumped
# from `cleanup` when anything failed, so redirecting it costs no diagnostics.
DAEMON_LOG="/tmp/facelock-daemon.log"
: > "$DAEMON_LOG"

cleanup() {
    if [ -n "${DAEMON_PID:-}" ]; then
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
    if [ "$FAIL" -gt 0 ] && [ -s "$DAEMON_LOG" ]; then
        echo ""
        echo "--- daemon log (last 200 lines) ---"
        tail -n 200 "$DAEMON_LOG" || true
    fi
    pkill dbus-daemon 2>/dev/null || true
}
trap cleanup EXIT

# Start daemon in background
RUST_LOG="${RUST_LOG:-facelock=info,facelock_daemon=debug}" facelock daemon > "$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!
sleep 2

# Verify daemon is running
run_test "Daemon responds to ping" \
    "wait_for_daemon" || exit 1

# The report a person reads still says it, in the words it has always used.
# `wait_for_daemon` parses the document now, so without this row nothing here
# would exercise the human renderer at all.
run_test_contains "Status report still names a responding daemon" \
    "facelock status" \
    "\[ok\] responding"

# Every section answers with one of the three verdict words. The filters live in
# files rather than inline so the quoting survives `run_test`'s `eval`.
cat > /tmp/status-sections.jq <<'EOF'
[.config, .daemon, .oneshot_fallback, .camera, .models, .execution_provider,
 .encryption, .enrollment, .security, .notifications, .pam]
| length == 11
  and (map(.state) | all(. == "ok" or . == "problem" or . == "unknown"))
EOF
run_test "Status --json carries a verdict for every section" \
    "facelock status --json | jq -e -f /tmp/status-sections.jq > /dev/null"

# The payoff a setup script wants: the PAM scan as data, with "could not look"
# distinguishable from "nothing configured" without reading prose.
cat > /tmp/status-pam.jq <<'EOF'
.pam.services
| (.state == "ok" or .state == "unknown")
  and (.configured | type) == "array"
  and (.not_checked | type) == "array"
EOF
run_test "Status --json enumerates the PAM scan as data" \
    "facelock status --json | jq -e -f /tmp/status-pam.jq > /dev/null"

# Test device listing
run_test_contains "Device listing works" \
    "facelock devices" \
    "/dev/video" || exit 1

# Test enrollment (will capture faces from camera)
run_test_contains "Enroll a face" \
    "timeout --foreground $LIVE_TIMEOUT facelock enroll --user testuser --label test-face --skip-setup-check" \
    "Face enrolled successfully" || exit 1

# Test listing enrolled models
run_test_contains "List enrolled models" \
    "facelock list --user testuser" \
    "test-face"

# Test authentication via CLI
run_test_contains "Authenticate enrolled face (CLI)" \
    "timeout --foreground $LIVE_TIMEOUT facelock test --user testuser" \
    "Matched model"

# Test authentication via PAM (the real auth path)
run_test "Authenticate enrolled face (PAM)" \
    "timeout --foreground $LIVE_TIMEOUT pamtester facelock-test testuser authenticate"

# --- Device coupling (Plan 02, daemon path) ---
# The daemon runs migrations at startup and records the live camera fingerprint
# in face_models.device_id at enroll. Verify V6 applied, enroll recorded a
# device_id, a forged/mismatched id falls through to no-match, and a legacy NULL
# id still authenticates. (Reads/writes go through the WAL, which the daemon's
# own connection sees on its next query.)
# Resolve the DB path the daemon actually uses (config db_path if uncommented,
# else the compiled default /var/lib/facelock/facelock.db).
# `grep` exiting 1 on no-match must not abort the script (set -e + pipefail), so
# tolerate it; fall back to the compiled default when db_path is unset.
DB="$({ grep -E '^[[:space:]]*db_path[[:space:]]*=' /etc/facelock/config.toml 2>/dev/null || true; } | tail -1 | sed -E 's/^[^=]*=[[:space:]]*"?([^"]*)"?[[:space:]]*$/\1/')"
[ -n "$DB" ] || DB="/var/lib/facelock/facelock.db"

SCHEMA_VER="$(sqlite3 "$DB" 'SELECT MAX(version) FROM schema_version' 2>/dev/null || echo 0)"
run_test "V6 schema migration applied on daemon startup (db=$DB)" \
    "[ \"$SCHEMA_VER\" -ge 6 ]" 0

# --- Encryption at rest (Plan 04, finding #8) ---
# Encryption defaults to keyfile, so the daemon enroll above must have stored
# ciphertext (sealed=1, version byte 0x02, length != raw 2048), and the auth
# tests above already round-tripped enroll->auth through the decrypt path.
# CAMERA-REQUIRED: depends on the live daemon enroll above.
ENC_SEALED="$(sqlite3 "$DB" "SELECT fe.sealed FROM face_embeddings fe JOIN face_models fm ON fe.model_id=fm.id WHERE fm.user='testuser' LIMIT 1" 2>/dev/null || echo '?')"
ENC_LEN="$(sqlite3 "$DB" "SELECT LENGTH(fe.embedding) FROM face_embeddings fe JOIN face_models fm ON fe.model_id=fm.id WHERE fm.user='testuser' LIMIT 1" 2>/dev/null || echo 0)"
ENC_VB="$(sqlite3 "$DB" "SELECT hex(substr(fe.embedding,1,1)) FROM face_embeddings fe JOIN face_models fm ON fe.model_id=fm.id WHERE fm.user='testuser' LIMIT 1" 2>/dev/null || echo '')"
run_test "fresh enroll stores encrypted blob, not raw f32 (sealed=$ENC_SEALED len=$ENC_LEN vb=$ENC_VB)" \
    "[ \"$ENC_SEALED\" = 1 ] && [ \"$ENC_VB\" = '02' ] && [ \"$ENC_LEN\" -ne 2048 ]" 0

DEVID="$(sqlite3 "$DB" "SELECT COALESCE(device_id,'') FROM face_models WHERE user='testuser' LIMIT 1" 2>/dev/null || echo '')"
if [ -n "$DEVID" ]; then
    run_test "enrolled template has non-null device_id (daemon, camera-fingerprinted): '$DEVID'" "true" 0
else
    echo "TEST: enrolled template device_id ... SKIP (live camera exposed no USB identity in-container; coupling degrades to legacy-allow)"
fi

# Swap-in regression gate: a forged, non-matching device_id must fall through to
# no-match, never authenticate.
sqlite3 "$DB" "PRAGMA busy_timeout=8000; UPDATE face_models SET device_id='ffff:ffff:forged' WHERE user='testuser'" || true
echo -n "TEST: facelock test reports no match on forged device_id (coupling; no success) ... "
set +o pipefail
timeout --foreground "$LIVE_TIMEOUT" facelock test --user testuser > /tmp/test-output 2>&1 || true
set -o pipefail
if grep -q "No match" /tmp/test-output; then
    echo "PASS"; PASS=$((PASS + 1))
else
    echo "FAIL"; cat /tmp/test-output; FAIL=$((FAIL + 1))
fi

# Legacy NULL device_id still authenticates (allow-with-warn; no lockout).
sqlite3 "$DB" "PRAGMA busy_timeout=8000; UPDATE face_models SET device_id=NULL WHERE user='testuser'" || true
run_test_contains "facelock test matches again on legacy NULL device_id (daemon)" \
    "timeout --foreground $LIVE_TIMEOUT facelock test --user testuser" \
    "Matched"

# --- Plan 05: rate-limited daemon state must never escalate to a fresh oneshot ---

# Resolve the database the daemon actually uses (config db_path or default).
FACELOCK_DB="$(sed -n 's/^[[:space:]]*db_path[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' /etc/facelock/config.toml | head -1)"
FACELOCK_DB="${FACELOCK_DB:-/var/lib/facelock/facelock.db}"

# Force a rate-limited state by inserting failed attempts directly into the
# shared SQLite window (default: 5 attempts / 60s). Requires enrolled models
# (rate limiting is checked after the has-models pre-check).
run_test "Rate limit: seed failed attempts" \
    "sqlite3 $FACELOCK_DB \"INSERT INTO rate_limit (user, attempt_time) SELECT 'testuser', strftime('%s','now') FROM (VALUES (1),(2),(3),(4),(5),(6));\""

run_test_contains "Rate limit: daemon encodes recoverable error in-band (model_id=-2)" \
    "dbus-send --system --print-reply --dest=org.facelock.Daemon /org/facelock/Daemon org.facelock.Daemon.Authenticate string:testuser" \
    "int32 -2"

# With the in-band encoding the PAM module classifies the error itself
# (rate limited -> PAM_AUTH_ERR) instead of retrying as a root oneshot.
# Swapping a marker stub in at /usr/bin/facelock proves no oneshot child is
# ever spawned (the module spawns that fixed path; an auth_bin config
# redirect would be ignored and make this test vacuous). The daemon keeps
# answering from its already-exec'd binary while the file is swapped.
run_test "Rate limit: PAM fails without oneshot escalation" \
    "printf '#!/bin/bash\ntouch /tmp/oneshot-invoked\nexit 2\n' > /usr/local/bin/oneshot-marker && chmod 755 /usr/local/bin/oneshot-marker && rm -f /tmp/oneshot-invoked && mv /usr/bin/facelock /usr/bin/facelock.orig && install -m 755 /usr/local/bin/oneshot-marker /usr/bin/facelock; timeout 30 pamtester facelock-test testuser authenticate < /dev/null; rc=\$?; mv -f /usr/bin/facelock.orig /usr/bin/facelock; test \$rc -ne 0 && test ! -f /tmp/oneshot-invoked"

run_test "Rate limit: clear seeded attempts" \
    "sqlite3 $FACELOCK_DB \"DELETE FROM rate_limit WHERE user = 'testuser';\""

# --- D-Bus hardening assertions (security plan 06) ---

# ADR 010 left no groups: testuser is just an enrolled local user and
# sigwatcher an unenrolled one, and neither is a member of anything facelock
# knows about. Both may call Authenticate for themselves and nothing else;
# only root receives AuthAttempted.
useradd -m sigwatcher 2>/dev/null || true

# (a) Signal hardening — needs the daemon up plus one auth attempt to emit
# the signal. Unprivileged users must not receive AuthAttempted, and the
# payload must carry no similarity score (no 'double' argument).
runuser -u sigwatcher -- dbus-monitor --system \
    "type='signal',interface='org.facelock.Daemon'" > /tmp/sig-unpriv.log 2>&1 &
UNPRIV_MON_PID=$!
dbus-monitor --system \
    "type='signal',interface='org.facelock.Daemon'" > /tmp/sig-root.log 2>&1 &
ROOT_MON_PID=$!
sleep 2
timeout --foreground "$LIVE_TIMEOUT" facelock test --user testuser > /dev/null 2>&1 || true
sleep 2
kill "$UNPRIV_MON_PID" "$ROOT_MON_PID" 2>/dev/null || true
wait "$UNPRIV_MON_PID" "$ROOT_MON_PID" 2>/dev/null || true

run_test "AuthAttempted signal visible to root monitor" \
    "grep -q 'member=AuthAttempted' /tmp/sig-root.log"

run_test "AuthAttempted payload carries no similarity score" \
    "! grep -A3 'member=AuthAttempted' /tmp/sig-root.log | grep -q 'double'"

run_test "Unprivileged user receives no AuthAttempted signal" \
    "! grep -q 'member=AuthAttempted' /tmp/sig-unpriv.log"

# (a2) ADR 010: Authenticate is open to every local user for its own username
# and nothing else is. sigwatcher is not enrolled, so its own Authenticate is
# answered by the enrollment pre-check (model_id -1, or -3 under
# suppress_unknown) without opening the camera; naming another user is refused
# by the daemon; Ping is refused by the bus.
run_test_contains "A plain local user's Authenticate for its own user reaches the daemon (no model)" \
    "runuser -u sigwatcher -- dbus-send --system --print-reply --reply-timeout=30000 --dest=org.facelock.Daemon /org/facelock/Daemon org.facelock.Daemon.Authenticate string:sigwatcher" \
    "int32 -[13]"

check_denied_as() {
    # $1 = user, $2 = method, $3.. = dbus-send args
    local user="$1" method="$2"
    shift 2
    local out rc
    set +e
    out=$(runuser -u "$user" -- dbus-send --system --print-reply --reply-timeout=30000 \
        --dest=org.facelock.Daemon /org/facelock/Daemon "org.facelock.Daemon.$method" "$@" 2>&1)
    rc=$?
    set -e
    echo "$out"
    [ "$rc" -ne 0 ] || { echo "$method unexpectedly succeeded for $user"; return 1; }
    echo "$out" | grep -qi "AccessDenied" || return 1
    return 0
}
run_test "A plain local user's Authenticate for another user is denied by the daemon" \
    "check_denied_as sigwatcher Authenticate string:testuser"
run_test "A plain local user's Ping is denied by the bus" \
    "check_denied_as sigwatcher Ping"

# Policy: the default context explicitly denies owning the daemon name —
# only root may own org.facelock.Daemon.
check_own_denied() {
    local out rc
    set +e
    out=$(runuser -u sigwatcher -- dbus-send --system --print-reply \
        --dest=org.freedesktop.DBus /org/freedesktop/DBus \
        org.freedesktop.DBus.RequestName string:org.facelock.Daemon uint32:0 2>&1)
    rc=$?
    set -e
    echo "$out"
    [ "$rc" -ne 0 ] || { echo "RequestName unexpectedly succeeded"; return 1; }
    echo "$out" | grep -qiE "not allowed to own|AccessDenied" || return 1
    return 0
}
run_test "Unprivileged user cannot own org.facelock.Daemon" \
    "check_own_denied"

# (b) PreviewDetectFrame authz parity — the intent here has always been that a
# non-root caller obtains no imagery. Under N13 the method became root-only, so
# the way that holds changed: a non-root caller is now denied outright, before
# the method reaches the camera, rather than receiving a reply with jpeg_data
# stripped. This asserts the denial AND that the error reply carries no frame
# bytes (dbus-send renders non-empty byte arrays as hex; a JPEG starts with
# ff d8).
check_preview_detect_frame_denied() {
    local out rc
    set +e
    out=$(runuser -u testuser -- dbus-send --system --print-reply \
        --reply-timeout=60000 \
        --dest=org.facelock.Daemon /org/facelock/Daemon \
        org.facelock.Daemon.PreviewDetectFrame string:testuser 2>&1)
    rc=$?
    set -e
    echo "$out"
    [ "$rc" -ne 0 ] || {
        echo "PreviewDetectFrame unexpectedly succeeded for a non-root caller"
        return 1
    }
    echo "$out" | grep -qi "AccessDenied" || return 1
    if echo "$out" | grep -qi "ff d8"; then
        echo "denial reply contains JPEG frame bytes (ff d8)"
        return 1
    fi
    return 0
}
run_test "PreviewDetectFrame denies non-root caller (no raw frame)" \
    "check_preview_detect_frame_denied"

# Release the preview camera session before the concurrency test
dbus-send --system --print-reply --dest=org.facelock.Daemon /org/facelock/Daemon \
    org.facelock.Daemon.ReleaseCamera > /dev/null 2>&1 || true

# (c) CAMERA-REQUIRED: mutex-DoS guard — race two simultaneous Authenticate
# calls. Exactly one wins the capture slot; the other must be rejected
# immediately with a "busy" error (milliseconds), never queued toward the
# 10s handler-lock timeout.
timed_authenticate() {
    # $1 = output file, $2 = meta file (rc + elapsed_ms), $3 = run as user ("" = root)
    local s e rc
    s=$(date +%s%N)
    if [ -n "$3" ]; then
        runuser -u "$3" -- dbus-send --system --print-reply --reply-timeout=30000 \
            --dest=org.facelock.Daemon /org/facelock/Daemon \
            org.facelock.Daemon.Authenticate string:testuser > "$1" 2>&1
        rc=$?
    else
        dbus-send --system --print-reply --reply-timeout=30000 \
            --dest=org.facelock.Daemon /org/facelock/Daemon \
            org.facelock.Daemon.Authenticate string:testuser > "$1" 2>&1
        rc=$?
    fi
    e=$(date +%s%N)
    echo "$rc $(((e - s) / 1000000))" > "$2"
}

check_concurrent_auth_busy() {
    set +e
    timed_authenticate /tmp/auth-a.out /tmp/auth-a.meta "" &
    local pid_a=$!
    timed_authenticate /tmp/auth-b.out /tmp/auth-b.meta testuser &
    local pid_b=$!
    wait "$pid_a" "$pid_b"
    set -e

    local rc_a ms_a rc_b ms_b
    read -r rc_a ms_a < /tmp/auth-a.meta
    read -r rc_b ms_b < /tmp/auth-b.meta
    echo "call A (root):     rc=$rc_a elapsed=${ms_a}ms"
    echo "call B (testuser): rc=$rc_b elapsed=${ms_b}ms"
    echo "--- A reply:"
    cat /tmp/auth-a.out
    echo "--- B reply:"
    cat /tmp/auth-b.out

    local busy=0 busy_ms=0
    if grep -qi "busy" /tmp/auth-a.out; then
        busy=$((busy + 1))
        busy_ms=$ms_a
    fi
    if grep -qi "busy" /tmp/auth-b.out; then
        busy=$((busy + 1))
        busy_ms=$ms_b
    fi
    [ "$busy" -eq 1 ] || { echo "expected exactly one busy rejection, got $busy"; return 1; }
    # Rejected immediately — well under the 10s handler-lock stall
    [ "$busy_ms" -lt 5000 ] || { echo "busy rejection took ${busy_ms}ms (stall)"; return 1; }
    return 0
}
run_test "Concurrent Authenticate rejected immediately with busy" \
    "check_concurrent_auth_busy"

# The busy guard must not starve legitimate sequential auth: once the
# concurrent capture finished, a new Authenticate must run a full capture
# (match or no-match depending on who is in front of the camera), and must
# NOT be rejected with a busy error.
check_sequential_auth_not_starved() {
    local out rc
    set +e
    out=$(timeout --foreground "$LIVE_TIMEOUT" facelock test --user testuser 2>&1)
    rc=$?
    set -e
    echo "$out"
    if echo "$out" | grep -qi "busy"; then
        echo "sequential auth was rejected as busy (starved by the guard)"
        return 1
    fi
    echo "$out" | grep -qE "Matched model|No match" || return 1
    return 0
}
run_test "Sequential auth not starved after busy rejection" \
    "check_sequential_auth_not_starved"

# --- ADR 008: camera lifecycle (outcome-based hold, cancellation) ---
#
# The invariant these assertions pin is §1: the camera is on only while an
# authentication is in progress or a retry is plausibly imminent. Success and
# cancellation end the interaction, so the stream closes at once; only a failed
# attempt keeps it warm, and only for device.camera_release_secs. Everything is
# read from the daemon's own log, whose strings live in
# crates/facelock-daemon/src/handler.rs (`releasing camera`, `holding camera
# warm after a failed attempt`), src/auth.rs (`authentication cancelled`) and
# src/server.rs (`caller disconnected, cancelling in-flight request`).

# Milliseconds since the epoch.
now_ms() {
    echo $(( $(date +%s%N) / 1000000 ))
}

# Lines currently in the daemon log — the "from here on" marker every
# assertion takes before it provokes the daemon.
daemon_log_lines() {
    wc -l < "$DAEMON_LOG" 2>/dev/null || echo 0
}

# ADR 008 §9: wait for a daemon log line that contains EVERY pattern given,
# looking only at lines added after line $1, for at most $2 milliseconds.
# Echoes how long the wait took, in milliseconds; returns 1 on timeout.
#
# The patterns are applied one at a time to the same line rather than as a
# single string because tracing writes ANSI styling between a field's name and
# its value, so `reason="warm hold expired"` is not contiguous in the file.
wait_for_daemon_log() {
    local from_line="$1"
    local timeout_ms="$2"
    shift 2
    local start deadline matched pat
    start=$(now_ms)
    deadline=$((start + timeout_ms))
    while :; do
        matched="$(tail -n "+$((from_line + 1))" "$DAEMON_LOG" 2>/dev/null || true)"
        for pat in "$@"; do
            matched="$(printf '%s\n' "$matched" | grep -F -- "$pat" || true)"
        done
        if [ -n "$matched" ]; then
            echo $(( $(now_ms) - start ))
            return 0
        fi
        [ "$(now_ms)" -lt "$deadline" ] || return 1
        sleep 0.05
    done
}

# ADR 008 §9, acceptance (1): a successful authentication ends the interaction,
# so `finish(Success)` drops the camera before the reply is even sent. The
# release line must therefore already be in the log by the time the client has
# its answer; the 500 ms budget is for the log write, not for the daemon.
#
# The precondition reads the daemon's own log rather than the client's output.
# `facelock test` prints "Matched ..." through gettext and exits 0 whether it
# matched or not, so the CLI offers this check neither a stable string nor a
# usable exit code; the daemon's `tracing` lines are never translated.
adr008_success_releases_camera() {
    local from elapsed
    from=$(daemon_log_lines)
    timeout --foreground "$LIVE_TIMEOUT" facelock test --user testuser \
        > /tmp/adr008-success.log 2>&1 || true
    if ! wait_for_daemon_log "$from" 500 "authentication succeeded" > /dev/null; then
        cat /tmp/adr008-success.log
        echo "no successful authentication to observe (needs a live enrolled face)"
        return 3
    fi
    if ! elapsed=$(wait_for_daemon_log "$from" 500 "releasing camera" "authentication succeeded"); then
        echo "camera not released within 500ms of a successful reply"
        tail -n "+$((from + 1))" "$DAEMON_LOG"
        return 1
    fi
    echo "camera released ${elapsed}ms after the successful reply"
}

# ADR 008 §9, acceptance (2): a failed attempt is the one outcome that predicts
# a retry, so the stream stays open for device.camera_release_secs and then
# closes. Runs inside the forged-device_id window below, which is what makes
# the failure certain rather than dependent on who is in front of the camera.
adr008_failure_holds_then_releases() {
    local secs from after elapsed lo hi
    secs="$({ grep -E '^[[:space:]]*camera_release_secs[[:space:]]*=' /etc/facelock/config.toml \
        || true; } | tail -1 | sed -E 's/[^0-9]//g')"
    [ -n "$secs" ] || secs=3
    if [ "$secs" -eq 0 ]; then
        echo "camera_release_secs = 0: the daemon never holds, nothing to time"
        return 3
    fi

    from=$(daemon_log_lines)
    timeout --foreground "$LIVE_TIMEOUT" facelock test --user testuser \
        > /tmp/adr008-failure.log 2>&1 || true
    after=$(daemon_log_lines)

    if ! wait_for_daemon_log "$from" 0 "holding camera warm after a failed attempt" > /dev/null; then
        cat /tmp/adr008-failure.log
        echo "the attempt did not end in a failure the daemon holds the camera for"
        return 3
    fi
    if ! elapsed=$(wait_for_daemon_log "$after" $(( secs * 1000 + 1500 )) "releasing camera" "warm hold expired"); then
        echo "warm hold never expired (camera still open ${secs}s + 1.5s after the failed attempt)"
        tail -n "+$((from + 1))" "$DAEMON_LOG"
        return 1
    fi
    lo=$(( secs * 1000 - 500 ))
    hi=$(( secs * 1000 + 500 ))
    if [ "$elapsed" -lt "$lo" ] || [ "$elapsed" -gt "$hi" ]; then
        echo "warm hold expired ${elapsed}ms after the reply, expected ${secs}000ms +/-500ms"
        return 1
    fi
    echo "warm hold expired ${elapsed}ms after the failed reply (camera_release_secs=${secs})"
}

# Has the daemon reached the per-frame scan loop since line $1? Any of these
# three debug lines carries a frame number, so seeing one means the camera is
# open, the warmup discard is done, and the request is somewhere it can be
# cancelled per frame.
daemon_is_scanning() {
    tail -n "+$(($1 + 1))" "$DAEMON_LOG" 2>/dev/null \
        | grep -qE "face comparison|no faces detected|dark frame"
}

# ADR 008 §9, acceptance (1) and §5: a caller that goes away is not a slow
# user. SIGKILLing the client drops its bus connection, the daemon's name-owner
# watch fires, and the request ends within one frame instead of running to
# recognition.timeout_secs with the emitter lit.
#
# The kill waits for the scan rather than firing at a fixed offset: a client
# that dies during the camera open aborts the acquire instead (ADR 008 §8),
# which is a different path with different log lines, so a fixed offset would
# be timing the camera open on whatever hardware happens to be mounted.
adr008_caller_departure_cancels() {
    local from client deadline cancel_ms auth_ms release_ms
    from=$(daemon_log_lines)
    facelock test --user testuser > /tmp/adr008-cancel.log 2>&1 &
    client=$!
    deadline=$(( $(now_ms) + 20000 ))
    while [ "$(now_ms)" -lt "$deadline" ]; do
        daemon_is_scanning "$from" && break
        kill -0 "$client" 2>/dev/null || break
        sleep 0.05
    done
    if ! daemon_is_scanning "$from"; then
        kill -9 "$client" 2>/dev/null || true
        wait "$client" 2>/dev/null || true
        cat /tmp/adr008-cancel.log
        echo "the daemon never reached the scan loop; nothing was in flight to cancel"
        return 3
    fi
    kill -9 "$client" 2>/dev/null || true
    wait "$client" 2>/dev/null || true
    if ! cancel_ms=$(wait_for_daemon_log "$from" 500 "caller disconnected, cancelling in-flight request"); then
        echo "daemon did not notice the caller's departure within 500ms"
        tail -n "+$((from + 1))" "$DAEMON_LOG"
        return 1
    fi
    if ! auth_ms=$(wait_for_daemon_log "$from" 500 "authentication cancelled"); then
        echo "request did not end as cancelled within 500ms of the kill"
        tail -n "+$((from + 1))" "$DAEMON_LOG"
        return 1
    fi
    if ! release_ms=$(wait_for_daemon_log "$from" 500 "releasing camera" "request cancelled"); then
        echo "camera not released within 500ms of the cancellation"
        tail -n "+$((from + 1))" "$DAEMON_LOG"
        return 1
    fi
    echo "caller departure seen at ${cancel_ms}ms, attempt cancelled at ${auth_ms}ms, camera released at ${release_ms}ms"
}

run_test_or_skip "ADR 008: success releases the camera immediately" \
    "adr008_success_releases_camera"

# The two checks below need an attempt that does NOT match: one to observe the
# warm hold a failure earns, one to have something still in flight to cancel.
# A forged device_id makes every enrolled template ineligible, so the attempt
# sees a face and matches nothing, and the scan runs to recognition
# .timeout_secs instead of ending on the first frame. `facelock test` charges
# no rate-limit budget (AuthIntent::Test) and a cancelled attempt charges none
# either, so neither costs the later tests an attempt.
#
# Captured as a SQL literal so the window really is a window: `quote()` renders
# a real fingerprint as a quoted string and a legacy row as the bare token
# NULL, which is the one distinction a plain SELECT loses — and writing NULL
# back unconditionally would silently un-couple the template from its camera
# for every test after this point.
#
# The busy timeout arrives as the `.timeout` dot command, not as a PRAGMA in
# the statement: `PRAGMA busy_timeout=8000` returns a row, so sqlite3 prints
# 8000 ahead of the SELECT and the restore below interpolates both tokens.
ADR008_DEVID="$(sqlite3 -cmd ".timeout 8000" "$DB" "SELECT quote(device_id) FROM face_models WHERE user='testuser' LIMIT 1" 2>/dev/null || echo 'NULL')"
# Two forms are safe to write back: the bare token NULL, or a single-quoted
# string with interior quotes doubled, which is all quote() ever emits.
# Anything else restores NULL instead of a syntax error the `|| true` hides.
[[ "$ADR008_DEVID" =~ ^(NULL|\'([^\']|\'\')*\')$ ]] || ADR008_DEVID='NULL'
sqlite3 "$DB" "PRAGMA busy_timeout=8000; UPDATE face_models SET device_id='ffff:ffff:forged' WHERE user='testuser'" || true

run_test_or_skip "ADR 008: a failed attempt holds the camera, then releases it at camera_release_secs" \
    "adr008_failure_holds_then_releases"

run_test_or_skip "ADR 008: a departed caller cancels the request and closes the camera" \
    "adr008_caller_departure_cancels"

sqlite3 "$DB" "PRAGMA busy_timeout=8000; UPDATE face_models SET device_id=$ADR008_DEVID WHERE user='testuser'" || true

# The restore runs under `|| true`, so assert it instead of trusting it. Every
# test after this point reads the row, and a forged id left in place makes them
# pass against a template no camera can ever match again.
ADR008_RESTORED="$(sqlite3 -cmd ".timeout 8000" "$DB" "SELECT quote(device_id) FROM face_models WHERE user='testuser' LIMIT 1" 2>/dev/null || echo 'NULL')"
[ -n "$ADR008_RESTORED" ] || ADR008_RESTORED='NULL'
run_test "ADR 008: device_id survives the forged-id window (want=$ADR008_DEVID got=$ADR008_RESTORED)" \
    "[ \"\$ADR008_RESTORED\" = \"\$ADR008_DEVID\" ]" 0

# --- End ADR 008 camera lifecycle ---

# Clean up
run_test "Clear enrolled models" \
    "facelock clear --user testuser --yes"

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
