#!/usr/bin/env bash
# Exercise select_main_package() from test/build-arch-source-package.sh against
# a directory of package file names, without makepkg or a container.
#
# makepkg's `debug` option emits facelock-debug-<pkgver>-<pkgrel>-<arch>.pkg.tar.zst
# beside the package the Arch lane installs. The lane used to take the first
# match `find` returned, which is directory order: on one pull request that was
# the debug package, `pacman -U` installed it, and every later assertion failed
# with "package 'facelock' was not found"; on the next the same code picked the
# real package and passed (#212). Directory order is the variable, so both
# creation orders are built here -- a selection that reads position instead of
# name passes one and fails the other.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Sourcing defines select_main_package() and returns before the build.
# shellcheck source=test/build-arch-source-package.sh
source "$repo_root/test/build-arch-source-package.sh"

work="$(mktemp -d "${TMPDIR:-/tmp}/facelock-arch-select.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT

failures=0

# A fresh directory holding exactly the named files, created in the order given.
fixture() {
    local name="$1"
    shift
    local dir="$work/$name" file
    rm -rf -- "$dir"
    mkdir -p -- "$dir"
    for file in "$@"; do
        : > "$dir/$file"
    done
    printf '%s\n' "$dir"
}

expect_pick() {
    local name="$1" want="$2" dir="$3"
    local got
    # select_main_package fails by exiting, so the subshell keeps a rejection
    # from taking this test down with it.
    got="$(select_main_package "$dir" 2>/dev/null)" || got=""
    if [ "${got##*/}" = "$want" ]; then
        echo "  ok    $name -> $want"
    else
        echo "  FAIL  $name -> ${got##*/}, expected $want"
        failures=$((failures + 1))
    fi
}

expect_reject() {
    local name="$1" dir="$2"
    local got
    if got="$(select_main_package "$dir" 2>/dev/null)"; then
        echo "  FAIL  $name -> accepted ${got##*/}, expected a failure"
        failures=$((failures + 1))
    else
        echo "  ok    $name -> rejected"
    fi
}

main=facelock-0.1.4-1-x86_64.pkg.tar.zst
debug=facelock-debug-0.1.4-1-x86_64.pkg.tar.zst

echo "select_main_package -- a debug split beside the package"
expect_pick "main written first" "$main" \
    "$(fixture main-first "$main" "$debug")"
expect_pick "debug written first" "$main" \
    "$(fixture debug-first "$debug" "$main")"
expect_pick "no debug split" "$main" \
    "$(fixture solo "$main")"

echo "select_main_package -- nothing to install"
expect_reject "empty directory" "$(fixture empty)"
expect_reject "only the debug split" "$(fixture debug-only "$debug")"

echo "select_main_package -- more than one candidate"
expect_reject "two versions of the package" \
    "$(fixture two-mains "$main" facelock-0.1.5-1-x86_64.pkg.tar.zst)"

if [ "$failures" -ne 0 ]; then
    echo "$failures package selection case(s) failed" >&2
    exit 1
fi
echo "arch package selection: OK"
