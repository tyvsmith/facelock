#!/bin/bash
set -euo pipefail

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

echo "=== PAM Container Tests ==="
echo ""

# Test 1: Module loads without crash
run_test "Module loads without crash" \
    "pamtester facelock-test testuser authenticate" \
    "any"

# Test 2: PAM returns PAM_IGNORE when daemon not running
# pamtester returns non-zero when auth fails, but the module shouldn't crash
run_test "Module returns gracefully when daemon not running" \
    "pamtester facelock-test testuser authenticate < /dev/null" \
    "any"

# Test 3: Module handles missing config gracefully
run_test "Module handles missing config" \
    "mv /etc/facelock/config.toml /etc/facelock/config.toml.bak && pamtester facelock-test testuser authenticate; mv /etc/facelock/config.toml.bak /etc/facelock/config.toml" \
    "any"

# Test 4: Disabled config returns PAM_IGNORE
run_test "Module respects disabled config" \
    "sed -i 's/disabled = false/disabled = true/' /etc/facelock/config.toml && pamtester facelock-test testuser authenticate; sed -i 's/disabled = true/disabled = false/' /etc/facelock/config.toml" \
    "any"

# Test 5: PAM symbols are exported
run_test "pam_sm_authenticate symbol exists" \
    "nm -D /lib/security/pam_facelock.so | grep -q pam_sm_authenticate" \
    0

run_test "pam_sm_setcred symbol exists" \
    "nm -D /lib/security/pam_facelock.so | grep -q pam_sm_setcred" \
    0

# --- Spec 28: Privilege enforcement ---

run_test "facelock setup requires root" \
    "su -s /bin/bash testuser -c 'facelock setup 2>&1' | grep -q 'Root required'" \
    0

run_test "facelock daemon requires root" \
    "su -s /bin/bash testuser -c 'facelock daemon 2>&1' | grep -q 'Root required'" \
    0

# --- #174: `facelock pam add | remove | status` against the real /etc/pam.d ---
#
# The verb is the only writer of /etc/pam.d, and everything that decides
# whether it writes is machine state a tempdir test cannot stand in for: the
# root check, the pam_facelock.so-is-installed check, the hard-coded
# /etc/pam.d base, and the C-locale text of the refusals. This block proves
# the whole path as root on a live system — install, idempotence, the status
# probe's 0/1 scale, the sensitive-service gate, two-phase validation, name
# confinement, removal, and the `setup --pam` alias landing in the same file.
#
# It writes only to service files it creates itself (facelock-scratch*) and
# removes them at the end, so it is safe to run twice; sudo and the sensitive
# services are read and never written — the one row that aims at a sensitive
# service asserts a refusal, and saves and restores the file either way. The
# --json documents are asserted with python, which the fake-daemon harness
# already requires.

PAM_LINE_TEXT='auth      sufficient pam_facelock.so'

# The `action` word for the first service in a `facelock pam --json` document.
cat > /tmp/pam-action.py <<'EOF'
import json, sys
print(json.load(open(sys.argv[1]))["services"][0]["action"])
EOF

# Exit 0 only if the facelock line is present AND is the file's first `auth`
# line — the placement contract, asserted by index rather than by eyeball.
cat > /tmp/pam-first-auth.py <<'EOF'
import sys
line = "auth      sufficient pam_facelock.so"
lines = open(sys.argv[1]).read().splitlines()
auth = [i for i, text in enumerate(lines) if text.lstrip().startswith("auth")]
sys.exit(0 if line in lines and auth and lines[auth[0]] == line else 1)
EOF

# Two throwaway service files with a realistic body. Nothing consumes them.
rm -f /etc/pam.d/facelock-scratch /etc/pam.d/facelock-scratch2 \
      /etc/pam.d/facelock-scratch.facelock-backup \
      /etc/pam.d/facelock-scratch2.facelock-backup
find /var/lib/facelock/pam-backups -maxdepth 1 \
    \( -name 'facelock-scratch.*' -o -name 'facelock-scratch2.*' \) -delete
cat > /etc/pam.d/facelock-scratch <<'EOF'
#%PAM-1.0
auth       include        system-auth
account    include        system-auth
session    include        system-auth
EOF
chmod 644 /etc/pam.d/facelock-scratch
cp /etc/pam.d/facelock-scratch /etc/pam.d/facelock-scratch2

run_test "pam status: existing file without the line is 'missing', exit 1" \
    "facelock pam status --service facelock-scratch --json > /tmp/pam-status.json 2>/dev/null; test \$? -eq 1 && python3 /tmp/pam-action.py /tmp/pam-status.json | grep -qx missing" \
    0

run_test "pam status: an absent service file is exit 2" \
    "facelock pam status --service facelock-does-not-exist > /dev/null 2>&1; test \$? -eq 2" \
    0

# The pair `pam add --if-present` was always half of: install the optional
# integrations, then verify them. Without --if-present here, verifying an
# integration the host does not have is exit 2 and a `set -e` script dies on a
# service it deliberately made optional.
run_test "pam status --if-present: an absent service file is exit 0" \
    "facelock pam status --service facelock-does-not-exist --if-present" \
    0

run_test "pam add: committed root-only state backup, line is the first auth line" \
    "facelock pam add --service facelock-scratch --json > /tmp/pam-add.json 2>/dev/null; test \$? -eq 0 && python3 /tmp/pam-action.py /tmp/pam-add.json | grep -qx installed && backup=\$(python3 -c 'import json; print(json.load(open(\"/tmp/pam-add.json\"))[\"services\"][0][\"backup\"])') && case \"\$backup\" in /var/lib/facelock/pam-backups/facelock-scratch.*) ;; *) exit 1;; esac && test -f \"\$backup\" && test -f \"\$backup.json\" && test \"\$(stat -c '%a:%U:%G' \"\$backup\")\" = 600:root:root && test \"\$(stat -c '%a:%U:%G' \"\$backup.json\")\" = 600:root:root && python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert d[\"version\"] == 1 and d[\"sequence\"] > 0 and d[\"state\"] == \"committed\" and d[\"service\"] == \"facelock-scratch\" and \"path\" not in d and \"target\" not in d' \"\$backup.json\" && ! test -e /etc/pam.d/facelock-scratch.facelock-backup && python3 /tmp/pam-first-auth.py /etc/pam.d/facelock-scratch" \
    0

