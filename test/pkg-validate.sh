#!/bin/bash
set -euo pipefail

PASS=0
FAIL=0
SKIP=0

# Record an assertion (or a whole block of them) that did not run.
#
# Counting these is the point. A summary that can only say "N passed, 0 failed"
# reads identically whether every assertion ran or a third of them were stepped
# over — which is exactly how the runtime CAP_CHOWN thread walk went missing
# from a green "39 passed, 0 failed" run while the same command with models
# present reported 42. A run that skips must not be able to look like a run
# that checked.
skip_test() {
    local name="$1"
    local reason="$2"

    echo "SKIP: $name ($reason)"
    SKIP=$((SKIP + 1))
}

run_test() {
    local name="$1"
    local cmd="$2"
    local expected_result="${3:-0}"

    echo -n "TEST: $name ... "
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

run_warn_check() {
    local name="$1"
    local cmd="$2"

    echo -n "WARN: $name ... "
    if bash -c "$cmd" > /tmp/test-output 2>&1; then
        echo "present"
    else
        echo "missing"
    fi
}

PAM_MODULE_PATH=""
for candidate in /usr/lib/security/pam_facelock.so /usr/lib64/security/pam_facelock.so /lib/security/pam_facelock.so; do
    if [ -f "$candidate" ]; then
        PAM_MODULE_PATH="$candidate"
        break
    fi
done
export PAM_MODULE_PATH

DBUS_POLICY_FILE="/usr/share/dbus-1/system.d/org.facelock.Daemon.conf"
export DBUS_POLICY_FILE

PACKAGE_FORMAT=unpackaged
if command -v dpkg-query >/dev/null 2>&1; then
    DPKG_FACELOCK_STATUS="$(dpkg-query -W -f='${db:Status-Abbrev}' facelock 2>/dev/null || true)"
    if [ "$DPKG_FACELOCK_STATUS" = "ii " ]; then
        PACKAGE_FORMAT=deb
    fi
fi
if [ "$PACKAGE_FORMAT" = unpackaged ] &&
    command -v rpm >/dev/null 2>&1 && rpm -q facelock >/dev/null 2>&1; then
    PACKAGE_FORMAT=rpm
fi
export PACKAGE_FORMAT

ORT_LIBRARY_FILE="/usr/lib/facelock/libonnxruntime.so"
ORT_DOCUMENT_ROOT="/usr/share/doc/facelock/onnxruntime"
ORT_BUNDLE_CHECKSUMS_SHA256="e1b3397670dcabfea8b0d0608409b8409488267185fa82c99442d7c694486225"
export ORT_LIBRARY_FILE ORT_DOCUMENT_ROOT ORT_BUNDLE_CHECKSUMS_SHA256

verify_installed_deb_ort_bundle() {
    local actual_entries actual_hash expected_entries expected_hash extra path relative_path

    [ -f "$ORT_LIBRARY_FILE" ] && [ ! -L "$ORT_LIBRARY_FILE" ] || return 1
    [ -d "$ORT_DOCUMENT_ROOT" ] && [ ! -L "$ORT_DOCUMENT_ROOT" ] || return 1
    expected_entries="$(printf '%s\n' \
        GIT_COMMIT_ID \
        LICENSE \
        PROVENANCE.md \
        SHA256SUMS \
        ThirdPartyNotices.txt \
        VERSION_NUMBER \
        manifest.json)"
    actual_entries="$(find "$ORT_DOCUMENT_ROOT" -mindepth 1 -printf '%P\n' | LC_ALL=C sort)"
    [ "$actual_entries" = "$expected_entries" ] || return 1
    actual_hash="$(sha256sum "$ORT_DOCUMENT_ROOT/SHA256SUMS" | cut -d' ' -f1)"
    [ "$actual_hash" = "$ORT_BUNDLE_CHECKSUMS_SHA256" ] || return 1

    while read -r expected_hash relative_path extra; do
        [ -n "${relative_path:-}" ] && [ -z "${extra:-}" ] || return 1
        case "$relative_path" in
            lib/libonnxruntime.so) path="$ORT_LIBRARY_FILE" ;;
            *) path="$ORT_DOCUMENT_ROOT/$relative_path" ;;
        esac
        [ -f "$path" ] && [ ! -L "$path" ] || return 1
        actual_hash="$(sha256sum "$path" | cut -d' ' -f1)"
        [ "$actual_hash" = "$expected_hash" ] || return 1
    done <"$ORT_DOCUMENT_ROOT/SHA256SUMS"
}

export -f verify_installed_deb_ort_bundle

pam_facelock_executes() {
    local output rc service="$1"

    output="$(mktemp /tmp/facelock-pam-output.XXXXXX)" || return 1
    if LC_ALL=C timeout 30 pamtester "$service" testuser authenticate </dev/null >"$output" 2>&1; then
        rc=0
    else
        rc=$?
    fi
    if [ "$rc" -ne 124 ] && grep -Fq 'Identifying face' "$output"; then
        rm -f -- "$output"
        return 0
    fi
    cat "$output" >&2
    rm -f -- "$output"
    return 1
}

pam_missing_module_control_is_rejected() {
    local service=facelock-missing-module-test
    local service_path="/etc/pam.d/$service"

    sed 's/pam_facelock\.so/pam_definitely_missing.so/' \
        /etc/pam.d/facelock-test >"$service_path" || return 1
    if pam_facelock_executes "$service"; then
        rm -f -- "$service_path"
        return 1
    fi
    rm -f -- "$service_path"
}

