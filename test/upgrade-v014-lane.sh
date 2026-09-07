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
#   * the V5 database migrated to V7 and legacy rows carry device_id = NULL
#     and key_id = NULL
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
# read its own encrypted rows. V7 has no down-migration, so this is the only
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
        # `dnf downgrade` on a local file is the supported path; `rpm -Uvh
        # --oldpackage` is the other real one an administrator would reach for.
        # The fallback prints, so a run that needed it says so rather than
        # quietly proving something about a different command.
        pkg_downgrade() {
            if dnf downgrade -y "$1" >>"$LOG" 2>&1; then
                return 0
            fi
            echo "NOTE: dnf downgrade refused $1; falling back to rpm -Uvh --oldpackage" >&2
            rpm -Uvh --oldpackage "$1" >>"$LOG" 2>&1
        }
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

# The bus the daemon needs, and the daemon that would otherwise be holding it.
#
# dbus arrives as a dependency of the facelock transaction, so its socket unit
# was never started at boot and /run/dbus/system_bus_socket does not exist until
# something starts it. deb-package-lifecycle.sh's assert_system_bus_available
# does the same thing for the same reason.
ensure_system_bus() {
    [ -d /run/systemd/system ] || return 0
    systemctl start dbus.socket >>"$LOG" 2>&1 || true
    [ -S /run/dbus/system_bus_socket ] ||
        fail "system bus socket is missing after a package transaction"
}

# pamtester reaches pam_facelock.so, which can D-Bus-activate the packaged
# daemon. A foreground `facelock daemon` started afterwards cannot claim
# org.facelock.Daemon and exits non-zero -- which reads exactly like the daemon
# refusing to serve, and is not.
# The candidate refuses to be uninstalled while a PAM service still references
# pam_facelock.so, so the reference goes first. The predecessor has no such
# guard, which is why the Debian half never needed this: after a rollback it is
# always v0.1.4 that gets removed.
remove_installed_package() {
    rm -f "$PAM_PATH"
    pkg_remove
}

stop_packaged_daemon() {
    [ -d /run/systemd/system ] || return 0
    systemctl stop facelock-daemon.service >>"$LOG" 2>&1 || true
}

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

# The layout v0.1.4 leaves behind. Its postinst runs systemd-sysusers and
# systemd-tmpfiles and then chowns the state directories to root:facelock 0750
# (models to root:root 0755); ADR 010 retired that group and tightened those
# modes. Seeding the new modes here, as this lane first did, meant the upgrade
# had nothing to tighten and every mode assertion passed on state the lane had
# already put in the right shape.
#
# `pam-backups` is deliberately absent: it is new-layout, and the candidate's
# postinst has to create it.
seed_legacy_layout() {
    local legacy_group=root
    if getent group facelock >/dev/null 2>&1; then
        legacy_group=facelock
    fi
    install -d /var/lib/facelock /var/lib/facelock/models \
        /var/log/facelock /var/log/facelock/snapshots
    chown "root:$legacy_group" /var/lib/facelock /var/log/facelock \
        /var/log/facelock/snapshots
    chmod 0750 /var/lib/facelock /var/log/facelock /var/log/facelock/snapshots
    chown root:root /var/lib/facelock/models
    chmod 0755 /var/lib/facelock/models
    # Holds the enrollment marker, so it has to exist; created loose so the
    # tightening to 0711 is exercised too.
    install -d -m0750 /var/lib/facelock/enrolled
}

seed_common_state() {
    seed_legacy_layout

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
    snapshot_model_files
    snapshot_rows
}