sha256sum /etc/pam.d/facelock-scratch > /tmp/pam-scratch.sha

run_test "pam add is idempotent: 'unchanged' and the file is byte-identical" \
    "facelock pam add --service facelock-scratch --json > /tmp/pam-add2.json 2>/dev/null; test \$? -eq 0 && python3 /tmp/pam-action.py /tmp/pam-add2.json | grep -qx unchanged && sha256sum -c --status /tmp/pam-scratch.sha" \
    0

run_test "pam status exits 0 once the line is present" \
    "facelock pam status --service facelock-scratch" \
    0

# --- P1b (#170): the module probe ---
#
# This is the only tier where pam_facelock.so is genuinely installed, so it is
# where a regressed probe shows: `just install-files` puts it at
# /lib/security, the first candidate. The key is top-level and additive, and
# `null` here would mean `pam add` is about to refuse on a machine that has
# the module.
cat > /tmp/pam-module-path.py <<'EOF'
import json, sys
print(json.load(open(sys.argv[1])).get("module_path"))
EOF

run_test "pam status --json reports where the module was found" \
    "facelock pam status --service facelock-scratch --json > /tmp/pam-module.json 2>/dev/null; python3 /tmp/pam-module-path.py /tmp/pam-module.json | grep -qx /lib/security/pam_facelock.so" \
    0

rm -f /tmp/pam-module.json /tmp/pam-module-path.py

# The sensitive-service gate. Arch's `pam` package ships /etc/pam.d/system-auth,
# so that is what this normally runs against; the loop keeps the row honest if
# the base image ever changes which of the three exists. The refusal is decided
# in the validation phase, before any prompt, so --no-confirm cannot unlock it.
PAM_SENSITIVE=""
for svc in system-auth password-auth common-auth system-login login sshd; do
    if [ -f "/etc/pam.d/$svc" ]; then
        PAM_SENSITIVE="$svc"
        break
    fi
done

if [ -n "$PAM_SENSITIVE" ]; then
    # Belt and braces: the sha256 below detects a regression, and the copy
    # taken here undoes one. This is the container's own auth stack, and every
    # row after this one runs through it, so a failure must not be allowed to
    # leak into them as a second, mystifying failure.
    cp -p "/etc/pam.d/$PAM_SENSITIVE" /tmp/pam-sensitive.orig
    sha256sum "/etc/pam.d/$PAM_SENSITIVE" > /tmp/pam-sensitive.sha
    run_test "pam add refuses sensitive service $PAM_SENSITIVE under --no-confirm" \
        "facelock pam add --service $PAM_SENSITIVE --no-confirm > /tmp/pam-sensitive.out 2>&1; test \$? -ne 0 && grep -q 'sensitive PAM service' /tmp/pam-sensitive.out && sha256sum -c --status /tmp/pam-sensitive.sha" \
        0
    # The alias has its own refusal, naming its own flag: `setup --yes` is the
    # documented exception that means both "do not ask" and "unlock the gate",
    # so the message has to say --yes and not --allow-sensitive. Without this
    # row the alias could lose the gate entirely and only the verb would notice.
    run_test "setup --pam refuses sensitive service $PAM_SENSITIVE without --yes" \
        "facelock setup --pam --service $PAM_SENSITIVE > /tmp/pam-sensitive-alias.out 2>&1; test \$? -ne 0 && grep -q 'sensitive PAM service' /tmp/pam-sensitive-alias.out && grep -q -- '--yes' /tmp/pam-sensitive-alias.out && sha256sum -c --status /tmp/pam-sensitive.sha" \
        0
    cp -p /tmp/pam-sensitive.orig "/etc/pam.d/$PAM_SENSITIVE"
    rm -f "/etc/pam.d/$PAM_SENSITIVE.facelock-backup" /tmp/pam-sensitive.orig \
          /tmp/pam-sensitive-alias.out
else
    echo "SKIP: no sensitive service file in the image to test the gate"
fi

sha256sum /etc/pam.d/facelock-scratch2 > /tmp/pam-scratch2.sha

# Two-phase: the second service is rejected in validation, so the first one —
# which would otherwise have been written by the time the failure happened —
# is untouched, has no backup, and no JSON document is emitted at all.
run_test "pam add validates every service before writing any" \
    "facelock pam add --service facelock-scratch2 --service facelock-does-not-exist --json > /tmp/pam-twophase.out 2>&1; test \$? -ne 0 && sha256sum -c --status /tmp/pam-scratch2.sha && ! test -e /etc/pam.d/facelock-scratch2.facelock-backup && ! grep -q '\"services\"' /tmp/pam-twophase.out" \
    0

# The message grep is what makes this row mean anything: without `confined`,
# `../facelock-escape` resolves to /etc/facelock-escape, which does not exist,
# so the command would still exit non-zero (file-not-found) and --dry-run would
# still write nothing — every `! test -e` would hold on the broken path too.
run_test "pam add rejects a service name that escapes /etc/pam.d" \
    "facelock pam add --service ../facelock-escape --dry-run > /tmp/pam-escape.out 2>&1; test \$? -ne 0 && grep -q 'Invalid PAM service name' /tmp/pam-escape.out && ! test -e /etc/facelock-escape && ! test -e /etc/facelock-escape.facelock-backup && ! test -e /etc/pam.d/facelock-escape" \
    0

# The authselect shape, on a real /etc/pam.d rather than a tempdir: a service
# file that is a symlink out of the directory is refused, not written through.
# The target is checked by hash because the failure mode this guards against is
# a *successful-looking* run that edited a file elsewhere — exit status alone
# would not have caught it.
cat > /tmp/facelock-outside <<'EOF'
#%PAM-1.0
auth       include        system-auth
EOF
sha256sum /tmp/facelock-outside > /tmp/facelock-outside.sha
ln -sfn /tmp/facelock-outside /etc/pam.d/facelock-scratch-link

run_test "pam add refuses a service file symlinked out of /etc/pam.d" \
    "facelock pam add --service facelock-scratch-link --no-confirm > /tmp/pam-symlink.out 2>&1; test \$? -ne 0 && grep -q 'is a symlink to' /tmp/pam-symlink.out && sha256sum -c --status /tmp/facelock-outside.sha && ! test -e /tmp/facelock-outside.facelock-backup && ! test -e /etc/pam.d/facelock-scratch-link.facelock-backup" \
    0

