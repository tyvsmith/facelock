#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../scripts/release-versions.sh
source "$repo_root/scripts/release-versions.sh"

kind="${1:?Usage: release-native-ordering.sh <debian|rpm|arch>}"

case "$kind" in
    debian)
        versions=(
            0.1.4-1
            "$(release_debian_common_version 0.2.0-alpha.1 1)"
            "$(release_debian_common_version 0.2.0-alpha.1 2)"
            "$(release_debian_common_version 0.2.0-alpha.2 1)"
            "$(release_debian_common_version 0.2.0-beta.1 1)"
            "$(release_debian_common_version 0.2.0-rc.1 1)"
            "$(release_debian_common_version 0.2.0 1)"
        )
        compare() { dpkg --compare-versions "$1" lt "$2"; }
        ;;
    rpm)
        versions=(
            0.1.4-1
            "$(release_rpm_evr 0.2.0-alpha.1 1)"
            "$(release_rpm_evr 0.2.0-alpha.1 2)"
            "$(release_rpm_evr 0.2.0-alpha.2 3)"
            "$(release_rpm_evr 0.2.0-beta.1 4)"
            "$(release_rpm_evr 0.2.0-rc.1 5)"
            "$(release_rpm_evr 0.2.0 1)"
        )
        compare() {
            local status
            if rpmdev-vercmp "$1" "$2" >/dev/null; then
                status=0
            else
                status=$?
            fi
            # rpmdev-vercmp status 12 means EVR2 is newer than EVR1.
            [ "$status" -eq 12 ]
        }
        ;;
    arch)
        versions=(
            0.1.4-1
            "$(release_arch_version 0.2.0-alpha.1 1)"
            "$(release_arch_version 0.2.0-alpha.1 2)"
            "$(release_arch_version 0.2.0-alpha.2 1)"
            "$(release_arch_version 0.2.0-beta.1 1)"
            "$(release_arch_version 0.2.0-rc.1 1)"
            "$(release_arch_version 0.2.0 1)"
        )
        compare() { [ "$(vercmp "$1" "$2")" -lt 0 ]; }
        ;;
    *)
        echo "unknown native ordering kind: $kind" >&2
        exit 2
        ;;
esac

for ((index = 0; index + 1 < ${#versions[@]}; index++)); do
    left="${versions[index]}"
    right="${versions[index + 1]}"
    if ! compare "$left" "$right"; then
        echo "native $kind ordering failed: $left !< $right" >&2
        exit 1
    fi
done

echo "native $kind ordering: OK (${versions[*]})"

# facelock-git is versioned by dist/PKGBUILD-git's pkgver(), which appends
# `.r<commits>.g<sha>` to the newest release tag it can reach. That has to land
# strictly between the release it descends from and the next one, or the AUR
# package is unupgradeable in one direction and blocks the real release in the
# other (#330). The recipe's half -- that it produces this shape at all -- is
# test/release-version-contract.sh; pacman decides the ordering, so it is
# decided here.
if [ "$kind" = arch ]; then
    git_build_pairs=(
        "$(release_arch_pkgver 0.1.4)|$(release_arch_git_pkgver 0.1.4 650 a8c48b7)"
        "$(release_arch_git_pkgver 0.1.4 650 a8c48b7)|$(release_arch_pkgver 0.2.0-alpha.1)"
        "$(release_arch_pkgver 0.2.0-alpha.1)|$(release_arch_git_pkgver 0.2.0-alpha.1 7 deadbee)"
        "$(release_arch_git_pkgver 0.2.0-alpha.1 7 deadbee)|$(release_arch_pkgver 0.2.0-alpha.2)"
        "$(release_arch_pkgver 0.2.0-rc.1)|$(release_arch_git_pkgver 0.2.0-rc.1 2 facefee)"
        "$(release_arch_git_pkgver 0.2.0-rc.1 2 facefee)|$(release_arch_pkgver 0.2.0)"
        "$(release_arch_pkgver 0.2.0)|$(release_arch_git_pkgver 0.2.0 1 c0ffee1)"
    )
    for pair in "${git_build_pairs[@]}"; do
        if ! compare "${pair%%|*}" "${pair##*|}"; then
            echo "native arch facelock-git ordering failed: ${pair%%|*} !< ${pair##*|}" >&2
            exit 1
        fi
    done
    echo "native arch facelock-git ordering: OK (${#git_build_pairs[@]} pairs)"
fi
