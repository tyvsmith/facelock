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

tmpfiles_create_count="$(grep -Ec '^[[:space:]]*systemd-tmpfiles[[:space:]].*--create' \
    "$control_root/postinst")"
[ "$tmpfiles_create_count" -eq 1 ] ||
    fail "generated postinst must contain only dh_installtmpfiles' package-scoped create"
grep -Eq '^[[:space:]]*systemd-tmpfiles[[:space:]].*--create[[:space:]]+facelock\.conf([[:space:]]|$)' \
    "$control_root/postinst" ||
    fail "generated postinst must activate only facelock.conf"

if grep -Eq 'deb-systemd-invoke[[:space:]]+(start|restart)' "$control_root/postinst" ||
   grep -Eq 'systemctl[[:space:]]+([^[:space:]]+[[:space:]]+)*(enable|start)([[:space:]]|$)' \
       "$control_root/postinst"; then
    fail "postinst starts or unconditionally enables facelock-daemon before explicit activation"
fi

active_line="$(grep -n -m1 -Fx '           systemctl is-active --quiet facelock-daemon.service; then' \
    "$control_root/postinst" | cut -d: -f1 || true)"
restart_line="$(grep -n -m1 -Fx '            systemctl try-restart facelock-daemon.service 2>/dev/null || true' \
    "$control_root/postinst" | cut -d: -f1 || true)"
[ "$(grep -Fxc '           systemctl is-active --quiet facelock-daemon.service; then' \
    "$control_root/postinst")" -eq 1 ] &&
    [ "$(grep -Fxc '            systemctl try-restart facelock-daemon.service 2>/dev/null || true' \
        "$control_root/postinst")" -eq 1 ] ||
    fail "postinst must contain one exact active-only restart guard"
[ -n "$active_line" ] && [ -n "$restart_line" ] &&
    [ "$restart_line" -eq "$((active_line + 1))" ] ||
    fail "postinst must guard the single daemon try-restart with an adjacent is-active check"
[ "$(grep -Ec 'systemctl[[:space:]].*restart' "$control_root/postinst")" -eq 1 ] ||
    fail "postinst must contain exactly one guarded systemctl restart"

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

[ -f "$control_root/prerm" ] || fail "package has no generated prerm"
[ -f "$control_root/postrm" ] || fail "package has no generated postrm"
if grep -Eq 'systemctl[[:space:]]+(stop|disable)[[:space:]]+facelock-daemon' \
    "$control_root/prerm"; then
    fail "prerm must not manually stop or disable facelock-daemon"
fi
[ "$(grep -Ec "^[[:space:]]*deb-systemd-invoke stop 'facelock-daemon\.service' >/dev/null \|\| true$" \
    "$control_root/prerm")" -eq 1 ] ||
    fail "generated prerm must contain one debhelper-owned service stop"
profile_probe_line="$(grep -n -m1 -F 'facelock pam shared-profile-status' \
    "$control_root/prerm" | cut -d: -f1 || true)"
cleanup_preflight_line="$(grep -n -m1 -F 'facelock pam remove --all --dry-run' \
    "$control_root/prerm" | cut -d: -f1 || true)"
cleanup_line="$(grep -n -F 'facelock pam remove --all' "$control_root/prerm" |
    tail -n1 | cut -d: -f1 || true)"
generated_stop_line="$(grep -n -m1 -F "deb-systemd-invoke stop 'facelock-daemon.service'" \
    "$control_root/prerm" | cut -d: -f1 || true)"
[ -n "$profile_probe_line" ] && [ -n "$cleanup_preflight_line" ] &&
    [ -n "$cleanup_line" ] && [ -n "$generated_stop_line" ] &&
    [ "$profile_probe_line" -lt "$cleanup_preflight_line" ] &&
    [ "$cleanup_preflight_line" -lt "$cleanup_line" ] &&
    [ "$cleanup_line" -lt "$generated_stop_line" ] ||
    fail "generated prerm must clean PAM before debhelper stops the service"
if grep -Fq "deb-systemd-helper purge 'facelock-daemon.service'" "$control_root/prerm"; then
    fail "prerm must not purge enabled state during ordinary removal"
fi
# Match the generated script's literal positional parameter.
# shellcheck disable=SC2016
if ! grep -Fq 'if [ "$1" = "purge" ]; then' "$control_root/postrm" ||
   ! grep -Fq "deb-systemd-helper purge 'facelock-daemon.service'" "$control_root/postrm"; then
    fail "generated postrm must reserve service-state purge for package purge"
fi

echo "deb maintscript contract: ok"