rm -f /etc/pam.d/facelock-scratch-link /tmp/facelock-outside \
      /tmp/facelock-outside.sha /tmp/pam-symlink.out

cp /etc/pam.d/facelock-scratch /etc/pam.d/facelock-scratch.facelock-backup
run_test "pam remove: line, owned state, and legacy backup are removed" \
    "facelock pam remove --service facelock-scratch --json > /tmp/pam-remove.json 2>/dev/null; test \$? -eq 0 && python3 /tmp/pam-action.py /tmp/pam-remove.json | grep -qx removed && ! grep -q pam_facelock.so /etc/pam.d/facelock-scratch && ! test -e /etc/pam.d/facelock-scratch.facelock-backup && ! find /var/lib/facelock/pam-backups -maxdepth 1 -name 'facelock-scratch.*' | grep -q ." \
    0

# The `setup --pam` alias must reach the same writer and the same bytes.
# Placement, not just presence: the alias and the verb share one writer, and
# the thing that would prove they had stopped sharing it is the line landing
# somewhere else. Asserted by index, with the same probe the verb's row uses.
run_test "setup --pam alias installs the line as the first auth line" \
    "facelock setup --pam --service facelock-scratch --yes > /dev/null 2>&1; test \$? -eq 0 && grep -qxF '$PAM_LINE_TEXT' /etc/pam.d/facelock-scratch && python3 /tmp/pam-first-auth.py /etc/pam.d/facelock-scratch" \
    0

run_test "setup --pam --remove alias removes the line" \
    "facelock setup --pam --service facelock-scratch --remove --yes --if-present > /dev/null 2>&1; test \$? -eq 0 && ! grep -q pam_facelock.so /etc/pam.d/facelock-scratch" \
    0

rm -f /etc/pam.d/facelock-scratch

run_test "setup --pam --remove --if-present succeeds on an absent service file" \
    "facelock setup --pam --service facelock-scratch --remove --yes --if-present" \
    0

# The add side of the same flag. A provisioning script configures a set of
# optional integrations in one pass; before this, the alias could only say
# "add", so a machine without hyprlock failed the whole run. Absence must be a
# successful no-op that creates nothing. Both halves are asserted: a `setup
# --pam` that reached exit 0 by writing a service file out of thin air would
# satisfy an exit-code-only row.
run_test "setup --pam --if-present succeeds on an absent service file" \
    "facelock setup --pam --service facelock-scratch --yes --if-present > /dev/null 2>&1; test \$? -eq 0 && ! test -e /etc/pam.d/facelock-scratch" \
    0

# ...and the default is still a hard error, which is what catches a typo'd
# --service rather than silently configuring nothing.
run_test "setup --pam without --if-present still fails on an absent service file" \
    "facelock setup --pam --service facelock-scratch --yes > /dev/null 2>&1; test \$? -ne 0 && ! test -e /etc/pam.d/facelock-scratch" \
    0

# --- P1: vendor pam.d resolution ---
#
# Linux-PAM reads /etc/pam.d first and /usr/lib/pam.d second, and packages have
# moved their configuration there: on this image `polkit` ships
# /usr/lib/pam.d/polkit-1 and /etc/pam.d/polkit-1 does not exist. No tempdir
# test can prove the real directories are the ones facelock reaches, and no row
# above ever exercised a service that exists *only* in a vendor directory —
# which is exactly how the bug shipped.
#
# The vendor file is hashed before and after every row. Exit status alone would
# not catch the failure that matters: a successful-looking run that edited the
# package's own file.

VENDOR_PAM_DIR=/usr/lib/pam.d
mkdir -p "$VENDOR_PAM_DIR"
rm -f "$VENDOR_PAM_DIR/facelock-vendor-scratch" \
      "$VENDOR_PAM_DIR/facelock-vendor-scratch.facelock-backup" \
      /etc/pam.d/facelock-vendor-scratch \
      /etc/pam.d/facelock-vendor-scratch.facelock-backup
cat > "$VENDOR_PAM_DIR/facelock-vendor-scratch" <<'EOF'
#%PAM-1.0
auth       include        system-auth
account    include        system-auth
session    include        system-auth
EOF
chmod 644 "$VENDOR_PAM_DIR/facelock-vendor-scratch"
sha256sum "$VENDOR_PAM_DIR/facelock-vendor-scratch" > /tmp/pam-vendor.sha
# The file's bytes are one assertion; the directory's contents are another. A
# stray temp file, a backup, or any new entry in a package-owned directory
# passes every per-file hash, so the whole listing is snapshotted too.
ls -a "$VENDOR_PAM_DIR" | LC_ALL=C sort > /tmp/pam-vendor-dir.before

run_test "pam status: a vendor-only service is 'vendor-only', exit 1" \
    "facelock pam status --service facelock-vendor-scratch --json > /tmp/pam-vendor-status.json 2>/dev/null; test \$? -eq 1 && python3 /tmp/pam-action.py /tmp/pam-vendor-status.json | grep -qx vendor-only && grep -q '$VENDOR_PAM_DIR/facelock-vendor-scratch' /tmp/pam-vendor-status.json && sha256sum -c --status /tmp/pam-vendor.sha" \
    0

# The headline row: the service is configured without the package's file being
# touched, and the override says in its own bytes where it came from.
run_test "pam add on a vendor-only service creates an /etc override" \
    "facelock pam add --service facelock-vendor-scratch --json > /tmp/pam-vendor-add.json 2>/dev/null; test \$? -eq 0 && python3 /tmp/pam-action.py /tmp/pam-vendor-add.json | grep -qx overridden && test -f /etc/pam.d/facelock-vendor-scratch && python3 /tmp/pam-first-auth.py /etc/pam.d/facelock-vendor-scratch && test \$(grep -c '^# Copied from $VENDOR_PAM_DIR/facelock-vendor-scratch' /etc/pam.d/facelock-vendor-scratch) -eq 1 && sha256sum -c --status /tmp/pam-vendor.sha && ! test -e $VENDOR_PAM_DIR/facelock-vendor-scratch.facelock-backup" \
    0

