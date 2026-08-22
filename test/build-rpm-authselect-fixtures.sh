#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
topdir=/tmp/facelock-authselect-rpmbuild
artifacts=/artifacts
legacy_sha256=e8d3858adbf001676cc1d25171702c396e6ed22dd2a8c4f0d064a8c2febb3a0b

rm -rf "$topdir"
mkdir -p "$topdir"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS} "$artifacts"

printf '%s  %s\n' "$legacy_sha256" "$artifacts/facelock-old.rpm" | sha256sum -c -
[ "$(rpm -qp --qf '%{NAME} %{VERSION}-%{RELEASE} %{ARCH}\n' \
    "$artifacts/facelock-old.rpm")" = "facelock 0.1.4-1.fc44 x86_64" ]

# Build the production spec and scriptlets around model-free disposable
# payloads. Lifecycle assertions exercise RPM/DNF, authselect and real PAM;
# no daemon, inference runtime, model, or camera is started.
mkdir -p "$repo_root/target/release"
cp /usr/bin/true "$repo_root/target/release/facelock"
cp /usr/bin/true "$repo_root/target/release/facelock-polkit-agent"
cp /usr/bin/true "$repo_root/target/release/libpam_facelock.so"
(
    cd "$repo_root"
    bash test/build-rpm-prebuilt.sh 0.2.0 copr
)
new_rpm="$(find "$repo_root" -maxdepth 1 -type f -name 'facelock-0.2.0-1*.rpm' -print -quit)"
[ -n "$new_rpm" ]
cp "$new_rpm" "$artifacts/facelock-new.rpm"

"$repo_root/.github/workflows/scripts/validate-rpm.sh" "$new_rpm" copr

FACELOCK_TEST_RPM="$artifacts/facelock-new.rpm" \
    bash "$repo_root/test/rpm-authselect-artifact-contract.sh"
