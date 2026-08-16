#!/bin/bash
set -euo pipefail

PASS=0
FAIL=0

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

echo "=== Facelock Package Validation ==="
echo ""

run_test "facelock binary exists and is executable" "[ -x /usr/bin/facelock ]"
run_test "PAM module exists in supported path" "[ -n \"$PAM_MODULE_PATH\" ]"
run_test "config exists" "[ -f /etc/facelock/config.toml ]"
run_test "D-Bus policy exists" "[ -f /usr/share/dbus-1/system.d/org.facelock.Daemon.conf ]"
run_test "D-Bus activation exists" "[ -f /usr/share/dbus-1/system-services/org.facelock.Daemon.service ]"
run_test "sysusers file exists" "[ -f /usr/lib/sysusers.d/facelock.conf ] || [ -f /usr/share/sysusers.d/facelock.conf ]"
run_test "tmpfiles file exists" "[ -f /usr/lib/tmpfiles.d/facelock.conf ] || [ -f /usr/share/tmpfiles.d/facelock.conf ]"

run_warn_check "facelock-polkit-agent binary" "[ -x /usr/bin/facelock-polkit-agent ]"
run_warn_check "quirks database files" "ls /usr/share/facelock/quirks.d/*.toml >/dev/null 2>&1"
run_warn_check "bundled ONNX Runtime" "[ -f /usr/lib/facelock/libonnxruntime.so ] || [ -f /usr/lib64/facelock/libonnxruntime.so ]"

run_test "PAM module exports pam_sm_authenticate" "nm -D \"$PAM_MODULE_PATH\" | grep -q pam_sm_authenticate"
run_test "PAM module exports pam_sm_setcred" "nm -D \"$PAM_MODULE_PATH\" | grep -q pam_sm_setcred"
run_test "PAM module avoids heavy dependencies" "! ldd \"$PAM_MODULE_PATH\" | grep -Eqi '(onnxruntime|libort|libv4l|opencv|gstreamer|openvino|cuda|rocm)'"
run_test "PAM module is under 5MB" "test $(stat -c%s $PAM_MODULE_PATH) -lt 5242880"

run_test "facelock --version exits successfully" "/usr/bin/facelock --version >/dev/null"
run_test "facelock --help exits successfully" "/usr/bin/facelock --help >/dev/null"

run_test "D-Bus policy XML is valid" "if command -v xmllint >/dev/null 2>&1; then xmllint --noout \"$DBUS_POLICY_FILE\"; else python3 -c \"import os, xml.etree.ElementTree as ET; ET.parse(os.environ.get(\\\"DBUS_POLICY_FILE\\\"))\"; fi"

run_test "facelock group exists (sysusers)" "if command -v systemd-sysusers >/dev/null 2>&1; then systemd-sysusers >/dev/null 2>&1 || true; fi; getent group facelock >/dev/null"

run_test "facelock runtime directories exist (tmpfiles)" "if command -v systemd-tmpfiles >/dev/null 2>&1; then systemd-tmpfiles --create >/dev/null 2>&1 || true; fi; [ -d /var/lib/facelock ] && [ -d /var/log/facelock ]"

# PAM tests (only if pamtester is available)
if command -v pamtester >/dev/null 2>&1 && [ -f /etc/pam.d/facelock-test ]; then
    run_test "PAM module loads via pamtester" "pamtester facelock-test testuser authenticate < /dev/null 2>&1 | grep -qiE '(successfully|authentication failure)'"
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
    fi

    # Polkit agent D-Bus boundary: non-allowlisted actions decline (fall
    # through to password), allowlisted actions pass the allowlist gate.
    if [ -x /polkit-agent-validate.sh ]; then
        run_test "polkit agent allowlist gate (D-Bus boundary)" "/polkit-agent-validate.sh"
    fi
fi