# Model files by content, not just name/size/mode. Size and mode alone let an
# upgrade that rewrites a model file at the same size and permissions pass the
# "preserve models" check without ever proving the bytes didn't change; the
# sha256 closes that gap the same way snapshot_file does for the other
# invariant files.
snapshot_model_files() {
    local name mpath
    while IFS= read -r name; do
        mpath="/var/lib/facelock/models/$name"
        printf 'model|%s|%s|%s|%s\n' \
            "$name" \
            "$(LC_ALL=C stat -c '%s' -- "$mpath")" \
            "$(LC_ALL=C stat -c '%a' -- "$mpath")" \
            "$(sha256sum -- "$mpath" | cut -d' ' -f1)"
    done < <(find /var/lib/facelock/models -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort)
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

assert_schema_v7_with_null_columns() {
    assert_eq 7 "$(schema_version)" "schema version after the candidate opened the database"
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
    case ",$columns," in
        *,key_id,*) ;;
        *) fail "V7 migration did not add face_models.key_id (columns: $columns)" ;;
    esac
    # Legacy rows must stay uncoupled. A migration that invented a device id
    # or a key id would bind every existing template to whatever camera
    # happened to be plugged in during the upgrade, or to a key it was never
    # sealed under.
    assert_eq 0 "$(sqlite_query 'SELECT COUNT(*) FROM face_models WHERE device_id IS NOT NULL')" \
        "legacy rows left with a non-NULL device_id"
    assert_eq 0 "$(sqlite_query 'SELECT COUNT(*) FROM face_models WHERE key_id IS NOT NULL')" \
        "legacy rows left with a non-NULL key_id"
}

# Which model's first embedding is the known fixture for a given shape. The
# mixed shape is why this exists: it holds an encrypted model and a plaintext
# one, both carrying the known blob, so "the first 2048-byte blob" would find
# the plaintext copy and a decrypt that did nothing would pass.
shape_probe_label() {
    case "$1" in
        plaintext) echo plaintext ;;
        keyfile) echo keyfile ;;
        mixed) echo encrypted-half ;;
        tpm-pcr-unbound | tpm-pcr-bound) echo tpm ;;
        *) fail "no probe label for shape: $1" ;;
    esac
}

# SQLite's online backup API, not `cp`. A live WAL means the file on disk is not
# the whole database, and a checkpoint landing mid-copy turns the probe into a
# corruption failure that has nothing to do with what is under test.
copy_database_for_probe() {
    local destination="$1"
    python3 - /var/lib/facelock/facelock.db "$destination" <<'PROBE_COPY_PY'
import sqlite3
import sys

source = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
target = sqlite3.connect(sys.argv[2])
with target:
    source.backup(target)
target.close()
source.close()
PROBE_COPY_PY
}

# The blob stored against one model's first embedding, as a digest. Fails
# loudly if the row is missing or is not a raw 512-float embedding, so a blob
# left encrypted is a failure rather than a digest that happens not to match.
probe_known_embedding_digest() {
    local probe_db="$1" label="$2"
    python3 - "$probe_db" "$label" <<'PROBE_DIGEST_PY'
import hashlib
import sqlite3
import sys

connection = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
row = connection.execute(
    "SELECT fe.embedding FROM face_embeddings AS fe "
    "JOIN face_models AS fm ON fm.id = fe.model_id "
    "WHERE fm.label = ? ORDER BY fe.id LIMIT 1",
    (sys.argv[2],),
).fetchone()
connection.close()
if row is None:
    raise SystemExit(f"no embedding row for model label {sys.argv[2]!r}")
blob = row[0]
if len(blob) != 2048:
    raise SystemExit(
        f"embedding for {sys.argv[2]!r} is {len(blob)} bytes, not a decrypted "
        "512-float embedding"
    )
print(hashlib.sha256(blob).hexdigest())
PROBE_DIGEST_PY
}

# Stage a copy of the live database plus a config pointing at it, so a decrypt
# run for evidence never touches the state under test.
stage_probe() {
    local probe="$1"
    rm -rf "$probe"
    install -d -m0700 "$probe"
    copy_database_for_probe "$probe/probe.db"
    sed "s|^db_path = .*|db_path = \"$probe/probe.db\"|" \
        /etc/facelock/config.toml >"$probe/config.toml"
}

