#!/usr/bin/env bash
# Booted Fedora proof that Facelock edits only explicitly selected leaf PAM
# services and never rewrites authselect's shared policy.
set -euo pipefail

local_service=facelock-rpm-leaf
vendor_service=facelock-rpm-vendor-leaf
symlink_service=facelock-rpm-symlink
local_path="/etc/pam.d/$local_service"
vendor_path="/usr/lib/pam.d/$vendor_service"
vendor_override="/etc/pam.d/$vendor_service"
symlink_path="/etc/pam.d/$symlink_service"
snapshot_dir="$(mktemp -d /tmp/facelock-rpm-pam.XXXXXX)"
outside_path="$snapshot_dir/outside-pam"
pass_count=0

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

pass() {
    printf 'TEST: %s ... PASS\n' "$1"
    pass_count=$((pass_count + 1))
}

snapshot_authselect() {
    local entry
    /usr/bin/authselect current -r
    for entry in \
        /etc/authselect/authselect.conf \
        /etc/authselect/system-auth \
        /etc/authselect/password-auth \
        /etc/authselect/postlogin; do
        stat -c '%n|%F|%u|%g|%a|%h|%s' -- "$entry"
        sha256sum -- "$entry"
    done
}

snapshot_pam_root() {
    local path basename
    while IFS= read -r -d '' path; do
        basename="${path##*/}"
        case "$basename" in
            "$local_service"|"$vendor_service"|"$symlink_service") continue ;;
        esac
        stat -c '%n|%F|%u|%g|%a|%h|%s' -- "$path"
        if [ -L "$path" ]; then
            readlink -- "$path"
        elif [ -f "$path" ]; then
            sha256sum -- "$path"
        fi
    done < <(find /etc/pam.d -mindepth 1 -maxdepth 1 -print0 | sort -z)
}

snapshot_file() {
    local path="$1"
    stat -c '%F|%u|%g|%a|%h|%s' -- "$path"
    sha256sum -- "$path" | awk '{print $1}'
}

authselect_is_unchanged() {
    snapshot_authselect /dev/stdout | cmp -s - "$snapshot_dir/authselect.before"
}

cleanup() {
    /usr/bin/facelock pam remove --service "$local_service" --if-present --no-confirm \
        >/dev/null 2>&1 || true
    /usr/bin/facelock pam remove --service "$vendor_service" --if-present --no-confirm \
        >/dev/null 2>&1 || true
    rm -f -- "$local_path" "$vendor_override" "$vendor_path" "$symlink_path" "$outside_path"
    rm -rf -- "$snapshot_dir"
}
trap cleanup EXIT

for required in /usr/bin/facelock /usr/bin/authselect /usr/bin/pamtester; do
    [ -x "$required" ] || fail "missing required executable: $required"
done
[ ! -e "$local_path" ] && [ ! -L "$local_path" ] || fail "$local_path already exists"
[ ! -e "$vendor_path" ] && [ ! -L "$vendor_path" ] || fail "$vendor_path already exists"
[ ! -e "$vendor_override" ] && [ ! -L "$vendor_override" ] || \
    fail "$vendor_override already exists"
[ ! -e "$symlink_path" ] && [ ! -L "$symlink_path" ] || fail "$symlink_path already exists"

snapshot_authselect >"$snapshot_dir/authselect.before"
snapshot_pam_root >"$snapshot_dir/pam-root.before"

printf '%s\n' \
    '#%PAM-1.0' \
    'auth      required pam_unix.so' \
    'account   required pam_permit.so' \
    >"$local_path"
chmod 0644 "$local_path"
snapshot_file "$local_path" >"$snapshot_dir/local.before"

/usr/bin/facelock pam add --service "$local_service" --no-confirm >/dev/null
grep -qxF 'auth      sufficient pam_facelock.so' "$local_path" || \
    fail "local leaf is missing the Facelock rule"
grep -qxF 'auth      required pam_unix.so' "$local_path" || \
    fail "local leaf lost its password fallback"
pass "service-scoped PAM setup succeeds on an RPM install"

authselect_is_unchanged || fail "service-scoped setup changed authselect state"
pass "service-scoped PAM setup leaves authselect selection unchanged"
pass "service-scoped PAM setup leaves shared authselect PAM files unchanged"
snapshot_pam_root | cmp -s - "$snapshot_dir/pam-root.before" || \
    fail "service-scoped setup changed an unrelated PAM service"