verify_debian_packaged_pam_profile() {
    local before=/tmp/facelock-common-auth-profile.before
    local before_metadata=/tmp/facelock-common-auth-profile.metadata.before
    local good_output=/tmp/facelock-common-auth-profile.good
    local bad_output=/tmp/facelock-common-auth-profile.bad
    local active=/tmp/facelock-common-auth-profile.active
    local selected=/tmp/facelock-pam-state-profile.active
    local failed=0 service_path=/etc/pam.d/facelock-profile-test

    cp -- /etc/pam.d/common-auth "$before" || return 1
    stat -c '%a %u %g' /etc/pam.d/common-auth >"$before_metadata" || return 1
    printf '%s\n' \
        'auth include common-auth' \
        'account required pam_permit.so' >"$service_path" || return 1

    pam-auth-update --enable facelock --force || failed=1
    grep -Eq '^[[:space:]]*auth[[:space:]].*pam_facelock\.so([[:space:]]|$)' \
        /etc/pam.d/common-auth || failed=1
    cp -- /etc/pam.d/common-auth "$active" || failed=1
    cp -- /var/lib/pam/auth "$selected" || failed=1
    apt-get install -y --reinstall /facelock-test-package.deb || failed=1
    cmp -s "$active" /etc/pam.d/common-auth || failed=1
    cmp -s "$selected" /var/lib/pam/auth || failed=1
    if [ -d /run/systemd/system ]; then
        ! systemctl is-active --quiet facelock-daemon || failed=1
        [ "$(systemctl is-enabled facelock-daemon 2>/dev/null || true)" = disabled ] || failed=1
    fi

    if ! printf '%s\n' test | LC_ALL=C timeout 30 \
        pamtester facelock-profile-test testuser authenticate >"$good_output" 2>&1; then
        failed=1
    fi
    grep -Fq 'Identifying face' "$good_output" || failed=1
    grep -Fq 'successfully authenticated' "$good_output" || failed=1

    if printf '%s\n' wrong | LC_ALL=C timeout 30 \
        pamtester facelock-profile-test testuser authenticate >"$bad_output" 2>&1; then
        failed=1
    fi
    grep -Fq 'Identifying face' "$bad_output" || failed=1
    grep -Fq 'Authentication failure' "$bad_output" || failed=1

    pam-auth-update --disable facelock --force || failed=1
    cmp -s "$before" /etc/pam.d/common-auth || failed=1
    [ "$(stat -c '%a %u %g' /etc/pam.d/common-auth)" = "$(cat "$before_metadata")" ] || failed=1
    ! grep -q pam_facelock\.so /etc/pam.d/common-auth || failed=1

    rm -f -- "$before" "$before_metadata" "$good_output" "$bad_output" \
        "$active" "$selected" "$service_path"
    return "$failed"
}

verify_debian_active_profile_removal_guard() {
    local inactive=/tmp/facelock-common-auth-removal.inactive
    local inactive_metadata=/tmp/facelock-common-auth-removal.inactive.metadata
    local inactive_selected=/tmp/facelock-pam-state-removal.inactive
    local inactive_selected_metadata=/tmp/facelock-pam-state-removal.inactive.metadata
    local active=/tmp/facelock-common-auth-removal.active
    local active_metadata=/tmp/facelock-common-auth-removal.active.metadata
    local selected=/tmp/facelock-pam-state-removal.active
    local selected_metadata=/tmp/facelock-pam-state-removal.active.metadata
    local profile=/tmp/facelock-pam-profile-removal
    local profile_metadata=/tmp/facelock-pam-profile-removal.metadata
    local remove_output=/tmp/facelock-profile-removal.dpkg
    local profile_status_output=/tmp/facelock-profile-removal.status
    local good_output=/tmp/facelock-common-auth-removal.good
    local bad_output=/tmp/facelock-common-auth-removal.bad
    local service_path=/etc/pam.d/facelock-profile-removal-test
    local active_before enabled_before failed=0

    cp -- /etc/pam.d/common-auth "$inactive" || return 1
    stat -c '%a %u %g' /etc/pam.d/common-auth >"$inactive_metadata" || return 1
    cp -- /var/lib/pam/auth "$inactive_selected" || return 1
    stat -c '%a %u %g' /var/lib/pam/auth >"$inactive_selected_metadata" || return 1
    printf '%s\n' \
        'auth include common-auth' \
        'account required pam_permit.so' >"$service_path" || return 1
    pam-auth-update --enable facelock --force || failed=1
    cp -- /etc/pam.d/common-auth "$active" || failed=1
    stat -c '%a %u %g' /etc/pam.d/common-auth >"$active_metadata" || failed=1
    cp -- /var/lib/pam/auth "$selected" || failed=1
    stat -c '%a %u %g' /var/lib/pam/auth >"$selected_metadata" || failed=1
    cp -- /usr/share/pam-configs/facelock "$profile" || failed=1
    stat -c '%a %u %g' /usr/share/pam-configs/facelock >"$profile_metadata" || failed=1
    if [ -d /run/systemd/system ]; then
        active_before="$(systemctl is-active facelock-daemon 2>/dev/null || true)"
        enabled_before="$(systemctl is-enabled facelock-daemon 2>/dev/null || true)"
    fi

    if ! facelock pam shared-profile-status >"$profile_status_output" 2>&1; then
        failed=1
    fi
    [ ! -s "$profile_status_output" ] || failed=1
    if dpkg -r facelock >"$remove_output" 2>&1; then
        failed=1
    fi
    grep -Fq 'facelock: refusing package removal because the pam-auth-update profile is active.' \
        "$remove_output" || failed=1
    grep -Fq "run 'sudo pam-auth-update --disable facelock'" "$remove_output" || failed=1
    cmp -s "$active" /etc/pam.d/common-auth || failed=1
    [ "$(stat -c '%a %u %g' /etc/pam.d/common-auth)" = "$(cat "$active_metadata")" ] || failed=1
    cmp -s "$selected" /var/lib/pam/auth || failed=1
    [ "$(stat -c '%a %u %g' /var/lib/pam/auth)" = "$(cat "$selected_metadata")" ] || failed=1
    cmp -s "$profile" /usr/share/pam-configs/facelock || failed=1
    [ "$(stat -c '%a %u %g' /usr/share/pam-configs/facelock)" = "$(cat "$profile_metadata")" ] || failed=1
    dpkg-query -W -f='${db:Status-Status}\n' facelock 2>/dev/null | grep -qx installed || failed=1
    [ -x /usr/bin/facelock ] || failed=1
    [ -f "$PAM_MODULE_PATH" ] || failed=1
    if [ -d /run/systemd/system ]; then
        [ "$(systemctl is-active facelock-daemon 2>/dev/null || true)" = "$active_before" ] || failed=1
        [ "$(systemctl is-enabled facelock-daemon 2>/dev/null || true)" = "$enabled_before" ] || failed=1
    fi

    pam-auth-update --disable facelock --force || failed=1
    cmp -s "$inactive" /etc/pam.d/common-auth || failed=1
    [ "$(stat -c '%a %u %g' /etc/pam.d/common-auth)" = "$(cat "$inactive_metadata")" ] || failed=1
    cmp -s "$inactive_selected" /var/lib/pam/auth || failed=1
    [ "$(stat -c '%a %u %g' /var/lib/pam/auth)" = "$(cat "$inactive_selected_metadata")" ] || failed=1
    if ! printf '%s\n' test | LC_ALL=C timeout 30 \
        pamtester facelock-profile-removal-test testuser authenticate >"$good_output" 2>&1; then
        failed=1
    fi
    grep -Fq 'successfully authenticated' "$good_output" || failed=1
    if printf '%s\n' wrong | LC_ALL=C timeout 30 \
        pamtester facelock-profile-removal-test testuser authenticate >"$bad_output" 2>&1; then
        failed=1
    fi
    grep -Fq 'Authentication failure' "$bad_output" || failed=1

    rm -f -- "$inactive" "$inactive_metadata" "$inactive_selected" \
        "$inactive_selected_metadata" "$active" "$active_metadata" "$selected" \
        "$selected_metadata" "$profile" "$profile_metadata" "$remove_output" \
        "$profile_status_output" "$good_output" "$bad_output" "$service_path"
    return "$failed"
}