# The proof a file hash cannot give: take the blob the released binary wrote,
# decrypt it with the candidate and the preserved key, and compare the
# plaintext to the digest this lane pinned before any of it was encrypted.
assert_known_embedding_decrypts() {
    local shape="$1" probe=/tmp/facelock-decrypt-probe label digest
    label="$(shape_probe_label "$shape")"
    stage_probe "$probe"

    if [ "$shape" != plaintext ]; then
        facelock --config "$probe/config.toml" tpm decrypt >>"$LOG" 2>&1 ||
            fail "the candidate could not decrypt rows the released binary encrypted"
    fi

    digest="$(probe_known_embedding_digest "$probe/probe.db" "$label")" ||
        fail "the known embedding for '$label' was not recoverable after the upgrade"
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
# present and the key artifact is missing or unusable -- the upgrade-day failure
# that turns a recoverable backup restore into permanent loss.
#
# Two shapes carry a plaintext keyfile over encrypted rows and so can have that
# key taken away: `keyfile` and `mixed`. The others are skipped for a reason,
# not for coverage: `plaintext` has no encrypted row to protect and is the one
# state where creating a key is allowed, and the TPM shapes keep their key
# sealed rather than on disk, so removing the plaintext artifact would prove
# nothing about the key actually in use. TPM unseal failure is
# test/tpm-pcr-e2e.sh's subject.
assert_no_replacement_key_over_encrypted_state() {
    local shape="$1"
    case "$shape" in
        keyfile | mixed) ;;
        *) return 0 ;;
    esac
    local saved="$STATE_ROOT/key.saved"
    cp /etc/facelock/encryption.key "$saved"

    # Missing, then malformed. "Malformed" is the case an operator actually
    # hits: a truncated key from a half-finished restore or a filled disk looks
    # present, and a replacement written over it is just as final as one
    # written over nothing.
    assert_key_refusal "$shape" missing "$saved"
    assert_key_refusal "$shape" malformed "$saved"

    install -m0600 "$saved" /etc/facelock/encryption.key
    rm -f "$saved"
}

assert_key_refusal() {
    local shape="$1" variant="$2" saved="$3"
    local output="/tmp/facelock-replacement-key-$variant.log" status=0
    stop_packaged_daemon
    case "$variant" in
        missing) rm -f /etc/facelock/encryption.key ;;
        malformed)
            printf 'not-a-key' >/etc/facelock/encryption.key
            chmod 0600 /etc/facelock/encryption.key
            ;;
        *) fail "unknown key-refusal variant: $variant" ;;
    esac

    # The daemon must keep running: refusing to write a key must not turn an
    # authentication that already falls through to password into a lockout.
    # Exit 124 is the timeout firing on a daemon that is still up.
    RUST_LOG=warn timeout --foreground 30 facelock daemon >"$output" 2>&1 || status=$?

    # The key must be exactly as this function left it. Anything else means the
    # daemon wrote over it.
    local now="absent"
    if [ -e /etc/facelock/encryption.key ]; then
        now="$(stat -c %s /etc/facelock/encryption.key)"
    fi
    local want="absent"
    [ "$variant" = malformed ] && want=9
    [ "$now" = "$want" ] || {
        install -m0600 "$saved" /etc/facelock/encryption.key
        fail "the candidate replaced a $variant key over encrypted rows (size now $now)"
    }
    [ "$status" = 124 ] || {
        tail -20 "$output" >&2
        install -m0600 "$saved" /etc/facelock/encryption.key
        fail "the daemon stopped serving on a $variant key (exit $status)"
    }
    # Assert what the message has to *contain*, not how it opens. The two
    # variants reach the operator by different routes -- a missing key raises
    # "refusing to write an encryption key at <path>", a malformed one raises
    # "keyfile could not be read: ... must be exactly 32 bytes, got 9" -- but
    # both carry the same tail from `key_policy::encrypted_rows_at_risk`, and
    # that tail is the actual contract: say which rows are at risk and how to
    # get them back. Pinning the opening clause made this red on wording when
    # the message was reworded, while a daemon that logged the opening and
    # nothing else would have passed.
    # shellcheck disable=SC2015 # deliberate: either grep failing takes the block
    grep -qE 'row\(s\) are (software-encrypted|TPM-sealed)' "$output" &&
        grep -qF 'facelock clear' "$output" || {
        tail -20 "$output" >&2
        install -m0600 "$saved" /etc/facelock/encryption.key
        fail "the daemon did not report the $variant key over encrypted rows"
    }
    # Password still works with the key unusable: that fall-through is the whole
    # reason refusing to write a replacement is the safe choice.
    assert_real_password_behavior
    install -m0600 "$saved" /etc/facelock/encryption.key
    pass "$shape refuses a replacement over a $variant key and keeps serving"
}

# The paths ADR 010 governs and the mode each must end up at. Read by both the
# pre-upgrade recording and the post-upgrade assertion, so the two cannot drift.
adr010_expectations() {
    cat <<'EOF'
711:0:0 /var/lib/facelock
755:0:0 /var/lib/facelock/models
711:0:0 /var/lib/facelock/enrolled
700:0:0 /var/lib/facelock/pam-backups
700:0:0 /var/log/facelock
700:0:0 /var/log/facelock/snapshots
600:0:0 /var/lib/facelock/facelock.db
600:0:0 /var/log/facelock/audit.jsonl
EOF
}