# Second add: the override now shadows the vendor file, so this is an ordinary
# in-place no-op — one header, not two, and still nothing written to /usr.
run_test "pam add again edits the override, writes no second header" \
    "facelock pam add --service facelock-vendor-scratch --json > /tmp/pam-vendor-add2.json 2>/dev/null; test \$? -eq 0 && python3 /tmp/pam-action.py /tmp/pam-vendor-add2.json | grep -qx unchanged && test \$(grep -c '^# Copied from ' /etc/pam.d/facelock-vendor-scratch) -eq 1 && sha256sum -c --status /tmp/pam-vendor.sha" \
    0

run_test "pam remove deletes the unchanged Facelock-created override" \
    "facelock pam remove --service facelock-vendor-scratch --json > /tmp/pam-vendor-remove.json 2>/dev/null; test \$? -eq 0 && python3 /tmp/pam-action.py /tmp/pam-vendor-remove.json | grep -qx removed && ! test -e /etc/pam.d/facelock-vendor-scratch && sha256sum -c --status /tmp/pam-vendor.sha" \
    0

run_test "pam remove keeps a drifted vendor override after removing its line" \
    "facelock pam add --service facelock-vendor-scratch --json > /dev/null 2>/dev/null && printf '%s\n' '# local customization' >> /etc/pam.d/facelock-vendor-scratch && facelock pam remove --service facelock-vendor-scratch --json > /tmp/pam-vendor-drift-remove.json 2>/dev/null; test \$? -eq 0 && python3 /tmp/pam-action.py /tmp/pam-vendor-drift-remove.json | grep -qx removed && test -f /etc/pam.d/facelock-vendor-scratch && ! grep -q pam_facelock.so /etc/pam.d/facelock-vendor-scratch && grep -qxF '# local customization' /etc/pam.d/facelock-vendor-scratch && sha256sum -c --status /tmp/pam-vendor.sha" \
    0

rm -f /etc/pam.d/facelock-vendor-scratch \
      /etc/pam.d/facelock-vendor-scratch.facelock-backup

run_test "pam remove on a vendor-only service is a no-op, exit 0" \
    "facelock pam remove --service facelock-vendor-scratch --json > /tmp/pam-vendor-remove2.json 2>/dev/null; test \$? -eq 0 && python3 /tmp/pam-action.py /tmp/pam-vendor-remove2.json | grep -qx vendor-only && ! test -e /etc/pam.d/facelock-vendor-scratch && sha256sum -c --status /tmp/pam-vendor.sha" \
    0

# A genuinely absent service still errors — and the message names every
# directory searched, not just the first. "Not found in /etc/pam.d" would send
# an operator to create a file a vendor directory may already hold.
run_test "an absent service names every directory searched" \
    "facelock pam add --service facelock-nowhere-scratch > /tmp/pam-nowhere.out 2>&1; test \$? -ne 0 && grep -q '/etc/pam.d/facelock-nowhere-scratch' /tmp/pam-nowhere.out && grep -q '$VENDOR_PAM_DIR/facelock-nowhere-scratch' /tmp/pam-nowhere.out" \
    0

run_test "the vendor directory gained and lost nothing" \
    "ls -a $VENDOR_PAM_DIR | LC_ALL=C sort > /tmp/pam-vendor-dir.after && diff -u /tmp/pam-vendor-dir.before /tmp/pam-vendor-dir.after" \
    0

rm -f "$VENDOR_PAM_DIR/facelock-vendor-scratch" /tmp/pam-vendor.sha \
      /tmp/pam-vendor-dir.before /tmp/pam-vendor-dir.after \
      /tmp/pam-vendor-status.json /tmp/pam-vendor-add.json \
      /tmp/pam-vendor-add2.json /tmp/pam-vendor-remove.json \
      /tmp/pam-vendor-drift-remove.json /tmp/pam-vendor-remove2.json \
      /tmp/pam-nowhere.out

# The real thing, on the stock image: `sudo facelock setup --pam --service
# polkit-1` is the invocation omarchy#7040 runs under `set -e`, and it exited 1
# on every current Arch box. The guard is not a way out of the assertion — if
# the layout is not the one this row exists for, it says so loudly rather than
# passing quietly.
if [ -f /usr/lib/pam.d/polkit-1 ] && [ ! -e /etc/pam.d/polkit-1 ]; then
    sha256sum /usr/lib/pam.d/polkit-1 > /tmp/pam-polkit.sha
    run_test "polkit-1 ships in the vendor directory and setup --pam configures it" \
        "facelock setup --pam --service polkit-1 --yes > /tmp/pam-polkit.out 2>&1; test \$? -eq 0 && grep -qxF '$PAM_LINE_TEXT' /etc/pam.d/polkit-1 && python3 /tmp/pam-first-auth.py /etc/pam.d/polkit-1 && sha256sum -c --status /tmp/pam-polkit.sha" \
        0
    run_test "pam status now answers 0 for polkit-1" \
        "facelock pam status --service polkit-1" \
        0
    run_test "pam remove retires the unchanged polkit-1 override" \
        "facelock pam remove --service polkit-1 --json > /tmp/pam-polkit-remove.json 2>/dev/null; test \$? -eq 0 && python3 /tmp/pam-action.py /tmp/pam-polkit-remove.json | grep -qx removed && ! test -e /etc/pam.d/polkit-1 && sha256sum -c --status /tmp/pam-polkit.sha" \
        0
    rm -f /etc/pam.d/polkit-1 /etc/pam.d/polkit-1.facelock-backup \
          /tmp/pam-polkit.sha /tmp/pam-polkit.out /tmp/pam-polkit-remove.json
else
    # Not a skip. This is the only end-to-end row for the bug the whole gap
    # exists to fix, so an image that stops presenting the layout must cost a
    # red suite rather than quietly delete the coverage.
    echo "FAIL: polkit-1 is not vendor-only in this image \
(expected /usr/lib/pam.d/polkit-1 to exist and /etc/pam.d/polkit-1 not to) — \
the end-to-end vendor row did not run; fix the image or move the row to a \
service that is vendor-only"
    FAIL=$((FAIL + 1))
fi

