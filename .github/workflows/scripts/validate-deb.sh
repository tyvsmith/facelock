#!/usr/bin/env bash
set -euo pipefail

DEB_FILE="${1:?Usage: validate-deb.sh <DEB_FILE>}"

CONTENTS=$(dpkg-deb -c "$DEB_FILE")
echo "=== .deb contents ==="
echo "$CONTENTS"
echo ""
echo "=== Checking required files ==="

CHECKS=(
  "usr/bin/facelock:facelock binary"
  "lib/security/pam_facelock.so:PAM module"
  "etc/facelock/config.toml:config"
  "dbus-1/system.d/org.facelock.Daemon.conf:D-Bus policy"
  "dbus-1/system-services/org.facelock.Daemon.service:D-Bus activation"
  "sysusers.d/facelock.conf:sysusers"
  "tmpfiles.d/facelock.conf:tmpfiles"
  "usr/lib/facelock/libonnxruntime.so:bundled ORT"
)

FAILED=0
for check in "${CHECKS[@]}"; do
  pattern="${check%%:*}"
  label="${check#*:}"
  if echo "$CONTENTS" | grep -q "$pattern"; then
    echo "OK: $label"
  else
    echo "FAIL: $label (missing $pattern)"
    FAILED=1
  fi
done

if [ "$FAILED" -ne 0 ]; then
  echo "=== .deb validation FAILED ==="
  exit 1
fi

echo "=== .deb validation passed ==="
