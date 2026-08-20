#!/usr/bin/env bash
set -euo pipefail

RPM_FILE="${1:?Usage: validate-rpm.sh <RPM_FILE> <direct|copr>}"
MODE="${2:?Usage: validate-rpm.sh <RPM_FILE> <direct|copr>}"
case "$MODE" in
  direct|copr) ;;
  *) echo "Unknown RPM channel: $MODE" >&2; exit 2 ;;
esac

CONTENTS=$(rpm -qlp "$RPM_FILE")
REQUIRES=$(rpm -qp --requires "$RPM_FILE")
echo "=== .rpm contents ==="
echo "$CONTENTS"
echo ""
echo "=== Checking required files ==="

CHECKS=(
  "usr/bin/facelock:facelock binary"
  "security/pam_facelock.so:PAM module"
  "etc/facelock/config.toml:config"
  "dbus-1/system.d/org.facelock.Daemon.conf:D-Bus policy"
  "dbus-1/system-services/org.facelock.Daemon.service:D-Bus activation"
  "tmpfiles.d/facelock.conf:tmpfiles"
  "authselect/vendor/facelock:authselect"
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

require_content() {
  local pattern="$1"
  local label="$2"
  if echo "$CONTENTS" | grep -q "$pattern"; then
    echo "OK: $label"
  else
    echo "FAIL: $label (missing $pattern)"
    FAILED=1
  fi
}

if [ "$MODE" = direct ]; then
  require_content '/facelock/libonnxruntime\.so\.1\.20\.1$' "bundled ORT library"
  require_content '/facelock/libonnxruntime\.so\.1$' "bundled ORT SONAME link"
  require_content '/onnxruntime-manifest\.json$' "bundled ORT manifest/SBOM input"
  require_content '/onnxruntime-SHA256SUMS$' "bundled ORT checksums"
  require_content '/onnxruntime-PROVENANCE\.md$' "bundled ORT provenance"
  require_content '/onnxruntime-ThirdPartyNotices\.txt$' "bundled ORT notices"
  require_content '/onnxruntime-VERSION_NUMBER$' "bundled ORT version"
  require_content '/onnxruntime-GIT_COMMIT_ID$' "bundled ORT commit"
  require_content '/onnxruntime-LICENSE$' "bundled ORT license"
  BUNDLED_SHA256="$({
    rpm2cpio "$RPM_FILE" |
      cpio --quiet --to-stdout -i './usr/lib64/facelock/libonnxruntime.so.1.20.1'
  } 2>/dev/null | sha256sum | awk '{print $1}')"
  if [ "$BUNDLED_SHA256" = a5faaf78a37590d3fe640f887620e74f6022d34550172b91ad2131bf0ad77d64 ]; then
    echo "OK: bundled ORT payload matches reviewed checksum"
  else
    echo "FAIL: bundled ORT payload checksum is $BUNDLED_SHA256"
    FAILED=1
  fi
  if echo "$REQUIRES" | grep -Eq '^onnxruntime([[:space:](]|$)'; then
    echo "FAIL: direct RPM depends on the system onnxruntime package"
    FAILED=1
  else
    echo "OK: direct RPM has no system onnxruntime dependency"
  fi
else
  if echo "$CONTENTS" | grep -Eq '/facelock/libonnxruntime|/onnxruntime-(manifest|SHA256SUMS|PROVENANCE|ThirdPartyNotices|VERSION_NUMBER|GIT_COMMIT_ID|LICENSE)'; then
    echo "FAIL: COPR RPM contains a bundled ONNX Runtime payload"
    FAILED=1
  else
    echo "OK: COPR RPM contains no bundled ONNX Runtime payload"
  fi
  if echo "$REQUIRES" | grep -Eq '^onnxruntime([[:space:](]|$)'; then
    echo "OK: COPR RPM requires Fedora onnxruntime"
  else
    echo "FAIL: COPR RPM does not require Fedora onnxruntime"
    FAILED=1
  fi
fi

if [ "$FAILED" -ne 0 ]; then
  echo "=== .rpm validation FAILED ==="
  exit 1
fi

echo "=== .rpm validation passed ==="
