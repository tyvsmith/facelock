#!/usr/bin/env bash
# Model-free, booted Fedora lifecycle proof for authselect profile retirement.
set -euo pipefail

OLD_RPM=/artifacts/facelock-old.rpm
NEW_RPM=/artifacts/facelock-new.rpm
PASSWORD_USER=facelock-password-user
PASSWORD_SERVICE=facelock-password-test
CORRECT_PASSWORD=correct-password
case_name=

log() {
    printf '\n=== %s ===\n' "$*"
}

fail() {
    echo "FAIL [$case_name]: $*" >&2
    exit 1
}

assert_eq() {
    local expected="$1" actual="$2" label="$3"
    [ "$actual" = "$expected" ] ||
        fail "$label: expected '$expected', got '$actual'"
}

assert_installed_version() {
    assert_eq "$1" "$(rpm -q --qf '%{VERSION}' facelock)" \
        "installed facelock version"
}

current_raw() {
    /usr/bin/authselect current -r
}

authselect_hashes() {
    sha256sum /etc/authselect/authselect.conf /etc/authselect/system-auth \
        /etc/authselect/password-auth | awk '{print $1}' | paste -sd: -
}

snapshot_entry() {
    local path="$1"
    if [ ! -e "$path" ] && [ ! -L "$path" ]; then
        printf 'absent|%s\n' "$path"
        return
    fi

    LC_ALL=C stat -c 'entry|%n|%F|%u|%g|%a|%h|%s|%d|%i' -- "$path"
    if [ -L "$path" ]; then
        printf 'link|%s|' "$path"
        readlink -- "$path"
        if [ -f "$path" ]; then
            LC_ALL=C stat -Lc 'target|%n|%F|%u|%g|%a|%h|%s|%d|%i' -- "$path"
            sha256sum -- "$path"
        fi
    elif [ -f "$path" ]; then
        sha256sum -- "$path"
    fi
}

snapshot_rejected_upgrade_state() {
    local entry profile_root=/usr/share/authselect/vendor/facelock
    for entry in \
        /etc/authselect/authselect.conf \
        /etc/authselect/system-auth \
        /etc/authselect/password-auth \
        /etc/authselect/postlogin \
        /etc/pam.d/system-auth \
        /etc/pam.d/password-auth; do
        snapshot_entry "$entry"
    done

    if [ ! -e "$profile_root" ] && [ ! -L "$profile_root" ]; then
        printf 'absent|%s\n' "$profile_root"
        return
    fi
    while IFS= read -r -d '' entry; do
        snapshot_entry "$entry"
    done < <(find -P "$profile_root" -xdev -print0 | sort -z)
}

assert_password_fallback() {
    printf '%s\n' "$CORRECT_PASSWORD" |
        pamtester "$PASSWORD_SERVICE" "$PASSWORD_USER" authenticate >/dev/null 2>&1 ||
        fail "real pam_unix password authentication failed"
    if printf '%s\n' wrong-password |
        pamtester "$PASSWORD_SERVICE" "$PASSWORD_USER" authenticate >/dev/null 2>&1; then
        fail "wrong password unexpectedly authenticated"
    fi
}

assert_authselect_healthy() {
    /usr/bin/authselect check >/dev/null || fail "authselect check failed"
    [ -L /etc/pam.d/system-auth ] || fail "system-auth is not an authselect link"
    [ -L /etc/pam.d/password-auth ] || fail "password-auth is not an authselect link"
    assert_password_fallback
}

select_sssd() {
    /usr/bin/authselect select sssd --force >/dev/null
    assert_eq sssd "$(current_raw)" "baseline authselect selection"
    assert_authselect_healthy
}

install_old() {
    dnf -y install "$OLD_RPM" >/dev/null
    assert_installed_version 0.1.4
}

install_new() {
    dnf -y install "$NEW_RPM" >/dev/null
    assert_installed_version 0.2.0
}

upgrade_new() {
    dnf -y upgrade "$NEW_RPM" >/dev/null
    assert_installed_version 0.2.0
}

remove_facelock() {
    if rpm -q facelock >/dev/null 2>&1; then
        rpm -e facelock >/dev/null
    fi
}

assert_new_payload_has_no_authselect() {
    if rpm -ql facelock | grep -Fq '/authselect/'; then
        fail "new RPM installed an authselect path"
    fi
    if rpm -q --requires facelock | grep -Eq '(^|[[:space:]])authselect([[:space:]]|$)'; then
        fail "new RPM retained an authselect dependency"
    fi
}

