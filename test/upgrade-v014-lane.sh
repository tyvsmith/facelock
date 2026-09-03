#!/usr/bin/env bash
# Released-predecessor upgrade and rollback proof (#231, Track K).
#
# Runs inside a booted container that already holds two artifacts:
#
#   /artifacts/facelock-predecessor.<ext>  the real published v0.1.4 asset,
#                                          pinned by asset id + SHA256 in
#                                          dist/release-matrix.json
#   /artifacts/facelock-candidate.<ext>    the locally built candidate
#
# Predecessor state is built with the **released** binary, not with this
# checkout's: the question is whether what v0.1.4 actually wrote survives, and
# a fixture written by the candidate would only prove the candidate agrees with
# itself. Every encrypted shape is encrypted by the 0.1.4 binary under a key
# 0.1.4 generated (or sealed, against a software TPM).
#
# What each shape proves after a native package-manager upgrade:
#   * the V5 database migrated to V6 and legacy rows carry device_id = NULL
#   * a **known embedding still decrypts** — the plaintext bytes are compared,
#     not the file hash, because a file hash cannot tell a preserved key from a
#     preserved ciphertext nobody can open any more
#   * no key artifact was replaced, and none appeared that did not exist before
#   * modes converged to ADR 010 without touching content
#   * the pre-existing PAM path is byte-identical and still authenticates, with
#     a real correct password succeeding and a real wrong password failing
#
# Then the candidate is downgraded back to v0.1.4 — after the candidate daemon
# has opened and migrated the database — and the released binary has to still
# read its own encrypted rows. V6 has no down-migration, so this is the only
# thing that says whether shipping the alpha strands a rollback.
set -euo pipefail

FAMILY="${1:-}"
case "$FAMILY" in
    deb) EXT=deb ;;
    rpm) EXT=rpm ;;
    *)
        echo "usage: upgrade-v014-lane.sh <deb|rpm>" >&2
        exit 2
        ;;
esac

PREDECESSOR="/artifacts/facelock-predecessor.$EXT"
CANDIDATE="/artifacts/facelock-candidate.$EXT"
PREDECESSOR_SHA256_FILE=/artifacts/predecessor.sha256
PREDECESSOR_VERSION_FILE=/artifacts/predecessor.version
CANDIDATE_VERSION_FILE=/artifacts/candidate.version

STATE_ROOT=/run/facelock-upgrade-v014
LOG=/tmp/facelock-upgrade-v014.log
PAM_SERVICE=facelock-upgrade-v014
PAM_PATH="/etc/pam.d/$PAM_SERVICE"
# The stack a distribution actually authenticates through, as opposed to the
# lane's own service. An upgrade must not edit it in either direction.
case "${1:-}" in
    deb) GLOBAL_PAM=/etc/pam.d/common-auth ;;
    *) GLOBAL_PAM=/etc/pam.d/system-auth ;;
esac
PASSWORD_USER=testuser
CORRECT_PASSWORD="test"
WRONG_PASSWORD=definitely-not-the-password
TCTI="swtpm:host=127.0.0.1,port=2321"

# The embedding every shape enrolls, as `struct.pack("<512f", (i+1)/512)`.
# Same fixture the Debian lifecycle gate uses, so one pinned digest describes
# the known plaintext in both places.
KNOWN_EMBEDDING_SHA256=82a0081de4c338fc91c362ed4d2ab615bca1dd45152aaa713322b5482078ddee

# Every state shape this lane builds with the released binary. A shape added
# here without a `seed_shape_*` function fails immediately rather than being
# skipped, and test/upgrade-v014-contract.sh holds this list against the
# functions that implement it.
SHAPES=(plaintext keyfile mixed tpm-pcr-unbound tpm-pcr-bound)

# Every fault the migration has to survive without resetting or recreating the
# database. Same rule: named here, implemented as `fault_*`, checked by the
# contract gate.
FAULTS=(disk-full interrupted corrupt-database concurrent-key-creation)

case_name=init

log() {
    printf '\n=== %s ===\n' "$*"
}

fail() {
    echo "FAIL [$FAMILY $case_name]: $*" >&2
    if [ -s "$LOG" ]; then
        echo "--- last 40 lines of package manager output ---" >&2
        tail -40 "$LOG" >&2
    fi
    exit 1
}

pass() {
    printf 'TEST: %s upgrade %s ... PASS\n' "$FAMILY" "$1"
}

assert_eq() {
    local expected="$1" actual="$2" label="$3"
    [ "$actual" = "$expected" ] ||
        fail "$label: expected '$expected', got '$actual'"
}

# --- package manager, per family ------------------------------------------

case "$FAMILY" in
    deb)
        # `apt-get update` before every transaction, the same as
        # test/deb-package-lifecycle.sh:162. The runtime image ends with
        # `rm -rf /var/lib/apt/lists/*`, so without it APT knows about no
        # archive at all and the candidate's own `Depends: dbus` is
        # "not installable" rather than merely absent.
        #
        # --force-confdef/--force-confold take dpkg's own documented default
        # action for a modified conffile ("keep your current version") without
        # a prompt, which is what unattended-upgrades does on a real machine.
        # It is needed here and not in the synthesized-predecessor lane because
        # that lane rebuilds the candidate's own payload as the older package,
        # so its packaged config.toml is byte-identical across the upgrade and
        # dpkg never asks. A real v0.1.4 ships a different config.toml, so the
        # modified file this lane plants does provoke the prompt -- and an
        # unanswered prompt is `end of file on stdin at conffile prompt`, not a
        # preserved config.
        apt_transaction() {
            DEBIAN_FRONTEND=noninteractive apt-get update >>"$LOG" 2>&1
            DEBIAN_FRONTEND=noninteractive apt-get \
                -o Dpkg::Options::=--force-confdef \
                -o Dpkg::Options::=--force-confold \
                "$@" >>"$LOG" 2>&1
        }
        pkg_install() { apt_transaction install -y --no-install-recommends "$1"; }
        pkg_upgrade() { apt_transaction install -y --no-install-recommends "$1"; }
        pkg_downgrade() {
            apt_transaction install -y --no-install-recommends --allow-downgrades "$1"
        }
        pkg_remove() { apt_transaction remove -y facelock; }
        pkg_installed_version() { dpkg-query -W -f='${Version}' facelock 2>/dev/null; }
        pkg_is_installed() { [ "$(dpkg-query -W -f='${Status}' facelock 2>/dev/null)" = "install ok installed" ]; }
        pkg_version_gt() { dpkg --compare-versions "$1" gt "$2"; }
        pkg_file_version() { dpkg-deb --field "$1" Version; }
        ;;
    rpm)
        pkg_install() { dnf install -y "$1" >>"$LOG" 2>&1; }
        pkg_upgrade() { dnf upgrade -y "$1" >>"$LOG" 2>&1; }
        pkg_downgrade() { dnf downgrade -y "$1" >>"$LOG" 2>&1; }
        pkg_remove() { dnf remove -y facelock >>"$LOG" 2>&1; }
        pkg_installed_version() { rpm -q --qf '%{VERSION}-%{RELEASE}' facelock 2>/dev/null; }
        pkg_is_installed() { rpm -q facelock >/dev/null 2>&1; }
        pkg_version_gt() {
            # rpmdev-vercmp exits 11 when the first argument is newer.
            local status=0
            rpmdev-vercmp "$1" "$2" >/dev/null 2>&1 || status=$?
            [ "$status" -eq 11 ]
        }
        pkg_file_version() { rpm -qp --qf '%{VERSION}-%{RELEASE}' "$1" 2>/dev/null; }
        ;;
