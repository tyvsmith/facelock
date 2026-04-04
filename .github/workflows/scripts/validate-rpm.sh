#!/usr/bin/env bash
set -euo pipefail

RPM_FILE="${1:?Usage: validate-rpm.sh <RPM_FILE>}"

echo "=== .rpm contents ==="
rpm -qlp "$RPM_FILE"
echo ""
echo "=== Checking required files ==="

CHECKS=(
  "usr/bin/facelock:facelock binary"
  "security/pam_facelock.so:PAM module"
  "etc/facelock/config.toml:config"
  "dbus-1/system.d/org.facelock.Daemon.conf:D-Bus policy"
  "dbus-1/system-services/org.facelock.Daemon.service:D-Bus activation"
  "sysusers.d/facelock.conf:sysusers"
  "tmpfiles.d/facelock.conf:tmpfiles"
  "authselect/vendor/facelock:authselect"
)

FAILED=0
for check in "${CHECKS[@]}"; do
  pattern="${check%%:*}"
  label="${check#*:}"
  if rpm -qlp "$RPM_FILE" | grep -q "$pattern"; then
    echo "OK: $label"
  else
    echo "FAIL: $label (missing $pattern)"
    FAILED=1
  fi
done

if [ "$FAILED" -ne 0 ]; then
  echo "=== .rpm validation FAILED ==="
  exit 1
fi

echo "=== .rpm validation passed ==="