export -f pam_facelock_executes pam_missing_module_control_is_rejected
export -f verify_debian_packaged_pam_profile verify_debian_active_profile_removal_guard

echo "=== Facelock Package Validation ==="
echo ""

run_test "facelock binary exists and is executable" "[ -x /usr/bin/facelock ]"
run_test "PAM module exists in supported path" "[ -n \"$PAM_MODULE_PATH\" ]"
run_test "config exists" "[ -f /etc/facelock/config.toml ]"
run_test "D-Bus policy exists" "[ -f /usr/share/dbus-1/system.d/org.facelock.Daemon.conf ]"
run_test "D-Bus activation exists" "[ -f /usr/share/dbus-1/system-services/org.facelock.Daemon.service ]"
run_test "tmpfiles file exists" "[ -f /usr/lib/tmpfiles.d/facelock.conf ] || [ -f /usr/share/tmpfiles.d/facelock.conf ]"
case "$PACKAGE_FORMAT" in
    deb)
        run_test "Debian copyright exists" "[ -f /usr/share/doc/facelock/copyright ]"
        run_test "Debian bundled ONNX Runtime and exact legal/provenance set are hash-verified" \
            "verify_installed_deb_ort_bundle"
        ;;
esac

run_warn_check "facelock-polkit-agent binary" "[ -x /usr/bin/facelock-polkit-agent ]"
run_warn_check "quirks database files" "ls /usr/share/facelock/quirks.d/*.toml >/dev/null 2>&1"

run_test "PAM module exports pam_sm_authenticate" "nm -D \"$PAM_MODULE_PATH\" | grep -q pam_sm_authenticate"
run_test "PAM module exports pam_sm_setcred" "nm -D \"$PAM_MODULE_PATH\" | grep -q pam_sm_setcred"
run_test "PAM module avoids heavy dependencies" "! ldd \"$PAM_MODULE_PATH\" | grep -Eqi '(onnxruntime|libort|libv4l|opencv|gstreamer|openvino|cuda|rocm)'"
run_test "PAM module is under 5MB" "test $(stat -c%s $PAM_MODULE_PATH) -lt 5242880"

run_test "facelock --version exits successfully" "/usr/bin/facelock --version >/dev/null"
run_test "facelock --help exits successfully" "/usr/bin/facelock --help >/dev/null"
run_test "facelock TPM command surface is installed" "/usr/bin/facelock tpm --help >/dev/null"

run_test "D-Bus policy XML is valid" "if command -v xmllint >/dev/null 2>&1; then xmllint --noout \"$DBUS_POLICY_FILE\"; else python3 -c \"import os, xml.etree.ElementTree as ET; ET.parse(os.environ.get(\\\"DBUS_POLICY_FILE\\\"))\"; fi"

run_test "no facelock group is created (ADR 010 retired it)" "! getent group facelock" 0

run_test "facelock runtime directories exist after package transaction" "[ -d /var/lib/facelock ] && [ -d /var/log/facelock ]"

# Debian installation must not activate face authentication before the user
# has downloaded models and explicitly completed setup. Keep these assertions
# before every D-Bus call below, since a call may activate the service.
if [ "$PACKAGE_FORMAT" = deb ] && [ -d /run/systemd/system ]; then
    run_test "Debian install leaves facelock-daemon disabled before activation" \
        "[ \"$(systemctl is-enabled facelock-daemon 2>/dev/null || true)\" = disabled ]"
    run_test "Debian install leaves facelock-daemon inactive before activation" \
        "! systemctl is-active --quiet facelock-daemon"