record_adr010_modes() {
    local shape="$1" expected path
    : >"$STATE_ROOT/$shape.modes"
    while read -r expected path; do
        if [ -e "$path" ]; then
            printf '%s %s\n' "$(stat -c '%a:%u:%g' "$path")" "$path" \
                >>"$STATE_ROOT/$shape.modes"
        else
            printf 'absent %s\n' "$path" >>"$STATE_ROOT/$shape.modes"
        fi
    done < <(adr010_expectations)
}

# ADR 010 modes. The packaged scriptlets tighten these on an upgrade from a
# release that predates the layout; content must be untouched while they do.
#
# Every path is required to exist afterwards. A `continue` on a missing path
# made all of this optional, and `pam-backups` in particular only exists
# because the candidate's postinst creates it -- so its absence is the failure
# this is here to catch, not a case to skip.
assert_adr010_modes() {
    local shape="$1" expected path actual before tightened=0
    while read -r expected path; do
        [ -e "$path" ] ||
            fail "ADR 010 path missing after the upgrade: $path"
        actual="$(stat -c '%a:%u:%g' "$path")"
        assert_eq "$expected" "$actual" "ADR 010 metadata for $path"
        before="$(awk -v p="$path" '$2 == p {print $1}' "$STATE_ROOT/$shape.modes")"
        if [ -n "$before" ] && [ "$before" != "$expected" ]; then
            tightened=$((tightened + 1))
        fi
    done < <(adr010_expectations)

    # At least one path has to have actually moved. If the predecessor already
    # left everything at the ADR 010 values, this lane is asserting nothing
    # about the upgrade and the legacy seeding above has silently stopped
    # working.
    [ "$tightened" -gt 0 ] ||
        fail "no ADR 010 path changed across the upgrade: nothing was tightened"

    # ADR 010 retired the group outright; an upgrade from a release that had
    # one must remove it rather than leave a group owning nothing.
    ! getent group facelock >/dev/null 2>&1 ||
        fail "the retired facelock group survived the upgrade"
}

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

# `grep -F "$(sha256sum "$path")"` matches every line of the snapshot when the
# file is gone, because sha256sum prints nothing and the empty pattern matches.
# So the file has to be proved present before its digest is looked up.
assert_file_digest_in_snapshot() {
    local path="$1" snapshot="$2" description="$3" line
    [ -f "$path" ] && [ ! -L "$path" ] ||
        fail "$description: $path is missing or is not a regular file"
    line="$(sha256sum -- "$path")"
    [ -n "$line" ] || fail "$description: could not digest $path"
    grep -Fxq -- "$line" "$snapshot" ||
        fail "$description: $path changed"
}

