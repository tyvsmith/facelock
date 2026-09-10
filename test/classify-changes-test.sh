#!/usr/bin/env bash
# Exercise .github/workflows/scripts/classify-changes.sh against real merge-base
# diffs in a throwaway repository.
#
# The classifier decides which packaging lanes run on a pull request. A
# pattern that stops matching does not fail anything -- it silently reports
# "skipped" for a deb, rpm or Arch lane, and the pull request goes green
# without that package having been built. That is the failure this file exists
# to make loud. The other direction matters too: a family-specific path that
# starts selecting every lane costs the Debian long pole on each pull request.
#
# The `rust-only` case is not an oversight. It pins the documented residual risk
# (docs/releasing.md): a change touching no packaging path is not gated by its
# own pull request, only by the nightly matrix and the release gate.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="$repo_root/.github/workflows/scripts/classify-changes.sh"
[ -x "$script" ] || {
    echo "classifier is missing or not executable: $script" >&2
    exit 1
}

work="$(mktemp -d "${TMPDIR:-/tmp}/facelock-classify.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT
cd "$work"

git init --quiet -b main .
mkdir -p docs debian dist test .github/workflows \
    crates/facelock-cli/src/commands crates/facelock-daemon/src
for f in \
    README.md \
    docs/releasing.md \
    debian/control \
    dist/facelock.spec \
    dist/PKGBUILD \
    dist/release-matrix.json \
    Cargo.toml \
    Cargo.lock \
    justfile \
    .packit.yaml \
    systemd/facelock-daemon.service \
    crates/facelock-cli/src/lifecycle.rs \
    crates/facelock-face/Cargo.toml \
    crates/facelock-cli/src/commands/pam.rs \
    crates/facelock-cli/src/commands/enroll.rs \
    crates/facelock-daemon/src/handler.rs \
    test/deb-package-contract.sh \
    test/Containerfile.rpm-e2e \
    test/arch-package-validate.sh \
    test/pkg-validate.sh \
    test/release-native-ordering.sh \
    .github/workflows/ci.yml \
    .github/workflows/packaging.yml \
    .github/workflows/scripts/build-deb.sh
do
    mkdir -p "$(dirname "$f")"
    printf 'base\n' > "$f"
done
git add -A
git -c user.email=test@example.invalid -c user.name=test commit --quiet -m base
base="$(git rev-parse HEAD)"

failures=0
# The lanes the classifier selected, as `deb,rpm,arch,release_binaries,
# release_matrix` in that fixed order, or `none`.
classification() {
    local selected
    selected="$(GITHUB_EVENT_NAME="$1" bash "$script" "${@:2}" 2>/dev/null |
        sed -n 's/^\(deb\|rpm\|arch\|release_binaries\|release_matrix\)=true$/\1/p' |
        paste -sd,)"
    echo "${selected:-none}"
}
all=deb,rpm,arch,release_binaries,release_matrix
deb=deb,release_matrix
rpm=rpm,release_binaries,release_matrix
arch=arch,release_matrix

expect_diff() {
    local want="$1" name="$2"
    shift 2
    git checkout --quiet -B "case-$name" "$base"
    for path in "$@"; do printf 'changed\n' >> "$path"; done
    git add -A
    git -c user.email=test@example.invalid -c user.name=test commit --quiet -m "$name"
    local got
    got="$(classification pull_request "$base" HEAD)"
    if [ "$got" = "$want" ]; then
        echo "  ok    $name -> $got"
    else
        echo "  FAIL  $name -> ${got:-<none>}, expected $want"
        failures=$((failures + 1))
    fi
}

expect_event() {
    local want="$1" event="$2"
    local got
    got="$(classification "$event")"
    if [ "$got" = "$want" ]; then
        echo "  ok    $event -> $got"
    else
        echo "  FAIL  $event -> ${got:-<none>}, expected $want"
        failures=$((failures + 1))
    fi
}

echo "classify-changes -- a diff that reaches every lane"
expect_diff "$all" systemd-unit systemd/facelock-daemon.service
expect_diff "$all" justfile justfile
expect_diff "$all" cargo-lock-only Cargo.lock
expect_diff "$all" workspace-manifest Cargo.toml
expect_diff "$all" crate-manifest crates/facelock-face/Cargo.toml
expect_diff "$all" shared-validator test/pkg-validate.sh
expect_diff "$all" release-matrix-json dist/release-matrix.json
expect_diff "$all" packaging-workflow .github/workflows/packaging.yml
expect_diff "$all" pam-command crates/facelock-cli/src/commands/pam.rs
expect_diff "$all" purge-lifecycle crates/facelock-cli/src/lifecycle.rs
# Renovate #306: a container digest bump inside packaging.yml. The path cannot
# say which job's image moved, so every lane runs -- by design, not by accident.
expect_diff "$all" digest-bump .github/workflows/ci.yml .github/workflows/packaging.yml

echo "classify-changes -- a diff that reaches one family"
expect_diff "$deb" debian debian/control
expect_diff "$deb" deb-harness test/deb-package-contract.sh
expect_diff "$deb" deb-release-script .github/workflows/scripts/build-deb.sh
expect_diff "$deb" mixed docs/releasing.md debian/control
expect_diff "$rpm" spec dist/facelock.spec
expect_diff "$rpm" packit .packit.yaml
expect_diff "$rpm" rpm-containerfile test/Containerfile.rpm-e2e
expect_diff "$arch" pkgbuild dist/PKGBUILD
expect_diff "$arch" arch-harness test/arch-package-validate.sh
expect_diff release_matrix ordering-harness test/release-native-ordering.sh
expect_diff deb,arch,release_matrix deb-and-arch debian/control dist/PKGBUILD

echo "classify-changes -- a diff that reaches no lane"
expect_diff none docs-only docs/releasing.md README.md
expect_diff none ci-workflow-only .github/workflows/ci.yml
expect_diff none rust-only crates/facelock-cli/src/commands/enroll.rs crates/facelock-daemon/src/handler.rs

echo "classify-changes -- unfiltered events and fail-open"
expect_event "$all" schedule
expect_event "$all" workflow_dispatch
expect_event "$all" pull_request   # no base to diff against

if [ "$failures" -ne 0 ]; then
    echo "$failures classification case(s) failed" >&2
    exit 1
fi
echo "classify-changes contract: OK"