# --- P3: `pam status --all` against the real directories ---
#
# The blind spot: `pam status` answers only about names it is given, so a
# configured polkit-1 or omarchy-lock-face was invisible, and "not configured"
# and "not checked" rendered identically. No tempdir test can prove the scan
# reaches the machine's real /etc/pam.d and /usr/lib/pam.d, and the unreadable
# case needs a directory a real process is really refused.
#
# The scan is run against a scratch pair through `[pam] config_dirs` for the
# rows that need to control what is in the directories, and against the real
# ones for the rows that must prove the defaults.

cat > /tmp/pam-all-services.py <<'EOF'
import json, sys
doc = json.load(open(sys.argv[1]))
print(" ".join(sorted(s["service"] for s in doc["services"])))
EOF

# One directory's `status` word from a --all document, by path.
cat > /tmp/pam-all-dir.py <<'EOF'
import json, sys
doc = json.load(open(sys.argv[1]))
for d in doc.get("directories", []):
    if d["path"] == sys.argv[2]:
        print(d["status"])
        break
EOF

# The `shadows` value of one service, or the empty string.
cat > /tmp/pam-all-shadows.py <<'EOF'
import json, sys
doc = json.load(open(sys.argv[1]))
for s in doc["services"]:
    if s["service"] == sys.argv[2]:
        print(s.get("shadows", ""))
        break
EOF

PAM_ALL_ROOT=/tmp/pam-all
rm -rf "$PAM_ALL_ROOT"
mkdir -p "$PAM_ALL_ROOT/etc" "$PAM_ALL_ROOT/vendor"
cat > "$PAM_ALL_ROOT/config.toml" <<EOF
[pam]
config_dirs = ["$PAM_ALL_ROOT/etc", "$PAM_ALL_ROOT/vendor"]
EOF
PAM_ALL="facelock --config $PAM_ALL_ROOT/config.toml pam status --all"

# Nothing configured: says so, and exits 1 — with and without --if-present,
# which has no absent case to forgive here.
cat > "$PAM_ALL_ROOT/etc/plain" <<'EOF'
#%PAM-1.0
auth       include        system-auth
EOF

run_test "pam status --all: nothing configured says so and exits 1" \
    "$PAM_ALL > /tmp/pam-all-none.out 2>&1; test \$? -eq 1 && grep -q 'carries the facelock PAM line' /tmp/pam-all-none.out" \
    0

run_test "pam status --all --if-present: still exit 1 when nothing is configured" \
    "$PAM_ALL --if-present > /dev/null 2>&1; test \$? -eq 1" \
    0

# Several services, from both directories, none of them named on the command
# line. This is the whole point of the flag.
cat > "$PAM_ALL_ROOT/etc/scratch-a" <<'EOF'
#%PAM-1.0
auth      sufficient pam_facelock.so
auth       include        system-auth
EOF
cat > "$PAM_ALL_ROOT/vendor/scratch-b" <<'EOF'
#%PAM-1.0
auth      sufficient pam_facelock.so
auth       include        system-auth
EOF
# A backup of a configured file carries the line and is not a service.
cp "$PAM_ALL_ROOT/etc/scratch-a" "$PAM_ALL_ROOT/etc/scratch-a.facelock-backup"
cp "$PAM_ALL_ROOT/etc/scratch-a" "$PAM_ALL_ROOT/etc/scratch-c.pacsave"

run_test "pam status --all lists every configured service and no backup" \
    "$PAM_ALL --json > /tmp/pam-all.json 2>/dev/null; test \$? -eq 0 && test \"\$(python3 /tmp/pam-all-services.py /tmp/pam-all.json)\" = 'scratch-a scratch-b'" \
    0

# An /etc copy shadowing a vendor file is configured *and* says so, in both
# renderings. The whole reason the note exists is that the copy will not
# follow the package's updates.
cat > "$PAM_ALL_ROOT/vendor/scratch-a" <<'EOF'
#%PAM-1.0
auth       include        system-auth
EOF

run_test "pam status --all marks an /etc override of a vendor file" \
    "$PAM_ALL --json > /tmp/pam-all-shadow.json 2>/dev/null && test \"\$(python3 /tmp/pam-all-shadows.py /tmp/pam-all-shadow.json scratch-a)\" = '$PAM_ALL_ROOT/vendor/scratch-a'" \
    0

run_test "pam status --all says 'local override' in the human output too" \
    "$PAM_ALL 2>/dev/null | grep -q 'local override of $PAM_ALL_ROOT/vendor/scratch-a'" \
    0

