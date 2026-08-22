#!/bin/bash
# State-layout conformance tests (camera-free).
#
# Asserts the exact on-disk contract from docs/contracts.md (ADR 010):
#
#   /var/lib/facelock/       0711 root:root    traverse-only, not listable
#     facelock.db            0600 root:root
#     facelock.db-wal/-shm   0600 root:root
#     models/                0755 root:root    public, SHA256-verified
#     enrolled/              0711 root:root
#       <user>               0600 <user>:<user>
#     pam-backups/           0700 root:root    rollback state
#   /var/log/facelock/       0700 root:root
#     snapshots/             0700 root:root
#
# The image is built with `just install-files`, so this is the one place the
# packaging wiring (install recipe + built-in defaults) is exercised end to
# end. It also asserts the semantics the modes exist for: any local user —
# there is no facelock group any more (ADR 010) — can traverse to a marker it
# knows by name but cannot list the state directory or read the database.
set -uo pipefail

PASS=0
FAIL=0

run_test() {
    local name="$1"
    local cmd="$2"
    local expected_result="${3:-0}"

    echo -n "TEST: $name ... "
    # Run command without pipefail so piped greps work correctly
    if bash -c "$cmd" > /tmp/test-output 2>&1; then
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

# stat-based assertion: mode owner group, e.g. assert_path /var/lib/facelock 711 root root
assert_path() {
    local path="$1" mode="$2" owner="$3" group="$4"
    run_test "$path is $mode $owner:$group" \
        "[ \"\$(stat -c '%a %U %G' $path)\" = '$mode $owner $group' ]" \
        0
}

echo "=== Facelock state-layout tests ==="
echo ""

# ---------------------------------------------------------------------------
# The static layout, as `just install-files` shipped it
# ---------------------------------------------------------------------------

assert_path /var/lib/facelock          711 root root
assert_path /var/lib/facelock/models   755 root root
assert_path /var/lib/facelock/enrolled 711 root root
assert_path /var/lib/facelock/pam-backups 700 root root
assert_path /var/log/facelock          700 root root
assert_path /var/log/facelock/snapshots 700 root root

run_test "no facelock group is shipped (ADR 010 retired it)" \
    "! getent group facelock" \
    0

# ---------------------------------------------------------------------------
# The binary converges an older install back onto the layout
# ---------------------------------------------------------------------------

# Simulate the pre-ADR-010 layout (0710 root:facelock, group-readable
# database) plus a wide-open state dir. The group is no longer shipped, so the
# simulation has to create the one an old install would have left behind; it
# is deleted again at the end of this block.
getent group facelock >/dev/null || groupadd -r facelock
install -m 640 -o root -g facelock /dev/null /var/lib/facelock/facelock.db
chown root:facelock /var/lib/facelock /var/lib/facelock/enrolled
chmod 710 /var/lib/facelock/enrolled
chmod 755 /var/lib/facelock

# Any root invocation that touches the store applies the layout first; `list`
# is the cheapest one that needs no camera. Its own exit code is irrelevant
# here (the seeded database is empty).
facelock list --user testuser > /dev/null 2>&1 || true

assert_path /var/lib/facelock             711 root root
assert_path /var/lib/facelock/enrolled    711 root root
assert_path /var/lib/facelock/facelock.db 600 root root

# Back to the shipped state for everything below: no facelock group.
groupdel facelock 2>/dev/null || true

# ---------------------------------------------------------------------------
# Nothing under the state directory is readable or listable by "other"
# ---------------------------------------------------------------------------

# Files carry no "other" bits; directories carry at most traverse (o+x).
# models/ is the single subtree allowed to carry "other" bits of its own
# (public data).
run_test "no file under the state dir is other-accessible (models/ excepted)" \
    "[ -z \"\$(find /var/lib/facelock -mindepth 1 -path /var/lib/facelock/models -prune -o -type f -perm /o+rwx -print)\" ]" \
    0

run_test "no directory under the state dir is other-readable or -writable (models/ excepted)" \
    "[ -z \"\$(find /var/lib/facelock -mindepth 1 -path /var/lib/facelock/models -prune -o -type d -perm /o+rw -print)\" ]" \
    0

run_test "the state dir grants 'other' traversal only" \
    "[ \$(( 0\$(stat -c '%a' /var/lib/facelock) & 07 )) -eq 1 ]" \
    0

# `[ -eq ]` reads its operands as decimal, so the expected value goes through
# $(( )) too — a bare `010` on the right would mean ten, not 0o10.
run_test "the state dir grants group traversal only (no listing)" \
    "[ \$(( 0\$(stat -c '%a' /var/lib/facelock) & 070 )) -eq \$(( 010 )) ]" \
    0

# ---------------------------------------------------------------------------
# Semantics for a plain local user: traverse by name, list nothing, read no
# secret. `outsider` is an ordinary account with no privileges of any kind —
# ADR 010 left no group for it to be inside or outside of.
# ---------------------------------------------------------------------------

useradd -m outsider

# A marker for outsider, as enrollment would write it.
install -m 600 -o outsider -g outsider /dev/null /var/lib/facelock/enrolled/outsider
echo '{"models":2,"updated":"2026-08-13T00:00:00Z"}' > /var/lib/facelock/enrolled/outsider
# And one for testuser, which outsider must not be able to read.
install -m 600 -o testuser -g testuser /dev/null /var/lib/facelock/enrolled/testuser
echo '{"models":1,"updated":"2026-08-13T00:00:00Z"}' > /var/lib/facelock/enrolled/testuser

run_test "a plain local user reads own marker through 0711 dirs" \
    "runuser -u outsider -- cat /var/lib/facelock/enrolled/outsider" \
    0

run_test "a plain local user cannot read another user's marker" \
    "runuser -u outsider -- cat /var/lib/facelock/enrolled/testuser" \
    1

run_test "a plain local user cannot list the state dir" \
    "runuser -u outsider -- ls /var/lib/facelock" \
    2

run_test "a plain local user cannot list enrolled/" \
    "runuser -u outsider -- ls /var/lib/facelock/enrolled" \
    2

run_test "a plain local user cannot list PAM backups" \
    "runuser -u outsider -- ls /var/lib/facelock/pam-backups" \
    2

run_test "a plain local user cannot read the database" \
    "runuser -u outsider -- cat /var/lib/facelock/facelock.db" \
    1

run_test "a plain local user can read a model file by name" \
    "touch /var/lib/facelock/models/probe.onnx && chmod 644 /var/lib/facelock/models/probe.onnx && runuser -u outsider -- cat /var/lib/facelock/models/probe.onnx && rm /var/lib/facelock/models/probe.onnx" \
    0

run_test "a plain local user cannot read the audit log directory" \
    "runuser -u outsider -- ls /var/log/facelock" \
    2

# ---------------------------------------------------------------------------
# is-enrolled answers from the marker, for any local user
# ---------------------------------------------------------------------------

run_test "is-enrolled exits 0 for an enrolled plain local user" \
    "runuser -u outsider -- facelock is-enrolled" \
    0

run_test "is-enrolled --json reports the model count for a plain local user" \
    "runuser -u outsider -- facelock is-enrolled --json | grep -q '\"models\":2'" \
    0

useradd -m nobody-enrolled
run_test "is-enrolled exits 1 for a user with no marker" \
    "runuser -u nobody-enrolled -- facelock is-enrolled" \
    1

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