pass "service-scoped PAM setup edits only the requested leaf service"

# The PAM module asks the system daemon first. Give that daemon the same
# container-only camera path used by the general package validator so D-Bus
# activation does not fail early on the deliberately camera-less fixture.
if ! grep -q '^path\s*=' /etc/facelock/config.toml; then
    sed -i '/^\[device\]/a path = "/dev/video0"' /etc/facelock/config.toml
fi

printf '%s\n' test |
    timeout --foreground 30 pamtester "$local_service" testuser authenticate \
        >/dev/null 2>&1 || \
    fail "correct password did not fall through after Facelock rejection"
if printf '%s\n' wrong-password |
    timeout --foreground 30 pamtester "$local_service" testuser authenticate \
        >/dev/null 2>&1; then
    fail "wrong password authenticated through the configured leaf"
fi
pass "correct password falls through after Facelock rejection"

# Leave the following package validator an independent daemon-start boundary.
systemctl stop facelock-daemon.service >/dev/null 2>&1 || true
systemctl reset-failed facelock-daemon.service >/dev/null 2>&1 || true

/usr/bin/facelock pam remove --service "$local_service" --no-confirm >/dev/null
snapshot_file "$local_path" | cmp -s - "$snapshot_dir/local.before" || \
    fail "local leaf was not restored byte-for-byte with its metadata"
authselect_is_unchanged || fail "local leaf removal changed authselect state"
pass "service-scoped PAM removal restores the requested leaf"
printf '%s\n' test |
    timeout --foreground 30 pamtester "$local_service" testuser authenticate \
        >/dev/null 2>&1 || \
    fail "correct password failed after service-scoped PAM removal"
if printf '%s\n' wrong-password |
    timeout --foreground 30 pamtester "$local_service" testuser authenticate \
        >/dev/null 2>&1; then
    fail "wrong password authenticated after service-scoped PAM removal"
fi
pass "service-scoped PAM removal preserves password success and rejection"

install -d -m0755 /usr/lib/pam.d
printf '%s\n' \
    '#%PAM-1.0' \
    'auth      required pam_unix.so' \
    'account   required pam_permit.so' \
    >"$vendor_path"
chmod 0644 "$vendor_path"
snapshot_file "$vendor_path" >"$snapshot_dir/vendor.before"

/usr/bin/facelock pam add --service "$vendor_service" --no-confirm >/dev/null
[ -f "$vendor_override" ] && [ ! -L "$vendor_override" ] || \
    fail "vendor-only setup did not create a regular local override"
grep -qxF 'auth      sufficient pam_facelock.so' "$vendor_override" || \
    fail "vendor-only local override is missing the Facelock rule"
snapshot_file "$vendor_path" | cmp -s - "$snapshot_dir/vendor.before" || \
    fail "vendor-only setup changed the vendor service"
authselect_is_unchanged || fail "vendor-only setup changed authselect state"
pass "vendor-only leaf setup leaves the vendor service unchanged"

/usr/bin/facelock pam remove --service "$vendor_service" --no-confirm >/dev/null
[ ! -e "$vendor_override" ] && [ ! -L "$vendor_override" ] || \
    fail "vendor-only removal retained the unchanged Facelock-created override"
snapshot_file "$vendor_path" | cmp -s - "$snapshot_dir/vendor.before" || \
    fail "vendor-only removal changed the vendor service"
authselect_is_unchanged || fail "vendor-only removal changed authselect state"
pass "vendor-only leaf removal retires the unchanged Facelock override"

printf '%s\n' '# administrator-owned PAM sentinel' >"$outside_path"
outside_hash="$(sha256sum "$outside_path" | awk '{print $1}')"
ln -s "$outside_path" "$symlink_path"
if /usr/bin/facelock pam add --service "$symlink_service" --no-confirm \
    >/dev/null 2>&1; then
    fail "outbound PAM service symlink was accepted"
fi
[ "$(sha256sum "$outside_path" | awk '{print $1}')" = "$outside_hash" ] || \
    fail "outbound symlink target was changed"
authselect_is_unchanged || fail "symlink refusal changed authselect state"
pass "outbound PAM service symlink is refused"

printf '\n=== RPM service-scoped PAM results: %d passed, 0 failed ===\n' "$pass_count"