# The distinction the gap exists for: a directory that could not be read is
# reported as unchecked and forces exit 2, rather than reading as "nothing
# configured here". Root ignores the mode bits, so this runs as testuser —
# which also proves the probe is genuinely unprivileged.
chmod 755 "$PAM_ALL_ROOT" "$PAM_ALL_ROOT/etc" "$PAM_ALL_ROOT/vendor"
chmod 644 "$PAM_ALL_ROOT/config.toml" "$PAM_ALL_ROOT"/etc/* "$PAM_ALL_ROOT"/vendor/*
chmod 000 "$PAM_ALL_ROOT/vendor"

run_test "pam status --all: an unreadable directory is 'not checked', exit 2" \
    "su -s /bin/bash testuser -c '$PAM_ALL > /tmp/pam-all-unreadable.out 2>&1'; test \$? -eq 2 && grep -q 'directory not checked' /tmp/pam-all-unreadable.out" \
    0

run_test "pam status --all --json distinguishes scanned from unreadable" \
    "su -s /bin/bash testuser -c '$PAM_ALL --json > /tmp/pam-all-unreadable.json 2>/dev/null'; test \"\$(python3 /tmp/pam-all-dir.py /tmp/pam-all-unreadable.json $PAM_ALL_ROOT/etc)\" = scanned && test \"\$(python3 /tmp/pam-all-dir.py /tmp/pam-all-unreadable.json $PAM_ALL_ROOT/vendor)\" = unreadable" \
    0

# The state the human sentence used to get wrong: nothing configured AND a
# directory unread. Read 2>/dev/null -- the ordinary way to take the answer --
# an unqualified "no service file under <both dirs> carries the line" asserts
# the very thing this flag exists to stop it asserting. stdout alone is
# captured here on purpose.
mv "$PAM_ALL_ROOT/etc/scratch-a" "$PAM_ALL_ROOT/scratch-a.parked"
rm -f "$PAM_ALL_ROOT/etc/scratch-a.facelock-backup"

run_test "pam status --all: nothing found + a dir unread never reads as 'none'" \
    "su -s /bin/bash testuser -c '$PAM_ALL > /tmp/pam-all-partial.out 2>/dev/null'; test \$? -eq 2 && grep -q 'could not be checked' /tmp/pam-all-partial.out && ! grep -q \"carries the facelock PAM line.\$\" /tmp/pam-all-partial.out" \
    0

mv "$PAM_ALL_ROOT/scratch-a.parked" "$PAM_ALL_ROOT/etc/scratch-a"
chmod 644 "$PAM_ALL_ROOT/etc/scratch-a"
chmod 755 "$PAM_ALL_ROOT/vendor"

# A FIFO where a service file should be must not hang the scan. read_to_string
# on one blocks until a writer appears, which is forever here -- and the same
# scan backs `facelock status`, so the diagnostic command would hang on exactly
# the broken machine it exists to describe. The timeout IS the assertion.
mkfifo "$PAM_ALL_ROOT/etc/fifo-service"
ln -sfn "$PAM_ALL_ROOT/etc/fifo-service" "$PAM_ALL_ROOT/etc/linked-fifo"
ln -sfn "$PAM_ALL_ROOT/vendor" "$PAM_ALL_ROOT/etc/linked-dir"

run_test "pam status --all returns on a FIFO, a linked FIFO and a linked dir" \
    "timeout 15 $PAM_ALL --json > /tmp/pam-all-fifo.json 2>/dev/null; test \$? -eq 0 && test \"\$(python3 /tmp/pam-all-services.py /tmp/pam-all-fifo.json)\" = 'scratch-a scratch-b'" \
    0

rm -f "$PAM_ALL_ROOT/etc/fifo-service" "$PAM_ALL_ROOT/etc/linked-fifo" \
      "$PAM_ALL_ROOT/etc/linked-dir" /tmp/pam-all-fifo.json /tmp/pam-all-partial.out

# A directory that is not there is a different answer: it demonstrably holds no
# service files, so it is 'absent' and does not raise the exit code. Without
# this the default search path would make every machine with no /usr/lib/pam.d
# exit 2 forever.
rm -rf "$PAM_ALL_ROOT/vendor"

run_test "pam status --all: a missing directory is 'absent', not an error" \
    "$PAM_ALL --json > /tmp/pam-all-absent.json 2>/dev/null; test \$? -eq 0 && test \"\$(python3 /tmp/pam-all-dir.py /tmp/pam-all-absent.json $PAM_ALL_ROOT/vendor)\" = absent" \
    0

# And against the real directories, unprivileged, with the defaults: sudo is
# configured by the rows above having been cleaned up, so this asserts the
# scan reaches /etc/pam.d at all rather than a specific service.
cat > /etc/pam.d/facelock-all-scratch <<'EOF'
#%PAM-1.0
auth      sufficient pam_facelock.so
auth       include        system-auth
EOF
chmod 644 /etc/pam.d/facelock-all-scratch

run_test "pam status --all reaches the real /etc/pam.d without root" \
    "su -s /bin/bash testuser -c 'facelock pam status --all --json > /tmp/pam-all-real.json 2>/dev/null'; test \$? -eq 0 && python3 /tmp/pam-all-services.py /tmp/pam-all-real.json | grep -qw facelock-all-scratch" \
    0

rm -f /etc/pam.d/facelock-all-scratch /tmp/pam-all-services.py /tmp/pam-all-dir.py \
      /tmp/pam-all-shadows.py /tmp/pam-all.json /tmp/pam-all-shadow.json \
      /tmp/pam-all-none.out /tmp/pam-all-unreadable.out /tmp/pam-all-unreadable.json \
      /tmp/pam-all-absent.json /tmp/pam-all-real.json
rm -rf "$PAM_ALL_ROOT"

# --- P3: `facelock status` reports the same scan ---
#
# The report could only ever say something about /etc/pam.d/sudo, so a
# correctly wired omarchy-lock-face was invisible to it and "not configured"
# and "not checked" rendered identically. These rows prove the summary reaches
# the machine's real directories and keeps the two apart. The report needs
# root and probes a daemon, camera and models that are absent here; only the
# PAM lines are asserted.

cat > /etc/pam.d/facelock-status-scratch <<'EOF'
#%PAM-1.0
auth      sufficient pam_facelock.so
auth       include        system-auth
EOF
chmod 644 /etc/pam.d/facelock-status-scratch

run_test "facelock status names every configured PAM service" \
    "facelock status 2>&1 | grep -E '^    - PAM services:' | grep -q facelock-status-scratch" \
    0

rm -f /etc/pam.d/facelock-status-scratch

# A directory that cannot be listed must not read as "none configured". Root
# ignores mode bits, so the unreadable directory here is a regular file where a
# directory was configured (ENOTDIR) rather than a chmod 000 one.
PAM_STATUS_ROOT=/tmp/pam-status
rm -rf "$PAM_STATUS_ROOT"
mkdir -p "$PAM_STATUS_ROOT/etc"
touch "$PAM_STATUS_ROOT/notadir"
cat > "$PAM_STATUS_ROOT/config.toml" <<EOF
[pam]
config_dirs = ["$PAM_STATUS_ROOT/etc", "$PAM_STATUS_ROOT/notadir"]
EOF

run_test "facelock status says 'not checked', not 'none configured'" \
    "facelock --config $PAM_STATUS_ROOT/config.toml status > /tmp/pam-status.out 2>&1; grep -q '^    - PAM services: not checked$' /tmp/pam-status.out && grep -q '^    - not checked: $PAM_STATUS_ROOT/notadir ' /tmp/pam-status.out && ! grep -q 'none configured' /tmp/pam-status.out" \
    0

# ...and with every directory readable and nothing configured, it is the other
# line. The pair is the whole point: one string per answer.
rm -f "$PAM_STATUS_ROOT/notadir"
mkdir -p "$PAM_STATUS_ROOT/vendor"
cat > "$PAM_STATUS_ROOT/config.toml" <<EOF
[pam]
config_dirs = ["$PAM_STATUS_ROOT/etc", "$PAM_STATUS_ROOT/vendor"]
EOF

run_test "facelock status says 'none configured' when it could read everywhere" \
    "facelock --config $PAM_STATUS_ROOT/config.toml status 2>&1 | grep -q '^    - PAM services: none configured$'" \
    0

rm -rf "$PAM_STATUS_ROOT" /tmp/pam-status.out


rm -f /etc/pam.d/facelock-scratch /etc/pam.d/facelock-scratch2 \
      /etc/pam.d/facelock-scratch.facelock-backup \
      /etc/pam.d/facelock-scratch2.facelock-backup \
      /etc/pam.d/facelock-scratch-link /tmp/facelock-outside \
      /etc/pam.d/facelock-vendor-scratch \
      /etc/pam.d/facelock-vendor-scratch.facelock-backup \
      /usr/lib/pam.d/facelock-vendor-scratch \
      /tmp/pam-action.py /tmp/pam-first-auth.py
find /var/lib/facelock/pam-backups -maxdepth 1 \
    \( -name 'facelock-scratch.*' -o -name 'facelock-scratch2.*' \) -delete

# --- Spec 29: Smart PAM skip (no enrolled faces) ---

# In oneshot mode with no enrolled faces, facelock auth should exit 2 (PAM_IGNORE)
run_test "facelock auth exits 2 when no faces enrolled" \
    "facelock auth --user testuser --config /etc/facelock/config.toml; test \$? -eq 2" \
    0

# pamtester should pass through (PAM_IGNORE from face → pam_deny catches it)
# The key: it should be FAST (no camera timeout)
run_test "No enrolled faces: pamtester completes quickly" \
    "timeout 3 pamtester facelock-test testuser authenticate 2>&1; test \$? -ne 124" \
    0

# --- Spec 30: PAM conversation messages ---

# When notification.enabled = true (default), "Identifying face..." should appear
run_test "PAM shows 'Identifying face...' text" \
    "pamtester facelock-test testuser authenticate 2>&1 | grep -q 'Identifying face'" \
    0

# When notification mode = off, no text message
run_test "PAM respects notification mode=off" \
    "sed -i '/^\[notification\]/,/^\[/{s/.*mode.*/mode = \"off\"/}' /etc/facelock/config.toml 2>/dev/null || (echo -e '\n[notification]\nmode = \"off\"' >> /etc/facelock/config.toml); pamtester facelock-test testuser authenticate 2>&1 | grep -qv 'Identifying face'; sed -i '/mode = \"off\"/d' /etc/facelock/config.toml" \
    0

