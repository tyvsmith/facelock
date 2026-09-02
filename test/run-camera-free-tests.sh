#!/bin/bash
# Camera-free half of the two end-to-end tiers (#139).
#
# `run-integration-tests.sh` and `run-oneshot-tests.sh` need /dev/video* and a
# person in frame, so they run on one machine and nothing gates them. Every
# assertion in those two scripts that never opens a camera lives HERE instead,
# where CI runs it on every pull request. The three rotted tests that #139
# describes were all of that kind: two D-Bus authorization checks and a
# classification rule, none of which needed a capture to be wrong.
#
# The boundary rule, applied conservatively:
#
#   moves    the assertion's subject is reached before any capture — a bus
#            policy decision, a pre-flight rejection, a schema migration, a
#            report rendered from machine state
#   stays    the assertion reads something only a real sensor produces — a
#            frame, a match, a device fingerprint, a warm-hold timing
#
# An assertion that merely *happens* to be cheap does not move; an assertion
# whose subject is camera-bound stays even when a headless stand-in could be
# rigged to make it pass. A test that passes without exercising what it names
# is worse than one that does not run.
#
# What this tier still needs, and why it is not "no dependencies":
#
#   * the ONNX models. `facelock daemon` refuses to start when they are
#     missing (crates/facelock-cli/src/commands/daemon.rs), and half of what
#     moved here is daemon-side authorization. CI fetches them before building
#     the image; FACELOCK_ALLOW_MISSING_MODELS=1 downgrades the daemon block
#     to a loud skip for a local run that only wants the one-shot half.
#   * a pinned device.path. Auto-detection is a HARD failure with no
#     /dev/video* (it is the one camera dependency that is not about capture),
#     while a pinned path that does not resolve is tolerated by design: the
#     daemon starts with unqueryable caps and auth falls through to password.
#     Pinning is therefore how this tier reaches the code the hardware tiers
#     reach, not a way of faking a camera.
set -euo pipefail

PASS=0
FAIL=0
SKIPPED=0

# A device node that cannot exist. Chosen over a made-up /dev/videoN so a real
# camera appearing on the host can never silently take part in this tier.
ABSENT_DEVICE="/dev/video-camera-free-none"

CONFIG="/etc/facelock/config.toml"
# The compiled default. Left unpinned on purpose: the daemon refuses to manage
# a shared system directory, so a db_path directly under /tmp aborts startup
# ("refusing to manage shared system directory /tmp"). The one-shot block below
# pins its own paths, which that rule does not reach.
DB="/var/lib/facelock/facelock.db"
DAEMON_LOG="/tmp/facelock-camera-free-daemon.log"

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
    fi

    echo "FAIL (exit=$result, expected=$expected_result)"
    cat /tmp/test-output
    FAIL=$((FAIL + 1))
    return 1
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

# Announce a block that could not run, and count it. A skip is not a pass and
# must not read like one in a log someone skims.
skip_block() {
    echo "SKIP: $1"
    SKIPPED=$((SKIPPED + 1))
}

# Force db_path in a config file (uncomment/replace, or append [storage] if
# absent). Lifted from run-oneshot-tests.sh, which owns the pre-V6 migration
# assertion that moved here with it.
set_db_path() {
    local cfg="$1" path="$2"
    if grep -qE '^[[:space:]]*#?[[:space:]]*db_path' "$cfg"; then
        sed -i -E "s|^[[:space:]]*#?[[:space:]]*db_path.*|db_path = \"$path\"|" "$cfg"
    elif grep -qE '^\[storage\]' "$cfg"; then
        sed -i "/^\[storage\]/a db_path = \"$path\"" "$cfg"
    else
        printf '\n[storage]\ndb_path = "%s"\n' "$path" >> "$cfg"
    fi
}

# Pin device.path under the [device] header. Inserted rather than
# sed-replacing `path` lines: `path` is not a unique key name ([audit] has one
# too), so a blind substitution rewrites the wrong line.
pin_device_path() {
    local cfg="$1" node="$2"
    if grep -qE '^\[device\]' "$cfg"; then
        sed -i "/^\[device\]/a path = \"$node\"" "$cfg"
    else
        printf '\n[device]\npath = "%s"\n' "$node" >> "$cfg"
    fi
}