assert_pam_path_intact() {
    local before="$1" shape="$2"
    assert_file_digest_in_snapshot "$PAM_PATH" "$before" \
        "the upgrade rewrote the administrator's PAM service"
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
# The rollback question is not "does v0.1.4 read a V7 file" in the abstract, it
# is "does it read the file the alpha daemon has already opened, migrated and
# written to". So the daemon runs for real before the downgrade.
open_database_with_candidate_daemon() {
    local output=/tmp/facelock-candidate-daemon.log pid
    stop_packaged_daemon
    RUST_LOG=warn facelock daemon >"$output" 2>&1 &
    pid=$!
    # Wait for the outcome, not for a proxy that races it.
    # `reconcile_enrollment_markers` runs at daemon.rs:346, *before*
    # `build_handler_from`, and the migration happens inside it -- so the store
    # reaches V7 partway through reconciliation. Polling on the schema version
    # alone stopped the daemon mid-reconcile, which the Debian half won by luck
    # and the Fedora half lost.
    for _ in $(seq 1 120); do
        if [ "$(schema_version)" = 7 ] &&
            [ "$(marker_model_count)" = "$(database_model_count)" ]; then
            break
        fi
        sleep 0.5
    done
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    if [ "$(marker_model_count)" != "$(database_model_count)" ]; then
        echo "--- candidate daemon output ---" >&2
        tail -20 "$output" >&2 || true
    fi
    assert_eq 7 "$(schema_version)" "schema version after the candidate daemon ran"
}

database_model_count() {
    sqlite_query "SELECT COUNT(*) FROM face_models WHERE user = 'testuser'"
}

# Never fails: a marker that is absent or unreadable is a value the caller
# compares, not an error that stops the poll.
marker_model_count() {
    python3 - /var/lib/facelock/enrolled/testuser <<'MARKER_COUNT_PY' 2>/dev/null || echo unknown
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        print(json.load(handle)["models"])
except Exception:  # noqa: BLE001 - any failure is just "not yet"
    print("unknown")
MARKER_COUNT_PY
}

# --- rollback --------------------------------------------------------------

assert_downgrade_usable() {
    local shape="$1" probe=/tmp/facelock-rollback-probe label digest
    case_name="$shape-downgrade"

    record_swtpm_state
    pkg_downgrade "$PREDECESSOR" ||
        fail "the package manager refused to downgrade to the pinned predecessor"
    assert_eq "$(cat "$PREDECESSOR_VERSION_FILE")" "$(pkg_installed_version)" \
        "installed version after downgrade"
    ensure_system_bus

    # V7 has no down-migration. The predecessor must still open the database the
    # candidate migrated and read its enrollment state. `tpm status` is the
    # readback rather than `list`, which in v0.1.4 is a D-Bus call to a daemon
    # this phase deliberately does not run.
    released_facelock tpm status >>"$LOG" 2>&1 ||
        fail "the released binary could not open the V7 database after rollback"
    assert_eq 7 "$(schema_version)" "schema version is not rolled back by a package downgrade"

    # The rollback claim is that the old binary can still *read the templates*,
    # so it decrypts the known embedding and the plaintext is compared. An exit
    # code alone would pass on a decrypt that quietly did nothing.
    label="$(shape_probe_label "$shape")"
    stage_probe "$probe"
    if [ "$shape" != plaintext ]; then
        released_facelock --config "$probe/config.toml" decrypt >>"$LOG" 2>&1 ||
            fail "the released binary could not decrypt its own rows after rollback"
    fi
    digest="$(probe_known_embedding_digest "$probe/probe.db" "$label")" ||
        fail "the known embedding for '$label' was not recoverable after rollback"
    assert_eq "$KNOWN_EMBEDDING_SHA256" "$digest" \
        "known embedding plaintext recovered by the released binary after rollback"
    rm -rf "$probe"

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

    if pkg_is_installed; then remove_installed_package; fi
    wipe_state
    pkg_install "$PREDECESSOR" || fail "the pinned predecessor did not install"
    ensure_system_bus
    assert_eq "$(cat "$PREDECESSOR_VERSION_FILE")" "$(pkg_installed_version)" \
        "installed predecessor version"

    seed_shape "$shape"
    snapshot_invariant_state >"$STATE_ROOT/$shape.before"

    record_swtpm_state
    record_pam_enabled_state "$shape"
    record_adr010_modes "$shape"
    pkg_upgrade "$CANDIDATE" || fail "the native upgrade to the candidate failed"
    assert_eq "$(cat "$CANDIDATE_VERSION_FILE")" "$(pkg_installed_version)" \
        "installed candidate version after upgrade"
    ensure_system_bus
    assert_swtpm_state_untouched

    open_database_with_candidate_daemon

    snapshot_invariant_state >"$STATE_ROOT/$shape.after"
    cmp -s "$STATE_ROOT/$shape.before" "$STATE_ROOT/$shape.after" || {
        diff -u "$STATE_ROOT/$shape.before" "$STATE_ROOT/$shape.after" >&2 || true
        fail "the upgrade changed state it had to preserve"
    }

    assert_schema_v7_with_null_columns
    assert_known_embedding_decrypts "$shape"
    assert_enrollment_marker_reconciled
    assert_key_artifacts_preserved "$STATE_ROOT/$shape.before"
    assert_adr010_modes "$shape"
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
    stop_packaged_daemon
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
    assert_eq 7 "$(fault_schema_version "$target")" \
        "positive control: the candidate daemon migrates a healthy V5 database"
    assert_eq 3 "$(fault_row_count "$target")" "positive control: rows after migration"
    [ "$status" = 124 ] || {
        tail -20 "$target/daemon.log" >&2
        fail "positive control: the candidate daemon did not stay up (exit $status)"
    }
    pass "positive control: a healthy V5 database migrates to V7"
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
    local target="$FAULT_ROOT/corrupt" inode rows status
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

    rows="$(fault_row_count "$target")"
    status="$(run_fault_daemon "$target" 45)"

    # The daemon is required to keep serving. Exiting on a corrupt database
    # would take D-Bus activation down with it, and a PAM module that cannot
    # reach the daemon is one step from a machine nobody can log into. Falling
    # through to password with the corruption reported is the safe direction,
    # so it is asserted rather than merely tolerated.
    [ "$status" = 124 ] || {
        tail -20 "$target/daemon.log" >&2
        fail "the daemon stopped serving on a corrupt database (exit $status)"
    }
    # Reported, not swallowed. A daemon that silently treats an unreadable
    # enrollment table as "nobody is enrolled" is how a corrupt database turns
    # into a quiet, permanent loss of face authentication.
    grep -Eqi 'corrupt|malformed' "$target/daemon.log" || {
        tail -20 "$target/daemon.log" >&2
        fail "the daemon never named the corruption it hit"
    }
    assert_eq "$inode" "$(fault_inode "$target")" \
        "the corrupt database was replaced instead of left alone"
    # Anything that "repaired" the database by recreating it would leave
    # face_models readable again. It must not be.
    assert_eq unreadable "$(fault_face_models_readable "$target")" \
        "the corrupt database was silently rebuilt"
    assert_eq "$rows" "$(fault_row_count "$target")" \
        "embedding rows after a corrupt-database start"
    pass "a corrupt database is reported and left alone, never reset"
}

fault_concurrent_key_creation() {
    case_name="fault-concurrent-key-creation"
    local target="$FAULT_ROOT/concurrent" keys creators digest
    seed_fault_database "$target"
    # A plaintext-only legacy database with no key artifact: the one state where
    # creating the default key is allowed at all.
    [ ! -e "$target/encryption.key" ] || fail "the concurrent fixture already has a key"
    stop_packaged_daemon

    # RUST_LOG=info so the creation line is emitted, and one log per racer so
    # the count below cannot be confused by interleaved writes.
    local index
    for index in $(seq 1 8); do
        (
            RUST_LOG=info timeout --foreground 40 \
                facelock --config "$target/config.toml" daemon \
                >"$target/race-$index.log" 2>&1 || true
        ) &
    done
    wait

    [ -f "$target/encryption.key" ] ||
        fail "no key was created for a plaintext-only legacy database"
    assert_eq 32 "$(stat -c %s "$target/encryption.key")" "concurrently created key size"
    assert_eq "600:0:0" "$(stat -c '%a:%u:%g' "$target/encryption.key")" \
        "concurrently created key metadata"
    keys="$(find "$target" -maxdepth 1 -name 'encryption.key*' | wc -l)"
    assert_eq 1 "$keys" "key artifacts after a concurrent creation race"

    # The assertion that actually separates O_EXCL from O_TRUNC. One key file on
    # disk is true either way: with O_TRUNC every racer creates and writes,
    # each overwriting the last, and the file count stays one. With O_EXCL
    # exactly one racer creates it and the other seven are told AlreadyExists
    # and read what the winner wrote, so exactly one of them logs the creation.
    creators="$(cat "$target"/race-*.log | grep -c 'generated encryption key' || true)"
    assert_eq 1 "$creators" "racers that created the key"

    # And the winner's bytes are the bytes that survive: a later start must read
    # the existing key, never replace it.
    digest="$(sha256sum "$target/encryption.key" | cut -d' ' -f1)"
    run_fault_daemon "$target" 20 >/dev/null
    assert_eq "$digest" "$(sha256sum "$target/encryption.key" | cut -d' ' -f1)" \
        "the existing key survived a later daemon start"
    pass "concurrent key creation resolves to exactly one key, written once"
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
if pkg_is_installed; then remove_installed_package; fi
wipe_state
pkg_install "$CANDIDATE" || fail "the candidate did not install for the fault matrix"
ensure_system_bus
stop_packaged_daemon
install -d -m0755 /var/lib/facelock/models
if [ -d /facelock-test-models ]; then
    for model in /facelock-test-models/*.onnx; do
        if [ -f "$model" ]; then install -m0644 "$model" /var/lib/facelock/models/; fi
    done
fi
run_faults

echo
echo "OK: $FAMILY v0.1.4 upgrade, rollback and fault matrix"
