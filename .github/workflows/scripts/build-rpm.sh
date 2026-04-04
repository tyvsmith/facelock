#!/usr/bin/env bash
set -euo pipefail

PKG_VERSION_RAW="${1:?Usage: build-rpm.sh <VERSION_RAW>}"
PKG_VERSION="$PKG_VERSION_RAW"
PKG_RELEASE='1%{?dist}'

echo "=== Building RPM package ==="
echo "Raw version: ${PKG_VERSION_RAW}"

# RPM Version cannot contain '-'. For prereleases, keep the base
# version and move the prerelease marker into Release so it sorts
# before final releases.
if [ "${PKG_VERSION_RAW#*-}" != "$PKG_VERSION_RAW" ]; then
  PRERELEASE_SUFFIX="${PKG_VERSION_RAW#*-}"
  PKG_VERSION="${PKG_VERSION_RAW%%-*}"
  PRERELEASE_SUFFIX="${PRERELEASE_SUFFIX//-/.}"
  PKG_RELEASE="0.${PRERELEASE_SUFFIX}%{?dist}"
fi

echo "RPM Version: ${PKG_VERSION}"
echo "RPM Release: ${PKG_RELEASE}"

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
rpmbuild --define "_topdir $HOME/rpmbuild" \
         -bb ~/rpmbuild/SPECS/facelock.spec

echo "=== RPM package built ==="