echo "=== Camera-free E2E Tests (no /dev/video*, no live face) ==="
echo ""

if [ -e /dev/video0 ]; then
    echo "note: /dev/video0 exists in this container; device.path is pinned to"
    echo "      $ABSENT_DEVICE regardless, so nothing here can capture."
fi

pin_device_path "$CONFIG" "$ABSENT_DEVICE"

# This container starts a standalone dbus-daemon, not systemd-logind, so it has
# no authoritative login sessions for the daemon's ProcessFD gate. Disabled for
# the same reason run-integration-tests.sh disables it; unit and service tests
# cover the gate's fail-closed and local/remote behavior.
sed -i '/^\[security\]/a abort_if_ssh = false' "$CONFIG"

# ADR 010: sigwatcher is an unenrolled plain local user, a member of nothing
# facelock knows about.
useradd -m sigwatcher 2>/dev/null || true

mkdir -p /run/dbus
dbus-uuidgen --ensure=/etc/machine-id >/dev/null 2>&1 || true
dbus-daemon --system --fork --nopidfile

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

# The daemon is up when `status --json` reports the bus round trip completed.
# Parsed, not grepped: `    [ok] responding` is one line of a report written
# for a person (docs/contracts.md, "facelock status Semantics"). stdout and
# stderr are captured apart because merging them is what makes a JSON document
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

# ============================================================================
# Daemon block — needs the ONNX models, never a camera.
# ============================================================================

MODEL_DIR="/var/lib/facelock/models"
DAEMON_BLOCK=1
for m in scrfd_2.5g_bnkps.onnx w600k_r50.onnx; do
    [ -f "$MODEL_DIR/$m" ] || DAEMON_BLOCK=0
done

if [ "$DAEMON_BLOCK" -eq 0 ] && [ "${FACELOCK_ALLOW_MISSING_MODELS:-0}" != "1" ]; then
    echo "error: $MODEL_DIR is missing the required ONNX models, so the daemon" >&2
    echo "       half of this tier cannot run. The models are not a camera" >&2
    echo "       dependency: 'facelock daemon' verifies them at startup." >&2
    echo "" >&2
    echo "       Build the image from a checkout with models/ populated:" >&2
    echo "         just link-models" >&2
    echo "       CI fetches them from the 'assets' release before building." >&2
    echo "" >&2
    echo "       To run only the one-shot half, with the daemon assertions" >&2
    echo "       reported as skipped: FACELOCK_ALLOW_MISSING_MODELS=1" >&2
    exit 1
fi

if [ "$DAEMON_BLOCK" -eq 0 ]; then
    skip_block "daemon assertions (no ONNX models, FACELOCK_ALLOW_MISSING_MODELS=1)"
else
    : > "$DAEMON_LOG"
    RUST_LOG="${RUST_LOG:-facelock=info,facelock_daemon=debug}" facelock daemon \
        > "$DAEMON_LOG" 2>&1 &
    DAEMON_PID=$!
    sleep 2

    run_test "Daemon responds to ping" \
        "wait_for_daemon" || exit 1

    # The report a person reads still says it, in the words it has always
    # used. `wait_for_daemon` parses the document, so without this row nothing
    # exercises the human renderer at all.
    run_test_contains "Status report still names a responding daemon" \
        "facelock status" \
        "\[ok\] responding"

    # Every section answers with one of the three verdict words. The filters
    # live in files rather than inline so the quoting survives `run_test`'s
    # `eval`. The camera section reports `problem` here and `ok` on the
    # hardware tier; the assertion is that a verdict exists, not which one.
    cat > /tmp/status-sections.jq <<'EOF'
[.config, .daemon, .oneshot_fallback, .camera, .models, .execution_provider,
 .encryption, .enrollment, .security, .notifications, .pam]
| length == 11
  and (map(.state) | all(. == "ok" or . == "problem" or . == "unknown"))
EOF
    run_test "Status --json carries a verdict for every section" \
        "facelock status --json | jq -e -f /tmp/status-sections.jq > /dev/null"

    # The payoff a setup script wants: the PAM scan as data, with "could not
    # look" distinguishable from "nothing configured" without reading prose.
    cat > /tmp/status-pam.jq <<'EOF'
.pam.services
| (.state == "ok" or .state == "unknown")
  and (.configured | type) == "array"
  and (.not_checked | type) == "array"