# systemd hardening validation — only runs under a booted systemd
# (e.g. `just test-deb-pkg` / `test-rpm-pkg`, which boot the container with
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
    run_test "unit: CapabilityBoundingSet is SETUID+SETGID+CHOWN only" 'v=$(unit_prop CapabilityBoundingSet); echo "$v" | grep -q cap_setuid && echo "$v" | grep -q cap_setgid && echo "$v" | grep -q cap_chown && [ "$(echo "$v" | tr " " "\n" | grep -c .)" = 3 ]'
    run_test "unit: AmbientCapabilities is SETUID+SETGID only" 'v=$(unit_prop AmbientCapabilities); echo "$v" | grep -q cap_setuid && echo "$v" | grep -q cap_setgid && ! echo "$v" | grep -q cap_chown'
    run_test "unit: RestrictAddressFamilies is AF_UNIX+AF_NETLINK only" 'v=$(unit_prop RestrictAddressFamilies); echo "$v" | grep -q AF_UNIX && echo "$v" | grep -q AF_NETLINK && ! echo "$v" | grep -q AF_INET'
    # systemctl show expands @system-service into individual syscalls: assert
    # allowlist mode (no "~" prefix), a marker syscall the daemon needs
    # (ioctl for V4L2, capset for the in-process drop), and the absence of a
    # @privileged-only syscall (chroot) to prove it is not allow-all.
    run_test "unit: SystemCallFilter allowlist active (@system-service)" 'v=$(unit_prop SystemCallFilter); [ -n "$v" ] && case "$v" in "~"*) false ;; *) true ;; esac && echo "$v" | grep -qw ioctl && echo "$v" | grep -qw capset && ! echo "$v" | grep -qw chroot'
    run_test "unit: SystemCallErrorNumber is EPERM" 'unit_prop SystemCallErrorNumber | grep -Eq "EPERM|^1$"'
    run_test "unit: SystemCallArchitectures is native" 'unit_prop SystemCallArchitectures | grep -q native'
    run_test "unit: IPAddressDeny is any" 'unit_prop IPAddressDeny | grep -Eq "any|0\.0\.0\.0/0"'
    run_test "unit: ProtectProc=invisible" '[ "$(unit_prop ProtectProc)" = "invisible" ]'
    run_test "unit: ProcSubset=pid" '[ "$(unit_prop ProcSubset)" = "pid" ]'
    run_test "unit: ProtectHostname=yes" '[ "$(unit_prop ProtectHostname)" = "yes" ]'
    run_test "unit: NoNewPrivileges=yes" '[ "$(unit_prop NoNewPrivileges)" = "yes" ]'
    run_test "unit: ProtectSystem=strict" '[ "$(unit_prop ProtectSystem)" = "strict" ]'
    run_test "unit: device cgroup stays permissive (no DeviceAllow)" '[ "$(unit_prop DevicePolicy)" = "auto" ] && [ -z "$(unit_prop DeviceAllow)" ]'

    if command -v systemd-run >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
        run_test "sandbox blocks AF_INET socket (outbound TCP impossible)" "! af_inet_in_sandbox"
        run_test "control: AF_INET socket allowed without sandbox" "af_inet_unrestricted"
    else
        echo "SKIP: outbound-TCP-blocked check (systemd-run or python3 unavailable)"
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
        systemctl stop facelock-daemon 2>/dev/null || true
    else
        echo "SKIP: daemon start test (no ONNX models at /var/lib/facelock/models — run via just test-deb-pkg/test-rpm-pkg with repo models present)"
    fi
else
    echo "SKIP: not running under a booted systemd (unit directives not verifiable here)"
fi

# Package removal test — must come last since it removes the package
echo ""
echo "=== Package Removal Test ==="

if command -v dpkg >/dev/null 2>&1 && dpkg -s facelock >/dev/null 2>&1; then
    run_test "Package removal via dpkg" "dpkg -r facelock"
    run_test "facelock binary removed after dpkg -r" "[ ! -f /usr/bin/facelock ]"
    run_test "PAM module removed after dpkg -r" \
        "[ ! -f /lib/security/pam_facelock.so ] && [ ! -f /usr/lib/security/pam_facelock.so ] && [ ! -f /usr/lib64/security/pam_facelock.so ]"
    run_test "Config preserved after dpkg -r (conffile)" "[ -f /etc/facelock/config.toml ]"
elif command -v rpm >/dev/null 2>&1 && rpm -q facelock >/dev/null 2>&1; then
    # Modify config so RPM treats it as user-edited and preserves it as .rpmsave
    echo "# modified by test" >> /etc/facelock/config.toml
    run_test "Package removal via rpm" "rpm -e facelock"
    run_test "facelock binary removed after rpm -e" "[ ! -f /usr/bin/facelock ]"
    run_test "PAM module removed after rpm -e" \
        "[ ! -f /lib/security/pam_facelock.so ] && [ ! -f /usr/lib/security/pam_facelock.so ] && [ ! -f /usr/lib64/security/pam_facelock.so ]"
    run_test "Config preserved after rpm -e (config(noreplace))" "[ -f /etc/facelock/config.toml ] || [ -f /etc/facelock/config.toml.rpmsave ]"
else
    echo "(no package-manager-installed facelock — skipping removal tests)"
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