esac

# --- pinned-artifact preconditions ----------------------------------------

assert_pinned_artifacts() {
    case_name=pinned-artifacts
    local expected_digest actual_digest expected_version

    for artifact in "$PREDECESSOR" "$CANDIDATE"; do
        [ -f "$artifact" ] && [ ! -L "$artifact" ] ||
            fail "lane artifact missing or not a regular file: $artifact"
    done

    # Re-check the digest at run time, not only at image build time. The image
    # layer that fetched it and the container that installs it are different
    # moments, and only the second one is the thing under test.
    expected_digest="$(cat "$PREDECESSOR_SHA256_FILE")"
    actual_digest="$(sha256sum "$PREDECESSOR" | cut -d' ' -f1)"
    assert_eq "$expected_digest" "$actual_digest" "pinned predecessor SHA256"

    expected_version="$(cat "$PREDECESSOR_VERSION_FILE")"
    assert_eq "$expected_version" "$(pkg_file_version "$PREDECESSOR")" \
        "pinned predecessor package version"
    assert_eq "$(cat "$CANDIDATE_VERSION_FILE")" "$(pkg_file_version "$CANDIDATE")" \
        "candidate package version"

    # An upgrade lane that is secretly a downgrade proves nothing. The native
    # comparator decides, not string order.
    pkg_version_gt "$(pkg_file_version "$CANDIDATE")" "$(pkg_file_version "$PREDECESSOR")" ||
        fail "candidate $(pkg_file_version "$CANDIDATE") does not sort above predecessor $(pkg_file_version "$PREDECESSOR")"
    pass "pinned predecessor and a strictly newer candidate"
}

# --- software TPM ----------------------------------------------------------

start_swtpm() {
    case_name=swtpm
    export TPM2TOOLS_TCTI="$TCTI"
    mkdir -p /tmp/swtpm
    swtpm socket --tpm2 \
        --server type=tcp,port=2321 \
        --ctrl type=tcp,port=2322 \
        --tpmstate dir=/tmp/swtpm \
        --flags startup-clear \
        --daemon
    local started=0
    for _ in $(seq 1 30); do
        if tpm2_pcrread sha256:16 >/dev/null 2>&1; then
            started=1
            break
        fi
        tpm2_startup -c >/dev/null 2>&1 || true
        sleep 0.3
    done
    [ "$started" = 1 ] || fail "swtpm not reachable via $TCTI"
}

# The TPM's own state plus the PCR bank the bound shape seals against. swtpm
# rewrites its state file on its own schedule, so this is only ever compared
# across a single package transaction: recorded immediately before, checked
# immediately after. That window is exactly what Track K task 12 forbids a
# maintainer script from touching.
swtpm_state_digest() {
    {
        find /tmp/swtpm -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum
        tpm2_pcrread sha256:16 2>/dev/null || echo "pcr-unreadable"
    } | sha256sum | cut -d' ' -f1
}

record_swtpm_state() {
    swtpm_state_digest >"$STATE_ROOT/swtpm.before"
}

assert_swtpm_state_untouched() {
    assert_eq "$(cat "$STATE_ROOT/swtpm.before")" "$(swtpm_state_digest)" \
        "software TPM state and PCR 16 across the package transaction"
}

# --- state fixtures, written by the RELEASED binary ------------------------

write_config() {
    local shape="$1" method="${2:-}"
    [ -n "$method" ] || method="$(config_method "$shape")"
    install -d -m0755 /etc/facelock
    cat >/etc/facelock/config.toml <<EOF
[storage]
db_path = "/var/lib/facelock/facelock.db"

[daemon]
model_dir = "/var/lib/facelock/models"

[device]
path = "/dev/video0"

[encryption]
method = "$method"
key_path = "/etc/facelock/encryption.key"
sealed_key_path = "/etc/facelock/encryption.key.sealed"

[tpm]
pcr_binding = $(config_pcr_binding "$shape")
pcr_indices = [16]
tcti = "$TCTI"

[security]
allow_plaintext = true
EOF
    chmod 0644 /etc/facelock/config.toml
}

config_method() {
    case "$1" in
        plaintext) echo none ;;
        keyfile | mixed) echo keyfile ;;
        tpm-pcr-unbound | tpm-pcr-bound) echo keyfile ;;
        *) fail "no encryption method for shape: $1" ;;
    esac
}

config_pcr_binding() {
    case "$1" in
        tpm-pcr-bound) echo true ;;
        *) echo false ;;
    esac
}

# Three embeddings per model, matching what a real enrollment writes since the
# store floor landed. The first is the known fixture whose plaintext this lane
# compares after the upgrade; the other two exist so the row count is what a
# 0.1.4 enrollment would actually have left behind.
insert_plaintext_rows() {
    local label="$1"
    python3 - "$label" <<'PY'
import sqlite3
import struct
import sys

label = sys.argv[1]
connection = sqlite3.connect("/var/lib/facelock/facelock.db")
connection.execute("PRAGMA journal_mode=WAL")
connection.execute(
    "INSERT INTO face_models (user, label, created_at, embedder_model) "
    "VALUES (?, ?, ?, ?)",
    ("testuser", label, 1700000000, "arcface"),
)
model_id = connection.execute(
    "SELECT id FROM face_models WHERE user = ? AND label = ?",
    ("testuser", label),
).fetchone()[0]
known = struct.pack("<512f", *((index + 1) / 512 for index in range(512)))
for offset in range(3):
    blob = known if offset == 0 else struct.pack(
        "<512f", *((index + 1 + offset) / 512 for index in range(512))
    )
    connection.execute(
        "INSERT INTO face_embeddings (model_id, embedding, sealed) VALUES (?, ?, 0)",
        (model_id, blob),
    )
connection.commit()
connection.close()
PY
}