EOF
    run_test "Status --json enumerates the PAM scan as data" \
        "facelock status --json | jq -e -f /tmp/status-pam.jq > /dev/null"

    # The daemon runs migrations at startup, before any request. On the
    # hardware tier this row sits after the live enroll and reads the same
    # fact; nothing about it needed the enroll.
    SCHEMA_VER="$(sqlite3 "$DB" 'SELECT MAX(version) FROM schema_version' 2>/dev/null || echo 0)"
    run_test "V6 schema migration applied on daemon startup (db=$DB)" \
        "[ \"$SCHEMA_VER\" -ge 6 ]" 0

    # --- ADR 010 authorization (security plan 06) ---
    #
    # ADR 010 left no groups: every local user may call Authenticate for its
    # own username and nothing else. sigwatcher is not enrolled, so its own
    # Authenticate is answered by the enrollment pre-check (model_id -1, or -3
    # under suppress_unknown) WITHOUT opening the camera — which is what makes
    # this whole block camera-free rather than merely camera-tolerant.
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

    # PreviewDetectFrame authz parity — the intent has always been that a
    # non-root caller obtains no imagery. Under N13 the method became
    # root-only, so a non-root caller is denied outright, BEFORE the method
    # reaches the camera. That "before" is why the check belongs here: this is
    # one of the two assertions #139 records as having rotted undetected, and
    # neither of them ever needed a frame to be right or wrong. Asserts the
    # denial AND that the error reply carries no frame bytes (dbus-send
    # renders non-empty byte arrays as hex; a JPEG starts with ff d8).
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

    # --- AuthAttempted signal hardening (security plan 06) ---
    #
    # Provoked with an Authenticate for an unenrolled testuser: the daemon
    # answers -1 from the pre-check and emits the signal on the way out
    # (crates/facelock-daemon/src/server.rs), so the broadcast happens without
    # a capture. The hardware tier provokes it with a real `facelock test`;
    # what is asserted — who receives it, and that the payload carries no
    # similarity score — is the same either way.
    runuser -u sigwatcher -- dbus-monitor --system \
        "type='signal',interface='org.facelock.Daemon'" > /tmp/sig-unpriv.log 2>&1 &
    UNPRIV_MON_PID=$!
    dbus-monitor --system \
        "type='signal',interface='org.facelock.Daemon'" > /tmp/sig-root.log 2>&1 &
    ROOT_MON_PID=$!
    sleep 2
    dbus-send --system --print-reply --reply-timeout=30000 \
        --dest=org.facelock.Daemon /org/facelock/Daemon \
        org.facelock.Daemon.Authenticate string:testuser > /dev/null 2>&1 || true
    sleep 2
    kill "$UNPRIV_MON_PID" "$ROOT_MON_PID" 2>/dev/null || true
    wait "$UNPRIV_MON_PID" "$ROOT_MON_PID" 2>/dev/null || true

    run_test "AuthAttempted signal visible to root monitor" \
        "grep -q 'member=AuthAttempted' /tmp/sig-root.log"

    run_test "AuthAttempted payload carries no similarity score" \
        "! grep -A3 'member=AuthAttempted' /tmp/sig-root.log | grep -q 'double'"

    run_test "Unprivileged user receives no AuthAttempted signal" \
        "! grep -q 'member=AuthAttempted' /tmp/sig-unpriv.log"

    # --- Plan 05: rate-limited daemon state must never escalate to a fresh
    # one-shot ---
    #
    # Rate limiting is checked AFTER the has-models pre-check and BEFORE the
    # camera is acquired (crates/facelock-daemon/src/auth.rs, pre_check), so
    # the only thing standing between this tier and the assertion is a row in
    # face_models. Seeded directly rather than enrolled: the subject is the
    # rate limiter's reply encoding, not the template, and a synthetic row
    # reaches the same gate a real one does. Nothing below ever gets far
    # enough to read the embedding.
    # `.timeout` rather than a busy_timeout PRAGMA: the PRAGMA returns a row,
    # so sqlite3 prints 8000 into the middle of the test output.
    sqlite3 -cmd ".timeout 8000" "$DB" "INSERT INTO face_models (user, label, created_at, embedder_model) VALUES ('testuser', 'rate-limit-fixture', strftime('%s','now'), 'w600k_r50.onnx');"

    run_test "Rate limit: seed failed attempts" \
        "sqlite3 $DB \"INSERT INTO rate_limit (user, attempt_time) SELECT 'testuser', strftime('%s','now') FROM (VALUES (1),(2),(3),(4),(5),(6));\""

    run_test_contains "Rate limit: daemon encodes recoverable error in-band (model_id=-2)" \
        "dbus-send --system --print-reply --dest=org.facelock.Daemon /org/facelock/Daemon org.facelock.Daemon.Authenticate string:testuser" \
        "int32 -2"

    # With the in-band encoding the PAM module classifies the error itself
    # (rate limited -> PAM_AUTH_ERR) instead of retrying as a root one-shot.
    # Swapping a marker stub in at /usr/bin/facelock proves no one-shot child
    # is ever spawned (the module spawns that fixed path; an auth_bin config
    # redirect would be ignored and make this test vacuous). The daemon keeps
    # answering from its already-exec'd binary while the file is swapped.
    run_test "Rate limit: PAM fails without oneshot escalation" \
        "printf '#!/bin/bash\ntouch /tmp/oneshot-invoked\nexit 2\n' > /usr/local/bin/oneshot-marker && chmod 755 /usr/local/bin/oneshot-marker && rm -f /tmp/oneshot-invoked && mv /usr/bin/facelock /usr/bin/facelock.orig && install -m 755 /usr/local/bin/oneshot-marker /usr/bin/facelock; timeout 30 pamtester facelock-test testuser authenticate < /dev/null; rc=\$?; mv -f /usr/bin/facelock.orig /usr/bin/facelock; test \$rc -ne 0 && test ! -f /tmp/oneshot-invoked"

    run_test "Rate limit: clear seeded attempts" \
        "sqlite3 $DB \"DELETE FROM rate_limit WHERE user = 'testuser';\""

    sqlite3 -cmd ".timeout 8000" "$DB" "DELETE FROM face_models WHERE label = 'rate-limit-fixture';" || true

    # #314: with a daemon owning the bus, a command under a non-default
    # `--config` still never enrolls through it. The daemon reads only the
    # default file, so the CLI selects direct access and says so, before it
    # tries the (absent) camera itself. The daemon transport's own failure
    # shapes are forbidden: had the request reached the bus, one of them is
    # what this run would have printed instead.
    cp "$CONFIG" /tmp/facelock-override-enroll.toml
    run_test "Enroll under --config bypasses the running daemon" \
        "facelock --config /tmp/facelock-override-enroll.toml enroll --user testuser --skip-setup-check < /dev/null > /tmp/enroll-override.out 2>&1; test \$? -ne 0 && grep -q 'Note: --config names a file other than /etc/facelock/config.toml' /tmp/enroll-override.out && ! grep -q 'D-Bus Enroll call failed' /tmp/enroll-override.out && ! grep -q 'enrollment timed out client-side' /tmp/enroll-override.out"
    rm -f /tmp/facelock-override-enroll.toml

    # The daemon is done: the one-shot block below must answer with no daemon
    # on the bus, exactly as it does on the hardware tier.
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
    DAEMON_PID=""
