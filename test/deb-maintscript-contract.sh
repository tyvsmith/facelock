#!/usr/bin/env bash
set -euo pipefail

package="${1:?usage: deb-maintscript-contract.sh <package.deb>}"

fail() {
    echo "deb maintscript contract: $*" >&2
    exit 1
}

[ -f "$package" ] || fail "package does not exist: $package"
control_root="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-control.XXXXXX")"
trap 'rm -rf -- "$control_root"' EXIT
dpkg-deb --control "$package" "$control_root"

[ -f "$control_root/postinst" ] || fail "package has no generated postinst"

if grep -Eq \
    'deb-systemd-invoke[[:space:]]+(start|restart)|systemctl[[:space:]].*(enable|start)' \
    "$control_root/postinst"; then
    fail "postinst starts or unconditionally enables facelock-daemon before explicit activation"
fi

# With --no-enable, debhelper still preserves an existing enabled state during
# upgrades. Permit only that generated debian-installed + was-enabled path;
# fresh installs remain disabled and are covered by the booted package test.
installed_guard_seen=false
upgrade_enable_pending=false
while IFS= read -r line || [ -n "$line" ]; do
    trimmed="${line#"${line%%[![:space:]]*}"}"
    case "$trimmed" in
        ''|'#'*)
            continue
            ;;
        "if deb-systemd-helper debian-installed 'facelock-daemon.service'; then")
            installed_guard_seen=true
            upgrade_enable_pending=false
            ;;
        "if deb-systemd-helper --quiet was-enabled 'facelock-daemon.service'; then")
            "$installed_guard_seen" ||
                fail "postinst has an unscoped was-enabled service guard"
            upgrade_enable_pending=true
            ;;
        "deb-systemd-helper enable 'facelock-daemon.service' >/dev/null || true")
            "$upgrade_enable_pending" ||
                fail "postinst enables facelock-daemon outside the guarded upgrade path"
            upgrade_enable_pending=false
            ;;
        *'deb-systemd-helper enable'*)
            fail "postinst contains an unexpected service-enable command"
            ;;
        *)
            upgrade_enable_pending=false
            ;;
    esac
done <"$control_root/postinst"

echo "deb maintscript contract: ok"