released_facelock() {
    facelock "$@"
}

# The daemon loads models at startup, so the shape phases need the real ones.
# The lane runner stages them read-only at /facelock-test-models; a lane without
# them cannot open the database with the candidate daemon and says so rather
# than quietly proving less.
stage_models() {
    local model staged=0
    for model in /facelock-test-models/*.onnx; do
        [ -f "$model" ] || continue
        install -m0644 "$model" /var/lib/facelock/models/
        staged=$((staged + 1))
    done
    [ "$staged" -gt 0 ] ||
        fail "no reviewed ONNX models staged at /facelock-test-models"
}

seed_common_state() {
    install -d -m0711 /var/lib/facelock
    install -d -m0755 /var/lib/facelock/models
    install -d -m0711 /var/lib/facelock/enrolled
    install -d -m0700 /var/lib/facelock/pam-backups /var/log/facelock
    install -d -m0700 /var/log/facelock/snapshots

    # The reviewed models the candidate daemon has to load, plus a payload of
    # this lane's own: an upgrade has no business touching either, and both
    # historically got moved or re-owned. The payload is deliberately not an
    # .onnx, so a stray file never reaches the engine's model discovery.
    stage_models
    printf '%s\n' upgrade-lane-payload >/var/lib/facelock/models/upgrade-lane.payload
    chmod 0644 /var/lib/facelock/models/upgrade-lane.payload
    # Deliberately stale: v0.1.4 shipped no enrollment markers, so an upgraded
    # system's marker either does not exist or describes a database it has since
    # diverged from. The candidate daemon reconciles it at startup (#137), and
    # `assert_enrollment_marker_reconciled` is what proves it did.
    printf '%s\n' '{"models":0,"updated":"2020-01-01T00:00:00Z"}' \
        >/var/lib/facelock/enrolled/testuser
    chown testuser:testuser /var/lib/facelock/enrolled/testuser
    chmod 0600 /var/lib/facelock/enrolled/testuser
    printf '%s\n' complete >/var/lib/facelock/setup.complete
    chmod 0600 /var/lib/facelock/setup.complete
    printf '%s\n' '{"event":"auth","user":"testuser"}' >/var/log/facelock/audit.jsonl
    chmod 0600 /var/log/facelock/audit.jsonl
    printf '%s\n' snapshot >/var/log/facelock/snapshots/upgrade-lane.jpg
    chmod 0600 /var/log/facelock/snapshots/upgrade-lane.jpg

    # The PAM path an 0.1.4 administrator would have wired by hand: v0.1.4 has
    # no `facelock pam` subcommand, so this is the shape the upgrade actually
    # finds on a real system.
    install -Dm0644 /dev/stdin "$PAM_PATH" <<EOF
#%PAM-1.0
auth      sufficient pam_facelock.so
auth      required   pam_unix.so
account   required   pam_unix.so
EOF
}

# The released binary creates and migrates its own database. Nothing here uses
# a candidate command: `facelock` on PATH is v0.1.4 for the whole seed phase.
#
# v0.1.4 has exactly one subcommand that opens the store read-write and runs
# migrations without a camera: `facelock encrypt`. `list` is a D-Bus call to the
# daemon and `tpm status` opens read-only, so neither creates a schema. Every
# shape therefore bootstraps its database through the keyfile path and then
# becomes whatever shape it is.
seed_released_database() {
    released_facelock encrypt --generate-key >>"$LOG" 2>&1 ||
        fail "the released binary could not generate its encryption key"
    released_facelock encrypt >>"$LOG" 2>&1 ||
        fail "the released binary could not create and migrate its database"
    [ -f /var/lib/facelock/facelock.db ] ||
        fail "the released binary did not create its database"
    assert_eq 5 "$(schema_version)" "schema version written by the released binary"
}

seed_shape_plaintext() {
    # A legacy plaintext install: no key artifact at all, which is the one state
    # the candidate is allowed to create a default key for.
    rm -f /etc/facelock/encryption.key
    write_config plaintext
    insert_plaintext_rows plaintext
    assert_eq "0|3" "$(sealed_counts)" "plaintext shape row encryption"
}

seed_shape_keyfile() {
    insert_plaintext_rows keyfile
    released_facelock encrypt >>"$LOG" 2>&1 ||
        fail "the released binary could not encrypt its rows"
    assert_eq "3|0" "$(sealed_counts)" "keyfile shape row encryption"
}

seed_shape_mixed() {
    insert_plaintext_rows encrypted-half
    released_facelock encrypt >>"$LOG" 2>&1 ||
        fail "the released binary could not encrypt its rows"
    # A second enrollment that never went through `encrypt`: the mixed database
    # a user gets by enrolling again after turning encryption on.
    insert_plaintext_rows plaintext-half
    assert_eq "3|3" "$(sealed_counts)" "mixed shape row encryption"
}

seed_shape_tpm_common() {
    released_facelock tpm seal-key >>"$LOG" 2>&1 ||
        fail "the released binary could not seal its key against swtpm"
    [ -s /etc/facelock/encryption.key.sealed ] ||
        fail "sealing produced no sealed key artifact"
    # `tpm seal-key` flips the configured method; make sure it did, because a
    # shape that stayed on the plaintext keyfile would prove the wrong thing.
    grep -Eq '^[[:space:]]*method[[:space:]]*=[[:space:]]*"tpm"' /etc/facelock/config.toml ||
        fail "sealing did not move the configured encryption method to tpm"
    insert_plaintext_rows tpm
    released_facelock encrypt >>"$LOG" 2>&1 ||
        fail "the released binary could not encrypt rows under the sealed key"
    assert_eq "3|0" "$(sealed_counts)" "TPM shape row encryption"
}

seed_shape_tpm_pcr_unbound() { seed_shape_tpm_common; }
seed_shape_tpm_pcr_bound() { seed_shape_tpm_common; }

seed_shape() {
    local shape="$1"
    # Bootstrap on the keyfile path whatever the shape, because that is the only
    # released code path that will create the schema. `write_config` keeps the
    # shape's own PCR selection from the first byte, so a bound shape seals
    # under the PCRs it is meant to.
    write_config "$shape" keyfile
    seed_common_state
    seed_released_database
    case "$shape" in
        plaintext) seed_shape_plaintext ;;
        keyfile) seed_shape_keyfile ;;
        mixed) seed_shape_mixed ;;
        tpm-pcr-unbound) seed_shape_tpm_pcr_unbound ;;
        tpm-pcr-bound) seed_shape_tpm_pcr_bound ;;
        *) fail "no seed function for declared shape: $shape" ;;
    esac
    # An administrator edit, added last so a released command that rewrites the
    # config cannot erase the thing the upgrade has to preserve.
    printf '\n# facelock upgrade-lane administrator marker (%s)\n' "$shape" \
        >>/etc/facelock/config.toml
}

# --- database inspection ---------------------------------------------------

schema_version() {
    sqlite_query "SELECT COALESCE(MAX(version), 0) FROM schema_version"
}

sealed_counts() {
    sqlite_query \
        "SELECT (SELECT COUNT(*) FROM face_embeddings WHERE sealed != 0) || '|' || \
                (SELECT COUNT(*) FROM face_embeddings WHERE sealed = 0)"
}

sqlite_query() {
    python3 - "$1" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect("file:/var/lib/facelock/facelock.db?mode=ro", uri=True)
print(connection.execute(sys.argv[1]).fetchone()[0])
connection.close()
PY
}

# --- snapshots -------------------------------------------------------------

snapshot_file() {
    local path="$1"
    if [ ! -e "$path" ] && [ ! -L "$path" ]; then
        printf 'absent|%s\n' "$path"
        return
    fi
    [ -f "$path" ] && [ ! -L "$path" ] || fail "not a regular file: $path"
    LC_ALL=C stat -c 'file|%n|%a|%u|%g|%s' -- "$path"
    sha256sum -- "$path"
}

# Everything an upgrade must leave byte-identical. Deliberately excludes the
# schema version and the device_id column: those are what the migration is for,
# and asserting them separately keeps "changed as designed" from hiding inside
# "changed".
snapshot_path_any() {
    local path="$1"
    if [ ! -e "$path" ] && [ ! -L "$path" ]; then
        printf 'absent|%s\n' "$path"
        return
    fi
    LC_ALL=C stat -c 'entry|%n|%F|%a|%u|%g' -- "$path"
    if [ -L "$path" ]; then
        printf 'link|%s|' "$path"
        readlink -- "$path"
    fi
    if [ -f "$path" ]; then
        sha256sum -- "$path" | sed 's|^|content|'
    fi
}

snapshot_invariant_state() {
    local path
    snapshot_path_any "$GLOBAL_PAM"
    for path in \
        /etc/facelock/config.toml \
        /etc/facelock/encryption.key \
        /etc/facelock/encryption.key.sealed \
        /var/lib/facelock/models/upgrade-lane.payload \
        /var/lib/facelock/setup.complete \
        /var/log/facelock/audit.jsonl \
        /var/log/facelock/snapshots/upgrade-lane.jpg \
        "$PAM_PATH"; do
        snapshot_file "$path"
    done
    find /var/lib/facelock/models -maxdepth 1 -type f -printf 'model|%f|%s|%m\n' |
        LC_ALL=C sort
    snapshot_rows
}

# Row identity by content, not by file digest: the database file legitimately
# changes across a migration, so comparing it byte for byte would either fail
# always or be relaxed into proving nothing.
snapshot_rows() {
    python3 - <<'PY'
import hashlib
import sqlite3

connection = sqlite3.connect("file:/var/lib/facelock/facelock.db?mode=ro", uri=True)
rows = connection.execute(
    "SELECT fm.user, fm.label, fm.created_at, fm.embedder_model, fe.embedding, fe.sealed "
    "FROM face_models AS fm JOIN face_embeddings AS fe ON fe.model_id = fm.id "
    "ORDER BY fm.label, fe.id"
).fetchall()
connection.close()
if not rows:
    raise SystemExit("snapshot found no enrollment rows")
for user, label, created_at, embedder, blob, sealed in rows:
    print(
        f"row|{user}|{label}|{created_at}|{embedder}|{sealed}|"
        f"{len(blob)}|{hashlib.sha256(blob).hexdigest()}"
    )
PY
}

# --- post-upgrade proofs ---------------------------------------------------

assert_schema_v6_with_null_device_id() {
    assert_eq 6 "$(schema_version)" "schema version after the candidate opened the database"
    local columns
    columns="$(python3 - <<'PY'
import sqlite3

connection = sqlite3.connect("file:/var/lib/facelock/facelock.db?mode=ro", uri=True)
print(
    ",".join(row[1] for row in connection.execute("PRAGMA table_info(face_models)"))
)
connection.close()
PY
)"
    case ",$columns," in
        *,device_id,*) ;;
        *) fail "V6 migration did not add face_models.device_id (columns: $columns)" ;;
    esac
    # Legacy rows must stay uncoupled. A migration that invented a device id
    # would bind every existing template to whatever camera happened to be
    # plugged in during the upgrade.
    assert_eq 0 "$(sqlite_query 'SELECT COUNT(*) FROM face_models WHERE device_id IS NOT NULL')" \
        "legacy rows left with a non-NULL device_id"
}

# The proof a file hash cannot give: take the blob the released binary wrote,
# decrypt it with the candidate and the preserved key, and compare the
# plaintext to the digest this lane pinned before any of it was encrypted.
assert_known_embedding_decrypts() {
    local shape="$1" probe=/tmp/facelock-decrypt-probe
    rm -rf "$probe"
    install -d -m0700 "$probe"
    cp /var/lib/facelock/facelock.db "$probe/probe.db"
    sed "s|^db_path = .*|db_path = \"$probe/probe.db\"|" \
        /etc/facelock/config.toml >"$probe/config.toml"

    if [ "$shape" != plaintext ]; then
        facelock --config "$probe/config.toml" tpm decrypt >>"$LOG" 2>&1 ||
            fail "the candidate could not decrypt rows the released binary encrypted"
    fi

    local digest
    digest="$(python3 - "$probe/probe.db" <<'PY'
import hashlib
import sqlite3
import sys

connection = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
blobs = [
    row[0]
    for row in connection.execute(
        "SELECT fe.embedding FROM face_embeddings AS fe "
        "JOIN face_models AS fm ON fm.id = fe.model_id ORDER BY fm.label, fe.id"
    )
]
connection.close()
print(next((hashlib.sha256(b).hexdigest() for b in blobs if len(b) == 2048), "none"))
PY
)"
    assert_eq "$KNOWN_EMBEDDING_SHA256" "$digest" \
        "known embedding plaintext recovered after the upgrade"
    rm -rf "$probe"
}

# The enrollment marker is the one piece of state an upgrade is *supposed* to
# rewrite. `facelock is-enrolled` reads it, v0.1.4 never wrote one, and a marker
# left describing a database it has diverged from answers "not enrolled" for a
# user whose face authentication works (#137). So the contract is not byte
# identity, which would happily preserve a marker that lies. It is that the
# marker still exists, is still the user's own private file, and now agrees with
# the database.
assert_enrollment_marker_reconciled() {
    local marker=/var/lib/facelock/enrolled/testuser expected
    expected="$(sqlite_query "SELECT COUNT(*) FROM face_models WHERE user = 'testuser'")"
    python3 - "$marker" "$(id -u testuser)" "$(id -g testuser)" "$expected" <<'MARKER_PY'
import datetime
import json
import os
import stat
import sys

path, expected_uid, expected_gid, expected_models = (
    sys.argv[1],
    int(sys.argv[2]),
    int(sys.argv[3]),
    int(sys.argv[4]),
)

descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
try:
    info = os.fstat(descriptor)
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise SystemExit(f"enrollment marker is not a single-link regular file: {path}")
    if stat.S_IMODE(info.st_mode) != 0o600:
        raise SystemExit(
            f"enrollment marker mode is {stat.S_IMODE(info.st_mode):o}, expected 600"
        )
    if (info.st_uid, info.st_gid) != (expected_uid, expected_gid):
        raise SystemExit(
            f"enrollment marker owner is {info.st_uid}:{info.st_gid}, "
            f"expected {expected_uid}:{expected_gid}"
        )
    with os.fdopen(descriptor, encoding="utf-8") as handle:
        descriptor = -1
        marker = json.load(handle)
finally:
    if descriptor >= 0:
        os.close(descriptor)

if set(marker) != {"models", "updated"}:
    raise SystemExit(f"enrollment marker has unexpected fields: {sorted(marker)!r}")
if type(marker["models"]) is not int or marker["models"] != expected_models:
    raise SystemExit(
        f"enrollment marker says {marker['models']!r} models, "
        f"the database holds {expected_models}"
    )
updated = marker["updated"]
try:
    parsed = datetime.datetime.fromisoformat(str(updated).replace("Z", "+00:00"))
except (TypeError, ValueError) as error:
    raise SystemExit(f"enrollment marker timestamp is invalid: {updated!r}") from error
if parsed.tzinfo is None:
    raise SystemExit(f"enrollment marker timestamp has no timezone: {updated!r}")
# The fixture is stamped 2020; anything at or before that means nothing
# reconciled it and the assertion above passed only by luck.
if parsed <= datetime.datetime(2020, 1, 2, tzinfo=datetime.timezone.utc):
    raise SystemExit(
        f"enrollment marker was never reconciled: still stamped {updated!r}"
    )
MARKER_PY
}

assert_key_artifacts_preserved() {
    local before="$1" path
    # Nothing appeared that was not there before. A replacement key written
    # beside the real one is the failure this catches.
    # -x, not a bare -F: "encryption.key" is a prefix of "encryption.key.sealed",
    # so an unanchored match finds the sealed path's absent line and reports a
    # key that was there all along as newly created.
    for path in /etc/facelock/encryption.key /etc/facelock/encryption.key.sealed; do
        if grep -Fxq "absent|$path" "$before" && [ -e "$path" ]; then
            fail "the upgrade created a key artifact that did not exist before: $path"
        fi
    done
}

# The candidate must refuse to write a replacement key when encrypted rows are
# present and the key artifact is gone — the upgrade-day failure that turns a
# recoverable backup restore into permanent loss.
assert_no_replacement_key_over_encrypted_state() {
    local shape="$1"
    case "$shape" in
        keyfile | mixed) ;;
        *) return 0 ;;
    esac
    local saved="$STATE_ROOT/key.saved" output=/tmp/facelock-replacement-key.log status=0
    cp /etc/facelock/encryption.key "$saved"
    rm -f /etc/facelock/encryption.key

    # The daemon must keep running: refusing to write a key must not turn an
    # authentication that already falls through to password into a lockout.
    # Exit 124 is the timeout firing on a daemon that is still up.
    RUST_LOG=warn timeout --foreground 30 facelock daemon >"$output" 2>&1 || status=$?
    if [ -e /etc/facelock/encryption.key ]; then
        install -m0600 "$saved" /etc/facelock/encryption.key
        fail "the candidate wrote a replacement key over encrypted rows"
    fi
    [ "$status" = 124 ] || {
        tail -20 "$output" >&2
        install -m0600 "$saved" /etc/facelock/encryption.key
        fail "the daemon stopped serving when its key went missing (exit $status)"
    }
    grep -qi 'refusing to write a replacement key' "$output" || {
        tail -20 "$output" >&2
        install -m0600 "$saved" /etc/facelock/encryption.key
        fail "the refusal did not name the missing key over encrypted rows"
    }

    install -m0600 "$saved" /etc/facelock/encryption.key
    rm -f "$saved"
}

# ADR 010 modes. The packaged scriptlets tighten these on an upgrade from a
# release that predates the layout; content must be untouched while they do.
assert_adr010_modes() {
    local expected path
    while read -r expected path; do
        [ -e "$path" ] || continue
        assert_eq "$expected" "$(stat -c '%a:%u:%g' "$path")" "ADR 010 metadata for $path"
    done <<'EOF'
711:0:0 /var/lib/facelock
755:0:0 /var/lib/facelock/models
711:0:0 /var/lib/facelock/enrolled
700:0:0 /var/lib/facelock/pam-backups
700:0:0 /var/log/facelock
700:0:0 /var/log/facelock/snapshots
600:0:0 /var/lib/facelock/facelock.db
600:0:0 /var/log/facelock/audit.jsonl
EOF
    # ADR 010 retired the group outright; an upgrade from a release that had
    # one must remove it rather than leave a group owning nothing.
    ! getent group facelock >/dev/null 2>&1 ||
        fail "the retired facelock group survived the upgrade"
}

# v0.1.4's pam-configs profile shipped `Default: yes`, so installing that
# release ran pam-auth-update --package and turned face authentication on in
# common-auth without anyone asking. The candidate ships `Default: no`.
#
# So the contract an upgrade has to meet is not "facelock is absent from the
# global stack" -- on a v0.1.4 system it is present, and removing it would
# silently take face authentication away from someone using it. It is that the
# upgrade leaves the stack exactly as it found it, in either direction. The
# byte comparison lives in the invariant snapshot; this records the verdict in
# a form a reader of the log can act on.
record_pam_enabled_state() {
    local shape="$1"
    pam_enabled_state >"$STATE_ROOT/$shape.pam-enabled"
}

pam_enabled_state() {
    if [ -f "$GLOBAL_PAM" ] && grep -q pam_facelock.so "$GLOBAL_PAM"; then
        echo enabled
    else
        echo disabled
    fi
}

assert_pam_path_intact() {
    local before="$1" shape="$2"
    grep -Fq "$(sha256sum "$PAM_PATH")" "$before" ||
        fail "the upgrade rewrote the administrator's PAM service: $PAM_PATH"
    assert_eq "$(cat "$STATE_ROOT/$shape.pam-enabled")" "$(pam_enabled_state)" \
        "whether Facelock is enabled in $GLOBAL_PAM across the upgrade"
    case "$FAMILY" in
        deb)
            [ -f /usr/share/pam-configs/facelock ] ||
                fail "the candidate did not register its pam-configs profile"
            # The packaged default flipped to `no` in the candidate. That governs
            # a fresh install only; it must never retire a profile an existing
            # system already has enabled.
            grep -Eq '^Default:[[:space:]]*no$' /usr/share/pam-configs/facelock ||
                fail "the candidate profile no longer defaults to off for fresh installs"
            ;;
        rpm)
            # The authselect profile was retired; an upgrade must not put one back.
            [ ! -e /usr/share/authselect/vendor/facelock ] ||
                fail "the upgrade reinstated the retired authselect profile"
            ;;
    esac
}

assert_real_password_behavior() {
    printf '%s\n' "$CORRECT_PASSWORD" |
        timeout --foreground 60 pamtester "$PAM_SERVICE" "$PASSWORD_USER" authenticate \
            >>"$LOG" 2>&1 ||
        fail "the correct password no longer authenticates through $PAM_SERVICE"
    if printf '%s\n' "$WRONG_PASSWORD" |
        timeout --foreground 60 pamtester "$PAM_SERVICE" "$PASSWORD_USER" authenticate \
            >>"$LOG" 2>&1; then
        fail "a wrong password authenticated through $PAM_SERVICE"
    fi
}

# --- the candidate daemon actually opens the database ----------------------
#
# The rollback question is not "does v0.1.4 read a V6 file" in the abstract, it
# is "does it read the file the alpha daemon has already opened, migrated and
# written to". So the daemon runs for real before the downgrade.
open_database_with_candidate_daemon() {
    local output=/tmp/facelock-candidate-daemon.log pid
    RUST_LOG=warn facelock daemon >"$output" 2>&1 &
    pid=$!
    for _ in $(seq 1 120); do
        if [ "$(schema_version)" = 6 ]; then break; fi
        sleep 0.5
    done
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    assert_eq 6 "$(schema_version)" "schema version after the candidate daemon ran"
}

# --- rollback --------------------------------------------------------------

assert_downgrade_usable() {
    local shape="$1" probe=/tmp/facelock-rollback-probe
    case_name="$shape-downgrade"

    record_swtpm_state
    pkg_downgrade "$PREDECESSOR" ||
        fail "the package manager refused to downgrade to the pinned predecessor"
    assert_eq "$(cat "$PREDECESSOR_VERSION_FILE")" "$(pkg_installed_version)" \
        "installed version after downgrade"

    # V6 has no down-migration. The predecessor must still open the database
    # the candidate migrated and count its rows. `tpm status` is the readback
    # rather than `list`, which in v0.1.4 is a D-Bus call to a daemon this
    # phase deliberately does not run.
    released_facelock tpm status >>"$LOG" 2>&1 ||
        fail "the released binary could not read the V6 database after rollback"
    assert_eq 6 "$(schema_version)" "schema version is not rolled back by a package downgrade"

    if [ "$shape" != plaintext ]; then
        rm -rf "$probe"
        install -d -m0700 "$probe"
        cp /var/lib/facelock/facelock.db "$probe/probe.db"
        sed "s|^db_path = .*|db_path = \"$probe/probe.db\"|" \
            /etc/facelock/config.toml >"$probe/config.toml"
        released_facelock --config "$probe/config.toml" decrypt >>"$LOG" 2>&1 ||
            fail "the released binary could not decrypt its own rows after rollback"
        rm -rf "$probe"
    fi

    assert_real_password_behavior
    assert_swtpm_state_untouched
    pass "$shape rollback to $(cat "$PREDECESSOR_VERSION_FILE") stays usable"
}

# --- one shape, end to end -------------------------------------------------

wipe_state() {
    rm -rf /etc/facelock /var/lib/facelock /var/log/facelock "$PAM_PATH"
    rm -f "$STATE_ROOT"/*.before "$STATE_ROOT"/*.after 2>/dev/null || true
}

run_shape() {
    local shape="$1"
    case_name="$shape"
    log "$FAMILY upgrade shape: $shape"

    if pkg_is_installed; then pkg_remove; fi
    wipe_state
    pkg_install "$PREDECESSOR" || fail "the pinned predecessor did not install"
    assert_eq "$(cat "$PREDECESSOR_VERSION_FILE")" "$(pkg_installed_version)" \
        "installed predecessor version"

    seed_shape "$shape"
    snapshot_invariant_state >"$STATE_ROOT/$shape.before"

    record_swtpm_state
    record_pam_enabled_state "$shape"
    pkg_upgrade "$CANDIDATE" || fail "the native upgrade to the candidate failed"
    assert_eq "$(cat "$CANDIDATE_VERSION_FILE")" "$(pkg_installed_version)" \
        "installed candidate version after upgrade"
    assert_swtpm_state_untouched

    open_database_with_candidate_daemon

    snapshot_invariant_state >"$STATE_ROOT/$shape.after"
    cmp -s "$STATE_ROOT/$shape.before" "$STATE_ROOT/$shape.after" || {
        diff -u "$STATE_ROOT/$shape.before" "$STATE_ROOT/$shape.after" >&2 || true
        fail "the upgrade changed state it had to preserve"
    }

    assert_schema_v6_with_null_device_id
    assert_known_embedding_decrypts "$shape"
    assert_enrollment_marker_reconciled
    assert_key_artifacts_preserved "$STATE_ROOT/$shape.before"
    assert_adr010_modes
    assert_pam_path_intact "$STATE_ROOT/$shape.before" "$shape"
    assert_real_password_behavior
    assert_no_replacement_key_over_encrypted_state "$shape"
    pass "$shape state survives $(cat "$PREDECESSOR_VERSION_FILE") to $(cat "$CANDIDATE_VERSION_FILE")"

    assert_downgrade_usable "$shape"
}

# --- fault cases -----------------------------------------------------------
#
# Every one of these asks the same question: when the migration cannot finish,
# does the database survive? A migration that resets or recreates a database it
# could not migrate destroys every enrollment on the machine, and the failure
# then looks exactly like a clean first run.
#
# The migrating command is `facelock daemon`, because that is what migrates in
# production: `FaceStore::create` runs inside its startup, ahead of the D-Bus
# name it never gets to claim here. Each case runs it against its own config,
# so nothing here can reach the system database.

FAULT_ROOT=/run/facelock-upgrade-v014/fault
# Filled to capacity for the disk-full case. /dev/shm is a real filesystem with
# a real, small size limit, so the ENOSPC is the kernel's rather than a
# simulation, and it needs no privilege this lane does not already have.
FAULT_FULL_ROOT=/dev/shm/facelock-fault-full

seed_fault_database() {
    local target="$1"
    rm -rf "$target"
    install -d -m0700 "$target"
    python3 - "$target/facelock.db" <<'FAULT_SEED_PY'
import sqlite3
import struct
import sys

connection = sqlite3.connect(sys.argv[1])
connection.executescript(
    """
    CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
    CREATE TABLE face_models (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        user TEXT NOT NULL,
        label TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        embedder_model TEXT NOT NULL DEFAULT '',
        UNIQUE(user, label)
    );
    CREATE INDEX idx_face_models_user ON face_models(user);
    CREATE TABLE face_embeddings (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        model_id INTEGER NOT NULL REFERENCES face_models(id) ON DELETE CASCADE,
        embedding BLOB NOT NULL,
        sealed INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_face_embeddings_model ON face_embeddings(model_id);
    CREATE TABLE rate_limit (user TEXT NOT NULL, attempt_time INTEGER NOT NULL);
    CREATE INDEX idx_rate_limit_user ON rate_limit(user);
    INSERT INTO schema_version (version) VALUES (5);
    """
)
connection.execute(
    "INSERT INTO face_models (user, label, created_at, embedder_model) "
    "VALUES ('testuser', 'fault', 1700000000, 'arcface')"
)
blob = struct.pack("<512f", *((index + 1) / 512 for index in range(512)))
for _ in range(3):
    connection.execute(
        "INSERT INTO face_embeddings (model_id, embedding, sealed) VALUES (1, ?, 0)",
        (blob,),
    )
connection.commit()
connection.close()
FAULT_SEED_PY
    cat >"$target/config.toml" <<EOF
[storage]
db_path = "$target/facelock.db"

[daemon]
model_dir = "/var/lib/facelock/models"

[device]
path = "/dev/video0"

[encryption]
method = "keyfile"
key_path = "$target/encryption.key"

[security]
allow_plaintext = true
EOF
}

# The daemon, bounded. Exit 124 means it was still running when the timeout
# fired, which is the healthy outcome; any other status is a startup failure,
# and `$target/daemon.log` says which.
run_fault_daemon() {
    local target="$1" seconds="${2:-30}" status=0
    RUST_LOG="${RUST_LOG:-warn}" timeout --foreground "$seconds" \
        facelock --config "$target/config.toml" daemon \
        >"$target/daemon.log" 2>&1 || status=$?
    printf '%s\n' "$status"
}

fault_inode() {
    stat -c %i "$1/facelock.db"
}

fault_schema_version() {
    python3 - "$1/facelock.db" <<'FAULT_SCHEMA_PY'
import sqlite3
import sys

try:
    connection = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
    print(
        connection.execute(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version"
        ).fetchone()[0]
    )
    connection.close()
except Exception as error:  # noqa: BLE001 - the value is the diagnosis
    print(f"unreadable: {error}")
FAULT_SCHEMA_PY
}

fault_row_count() {
    python3 - "$1/facelock.db" <<'FAULT_ROWS_PY'
import sqlite3
import sys

try:
    connection = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
    print(connection.execute("SELECT COUNT(*) FROM face_embeddings").fetchone()[0])
    connection.close()
except Exception as error:  # noqa: BLE001 - the value is the diagnosis
    print(f"unreadable: {error}")
FAULT_ROWS_PY
}

fault_face_models_readable() {
    python3 - "$1/facelock.db" <<'FAULT_READABLE_PY'
import sqlite3
import sys

try:
    connection = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
    connection.execute("SELECT COUNT(*) FROM face_models").fetchone()
    connection.close()
    print("readable")
except Exception:  # noqa: BLE001 - only the verdict matters
    print("unreadable")
FAULT_READABLE_PY
}

# Nothing below means anything unless the same command, in the same container,
# migrates a healthy database. Without this, a daemon that failed to start for
# an unrelated reason would satisfy every "the migration failed" assertion.
fault_positive_control() {
    case_name="fault-positive-control"
    local target="$FAULT_ROOT/control" status
    seed_fault_database "$target"
    status="$(run_fault_daemon "$target" 45)"
    assert_eq 6 "$(fault_schema_version "$target")" \
        "positive control: the candidate daemon migrates a healthy V5 database"
    assert_eq 3 "$(fault_row_count "$target")" "positive control: rows after migration"
    [ "$status" = 124 ] || {
        tail -20 "$target/daemon.log" >&2
        fail "positive control: the candidate daemon did not stay up (exit $status)"
    }
    pass "positive control: a healthy V5 database migrates to V6"
}

# Shared by the failure cases: the file is the same file, its rows are still
# there, and its schema was not carried forward.
assert_fault_database_survived() {
    local target="$1" inode="$2" rows="$3" label="$4"
    assert_eq "$inode" "$(fault_inode "$target")" \
        "$label: the database was replaced rather than left alone"
    assert_eq 5 "$(fault_schema_version "$target")" "$label: schema version"
    assert_eq "$rows" "$(fault_row_count "$target")" "$label: row count"
}

fault_disk_full() {
    case_name="fault-disk-full"
    local target="$FAULT_FULL_ROOT" inode rows status
    seed_fault_database "$target"
    inode="$(fault_inode "$target")"
    rows="$(fault_row_count "$target")"

    # Fill the filesystem the database lives on. dd stops at ENOSPC by itself,
    # and the probe below refuses to continue if it did not.
    dd if=/dev/zero of="$target/ballast" bs=64k status=none 2>/dev/null || true
    if dd if=/dev/zero of="$target/probe" bs=64k count=1 status=none 2>/dev/null; then
        rm -f "$target/probe" "$target/ballast"
        fail "the disk-full fixture is not full: $target still accepts writes"
    fi

    status="$(run_fault_daemon "$target" 45)"
    rm -f "$target/ballast"
    [ "$status" != 124 ] || {
        tail -20 "$target/daemon.log" >&2
        fail "the daemon kept running on a database it could not migrate"
    }
    assert_fault_database_survived "$target" "$inode" "$rows" "disk-full migration"
    rm -rf "$target"
    pass "a disk-full migration leaves the database intact"
}

fault_interrupted() {
    case_name="fault-interrupted"
    local target="$FAULT_ROOT/interrupted" inode rows holder migrator
    seed_fault_database "$target"
    inode="$(fault_inode "$target")"
    rows="$(fault_row_count "$target")"

    # Hold the write lock so the migration blocks mid-flight, then kill it.
    python3 - "$target/facelock.db" "$target/holder.ready" "$target/holder.stop" \
        <<'FAULT_HOLDER_PY' &
import pathlib
import sqlite3
import sys
import time

connection = sqlite3.connect(sys.argv[1], isolation_level=None)
connection.execute("BEGIN EXCLUSIVE")
pathlib.Path(sys.argv[2]).touch()
stop = pathlib.Path(sys.argv[3])
while not stop.exists():
    time.sleep(0.1)
connection.rollback()
connection.close()
FAULT_HOLDER_PY
    holder=$!
    for _ in $(seq 1 100); do
        if [ -f "$target/holder.ready" ]; then break; fi
        sleep 0.1
    done
    [ -f "$target/holder.ready" ] || fail "the exclusive-lock holder never started"

    RUST_LOG=warn facelock --config "$target/config.toml" daemon \
        >"$target/daemon.log" 2>&1 &
    migrator=$!
    sleep 20
    kill -KILL "$migrator" 2>/dev/null || true
    wait "$migrator" 2>/dev/null || true
    touch "$target/holder.stop"
    wait "$holder" 2>/dev/null || true

    assert_fault_database_survived "$target" "$inode" "$rows" "interrupted migration"
    pass "an interrupted migration leaves the database intact"
}

fault_corrupt_database() {
    case_name="fault-corrupt-database"
    local target="$FAULT_ROOT/corrupt" inode status
    seed_fault_database "$target"

    # Repoint face_models at a page of the wrong btree type: the database still
    # opens, and only fails when a query walks the table. Same trick as
    # facelock_test_support::schema_faults::break_face_models_table, which is
    # the Rust-side owner of this schema coupling.
    python3 - "$target/facelock.db" <<'FAULT_CORRUPT_PY'
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
connection.executescript(
    "CREATE TABLE d1(x); CREATE INDEX di1 ON d1(x); CREATE TABLE d2(x);"
)


def page(name):
    return connection.execute(
        "SELECT rootpage FROM sqlite_master WHERE name = ?", (name,)
    ).fetchone()[0]


donors = (page("d1"), page("di1"), page("d2"))
connection.executescript(
    f"""
    PRAGMA writable_schema=ON;
    UPDATE sqlite_master SET rootpage={donors[1]} WHERE name='face_models';
    UPDATE sqlite_master SET rootpage={donors[0]} WHERE name='idx_face_models_user';
    UPDATE sqlite_master SET rootpage={donors[2]} WHERE name='sqlite_autoindex_face_models_1';
    DELETE FROM sqlite_master WHERE name IN ('d1','di1','d2');
    PRAGMA writable_schema=OFF;
    """
)
for name in ("face_models", "idx_face_models_user", "sqlite_autoindex_face_models_1"):
    if (
        connection.execute(
            "SELECT rootpage FROM sqlite_master WHERE name = ?", (name,)
        ).fetchone()[0]
        not in donors
    ):
        raise SystemExit(f"fault injection did not repoint {name}")
connection.commit()
connection.close()
FAULT_CORRUPT_PY
    assert_eq unreadable "$(fault_face_models_readable "$target")" \
        "fault injection did not make face_models unreadable"
    inode="$(fault_inode "$target")"

    status="$(run_fault_daemon "$target" 45)"
    [ "$status" != 124 ] || {
        tail -20 "$target/daemon.log" >&2
        fail "the daemon served a database whose enrollment table cannot be read"
    }
    assert_eq "$inode" "$(fault_inode "$target")" \
        "the corrupt database was replaced instead of refused"
    # Anything that "repaired" the database by recreating it would leave
    # face_models readable again. It must not be.
    assert_eq unreadable "$(fault_face_models_readable "$target")" \
        "the corrupt database was silently rebuilt"
    pass "a corrupt database is refused, never reset"
}

fault_concurrent_key_creation() {
    case_name="fault-concurrent-key-creation"
    local target="$FAULT_ROOT/concurrent" keys
    seed_fault_database "$target"
    # A plaintext-only legacy database with no key artifact: the one state where
    # creating the default key is allowed at all.
    [ ! -e "$target/encryption.key" ] || fail "the concurrent fixture already has a key"

    for _ in $(seq 1 8); do
        (
            RUST_LOG=warn timeout --foreground 40 \
                facelock --config "$target/config.toml" daemon \
                >>"$target/daemon.log" 2>&1 || true
        ) &
    done
    wait

    [ -f "$target/encryption.key" ] ||
        fail "no key was created for a plaintext-only legacy database"
    assert_eq 32 "$(stat -c %s "$target/encryption.key")" "concurrently created key size"
    assert_eq "600:0:0" "$(stat -c '%a:%u:%g' "$target/encryption.key")" \
        "concurrently created key metadata"
    # Exactly one winner. O_EXCL is what makes that true: with O_TRUNC the last
    # writer replaces the key the first one's rows were sealed under, and a
    # reader in between gets a half-written key.
    keys="$(find "$target" -maxdepth 1 -name 'encryption.key*' | wc -l)"
    assert_eq 1 "$keys" "key artifacts after a concurrent creation race"
    pass "concurrent key creation resolves to exactly one key"
}

run_faults() {
    install -d -m0700 "$FAULT_ROOT"
    fault_positive_control
    local fault
    for fault in "${FAULTS[@]}"; do
        case "$fault" in
            disk-full) fault_disk_full ;;
            interrupted) fault_interrupted ;;
            corrupt-database) fault_corrupt_database ;;
            concurrent-key-creation) fault_concurrent_key_creation ;;
            *) fail "no implementation for declared fault: $fault" ;;
        esac
    done
}

# --- entry point -----------------------------------------------------------

install -d -m0700 "$STATE_ROOT"
: >"$LOG"

assert_pinned_artifacts
start_swtpm

for shape in "${SHAPES[@]}"; do
    run_shape "$shape"
done

# The faults run against the candidate, so install it once more and leave it in
# place: a fault case is about the candidate's migration, not the predecessor's.
case_name="fault-setup"
if pkg_is_installed; then pkg_remove; fi
wipe_state
pkg_install "$CANDIDATE" || fail "the candidate did not install for the fault matrix"
install -d -m0755 /var/lib/facelock/models
if [ -d /facelock-test-models ]; then
    for model in /facelock-test-models/*.onnx; do
        if [ -f "$model" ]; then install -m0644 "$model" /var/lib/facelock/models/; fi
    done
fi
run_faults

echo
echo "OK: $FAMILY v0.1.4 upgrade, rollback and fault matrix"