fi

# ============================================================================
# Setup under a config override (#314) — needs neither the models nor a
# camera, and runs as root, which is what makes the refusal reachable: the
# root gate answers first for anyone else (cli_smoke.rs pins that ordering).
#
# The packaged unit runs bare `facelock daemon` and reads only the default
# config file, so `--systemd` under any other `--config` would enable a
# daemon configured by a file setup never touched. Refused before the base
# flow writes anything and before any `systemctl` verb. `systemctl` is
# shadowed on the fixed PATH setup execs privileged commands from with a
# recording shim, so "never invoked" is a file that does not exist rather
# than a verb that happened to fail.
# ============================================================================

echo ""
echo "--- setup: --systemd under --config ---"

OVERRIDE_CONFIG="/tmp/facelock-override-setup.toml"
cp "$CONFIG" "$OVERRIDE_CONFIG"
SYSTEMCTL_CALLS="/tmp/facelock-systemctl-calls"
rm -f "$SYSTEMCTL_CALLS"
REAL_SYSTEMCTL=""
if [ -e /usr/bin/systemctl ]; then
    REAL_SYSTEMCTL="/tmp/facelock-systemctl.real"
    mv /usr/bin/systemctl "$REAL_SYSTEMCTL"
fi
printf '#!/bin/sh\necho "$*" >> %s\nexit 1\n' "$SYSTEMCTL_CALLS" > /usr/bin/systemctl
chmod 0755 /usr/bin/systemctl
rm -f /etc/facelock/.setup-complete

