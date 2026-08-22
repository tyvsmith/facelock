#!/usr/bin/env bash
set -euo pipefail

DEB_FILE="${1:?Usage: validate-deb.sh <DEB_FILE>}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

[ "$(dpkg-deb --field "$DEB_FILE" Package)" = facelock ] || {
  echo "FAIL: Debian binary package identity is not facelock" >&2
  exit 1
}
for relation in Provides Conflicts Replaces; do
  [ -z "$(dpkg-deb --field "$DEB_FILE" "$relation")" ] || {
    echo "FAIL: Debian binary package must not declare $relation" >&2
    exit 1
  }
done
dpkg-deb --field "$DEB_FILE" Depends |
  grep -Eq '(^|,[[:space:]])libtss2-(esys|tctildr|mu)[^,]*([[:space:]]*,|$)' || {
    echo "FAIL: Debian binary package lacks a generated TPM runtime dependency" >&2
    exit 1
  }

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
  "tmpfiles.d/facelock.conf:tmpfiles"
  "usr/lib/facelock/libonnxruntime.so:bundled ORT"
  "usr/share/doc/facelock/copyright:Debian copyright"
  "usr/share/doc/facelock/onnxruntime/GIT_COMMIT_ID:ORT upstream commit"
  "usr/share/doc/facelock/onnxruntime/LICENSE:ORT license"
  "usr/share/doc/facelock/onnxruntime/PROVENANCE.md:ORT provenance"
  "usr/share/doc/facelock/onnxruntime/ThirdPartyNotices.txt:ORT third-party notices"
  "usr/share/doc/facelock/onnxruntime/VERSION_NUMBER:ORT version"
  "usr/share/doc/facelock/onnxruntime/manifest.json:ORT component manifest"
  "usr/share/doc/facelock/onnxruntime/SHA256SUMS:ORT bundle checksums"
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

EXTRACT_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/facelock-validate-deb.XXXXXX")"
trap 'rm -rf -- "$EXTRACT_ROOT"' EXIT
dpkg-deb --extract "$DEB_FILE" "$EXTRACT_ROOT"
"$REPO_ROOT/scripts/prepare-ort-bundle.sh" verify-installed \
  "$EXTRACT_ROOT/usr/lib/facelock/libonnxruntime.so" \
  "$EXTRACT_ROOT/usr/share/doc/facelock/onnxruntime"

echo "=== .deb validation passed ==="
