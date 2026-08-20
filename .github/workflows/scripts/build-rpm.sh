#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ "${1:-}" != "--networkless-inner" ]; then
    exec "$SCRIPT_DIR/run-networkless.sh" "$0" --networkless-inner "$@"
fi
shift
[ "${FACELOCK_NETWORKLESS_ACTIVE:-}" = 1 ] || {
    echo "ERROR: refusing RPM assembly outside the networkless sandbox" >&2
    exit 1
}
python3 -c '
import errno
import socket

try:
    socket.socket()
except OSError as error:
    if error.errno == errno.ENOSYS:
        raise SystemExit(0)
    raise
raise SystemExit("networkless sandbox is not enforced")
'

PKG_VERSION_RAW="${1:?Usage: build-rpm.sh <VERSION_RAW> <PRERELEASE_COUNTER>}"
PRERELEASE_COUNTER="${2:?Usage: build-rpm.sh <VERSION_RAW> <PRERELEASE_COUNTER>}"
# shellcheck source=../../../scripts/release-versions.sh
source "$SCRIPT_DIR/../../../scripts/release-versions.sh"
PKG_VERSION="$(release_rpm_version "$PKG_VERSION_RAW")"
PKG_RELEASE="$(release_rpm_release "$PKG_VERSION_RAW" "$PRERELEASE_COUNTER")%{?dist}"

echo "=== Building RPM package ==="
echo "Raw version: ${PKG_VERSION_RAW}"

echo "RPM Version: ${PKG_VERSION}"
echo "RPM Release: ${PKG_RELEASE}"

# Package assembly is deliberately network-free. The release workflow fetches
# and verifies the pinned archive in download-ort before this script runs.
for required in \
    onnxruntime/lib/libonnxruntime.so \
    onnxruntime/LICENSE \
    onnxruntime/ThirdPartyNotices.txt \
    onnxruntime/VERSION_NUMBER \
    onnxruntime/GIT_COMMIT_ID \
    onnxruntime/PROVENANCE.md \
    onnxruntime/manifest.json \
    onnxruntime/SHA256SUMS; do
    test -s "$required"
done
(cd onnxruntime && sha256sum --check SHA256SUMS)

mkdir -p ~/rpmbuild/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# Copy spec file and set version/release
cp dist/facelock.spec ~/rpmbuild/SPECS/facelock.spec
sed -i "s|^Version:.*|Version:        ${PKG_VERSION}|" ~/rpmbuild/SPECS/facelock.spec
sed -i "s|^Release:.*|Release:        ${PKG_RELEASE}|" ~/rpmbuild/SPECS/facelock.spec

# Build source tarball expected by Source0 so rpmbuild can run the
# full %prep/%build/%install pipeline.
tar --exclude=.git --exclude=target \
    --transform "s|^|facelock-${PKG_VERSION}/|" \
    -czf "${HOME}/rpmbuild/SOURCES/facelock-${PKG_VERSION}.tar.gz" .

# Build RPM using spec-defined build/install steps.
# The preceding release build populated Cargo's cache; assembly must never
# resolve or fetch dependencies from the network.
export CARGO_NET_OFFLINE=true
rpmbuild --define "_topdir $HOME/rpmbuild" \
         --with bundled_ort \
         -bb ~/rpmbuild/SPECS/facelock.spec

echo "=== RPM package built ==="