fi

# PAM tests (only if pamtester is available)
if command -v pamtester >/dev/null 2>&1 && [ -f /etc/pam.d/facelock-test ]; then
    run_test "PAM module executes through the synthetic service" \
        "pam_facelock_executes facelock-test"
    run_test "missing PAM module control is rejected" \
        "pam_missing_module_control_is_rejected"
else
    skip_test "PAM execution block (real module and missing-module control)" \
        "pamtester or /etc/pam.d/facelock-test unavailable"
fi

if [ "$PACKAGE_FORMAT" = deb ]; then
    run_test "packaged opt-in PAM profile survives reinstall, falls back to password, and restores common-auth" \
        "verify_debian_packaged_pam_profile"
fi

# D-Bus tests (only if dbus-daemon is available)
if command -v dbus-daemon >/dev/null 2>&1; then
    # Start a system bus for testing (already running when booted under systemd)
    run_test "D-Bus system bus starts" "[ -S /run/dbus/system_bus_socket ] || (mkdir -p /run/dbus && dbus-daemon --system --fork --nopidfile 2>/dev/null)"

    # Verify the facelock service is visible on the bus
    if command -v busctl >/dev/null 2>&1; then
        run_test "D-Bus facelock service activatable" "busctl --system list --activatable 2>/dev/null | grep -q org.facelock.Daemon"
    elif command -v dbus-send >/dev/null 2>&1; then
        run_test "D-Bus facelock service activatable" "dbus-send --system --dest=org.freedesktop.DBus --print-reply /org/freedesktop/DBus org.freedesktop.DBus.ListActivatableNames 2>/dev/null | grep -q org.facelock.Daemon"
    else
        skip_test "D-Bus facelock service activatable" "neither busctl nor dbus-send available"
    fi

    # Polkit agent D-Bus boundary: non-allowlisted actions decline (fall
    # through to password), allowlisted actions pass the allowlist gate.
    if [ -x /polkit-agent-validate.sh ]; then
        run_test "polkit agent allowlist gate (D-Bus boundary)" "/polkit-agent-validate.sh"
    else
        skip_test "polkit agent allowlist gate (D-Bus boundary)" "/polkit-agent-validate.sh not present in this image"
    fi
else
    skip_test "D-Bus block (bus starts, service activatable, polkit allowlist gate)" "dbus-daemon unavailable"
fi

# systemd hardening validation — only runs under a booted systemd
# (e.g. a Debian suite package gate / `test-rpm-pkg`, which boot the container with
# systemd as PID 1 via test/run-pkg-validate-systemd.sh).
echo ""
echo "=== systemd Hardening Validation ==="

unit_prop() {
    systemctl show facelock-daemon -p "$1" --value 2>/dev/null
}
export -f unit_prop

# Attempt AF_INET socket creation inside a transient unit that replicates the
# facelock-daemon.service Phase 3 sandbox directives. This proves the directive
# set blocks outbound TCP (RestrictAddressFamilies is seccomp-based and works
# in containers; IPAddressDeny is BPF-based and may be a no-op in rootless
# containers, which is why the socket-level check is the one asserted here).
af_inet_in_sandbox() {
    systemd-run --quiet --wait --pipe --collect \
        -p CapabilityBoundingSet= \
        -p AmbientCapabilities= \
        -p 'RestrictAddressFamilies=AF_UNIX AF_NETLINK' \
        -p IPAddressDeny=any \
        -p SystemCallFilter=@system-service \
        -p SystemCallErrorNumber=EPERM \
        -p SystemCallArchitectures=native \
        -p NoNewPrivileges=yes \
        python3 -c 'import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM)' 2>/dev/null
}
export -f af_inet_in_sandbox

af_inet_unrestricted() {
    systemd-run --quiet --wait --pipe --collect \
        python3 -c 'import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM)' 2>/dev/null
}
export -f af_inet_unrestricted

# Is CAP_CHOWN (bit 0) clear in CapPrm/CapEff on EVERY thread of the running
# daemon?
#
# Every other capability assertion in this file reads `systemctl show`, i.e.
# how systemd was *configured*. None of them read what the daemon actually
# ended up holding, and the difference is where a real bug lived: capabilities
# are per-thread, so a drop performed after the process went multi-threaded
# narrowed one thread and left ONNX Runtime's inference pools and every tokio
# worker holding CAP_CHOWN for the daemon's whole life. `/proc/<pid>/status`
# would not have shown it either — that is the main thread, the one thread that
# did drop. Walking /proc/<pid>/task/* is the only read that can tell.
#
# Prints the offending thread ids so a failure is actionable.
daemon_threads_without_cap_chown() {
    pid=$(systemctl show facelock-daemon -p MainPID --value 2>/dev/null)
    [ -n "$pid" ] && [ "$pid" != "0" ] && [ -d "/proc/$pid/task" ] || {
        echo "no running daemon to inspect" >&2
        return 1
    }

    checked=0
    bad=""
    for t in /proc/"$pid"/task/*; do
        [ -r "$t/status" ] || continue
        checked=$((checked + 1))
        # CapPrm/CapEff are 16 hex digits; CAP_CHOWN is bit 0, so it is set iff
        # the last nibble is odd.
        for field in CapPrm CapEff; do
            v=$(awk -v f="$field:" '$1 == f { print $2 }' "$t/status")
            case "$v" in
                *[13579bdfBDF]) bad="$bad ${t##*/}($field=$v)" ;;
            esac
        done
    done

    # A single thread means the walk found nothing worth walking (unreadable
    # /proc, ProtectProc hiding the tree) — treat that as a failure rather than
    # a pass, or the assertion becomes decorative.
    [ "$checked" -gt 1 ] || {
        echo "only $checked thread(s) readable under /proc/$pid/task — cannot verify" >&2
        return 1
    }

    [ -z "$bad" ] || {
        echo "CAP_CHOWN still held by:$bad" >&2
        return 1
    }
    echo "verified $checked threads, none holding CAP_CHOWN"
}
export -f daemon_threads_without_cap_chown