# --- Spec 29: Smart PAM with oneshot config ---

run_test "Oneshot mode: no enrolled faces returns quickly" \
    "sed -i '/^\[daemon\]/a mode = \"oneshot\"' /etc/facelock/config.toml; timeout 3 pamtester facelock-test testuser authenticate 2>&1; rc=\$?; sed -i '/^mode = \"oneshot\"/d' /etc/facelock/config.toml; test \$rc -ne 124" \
    0

# --- Plan 05: PAM trust-boundary hardening (all camera-free) ---

# (a) A group/world-writable config must be rejected: the module ignores it
# and fails closed (PAM_IGNORE -> pam_deny). The 'Identifying face' prompt
# only appears once the config is accepted, so its absence plus an auth
# failure proves the module rejected the file instead of trusting it.
run_test "Group-writable config rejected, fails closed" \
    "chmod 664 /etc/facelock/config.toml; pamtester facelock-test testuser authenticate < /dev/null > /tmp/gw-out 2>&1; rc=\$?; chmod 644 /etc/facelock/config.toml; test \$rc -ne 0 && ! grep -q 'Identifying face' /tmp/gw-out" \
    0

run_test "World-writable config rejected, fails closed" \
    "chmod 666 /etc/facelock/config.toml; pamtester facelock-test testuser authenticate < /dev/null > /tmp/ww-out 2>&1; rc=\$?; chmod 644 /etc/facelock/config.toml; test \$rc -ne 0 && ! grep -q 'Identifying face' /tmp/ww-out" \
    0

run_test "Config accepted again after restoring 644" \
    "pamtester facelock-test testuser authenticate 2>&1 | grep -q 'Identifying face'" \
    0

# (b) env_clear: LD_PRELOAD must never reach the spawned oneshot child while
# SSH_CONNECTION must survive. A constructor-marker .so logs every process it
# is loaded into; a root-owned capture stub stands in for the auth binary so
# the exact child environment can be asserted.
cat > /tmp/preload-marker.c <<'EOF'
#define _GNU_SOURCE
#include <stdio.h>
#include <unistd.h>
__attribute__((constructor)) static void mark(void) {
    char exe[512] = {0};
    ssize_t n = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
    FILE *f = fopen("/tmp/preload-log", "a");
    if (f) { fprintf(f, "%s\n", n > 0 ? exe : "?"); fclose(f); }
}
EOF
gcc -shared -fPIC -o /tmp/preload-marker.so /tmp/preload-marker.c
printf '#!/bin/bash\nenv > /tmp/oneshot-child-env\nexit 2\n' > /usr/local/bin/facelock-env-capture
chmod 755 /usr/local/bin/facelock-env-capture
rm -f /tmp/preload-log /tmp/oneshot-child-env

# Intercept the oneshot spawn by BEING /usr/bin/facelock for the duration
# rather than pointing an auth_bin config key at the stub: the PAM module
# spawns the oneshot binary by that fixed path (post-#109 the key does not
# exist; pre-#109 its default is the same path and nothing here sets it), so
# the swap works on both sides of that change and the assertions can never
# pass vacuously through an ignored redirect.
sed -i '/^\[daemon\]/a mode = "oneshot"' /etc/facelock/config.toml
sed -i '/^\[security\]/a abort_if_ssh = false' /etc/facelock/config.toml
mv /usr/bin/facelock /usr/bin/facelock.orig
install -m 755 /usr/local/bin/facelock-env-capture /usr/bin/facelock
env LD_PRELOAD=/tmp/preload-marker.so SSH_CONNECTION='192.0.2.1 1111 192.0.2.2 22' \
    pamtester facelock-test testuser authenticate < /dev/null > /dev/null 2>&1 || true
mv -f /usr/bin/facelock.orig /usr/bin/facelock
sed -i '/^mode = "oneshot"/d;/^abort_if_ssh = false/d' /etc/facelock/config.toml