# The base flow: refused ahead of its first line ("preparing system"), so
# no directory, model, key or marker is written.
run_test "setup --systemd --non-interactive under --config is refused before any mutation" \
    "facelock --config $OVERRIDE_CONFIG setup --systemd --non-interactive > /tmp/setup-override.out 2>&1; test \$? -ne 0 && grep -q -- '--systemd is not supported with --config $OVERRIDE_CONFIG' /tmp/setup-override.out && grep -q 're-run without --systemd' /tmp/setup-override.out && ! grep -q 'preparing system' /tmp/setup-override.out && ! test -e $SYSTEMCTL_CALLS && ! test -e /etc/facelock/.setup-complete"

# The standalone form has no base flow: `run_systemd` asks the same question
# itself, after its own root and systemd checks and before the legacy-asset
# migration. This container has no systemd, so the directory that check
# looks for is created for the one row and removed again.
mkdir -p /run/systemd/system
run_test "setup --systemd (standalone) under --config is refused before systemctl" \
    "facelock --config $OVERRIDE_CONFIG setup --systemd > /tmp/setup-override-standalone.out 2>&1; test \$? -ne 0 && grep -q -- '--systemd is not supported with --config $OVERRIDE_CONFIG' /tmp/setup-override-standalone.out && ! grep -q 'Validating installed' /tmp/setup-override-standalone.out && ! test -e $SYSTEMCTL_CALLS"

# The default path is not an override, however it is spelled: the same
# standalone invocation naming the real file through a `..` component gets
# past the identity check and on to the asset validation, which is where
# this container (no installed unit, a shim for systemctl) stops it. The
# row asserts the refusal did not fire and the validation was reached.
run_test "setup --systemd under --config /etc/facelock/../facelock/config.toml is not refused" \
    "facelock --config /etc/facelock/../facelock/config.toml setup --systemd > /tmp/setup-default-spelling.out 2>&1; ! grep -q -- '--systemd is not supported' /tmp/setup-default-spelling.out && grep -q 'Validating installed' /tmp/setup-default-spelling.out"
rmdir /run/systemd/system 2>/dev/null || true

rm -f /usr/bin/systemctl
if [ -n "$REAL_SYSTEMCTL" ]; then
    mv "$REAL_SYSTEMCTL" /usr/bin/systemctl
fi
rm -f "$OVERRIDE_CONFIG" "$SYSTEMCTL_CALLS"

# ============================================================================
# One-shot block — needs neither the models nor a camera.
#
# `facelock auth` opens the store (running migrations) and runs the daemon's
# pre-flight gates before it loads the ONNX engine or touches the camera
# (crates/facelock-cli/src/commands/auth.rs), so every rejection that a gate
# produces is reachable here.
# ============================================================================

echo ""
echo "--- one-shot (daemonless) ---"

sed -i '/^\[daemon\]/a mode = "oneshot"' "$CONFIG"

# facelock auth rejects unknown user. Exit 2 is "no opinion": the camera never
# opened, so exit 1's "scanned and not matched" is not available to this path
# by construction (docs/contracts.md, "facelock auth Exit Codes").
run_test "facelock auth rejects unknown user" \
    "facelock auth --user nobody --config $CONFIG" \
    2

# Exit 2 carries more classes than it used to: the storage class moved onto it
# when a failed embeddings load and a failed model-list read stopped exiting 1
# (#141). A broken database would now answer this row with the same number the
# enrollment gate does. So pin the code to the gate that produced it, the way
# run-oneshot-tests.sh pins the require_ir refusal, and the row cannot start
# passing for a different reason than it was written for. `AuthResult` is the
# not-enrolled short-circuit; every error class renders as `Error {`.
run_test "the exit 2 above is the enrollment gate, not a storage fault" \
    "RUST_LOG=debug facelock auth --user nobody --config $CONFIG 2>&1 | grep 'pre-check short-circuit' | grep -q 'AuthResult(MatchResult'"