verify_debian_reinstall_service_lifecycle() {
    local before after failed=0

    systemctl disable facelock-daemon >/dev/null 2>&1 || failed=1
    systemctl start facelock-daemon || failed=1
    before="$(unit_prop ExecMainStartTimestampMonotonic)"
    apt-get install -y --reinstall /facelock-test-package.deb || failed=1
    after="$(unit_prop ExecMainStartTimestampMonotonic)"
    systemctl is-active --quiet facelock-daemon || failed=1
    [ "$(systemctl is-enabled facelock-daemon 2>/dev/null || true)" = disabled ] || failed=1
    [ -n "$before" ] && [ -n "$after" ] && [ "$before" != "$after" ] || failed=1

    systemctl enable facelock-daemon >/dev/null 2>&1 || failed=1
    before="$after"
    apt-get install -y --reinstall /facelock-test-package.deb || failed=1
    after="$(unit_prop ExecMainStartTimestampMonotonic)"
    systemctl is-active --quiet facelock-daemon || failed=1
    systemctl is-enabled --quiet facelock-daemon || failed=1
    [ -n "$after" ] && [ "$before" != "$after" ] || failed=1

    systemctl stop facelock-daemon || failed=1
    apt-get install -y --reinstall /facelock-test-package.deb || failed=1
    ! systemctl is-active --quiet facelock-daemon || failed=1
    systemctl is-enabled --quiet facelock-daemon || failed=1

    systemctl disable facelock-daemon >/dev/null 2>&1 || failed=1
    apt-get install -y --reinstall /facelock-test-package.deb || failed=1
    ! systemctl is-active --quiet facelock-daemon || failed=1
    [ "$(systemctl is-enabled facelock-daemon 2>/dev/null || true)" = disabled ] || failed=1

    return "$failed"
}
export -f verify_debian_reinstall_service_lifecycle