run_test "env_clear: marker was active in the PAM process" \
    "grep -q pamtester /tmp/preload-log" \
    0

run_test "env_clear: LD_PRELOAD marker not loaded by oneshot child" \
    "test -f /tmp/oneshot-child-env && ! grep -q '^LD_PRELOAD=' /tmp/oneshot-child-env && ! grep -q bash /tmp/preload-log" \
    0

run_test "env_clear: SSH_CONNECTION survives to oneshot child" \
    "grep -q '^SSH_CONNECTION=192.0.2.1' /tmp/oneshot-child-env" \
    0

run_test "env_clear: oneshot child PATH pinned to /usr/bin:/bin" \
    "grep -qx 'PATH=/usr/bin:/bin' /tmp/oneshot-child-env" \
    0

# (c) Bus policy (ADR 010): the default context may call Authenticate and
# nothing else. There is no group policy — signals are root-only. A fake
# daemon owned by ROOT stands in for the real one — the real daemon needs a
# camera to start, and the bus enforces the policy regardless of who answers.
# `outsider` is a plain account: not root, with no group that grants it
# anything. The daemon-side check that a caller may only name its own username
# has its own unit tests (facelock-daemon server.rs) and runs live in the
# integration tier.
useradd -m outsider 2>/dev/null || true
mkdir -p /run/dbus
dbus-uuidgen --ensure=/etc/machine-id > /dev/null 2>&1 || true
dbus-daemon --system --fork --nopidfile

wait_for_daemon_name() {
    for _ in $(seq 1 40); do
        dbus-send --system --print-reply --dest=org.freedesktop.DBus \
            /org/freedesktop/DBus org.freedesktop.DBus.NameHasOwner \
            string:org.facelock.Daemon 2>/dev/null | grep -q 'boolean true' && return 0
        sleep 0.25
    done
    return 1
}

python3 /fake-facelock-daemon.py > /tmp/fake-daemon-root.log 2>&1 &
FAKE_ROOT_PID=$!
wait_for_daemon_name || echo "warning: root fake daemon did not claim the name"

run_test "bus policy: a plain local user may call Authenticate" \
    "runuser -u outsider -- dbus-send --system --print-reply --dest=org.facelock.Daemon /org/facelock/Daemon org.facelock.Daemon.Authenticate string:outsider | grep -q 'boolean true'" \
    0

# PAM addresses the daemon by its unique name (GetNameOwner), so the policy's
# send_destination must match the owner, not just the well-known name.
FAKE_OWNER=$(dbus-send --system --print-reply --dest=org.freedesktop.DBus \
    /org/freedesktop/DBus org.freedesktop.DBus.GetNameOwner \
    string:org.facelock.Daemon 2>/dev/null | awk '/string/ {gsub(/"/, "", $2); print $2}' || true)
run_test "bus policy: a plain local user may call Authenticate on the daemon's unique name" \
    "[ -n '$FAKE_OWNER' ] && runuser -u outsider -- dbus-send --system --print-reply --dest=$FAKE_OWNER /org/facelock/Daemon org.facelock.Daemon.Authenticate string:outsider | grep -q 'boolean true'" \
    0
run_test "bus policy: a plain local user cannot call Ping on the unique name either" \
    "[ -n '$FAKE_OWNER' ] && runuser -u outsider -- dbus-send --system --print-reply --dest=$FAKE_OWNER /org/facelock/Daemon org.facelock.Daemon.Ping 2>&1 | grep -q AccessDenied" \
    0

run_test "bus policy: a plain local user cannot call Ping" \
    "runuser -u outsider -- dbus-send --system --print-reply --dest=org.facelock.Daemon /org/facelock/Daemon org.facelock.Daemon.Ping 2>&1 | grep -q AccessDenied" \
    0

run_test "bus policy: a plain local user cannot call ListModels" \
    "runuser -u outsider -- dbus-send --system --print-reply --dest=org.facelock.Daemon /org/facelock/Daemon org.facelock.Daemon.ListModels 2>&1 | grep -q AccessDenied" \
    0

# The whole user-run PAM path under the real policy: pam_facelock as a plain
# local user → verify_daemon_peer (owner is root) → Authenticate on the
# daemon's unique name → the (fake, root-owned) daemon answers matched=true →
# PAM_SUCCESS. Before ADR 010 the bus denied this send.
run_test "bus policy: plain-local-user pamtester succeeds through pam_facelock against the root fake daemon" \
    "timeout 15 runuser -u outsider -- pamtester facelock-test outsider authenticate < /dev/null" \
    0

kill "$FAKE_ROOT_PID" 2>/dev/null || true
wait "$FAKE_ROOT_PID" 2>/dev/null || true

# (d) Peer-UID check: a non-root process owning org.facelock.Daemon and
# replying matched=true must never produce PAM_SUCCESS. A deliberately
# loosened bus policy simulates a broken/compromised policy file. The bus is
# already running, so ask it to re-read the policy directory.
cat > /usr/share/dbus-1/system.d/zz-facelock-peer-test.conf <<'EOF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <policy user="testuser">
    <allow own="org.facelock.Daemon"/>
  </policy>
  <policy context="default">
    <allow send_destination="org.facelock.Daemon"/>
  </policy>
</busconfig>
EOF
dbus-send --system --print-reply --type=method_call --dest=org.freedesktop.DBus \
    /org/freedesktop/DBus org.freedesktop.DBus.ReloadConfig >/dev/null
runuser -u testuser -- python3 /fake-facelock-daemon.py > /tmp/fake-daemon.log 2>&1 &
FAKE_PID=$!
wait_for_daemon_name || echo "warning: fake non-root daemon did not claim the name"

run_test "Peer-UID harness: fake non-root daemon replies matched=true" \
    "dbus-send --system --print-reply --dest=org.facelock.Daemon /org/facelock/Daemon org.facelock.Daemon.Authenticate string:testuser | grep -q 'boolean true'" \
    0

run_test "Peer-UID: non-root daemon owner yields no PAM_SUCCESS" \
    "! timeout 15 pamtester facelock-test testuser authenticate < /dev/null" \
    0

kill "$FAKE_PID" 2>/dev/null || true
rm -f /usr/share/dbus-1/system.d/zz-facelock-peer-test.conf

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