# A fresh database migrates on first open by the one-shot binary, not only by
# the daemon. Given its own db_path so the daemon's already-migrated file
# cannot make this vacuous.
FRESH="/tmp/facelock-oneshot-fresh.db"
rm -f "$FRESH" "$FRESH-wal" "$FRESH-shm"
cp "$CONFIG" /tmp/facelock-fresh.toml
set_db_path /tmp/facelock-fresh.toml "$FRESH"
facelock auth --user nobody --config /tmp/facelock-fresh.toml >/dev/null 2>&1 || true
FRESH_VER="$(sqlite3 "$FRESH" 'SELECT MAX(version) FROM schema_version' 2>/dev/null || echo 0)"
FRESH_COL="$(sqlite3 "$FRESH" "SELECT COUNT(*) FROM pragma_table_info('face_models') WHERE name='device_id'" 2>/dev/null || echo 0)"
run_test "V6 schema migration applied (oneshot; fresh db v=$FRESH_VER col=$FRESH_COL)" \
    "[ \"$FRESH_VER\" -ge 6 ] && [ \"$FRESH_COL\" = 1 ]" 0
rm -f "$FRESH" "$FRESH-wal" "$FRESH-shm" /tmp/facelock-fresh.toml

# A pre-V6 database migrates cleanly on open: seed schema V5, open it via a
# store-opening command, then confirm the column was added, the version bumped
# to >=6, and the legacy row survived with a NULL device_id.
PREV6="/tmp/facelock-prev6.db"
rm -f "$PREV6" "$PREV6-wal" "$PREV6-shm"
sqlite3 "$PREV6" "
CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
CREATE TABLE face_models (id INTEGER PRIMARY KEY AUTOINCREMENT, user TEXT NOT NULL, label TEXT NOT NULL, created_at INTEGER NOT NULL, embedder_model TEXT NOT NULL DEFAULT '', UNIQUE(user,label));
CREATE TABLE face_embeddings (id INTEGER PRIMARY KEY AUTOINCREMENT, model_id INTEGER NOT NULL REFERENCES face_models(id) ON DELETE CASCADE, embedding BLOB NOT NULL, sealed INTEGER NOT NULL DEFAULT 0);
CREATE TABLE rate_limit (user TEXT NOT NULL, attempt_time INTEGER NOT NULL);
INSERT INTO schema_version (version) VALUES (5);
INSERT INTO face_models (user,label,created_at,embedder_model) VALUES ('legacyuser','legacy-face',1700000000,'w600k_r50.onnx');
" || true
cp "$CONFIG" /tmp/facelock-prev6.toml
set_db_path /tmp/facelock-prev6.toml "$PREV6"
# Opening the store (via any command) runs migrations; auth on a user with no
# embeddings exits non-zero after migrating — we only care about the migration.
timeout --foreground 20s facelock auth --user legacyuser --config /tmp/facelock-prev6.toml >/dev/null 2>&1 || true
PREV6_VER="$(sqlite3 "$PREV6" 'SELECT MAX(version) FROM schema_version' 2>/dev/null || echo 0)"
PREV6_COL="$(sqlite3 "$PREV6" "SELECT COUNT(*) FROM pragma_table_info('face_models') WHERE name='device_id'" 2>/dev/null || echo 0)"
PREV6_ROW="$(sqlite3 "$PREV6" "SELECT label FROM face_models WHERE user='legacyuser'" 2>/dev/null || echo '')"
PREV6_DID="$(sqlite3 "$PREV6" "SELECT COALESCE(device_id,'NULL') FROM face_models WHERE user='legacyuser'" 2>/dev/null || echo '?')"
run_test "pre-V6 DB migrates cleanly, preserves row, device_id NULL (v=$PREV6_VER col=$PREV6_COL row=$PREV6_ROW did=$PREV6_DID)" \
    "[ \"$PREV6_VER\" -ge 6 ] && [ \"$PREV6_COL\" = 1 ] && [ \"$PREV6_ROW\" = legacy-face ] && [ \"$PREV6_DID\" = NULL ]" 0
rm -f "$PREV6" "$PREV6-wal" "$PREV6-shm" /tmp/facelock-prev6.toml

echo ""
echo "=== Results: $PASS passed, $FAIL failed, $SKIPPED block(s) skipped ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
