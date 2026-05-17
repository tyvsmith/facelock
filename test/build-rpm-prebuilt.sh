#!/usr/bin/env bash
# Build an RPM from pre-built release binaries (no Rust compilation).
# Used by test/Containerfile.rpm-e2e for local end-to-end package testing.
set -euo pipefail

VERSION="${1:-0.0.0}"

echo "=== Building RPM from pre-built binaries ==="
echo "Version: ${VERSION}"

# Set up rpmbuild tree
mkdir -p ~/rpmbuild/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# Copy and patch the spec: replace cargo build commands with no-ops
cp dist/facelock.spec ~/rpmbuild/SPECS/facelock.spec
sed -i "s|^Version:.*|Version:        ${VERSION}|" ~/rpmbuild/SPECS/facelock.spec
sed -i "s|^Release:.*|Release:        1%{?dist}|" ~/rpmbuild/SPECS/facelock.spec
sed -i 's|^cargo build.*|true|g' ~/rpmbuild/SPECS/facelock.spec
# Ensure facelock lib dir exists even without bundled ORT
sed -i '/^if \[ -f onnxruntime/i install -dm755 %{buildroot}%{_libdir}/facelock' ~/rpmbuild/SPECS/facelock.spec

# Create source tarball INCLUDING target/release/ (pre-built binaries)
tar --exclude=.git \
    --transform "s|^\.|facelock-${VERSION}|" \
    -czf "${HOME}/rpmbuild/SOURCES/facelock-${VERSION}.tar.gz" .

# Build the RPM (compilation is skipped via the patched spec).
# --nodeps skips BuildRequires enforcement since we're packaging pre-built binaries.
# Disable debuginfo/debugsource — pre-built binaries have no debug source files.
rpmbuild --define "_topdir $HOME/rpmbuild" \
         --define "debug_package %{nil}" \
         --nodeps \
         -bb ~/rpmbuild/SPECS/facelock.spec

# Copy resulting RPM to current directory
cp ~/rpmbuild/RPMS/*/*.rpm ./

echo "=== RPM package built ==="
ls -la ./*.rpm