if [ -d /run/systemd/system ] && systemctl show facelock-daemon >/dev/null 2>&1; then
    # Not empty: the notification privilege-drop needs CAP_SETUID+CAP_SETGID
    # (ambient, to survive the exec into runuser), and startup needs CAP_CHOWN
    # to chown the state tree and the enrollment markers on an upgraded install.
    # CAP_CHOWN is bounding-only and dropped in-process before the daemon
    # spawns its first thread — asserting it is *absent* from the ambient set
    # is the half of that these `systemctl show` reads can see. The other half,
    # that the drop actually happened on every thread, is asserted against the
    # running daemon further down (daemon_threads_without_cap_chown); these
    # directive checks cannot substitute for it. See docs/security.md, Phase 3.
    # These command strings expand only inside run_test's bash -c.
    # shellcheck disable=SC2016
    run_test "unit: CapabilityBoundingSet is SETUID+SETGID+CHOWN only" 'v=$(unit_prop CapabilityBoundingSet); echo "$v" | grep -q cap_setuid && echo "$v" | grep -q cap_setgid && echo "$v" | grep -q cap_chown && [ "$(echo "$v" | tr " " "\n" | grep -c .)" = 3 ]'
    # shellcheck disable=SC2016
    run_test "unit: AmbientCapabilities is SETUID+SETGID only" 'v=$(unit_prop AmbientCapabilities); echo "$v" | grep -q cap_setuid && echo "$v" | grep -q cap_setgid && ! echo "$v" | grep -q cap_chown'
    # shellcheck disable=SC2016
    run_test "unit: RestrictAddressFamilies is AF_UNIX+AF_NETLINK only" 'v=$(unit_prop RestrictAddressFamilies); echo "$v" | grep -q AF_UNIX && echo "$v" | grep -q AF_NETLINK && ! echo "$v" | grep -q AF_INET'
    # systemctl show expands @system-service into individual syscalls: assert
    # allowlist mode (no "~" prefix), a marker syscall the daemon needs
    # (ioctl for V4L2, capset for the in-process drop), and the absence of a
    # @privileged-only syscall (chroot) to prove it is not allow-all.
    # shellcheck disable=SC2016
    run_test "unit: SystemCallFilter allowlist active (@system-service)" 'v=$(unit_prop SystemCallFilter); [ -n "$v" ] && case "$v" in "~"*) false ;; *) true ;; esac && echo "$v" | grep -qw ioctl && echo "$v" | grep -qw capset && ! echo "$v" | grep -qw chroot'
    run_test "unit: SystemCallErrorNumber is EPERM" 'unit_prop SystemCallErrorNumber | grep -Eq "EPERM|^1$"'
    run_test "unit: SystemCallArchitectures is native" 'unit_prop SystemCallArchitectures | grep -q native'
    run_test "unit: IPAddressDeny is any" 'unit_prop IPAddressDeny | grep -Eq "any|0\.0\.0\.0/0"'
    # shellcheck disable=SC2016
    run_test "unit: ProtectProc=invisible" '[ "$(unit_prop ProtectProc)" = "invisible" ]'
    # shellcheck disable=SC2016
    run_test "unit: ProcSubset=pid" '[ "$(unit_prop ProcSubset)" = "pid" ]'
    # shellcheck disable=SC2016
    run_test "unit: ProtectHostname=yes" '[ "$(unit_prop ProtectHostname)" = "yes" ]'
    # shellcheck disable=SC2016
    run_test "unit: NoNewPrivileges=yes" '[ "$(unit_prop NoNewPrivileges)" = "yes" ]'
    # shellcheck disable=SC2016
    run_test "unit: ProtectSystem=strict" '[ "$(unit_prop ProtectSystem)" = "strict" ]'
    # shellcheck disable=SC2016
    run_test "unit: device cgroup stays permissive (no DeviceAllow)" '[ "$(unit_prop DevicePolicy)" = "auto" ] && [ -z "$(unit_prop DeviceAllow)" ]'

    if command -v systemd-run >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
        run_test "sandbox blocks AF_INET socket (outbound TCP impossible)" "! af_inet_in_sandbox"
        run_test "control: AF_INET socket allowed without sandbox" "af_inet_unrestricted"
    else
        skip_test "sandbox blocks AF_INET socket (outbound TCP impossible)" "systemd-run or python3 unavailable"
        skip_test "control: AF_INET socket allowed without sandbox" "systemd-run or python3 unavailable"
    fi

    # Daemon start test. The daemon loads ONNX models at startup, so this
    # needs models bind-mounted at /var/lib/facelock/models (the runner
    # mounts the repo models/ dir when present). There is no camera in the
    # container: an explicit device.path skips auto-detection, and a probe
    # failure on that path is non-fatal (the camera is only opened on auth).
    if ls /var/lib/facelock/models/*.onnx >/dev/null 2>&1; then
        if ! grep -q '^path\s*=' /etc/facelock/config.toml; then
            sed -i '/^\[device\]/a path = "/dev/video0"' /etc/facelock/config.toml
        fi
        run_test "facelock-daemon starts under hardened unit" "systemctl start facelock-daemon && systemctl is-active --quiet facelock-daemon"
        run_test "facelock-daemon answers on D-Bus" "busctl --system call org.facelock.Daemon /org/facelock/Daemon org.freedesktop.DBus.Peer Ping"
        # The runtime counterpart to the unit-property assertions above: what
        # the daemon *holds* while it is serving, on every thread, not what the
        # unit was configured to allow. docs/security.md §6.A promises
        # CAP_CHOWN is "never held while authenticating anyone" — this is the
        # assertion that makes the promise checkable.
        run_test "runtime: no daemon thread holds CAP_CHOWN while serving" "daemon_threads_without_cap_chown"
        if [ "$PACKAGE_FORMAT" = deb ]; then
            run_test "Debian reinstall restarts only active daemons and preserves enabled state" \
                "verify_debian_reinstall_service_lifecycle"
        fi
        systemctl stop facelock-daemon 2>/dev/null || true
    elif [ "${FACELOCK_ALLOW_MISSING_MODELS:-0}" = "1" ]; then
        # Explicitly asked for a partial run. Name every assertion that is not
        # running so the results line has to account for them.
        no_models="no ONNX models at /var/lib/facelock/models, FACELOCK_ALLOW_MISSING_MODELS=1"
        skip_test "facelock-daemon starts under hardened unit" "$no_models"
        skip_test "facelock-daemon answers on D-Bus" "$no_models"
        skip_test "runtime: no daemon thread holds CAP_CHOWN while serving" "$no_models"
        if [ "$PACKAGE_FORMAT" = deb ]; then
            skip_test "Debian reinstall restarts only active daemons and preserves enabled state" "$no_models"
        fi
    else
        # Under a booted systemd, missing models are a broken invocation, not a
        # property of the environment: the whole reason to boot systemd here is
        # to start the daemon and read what it holds. Silently dropping the
        # block left a Debian package gate reporting a clean pass on a checkout
        # that never started a daemon — and the assertion it dropped is the
        # only one that can catch a per-thread capability regression. Fail.
        echo "FAIL: daemon-start block did not run (no ONNX models at /var/lib/facelock/models)"
        echo "      Missing: the daemon start, its D-Bus Ping, the runtime CAP_CHOWN"
        echo "      thread walk, and Debian service reinstall lifecycle — the pins for"
        echo "      active-only restart and the per-thread"
        echo "      capability drop. The unit: CapabilityBoundingSet assertions above"
        echo "      read systemd configuration only and pass either way."
        echo "      Fix: run from a checkout with the ONNX models present"
        echo "        sudo cp /var/lib/facelock/models/*.onnx models/   # gitignored, cannot be committed"
        echo "        just test-deb-trixie-pkg   # or test-deb-resolute-pkg / test-rpm-pkg"
        echo "      To accept a partial run, set FACELOCK_ALLOW_MISSING_MODELS=1;"
        echo "      the three assertions are then counted as skipped, not passed."
        FAIL=$((FAIL + 1))
    fi
else
    skip_test "systemd hardening + daemon-runtime block (unit directives, sandbox probes, daemon start, runtime CAP_CHOWN walk)" \
        "not running under a booted systemd"
fi

# Package removal test — must come last since it removes the package
echo ""
echo "=== Package Removal Test ==="

if [ "$PACKAGE_FORMAT" = deb ]; then
    run_test "fresh Debian install leaves common-auth unchanged and Facelock-free" \
        "[ -f /facelock-common-auth-install-invariant ] && ! grep -q pam_facelock.so /etc/pam.d/common-auth"
    run_test "active administrator-selected profile blocks removal, preserves PAM, and allows verified migration retry" \
        "verify_debian_active_profile_removal_guard"
fi

PACKAGE_OWNED_PAM=/etc/pam.d/facelock-package-owned
PACKAGE_BLOCKER_PAM=/etc/pam.d/facelock-package-blocker
# The PAM loading rows above own this synthetic service. Package cleanup must
# see only the fixtures created for this block, so retire it before preflight.
rm -f /etc/pam.d/facelock-test "$PACKAGE_OWNED_PAM" "$PACKAGE_BLOCKER_PAM"
cat > "$PACKAGE_OWNED_PAM" <<'EOF'
#%PAM-1.0
auth      sufficient pam_facelock.so
auth      include system-auth
EOF
cat > "$PACKAGE_BLOCKER_PAM" <<'EOF'
#%PAM-1.0
auth required pam_facelock.so debug
auth include system-auth
EOF
chmod 644 "$PACKAGE_OWNED_PAM" "$PACKAGE_BLOCKER_PAM"
sha256sum "$PACKAGE_OWNED_PAM" > /tmp/facelock-package-owned.sha
sha256sum "$PACKAGE_BLOCKER_PAM" > /tmp/facelock-package-blocker.sha

# Cleanup is intentionally independent of every runtime input. These invalid
# or absent values are installed only after all daemon/auth checks above.
printf '[invalid\n' > /etc/facelock/config.toml
install -Dm600 /dev/null /var/lib/facelock/facelock.db
rm -rf /var/lib/facelock/models
export ORT_DYLIB_PATH=/facelock-test-missing-onnxruntime.so

if [ "$PACKAGE_FORMAT" = deb ]; then
    sha256sum /etc/pam.d/common-auth > /tmp/facelock-common-auth.before
    run_test "dpkg removal aborts on an unmanaged PAM reference" \
        "! dpkg -r facelock"
    run_test "aborted dpkg removal leaves inactive common-auth bytes unchanged" \
        "sha256sum -c --status /tmp/facelock-common-auth.before && ! grep -q pam_facelock.so /etc/pam.d/common-auth"
    run_test "dpkg keeps the package installed after aborted removal" \
        "dpkg-query -W -f='\${binary:Package}\n' facelock | grep -qx facelock"
    run_test "PAM module remains after aborted package removal" \
        "[ -f /lib/security/pam_facelock.so ] || [ -f /usr/lib/security/pam_facelock.so ] || [ -f /usr/lib64/security/pam_facelock.so ]"
    run_test "recognized PAM edit remains after aborted package removal" \
        "sha256sum -c --status /tmp/facelock-package-owned.sha"
    run_test "unmanaged PAM reference remains after aborted package removal" \
        "sha256sum -c --status /tmp/facelock-package-blocker.sha"
    run_test "apt-get wrapper removal aborts on an unmanaged PAM reference" \
        "! apt-get remove -y facelock"
    run_test "apt-get wrapper keeps the package installed after abort" \
        "dpkg-query -W -f='\${binary:Package}\n' facelock | grep -qx facelock"
    run_test "apt-get wrapper keeps the PAM module after abort" \
        "[ -f /lib/security/pam_facelock.so ] || [ -f /usr/lib/security/pam_facelock.so ] || [ -f /usr/lib64/security/pam_facelock.so ]"
    run_test "apt-get wrapper preserves recognized PAM edit bytes after abort" \
        "sha256sum -c --status /tmp/facelock-package-owned.sha"
    run_test "apt-get wrapper preserves blocker bytes after abort" \
        "sha256sum -c --status /tmp/facelock-package-blocker.sha"
    run_test "apt wrapper removal aborts on an unmanaged PAM reference" \
        "! apt remove -y facelock"
    run_test "apt wrapper keeps the package installed after abort" \
        "dpkg-query -W -f='\${binary:Package}\n' facelock | grep -qx facelock"
    run_test "apt wrapper keeps the PAM module after abort" \
        "[ -f /lib/security/pam_facelock.so ] || [ -f /usr/lib/security/pam_facelock.so ] || [ -f /usr/lib64/security/pam_facelock.so ]"
    run_test "apt wrapper preserves recognized PAM edit bytes after abort" \
        "sha256sum -c --status /tmp/facelock-package-owned.sha"
    run_test "apt wrapper preserves blocker bytes after abort" \
        "sha256sum -c --status /tmp/facelock-package-blocker.sha"
    rm -f "$PACKAGE_BLOCKER_PAM"
    run_test "ordinary Debian removal starts with the daemon enabled" \
        "systemctl enable facelock-daemon.service"
    run_test "Package removal via dpkg" "dpkg -r facelock"
    run_test "recognized arbitrary PAM edit cleaned before dpkg removes the module" \
        "[ -f $PACKAGE_OWNED_PAM ] && ! grep -q pam_facelock.so $PACKAGE_OWNED_PAM"
    run_test "facelock binary removed after dpkg -r" "[ ! -f /usr/bin/facelock ]"
    run_test "PAM module removed after dpkg -r" \
        "[ ! -f /lib/security/pam_facelock.so ] && [ ! -f /usr/lib/security/pam_facelock.so ] && [ ! -f /usr/lib64/security/pam_facelock.so ]"
    run_test "Config preserved after dpkg -r (conffile)" "[ -f /etc/facelock/config.toml ]"
    run_test "ordinary Debian remove preserves enabled state across reinstall" \
        "apt-get install -y /facelock-test-package.deb && systemctl is-enabled --quiet facelock-daemon.service"
    cat > "$PACKAGE_OWNED_PAM" <<'EOF'
#%PAM-1.0
auth      sufficient pam_facelock.so
auth      include system-auth
EOF
    chmod 644 "$PACKAGE_OWNED_PAM"
    run_test "apt-get wrapper removal succeeds without a blocker" \
        "apt-get remove -y facelock"
    run_test "apt-get wrapper cleans the recognized direct PAM edit" \
        "[ -f $PACKAGE_OWNED_PAM ] && ! grep -q pam_facelock.so $PACKAGE_OWNED_PAM"
    run_test "apt-get wrapper leaves the package not installed" \
        "! dpkg-query -W -f='\${db:Status-Status}\n' facelock 2>/dev/null | grep -qx installed"
    run_test "apt-get wrapper removes the PAM module" \
        "[ ! -f /lib/security/pam_facelock.so ] && [ ! -f /usr/lib/security/pam_facelock.so ] && [ ! -f /usr/lib64/security/pam_facelock.so ]"
    run_test "facelock reinstalls before apt wrapper success" \
        "apt-get install -y /facelock-test-package.deb"
    cat > "$PACKAGE_OWNED_PAM" <<'EOF'
#%PAM-1.0
auth      sufficient pam_facelock.so
auth      include system-auth
EOF
    chmod 644 "$PACKAGE_OWNED_PAM"
    run_test "apt wrapper removal succeeds without a blocker" \
        "apt remove -y facelock"
    run_test "apt wrapper cleans the recognized direct PAM edit" \
        "[ -f $PACKAGE_OWNED_PAM ] && ! grep -q pam_facelock.so $PACKAGE_OWNED_PAM"
    run_test "apt wrapper leaves the package not installed" \
        "! dpkg-query -W -f='\${db:Status-Status}\n' facelock 2>/dev/null | grep -qx installed"
    run_test "apt wrapper removes the PAM module" \
        "[ ! -f /lib/security/pam_facelock.so ] && [ ! -f /usr/lib/security/pam_facelock.so ] && [ ! -f /usr/lib64/security/pam_facelock.so ]"
elif [ "$PACKAGE_FORMAT" = rpm ]; then
    # Modify config so RPM treats it as user-edited and preserves it as .rpmsave
    echo "# modified by test" >> /etc/facelock/config.toml
    run_test "rpm removal aborts on an unmanaged PAM reference" \
        "! rpm -e facelock"
    run_test "rpm keeps the package installed after aborted removal" \
        "rpm -q facelock"
    run_test "PAM module remains after aborted package removal" \
        "[ -f /lib/security/pam_facelock.so ] || [ -f /usr/lib/security/pam_facelock.so ] || [ -f /usr/lib64/security/pam_facelock.so ]"
    run_test "recognized PAM edit remains after aborted package removal" \
        "sha256sum -c --status /tmp/facelock-package-owned.sha"
    run_test "unmanaged PAM reference remains after aborted package removal" \
        "sha256sum -c --status /tmp/facelock-package-blocker.sha"
    run_test "dnf wrapper removal aborts on an unmanaged PAM reference" \
        "! dnf remove -y facelock"
    run_test "dnf wrapper keeps the package installed after abort" \
        "rpm -q facelock"
    run_test "dnf wrapper keeps the PAM module after abort" \
        "[ -f /lib/security/pam_facelock.so ] || [ -f /usr/lib/security/pam_facelock.so ] || [ -f /usr/lib64/security/pam_facelock.so ]"
    run_test "dnf wrapper preserves recognized PAM edit bytes after abort" \
        "sha256sum -c --status /tmp/facelock-package-owned.sha"
    run_test "dnf wrapper preserves blocker bytes after abort" \
        "sha256sum -c --status /tmp/facelock-package-blocker.sha"
    rm -f "$PACKAGE_BLOCKER_PAM"
    run_test "Package removal via rpm" "rpm -e facelock"
    run_test "recognized arbitrary PAM edit cleaned before rpm removes the module" \
        "[ -f $PACKAGE_OWNED_PAM ] && ! grep -q pam_facelock.so $PACKAGE_OWNED_PAM"
    run_test "facelock binary removed after rpm -e" "[ ! -f /usr/bin/facelock ]"
    run_test "PAM module removed after rpm -e" \
        "[ ! -f /lib/security/pam_facelock.so ] && [ ! -f /usr/lib/security/pam_facelock.so ] && [ ! -f /usr/lib64/security/pam_facelock.so ]"
    run_test "Config preserved after rpm -e (config(noreplace))" "[ -f /etc/facelock/config.toml ] || [ -f /etc/facelock/config.toml.rpmsave ]"
    run_test "facelock reinstalls before dnf wrapper success" \
        "dnf install -y /facelock-test-package.rpm"
    cat > "$PACKAGE_OWNED_PAM" <<'EOF'
#%PAM-1.0
auth      sufficient pam_facelock.so
auth      include system-auth
EOF
    chmod 644 "$PACKAGE_OWNED_PAM"
    run_test "dnf wrapper removal succeeds without a blocker" \
        "dnf remove -y facelock"
    run_test "dnf wrapper cleans the recognized direct PAM edit" \
        "[ -f $PACKAGE_OWNED_PAM ] && ! grep -q pam_facelock.so $PACKAGE_OWNED_PAM"
    run_test "dnf wrapper removes the package and PAM module" \
        "! rpm -q facelock && [ ! -f /lib/security/pam_facelock.so ] && [ ! -f /usr/lib/security/pam_facelock.so ] && [ ! -f /usr/lib64/security/pam_facelock.so ]"
else
    skip_test "package removal block (removal, binary/PAM module gone, config preserved)" \
        "facelock was not installed by dpkg or rpm here"
fi

rm -f "$PACKAGE_OWNED_PAM" "$PACKAGE_BLOCKER_PAM" \
    /tmp/facelock-package-owned.sha /tmp/facelock-package-blocker.sha \
    /tmp/facelock-common-auth.before

echo ""
echo "=== Results: $PASS passed, $FAIL failed, $SKIP skipped ==="
if [ "$SKIP" -gt 0 ]; then
    echo "NOTE: $SKIP assertion(s)/block(s) above did not run — this run proves less"
    echo "      than a full one. Grep the log for '^SKIP:' to see which."
fi

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
