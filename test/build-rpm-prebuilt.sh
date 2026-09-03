#!/usr/bin/env bash
# Build an RPM from pre-built release binaries (no Rust compilation).
# Used by test/Containerfile.rpm-e2e for local end-to-end package testing.
set -euo pipefail

VERSION="${1:-0.0.0}"
CHANNEL="${2:-direct}"
# The Release field, without the dist tag. Callers that only have a stable
# version leave it alone; a caller packaging a pre-release has to pass the
# release pipeline's answer (`0.<counter>.alpha.N`), because RPM keeps the
# pre-release in Release and rpmbuild will not accept a hyphen in Version.
RPM_RELEASE="${3:-1}"
case "$CHANNEL" in
    direct|copr) ;;
    *) echo "unknown RPM test channel: $CHANNEL" >&2; exit 2 ;;
esac
case "$VERSION" in
    *-*) echo "RPM Version cannot carry a pre-release suffix: $VERSION" >&2; exit 2 ;;
esac

echo "=== Building RPM from pre-built binaries ==="
echo "Version: ${VERSION}"
echo "Release: ${RPM_RELEASE}%{?dist}"

# Set up rpmbuild tree
mkdir -p ~/rpmbuild/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# Copy and patch the spec: replace cargo build commands with no-ops
cp dist/facelock.spec ~/rpmbuild/SPECS/facelock.spec
cp dist/rpm/facelock-authselect-retirement-guard ~/rpmbuild/SOURCES/facelock-authselect-retirement-guard
sed -i "s|^Version:.*|Version:        ${VERSION}|" ~/rpmbuild/SPECS/facelock.spec
sed -i "s|^Release:.*|Release:        ${RPM_RELEASE}%{?dist}|" ~/rpmbuild/SPECS/facelock.spec
sed -i 's|^cargo build.*|true|g' ~/rpmbuild/SPECS/facelock.spec
# Create source tarball INCLUDING target/release/ (pre-built binaries)
tar --exclude=.git \
    --transform "s|^\.|facelock-${VERSION}|" \
    -czf "${HOME}/rpmbuild/SOURCES/facelock-${VERSION}.tar.gz" .

# Build the RPM (compilation is skipped via the patched spec).
# --nodeps skips BuildRequires enforcement since we're packaging pre-built binaries.
# Disable debuginfo/debugsource — pre-built binaries have no debug source files.
rpmbuild_args=(
    --define "_topdir $HOME/rpmbuild"
    --define "debug_package %{nil}"
    --nodeps
)
if [ "$CHANNEL" = direct ]; then
    rpmbuild_args+=(--with bundled_ort)
else
    # The model-free authselect lifecycle fixture exercises scriptlets and
    # package shape against Fedora's system ORT. It has no Rust source for the
    # live COPR %check, which is covered by the real COPR gate.
    rpmbuild_args+=(--without bundled_ort --nocheck)
fi
rpmbuild "${rpmbuild_args[@]}" -bb ~/rpmbuild/SPECS/facelock.spec

# Copy resulting RPM to current directory
cp ~/rpmbuild/RPMS/*/*.rpm ./

echo "=== RPM package built ==="
ls -la ./*.rpm