expect_upgrade_rejected() {
    local label="$1"
    local log_file="/tmp/facelock-${label}-upgrade.log"
    local before="/tmp/facelock-${label}-state.before"
    local after="/tmp/facelock-${label}-state.after"
    snapshot_rejected_upgrade_state >"$before"
    if dnf -y upgrade "$NEW_RPM" >"$log_file" 2>&1; then
        fail "unsafe $label state unexpectedly upgraded"
    fi
    snapshot_rejected_upgrade_state >"$after"
    if ! cmp -s "$before" "$after"; then
        diff -u "$before" "$after" >&2 || true
        fail "rejected $label upgrade mutated authselect state or legacy profile identity"
    fi
    assert_installed_version 0.1.4
    [ -d /usr/share/authselect/vendor/facelock ] ||
        fail "rejected upgrade removed the legacy profile"
    grep -Fq "Facelock no longer ships a system-wide authselect profile" "$log_file" ||
        fail "rejected upgrade omitted retirement guidance"
    grep -Fq "Inspect the current identity provider" "$log_file" ||
        fail "rejected upgrade omitted provider guidance"
    grep -Fq "with an authselect backup" "$log_file" ||
        fail "rejected upgrade omitted backup guidance"
    grep -Fq "retry this package transaction" "$log_file" ||
        fail "rejected upgrade omitted retry guidance"
}

cat >/etc/pam.d/$PASSWORD_SERVICE <<'EOF'
auth include system-auth
account required pam_permit.so
EOF

case_name=fresh-install
log "$case_name"
select_sssd
fresh_raw=$(current_raw)
fresh_hashes=$(authselect_hashes)
install_new
assert_new_payload_has_no_authselect
assert_eq "$fresh_raw" "$(current_raw)" "fresh install selection"
assert_eq "$fresh_hashes" "$(authselect_hashes)" "fresh install generated bytes"
assert_authselect_healthy
remove_facelock

case_name=unselected-upgrade
log "$case_name"
install_old
select_sssd
unselected_raw=$(current_raw)
unselected_hashes=$(authselect_hashes)
upgrade_new
assert_new_payload_has_no_authselect
[ ! -e /usr/share/authselect/vendor/facelock ] ||
    fail "upgrade retained the old vendor profile"
assert_eq "$unselected_raw" "$(current_raw)" "unselected upgrade selection"
assert_eq "$unselected_hashes" "$(authselect_hashes)" "unselected upgrade generated bytes"
assert_authselect_healthy
remove_facelock

case_name=selected-exact-upgrade
log "$case_name"
install_old
/usr/bin/authselect select facelock with-facelock --force >/dev/null
assert_eq "facelock with-facelock" "$(current_raw)" "legacy selection"
assert_eq facelock "$(head -n 1 /etc/authselect/authselect.conf)" \
    "legacy selection-state first line"
expect_upgrade_rejected selected-exact
grep -Fq "the retired Facelock authselect profile is still selected" \
    /tmp/facelock-selected-exact-upgrade.log ||
    fail "selected exact-profile rejection omitted the token-specific reason"
/usr/bin/authselect select sssd --force -b >/dev/null
upgrade_new
assert_new_payload_has_no_authselect
assert_eq sssd "$(current_raw)" "selection after explicit remediation"
assert_authselect_healthy
remove_facelock

case_name=custom-facelock-upgrade
log "$case_name"
select_sssd
install_old
if [ ! -d /etc/authselect/custom/facelock ]; then
    /usr/bin/authselect create-profile facelock -b sssd >/dev/null
fi
/usr/bin/authselect select custom/facelock --force >/dev/null
custom_raw=$(current_raw)
custom_hashes=$(authselect_hashes)
upgrade_new
assert_new_payload_has_no_authselect
assert_eq "$custom_raw" "$(current_raw)" "custom profile selection"
assert_eq "$custom_hashes" "$(authselect_hashes)" "custom profile bytes"
assert_authselect_healthy
remove_facelock

case_name=untrusted-state-matrix
log "$case_name"
select_sssd
install_old
state=/etc/authselect/authselect.conf
cp -a "$state" /tmp/facelock-authselect.conf

chmod 0600 "$state"
expect_upgrade_rejected wrong-mode
chmod 0644 "$state"

chown "$PASSWORD_USER:$PASSWORD_USER" "$state"
expect_upgrade_rejected wrong-owner
chown root:root "$state"

ln "$state" /etc/authselect/.facelock-authselect-hardlink
expect_upgrade_rejected hard-link
rm -f /etc/authselect/.facelock-authselect-hardlink

mv "$state" /tmp/facelock-authselect-real
ln -s /tmp/facelock-authselect-real "$state"
expect_upgrade_rejected symlink
rm -f "$state"
mv /tmp/facelock-authselect-real "$state"

printf '%s\n' 'facelock/extra' >"$state"
expect_upgrade_rejected invalid-token
cp -a /tmp/facelock-authselect.conf "$state"

printf 'facelock\0x\n' >"$state"
expect_upgrade_rejected nul-byte
cp -a /tmp/facelock-authselect.conf "$state"

dd if=/dev/zero bs=16385 count=1 status=none >>"$state"
expect_upgrade_rejected oversized
cp -a /tmp/facelock-authselect.conf "$state"

upgrade_new
assert_eq sssd "$(current_raw)" "selection after repaired state"
assert_authselect_healthy
remove_facelock

case_name=absent-state-without-authselect-dependency
log "$case_name"
install_old
rpm -e --nodeps authselect
rm -rf /etc/authselect
upgrade_new
assert_new_payload_has_no_authselect
if rpm -q authselect >/dev/null 2>&1; then
    fail "upgrade reinstalled authselect"
fi
remove_facelock

echo "RPM authselect retirement lifecycle passed"
